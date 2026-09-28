#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""main.rs 1 拆 N —— 第五刀：语法高亮 → src-tauri/src/highlight.rs。

高亮在 main.rs 里是**三段**（原始布局就是这样，所以按三段搬，顺序不变）：
  ① 数据结构段：`Tok` / `builtin_token_names` / `is_builtin_token` / `TOK_TABLE` / `HighlightPayload`
  ② Tauri 命令段：`highlight_code` / `resolve_language` / `detect_language` / `highlight_spans`
     / `overridden_highlighter` / `highlight_plugins`
  ③ "PTY 终端命令"段尾部（**那段名不副实**：高亮的载荷构造函数一直寄居在这里）：
     `unit_table` / `resolve_token_name` / `build_line_highlights`

留在 main.rs：`mod highlight;` + invoke_handler 的 2 条注册（`highlight::highlight_code,` /
`highlight::highlight_plugins,`）。`plugin.rs` 的两处 `crate::is_builtin_token` /
`crate::builtin_token_names` 改成 `crate::highlight::…`（token 名表的唯一来源跟着搬了）。

自检：三段边界形状 / 每段的项逐个点名 / 顶层项守恒 / `crate::` 前缀命中数。
"""
import io
import os
import re

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
MAIN = os.path.join(ROOT, "src-tauri", "src", "main.rs")
TESTS = os.path.join(ROOT, "src-tauri", "src", "tests.rs")
PLUGIN = os.path.join(ROOT, "src-tauri", "src", "plugin.rs")
HL = os.path.join(ROOT, "src-tauri", "src", "highlight.rs")

HEADER = """\
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
//!    用它的行去拼 textarea 会让"以换行结尾的文件"少一个 `\\n`。
//!
//! **性能约束（硬要求）**：只允许对源码做一次线性扫描；`tests.rs` 有一条用例专门用
//! "线性版 vs 参照实现"的耗时比值（>3×）钉住它别退化回逐行重扫。
//!
//! 配色**不在编译期**：`ui/styles.css` / `index.html` 里没有 `.tok-*` 规则，全部来自插件
//! （`plugins/highlight/<id>/theme.css`）；[`builtin_token_names`] / [`is_builtin_token`] 是
//! 内置名字的唯一来源（`plugin.rs` 校验插件 `token_map` 时用它）。

