#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""main.rs 1 拆 N —— 第二刀：外链 / 导航闸门（P0）→ src-tauri/src/nav.rs。

搬走（整段，原文照抄，只加可见性）：
    Nav / is_app_host / same_site / is_document_path / nav_verdict / check_open_url /
    open_external / launch_in_browser（两个平台版本）+ 那段 P0 说明
留在 main.rs：
    build_main_window（只把 5 处调用加 `nav::` 前缀）+ invoke_handler 的注册

自检：
  · 每个锚点的命中数（≠ 期望即失败）
  · **内容守恒**：main.rs+tests.rs 的顶层项名字集合，搬移前后必须一模一样
  · 搬完跑 `cargo check -p ruyix --bins`（脚本只做文本手术，编译器才是判据）
"""
import io
import os
import re

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
MAIN = os.path.join(ROOT, "src-tauri", "src", "main.rs")
TESTS = os.path.join(ROOT, "src-tauri", "src", "tests.rs")
NAV = os.path.join(ROOT, "src-tauri", "src", "nav.rs")


def rd(p):
    with io.open(p, encoding="utf-8", newline="") as f:
        return f.read()


def wr(p, t):
    with io.open(p, "w", encoding="utf-8", newline="") as f:
        f.write(t)


def sub(t, old, new, n=1, tag=""):
    """锚点替换 + 命中数断言（CRLF 感知）。返回 (新文本, 命中数)。"""
    nl = "\r\n" if "\r\n" in t else "\n"
    o, w = old.replace("\n", nl), new.replace("\n", nl)
    c = t.count(o)
    if c != n:
        raise SystemExit("!! %s 命中 %d 次（期望 %d）\n--- 锚点 ---\n%s" % (tag, c, n, old[:200]))
    return t.replace(o, w), c


def items(src):
    """顶层项名字集合（用来做内容守恒自检）。`mod x;` 声明不算 —— 搬移会新增一条。"""
    out = set()
    for m in re.finditer(r"(?m)^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?"
                         r"(fn|enum|struct|const|static|type|trait|union|mod)\s+([A-Za-z_]\w*)", src):
        if m.group(1) == "mod":
            continue
        out.add(m.group(1) + " " + m.group(2))
    return out


HEADER = """\
//! 外链与导航闸门（P0）—— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第二刀）。
//!
//! 判据在 [`nav_verdict`]（纯函数，`src-tauri/src/tests.rs` 钉着：localhost 不是自家页面、
//! 同源非文档路径拒掉、`about:blank` 放行），出口在 [`open_external`]（三种 scheme 白名单 +
//! 系统默认处理程序，**不走 shell**；`file:` / `javascript:` / `data:` 拦在能执行之前）。
//!
//! `main.rs` 只留两处接线：`build_main_window` 的 `on_navigation` / `on_new_window`
//! （真闸门 —— 窗口必须建在 Rust 里才挂得上）与 invoke_handler 里的 `nav::open_external`。
//!
//! 下面是拆出来时**原封不动**的那段说明（这条 P0 的"为什么"）：

