//! 自我纠正循环：把 lint/verify 的诊断回灌给模型 → 拿到**精确替换** → 落盘 → 重验。
//!
//! 三个刻意的设计决定：
//!
//! 1. **不用 unified diff，用「find/replace + 唯一性校验」**。diff 的上下文匹配
//!    是经典脆弱点（空白、行尾、hunk 偏移都能让 patch 失败），而且失败了很难归因。
//!    `find` 必须**逐字符一致且全文只出现一次**，不满足就整轮作废——
//!    宁可这轮不算，也不要半吊子改坏代码。
//! 2. **一轮是全有或全无（原子）**。任何一条编辑不合法 → 回滚所有改动后报错，
//!    不留"改了一半"的状态（那种状态比不改更难排查）。
//! 3. **停止条件是"违规数必须下降"**，不是"跑满 N 轮"。不下降就停，
//!    否则模型会靠反复微调把 token 烧光而问题不动。

use crate::config::LlmConfig;
use crate::exec::clip;
use crate::generate::safe_rel_path;
use crate::llm::{self, ChatMessage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 一条编辑指令：把 `find` 替换成 `replace`。要求 find 在文件里**恰好出现一次**。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RepairEdit {
    pub path: String,
    pub find: String,
    pub replace: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RepairAttempt {
    #[serde(default)]
    pub edits: Vec<RepairEdit>,
    #[serde(default)]
    pub notes: String,
}

/// 一轮修复的结果快照（UI 要展示"每轮改了什么、违规数怎么变"）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RepairRound {
    pub round: u32,
    pub before: u32,
    pub after: u32,
    pub applied: Vec<String>,
    pub notes: String,
    pub usage_tokens: u64,
    pub elapsed_ms: u128,
    /// ok（改完并重验）| rejected（编辑不合法，本轮作废）| no_progress（违规数没降，停止）
    pub status: String,
    pub detail: String,
}

pub const REPAIR_SYSTEM: &str = r#"你是修复代码的工程师。用户会给你一批静态检查诊断，每条诊断自带"为什么 / 怎么改 / 参考文档"，你只需要照着改。

只输出一个 JSON 对象：
{"edits": [{"path": "相对路径", "find": "要被替换的原文", "replace": "替换成的内容", "reason": "对应哪条诊断"}], "notes": "整体说明"}

硬性要求：
1. `find` 必须是目标文件里**逐字符一致**的片段（包含缩进和标点），并且在文件里**只出现一次**。
   不确定唯一性时，就把上下文多带几行，让它唯一。
2. 每次改动尽量小：不要整文件重写，不要顺手改无关代码，不要新增文件或改文件名。
3. 只修诊断指出的问题。如果某条诊断你判断不该修（疑似误报，或必须由人决定），
   就不要为它编造改动，在 notes 里说明理由。
4. **禁止用"让检查闭嘴"的方式过关**：不允许加 `# noqa`、不允许删测试、不允许把断言改弱、
   不允许把阈值配置调高。这些会被反作弊规则抓出来，比原问题更严重。
5. 如果诊断里给了"建议替换"，优先采用它（applicability 为 MachineApplicable 时基本可以直接用）。"#;

/// 组装给模型的用户消息：任务 + 诊断包 + 上一轮的反馈 + （可选）规则文档。
///
/// 自我纠正阶段的检索查询是**诊断里的规则 ID**（需求 §4）：第二节已经证明
/// "只给诊断就够改"，但给上规则文档正文会更快更稳。知识块同样只进 user 段落。
pub fn build_prompt(
    task: &str,
    package: &str,
    attempt: u32,
    max_rounds: u32,
    prev: Option<&RepairRound>,
    kb_block: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("原始任务：\n{task}\n\n"));
    out.push_str(&format!(
        "这是第 {attempt}/{max_rounds} 轮修复。下面是静态检查报告，逐条修：\n\n{package}\n"
    ));
    if let Some(p) = prev {
        out.push_str(&format!(
            "\n上一轮结果：{} → {} 条违规（{}）。{}\n",
            p.before, p.after, p.status, p.detail
        ));
        if p.after >= p.before {
            out.push_str("上一轮没有让违规数下降，这一轮请换思路：优先处理 error 级、优先用诊断给的“怎么改”。\n");
        }
    }
    if let Some(b) = kb_block
        && !b.trim().is_empty()
    {
        out.push_str(&format!("\n{b}\n"));
    }
    out.push_str("\n只输出 JSON：{\"edits\": [...], \"notes\": \"...\"}");
    out
}

