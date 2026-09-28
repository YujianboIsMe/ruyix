#!/usr/bin/env python3
"""给 `run_with_ask` 的第 4 个参数（history）后面补上 `images`（v1.3 多模态接线）。

只做一件事：**在参数表里插一个实参**。为什么值得写成脚本而不是手改：调用点散在
单测夹具 / 域测试 / 两个 example 里，手改 9 处容易漏一处（漏了就是编译错，但读 diff 时
很难看出"哪一处没跟上"），而脚本可以断言"找到几个调用点、每个都插到了"。

插入的是 `&[]` —— 这些调用点（单测/冒烟）都没有图，语义上就是"无附件"。
会话那条路（`src-tauri/src/agent/mod.rs`）带真图，属于接线本身，手工改。

用法：`python refactor/mm-wire.py`（幂等：已经有 4 个实参后面跟 `&[]` 的会被跳过）
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SITES = {
    "crates/harness-engine/src/agent/tests.rs": 1,
    "crates/harness-engine/src/agent/tests/ask.rs": 4,
    "crates/harness-engine/src/agent/tests/parse.rs": 2,
    "crates/harness-engine/examples/agent_loop_smoke.rs": 1,
    "crates/harness-engine/examples/llm_tool_probe.rs": 1,
}
ARG_INDEX = 3  # 0=cfg 1=proj 2=task 3=history ⇒ 新实参插在它后面


def split_args(s: str):
    """按**顶层**逗号切参数（跳过字符串/字符字面量与各类括号）—— 参数里有 `format!("…, …")`。"""
    out, depth, i, start = [], 0, 0, 0
    quote = None
    while i < len(s):
        c = s[i]
        if quote:
            if c == "\\":
                i += 2
                continue
            if c == quote:
                quote = None
        elif c in "\"'":
            quote = c
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == "," and depth == 0:
            out.append((start, i))
            start = i + 1
        i += 1
    out.append((start, len(s)))
    return out


def wire(text: str):
    """在每个 `run_with_ask(` 调用点的第 ARG_INDEX 个实参后插 `, &[]`（从后往前改，偏移不失效）。"""
    edits, found = [], 0
    for m in re.finditer(r"\brun_with_ask\(", text):
        open_paren = m.end() - 1
        depth, i, quote = 0, open_paren, None
        while i < len(text):
            c = text[i]
            if quote:
                if c == "\\":
                    i += 2
                    continue
                if c == quote:
                    quote = None
            elif c in "\"'":
                quote = c
            elif c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        args = split_args(text[open_paren + 1 : i])
        if len(args) < ARG_INDEX + 1:
            print(f"!! 有调用点实参只有 {len(args)} 个（期望 >= {ARG_INDEX + 1}）：{m.group(0)}")
            return None, 0
        if len(args) == ARG_INDEX + 2 and "&[]" in text[args[ARG_INDEX + 1][0] : args[ARG_INDEX + 1][1]]:
            continue  # 已经接过线
        edits.append(open_paren + 1 + args[ARG_INDEX][1])
        found += 1
    for at in reversed(edits):
        text = text[:at] + ", &[]" + text[at:]
    return text, found


def main():
    bad = 0
    for rel, expect in SITES.items():
        p = ROOT / rel
        src = p.read_text(encoding="utf-8")
        out, n = wire(src)
        if out is None:
            bad += 1
            continue
        if n != expect:
            print(f"!! {rel}: 插了 {n} 处，期望 {expect}")
            bad += 1
            continue
        p.write_text(out, encoding="utf-8", newline="")
        print(f"ok {rel}: {n} 处")
    if bad:
        print(f"有 {bad} 个文件没对上，未继续")
        sys.exit(1)
    print("全部接线完成")


if __name__ == "__main__":
    main()
