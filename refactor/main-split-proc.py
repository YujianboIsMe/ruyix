#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""main.rs 1 拆 N —— 第四刀：托管进程 / 运行目标 → src-tauri/src/proc_cmds.rs。

搬走：
  · 两条平台常量 `CREATE_NEW_CONSOLE` / `CREATE_NO_WINDOW`（全仓库只在这一段用）+ 各自的说明
  · 段落横幅与"为什么宿主必须能看见它们"说明
  · proc_list / proc_stop / proc_log_read、`RunOutput`、resolve_run_dir、run_target、spawn_terminal
留在 main.rs：invoke_handler 的 5 条注册（改成 `proc_cmds::*`）
另加 `use std::path::Path;` + `#[cfg(windows)] use std::os::windows::process::CommandExt;`

自检：锚点命中数 / 顶层项守恒 / 搬移块里 5 条命令 + 1 个 struct；改完 `cargo check`。
"""
import io
import os
import re

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
MAIN = os.path.join(ROOT, "src-tauri", "src", "main.rs")
TESTS = os.path.join(ROOT, "src-tauri", "src", "tests.rs")
PROC = os.path.join(ROOT, "src-tauri", "src", "proc_cmds.rs")

HEADER = """\
//! 托管进程与运行目标 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第四刀）。
//!
//! 两个域，都在"跑东西"这件事上：
//! · **托管进程**（"服务"面板）：agent 用 `background` 起的服务是**宿主** spawn 的，却不在宿主的
//!   进程树里 —— 以前只活在引擎的进程表里，UI 上等于不存在（起得来、看不见、停不掉）。面板读的
//!   是引擎那**同一份**表（`harness_engine::proc::listing`），不另立一份，否则面板与模型各说各话。
//! · **运行目标**：`run_target` 跑完才回（一次性），工作目录由 `resolve_run_dir` 从 `bind`
//!   清单文件的位置推出来；`spawn_terminal` 是新开一个 OS 控制台窗口（不走 PTY）。
//!
//! `main.rs` 只留注册（`proc_cmds::*`，见 invoke_handler）。测试用 [`resolve_run_dir`]，
//! 见 `src-tauri/src/tests.rs`。

