//! 语法高亮 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第五刀）。
//!
//! 一条链三层，出错的位置决定症状，所以三层各有各的判据：
//!
//! 1. **取 span**（[`highlight_spans`] / [`overridden_highlighter`]）：tree-sitter 出字节区间；
//!    插件的 `.scm` 覆盖查询在这一层编译（编译不过要报错，不能静默退回内置）。
//! 2. **摊到行 + 换单位**（[`unit_table`] / [`build_line_highlights`]）：tree-sitter 报字节，
//!    前端 `String.prototype.slice()` 的单位是 **UTF-16 码元** —— 行里只要出现过中文/emoji，
//!    从那里往后整体错位（症状："中文注释的着色起点跑了"）。转换必须在后端做完。
//! 3. **载荷**（[`HighlightPayload`]）：**不回传正文**（前端手里就有），tag 走"名表 + 下标"
//!    （整份响应里名表只出现一次）。行由前端自己切 —— 后端 `code.lines()` 会吃掉末尾空行，
//!    用它的行去拼 textarea 会让"以换行结尾的文件"少一个 `\n`。
//!
//! **性能约束（硬要求）**：只允许对源码做一次线性扫描；`tests.rs` 有一条用例专门用
//! "线性版 vs 参照实现"的耗时比值（>3×）钉住它别退化回逐行重扫。
//!
//! 配色**不在编译期**：`ui/styles.css` / `index.html` 里没有 `.tok-*` 规则，全部来自插件
//! （`plugins/highlight/<id>/theme.css`）；[`builtin_token_names`] / [`is_builtin_token`] 是
//! 内置名字的唯一来源（`plugin.rs` 校验插件 `token_map` 时用它）。

use std::sync::Mutex;

/// 语法高亮的 tag（前端 CSS 类是 `tok-<name>`）。
///
/// 取值集合是**闭的**：`arborium_theme::tag_to_name` 的 match 只有 27 个出口，
/// 而它上游的 `.and_then()` 已经把不认识的捕获名滤掉了 —— 也就是说**今天**这条链路
/// 最多也只能产出这 27 个名字。既然集合是闭的，就没有理由让每个 span 各自扛一个
/// `String`：枚举化之后打错一个名字是编译错误，而不是"这个 span 悄悄没颜色"。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tok {
    Keyword,
    Function,
    String,
    Comment,
    Type,
    Variable,
    Constant,
    Number,
    Operator,
    Punctuation,
    Property,
    Attribute,
    Tag,
    Macro,
    Label,
    Namespace,
    Constructor,
    Title,
    Strong,
    Emphasis,
    Link,
    Literal,
    Strikethrough,
    DiffAdd,
    DiffDelete,
    Embedded,
    Error,
}

/// 内置 token 名（`TOK_TABLE` 的名字那一列）。插件通过 `token_map` 引入的新名不在这里。
/// 单独给一个函数是为了**别处不用复制这张表**（插件加载器要拿它判断"这是个新名"）。
pub fn builtin_token_names() -> Vec<&'static str> {
    TOK_TABLE.iter().map(|(n, _)| *n).collect()
}

/// 这个名字是不是内置的（内置 = 主题里一定有它的配色，插件不必自带 CSS）。
pub fn is_builtin_token(name: &str) -> bool {
    TOK_TABLE.iter().any(|(n, _)| *n == name)
}

/// 名字 ↔ 语义的**唯一来源**（`TOK_TABLE`）。
///
/// 为什么表里还带一个枚举值：它让这张表有归属、不是"一串没有类型的字符串"；
/// 而**解析**走 [`resolve_token_name`] —— 它还要认插件通过 `token_map` 引入的新名字
/// （枚举是个闭集，装不下插件的新名），所以载荷里传的是**名字**而不是枚举。
pub(crate) const TOK_TABLE: &[(&str, Tok)] = &[
    ("keyword", Tok::Keyword),
    ("function", Tok::Function),
    ("string", Tok::String),
    ("comment", Tok::Comment),
    ("type", Tok::Type),
    ("variable", Tok::Variable),
    ("constant", Tok::Constant),
    ("number", Tok::Number),
    ("operator", Tok::Operator),
    ("punctuation", Tok::Punctuation),
    ("property", Tok::Property),
    ("attribute", Tok::Attribute),
    ("tag", Tok::Tag),
    ("macro", Tok::Macro),
    ("label", Tok::Label),
    ("namespace", Tok::Namespace),
    ("constructor", Tok::Constructor),
    ("title", Tok::Title),
    ("strong", Tok::Strong),
    ("emphasis", Tok::Emphasis),
    ("link", Tok::Link),
    ("literal", Tok::Literal),
    ("strikethrough", Tok::Strikethrough),
    ("diff-add", Tok::DiffAdd),
    ("diff-delete", Tok::DiffDelete),
    ("embedded", Tok::Embedded),
    ("error", Tok::Error),
];

