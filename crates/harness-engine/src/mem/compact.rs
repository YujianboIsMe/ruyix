//! **转录压实**（切片 3）：会话历史超预算时怎么压 —— **压了就必须留收据**。
//!
//! 为什么这件事归记忆管：论文里"注意力层"是有界的视图，逐出**不等于销毁**；而我们此前
//! 对会话的做法是**什么都不做** —— 历史一路长下去，直到模型自己开始丢东西（那才是真丢，
//! 而且没有任何痕迹）。这里补的是"要压的时候，怎么压得可追溯"：
//!
//! 三条纪律：
//!
//! 1. **抽走的不改写**：摘要只取**逐字前缀**（`[user] 帮我改一下 pom.xml 的…`），
//!    不做生成式总结 —— 生成式总结会引入从未说过的话，那是伪造证据；
//! 2. **最新一句必须原样在**：把刚说的话压掉，模型下一轮就会答非所问；
//! 3. **留下换回的坐标**：收据里写明"抽走的是原历史的第 a..b 条"，原话仍在会话文件里，
//!    按坐标就能取回（`rehydrate`）。收据说的是"注意力丢了什么"，不是"账本丢了什么"。

use crate::agent::HistoryMsg;

use super::{Memory, retrieve};

/// 默认预算（字符）。24000 字符 ≈ 8k token 量级，够放几十轮；调大也只是晚点压。
pub const DEFAULT_BUDGET_CHARS: usize = 24_000;

/// 被抽走条目在收据里保留的逐字前缀长度（每条）。
const PER_LINE_CHARS: usize = 90;

/// 收据里 `dropped` 的总字符上限 —— 收据本身不能又变成一个无界的东西。
const EXTRACT_CAP_CHARS: usize = 900;

/// 一次压实的结论。
#[derive(Debug, Clone)]
pub struct Compaction {
    /// 保留的原文（最近的若干轮，顺序与原历史一致）
    pub kept: Vec<HistoryMsg>,
    /// 被抽走的条数
    pub dropped: usize,
    /// 被抽走的第一条/最后一条在原历史里的下标（**换回的坐标**，1-based 便于人读）
    pub from_index: usize,
    pub to_index: usize,
    /// 被抽走条目的**逐字前缀**（不改写、不总结）
    pub extractive: Vec<String>,
}

impl Compaction {
    pub fn happened(&self) -> bool {
        self.dropped > 0
    }
}

/// 按预算压实会话历史：**从最新往回吃**，吃到预算为止。
///
/// 边界行为：预算极小（哪怕一条都放不下）也**至少保留最新一条** —— 宁可超预算也不许
/// 把"用户刚说的话"压掉；那是压实的意义反噬。
pub fn compact_history(history: &[HistoryMsg], budget_chars: usize) -> Compaction {
    let mut kept_rev: Vec<HistoryMsg> = Vec::new();
    let mut used = 0usize;
    for m in history.iter().rev() {
        let cost = m.text.chars().count() + m.role.chars().count() + 8;
        if used + cost > budget_chars && !kept_rev.is_empty() {
            break;
        }
        used += cost;
        kept_rev.push(m.clone());
    }
    kept_rev.reverse();
    let dropped = history.len().saturating_sub(kept_rev.len());
    let mut extractive: Vec<String> = Vec::new();
    if dropped > 0 {
        let mut budget = EXTRACT_CAP_CHARS;
        for m in history.iter().take(dropped) {
            let head: String = m.text.chars().take(PER_LINE_CHARS).collect();
            let ellipsis = if m.text.chars().count() > PER_LINE_CHARS {
                "…"
            } else {
                ""
            };
            let line = format!("[{}] {head}{ellipsis}", m.role);
            if line.chars().count() > budget {
                extractive.push(format!(
                    "…（还有 {} 条，见会话文件）",
                    dropped - extractive.len()
                ));
                break;
            }
            budget -= line.chars().count();
            extractive.push(line);
        }
    }
    Compaction {
        kept: kept_rev,
        dropped,
        from_index: if dropped == 0 { 0 } else { 1 },
        to_index: dropped,
        extractive,
    }
}

/// 把这次压实写进账本（**收据**），并回一句可以直接注入提示词的话。
///
/// `session_ref` 是会话文件/会话 id —— **换回的办法就是它**：转录不复制进账本（那会让账本
/// 无限膨胀，也与"零残留/绿色"冲突），账本只记"哪一段被注意力丢了、去哪儿拿回来"。
pub fn record_compaction(
    mem: &Memory,
    scope: &str,
    session_ref: &str,
    c: &Compaction,
) -> Result<String, String> {
    if !c.happened() {
        return Ok(String::new());
    }
    let covered = vec![format!(
        "{session_ref} 第 {}..{} 条",
        c.from_index, c.to_index
    )];
    let rehydrate = format!(
        "打开会话 {session_ref} 的第 {}..{} 条（原话一字未动）",
        c.from_index, c.to_index
    );
    let note = format!("历史超出预算，压实了 {} 条", c.dropped);
    retrieve::add_receipt(mem, scope, KIND, &covered, &c.extractive, &rehydrate, &note)?;
    Ok(format!(
        "[转录压实] 本轮已把较早的 {} 条历史移出上下文（原话未删）：换回办法 —— {rehydrate}。",
        c.dropped
    ))
}

/// 收据类型（面板与 note 都按它找）。
pub const KIND: &str = "transcript_compaction";

/// 最近一次转录压实的**一句话**（给注入块用）。没有压实过就是 `None` —— 不许无中生有。
pub fn compaction_note(mem: &Memory, scope: &str) -> Option<String> {
    let rs = retrieve::receipts(mem, scope, 20).ok()?;
    let r = rs.iter().find(|r| r.kind == KIND)?;
    Some(format!(
        "[转录压实] 最近一次压掉了 {} 条（{}）：{}",
        r.dropped.len(),
        r.covered.first().cloned().unwrap_or_default(),
        r.rehydrate
    ))
}
