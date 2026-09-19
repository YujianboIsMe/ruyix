//! 自我纠正循环：把 lint/verify 的诊断回灌给模型 → 拿到**精确替换** → 落盘 → 重验。
//!
//! 四个刻意的设计决定：
//!
//! 1. **不用 unified diff，用「find/replace + 唯一性校验」**。diff 的上下文匹配
//!    是经典脆弱点（空白、行尾、hunk 偏移都能让 patch 失败），而且失败了很难归因。
//!    `find` 必须**逐字符一致且全文只出现一次**，不满足就整轮作废——
//!    宁可这轮不算，也不要半吊子改坏代码。
//! 2. **一轮是全有或全无（原子）**。任何一条编辑不合法 → 回滚所有改动后报错，
//!    不留"改了一半"的状态（那种状态比不改更难排查）。
//! 3. **停止条件是"违规数必须下降"**，不是"跑满 N 轮"。不下降就停，
//!    否则模型会靠反复微调把 token 烧光而问题不动。
//! 4. **看不准就追问，不瞎猜**（`{"need": [...]}` 协议）。模型缺上下文时可以
//!    要文件/目录内容，harness 读出来附给下一轮；追问不占修复轮次但有独立上限。
//!    唯一的"越界"例外：报告说文件不存在而它本该存在时，允许用整文件形态**创建**
//!    （生成阶段失败的步骤，产物压根没落盘，find/replace 永远修不了缺失）。

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
    /// 追问：模型信息不足时不开瞎猜的补丁，而是列出想看的文件/目录路径，
    /// 由 harness 读出内容补给下一轮（`need` 非空时 `edits` 应为空）。
    #[serde(default)]
    pub need: Vec<String>,
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

pub const REPAIR_SYSTEM: &str = r#"你是修复代码的工程师。用户会给你一批检查/测试诊断（含真实 stderr），你给出精确修复。

只输出一个 JSON 对象，两种形态二选一：

一、可以直接改（默认）：
{"edits": [{"path": "相对路径", "find": "要被替换的原文", "replace": "替换成的内容", "reason": "对应哪条诊断"}], "notes": "整体说明"}

二、信息不足，先追问再改：
{"need": ["想看的相对文件路径或目录路径"], "notes": "缺什么信息、为什么要"}
系统会把你要的内容附在下一轮给你。**看不准就不要猜**——猜出来的 find 匹配不上会让整轮作废。

硬性要求：
1. `find` 必须是目标文件里**逐字符一致**的片段（包含缩进和标点），并且在文件里**只出现一次**。
   不确定唯一性时，就把上下文多带几行，让它唯一。
2. 每次改动尽量小：不要整文件重写，不要顺手改无关代码，不要改文件名。
   例外：报告说文件不存在（ModuleNotFoundError / No such file / No module named 等）而它本该存在时，
   用 files 形态直接给出完整内容创建它：{"files": [{"path": "...", "content": "完整文件内容"}]}。
3. 只修诊断指出的问题。如果某条诊断你判断不该修（疑似误报，或必须由人决定），
   就不要为它编造改动，在 notes 里说明理由。
4. **禁止用"让检查闭嘴"的方式过关**：不允许加 `# noqa`、不允许删测试、不允许把断言改弱、
   不允许把阈值配置调高。这些会被反作弊规则抓出来，比原问题更严重。
5. 如果诊断里给了"建议替换"，优先采用它（applicability 为 MachineApplicable 时基本可以直接用）。"#;

