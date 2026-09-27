//! ruyix 宿主（bin crate）的单元测试 —— 从 `main.rs` 拆出来的（3665 行 → 2693 + 972）。
//!
//! **为什么单开一个文件**：生产代码里夹着近千行测试，读代码时一直在中间穿行；
//! 拆开之后 `main.rs` 只剩"能跑起来的那部分"。接线只有一处 —— `main.rs` 末尾的
//! `#[cfg(test)] mod tests;`。
//!
//! **可见性没变**：`use super::*;` 现在指 crate 根（本文件就是 crate 根下的 `tests` 模块），
//! 所以测试照样能摸到 `main.rs` 里的私有项 —— 这也是当初没选 `tests/` 集成测试目录的原因
//! （那一层看不到私有项，得先把接口开出来）。
//!
//! **`include_str!` 的相对基准没变**：它相对**本文件所在目录**（`src/`）解析，与 `main.rs`
//! 同目录 ⇒ 下面那些 `"../../plugins/highlight/…"` 指向的还是仓库根的 `plugins/`。
//!
//! 只搬了一个"生产段里的测试代码"：`utf16_offset()`（逐字符数一遍的朴素换算）本来就是
//! `#[cfg(test)]` 门禁的测试专供参照，顺带去掉它那行属性 —— 整个文件都只在测试构建里编译。

use super::*;
#[cfg(feature = "preinstalled")]
use arborium::Highlighter;

/// 不经查表、逐字符数一遍的等价实现（`slow_reference` 的偏移换算用它）。
///
/// 生产路径**不能**用它：它在每个片段上重走一遍整行，单行超长的压缩文件会退化成
/// O(片段数 × 行长)。两份实现互相独立，单位一旦搞错，等价性测试就会红。
///
/// 它原先住在 `main.rs` 的生产段、靠 `#[cfg(test)]` 门禁；测试搬进本文件后，门禁由
/// **整个文件**承担，那行属性就去掉了。`#[cfg(feature = "preinstalled")]` 留着 ——
/// 纯净模式（`--no-default-features`）里 arborium 根本没编进来，而用它的 `slow_reference`
/// 被同一条件门禁着（没有这行就是 unresolved import / 死代码）。
#[cfg(feature = "preinstalled")]
fn utf16_offset(line: &str, byte_off: usize) -> usize {
    if line.is_ascii() {
        return byte_off;
    }
    let mut units = 0usize;
    for (byte_idx, ch) in line.char_indices() {
        if byte_idx >= byte_off {
            break;
        }
        units += ch.len_utf16();
    }
    units
}

/// `--debug` 的识别：**整词相等**、未知参数忽略。
///
/// 反向验证过：把实现换成宽松匹配（`a.contains("debug")`），`--debugx` 与
/// "路径里含 debug" 这两条立刻变红 —— 这条测试是能红的，不是摆设。
#[test]
// 依赖 arborium / 那两个 helper：纯净模式里它们不存在（编进去会 unresolved import）
#[cfg(feature = "preinstalled")]
fn debug_flag_matches_whole_words_only() {
    let f = |v: &[&str]| parse_debug_flag(v.iter().copied());
    assert!(f(&["ruyix.exe", "--debug"]));
    assert!(f(&["ruyix.exe", "-d"]));
    assert!(f(&["ruyix.exe", "-v"]));
    assert!(f(&["ruyix.exe", "--verbose"]));
    assert!(!f(&["ruyix.exe"]), "不带参数时不许开");
    // 整词边界：这些都不许命中
    assert!(!f(&["ruyix.exe", "--debugx"]));
    assert!(!f(&["ruyix.exe", "--no-debug"]));
    assert!(!f(&[r"C:\Users\me\debug\ruyix.exe"]));
    // 未知参数忽略，但不影响同一批里其它参数的识别
    assert!(f(&["ruyix.exe", "--unknown-flag", "--debug"]));
    assert!(!f(&["ruyix.exe", "--unknown-flag"]));
}

/// tree-sitter 跑一遍，转成 build_line_highlights 吃的 (start, end, tag)
// 只有预装模式才编进了 arborium：纯净模式下这两个 helper 没有调用者
// （用它们的用例也被同一条件门禁，见文件末的测试模块）
#[cfg(feature = "preinstalled")]
fn themed_spans(lang: &str, src: &str) -> Vec<(u32, u32, &'static str)> {
    let mut highlighter = Highlighter::new();
    let spans = highlighter.highlight_spans(lang, src).expect("高亮失败");
    spans
        .iter()
        .filter_map(|s| {
            arborium_theme::tag_for_capture(&s.capture)
                .and_then(arborium_theme::tag_to_name)
                .map(|name| (s.start, s.end, name))
        })
        .collect()
}