/// 从当前诊断里抽出规则 ID（检索查询用它）。
///
/// 规则 ID 是知识库里最"字面"的锚点：规则文档、约定文档里都会出现同一个 ID，
/// 所以这一步不需要语义检索。
pub fn rule_ids(rec: &crate::workspace::RunRecord) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(l) = &rec.lint {
        for d in &l.diagnostics {
            if !d.rule.trim().is_empty() && !ids.contains(&d.rule) {
                ids.push(d.rule.clone());
            }
        }
    }
    ids.sort();
    ids
}

/// 解析模型的修复建议（容错，兼容 files 形态）。
pub fn parse_attempt(raw: &str) -> Result<RepairAttempt, String> {
    let json = llm::extract_json_object(raw);
    if let Ok(a) = serde_json::from_str::<RepairAttempt>(&json)
        && !a.edits.is_empty()
    {
        return Ok(a);
    }
    // 兼容：模型可能只回 files（整文件重写），把它转成"整文件替换"的编辑
    let v: serde_json::Value = serde_json::from_str(&json)
        .map_err(|e| format!("修复建议不是合法 JSON：{e}；片段：{}", clip(&json, 300)))?;
    let notes = v
        .get("notes")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    let mut edits = Vec::new();
    if let Some(arr) = v.get("files").and_then(|x| x.as_array()) {
        for f in arr {
            let path = f
                .get("path")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let content = f
                .get("content")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if !path.is_empty() && !content.is_empty() {
                edits.push(RepairEdit {
                    path,
                    find: String::new(), // 空 find 表示"整文件重写"
                    replace: content,
                    reason: "整文件替换（模型未按 find/replace 输出）".into(),
                });
            }
        }
    }
    if edits.is_empty() {
        return Err(format!(
            "模型没有给出任何可应用的改动（edits 为空）。notes: {}",
            clip(&notes, 200)
        ));
    }
    Ok(RepairAttempt { edits, notes })
}

/// 应用一批编辑。**原子**：任何一条不合法都不改盘。
///
/// 返回人类可读的改动摘要（用于 UI 与日志）。
pub fn apply_edits(root: &Path, edits: &[RepairEdit]) -> Result<Vec<String>, String> {
    // 1. 全部先校验 + 在内存里推演（同一文件的多条编辑按顺序累积）
    let mut current: HashMap<PathBuf, String> = HashMap::new();
    let mut order: Vec<PathBuf> = Vec::new();
    let mut summary: Vec<String> = Vec::new();

    for (i, e) in edits.iter().enumerate() {
        let rel = safe_rel_path(&e.path)?;
        let path = root.join(&rel);
        if !current.contains_key(&path) {
            let text = std::fs::read_to_string(&path)
                .map_err(|err| format!("第 {} 条编辑：读不到 {} —— {err}", i + 1, rel))?;
            current.insert(path.clone(), text);
            order.push(path.clone());
        }
        let text = current.get(&path).cloned().unwrap_or_default();

        if e.find.is_empty() {
            // 整文件替换：必须与原文不同，否则视为无效
            if text == e.replace {
                return Err(format!(
                    "第 {} 条编辑：整文件替换后内容未变化（{}）",
                    i + 1,
                    rel
                ));
            }
            summary.push(format!(
                "{rel}：整文件替换（{} → {} 字节）",
                text.len(),
                e.replace.len()
            ));
            current.insert(path.clone(), e.replace.clone());
            continue;
        }

        // **行尾归一化**：find 匹配绝不能败给行尾风格。
        //
        // 真机踩到过：主仓库里文件是 LF，但 git 检出到 worktree 时被
        // `core.autocrlf` 转成了 CRLF，于是模型给的（LF）片段一条都匹配不上。
        // 匹配前把两边都归一化成 LF，落盘时再按原文件风格还原。
        let had_crlf = text.contains("\r\n");
        let text_n = text.replace("\r\n", "\n");
        let find_n = e.find.replace("\r\n", "\n");
        let repl_n = e.replace.replace("\r\n", "\n");

        let hits = text_n.matches(&find_n).count();
        if hits != 1 {
            return Err(format!(
                "第 {} 条编辑：find 片段在 {} 中出现 {} 次（要求恰好 1 次）——把上下文多带几行让它唯一。片段：{:?}",
                i + 1,
                rel,
                hits,
                clip(&e.find, 100)
            ));
        }
        let updated_lf = text_n.replacen(&find_n, &repl_n, 1);
        if updated_lf == text_n {
            return Err(format!("第 {} 条编辑：替换前后完全一致（{}）", i + 1, rel));
        }
        let updated = if had_crlf {
            updated_lf.replace('\n', "\r\n")
        } else {
            updated_lf
        };

        summary.push(format!(
            "{rel}：{} → {}（{}）",
            clip(e.find.trim(), 40),
            clip(e.replace.trim(), 40),
            if e.reason.is_empty() {
                "未说明理由"
            } else {
                &e.reason
            }
        ));
        current.insert(path.clone(), updated);
    }

    // 2. 校验全过 → 落盘。写入失败就回滚已写的部分。
    let mut written: Vec<(PathBuf, String)> = Vec::new();
    for path in &order {
        let original = std::fs::read_to_string(path).unwrap_or_default();
        let new = match current.get(path) {
            Some(t) => t.clone(),
            None => continue,
        };
        if new == original {
            continue;
        }
        if let Err(e) = std::fs::write(path, &new) {
            for (p, o) in written.iter().rev() {
                let _ = std::fs::write(p, o);
            }
            return Err(format!(
                "写入 {} 失败：{e}（已回滚本轮全部改动）",
                path.display()
            ));
        }
        written.push((path.clone(), original));
    }
    Ok(summary)
}