/// 组装给模型的用户消息：任务 + 诊断包 + 上一轮的反馈 + 追问来的上下文 + （可选）规则文档。
///
/// 自我纠正阶段的检索查询是**诊断里的规则 ID**（需求 §4）：第二节已经证明
/// "只给诊断就够改"，但给上规则文档正文会更快更稳。知识块同样只进 user 段落。
#[allow(clippy::too_many_arguments)]
pub fn build_prompt(
    task: &str,
    package: &str,
    attempt: u32,
    max_rounds: u32,
    prev: Option<&RepairRound>,
    context_block: Option<&str>,
    kb_block: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("原始任务：\n{task}\n\n"));
    out.push_str(&format!(
        "这是第 {attempt}/{max_rounds} 轮修复。下面是检查/测试报告，逐条修：\n\n{package}\n"
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
    if let Some(c) = context_block
        && !c.trim().is_empty()
    {
        out.push_str(&format!("\n【你上一轮追问的内容】\n{c}\n"));
    }
    if let Some(b) = kb_block
        && !b.trim().is_empty()
    {
        out.push_str(&format!("\n{b}\n"));
    }
    out.push_str("\n只输出 JSON：{\"edits\": [...], \"notes\": \"...\"}，或信息不足时 {\"need\": [...], \"notes\": \"...\"}");
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

/// 解析模型的修复建议（容错，兼容 files / need 形态）。
pub fn parse_attempt(raw: &str) -> Result<RepairAttempt, String> {
    let json = llm::extract_json_object(raw);
    if let Ok(a) = serde_json::from_str::<RepairAttempt>(&json)
        && (!a.edits.is_empty() || !a.need.is_empty())
    {
        return Ok(a);
    }
    let v: serde_json::Value = serde_json::from_str(&json)
        .map_err(|e| format!("修复建议不是合法 JSON：{e}；片段：{}", clip(&json, 300)))?;
    let notes = v
        .get("notes")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    // 追问形态：模型要看文件/目录内容才肯动手
    let need = v
        .get("need")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

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
                    find: String::new(), // 空 find 表示"整文件替换"
                    replace: content,
                    reason: "整文件替换（模型未按 find/replace 输出）".into(),
                });
            }
        }
    }
    if edits.is_empty() && need.is_empty() {
        return Err(format!(
            "模型没有给出任何可应用的改动（edits/need 均为空）。notes: {}",
            clip(&notes, 200)
        ));
    }
    Ok(RepairAttempt { edits, notes, need })
}

