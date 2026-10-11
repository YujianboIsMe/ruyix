//! 本 run 的**可检索留痕**（v1.5 第 1 服「甸服」的正文来源）。
//!
//! ## 为什么需要它（2026-10-09 用户场景逼出来的）
//!
//! 用户要问的是「我一个会话里第 1 个 run 和第 3 个 run 关于【user 表 password 字段长度】有冲突吗？」
//! —— 核过代码后发现**要搜的东西压根没被存下来**：
//!
//! - `sessions/<id>.json` 里只有 `role/text`（用户提示词与最终回答）与**派生品**（`trace` 一行摘要、
//!   `findings` 结论、`plan`/`verify`/`reflect`）；工具调用的**输入与结果原文**一个字段都没有；
//! - capsule 侧存只留"被折掉 / 被去重命中要还回去"的那部分结果，且**一 run 一目录**；
//! - 于是第 1 服此前只能搜到"本 run 里已经离开上下文的一小块"，跨 run 更是一点都搜不到。
//!
//! 所以第 1 服的正文得**自己存一份**：`<状态根>/ctx/<run-id>/transcript.jsonl`，一行一条，只追加。
//!
//! ## 形状（每行一个 JSON）
//!
//! ```jsonl
//! {"kind":"prompt","text":"…"}                                  // 这一 run 的提问
//! {"kind":"call","tool":"execute","brief":"cargo test","ok":false,"len":812,"result":"…"}
//! {"kind":"final","text":"…"}
//! {"kind":"capped","bytes":1000123}                             // 到顶了：如实留痕，不静默丢
//! ```
//!
//! ## 三条纪律
//!
//! 1. **绝不写用户仓库**：落状态根（与 capsule 同目录），与 findings 落盘同一处；
//! 2. **有界**：单条结果裁 [`MAX_ENTRY_BYTES`]（记全文长度，不假装完整），整文件到
//!    [`MAX_FILE_BYTES`] 就停写并留一条 `capped`。留痕是给检索用的，不是备份；
//! 3. **检索不写盘**：`search_in` 只读；读不回来就是不命中，**不编**。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::capsule::Capsule;

/// 留痕文件名（与 capsule 的 `index.jsonl` 同目录、互不干扰：capsule 管"召回"，这里管"检索"）
pub const FILE: &str = "transcript.jsonl";
/// 单条结果最多记这么多字节（超了记全文长度 + 截断标记）
pub const MAX_ENTRY_BYTES: usize = 4096;
/// 整份留痕的上限（到顶就停写并留 `capped`；一个 run 的正常留痕远小于它）
pub const MAX_FILE_BYTES: usize = 1_000_000;

/// 一次检索命中（行号 + 这条是什么 + 正文）
#[derive(Debug)]
pub struct Hit {
    /// 留痕文件里的行号（从 1 起；给人看，也是"去翻原文"的路标）
    pub line: usize,
    /// 给人看的标签：`提问` / `工具 execute` / `工具 execute 的结果` / `回答`
    pub label: String,
    /// 命中的那一行（已裁剪）
    pub text: String,
}

/// 一个 run 的留痕文件句柄（只追加）。
#[derive(Debug)]
pub struct Transcript {
    dir: PathBuf,
    file: PathBuf,
    bytes: usize,
    capped: bool,
}

impl Transcript {
    /// 给**本 run** 开一份留痕（目录已存在也行）。
    pub fn create(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let file = dir.join(FILE);
        // 只追加：进程重启后同一个 run-id 的目录不会复用（run-id 唯一），但防御性读一下大小
        let bytes = fs::metadata(&file).map(|m| m.len() as usize).unwrap_or(0);
        Ok(Self {
            dir: dir.to_path_buf(),
            file,
            bytes,
            capped: bytes >= MAX_FILE_BYTES,
        })
    }