"""

NAV_VIS = [
    # (原文, 换成的, 期望命中数) —— 只加可见性，一个字不改
    ("enum Nav {", "pub(crate) enum Nav {", 1),
    ("fn nav_verdict(url: &tauri::Url", "pub(crate) fn nav_verdict(url: &tauri::Url", 1),
    ("fn check_open_url(raw: &str)", "pub(crate) fn check_open_url(raw: &str)", 1),
    ("fn open_external(url: String)", "pub fn open_external(url: String)", 1),
    # 两个平台各一份，都得加
    ("fn launch_in_browser(url: &str)", "pub(crate) fn launch_in_browser(url: &str)", 2),
]

WIRING = [
    # build_main_window：调用加前缀（判据与出口都在 nav 里）
    ("match nav_verdict(url, nav_dev_url.as_ref()) {",
     "match nav::nav_verdict(url, nav_dev_url.as_ref()) {"),
    ("            Nav::Allow => true,", "            nav::Nav::Allow => true,"),
    ("            Nav::External => {", "            nav::Nav::External => {"),
    ("            Nav::Refuse => {", "            nav::Nav::Refuse => {"),
    ("if let Err(e) = launch_in_browser(url.as_str()) {",
     "if let Err(e) = nav::launch_in_browser(url.as_str()) {"),
    ("if nav_verdict(&url, dev_url.as_ref()) == Nav::External",
     "if nav::nav_verdict(&url, dev_url.as_ref()) == nav::Nav::External"),
    ("&& let Err(e) = launch_in_browser(url.as_str())",
     "&& let Err(e) = nav::launch_in_browser(url.as_str())"),
    # invoke_handler 的注册（留在 main.rs —— 注册是接线，不是实现）
    ("            open_external,\n", "            nav::open_external,\n"),
    # mod 声明（按字母序插在 mcp 与 paths 之间）
    ("mod mcp;\nmod paths;", "mod mcp;\nmod nav;\nmod paths;"),
]


def main():
    main_t, tests_t = rd(MAIN), rd(TESTS)
    nl = "\r\n" if "\r\n" in main_t else "\n"

    if "mod nav;" in main_t:
        print("nav.rs 已经拆过了（main.rs 里已有 `mod nav;`）—— 这活已经做完")
        return 0
    if os.path.exists(NAV):
        raise SystemExit("!! src-tauri/src/nav.rs 已存在但 main.rs 还没接线，先弄清状态")

    before = items(main_t) | items(tests_t)

    lines = main_t.split(nl)
    start_anchor = "// 外链：WebView 只是我们的画布，不是浏览器（P0）"
    end_anchor = '.map_err(|e| format!("系统没有能打开它的程序（{opener}: {e}）"))'
    starts = [i for i, l in enumerate(lines) if l.strip() == start_anchor]
    ends = [i for i, l in enumerate(lines) if l.strip() == end_anchor]
    if len(starts) != 1 or len(ends) != 1:
        raise SystemExit("!! 段落边界锚点命中 %d / %d 次（各应为 1）" % (len(starts), len(ends)))
    s, e = starts[0], ends[0]
    # 段落横幅的上横线（`// ===…`）也要跟着搬：只搬"标签行"会把一条孤零零的
    # `// =====` 留在 main.rs（拆完 review 时抓到的），nav.rs 那边的横幅也不完整。
    rule = lines[s - 1]
    if not re.match(r"^// =+$", rule) or lines[s + 1] != rule:
        raise SystemExit("!! 横幅形状变了：%r / %r / %r" % (lines[s - 1], lines[s], lines[s + 1]))
    s = s - 1
    if lines[s - 1].strip() != "":
        raise SystemExit("!! 横幅之前应当是空行：%r" % lines[s - 1])
    e2 = next((i for i in range(e + 1, len(lines)) if lines[i] == "}"), None)
    if e2 is None or lines[e2 + 1].strip() != "" or not lines[e2 + 2].startswith("/// 建主窗口"):
        raise SystemExit("!! 段尾形状变了：%r" % (lines[e + 1:e + 5],))
    e = e2                         # 含收尾 `}`
    block = lines[s:e + 1]

    want = {
        "enum Nav {": 1,
        "fn nav_verdict(": 1,
        "fn check_open_url(": 1,
        "fn open_external(": 1,
        "fn is_app_host(": 1,
        "fn is_document_path(": 1,
        "fn same_site(": 1,
        "fn launch_in_browser(url: &str)": 2,   # 两个平台各一份
    }
    for k, want_n in want.items():
        n = sum(1 for l in block if l.startswith(k))
        if n != want_n:
            raise SystemExit("!! 搬移块里 %r 出现 %d 次（应为 %d）" % (k, n, want_n))
    for cfg_line, want_n in (("#[cfg(windows)]", 1), ("#[cfg(not(windows))]", 1)):
        n = sum(1 for l in block if l.strip() == cfg_line)
        if n != want_n:
            raise SystemExit("!! 搬移块里 %r 出现 %d 次（应为 %d）—— 两个平台版本都得在块里"
                             % (cfg_line, n, want_n))

    # 1) nav.rs
    body = nl.join(block)
    for old, new, n in NAV_VIS:
        body, _ = sub(body, old, new, n, "nav 可见性 %s" % old)
    wr(NAV, HEADER.replace("\n", nl) + body + nl)

    # 2) main.rs：删块 + 接线
    rest = lines[:s] + lines[e + 1:]
    while rest and rest[s - 1].strip() == "" and rest[s].strip() == "":
        del rest[s]                      # 别留双空行
    out = nl.join(rest)
    for old, new in WIRING:
        out, _ = sub(out, old, new, 1, "接线 %s" % old.strip()[:50])
    wr(MAIN, out)

    # 3) tests.rs：测试原来靠 `use super::*` 摸到这些项，现在要显式引
    tests_out, _ = sub(
        tests_t,
        "use super::*;\n",
        "use super::*;\nuse crate::nav::{check_open_url, nav_verdict, Nav};\n",
        1, "tests.rs 引入 nav",
    )
    wr(TESTS, tests_out)

    # 4) 内容守恒
    after = items(rd(MAIN)) | items(rd(TESTS)) | items(rd(NAV))
    if after != before:
        raise SystemExit("!! 顶层项集合变了：丢了 %s / 多了 %s"
                         % (sorted(before - after), sorted(after - before)))

    print("main.rs %d → %d 行；nav.rs %d 行；顶层项 %d 个（守恒 ✓）"
          % (len(lines), len(rd(MAIN).split(nl)), len(rd(NAV).split(nl)), len(before)))
    print("搬走的项：%s" % ", ".join(sorted(
        n for n in items(rd(NAV)) if n not in items(rd(MAIN)))))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