/// find/replace 的匹配核心（`apply_edits` 与 agent 工具循环的 Edit 工具共用同一套规则）：
/// 行尾归一化（CRLF 归 LF 匹配、按原文风格还原）、find 必须恰好出现一次、必须产生变化。
pub(crate) fn replace_unique(text: &str, find: &str, replace: &str) -> Result<String, String> {
    let had_crlf = text.contains("\r\n");
    let text_n = text.replace("\r\n", "\n");
    let find_n = find.replace("\r\n", "\n");
    let repl_n = replace.replace("\r\n", "\n");

    let hits = text_n.matches(&find_n).count();
    if hits != 1 {
        return Err(format!(
            "find 片段出现 {hits} 次（要求恰好 1 次）——把上下文多带几行让它唯一。片段：{:?}",
            clip(find, 100)
        ));
    }
    let updated_lf = text_n.replacen(&find_n, &repl_n, 1);
    if updated_lf == text_n {
        return Err("替换前后完全一致".into());
    }
    Ok(if had_crlf {
        updated_lf.replace('\n', "\r\n")
    } else {
        updated_lf
    })
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
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                // 文件不存在：只有"整文件"形态允许走到这里 = 创建缺失文件
                // （典型场景：某个生成步骤失败、文件根本没落盘，测试报 ModuleNotFoundError）
                Err(_) if e.find.is_empty() => String::new(),
                Err(err) => return Err(format!("第 {} 条编辑：读不到 {} —— {err}", i + 1, rel)),
            };
            current.insert(path.clone(), text);
            order.push(path.clone());
        }
        let text = current.get(&path).cloned().unwrap_or_default();

        if e.find.is_empty() {
            // 整文件替换/创建：必须与原文不同，否则视为无效
            if text == e.replace {
                return Err(format!(
                    "第 {} 条编辑：整文件替换后内容未变化（{}）",
                    i + 1,
                    rel
                ));
            }
            summary.push(format!(
                "{rel}：{}（{} 字节）",
                if text.is_empty() {
                    "创建新文件"
                } else {
                    "整文件替换"
                },
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
        let updated = match replace_unique(&text, &e.find, &e.replace) {
            Ok(u) => u,
            Err(reason) => return Err(format!("第 {} 条编辑：{reason}（{rel}）", i + 1)),
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

    // 2. 校验全过 → 落盘。写入失败就回滚已写的部分（新建的文件直接删除）。
    let mut written: Vec<(PathBuf, String)> = Vec::new();
    let mut created: Vec<PathBuf> = Vec::new();
    for path in &order {
        let existed = path.exists();
        let original = std::fs::read_to_string(path).unwrap_or_default();
        let new = match current.get(path) {
            Some(t) => t.clone(),
            None => continue,
        };
        if new == original {
            continue;
        }
        if !existed && let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, &new) {
            for (p, o) in written.iter().rev() {
                let _ = std::fs::write(p, o);
            }
            for p in created.iter().rev() {
                let _ = std::fs::remove_file(p);
            }
            return Err(format!(
                "写入 {} 失败：{e}（已回滚本轮全部改动）",
                path.display()
            ));
        }
        if existed {
            written.push((path.clone(), original));
        } else {
            created.push(path.clone());
        }
    }
    Ok(summary)
}

/// 读取模型追问的文件/目录内容，拼成下一轮的上下文块。
///
/// - 文件：全文（按单文件/总预算裁剪）
/// - 目录：递归列出条目（只列名字和大小，不展开内容）
/// - 不存在：明说，并提示可用 files 形态直接创建它
///
/// 路径全部经 `safe_rel_path` 封闭在产物目录内（追问不能变成任意读）。
pub fn gather_context(root: &Path, need: &[String], budget: usize) -> String {
    const PER_FILE: usize = 4_000;
    const MAX_ENTRIES: usize = 10;
    let mut used = 0usize;
    let mut out = String::new();
    for raw in need.iter().take(MAX_ENTRIES) {
        if used >= budget {
            out.push_str("\n（上下文预算已用完，剩余条目省略）\n");
            break;
        }
        let rel = match safe_rel_path(raw) {
            Ok(r) => r,
            Err(e) => {
                out.push_str(&format!("\n--- {raw}：路径被拒绝（{e}）---\n"));
                continue;
            }
        };
        let p = root.join(&rel);
        if p.is_dir() {
            out.push_str(&format!("\n--- 目录 {rel}/ ---\n"));
            let mut n = 0usize;
            list_dir(&p, &rel, 0, &mut n, &mut out);
        } else if p.is_file() {
            let content =
                std::fs::read_to_string(&p).unwrap_or_else(|e| format!("<读取失败：{e}>"));
            let room = budget.saturating_sub(used).clamp(200, PER_FILE);
            let clipped = clip(&content, room);
            used += clipped.len();
            out.push_str(&format!("\n--- {rel} ---\n{clipped}\n"));
        } else {
            out.push_str(&format!(
                "\n--- {raw}：沙箱里不存在这个文件。如果测试需要它，请用 files 形态直接给出完整内容创建它。---\n"
            ));
        }
    }
    out
}

/// 目录列表（深度 ≤ 3、每目录 ≤ 60 条 —— 够模型定位文件，不灌爆 prompt）。
/// `rel` 为空串 = 列根目录（agent 工具循环的 `read "."`）。
pub(crate) fn list_dir(dir: &Path, rel: &str, depth: usize, count: &mut usize, out: &mut String) {
    const MAX_LIST: usize = 60;
    if depth > 3 || *count >= MAX_LIST {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if *count >= MAX_LIST {
            out.push_str("  …（条目过多已截断）\n");
            return;
        }
        *count += 1;
        let name = e.file_name().to_string_lossy().to_string();
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if e.path().is_dir() {
            out.push_str(&format!("  {child_rel}/\n"));
            list_dir(&e.path(), &child_rel, depth + 1, count, out);
        } else {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            out.push_str(&format!("  {child_rel}（{size} B）\n"));
        }
    }
}

/// 让模型对一批诊断给出修复建议（或追问更多上下文）。
#[allow(clippy::too_many_arguments)]
pub async fn propose(
    llm_cfg: &LlmConfig,
    task: &str,
    package: &str,
    attempt: u32,
    max_rounds: u32,
    prev: Option<&RepairRound>,
    context_block: Option<&str>,
    kb_block: Option<&str>,
) -> Result<(RepairAttempt, u64, u128), String> {
    let user = build_prompt(
        task,
        package,
        attempt,
        max_rounds,
        prev,
        context_block,
        kb_block,
    );
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

    /// 追问与创建是修复循环能自救的两条路：prompt 契约必须写清楚
    #[test]
    fn system_prompt_defines_need_and_create_protocols() {
        assert!(REPAIR_SYSTEM.contains("\"need\""), "必须定义追问协议");
        assert!(
            REPAIR_SYSTEM.contains("看不准就不要猜"),
            "必须鼓励追问而非瞎猜"
        );
        assert!(
            REPAIR_SYSTEM.contains("文件不存在"),
            "必须说明缺文件时可以用 files 形态创建"
        );
    }

    #[test]
    fn user_prompt_carries_task_and_diagnostics() {
        let p = build_prompt("写个加法器", "HX201 ...", 1, 2, None, None, None);
        assert!(p.contains("写个加法器"), "{p}");
        assert!(p.contains("HX201"), "{p}");
        assert!(p.contains("第 1/2 轮"), "{p}");
        // 没有知识库时不该留下任何注入痕迹
        assert!(!p.contains("参考资料"), "{p}");
    }

    #[test]
    fn rule_docs_go_into_the_user_prompt_only() {
        let kb = "【参考资料 · 本地知识库】\n--- [项目文档] doc/规则.md#4 ---\nHX201：异常不许吞掉，要写明处理方式";
        let p = build_prompt("t", "HX201 at a.py:3", 1, 2, None, None, Some(kb));
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
        let p = build_prompt("t", "diag", 2, 2, Some(&prev), None, None);
        assert!(p.contains("没有让违规数下降"), "{p}");
    }

    /// 上一轮追问来的内容必须进 user 段落（且绝不进 system prompt）
    #[test]
    fn asked_context_lands_in_user_prompt() {
        let ctx = "--- tests/test_add.py ---\ndef test_add():\n    assert add(1, 2) == 3\n";
        let p = build_prompt("t", "diag", 2, 5, None, Some(ctx), None);
        assert!(p.contains("你上一轮追问的内容"), "{p}");
        assert!(p.contains("test_add.py"), "{p}");
        assert!(!REPAIR_SYSTEM.contains("追问的内容"));
    }

    #[test]
    fn parse_attempt_accepts_need_shape() {
        let raw = r#"{"need":["tests/test_add.py","src/"],"notes":"要看断言和目录结构"}"#;
        let a = parse_attempt(raw).unwrap();
        assert!(a.edits.is_empty());
        assert_eq!(a.need, vec!["tests/test_add.py", "src/"]);
        assert_eq!(a.notes, "要看断言和目录结构");
    }

    /// files 形态 + 不存在的文件 = 创建（生成失败的步骤产物没落盘时的自救路径）
    #[test]
    fn apply_edits_creates_missing_file_with_parent_dirs() {
        let d = TempDir::new("create");
        apply_edits(
            &d.0,
            &[RepairEdit {
                path: "src/todo.py".into(),
                find: String::new(),
                replace: "def add(a, b):\n    return a + b\n".into(),
                reason: "测试报 ModuleNotFoundError".into(),
            }],
        )
        .unwrap();
        let got = std::fs::read_to_string(d.0.join("src/todo.py")).unwrap();
        assert!(got.contains("def add"), "{got}");
    }

    /// 创建与修改混编时仍保持原子：后一条校验失败 → 新文件一个都不能出现
    #[test]
    fn creation_is_atomic_with_other_edits() {
        let d = TempDir::new("atomic-create");
        d.write("keep.py", "x = 1\n");
        let err = apply_edits(
            &d.0,
            &[
                RepairEdit {
                    path: "new_missing.py".into(),
                    find: String::new(),
                    replace: "print(1)\n".into(),
                    reason: "创建".into(),
                },
                edit("keep.py", "NOT_PRESENT", "boom"),
            ],
        )
        .unwrap_err();
        assert!(err.contains("出现 0 次"), "{err}");
        assert!(
            !d.0.join("new_missing.py").exists(),
            "校验失败的创建不得落盘"
        );
        assert_eq!(
            std::fs::read_to_string(d.0.join("keep.py")).unwrap(),
            "x = 1\n"
        );
    }

    /// find 非空但文件不存在 = 还是错误（不允许用 find/replace 凭空建文件）
    #[test]
    fn find_edit_on_missing_file_is_rejected() {
        let d = TempDir::new("find-missing");
        let err = apply_edits(&d.0, &[edit("ghost.py", "a", "b")]).unwrap_err();
        assert!(err.contains("读不到"), "{err}");
        assert!(!d.0.join("ghost.py").exists());
    }

    /// 追问的内容收集：文件给内容、目录给列表、不存在的明说并提示可创建
    #[test]
    fn gather_context_covers_file_dir_and_missing() {
        let d = TempDir::new("gather");
        d.write("src/todo.py", "def add(a, b):\n    return a + b\n");
        d.write("src/README.md", "# notes\n");
        let out = gather_context(
            &d.0,
            &[
                "src/todo.py".into(),
                "src".into(),
                "ghost.py".into(),
                "../../escape.py".into(),
            ],
            12_000,
        );
        assert!(out.contains("--- src/todo.py ---"), "{out}");
        assert!(out.contains("def add"), "{out}");
        assert!(out.contains("目录 src/"), "{out}");
        assert!(out.contains("README.md"), "{out}");
        assert!(out.contains("ghost.py：沙箱里不存在"), "{out}");
        assert!(out.contains("路径被拒绝"), "{out}");
    }

    /// 追问路径必须封闭在产物目录内 —— 不能变成任意文件读取
    #[test]
    fn gather_context_rejects_path_escape() {
        let d = TempDir::new("gather-escape");
        let out = gather_context(&d.0, &["../../../etc/passwd".into()], 12_000);
        assert!(out.contains("路径被拒绝"), "{out}");
    }
}