    /// 指向**已有的**某条臂/某个 run 的留痕目录（检索前几个 run 时用）。
    pub fn at(dir: PathBuf) -> Self {
        let file = dir.join(FILE);
        let bytes = fs::metadata(&file).map(|m| m.len() as usize).unwrap_or(0);
        Self {
            dir,
            file,
            bytes,
            capped: true, // `at` 只用于读：不给它写的机会
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// **这一 run 的提问**（主循环开头记一次）。
    pub fn prompt(&mut self, text: &str) {
        self.append(serde_json::json!({ "kind": "prompt", "text": text }));
    }

    /// **一次工具调用与它的结果**（每次回灌观测前后记一条：结果原文就是检索要用的正文）。
    pub fn call(&mut self, tool: &str, brief: &str, ok: bool, result: &str) {
        let (body, truncated) = clip(result);
        self.append(serde_json::json!({
            "kind": "call",
            "tool": tool,
            "brief": brief,
            "ok": ok,
            "len": result.len(),
            "truncated": truncated,
            "result": body,
        }));
    }

    /// **交付的 final**（被门禁接受的那一条才算；没收下就没有它）。
    pub fn final_text(&mut self, text: &str) {
        self.append(serde_json::json!({ "kind": "final", "text": text }));
    }

    fn append(&mut self, v: serde_json::Value) {
        if self.capped {
            return;
        }
        let line = v.to_string();
        let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
        else {
            return; // 留痕建不出来不该影响这一 run（与 capsule 同款：降级而不是失败）
        };
        if writeln!(f, "{line}").is_err() {
            return;
        }
        self.bytes += line.len() + 1;
        if self.bytes >= MAX_FILE_BYTES {
            self.capped = true;
            let _ = writeln!(
                f,
                "{}",
                serde_json::json!({ "kind": "capped", "bytes": self.bytes })
            );
        }
    }
}

/// 把结果裁到 [`MAX_ENTRY_BYTES`]（按**字符边界**裁，不切碎 UTF-8）。
/// 返回 `(裁剪后的正文, 是否裁过)`；裁过时在结尾留下"原文多少字节"的实话。
fn clip(s: &str) -> (String, bool) {
    if s.len() <= MAX_ENTRY_BYTES {
        return (s.to_string(), false);
    }
    let mut end = MAX_ENTRY_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (
        format!("{}…[留痕截断：原文 {} 字节]", &s[..end], s.len()),
        true,
    )
}

/// 在**某一 run 的留痕**里按内容找（大小写不敏感的字面匹配，与第 4 服 `files` 同一套判法）。
///
/// 读不回来（文件不存在 / 行坏了）就跳过那一条 —— 留痕是**补充证据**，不是必须可用的库。
pub fn search_in(dir: &Path, q: &str, limit: usize) -> Vec<Hit> {
    let needle = q.to_lowercase();
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let Ok(text) = fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        if out.len() >= limit {
            break;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        // 一行的**可搜字段**：工具行与它的结果分开算 —— 命中哪一段就把哪一段带回去
        // （混成一个字符串会让人以为"结果里也有那个词"）。
        let fields: Vec<(String, &str)> = match kind {
            "prompt" => vec![(
                "提问".to_string(),
                v.get("text").and_then(|t| t.as_str()).unwrap_or(""),
            )],
            "final" => vec![(
                "回答".to_string(),
                v.get("text").and_then(|t| t.as_str()).unwrap_or(""),
            )],
            "call" => {
                let tool = v.get("tool").and_then(|t| t.as_str()).unwrap_or("?");
                let brief = v.get("brief").and_then(|t| t.as_str()).unwrap_or("");
                let result = v.get("result").and_then(|t| t.as_str()).unwrap_or("");
                vec![
                    (format!("工具 {tool}"), brief),
                    (format!("工具 {tool} 的结果"), result),
                ]
            }
            _ => continue,
        };
        for (label, hay) in fields {
            if out.len() >= limit {
                break;
            }
            if hay.to_lowercase().contains(&needle) {
                out.push(Hit {
                    line: i + 1,
                    label,
                    text: hay.chars().take(200).collect(),
                });
            }
        }
    }
    out
}

/// **版本源**：参与这次检索的那几份留痕的 `(目录名, 字节数, mtime)` 组合指纹。
///
/// 为什么不是单个文件：跨 run 之后正文来自多份留痕，任何一份变了（尤其是**本 run 的那份还在长**）
/// 结论就可能变 ⇒ 必须把整组算进去。拿不到 mtime 的项按 `0` 记（宁可版本变，不可假新鲜）。
pub fn version_of(dirs: &[(String, PathBuf)]) -> String {
    let mut parts = Vec::new();
    for (label, d) in dirs {
        let f = d.join(FILE);
        let (size, mtime) = match fs::metadata(&f) {
            Ok(md) => (
                md.len(),
                md.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis())
                    .unwrap_or(0),
            ),
            Err(_) => (0, 0), // 这份还没有留痕：也算进指纹（「过去没有、现在有了」必须让版本变）
        };
        parts.push(format!("{}|{}|{}", label, size, mtime));
    }
    Capsule::sha256(&parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rsib-tr-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn records_prompt_calls_and_final_and_finds_them() {
        let d = tmp("basic");
        let mut t = Transcript::create(&d).unwrap();
        t.prompt("user 表的 password 字段长度是多少？");
        t.call(
            "execute",
            "cargo test",
            false,
            "error: password 字段应为 varchar(72)",
        );
        t.call("read", "src/db.rs", true, "password varchar(60)");
        t.final_text("结论：password 字段两处不一致（60 vs 72）。");
        assert!(d.join(FILE).is_file());

        let hits = search_in(&d, "PASSWORD", 10); // 大小写不敏感
        assert_eq!(hits.len(), 4, "提问 / 两次工具结果 / 回答各一条");
        assert_eq!(hits[0].label, "提问", "最先命中是提问");
        assert!(
            hits.iter().any(|h| h.label.contains("execute")),
            "工具那一条要带工具名"
        );
        assert!(hits.iter().any(|h| h.label == "回答"), "final 也要能被搜到");
        assert!(
            hits.iter().any(|h| h.label.contains("的结果")),
            "结果命中要标明是结果"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_long_result_is_clipped_and_says_so() {
        let d = tmp("clip");
        let mut t = Transcript::create(&d).unwrap();
        let big = "x".repeat(MAX_ENTRY_BYTES * 3);
        t.call("execute", "big", true, &big);
        let text = fs::read_to_string(d.join(FILE)).unwrap();
        assert!(text.contains("留痕截断：原文"), "裁了必须留实话");
        assert!(text.len() < big.len(), "确实裁了");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn version_changes_when_the_transcript_grows() {
        let d = tmp("ver");
        let mut t = Transcript::create(&d).unwrap();
        let label = ("本 run".to_string(), d.clone());
        let v1 = version_of(std::slice::from_ref(&label));
        t.call("read", "a", true, "hello");
        let v2 = version_of(std::slice::from_ref(&label));
        assert_ne!(
            v1, v2,
            "本 run 的留痕在长 ⇒ 版本必变（自失效方向，与 capsule 那条一致）"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_transcript_is_an_empty_answer_not_an_error() {
        let d = tmp("missing");
        assert!(search_in(&d, "x", 10).is_empty());
    }
}
