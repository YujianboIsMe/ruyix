#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""agent/action.rs 1 拆 2：把「波次调度 + 批量执行」那一大块抽成 agent/dispatch.rs。

为什么是这一刀：`action.rs` 拆出来时 1840 行，块内次序是

    形状与解析（specs/Action/parse_*） → **波次调度与批量执行** → 形状与解析（parse_* 续）

中间那段（原来的 724-1443，720 行）自成一体：`Shape`（动作形状）→ `conflicts`/`waves_by`
（谁能同波并发）→ `read_group`（并行读分组）→ `resolve_write`/`flush_write_disk`（写回落地）
→ `JoinAll`/`batch_json_result`（并发合并与结果拼装）→ `exec_one`/`run_wave`（真正跑一波）。
它就是"**动作已经有了，怎么并发地把它跑掉**"，与"模型输出怎么变成动作"不是一件事。

接线照 `agent/` 里的老规矩：子模块 + `use super::*;` + `agent.rs` 里 `pub use dispatch::*;`
⇒ 对外路径 `agent::Shape` / `agent::run_wave` 一字不变，`tests.rs` 与 `tool_loop.rs` 不用改。

判据：顶层项名字集合守恒（action.rs 拆前 == action.rs 拆后 ∪ dispatch.rs）+ 逐项命中数断言。
"""
import io
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
AGENTDIR = os.path.join(ROOT, "crates", "harness-engine", "src", "agent")
SRC = os.path.join(AGENTDIR, "action.rs")
DST = os.path.join(AGENTDIR, "dispatch.rs")
AGENT = os.path.join(ROOT, "crates", "harness-engine", "src", "agent.rs")

DOC = """//! 波次调度与批量执行：**动作已经有了，怎么并发地把它跑掉**。
//!
//! 一趟模型回复可以带多个动作（`parse_actions` 的批量形态）。这里决定它们怎么落地：
//!
//! - **形状**（[`Shape`]）：一个动作"碰"了哪些资源（文件 / 命令 / 目录），据此判 [`conflicts`]；
//! - **波次**（[`waves_by`] / [`batch_waves`]）：同波之间无冲突 ⇒ 并发；有冲突 ⇒ 排到下一波。
//!   串行化是**保守**的选择：模型写同一个文件的两次动作之间一定有因果，并发跑就是掷骰子；
//! - **执行**（[`exec_one`] / [`run_wave`]）：一波一个 `JoinAll`（`futures` 风格的并发合并），
//!   结果按原顺序拼成一条 [`batch_json_result`] 观察 —— 顺序不能乱，模型靠下标对齐它自己的动作；
//! - **写回落地**（[`resolve_write`] / [`flush_write_disk`]）：Stage 模式只进覆盖层 + 暂存区，
//!   Apply 模式落盘（被覆盖的先备份）。
//!
//! 从 `agent/action.rs` 拆出的（2026-09-28，那一半当时 1840 行）：它被夹在解析层的两段之间
//! —— 上面是"模型输出 → 动作"，下面是"参数形状校验与纠偏文案"，中间这一层只关心**调度与执行**。
"""

PRIV_RE = re.compile(r"^(async\s+)?(fn|struct|enum|const|static|type|trait)\s+([A-Za-z_]\w*)")
ITEM_RE = re.compile(
    r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(fn|struct|enum|const|static|type|trait)\s+([A-Za-z_]\w*)"
)


def rd(p):
    return io.open(p, encoding="utf-8", newline="").read()


def wr(p, t):
    io.open(p, "w", encoding="utf-8", newline="").write(t)


def items(text):
    out = set()
    for line in text.splitlines():
        m = ITEM_RE.match(line)
        if m:
            out.add((m.group(1), m.group(2)))
    return out


def main():
    t = rd(SRC)
    nl = "\r\n" if "\r\n" in t else "\n"
    lines = t.split(nl)

    if os.path.exists(DST):
        print("这活已经做完了（%s 已在）" % os.path.basename(DST))
        return 0

    before = items(t)

    # 起锚点：`struct Shape`（连同它上面的文档注释）
    hit = [i for i, l in enumerate(lines) if re.match(r"^pub\(crate\) struct Shape\b", l)]
    assert len(hit) == 1, "!! 起锚点命中 %d" % len(hit)
    s = hit[0]
    while s > 0 and (lines[s - 1].startswith("///") or lines[s - 1].startswith("//")
                     or lines[s - 1].startswith("#[")):
        s -= 1

    # 止锚点：`run_wave`（其后第一个行首 `}`）
    hit = [i for i, l in enumerate(lines) if re.match(r"^pub\(crate\) async fn run_wave\(", l)]
    assert len(hit) == 1, "!! 止锚点命中 %d" % len(hit)
    e = hit[0]
    while lines[e] != "}":
        e += 1
        assert e < len(lines), "!! run_wave 后找不到行首 }"
    assert lines[e + 1].strip() == "", "!! run_wave 之后不是空行：%r" % lines[e + 1]

    body = lines[s:e + 1]
    # 可见性：块内私有项 → pub(crate)（幂等：已是 pub(crate)/pub 的不匹配）
    bumped = 0
    state = None
    for i, l in enumerate(body):
        if PRIV_RE.match(l):
            body[i] = "pub(crate) " + l
            bumped += 1
        if re.match(r"^(?:pub(?:\([^)]*\))?\s+)?struct\s+\w+", l):
            state = "struct"
        elif re.match(r"^(?:pub(?:\([^)]*\))?\s+)?enum\s+\w+", l):
            state = "enum"
        elif re.match(r"^impl\b", l):
            state = "impl_trait" if " for " in l else "impl"
        elif l.startswith("}"):
            state = None
        elif state == "struct" and re.match(r"^    [a-z_]\w*\s*:", l):
            body[i] = "    pub(crate) " + l.strip()
            bumped += 1
        elif state == "impl" and re.match(r"^    (?:async )?fn\s+\w+", l):
            body[i] = "    pub(crate) " + l.strip()
            bumped += 1

    wr(DST, DOC + "\nuse super::*;\n\n" + nl.join(body) + nl)

    # 从 action.rs 删掉这一段（连同其后那个空行）
    out = nl.join(lines[:s] + lines[e + 2:])
    if not out.endswith(nl):
        out += nl
    wr(SRC, out)

    # agent.rs 里登记子模块（`pub(crate) use`：dispatch.rs 里的项全是 pub(crate)，
    # 用 `pub use` 会被 rustc 提醒"glob 没有导出任何 pub 可见性的东西"）
    a = rd(AGENT)
    assert "mod dispatch;" not in a, "!! agent.rs 里已有 mod dispatch;"
    anchor = "mod gate;".replace("\n", nl) if False else "mod gate;"
    assert a.count("mod gate;") == 1
    a = a.replace("mod gate;", "mod dispatch;" + nl + "pub(crate) use dispatch::*;" + nl + "mod gate;", 1)
    wr(AGENT, a)

    after = items(rd(SRC)) | items(rd(DST))
    lost, gained = before - after, after - before
    print("action.rs %d 行 → %d 行；dispatch.rs %d 行；提升可见性 %d 项；顶层项 丢 %d 多 %d"
          % (len(t.splitlines()), len(rd(SRC).splitlines()), len(rd(DST).splitlines()),
             bumped, len(lost), len(gained)))
    if lost:
        print("!! 丢掉的顶层项：", sorted(lost))
    if gained:
        print(".. 新增的顶层项：", sorted(gained))
    return 1 if lost else 0


if __name__ == "__main__":
    sys.exit(main())
