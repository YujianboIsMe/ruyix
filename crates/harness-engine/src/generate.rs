//! 代码生成：按步骤调模型 → 白名单校验路径 → 落盘。
//!
//! 路径白名单是硬约束：模型完全可能吐 `../../etc/x`、`C:\Windows\x`、
//! 或者 `/dev/null`。任何绝对路径、`..`、盘符、非法字符一律拒绝并记进 notes，
//! 而不是"静默写真去了"。这一步的失败要能被用户看见。

use crate::config::{AppConfig, LlmConfig};
use crate::exec::{CancelFlag, is_cancelled};
use crate::llm::{self, ChatMessage, Usage};
use crate::plan::{self, Plan, PlanStep};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GenFile {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub bytes: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StepOutcome {
    pub step_id: u32,
    pub title: String,
    pub status: String, // done | error | skipped
    pub notes: String,
    pub files: Vec<String>,
    pub error: Option<String>,
    /// 本步没有产出任何文件（合法，但 UI/日志要提示，避免"静默空转"）
    #[serde(default)]
    pub no_files: bool,
    /// 本步注入了什么知识（v0.6）：命中的来源、多少字符、哪些被裁
    #[serde(default)]
    pub kb: Option<crate::kb::retrieve::KbInjection>,
    pub elapsed_ms: u128,
    pub usage: Usage,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct GenOutcome {
    pub run_id: String,
    pub dir: String,
    pub files: Vec<GenFile>,
    pub steps: Vec<StepOutcome>,
    pub rejected: Vec<String>,
    pub usage: Usage,
    pub elapsed_ms: u128,
}

/// 把模型给的路径规范化成"运行目录内的安全相对路径"。
pub fn safe_rel_path(raw: &str) -> Result<String, String> {
    let s = raw.trim().replace('\\', "/");
    let s = s.trim_start_matches("./").trim();
    if s.is_empty() {
        return Err("空路径".into());
    }
    if s.starts_with('/') {
        return Err(format!("拒绝绝对路径: {raw}"));
    }
    // Windows 盘符 / UNC
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        return Err(format!("拒绝盘符路径: {raw}"));
    }
    let mut out: Vec<String> = Vec::new();
    for part in s.split('/') {
        let p = part.trim();
        if p.is_empty() || p == "." {
            continue;
        }
        if p == ".." {
            return Err(format!("拒绝向上越界路径: {raw}"));
        }
        if p.contains([':', '*', '?', '"', '<', '>', '|']) {
            return Err(format!("路径含非法字符: {raw}"));
        }
        // 目录名不能以点结尾（Windows 会静默改名，导致后续读文件找不到）
        if p.ends_with('.') {
            return Err(format!("路径片段以点结尾: {raw}"));
        }
        out.push(p.to_string());
    }
    if out.is_empty() {
        return Err(format!("规范化后为空路径: {raw}"));
    }
    // 兜底：再次确认拼出来的路径不会逃出根目录
    let joined = out.join("/");
    if Path::new(&joined).components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("路径越界: {raw}"));
    }
    Ok(joined)
}

fn write_file(root: &Path, rel: &str, content: &str) -> Result<usize, String> {
    let target: PathBuf = root.join(rel);
    // 双保险：canonicalize 父目录后必须仍在 root 下
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录失败 {}: {e}", parent.display()))?;
        let root_c = std::fs::canonicalize(root).map_err(|e| format!("规范化根目录失败: {e}"))?;
        let parent_c = std::fs::canonicalize(parent).map_err(|e| format!("规范化目录失败: {e}"))?;
        if !parent_c.starts_with(&root_c) {
            return Err(format!("写入位置越出运行目录: {}", target.display()));
        }
    }
    std::fs::write(&target, content)
        .map_err(|e| format!("写文件失败 {}: {e}", target.display()))?;
    Ok(content.len())
}