/// 旧实现的等价物，**只作测试参照**：每行重扫全文件找行首 + 每行遍历全部 span。
/// 刻意保留它的 O(n²) —— 用来钉住线性重写的输出，并给出复杂度比值。
/// 片段语义（切点 / 排序 / 去重）与生产实现各自独立写一遍，两边都错成同一个样子
/// 才会通过，所以它同时也是"没把语义顺手改歪"的参照。偏移换算同理：这里用逐字符
/// 数一遍的朴素写法，生产路径用按行建一次的查表版。
// 只有预装模式才编进了 arborium：纯净模式下这两个 helper 没有调用者
// （用它们的用例也被同一条件门禁，见文件末的测试模块）
#[cfg(feature = "preinstalled")]
fn slow_reference(code: &str, spans: &[(u32, u32, &'static str)]) -> HighlightPayload {
    fn line_byte_offset(source: &str, line_number: usize) -> usize {
        if line_number <= 1 {
            return 0;
        }
        source
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .nth(line_number - 2)
            .map(|(i, _)| i + 1)
            .unwrap_or(source.len())
    }

    let lines: Vec<&str> = code.lines().collect();
    let mut tags: Vec<&'static str> = Vec::new();
    let mut out_lines: Vec<Vec<u32>> = Vec::new();
    for (line_idx, line_text) in lines.iter().enumerate() {
        let line_start = line_byte_offset(code, line_idx + 1);
        let line_end = line_start + line_text.len();

        let mut line_spans: Vec<(usize, usize, &'static str)> = Vec::new();
        for &(start, end, tag) in spans {
            let Some(tok) = resolve_token_name(tag, &[]) else {
                continue;
            };
            let s = start as usize;
            let e = end as usize;
            if e > line_start && s < line_end {
                let rel_start = s.saturating_sub(line_start);
                let rel_end = if e < line_end {
                    e - line_start
                } else {
                    line_text.len()
                };
                if rel_start < rel_end {
                    line_spans.push((rel_start, rel_end, tok));
                }
            }
        }

        line_spans.sort_by_key(|a| a.0);
        let mut flat: Vec<u32> = Vec::new();
        let mut covered = 0usize;
        for (start_col, end_col, tok) in line_spans {
            if end_col <= covered {
                continue;
            }
            let start_col = start_col.max(covered);
            covered = end_col;
            // 字节 → UTF-16 码元。这里用"逐字符数一遍"的朴素写法（与生产路径的查表
            // 实现相互独立）—— 两者不一致就是等价性测试该抓的东西。
            let unit_start = utf16_offset(line_text, start_col);
            let unit_end = utf16_offset(line_text, end_col);
            if unit_start >= unit_end {
                continue;
            }
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
    HighlightPayload {
        tags,
        lines: out_lines,
    }
}

/// 线性重写必须与旧实现**逐字节相同**。
/// 覆盖的形态：空文件 / 只有换行 / 无尾换行 / CRLF / 跨行块注释 / 跨行字符串 /
// 依赖 arborium/arborium_theme 的用例：纯净模式没有它们
#[cfg(feature = "preinstalled")]
/// 以及一份有规模的源码（让 span 数量和跨行覆盖接近真实文件）。
#[test]
fn highlight_output_is_unchanged_by_the_linear_rewrite() {
    let mut sources = vec![
        String::new(),
        "\n".into(),
        "\n\n\n".into(),
        "no trailing newline".into(),
        "a\nb\nc".into(),
        "// 注释\nfn main() {\n    let s = \"a\\nb\";\n}\n".into(),
        "/* 跨行\n   注释 */\nfn f() {}\n".into(),
        "fn a() {}\r\nfn b() {}\r\n".into(),
        "let x = 1;".into(),
    ];
    let mut big = String::new();
    for i in 0..400 {
        big.push_str(&format!(
            "fn f{i}(x: i32) -> i32 {{\n    // note {i}\n    x + {i}\n}}\n"
        ));
    }
    sources.push(big);

    for src in &sources {
        for lang in ["rust", "javascript", "python", "markdown"] {
            let spans = themed_spans(lang, src);

            let linear = build_line_highlights(src, &spans, &[]).unwrap();
            let reference = slow_reference(src, &spans);
            assert_eq!(
                serde_json::to_string(&linear).unwrap(),
                serde_json::to_string(&reference).unwrap(),
                "{lang} 上线性重写与旧实现输出不一致（源码 {} 字节）",
                src.len()
            );
        }
    }
}

/// **单位门禁**：片段偏移必须是 UTF-16 码元，而不是字节。
///
/// 前端拿它直接 `String.prototype.slice()` —— JS 的下标是 UTF-16 码元；而 tree-sitter
/// 报的是字节。中文一个字 3 字节 / 1 码元，**含非 ASCII 的行从那里往后就整体错位**，
/// 表现为"中文注释的着色起点跑了、顺带把后面的标点也染上"。
///
/// 判据用**包含性**而不是逐字相等：去重会按 `covered` 裁剪片段，而裁出来的片段一定是
/// 原捕获的子集（`max(start, covered) ≥ start` 且 `end` 不变），所以"落在同 tag 的原始
/// 捕获内"对正确输出恒成立；单位写错时切出来的字节范围会**越出**原捕获 ——
// 依赖 arborium/arborium_theme 的用例：纯净模式没有它们
#[cfg(feature = "preinstalled")]
/// 修前的 `;` 就是这样（报 13..14，按码元解释切出来是 `/`，落进旁边的捕获）。
#[test]
fn span_offsets_are_utf16_units() {
    // 中文（3 字节 / 1 码元）+ emoji（4 字节 / 2 码元、且在补充平面）：
    // "按字节当码元"与"按 char 个数当码元"两种错法各覆盖一次
    let src = "let s = \"中文\"; // 注释\nlet t = \"a\"; // 🚀 起飞\nfn f() {}\n";
    let raw = themed_spans("rust", src);
    let payload = build_line_highlights(src, &raw, &[]).unwrap();
    let lines: Vec<&str> = src.lines().collect();

    let mut line_starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }

    // 行内 UTF-16 码元偏移 → 行内字节偏移（= JS `slice()` 选中哪些字符、占哪些字节）
    fn byte_of_unit(line: &str, unit_off: usize) -> usize {
        let mut unit = 0usize;
        for (byte_idx, ch) in line.char_indices() {
            if unit == unit_off {
                return byte_idx;
            }
            unit += ch.len_utf16();
        }
        line.len()
    }

    let mut checked = 0usize;
    let mut differentiated = 0usize;
    for (li, flat) in payload.lines.iter().enumerate() {
        let line = lines[li];
        let line_start = line_starts[li];
        for t in flat.chunks(3) {
            let (us, ue) = (t[0] as usize, t[1] as usize);
            let tag = payload.tags[t[2] as usize];
            assert!(us < ue, "第 {li} 行出现空片段 {us}..{ue}");
            let (bs, be) = (byte_of_unit(line, us), byte_of_unit(line, ue));
            assert!(
                bs < be,
                "第 {li} 行片段 {us}..{ue} 的码元偏移不在字符边界上，翻不回字节区间"
            );
            assert!(
                raw.iter().any(|&(s, e, t)| t == tag
                    && (s as usize) <= line_start + bs
                    && line_start + be <= (e as usize)),
                "第 {li} 行片段 {us}..{ue}（tag={tag}）切出来是 {:?}，越出了同 tag 的原始捕获 \
                     —— 偏移单位错了（大概率报了**字节**，而前端按 **UTF-16 码元**切）",
                &line[bs..be]
            );
            // 正控：把同一对数字**当字节**解释（= 修前的行为）切出的文本不一样，
            // 说明这份语料真能分辨两种单位。一处都分不出来，这条测试就是空转。
            let as_bytes = line
                .as_bytes()
                .get(us..ue)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            if line.get(bs..be) != Some(as_bytes.as_str()) {
                differentiated += 1;
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "语料没产出任何高亮片段，测试空转");
    assert!(
        differentiated > 0,
        "语料对偏移单位不敏感（{checked} 个片段里没有一个能区分字节与码元）—— \
             换成含中文/emoji 的行，否则这条门禁抓不到「把单位改回字节」的改动"
    );

    // 定点核对：含中文 / emoji 的那两行，必须正好切出源码里的那几段字符
    let texts_on = |li: usize| -> Vec<String> {
        let line = lines[li];
        payload.lines[li]
            .chunks(3)
            .map(|t| {
                let (bs, be) = (
                    byte_of_unit(line, t[0] as usize),
                    byte_of_unit(line, t[1] as usize),
                );
                line[bs..be].to_string()
            })
            .collect()
    };
    for (li, needle) in [(0usize, "\"中文\""), (1usize, "// 🚀 起飞")] {
        let texts = texts_on(li);
        assert!(
            texts.iter().any(|t| t.contains(needle)),
            "第 {li} 行没切出 {needle:?}（实际切出 {texts:?}）—— 含非 ASCII 的行偏移错位"
        );
    }
}

/// **复杂度金丝雀**：有人再把"逐行重扫全文件"写回来，这条必须转红。
///
/// 判据用**比值**而不是绝对耗时 —— 机器快慢不影响结论。阈值放到 3 倍是刻意留的
// 依赖 arborium/arborium_theme 的用例：纯净模式没有它们
#[cfg(feature = "preinstalled")]
/// 余量（实测余量在 10 倍以上），避免慢机器 / CI 上假红。
#[test]
fn highlight_does_not_rescan_the_file_per_line() {
    let mut src = String::new();
    for i in 0..400 {
        src.push_str(&format!(
            "fn f{i}(x: i32) -> i32 {{\n    // note {i}\n    x + {i}\n}}\n"
        ));
    }
    let spans = themed_spans("rust", &src);

    let t0 = std::time::Instant::now();
    let linear = build_line_highlights(&src, &spans, &[]).unwrap();
    let d_linear = t0.elapsed().as_secs_f64();

    let t1 = std::time::Instant::now();
    let reference = slow_reference(&src, &spans);
    let d_reference = t1.elapsed().as_secs_f64();

    let span_count = |p: &HighlightPayload| p.lines.iter().map(|l| l.len() / 3).sum::<usize>();
    assert_eq!(linear.lines.len(), reference.lines.len(), "行数不一致");
    assert_eq!(
        span_count(&linear),
        span_count(&reference),
        "span 总数不一致"
    );

    assert!(
        d_linear * 3.0 < d_reference,
        "build_line_highlights 疑似又变成逐行重扫：线性版 {:.1}ms，旧算法 {:.1}ms（比值 {:.1}×，要求 >3×）",
        d_linear * 1e3,
        d_reference * 1e3,
        d_reference / d_linear.max(1e-9)
    );
}

/// SQL 语法高亮：验证 lang-sql feature 启用后 arborium 能识别 "sql" 语言
#[test]
// 依赖 arborium / 那两个 helper：纯净模式里它们不存在（编进去会 unresolved import）
#[cfg(feature = "preinstalled")]
fn sql_highlight_works() {
    let mut highlighter = Highlighter::new();
    let spans = highlighter
        .highlight_spans("sql", "SELECT id, name FROM users WHERE age > 18;")
        .expect("SQL 高亮失败");
    assert!(!spans.is_empty(), "SQL 高亮应返回 span");
}

// 依赖 arborium/arborium_theme 的用例：纯净模式没有它们
#[cfg(feature = "preinstalled")]
/// Java 语法高亮：验证 lang-java feature 启用后 arborium 能识别 "java" 语言
#[test]
fn java_highlight_works() {
    let mut highlighter = Highlighter::new();
    let src = "public class Main {\n    public static void main(String[] args) {\n        // 打印\n        System.out.println(\"hello\");\n    }\n}";
    let spans = highlighter
        .highlight_spans("java", src)
        .expect("Java 高亮失败");
    assert!(!spans.is_empty(), "Java 高亮应返回 span");

    // 关键字 public/class/static 应被识别为关键字或类型捕获
    let captures: Vec<&str> = spans.iter().map(|s| s.capture.as_str()).collect();
    assert!(
        captures.iter().any(|c| c.contains("keyword")),
        "Java 应至少产生一个 keyword 捕获，实际: {:?}",
        captures
    );

    // 注释与字符串也应被捕获
    assert!(
        captures.iter().any(|c| c.contains("comment")),
        "Java 应捕获注释，实际: {:?}",
        captures
    );
    assert!(
        captures.iter().any(|c| c.contains("string")),
        "Java 应捕获字符串，实际: {:?}",
        captures
    );

    // 走一遍 highlight_code 的映射链路：capture → theme tag → CSS 类名
    // 若此步为空，说明前端拿不到任何 span（表现为"无高亮"）
    let themed: Vec<(u32, u32, &str)> = spans
        .iter()
        .filter_map(|s| {
            arborium_theme::tag_for_capture(&s.capture)
                .and_then(arborium_theme::tag_to_name)
                .map(|name| (s.start, s.end, name))
        })
        .collect();
    assert!(!themed.is_empty(), "Java 捕获应能映射到主题 tag");
    for expected in ["keyword", "string", "comment", "type"] {
        assert!(
            themed.iter().any(|(_, _, name)| *name == expected),
            "Java 高亮应包含 {} 主题 tag，实际: {:?}",
            expected,
            themed.iter().map(|(_, _, n)| *n).collect::<Vec<_>>()
        );
    }

    // 端到端：行级高亮结果应携带 span（前端据此渲染 <span class="tok-*">）
    let payload = build_line_highlights(src, &themed, &[]).expect("构建行级高亮失败");
    assert_eq!(payload.lines.len(), 6, "6 行源码应生成 6 行高亮");
    assert!(
        payload.lines.iter().any(|l| !l.is_empty()),
        "至少一行应包含高亮 span"
    );
}

// ============================================
// 高亮载荷的形状（P3）
// ============================================

/// 一段有规模的真源码：400 行、每行都有 span、还带中文注释（顺带覆盖多字节行）。
// 只被已门禁的用例用到：纯净模式下留着就是死代码（clippy 会红）
#[cfg(feature = "preinstalled")]
fn sample_source() -> String {
    let mut src = String::new();
    for i in 0..400 {
        src.push_str(&format!(
            "fn f{i}(x: i32) -> i32 {{\n    // 注释 {i}\n    x + {i}\n}}\n"
        ));
    }
    src
}

// 依赖 arborium / 那两个 helper：纯净模式里它们不存在（编进去会 unresolved import）
#[cfg(feature = "preinstalled")]
fn payload_of(lang: &str, src: &str) -> HighlightPayload {
    let spans = themed_spans(lang, src);
    build_line_highlights(src, &spans, &[]).expect("构建载荷失败")
}

/// **载荷里不许出现文件正文。**
///
/// 后端回传逐行 `text` 是纯浪费（前端手里就有 `tab.content`），而且 `code.lines()`
/// 会吃掉末尾空行、前端 `split("\n")` 不吃 —— 拿后端的行拼 textarea 的值，就会把
/// "以换行结尾的文件"的末尾换行弄丢。有人为了"省前端一次 split"把它加回来，这条要红。
#[test]
// 依赖 arborium（或依赖了依赖它的 helper）：纯净模式里不存在
#[cfg(feature = "preinstalled")]
fn highlight_payload_does_not_echo_the_source_text() {
    let src = "fn main() {\n    let secret = \"UNIQUE_MARKER_9f3a\";\n}\n";
    let json = serde_json::to_string(&payload_of("rust", src)).unwrap();
    assert!(
        !json.contains("UNIQUE_MARKER_9f3a") && !json.contains("main"),
        "载荷里出现了源码正文（逐行回传等于把文件复制一份再走一遍 JSON）：{json}"
    );
    assert!(
        !json.contains("start_col") && !json.contains("line_number"),
        "载荷退回了逐 span 的对象编码 —— key 名重复才是载荷的大头，扁平成三元组才有意义：{json}"
    );
}

/// 载荷必须**显著小于**旧形状（逐行对象 + 逐 span 对象 + 回传正文）。
///
/// 判据用**比值**，和复杂度金丝雀同一个路子：机器、内容都不影响结论。
/// 旧形状在同一份片段上现搭出来比 —— 这样比的只是"编码"，不是"高亮质量"。
#[test]
// 依赖 arborium（或依赖了依赖它的 helper）：纯净模式里不存在
#[cfg(feature = "preinstalled")]
fn highlight_payload_is_much_smaller_than_the_legacy_shape() {
    let src = sample_source();
    let payload = payload_of("rust", &src);

    let lines: Vec<&str> = src.lines().collect();
    let legacy: Vec<serde_json::Value> = lines
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let flat = payload.lines.get(i).cloned().unwrap_or_default();
            let spans: Vec<serde_json::Value> = (0..flat.len())
                .step_by(3)
                .map(|k| {
                    serde_json::json!({
                        "start_col": flat[k],
                        "end_col": flat[k + 1],
                        "tag": payload.tags[flat[k + 2] as usize],
                    })
                })
                .collect();
            serde_json::json!({ "line_number": i + 1, "text": text, "spans": spans })
        })
        .collect();

    let new_len = serde_json::to_string(&payload).unwrap().len();
    let old_len = serde_json::to_string(&legacy).unwrap().len();
    assert!(
        new_len * 3 <= old_len,
        "载荷没瘦下来：新 {new_len} 字节 vs 旧 {old_len} 字节（要求至少 3 倍）。\
             检查是不是把 text 加回来了、或片段又变成每 span 一个对象。"
    );
}

/// 载荷形状：行号即下标、每行是 3 的倍数、下标落在名表内、片段升序且不重叠。
/// 前端**直接顺序切片**渲染（不做排序/重叠检查），所以这些前提必须由后端保证。
#[test]
// 依赖 arborium / 那两个 helper：纯净模式里它们不存在（编进去会 unresolved import）
#[cfg(feature = "preinstalled")]
fn highlight_payload_shape_is_dense_and_flat() {
    let src = sample_source();
    let payload = payload_of("rust", &src);

    assert_eq!(
        payload.lines.len(),
        src.lines().count(),
        "载荷行数必须等于 code.lines().count()（下标即行号）"
    );
    assert!(
        !payload.tags.is_empty() && payload.tags.len() <= TOK_TABLE.len(),
        "名表只该含用到的 tag 且不超过全集：{:?}",
        payload.tags
    );
    let uniq: std::collections::BTreeSet<&str> = payload.tags.iter().copied().collect();
    assert_eq!(uniq.len(), payload.tags.len(), "名表必须去重");

    for (i, flat) in payload.lines.iter().enumerate() {
        assert_eq!(flat.len() % 3, 0, "第 {i} 行不是 3 的倍数：{flat:?}");
        let mut prev_end = 0usize;
        for k in (0..flat.len()).step_by(3) {
            let (s, e, t) = (flat[k] as usize, flat[k + 1] as usize, flat[k + 2] as usize);
            assert!(t < payload.tags.len(), "第 {i} 行的 tag 下标越界：{t}");
            assert!(s < e, "第 {i} 行有空/倒置片段：{s}..{e}");
            assert!(s >= prev_end, "第 {i} 行的片段重叠或未升序：{flat:?}");
            prev_end = e;
        }
    }
}

/// `Tok` 必须覆盖高亮器**实际会产出**的每个名字。
// 依赖 arborium/arborium_theme 的用例：纯净模式没有它们
#[cfg(feature = "preinstalled")]
/// 漏一个的后果不是报错，而是那一类片段从此没有颜色（`from_name` 返回 `None` 被丢弃）。
#[test]
fn tok_table_covers_every_name_the_highlighter_produces() {
    let corpus: &[(&str, &str)] = &[
        (
            "rust",
            "fn main() { let v = vec![1, 2]; println!(\"{:?}\", v); }",
        ),
        ("python", "def f(x):\n    return {'a': 1}\n"),
        ("javascript", "const f = (x) => x + 1; // note\n"),
        (
            "html",
            "<!DOCTYPE html>\n<html lang=\"zh\"><body class=\"a\">t</body></html>\n",
        ),
        ("css", ".a { color: #fff; }\n"),
        ("markdown", "# T\n\n[l](http://x)\n\n~~s~~ **b** *i* `c`\n"),
        ("sql", "SELECT id, name FROM t WHERE id = 1; -- note\n"),
        ("java", "public class A { int f(int x) { return x; } }\n"),
    ];

    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for (lang, src) in corpus {
        for (_, _, name) in themed_spans(lang, src) {
            assert!(
                is_builtin_token(name),
                "{lang} 产出了表外的 tag `{name}`：这一类片段会静默失去颜色。\
                     加进 TOK_TABLE，并补上 .tok-{name} 的样式"
            );
            seen.insert(name);
        }
    }
    assert!(
        seen.len() >= 8,
        "语料太弱，只覆盖了 {} 个 tag，钉不住表：{seen:?}",
        seen.len()
    );
}

/// 每个 `Tok` 都必须在 `styles.css` 里有 `.tok-<name>`。
///
/// 枚举化之后"表里有的 tag 却没人给它配色"是**新的**一类坏：以前未知名字至少还会
/// 拼出一个类名（配不配上色另说），现在得显式确认每个枚举值都真能画出颜色。
/// 反向读源码而不是靠人记 —— 加枚举值忘了配色时这条会红。
/// **query 覆盖（法子 2）真的生效**：拿一份只有一条模式的 `highlights.scm` 换掉编译进来的
/// 那份，结果必须**变**（片段数骤减 + 出现我们指定的 token 名）。
///
/// 为什么必须测"结果变了"而不是"函数返回 Ok"：`with_store` 那条路只要 store 里没插进去，
/// 或者插进去的名字对不上语言别名，仍然会"成功"返回一份**和原来一模一样**的高亮 ——
/// 插件作者会以为自己写的 query 生效了。所以判据是**差异**，不是返回码。
#[test]
#[cfg(feature = "preinstalled")]
fn plugin_query_override_actually_changes_tags() {
    let src = "let x = 1;\nlet y = 2;\n";
    let base = highlight_spans("rust", src, None, None, &[]).expect("默认高亮");
    let over = highlight_spans("rust", src, Some("(identifier) @variable"), None, &[])
        .expect("覆盖 query 之后的高亮");
    assert!(
        over.iter().any(|(_, _, tag)| *tag == "variable"),
        "覆盖后应当出现 variable 片段：{over:?}"
    );
    assert!(
        over.len() < base.len(),
        "覆盖一份只有一条模式的 query 之后片段数应当**变少**（默认 {} → 覆盖后 {}）——\
             数量没变说明覆盖根本没进 store",
        base.len(),
        over.len()
    );
}

/// 覆盖失败要**明确报错**，不能静默退回默认 query（那会让插件作者以为自己的 scm 生效了）。
#[test]
#[cfg(feature = "preinstalled")]
fn broken_query_override_reports_instead_of_silently_falling_back() {
    assert!(
        overridden_highlighter("rust", "(this is not a query").is_err(),
        "写坏的 scm 必须报错"
    );
    assert!(
        overridden_highlighter("cobol", "(x) @variable").is_err(),
        "没有解析器的语言必须报错"
    );
}

/// **纯净模式的定义**：没有内置解析器 ⇒ 任何语言都返回空片段 ⇒ 前端按纯文本渲染。
///
/// 这条判据的价值不在"函数返回空"，而在**它必须是有意的**：纯净模式最容易出的错是
/// "高亮没了但界面不说"，用户只会觉得程序坏了。所以这条路必须有一个测试盯着它。
#[test]
#[cfg(not(feature = "preinstalled"))]
fn pure_mode_has_no_highlighter_at_all() {
    let spans = highlight_spans("rust", "let x = 1;\n", None, None, &[]).expect("不该报错");
    assert!(spans.is_empty(), "纯净模式不该产出任何片段：{spans:?}");
    assert!(detect_language("a.rs").is_none(), "纯净模式没有内置探测表");
}

/// 每个内置 token 名都必须在**预装插件的主题**里有 `.tok-<name>`。
///
/// v1.0.0 起这份主题不再躺在 `ui/styles.css` 里，而是插件的一部分
/// （`plugins/highlight/ruyix-builtin/theme.css`）：**语法高亮作为预装插件**的判据就在这里 ——
/// 名字表在代码里、颜色在插件里，两边对不上就是"渲染成默认色、看起来像高亮丢了"。
#[test]
fn every_tag_has_a_css_class() {
    let theme = include_str!("../../plugins/highlight/ruyix-builtin/theme.css");
    let names: Vec<&str> = builtin_token_names();
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !theme.contains(&format!(".tok-{n} {{")))
        .collect();
    assert!(
        missing.is_empty(),
        "预装插件主题里缺这些 `.tok-*` 规则：{missing:?}\n\
             （高亮器能产出它们，缺了就是渲染成默认色 —— 改主题时别忘了同步）"
    );
}

/// **交付出去的那份 CSS 必须还是有效 CSS**（补的判据，2026-09-27）。
///
/// 上面那条只查"主题**文件**里有没有 `.tok-x`"；可真正注入进 WebView 的不是文件，
/// 而是 `plugin::filter_theme_css` 过完一遍的**产物**。现场（真事故）：过滤时用
/// `split('}')` 切规则、组回去漏了右花括号 ⇒ 整份注入 CSS 是一段永不闭合的块 ⇒
/// 浏览器只认第一条规则的属性（那份主题的第一条正好是注释）⇒ 症状"**只有注释高亮了**"；
/// 而**文件本身完全正常**，上面那条判据一直是绿的。
///
/// 教训与 §10.3 是同一句话：**判据要量交付物，不要量源料**。
#[test]
fn the_css_we_inject_is_valid_css() {
    let theme = include_str!("../../plugins/highlight/ruyix-builtin/theme.css");
    let (kept, dropped) = plugin::filter_theme_css(theme);
    assert!(
        dropped.is_empty(),
        "内置主题不该有被丢弃的规则：{dropped:?}"
    );
    // ① 结构：花括号配平，且每条规则自成一段（缺 `}` 的规则会把后面全部吞掉）
    assert_eq!(
        kept.matches('{').count(),
        kept.matches('}').count(),
        "注入的 CSS 花括号不配平 ⇒ 浏览器只认第一条规则：{kept}"
    );
    for seg in kept.split('}').filter(|s| !s.trim().is_empty()) {
        assert_eq!(seg.matches('{').count(), 1, "这条规则不完整：{seg:?}");
    }
    // ② 覆盖：名字表里的每个 token 都要在**注入产物**里有配色
    let missing: Vec<&str> = builtin_token_names()
        .iter()
        .copied()
        .filter(|n| !kept.contains(&format!(".tok-{n} {{")))
        .collect();
    assert!(
        missing.is_empty(),
        "注入的 CSS 里缺这些 token 的配色：{missing:?}（渲染成默认色 = 看着像高亮丢了）"
    );
}

/// 预装插件的清单必须覆盖内置的 8 门语言（否则"预装了但少一半语言"没人发现）。
#[test]
fn preinstalled_plugin_covers_builtin_languages() {
    let manifest = include_str!("../../plugins/highlight/ruyix-builtin/plugin.toml");
    for lang in [
        "python",
        "rust",
        "html",
        "css",
        "javascript",
        "markdown",
        "sql",
        "java",
    ] {
        assert!(
            manifest.contains(&format!("id = \"{lang}\"")),
            "预装插件清单里没有 {lang}：解析器编进来了却没人认领"
        );
    }
}

// ============================================
// 运行目录解析（0.0.4 修复：绑定清单文件后应在该文件所在目录运行）
// ============================================

/// 临时项目目录，Drop 时自动清理
struct TempProj(std::path::PathBuf);

impl TempProj {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间异常")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "dh-code-run-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        TempProj(dir)
    }

    fn path(&self) -> String {
        self.0.to_string_lossy().to_string()
    }

    /// 写文件（自动创建父目录）
    fn write(&self, rel: &str, content: &str) {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("创建父目录失败");
        }
        std::fs::write(&p, content).expect("写文件失败");
    }
}

impl Drop for TempProj {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 路径比较用规范化：统一分隔符、去尾部斜杠、忽略大小写
fn norm(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// 期望路径对齐 canonicalize 结果：resolve_run_dir 返回真实路径，
/// 而 macOS 的 temp_dir() 是 /private/var 的符号链接（/var/...），未解析
fn canon(p: &std::path::Path) -> String {
    clean_path(&p.canonicalize().expect("canonicalize 失败"))
}

#[test]
fn run_dir_defaults_to_project_root() {
    let proj = TempProj::new("root");
    assert_eq!(
        resolve_run_dir(Some(&proj.path()), None).map(|d| norm(&d)),
        Some(norm(&proj.path())),
        "未绑定文件时应使用项目根目录"
    );
    assert_eq!(
        resolve_run_dir(Some(&proj.path()), Some("  ")).map(|d| norm(&d)),
        Some(norm(&proj.path())),
        "空白 bind 应等价于未绑定"
    );
}

/// 复现用例：bind = admin-web\package.json、cmd = npm start
/// 期望工作目录是 <项目根>/admin-web（修复前是项目根 → 报错）
#[test]
fn run_dir_uses_bind_file_parent_dir() {
    let proj = TempProj::new("bind");
    proj.write("admin-web/package.json", "{\"name\":\"admin-web\"}");

    // Windows 配置里的 bind 可能写作反斜杠分隔；
    // Unix 上反斜杠是普通文件名字符，不作为分隔符解析，只测正斜杠
    #[cfg(windows)]
    let binds = [r"admin-web\package.json", "admin-web/package.json"];
    #[cfg(not(windows))]
    let binds = ["admin-web/package.json"];

    for bind in binds {
        let dir = resolve_run_dir(Some(&proj.path()), Some(bind)).expect("应解析出工作目录");
        assert_eq!(
            norm(&dir),
            norm(&canon(&proj.0.join("admin-web"))),
            "bind={} 时应在该文件所在目录运行",
            bind
        );
    }
}

#[test]
fn run_dir_bind_at_project_root_stays_at_root() {
    let proj = TempProj::new("rootbind");
    proj.write("package.json", "{}");
    let dir = resolve_run_dir(Some(&proj.path()), Some("package.json")).expect("应有工作目录");
    assert_eq!(
        norm(&dir),
        norm(&canon(&proj.0)),
        "根目录下的清单文件 → 项目根"
    );
}

#[test]
fn run_dir_bind_dir_uses_dir_itself() {
    let proj = TempProj::new("dirbind");
    std::fs::create_dir_all(proj.0.join("admin-web")).expect("创建目录失败");
    let dir = resolve_run_dir(Some(&proj.path()), Some("admin-web")).expect("应有工作目录");
    assert_eq!(norm(&dir), norm(&canon(&proj.0.join("admin-web"))));
}

#[test]
fn run_dir_missing_dir_falls_back_to_root() {
    let proj = TempProj::new("missing");
    let dir =
        resolve_run_dir(Some(&proj.path()), Some("not-exist/package.json")).expect("应有工作目录");
    assert_eq!(norm(&dir), norm(&proj.path()), "目录不存在应回退项目根");
}

#[test]
fn run_dir_blocks_path_escape() {
    let proj = TempProj::new("escape");
    let outside_name = format!("dh-code-run-test-escape-outside-{}", std::process::id());
    let outside = proj
        .0
        .parent()
        .expect("临时目录应有父目录")
        .join(&outside_name);
    std::fs::create_dir_all(&outside).expect("创建目录失败");

    let dir = resolve_run_dir(
        Some(&proj.path()),
        Some(&format!("../{outside_name}/package.json")),
    )
    .expect("应有工作目录");
    assert_eq!(
        norm(&dir),
        norm(&proj.path()),
        "越出项目根的 bind 应回退项目根"
    );

    let _ = std::fs::remove_dir_all(&outside);
}

/// 端到端：走真实 run_target（命令解析 + 工作目录设置），
/// Windows 用 `cmd /c cd`、Unix 用 `pwd` 回显实际工作目录
#[test]
fn run_target_executes_in_bind_dir() {
    let proj = TempProj::new("e2e");
    proj.write("admin-web/package.json", "{}");

    #[cfg(windows)]
    let (cmd, bind) = ("cmd /c cd", r"admin-web\package.json");
    #[cfg(not(windows))]
    let (cmd, bind) = ("pwd", "admin-web/package.json");

    let out = tauri::async_runtime::block_on(run_target(
        cmd.to_string(),
        Some(proj.path()),
        Some(bind.to_string()),
    ))
    .expect("run_target 执行失败");

    assert_eq!(out.exit_code, Some(0), "命令应执行成功: {}", out.stderr);
    assert!(
        out.stdout.to_lowercase().contains("admin-web"),
        "实际工作目录应包含 admin-web，实际输出: {:?}",
        out.stdout
    );
}

// ---- 外链闸门（P0：一次链接点击不能顶掉整个 IDE）----

fn u(s: &str) -> tauri::Url {
    tauri::Url::parse(s).expect("测试用 URL 应当合法")
}

/// 用户踩到的那个 P0 原样复现：agent 起了服务、回了 `http://localhost:8080`，点一下整块 IDE 被换掉。
/// 判据是"是不是自家文档"，所以 **localhost 不是自家页面** —— 这正是这次要拦的东西。
#[test]
fn localhost_is_not_an_app_document() {
    assert_eq!(
        nav_verdict(&u("http://localhost:8080/"), None),
        Nav::External
    );
    assert_eq!(
        nav_verdict(&u("http://localhost:8080/actuator/health"), None),
        Nav::External
    );
    assert_eq!(
        nav_verdict(&u("http://127.0.0.1:8080/"), None),
        Nav::External
    );
    assert_eq!(nav_verdict(&u("http://[::1]:8080/"), None), Nav::External);
    assert_eq!(
        nav_verdict(&u("https://newest-ai.com"), None),
        Nav::External
    );
}

/// 自家文档放行；同文档锚点还得放行 —— 拦了"跳到某节"就成了点了没反应。
#[test]
fn app_documents_are_allowed_along_with_in_page_anchors() {
    assert_eq!(nav_verdict(&u("http://tauri.localhost/"), None), Nav::Allow);
    assert_eq!(
        nav_verdict(&u("http://tauri.localhost/index.html"), None),
        Nav::Allow
    );
    assert_eq!(
        nav_verdict(&u("tauri://localhost/index.html"), None),
        Nav::Allow
    );
    assert_eq!(nav_verdict(&u("about:blank"), None), Nav::Allow);
    assert_eq!(
        nav_verdict(&u("http://tauri.localhost/index.html#sessions"), None),
        Nav::Allow
    );
}

/// 同源但**不是文档**的路径（markdown 里的相对链接 `src/main.rs` 解析出来就是它）不能放行：
/// 导航过去是一张 404 白页，和跳去外站一样丢状态；而它也没有"交给浏览器"的出口 → 拒。
#[test]
fn same_site_non_documents_are_refused_without_an_exit() {
    assert_eq!(
        nav_verdict(&u("http://tauri.localhost/src/main.rs"), None),
        Nav::Refuse
    );
    assert_eq!(
        nav_verdict(&u("http://tauri.localhost/styles.css"), None),
        Nav::Refuse
    );
}

/// 开发服务器（配置里写了才认）：认的是**配置那一条**，不是"凡是 localhost"。
#[test]
fn a_configured_dev_url_is_recognized_without_whitelisting_localhost() {
    let dev = u("http://localhost:1420/");
    assert_eq!(
        nav_verdict(&u("http://localhost:1420/index.html"), Some(&dev)),
        Nav::Allow
    );
    assert_eq!(
        nav_verdict(&u("http://localhost:8080/"), Some(&dev)),
        Nav::External,
        "另一个 localhost 端口不是开发服务器"
    );
    assert_eq!(
        nav_verdict(&u("https://newest-ai.com"), Some(&dev)),
        Nav::External
    );
}

/// 外链出口的白名单：只有三种 scheme 能落到操作系统，
/// `file:` / `javascript:` / `data:` 连试都不试（这是**能执行之前**的闸门）。
#[test]
fn open_url_whitelists_exactly_three_schemes() {
    assert_eq!(
        check_open_url("  https://newest-ai.com/x?a=1&b=2  ").expect("https 应当放行"),
        "https://newest-ai.com/x?a=1&b=2",
        "放行时要带上完整查询串"
    );
    assert!(check_open_url("http://localhost:8080/actuator").is_ok());
    assert!(check_open_url("mailto:yujianboisme@outlook.com").is_ok());
    for bad in [
        "file:///C:/Windows/System32/calc.exe",
        "javascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "not a url",
        "",
    ] {
        assert!(check_open_url(bad).is_err(), "{bad} 必须被拦下");
    }
}
