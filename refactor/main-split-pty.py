#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""main.rs 1 拆 N —— 第六刀：PTY 终端命令 + 终端目标 → src-tauri/src/pty_cmds.rs。

搬走：`get_term_targets`（原本挂在"Tauri 命令"段尾）+ `pty_spawn` / `pty_write` /
`pty_resize` / `pty_close`（"PTY 终端命令"段）。
**留在 main.rs**：`split_cmd` / `resolve_windows_cmd` —— 它们是 **crate 根共享工具**
（`git.rs` / `pty.rs` / `paths.rs` 都用 `crate::xxx` 调），不是 PTY 的私有实现；
它们原来的"PTY 终端命令"横幅跟着走，所以这里**补一条属于自己的横幅**，免得段落图说谎。

自检：整段边界形状 / 4 条命令 + 1 条 get_term_targets 逐个点名 / 顶层项守恒 / crate:: 前缀命中数。
"""
import io
import os
import re

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
MAIN = os.path.join(ROOT, "src-tauri", "src", "main.rs")
PTY = os.path.join(ROOT, "src-tauri", "src", "pty_cmds.rs")

CMDS = ["get_term_targets", "pty_spawn", "pty_write", "pty_resize", "pty_close"]

HEADER = """\
//! PTY 终端与终端目标 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第六刀）。
//!
//! 两块：
//! · **PTY**（[`pty_spawn`] / [`pty_write`] / [`pty_resize`] / [`pty_close`]）：命令层，
//!   真正的终端在 `pty::PtyManager`（`portable-pty`）；前端用 xterm.js，输出走 `pty://` 事件。
//! · **终端目标**（[`get_term_targets`]）：导航区「终端资源」里可点的终端条目，用户可增删改；
//!   与**运行目标共用同一份扫描器**（`config::ConfigManager::load_term_targets` →
//!   `scan_target_file`），否则"运行目标里能选、终端里找不到"这种漂移迟早发生。
//!
//! **`split_cmd` / `resolve_windows_cmd` 不在这个文件里**：它们是 crate 根的共享工具
//! （`git.rs` / `pty.rs` / `paths.rs` 都用 `crate::` 调），留在 `main.rs`。

"""

NEW_BANNER = """\
// ============================================
// 辅助：命令行拆分 / Windows .cmd 解析（crate 根共享）
// ============================================
//
// 为什么留在 crate 根而不是某个域模块里：`git.rs` / `pty.rs`（PTY 管理器）/ 搬出去的
// `proc_cmds.rs` / `pty_cmds.rs` 都用 `crate::split_cmd` / `crate::resolve_windows_cmd`
// 调它们 —— 是**跨模块**工具（crate 根的私有项对后代模块可见），不是谁家的私有实现。

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
    if "mod pty_cmds;" in main_t:
        print("pty_cmds.rs 已经拆过了 —— 这活已经做完")
        return 0
    if os.path.exists(PTY):
        raise SystemExit("!! pty_cmds.rs 已存在但 main.rs 没接线，先弄清状态")

    before = items(main_t)
    lines = main_t.split(nl)

    def one(pred, tag):
        hits = [i for i, l in enumerate(lines) if pred(l)]
        if len(hits) != 1:
            raise SystemExit("!! %s 命中 %d 次（应为 1）" % (tag, len(hits)))
        return hits[0]

    s = one(lambda l: l.startswith("/// 终端目标（导航区「终端资源」里可点的终端）"), "get_term_targets doc")
    p0 = one(lambda l: l.startswith("fn pty_spawn("), "pty_spawn")
    p1 = one(lambda l: l.startswith("fn pty_close("), "pty_close")
    e = next(i for i in range(p1 + 1, len(lines)) if lines[i] == "}")
    if lines[e] != "}" or lines[e + 1].strip() != "" or not lines[e + 2].startswith("/// 按空格拆分命令行"):
        raise SystemExit("!! 段尾形状变了：%r" % (lines[e:e + 3],))

    span = lines[s:e + 1]
    lab = [i for i, l in enumerate(span) if l == "// PTY 终端命令"]
    if len(lab) != 1 or span[lab[0] - 1] != "// ============================================":
        raise SystemExit("!! PTY 横幅不在段里：%r" % (lab,))

    get_term = span[:lab[0] - 2]              # get_term_targets（含它的 doc 与属性）
    cmds = span[lab[0] + 3:]                  # 横幅后：4 条 pty 命令
    if not get_term[0].startswith("/// 终端目标"):
        raise SystemExit("!! get_term 段头不对：%r" % get_term[0])
    if cmds[0] != "#[tauri::command]" or not cmds[1].startswith("fn pty_spawn("):
        raise SystemExit("!! 命令段头不对：%r" % cmds[:3])
    if sum(1 for l in get_term + cmds if re.match(r"^(?:async )?fn \w+\(", l)) != len(CMDS):
        raise SystemExit("!! 段里函数条数不是 %d" % len(CMDS))

    body = (nl.join(get_term) + nl + nl + nl.join(cmds) + nl)
    for old, new, n, tag in (
        ("fn get_term_targets(", "pub fn get_term_targets(", 1, "get_term_targets"),
        ("fn pty_spawn(", "pub fn pty_spawn(", 1, "pty_spawn"),
        ("fn pty_write(", "pub fn pty_write(", 1, "pty_write"),
        ("fn pty_resize(", "pub fn pty_resize(", 1, "pty_resize"),
        ("fn pty_close(", "pub fn pty_close(", 1, "pty_close"),
        ("Mutex<config::ConfigManager>", "Mutex<crate::config::ConfigManager>", 1, "crate::config"),
        ("Vec<config::RunTarget>", "Vec<crate::config::RunTarget>", 1, "crate::config::RunTarget"),
        ("Mutex<pty::PtyManager>", "Mutex<crate::pty::PtyManager>", 4, "crate::pty"),
    ):
        body, _ = sub(body, old, new, n, tag)
    wr(PTY, HEADER.replace("\n", nl) + "use std::sync::Mutex;\n\n" + body)

    # main.rs：删整段（get_term_targets…pty_close），补一条属于共享工具的横幅，再接线
    out = nl.join(lines[:s] + NEW_BANNER.replace("\n", nl).split(nl) + lines[e + 1:])
    out, _ = sub(out, "            get_term_targets,\n", "            pty_cmds::get_term_targets,\n",
                 1, "注册 get_term_targets")
    for c in ("pty_spawn", "pty_write", "pty_resize", "pty_close"):
        out, _ = sub(out, "            %s,\n" % c, "            pty_cmds::%s,\n" % c, 1,
                     "注册 %s" % c)
    out, _ = sub(out, "mod pty;\n", "mod pty;\nmod pty_cmds;\n", 1, "mod 声明")
    wr(MAIN, out)

    after = items(rd(MAIN)) | items(rd(PTY))
    if after != before:
        raise SystemExit("!! 顶层项集合变了：丢 %s / 多 %s"
                         % (sorted(before - after), sorted(after - before)))
    print("main.rs %d → %d 行；pty_cmds.rs %d 行；命令 %d 条；顶层项 %d 个（守恒 ✓）"
          % (len(lines), len(rd(MAIN).split(nl)), len(rd(PTY).split(nl)), len(CMDS), len(before)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