/// 解析模型返回的文件列表。兼容 files 是数组 / 是 {path: content} 映射两种形态。
/// 返回空列表是合法结果（该步骤本来就不需要写文件），只有"根本不是 JSON"才算失败。
pub fn parse_step_files(raw: &str) -> Result<(Vec<(String, String)>, String), String> {
    let json = llm::extract_json_object(raw);
    let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| {
        format!(
            "代码生成结果不是合法 JSON: {e}；片段: {}",
            crate::exec::clip(&json, 400)
        )
    })?;

    let notes = v
        .get("notes")
        .or_else(|| v.get("note"))
        .or_else(|| v.get("summary"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    let files_v = v
        .get("files")
        .or_else(|| v.get("file_list"))
        .or_else(|| v.get("outputs"));

    let mut out = Vec::new();
    match files_v {
        Some(serde_json::Value::Array(arr)) => {
            for f in arr {
                let path = f
                    .get("path")
                    .or_else(|| f.get("file"))
                    .or_else(|| f.get("name"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = f
                    .get("content")
                    .or_else(|| f.get("code"))
                    .or_else(|| f.get("text"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if !path.is_empty() {
                    out.push((path, content));
                }
            }
        }
        Some(serde_json::Value::Object(map)) => {
            for (k, val) in map {
                if let Some(s) = val.as_str() {
                    out.push((k.clone(), s.to_string()));
                }
            }
        }
        _ => {}
    }

    // 空 files 是合法状态：有的步骤（"跑一遍测试确认"）本来就不产出文件。
    // 这里返回 Ok，由调用方把它标成 done + no_files，而不是伪装成生成失败。
    Ok((out, notes))
}

fn build_gen_user_prompt(
    task: &str,
    plan_json: &str,
    step: &PlanStep,
    written: &[GenFile],
    budget: usize,
    kb_block: Option<&str>,
) -> String {
    let mut already = String::new();
    let mut used = 0usize;
    for f in written {
        if used >= budget {
            already.push_str(&format!("\n- {} （内容因上下文预算省略）", f.path));
            continue;
        }
        let head = crate::exec::clip(&f.content, (budget - used).clamp(600, 4000));
        used += head.len();
        already.push_str(&format!("\n--- {} ---\n{}\n", f.path, head));
    }
    if already.is_empty() {
        already.push_str("（本步骤之前还没有生成任何文件）");
    }

    // 知识块放在**当前步骤之后**：先把任务与步骤读进来，再给参考资料，
    // 且整段只出现在 user 消息里（system 里一个字都不许有）。
    let kb_part = match kb_block {
        Some(b) if !b.trim().is_empty() => format!("\n{b}\n"),
        _ => String::new(),
    };

    format!(
        r#"原始任务：
{task}

整体规划（JSON）：
{plan_json}

========================================
当前步骤（第 {id} 步 / 共 {total} 步）：
标题：{title}
类型：{kind}
要求：{detail}
预期文件：{files}

已生成文件：
{already}
{kb_part}
========================================
请只完成"当前步骤"，输出 JSON：{{"files":[{{"path":"...","content":"..."}}],"notes":"..."}}
不要提前实现后续步骤的内容，也不要重写已生成文件（除非当前步骤要求修改它）。"#,
        id = step.id,
        total = plan_total(plan_json),
        title = step.title,
        kind = step.kind,
        detail = if step.detail.trim().is_empty() {
            "（模型未给出细节，按标题执行）"
        } else {
            step.detail.trim()
        },
        files = if step.files.is_empty() {
            "（未指定）".to_string()
        } else {
            step.files.join(", ")
        },
    )
}

fn plan_total(plan_json: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(plan_json)
        .ok()
        .and_then(|v| v.get("steps").and_then(|s| s.as_array()).map(|a| a.len()))
        .unwrap_or(0)
}

/// 逐步骤生成并落盘。`on_step` 用于把进度推给 UI（每个步骤开始/结束各调一次）。
///
/// `kb` = 知识库引擎（None = 不注入）。**查询用"当前步骤标题 + 详情"**（需求 §4）：
/// 规划步骤的用词与项目文档高度重合，恰是字面检索的强项。
#[allow(clippy::too_many_arguments)]
pub async fn run_all(
    llm_cfg: &LlmConfig,
    app_cfg: &AppConfig,
    task: &str,
    plan: &Plan,
    run_id: &str,
    root: &Path,
    mut on_step: impl FnMut(&StepOutcome, usize, usize),
    flag: &CancelFlag,
    kb: Option<&crate::kb::Engine>,
) -> Result<GenOutcome, String> {
    std::fs::create_dir_all(root)
        .map_err(|e| format!("创建运行目录失败 {}: {e}", root.display()))?;

    let plan_json = serde_json::to_string_pretty(plan).unwrap_or_default();
    let total = plan.steps.len();
    let mut outcome = GenOutcome {
        run_id: run_id.to_string(),
        dir: root.to_string_lossy().to_string(),
        ..Default::default()
    };
    let mut written: Vec<GenFile> = Vec::new();

    for (idx, step) in plan.steps.iter().enumerate() {
        let mut st = StepOutcome {
            step_id: step.id,
            title: step.title.clone(),
            status: "running".into(),
            ..Default::default()
        };
        on_step(&st, idx + 1, total);

        if is_cancelled(flag) {
            st.status = "skipped".into();
            st.notes = "用户取消".into();
            outcome.steps.push(st.clone());
            on_step(&st, idx + 1, total);
            continue;
        }

        // 知识注入：查询 = 当前步骤标题 + 详情；工作区 = 已生成的文件
        // （工作区里已经有的文件不许再用知识库的历史副本覆盖 —— 硬规则）
        let mut kb_block: Option<String> = None;
        if let Some(engine) = kb {
            let query = format!("{} {}", step.title, step.detail);
            let ws = crate::kb::retrieve::Workspace::of(
                &[root.to_path_buf()],
                &written.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            );
            let inj = crate::kb::retrieve::retrieve(
                engine,
                &format!("generate:step-{}", step.id),
                &query,
                &ws,
            );
            kb_block = crate::kb::retrieve::render_block(&inj);
            st.kb = Some(inj);
        }

        let user = build_gen_user_prompt(
            task,
            &plan_json,
            step,
            &written,
            app_cfg.max_context_chars,
            kb_block.as_deref(),
        );
        let res = llm::chat(
            llm_cfg,
            &[
                ChatMessage::system(plan::GEN_SYSTEM),
                ChatMessage::user(user),
            ],
            true,
        )
        .await;

        match res {
            Ok(out) => {
                st.elapsed_ms = out.elapsed_ms;
                st.usage = out.usage.clone();
                outcome.usage.add(&out.usage);
                match parse_step_files(&out.content) {
                    Ok((files, notes)) => {
                        st.notes = notes;
                        for (raw_path, content) in files {
                            match safe_rel_path(&raw_path) {
                                Ok(rel) => match write_file(root, &rel, &content) {
                                    Ok(bytes) => {
                                        st.files.push(rel.clone());
                                        if let Some(existing) =
                                            written.iter_mut().find(|f| f.path == rel)
                                        {
                                            existing.content = content.clone();
                                            existing.bytes = bytes;
                                        } else {
                                            written.push(GenFile {
                                                path: rel,
                                                content,
                                                bytes,
                                            });
                                        }
                                    }
                                    Err(e) => outcome.rejected.push(format!("{raw_path}: {e}")),
                                },
                                Err(e) => {
                                    st.notes.push_str(&format!("\n[路径被拒绝] {e}"));
                                    outcome.rejected.push(format!("{raw_path}: {e}"));
                                }
                            }
                        }
                        st.status = "done".into();
                        if st.files.is_empty() {
                            st.no_files = true;
                            st.notes.push_str(
                                "
⚠ 本步没有产出任何文件（模型认为无需改动，或漏写了 files）",
                            );
                        }
                    }
                    Err(e) => {
                        st.status = "error".into();
                        st.error = Some(e);
                    }
                }
            }
            Err(e) => {
                st.status = "error".into();
                st.error = Some(e);
            }
        }

        outcome.steps.push(st.clone());
        on_step(&st, idx + 1, total);
    }

    outcome.files = written;
    Ok(outcome)
}

/// 把已生成的完整文件树给验证阶段用（只要路径 + 大小）
pub fn file_listing(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with('.')
                || name == "target"
                || name == "node_modules"
                || name == "__pycache__"
            {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_relative_paths() {
        assert_eq!(safe_rel_path("src/main.rs").unwrap(), "src/main.rs");
        assert_eq!(safe_rel_path("./a/b.py").unwrap(), "a/b.py");
        assert_eq!(safe_rel_path("src\\main.rs").unwrap(), "src/main.rs");
        assert_eq!(safe_rel_path("a//b/./c.txt").unwrap(), "a/b/c.txt");
    }

    #[test]
    fn rejects_escape_attempts() {
        for bad in [
            "../../etc/passwd",
            "/etc/passwd",
            "C:/Windows/system32/x.dll",
            "c:\\windows\\x",
            "a/../../b",
            "",
            "a<b.txt",
            "..",
        ] {
            assert!(safe_rel_path(bad).is_err(), "应拒绝: {bad}");
        }
    }

    #[test]
    fn parses_object_style_files() {
        let raw = r#"{"files":{"a.py":"print(1)","b.py":"print(2)"},"notes":"ok"}"#;
        let (files, notes) = parse_step_files(raw).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(notes, "ok");
    }

    #[test]
    fn parses_array_style_files_with_alt_keys() {
        let raw = r#"{"files":[{"file":"x/y.py","code":"hi"}],"note":"n"}"#;
        let (files, notes) = parse_step_files(raw).unwrap();
        assert_eq!(files[0].0, "x/y.py");
        assert_eq!(files[0].1, "hi");
        assert_eq!(notes, "n");
    }

    #[test]
    fn empty_files_is_a_noop_not_an_error() {
        // "跑测试确认"这类步骤本来就不写文件 —— 不能当成生成失败
        let (files, notes) = parse_step_files(r#"{"files":[],"notes":"本步只执行验证"}"#).unwrap();
        assert!(files.is_empty());
        assert_eq!(notes, "本步只执行验证");
    }

    #[test]
    fn non_json_output_is_still_an_error() {
        let e = parse_step_files("模型今天不想输出 JSON").unwrap_err();
        assert!(e.contains("不是合法 JSON"), "{e}");
    }

    #[test]
    fn kb_block_goes_into_the_user_prompt_only() {
        // 需求 §2.5：注入内容只进 user 段落，system prompt 里一个字都不许有
        let step = PlanStep {
            id: 1,
            title: "核心模块".into(),
            detail: "实现 add/safe_div".into(),
            files: vec!["mathx.py".into()],
            kind: "code".into(),
        };
        let block =
            "【参考资料 · 本地知识库】\n--- [项目文档] math/既有实现.py#0 ---\n已有 safe_div";
        let u = build_gen_user_prompt("写模块", "{}", &step, &[], 1000, Some(block));
        assert!(u.contains("已有 safe_div"), "{u}");
        assert!(u.contains("核心模块"), "{u}");
        assert!(!plan::GEN_SYSTEM.contains("已有 safe_div"));
        assert!(
            !plan::GEN_SYSTEM.contains("本地知识库"),
            "{:?}",
            plan::GEN_SYSTEM
        );

        let plain = build_gen_user_prompt("写模块", "{}", &step, &[], 1000, None);
        assert!(!plain.contains("本地知识库"), "{plain}");
        // 空块（知识库关着但传了空串）也不该在 prompt 里留下痕迹
        let empty = build_gen_user_prompt("写模块", "{}", &step, &[], 1000, Some("  "));
        assert!(!empty.contains("本地知识库"), "{empty}");
    }

    #[test]
    fn write_file_stays_inside_root() {
        let dir = std::env::temp_dir().join(format!("dh-harness-gen-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let n = write_file(&dir, "pkg/mod.py", "x = 1").unwrap();
        assert_eq!(n, 5);
        assert!(dir.join("pkg").join("mod.py").exists());
        // 第二道防线：即使绕过 safe_rel_path，write_file 也不能写到 root 之外
        assert!(write_file(&dir, "../escape.txt", "x").is_err());
        assert!(!dir.parent().unwrap().join("escape.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
