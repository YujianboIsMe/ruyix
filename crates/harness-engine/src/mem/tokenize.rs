//! 记忆检索用的分词（**零依赖**）。
//!
//! 为什么不用 jieba：这里要索引的是**短句事实**（"构建命令 = cargo tauri build"），
//! 词法腿的职责只是"把明显命中捞回来给语义腿排序"，而中文 bigram 在这个量级上足够
//! （见技能 `code-search-retrieval-tuning` 的 bigram/分词权衡）。零依赖也意味着
//! **不改坏便携形态**（不需要为了一门分词引入 C 依赖或几 MB 词典）。
//!
//! 两块必须一起做（否则代码符号搜不到 —— 技能里实测过的坑）：
//!
//! 1. **中文走 bigram**：`构建命令` → `构建 建命 命令`（长度 1 的串保留单字）；
//! 2. **标识符要切开**：`handleCommand` → `handlecommand handle command`，
//!    `term_target_v2` → `term target v2` —— 查询侧与索引侧**共用同一个函数**。
//!
//! 第三块是查询侧才做的：**问句助词过滤**（的/了/在哪/怎么…），否则长中文问句在 AND 语义下
//! 必然 0 命中（技能里记录的第一个失败模式）。

/// 中文问句助词 + 常见无信息词。**只在查询侧过滤**（索引侧保留原文）。
const STOPWORDS: &[&str] = &[
    "的",
    "了",
    "在",
    "是",
    "有",
    "和",
    "与",
    "或",
    "这",
    "那",
    "个",
    "些",
    "我",
    "你",
    "他",
    "我们",
    "你们",
    "他们",
    "它",
    "什么",
    "怎么",
    "如何",
    "为什么",
    "哪",
    "哪个",
    "哪里",
    "吗",
    "呢",
    "吧",
    "啊",
    "请",
    "帮",
    "一下",
    "告诉",
    "告诉一下",
    "现在",
    "目前",
    "当时",
    "以及",
    "the",
    "a",
    "an",
    "of",
    "is",
    "are",
    "to",
    "in",
    "on",
    "for",
    "and",
    "or",
    "what",
    "how",
    "why",
    "where",
    "please",
    "me",
    "tell",
];

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/' || c == ':'
}

/// 标识符切分：`handleCommand` → [handleCommand, handle, command]，`a2b_c` → [a2b_c, a2b, c]。
fn split_identifier(tok: &str) -> Vec<String> {
    let mut out = vec![tok.to_lowercase()];
    // snake / kebab / dot / slash 先拆
    let mut parts: Vec<String> = Vec::new();
    for seg in tok.split(['_', '-', '.', '/']) {
        if seg.is_empty() {
            continue;
        }
        // camelCase / PascalCase / ACRONYMWord
        let chars: Vec<char> = seg.chars().collect();
        let mut cur = String::new();
        for (i, &ch) in chars.iter().enumerate() {
            // 注意：i=0 时**不许**碰 chars[i-1]（`||` 的右半边会真的求值 → 下标下溢 panic，
            // 这个坑是 mem 的测试当场抓出来的）
            let prev_lower = i > 0 && chars[i - 1].is_lowercase();
            let prev_upper = i > 0 && chars[i - 1].is_uppercase();
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            if ch.is_uppercase() && (prev_lower || (prev_upper && next_lower)) && !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
        }
        if !cur.is_empty() {
            parts.push(cur);
        }
    }
    for p in parts {
        let low = p.to_lowercase();
        if low != out[0] && !out.contains(&low) {
            out.push(low);
        }
    }
    out
}

/// 索引侧：把一段文本变成 FTS5 吃的 token 串（中文 bigram + 标识符切分 + 小写）。
pub fn index_text(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut cjk_run: Vec<char> = Vec::new();
    let mut word = String::new();

    let flush_cjk = |run: &mut Vec<char>, out: &mut Vec<String>| {
        if run.is_empty() {
            return;
        }
        if run.len() == 1 {
            out.push(run[0].to_string());
        } else {
            for w in run.windows(2) {
                out.push(w.iter().collect::<String>());
            }
            // 单字也留一份：查"库"这种单字问句能命中"记忆库"
            for c in run.iter() {
                let s = c.to_string();
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        run.clear();
    };

    for ch in s.chars() {
        if is_cjk(ch) {
            if !word.is_empty() {
                for t in split_identifier(&word) {
                    out.push(t);
                }
                word.clear();
            }
            cjk_run.push(ch);
        } else if is_word(ch) {
            flush_cjk(&mut cjk_run, &mut out);
            word.push(ch);
        } else {
            flush_cjk(&mut cjk_run, &mut out);
            if !word.is_empty() {
                for t in split_identifier(&word) {
                    out.push(t);
                }
                word.clear();
            }
        }
    }
    flush_cjk(&mut cjk_run, &mut out);
    if !word.is_empty() {
        for t in split_identifier(&word) {
            out.push(t);
        }
    }
    out.join(" ")
}

/// 查询里有没有中文。**符号/路径类查询（全 ASCII）不该走语义腿**：
/// 实测（见 `mem::tests::向量腿_阈值标定`）bge-small-zh 对"file.cargo_toml.dep × person.7.city 巴黎"
/// 这种跨话题配对标出的余弦高达 0.41，而绝对阈值在 0.59/0.63 之间根本切不开 —— 与其拍一个
/// 站不住的阈值，不如用一条**结构性**规则：查符号交给词法腿（精确），查自然语言才请语义腿。
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk)
}

/// 查询侧：index_text 之后再过一遍助词过滤 + 去重。
pub fn query_tokens(q: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in index_text(q).split_whitespace() {
        if STOPWORDS.contains(&t) {
            continue;
        }
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// 组装 FTS5 `MATCH` 表达式：**≤2 个核心 token 走 AND（精确），>2 走 OR（提召回）**。
///
/// 长中文问句在 AND 语义下几乎必然 0 命中（技能里记录的第一个失败模式），
/// 而短问句走 OR 又会把噪声捞进来 —— 所以要按核心 token 数动态选。
pub fn match_expr(tokens: &[String]) -> Option<String> {
    let core: Vec<&String> = tokens
        .iter()
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect();
    if core.is_empty() {
        return None;
    }
    let quoted: Vec<String> = core
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect();
    if quoted.len() <= 2 {
        Some(quoted.join(" AND "))
    } else {
        Some(quoted.join(" OR "))
    }
}