"""

CMDS = ["proc_list", "proc_stop", "proc_log_read", "run_target", "spawn_terminal"]

CONST_START = "/// CREATE_NEW_CONSOLE — 为新进程创建独立控制台窗口"
CONST_END = "const CREATE_NO_WINDOW: u32 = 0x08000000;"
SECT_LABEL = '// 托管进程（"服务"面板）'
END_ANCHOR = 'c.spawn().map_err(|e| format!("启动失败: {}", e))?;'


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
    if "mod proc_cmds;" in main_t:
        print("proc_cmds.rs 已经拆过了 —— 这活已经做完")
        return 0
    if os.path.exists(PROC):
        raise SystemExit("!! proc_cmds.rs 已存在但 main.rs 没接线，先弄清状态")

    before = items(main_t)
    lines = main_t.split(nl)

    c0 = [i for i, l in enumerate(lines) if l == CONST_START]
    c1 = [i for i, l in enumerate(lines) if l == CONST_END]
    if len(c0) != 1 or len(c1) != 1 or c1[0] < c0[0]:
        raise SystemExit("!! 常量块锚点命中 %d / %d 次" % (len(c0), len(c1)))
    consts = lines[c0[0]:c1[0] + 1]
    if lines[c1[0] + 1].strip() != "":
        raise SystemExit("!! 常量块之后应当是空行：%r" % lines[c1[0] + 1])

    lab = [i for i, l in enumerate(lines) if l == SECT_LABEL]
    if len(lab) != 1:
        raise SystemExit("!! 段落标签命中 %d 次" % len(lab))
    s = lab[0] - 1                       # 横幅上横线
    if not re.match(r"^// =+$", lines[s]) or lines[lab[0] + 1] != lines[s]:
        raise SystemExit("!! 横幅形状变了：%r / %r / %r" % (lines[s - 1], lines[s], lines[lab[0] + 1]))
    end = [i for i, l in enumerate(lines) if l.strip() == END_ANCHOR]
    if len(end) != 1:
        raise SystemExit("!! 段尾锚点命中 %d 次" % len(end))
    e = end[0] + 1                       # 跳过可能存在的空行，再认 `Ok(())` 与 fn 的收尾 `}`
    while lines[e].strip() == "":
        e += 1
    if lines[e].strip() != "Ok(())" or lines[e + 1] != "}" or lines[e + 2].strip() != "":
        raise SystemExit("!! 段尾形状变了：%r" % (lines[end[0]:end[0] + 5],))
    e = e + 1                            # spawn_terminal 的收尾 `}`
    block = lines[s:e + 1]

    for k, want_n in (("fn proc_list(", 1), ("async fn proc_stop(", 1), ("fn proc_log_read(", 1),
                      ("struct RunOutput {", 1), ("fn resolve_run_dir(", 1),
                      ("async fn run_target(", 1), ("fn spawn_terminal(", 1)):
        n = sum(1 for l in block if l.startswith(k))
        if n != want_n:
            raise SystemExit("!! 搬移块里 %r 出现 %d 次（应为 %d）" % (k, n, want_n))
    if sum(1 for l in block if l.strip().startswith("/// 全部托管进程")) != 1:
        raise SystemExit("!! 托管进程那段说明不在块里")

    # 可见性：5 条命令 + RunOutput 给 pub；resolve_run_dir 给 pub(crate)（测试要用）；常量保持私有
    body = []
    for l in block:
        if re.match(r"^(?:async )?fn (?:proc_list|proc_stop|proc_log_read|run_target|spawn_terminal)\b", l):
            body.append("pub " + l)
        elif l.startswith("struct RunOutput {"):
            body.append("pub " + l)
        elif l.startswith("fn resolve_run_dir("):
            body.append("pub(crate) " + l)
        else:
            body.append(l)

    # `RunOutput` 的字段也得 pub：`tests.rs` 的端到端用例读 exit_code / stdout / stderr
    # （原来同在一个模块里，私有字段能摸到；拆开之后不行）
    body_t = nl.join(body)
    body_t, _ = sub(body_t,
                    "    exit_code: Option<i32>,\n    stdout: String,\n"
                    "    stderr: String,\n    killed: bool,\n",
                    "    pub exit_code: Option<i32>,\n    pub stdout: String,\n"
                    "    pub stderr: String,\n    pub killed: bool,\n",
                    1, "RunOutput 字段可见性")
    body = body_t.split(nl)

    # 三条 helper（clean_path / split_cmd / resolve_windows_cmd）是**crate 根**的共享工具：
    # git.rs / pty.rs / paths.rs 都在用 `crate::xxx` 调它们（crate 根的私有项对后代模块可见），
    # 所以它们留在 main.rs 不动，这里只把调用改成 `crate::` 前缀（与 git.rs / pty.rs 同口径）。
    for old, new, n in (
        ("Some(clean_path(&dir_canon))", "Some(crate::clean_path(&dir_canon))", 1),
        ("let parts = split_cmd(&cmd);", "let parts = crate::split_cmd(&cmd);", 2),
        ("let program = resolve_windows_cmd(&parts[0]);",
         "let program = crate::resolve_windows_cmd(&parts[0]);", 2),
    ):
        body_t = nl.join(body)
        body_t, _ = sub(body_t, old, new, n, "crate:: 前缀 %s" % old[:30])
        body = body_t.split(nl)

    uses = ("#[cfg(windows)]\nuse std::os::windows::process::CommandExt;\nuse std::path::Path;\n\n")
    wr(PROC, HEADER.replace("\n", nl) + uses.replace("\n", nl)
       + nl.join(consts) + nl + nl + nl.join(body) + nl)

    # main.rs：删常量块、删段落块、注册加前缀、mod 声明
    rest = lines[:c0[0]] + lines[c1[0] + 2:]            # 常量块 + 它后面的空行
    shift = c1[0] + 2 - c0[0]
    s2, e2 = s - shift, e - shift
    if rest[s2 - 1].strip() != "" or rest[e2 + 1].strip() != "":
        raise SystemExit("!! 段落前后应当是空行：%r / %r" % (rest[s2 - 1], rest[e2 + 1]))
    rest = rest[:s2] + rest[e2 + 2:]                    # 段落块 + 它后面的空行
    out = nl.join(rest)
    out, _ = sub(out, "mod preinstalled;\n", "mod preinstalled;\nmod proc_cmds;\n", 1, "mod 声明")
    for c in CMDS:
        out, _ = sub(out, "            %s,\n" % c, "            proc_cmds::%s,\n" % c, 1,
                     "注册 %s" % c)
    # 这两条平台常量的唯一用处跟着搬走了 ⇒ main.rs 的 `CommandExt` 导入成了死导入
    # （clippy 门禁盯 unused import）。`std::path::Path` 全仓库还在用，留着。
    out, _ = sub(out, "#[cfg(windows)]\nuse std::os::windows::process::CommandExt;\n", "", 1,
                 "删掉 main.rs 的 CommandExt 导入")
    wr(MAIN, out)

    # tests.rs：`resolve_run_dir`（8 处用例）+ `run_target`（1 条端到端）原来靠 `use super::*` 摸到
    t = rd(TESTS)
    t, _ = sub(t, "use super::*;\n",
               "use super::*;\nuse crate::proc_cmds::{resolve_run_dir, run_target};\n", 1,
               "tests.rs 引入 resolve_run_dir / run_target")
    wr(TESTS, t)

    after = items(rd(MAIN)) | items(rd(PROC))
    if after != before:
        raise SystemExit("!! 顶层项集合变了：丢 %s / 多 %s"
                         % (sorted(before - after), sorted(after - before)))
    print("main.rs %d → %d 行；proc_cmds.rs %d 行；命令 %d 条 + 2 常量；顶层项 %d 个（守恒 ✓）"
          % (len(lines), len(rd(MAIN).split(nl)), len(rd(PROC).split(nl)), len(CMDS), len(before)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