/// 让模型对一批诊断给出修复建议。
#[allow(clippy::too_many_arguments)]
pub async fn propose(
    llm_cfg: &LlmConfig,
    task: &str,
    package: &str,
    attempt: u32,
    max_rounds: u32,
    prev: Option<&RepairRound>,
    kb_block: Option<&str>,
) -> Result<(RepairAttempt, u64, u128), String> {
    let user = build_prompt(task, package, attempt, max_rounds, prev, kb_block);
    let out = llm::chat(
        llm_cfg,
        &[ChatMessage::system(REPAIR_SYSTEM), ChatMessage::user(user)],
        true,
    )
    .await?;
    let attempt_parsed = parse_attempt(&out.content)?;
    Ok((attempt_parsed, out.usage.total_tokens, out.elapsed_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "dh-repair-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn write(&self, rel: &str, content: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn edit(path: &str, find: &str, replace: &str) -> RepairEdit {
        RepairEdit {
            path: path.into(),
            find: find.into(),
            replace: replace.into(),
            reason: "测试".into(),
        }
    }

    #[test]
    fn applies_a_single_unambiguous_edit() {
        let d = TempDir::new("one");
        d.write("a.py", "def f(x):\n    if x == None:\n        return 1\n");
        let applied = apply_edits(&d.0, &[edit("a.py", "if x == None:", "if x is None:")]).unwrap();
        assert_eq!(applied.len(), 1);
        let got = std::fs::read_to_string(d.0.join("a.py")).unwrap();
        assert!(got.contains("if x is None:"), "{got}");
    }

    #[test]
    fn rejects_ambiguous_find_and_changes_nothing() {
        let d = TempDir::new("ambig");
        let original = "a = 1\nb = 1\n";
        d.write("a.py", original);
        let err = apply_edits(&d.0, &[edit("a.py", "1", "2")]).unwrap_err();
        assert!(err.contains("出现 2 次"), "{err}");
        assert_eq!(std::fs::read_to_string(d.0.join("a.py")).unwrap(), original);
    }

    #[test]
    fn rejects_path_escape() {
        let d = TempDir::new("escape");
        d.write("a.py", "x = 1\n");
        let err = apply_edits(&d.0, &[edit("../../evil.py", "x", "y")]).unwrap_err();
        assert!(err.contains("越界") || err.contains("拒绝"), "{err}");
    }

    #[test]
    fn atomic_when_one_edit_in_batch_is_bad() {
        let d = TempDir::new("atomic");
        let original = "x = 1\ny = 2\n";
        d.write("a.py", original);
        // 第一条合法、第二条非法 → 谁都不该落盘
        let err = apply_edits(
            &d.0,
            &[
                edit("a.py", "x = 1", "x = 10"),
                edit("a.py", "NOT_PRESENT", "boom"),
            ],
        )
        .unwrap_err();
        assert!(err.contains("出现 0 次"), "{err}");
        assert_eq!(std::fs::read_to_string(d.0.join("a.py")).unwrap(), original);
    }

    #[test]
    fn crlf_file_matches_lf_find_and_keeps_crlf() {
        // 真机 bug 回归：git 的 core.autocrlf 把检出文件变成 CRLF，
        // 而模型给的是 LF 片段 —— 不做归一化就会"一条都匹配不上"。
        let d = TempDir::new("crlf");
        d.write(
            "a.py",
            "def f():
    if x == None:
        return 1
",
        );
        let applied = apply_edits(&d.0, &[edit("a.py", "if x == None:", "if x is None:")]).unwrap();
        assert_eq!(applied.len(), 1);
        let got = std::fs::read_to_string(d.0.join("a.py")).unwrap();
        assert!(got.contains("if x is None:"), "{got:?}");
        assert!(
            got.contains(
                "
"
            ),
            "必须保留原文件的 CRLF 风格：{got:?}"
        );
    }

    #[test]
    fn lf_file_stays_lf() {
        let d = TempDir::new("lf");
        d.write(
            "a.py",
            "x = 1
y = 2
",
        );
        apply_edits(&d.0, &[edit("a.py", "x = 1", "x = 10")]).unwrap();
        let got = std::fs::read_to_string(d.0.join("a.py")).unwrap();
        assert_eq!(
            got,
            "x = 10
y = 2
",
            "不该把 LF 文件变成 CRLF"
        );
    }

    #[test]
    fn multiple_edits_same_file_accumulate() {
        let d = TempDir::new("multi");
        d.write("a.py", "x = 1\ny = 2\n");
        apply_edits(
            &d.0,
            &[
                edit("a.py", "x = 1", "x = 10"),
                edit("a.py", "y = 2", "y = 20"),
            ],
        )
        .unwrap();
        let got = std::fs::read_to_string(d.0.join("a.py")).unwrap();
        assert_eq!(got, "x = 10\ny = 20\n");
    }

    #[test]
    fn whole_file_replacement_is_supported_as_fallback() {
        let d = TempDir::new("whole");
        d.write("a.py", "old\n");
        apply_edits(&d.0, &[edit("a.py", "", "new content\n")]).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.0.join("a.py")).unwrap(),
            "new content\n"
        );
    }

    #[test]
    fn parse_attempt_accepts_edits_shape() {
        let raw = r#"{"edits":[{"path":"a.py","find":"x = 1","replace":"x = 2","reason":"HX204"}],"notes":"ok"}"#;
        let a = parse_attempt(raw).unwrap();
        assert_eq!(a.edits.len(), 1);
        assert_eq!(a.notes, "ok");
    }

    #[test]
    fn parse_attempt_falls_back_to_files_shape() {
        let raw = r#"{"files":[{"path":"a.py","content":"print(1)"}],"notes":"整文件"}"#;
        let a = parse_attempt(raw).unwrap();
        assert_eq!(a.edits.len(), 1);
        assert!(a.edits[0].find.is_empty());
    }

    #[test]
    fn parse_attempt_errors_when_nothing_actionable() {
        let err = parse_attempt(r#"{"edits":[],"notes":"我认为都是误报"}"#).unwrap_err();
        assert!(err.contains("误报"), "{err}");
    }

    #[test]
    fn system_prompt_warns_against_noqa_gaming() {
        // 反作弊条款在 system prompt 里（不是在每轮的 user prompt 里）
        assert!(REPAIR_SYSTEM.contains("禁止"), "{REPAIR_SYSTEM}");
        assert!(REPAIR_SYSTEM.contains("noqa"), "必须明确禁止用 noqa 糊过去");
        assert!(REPAIR_SYSTEM.contains("删测试"), "必须明确禁止删测试换通过");
        assert!(REPAIR_SYSTEM.contains("调高"), "必须明确禁止调高阈值换通过");
    }

    #[test]
    fn user_prompt_carries_task_and_diagnostics() {
        let p = build_prompt("写个加法器", "HX201 ...", 1, 2, None, None);
        assert!(p.contains("写个加法器"), "{p}");
        assert!(p.contains("HX201"), "{p}");
        assert!(p.contains("第 1/2 轮"), "{p}");
        // 没有知识库时不该留下任何注入痕迹
        assert!(!p.contains("参考资料"), "{p}");
    }

    #[test]
    fn rule_docs_go_into_the_user_prompt_only() {
        let kb = "【参考资料 · 本地知识库】\n--- [项目文档] doc/规则.md#4 ---\nHX201：异常不许吞掉，要写明处理方式";
        let p = build_prompt("t", "HX201 at a.py:3", 1, 2, None, Some(kb));
        assert!(p.contains("HX201：异常不许吞掉"), "{p}");
        assert!(!REPAIR_SYSTEM.contains("异常不许吞掉"), "{REPAIR_SYSTEM}");
        assert!(!REPAIR_SYSTEM.contains("本地知识库"), "{REPAIR_SYSTEM}");
    }

    #[test]
    fn prompt_mentions_no_progress_when_previous_round_stalled() {
        let prev = RepairRound {
            round: 1,
            before: 5,
            after: 5,
            status: "no_progress".into(),
            ..Default::default()
        };
        let p = build_prompt("t", "diag", 2, 2, Some(&prev), None);
        assert!(p.contains("没有让违规数下降"), "{p}");
    }
}