"""


def rd(p):
    with io.open(p, encoding="utf-8", newline="") as f:
        return f.read()


def wr(p, t):
    with io.open(p, "w", encoding="utf-8", newline="") as f:
        f.write(t)


def sub(t, old, new, n=1, tag=""):
    nl = "\r\n" if "\r\n" in t else "\n"
    o, w = old.replace("\n", nl), new.replace("\n", nl)
    c = t.count(o)
    if c != n:
        raise SystemExit("!! %s 命中 %d 次（期望 %d）\n--- 锚点 ---\n%s" % (tag, c, n, old[:200]))
    return t.replace(o, w), c


def items(src):
    out = set()
    for m in re.finditer(r"(?m)^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?"
                         r"(fn|enum|struct|const|static|type|trait|union|mod)\s+([A-Za-z_]\w*)", src):
        if m.group(1) != "mod":
            out.add(m.group(1) + " " + m.group(2))
    return out


def main():
    main_t = rd(MAIN)
    nl = "\r\n" if "\r\n" in main_t else "\n"
    if "mod highlight;" in main_t:
        print("highlight.rs 已经拆过了 —— 这活已经做完")
        return 0
    if os.path.exists(HL):
        raise SystemExit("!! highlight.rs 已存在但 main.rs 没接线，先弄清状态")

    before = items(main_t) | items(rd(TESTS))
    lines = main_t.split(nl)

    def one(pred, tag):
        hits = [i for i, l in enumerate(lines) if pred(l)]
        if len(hits) != 1:
            raise SystemExit("!! %s 命中 %d 次（应为 1）" % (tag, len(hits)))
        return hits[0]

    def col0_close(after, tag):
        i = next((i for i in range(after + 1, len(lines)) if lines[i] == "}"), None)
        if i is None:
            raise SystemExit("!! 找不到 %s 的收尾 `}`" % tag)
        return i

    # ---- 段 ①：Tok … HighlightPayload（含 Tok 的 doc）----
    a0 = one(lambda l: l.startswith("/// 语法高亮的 tag（前端 CSS 类是"), "① 起锚点")
    a1 = one(lambda l: l.startswith("struct HighlightPayload {"), "HighlightPayload")
    a1 = col0_close(a1, "HighlightPayload")

    # ---- 段 ②：highlight_code … highlight_plugins ----
    b1 = one(lambda l: l.startswith("async fn highlight_code("), "highlight_code")
    b0 = b1
    while b0 > 0 and (lines[b0 - 1].startswith("///") or lines[b0 - 1].startswith("#[")
                      or lines[b0 - 1].startswith("//")):
        b0 -= 1
    b2 = one(lambda l: l.startswith("fn highlight_plugins("), "highlight_plugins")
    b2 = col0_close(b2, "highlight_plugins")

    # ---- 段 ③：unit_table … build_line_highlights ----
    c0 = one(lambda l: l.startswith("/// 行内「字节偏移 → UTF-16 码元偏移」查表"), "③ 起锚点")
    c1 = one(lambda l: l.startswith("fn build_line_highlights("), "build_line_highlights")
    c1 = col0_close(c1, "build_line_highlights")

    ranges = [(a0, a1), (b0, b2), (c0, c1)]
    for (s, e), tag in zip(ranges, ("①", "②", "③")):
        if lines[s - 1].strip() != "" or lines[e + 1].strip() != "":
            raise SystemExit("!! 段 %s 前后应当是空行：%r / %r" % (tag, lines[s - 1], lines[e + 1]))

    blocks = [lines[s:e + 1] for s, e in ranges]
    names = ("Tok", "builtin_token_names", "is_builtin_token", "TOK_TABLE", "HighlightPayload")
    for nm in names:
        if not any(re.search(r"\b%s\b" % nm, l) for l in blocks[0]):
            raise SystemExit("!! 段① 里没有 %s" % nm)
    for nm in ("highlight_code", "resolve_language", "detect_language", "highlight_spans",
               "overridden_highlighter", "highlight_plugins"):
        if not any(re.search(r"\bfn %s\b" % nm, l) for l in blocks[1]):
            raise SystemExit("!! 段② 里没有 fn %s" % nm)
    for nm in ("unit_table", "resolve_token_name", "build_line_highlights"):
        if not any(re.search(r"\bfn %s\b" % nm, l) for l in blocks[2]):
            raise SystemExit("!! 段③ 里没有 fn %s" % nm)

    # ---- highlight.rs：可见性 + crate:: 前缀 + Mutex ----
    body = nl.join(nl.join(b) for b in blocks)          # 三段之间留一个空行
    for old, new, n, tag in (
        ("enum Tok {", "pub(crate) enum Tok {", 1, "Tok"),
        ("const TOK_TABLE:", "pub(crate) const TOK_TABLE:", 1, "TOK_TABLE"),
        ("struct HighlightPayload {", "pub struct HighlightPayload {", 1, "HighlightPayload"),
        ("    tags: Vec<&'static str>,", "    pub tags: Vec<&'static str>,", 1, "payload.tags"),
        ("    lines: Vec<Vec<u32>>,", "    pub lines: Vec<Vec<u32>>,", 1, "payload.lines"),
        ("async fn highlight_code(", "pub async fn highlight_code(", 1, "highlight_code"),
        ("fn detect_language(", "pub(crate) fn detect_language(", 2, "detect_language（两个平台/模式版本）"),
        ("fn highlight_spans(", "pub(crate) fn highlight_spans(", 2, "highlight_spans ×2"),
        ("fn overridden_highlighter(", "pub(crate) fn overridden_highlighter(", 1, "overridden_highlighter"),
        ("fn highlight_plugins(", "pub fn highlight_plugins(", 1, "highlight_plugins"),
        ("fn resolve_token_name(", "pub(crate) fn resolve_token_name(", 1, "resolve_token_name"),
        ("fn build_line_highlights(", "pub(crate) fn build_line_highlights(", 1, "build_line_highlights"),
        # 兄弟模块要从 crate:: 走（子模块里的裸 `plugin::` 解析不到）
        ("&plugin::Registry", "&crate::plugin::Registry", 1, "crate::plugin（resolve_language 签名）"),
        ("Mutex<plugin::Registry>", "Mutex<crate::plugin::Registry>", 2, "Mutex<crate::plugin>"),
        ("preinstalled::mode()", "crate::preinstalled::mode()", 1, "crate::preinstalled"),
    ):
        body, _ = sub(body, old, new, n, tag)
    wr(HL, HEADER.replace("\n", nl) + "use std::sync::Mutex;\n\n" + body + nl)

    # ---- main.rs：删三段（倒序，免得下标串位）+ 接线 ----
    rest = list(lines)
    for s, e in sorted(ranges, reverse=True):
        del rest[s - 1:e + 2]              # 连段落前/后的空行一起去掉
    out = nl.join(rest)
    out, _ = sub(out, "mod git;\n", "mod git;\nmod highlight;\n", 1, "mod 声明")
    for c in ("highlight_code", "highlight_plugins"):
        out, _ = sub(out, "            %s,\n" % c, "            highlight::%s,\n" % c, 1,
                     "注册 %s" % c)
    wr(MAIN, out)

    # ---- plugin.rs：token 名表的唯一来源跟着搬了 ----
    p = rd(PLUGIN)
    p, _ = sub(p, "crate::is_builtin_token(", "crate::highlight::is_builtin_token(", 1, "plugin.rs 1")
    p, _ = sub(p, "crate::builtin_token_names()", "crate::highlight::builtin_token_names()", 1,
               "plugin.rs 2")
    wr(PLUGIN, p)

    # ---- tests.rs：高亮用例显式引；**按用例的 cfg 门禁分组**（纯净模式里高亮整套不存在，
    #      不分组的显式 `use` 会变成 unused import —— 那是 clippy 门禁的一条）----
    t = rd(TESTS)
    t, _ = sub(t, "use super::*;\n",
               "use super::*;\n"
               "use crate::highlight::{builtin_token_names, highlight_spans};\n"
               "#[cfg(feature = \"preinstalled\")]\n"
               "use crate::highlight::{\n"
               "    HighlightPayload, TOK_TABLE, build_line_highlights, is_builtin_token, "
               "overridden_highlighter,\n    resolve_token_name,\n};\n"
               "#[cfg(not(feature = \"preinstalled\"))]\n"
               "use crate::highlight::detect_language;\n",
               1, "tests.rs 引入 highlight")
    wr(TESTS, t)

    after = items(rd(MAIN)) | items(rd(TESTS)) | items(rd(HL))
    if after != before:
        raise SystemExit("!! 顶层项集合变了：丢 %s / 多 %s"
                         % (sorted(before - after), sorted(after - before)))
    print("main.rs %d → %d 行；highlight.rs %d 行；顶层项 %d 个（守恒 ✓）"
          % (len(lines), len(rd(MAIN).split(nl)), len(rd(HL).split(nl)), len(before)))
    print("三段：① %d 行  ② %d 行  ③ %d 行" % (len(blocks[0]), len(blocks[1]), len(blocks[2])))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