/// `highlight_code` 的返回载荷。两处是刻意的：
///
/// 1. **不回传正文**。前端手里就有（`tab.content`），逐行回传一遍等于把文件复制一份
///    再走一遍 JSON。而且后端用 `code.lines()` 收行（**吃掉末尾空行**），前端
///    `split("\n")` 不吃 —— 拿后端的行去拼 textarea 的值，就会让"以换行结尾的文件"
///    少一个末尾 `\n`，用户一按键 `tab.content = textarea.value` 把差值固化，写回时
///    末尾换行就真没了。所以**行由前端自己切**，后端只回答"第 i 行的片段在哪"。
/// 2. **tag 走名表 + 下标**。整份响应里名表只出现一次（≤27 项），span 里只放一个下标。
///    比"每个 span 带一个 tag 字符串"省掉几乎全部字节；也不会因为两边枚举顺序不一致
///    而整体错色 —— 顺序漂移在这个设计下最多是"查到另一个名字"，而裸数字编码会是全篇错色。
#[derive(serde::Serialize, Clone)]
pub struct HighlightPayload {
    /// tag 名表：按首次出现顺序去重，span 里的第三个数是它的下标
    pub tags: Vec<&'static str>,
    /// 每行一串扁平三元组 `[start, end, tag_idx, start, end, tag_idx, ...]`；
    /// 下标就是行号（0 基），空行是 `[]`。行数与 `code.lines().count()` 一致，
    /// 可能比前端的 `split("\n")` **少一行**（末行换行）—— 前端按不足处理即可。
    /// `start`/`end` 是**行内 UTF-16 码元**偏移（不是字节），前端直接 `text.slice()` 即可。
    pub lines: Vec<Vec<u32>>,
}
#[tauri::command]
pub async fn highlight_code(
    language: String,
    path: Option<String>,
    code: String,
    plugins: tauri::State<'_, Mutex<crate::plugin::Registry>>,
) -> Result<HighlightPayload, String> {
    // 注册表是**启动时（或切项目时）**载入的：语言解析与"这门语言有没有被插件动过"都在这里问。
    // 取完立刻放锁 —— 高亮是 CPU 密集活儿，别攥着锁干。
    let (language, scm, tmap, extra) = {
        let reg = plugins.lock().map_err(|e| e.to_string())?;
        let language = resolve_language(&reg, &language, path.as_deref());
        let scm = language
            .as_deref()
            .and_then(|l| reg.highlights_override(l))
            .map(|(_, t)| t.to_string());
        let tmap = language.as_deref().and_then(|l| reg.token_map_for(l));
        (
            language,
            scm,
            tmap,
            reg.extra_token_names(&builtin_token_names()),
        )
    };
    // **没有语言 = 纯文本**（不是错误）：插件没认领这个扩展名、内置探测也不认识它 ——
    // 前端拿到的是一份空片段载荷，照常渲染（单色），这和"纯净模式"是同一套降级路径。
    let Some(language) = language else {
        return Ok(HighlightPayload {
            tags: Vec::new(),
            lines: Vec::new(),
        });
    };

    // 在后台线程中执行 CPU 密集的语法高亮，避免阻塞异步运行时
    tauri::async_runtime::spawn_blocking(move || {
        let themed = highlight_spans(&language, &code, scm.as_deref(), tmap.as_ref(), &extra)?;
        build_line_highlights(&code, &themed, &extra)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 这门文件用什么语言高亮：**插件的扩展名表优先**，其次内置探测（`arborium::detect_language`，
/// 112 门语言的表），都没有就是 `None`（⇒ 纯文本）。
///
/// 为什么要收在 Rust：以前这份 ext→语言 的 map 写死在 `ui/main.js` 里 —— 那是**插件化要消灭的
/// 三处硬编码之一**（插件声明了 `ext = ["tsx"]`，前端却还不认识它）。收进来之后，
/// "哪个扩展名归哪门语言"只有一处（插件的清单），前端不再需要那张表。
///
/// `language` 参数非空时优先（调用方明确指定，例如会话里贴一段代码）。
fn resolve_language(
    reg: &crate::plugin::Registry,
    language: &str,
    path: Option<&str>,
) -> Option<String> {
    let asked = language.trim();
    if !asked.is_empty() {
        return Some(asked.to_string());
    }
    let path = path?;
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if let Some(l) = reg.lang_for_ext(ext) {
        return Some(l.id.clone());
    }
    detect_language(path).map(|s| s.to_string())
}

/// 内置探测（预装模式才有 arborium 的那张 112 门语言的表）。
#[cfg(feature = "preinstalled")]
pub(crate) fn detect_language(path: &str) -> Option<&'static str> {
    arborium::detect_language(path)
}

/// 纯净模式没有探测表。**这不是"忘了实现"**：纯净模式的定义就是"一个解析器都不编进来"，
/// 想高亮就靠插件（插件的 ext 表照样生效 —— 见 [`resolve_language`] 的第一段）。
#[cfg(not(feature = "preinstalled"))]
pub(crate) fn detect_language(_path: &str) -> Option<&'static str> {
    None
}

/// 源码 → 片段（`(起始字节, 结束字节, token 名)`）。**高亮的唯一缝**。
///
/// 分成三种来源（见 `doc/v0.x/需求-高亮插件化-v0.14.md` §四）：
/// - 预装模式：内置 arborium 解析器；若插件给了 `highlights.scm` 覆盖，就用它换掉编译进来的那份；
/// - 插件 `token_map`：按 **capture 名**优先命中（插件的意图比我们的名表更权威）；
/// - 纯净模式：**没有解析器** ⇒ 返回空片段 ⇒ 前端按纯文本渲染（这是编译模式差异，不是故障）。
#[cfg(feature = "preinstalled")]
pub(crate) fn highlight_spans(
    language: &str,
    code: &str,
    scm_override: Option<&str>,
    token_map: Option<&std::collections::BTreeMap<String, String>>,
    extra: &[&'static str],
) -> Result<Vec<(u32, u32, &'static str)>, String> {
    use arborium::Highlighter;

    let mut highlighter = match scm_override {
        None => Highlighter::new(),
        Some(scm) => overridden_highlighter(language, scm)?,
    };
    let spans = highlighter
        .highlight_spans(language, code)
        .map_err(|e| e.to_string())?;

    Ok(spans
        .iter()
        .filter_map(|s| {
            // ① 插件映射优先（按 capture 名，例：`function.call` → `function`）
            let mapped: Option<&'static str> = token_map
                .and_then(|m| m.get(&s.capture))
                .and_then(|name| extra.iter().find(|n| **n == name.as_str()).copied());
            // ② 否则走内置名表（arborium-theme 把 capture 归一到我们的 27 个名字）
            let name = mapped.or_else(|| {
                arborium_theme::tag_for_capture(&s.capture).and_then(arborium_theme::tag_to_name)
            });
            name.map(|n| (s.start, s.end, n))
        })
        .collect())
}

/// 纯净模式：没有内置解析器。**返回空片段**（= 前端纯文本渲染），不是报错 ——
/// 用户装了插件（未来 dll/service 两条路）才有颜色，这是纯净模式的定义。
#[cfg(not(feature = "preinstalled"))]
pub(crate) fn highlight_spans(
    _language: &str,
    _code: &str,
    _scm_override: Option<&str>,
    _token_map: Option<&std::collections::BTreeMap<String, String>>,
    _extra: &[&'static str],
) -> Result<Vec<(u32, u32, &'static str)>, String> {
    Ok(Vec::new())
}

/// query 覆盖（法子 2）：拿插件那份 `highlights.scm` 重建这门语言的 grammar。
///
/// 为什么值得开这个口子：**"什么算关键字"就是一份文本** —— 换配色语义、修某门语言的高亮，
/// 甚至给内嵌语言（HTML 里的 JS/CSS）加规则，都不用重编译 ruyix。
#[cfg(feature = "preinstalled")]
pub(crate) fn overridden_highlighter(
    language: &str,
    scm: &str,
) -> Result<arborium::Highlighter, String> {
    use arborium::Highlighter;
    use arborium_highlight::{CompiledGrammar, GrammarConfig};
    use std::sync::Arc;

    // 这门语言要能**构造**出来才谈得上覆盖：只有编译进来的那 8 门有解析器。
    let lang = match language {
        "python" => arborium::lang_python::language().into(),
        "rust" => arborium::lang_rust::language().into(),
        "html" => arborium::lang_html::language().into(),
        "css" => arborium::lang_css::language().into(),
        "javascript" => arborium::lang_javascript::language().into(),
        "markdown" => arborium::lang_markdown::language().into(),
        "sql" => arborium::lang_sql::language().into(),
        "java" => arborium::lang_java::language().into(),
        other => return Err(format!("插件给了 query 覆盖，但 `{other}` 没有内置解析器")),
    };
    let compiled = CompiledGrammar::new(GrammarConfig {
        language: lang,
        highlights_query: scm,
        injections_query: "",
        locals_query: "",
    })
    .map_err(|e| format!("插件的 highlights.scm 编译失败：{e}"))?;

    // 用一份**带覆盖的** store 建 highlighter：内置那 8 门仍在 store 里，只换掉这一门。
    let store = Arc::new(arborium::GrammarStore::new());
    store.insert(language, Arc::new(compiled));
    Ok(Highlighter::with_store(store))
}

/// 插件里有哪些语言 / 什么颜色 / 加载时被拒绝了什么（前端的扩展名与图标表、主题 CSS 都从这来）。
#[tauri::command]
pub fn highlight_plugins(
    plugins: tauri::State<'_, Mutex<crate::plugin::Registry>>,
) -> Result<serde_json::Value, String> {
    let reg = plugins.lock().map_err(|e| e.to_string())?;
    Ok(reg.to_json(crate::preinstalled::mode()))
}
/// 行内「字节偏移 → UTF-16 码元偏移」查表，长度 `line.len() + 1`。
///
/// **为什么需要这一步**：tree-sitter 报的是字节区间，而前端用 `String.prototype.slice()`
/// 切片段 —— JS 的字符串下标是 UTF-16 码元。纯 ASCII 行里两者相等，但中文一个字 3 字节 /
/// 1 码元、emoji 4 字节 / 2 码元，**只要行里出现过非 ASCII，从那里往后就整体错位**，
/// 表现为"中文注释的着色起点跑了、顺带把后面的标点也染上"。单位必须在后端统一成
/// 前端真正在用的那个，别让前端去猜。
///
/// 表按行建一次、之后 O(1) 查；多字节字符内部的字节位置向前取整到该字符之前。
/// 调用方对纯 ASCII 行直接恒等映射，连表都不建。
fn unit_table(line: &str) -> Vec<u32> {
    let mut table = vec![0u32; line.len() + 1];
    let mut units = 0u32;
    for (byte_idx, ch) in line.char_indices() {
        for slot in &mut table[byte_idx..byte_idx + ch.len_utf8()] {
            *slot = units;
        }
        units += ch.len_utf16() as u32;
    }
    table[line.len()] = units;
    table
}

/// 把 tree-sitter 报的字节区间摊到每一行上。
///
/// **性能约束（硬要求，不要再退化）**：只允许对源码做一次线性扫描。
///
/// 旧版每行都调用一次 `line_byte_offset(code, n)`，而那个函数每次都从文件头重新数
/// `\n` —— 整体 O(行数 × 文件字节数)；内层又每行遍历全部 span —— 再加一层
/// O(行数 × span 数)。release 实测：5 千行 172ms、1 万行 552ms、2.5 万行 4.0s、
/// 5 万行 16.4s（tree-sitter 自己是线性的）。而高亮挂在自动保存上 —— 打字每停顿
/// 一秒就重跑一次，于是文件一大就整个编辑器无响应。
///
/// 现在：一次扫出每行起始偏移 → 每个 span 二分定位所在行 → 跨行 span 按行切分。
/// 片段语义（切点、排序、去重）与旧实现相同（`tests.rs` 里的 `slow_reference` 钉住），
/// 但有两处**刻意不同**：输出换成紧凑载荷（见 `HighlightPayload`），
/// 偏移单位从**字节**换成 **UTF-16 码元**（见 `unit_table`，前端 `slice()` 的单位）。
/// 认一个 token 名：内置名（TOK_TABLE）或插件引入的新名（extra，加载时已校验自带 CSS）。
pub(crate) fn resolve_token_name(name: &str, extra: &[&'static str]) -> Option<&'static str> {
    if let Some((n, _)) = TOK_TABLE.iter().find(|(n, _)| *n == name) {
        return Some(n);
    }
    extra.iter().find(|n| **n == name).copied()
}

pub(crate) fn build_line_highlights(
    code: &str,
    spans: &[(u32, u32, &'static str)],
    extra: &[&'static str],
) -> Result<HighlightPayload, String> {
    let lines: Vec<&str> = code.lines().collect();

    // 第 k 行的起始字节偏移 = 第 k 个 '\n' 之后。一次扫完，
    // 与旧版反复调用 line_byte_offset(code, k + 1) 的结果逐一相同。
    let mut starts: Vec<usize> = Vec::with_capacity(lines.len() + 1);
    starts.push(0);
    for (i, b) in code.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }

    // 二分定位每个 span 覆盖的行区间，再按行切分（跨行的注释/字符串会横跨多行）
    // 片段里存的**是 token 名而不是枚举**：插件可以通过 `token_map` 引入新名字，
    // 而枚举是个闭集（27 个变体），装不下插件的新名。名字是 `&'static str`（插件名在加载时
    // leak 成 'static，见 `plugin::Registry::extra_token_names`）。
    let mut per_line: Vec<Vec<(usize, usize, &'static str)>> = vec![Vec::new(); lines.len()];
    for &(start, end, tag) in spans {
        // 名字校验在这里做掉：内置名走 TOK_TABLE；插件引入的新名在 extra 里
        //（加载时已校验过"自带 CSS"）。认不出的名字直接丢 —— 它渲染出来会是没有配色的 span。
        let Some(tok) = resolve_token_name(tag, extra) else {
            continue;
        };
        let (s, e) = (start as usize, end as usize);
        if e <= s {
            continue;
        }
        let mut li = starts.partition_point(|&x| x <= s).saturating_sub(1);
        while li < lines.len() {
            let line_start = starts[li];
            let line_end = line_start + lines[li].len();
            if line_start >= e {
                break;
            }
            let rel_start = s.max(line_start) - line_start;
            let rel_end = e.min(line_end) - line_start;
            if rel_start < rel_end {
                per_line[li].push((rel_start, rel_end, tok));
            }
            li += 1;
        }
    }

    let mut tags: Vec<&'static str> = Vec::new();
    let mut out_lines: Vec<Vec<u32>> = Vec::with_capacity(per_line.len());
    // 直接消费 per_line：每行取走自己的片段，免得再用下标索引一遍。
    // 单位换算放在这里而不是定位那一步：**每行只出现一次**，且能按行建一次表 ——
    // 若在片段循环里逐片段换算，单行超长的压缩文件会退化成 O(片段数 × 行长)。
    for (line_idx, mut line_spans) in per_line.into_iter().enumerate() {
        // 排序并去重：tree-sitter 会对同一段文本产生多个重叠 capture
        line_spans.sort_by_key(|a| a.0);
        let mut flat: Vec<u32> = Vec::new();
        if line_spans.is_empty() {
            out_lines.push(flat); // 空行也要占一行（行号 = 下标）
            continue;
        }
        // 字节 → UTF-16 码元（前端 `slice()` 的单位）。纯 ASCII 行恒等，不建表。
        let line_text = lines[line_idx];
        let table = if line_text.is_ascii() {
            None
        } else {
            Some(unit_table(line_text))
        };
        let to_units = |byte_off: usize| -> usize {
            table.as_ref().map_or(byte_off, |t| t[byte_off] as usize)
        };
        // 去重仍在**字节**上做（片段边界是字节给的，比较自然在同一单位里），
        // 换算只作用于最终留下的那对切点：换算在字符边界上是单调且单射的，结果一致。
        let mut covered = 0usize;
        for (start_col, end_col, tok) in line_spans {
            if end_col <= covered {
                continue;
            }
            let start_col = start_col.max(covered);
            covered = end_col;
            let (unit_start, unit_end) = (to_units(start_col), to_units(end_col));
            // 换算只可能变小（多字节字符吃掉字节），所以仍落在 u32 里
            if unit_start >= unit_end {
                continue; // 防御：切点落在同一个字符内部，退化成空片段
            }
            // 名表按首次出现顺序收集；重复出现的只留下来一次
            // 名表按首次出现顺序收集；重复出现的只留下来一次
            let idx = match tags.iter().position(|t| *t == tok) {
                Some(i) => i,
                None => {
                    tags.push(tok);
                    tags.len() - 1
                }
            };
            flat.push(unit_start as u32);
            flat.push(unit_end as u32);
            flat.push(idx as u32);
        }
        out_lines.push(flat);
    }

    Ok(HighlightPayload {
        tags,
        lines: out_lines,
    })
}
