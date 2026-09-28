#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""agent.rs 1 拆 N：3975 行的引擎 agent 入口按域拆成 6 个子模块。

先例（照抄，不发明）：`agent/tool_loop.rs` 当年就是这么拆出去的 ——
**子模块 + `use super::*;` + 父模块 `pub use` 再导一次**，对外路径 `agent::X` 一字不变。
`agent/tests.rs`（4116 行）与 `step_agent.rs` 因此一行都不用改。

拆法（按 banner 段，段就是域）：

| 新模块 | 内容 | 源区间 |
|---|---|---|
| `types.rs` | 常量表 + HistoryMsg / WritePolicy / FileChange / StepTrace / Probe / AgentOutcome | 46-157 |
| `gate.rs` | 机械验证结论（CheckItem / VerifyOutcome）+ 门禁（GateState / narrow / full / 复核） | 158-327 + 3640-3925 |
| `connect.rs` | Connect 契约（ENV_CONNECTOR_KIND / Connector / NoConnector / connect_note） | 328-435 |
| `prompt.rs` | 系统提示词与提示注入（AGENT_SYSTEM / batch_hint / ask_hint / 写入策略注释…） | 436-630 |
| `action.rs` | 动作解析（ReadSpec/WriteSpec/Action + parse_* + 波次调度 Shape/batch） | 631-2454 |
| `tools.rs` | 工具执行（Ctx + execute 预检 + read/write/execute/proc/connect + 历史折叠 + 计划收尾） | 2455-3639 |

留在 `agent.rs` 的只有**门面与主循环**：`run()` + 模块声明 + 再导出。

两个必须点名的既有瑕疵，这次一起修：

1. 「Connect 原语」banner 底下其实塞着 **整套提示词**（AGENT_SYSTEM / agent_system_prompt /
   policy_system_note / staged_execute_note / batch_hint / ask_hint / ask_answer_note /
   ask_failed_note）—— banner 名不副实，所以提示词单独成 `prompt.rs`，不再寄居在 Connect 段里。
2. 段横幅随段走（横幅不留在 agent.rs 里当孤儿路标），新模块用 `//!` 头接管同样的说明。

机制（每条都有断言，不靠"看着对"）：
- 起锚点 = banner 三元组（上横线/标签/下横线）或某个顶层项（含其 `///`/`#[…]` 前缀）；
- 止锚点 = 该段最后一个顶层项，取其后**行首** `}`（中间的 `}` 都带缩进）；
- 可见性：搬走的**私有**顶层项一律提到 `pub(crate)`（跨子模块可见 + 能被再导出；
  只在一处用的多一份 `pub(crate)` 无害），已有的 `pub`/`pub(crate)` 不动；
- 守恒 = 顶层项**名字集合**改动前后一模一样（agent.rs ∪ 新模块 ∪ 既有子模块）；
- 幂等 = 已经拆过就直接说"这活做完了"。
"""
import io
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)  # D:\Projects\Rust\ruyix
SRCDIR = os.path.join(ROOT, "crates", "harness-engine", "src")
AGENT = os.path.join(SRCDIR, "agent.rs")
AGENTDIR = os.path.join(SRCDIR, "agent")

# ---------------------------------------------------------------- 模块表
# (file, doc, [(banner_label | None, start_item_pat | None, end_item_pat), ...])
MODS = [
    (
        "types.rs",
        """//! 引擎 Agent 的**数据形状**：常量上限 + 进出工具循环的结构体。
//!
//! 这些类型是 `agent` 的词汇表：`HistoryMsg` 是上一轮对话（`run` 收它、`AgentOutcome` 回它），
//! `WritePolicy` 决定写落在暂存区还是磁盘，`FileChange` 是改动台账（窄验证 / 回写 / 备份都认它），
//! `StepTrace` / `Probe` 是给 UI 与发现的观测形状。
//!
//! 从 `agent.rs` 拆出的（2026-09-28，文件当时 3975 行）；对外路径不变（`agent::WritePolicy` 等
//! 仍由父模块 `pub use` 再导一次）。
""",
        [(None, r"^pub const MAX_STEPS: usize = 96;", r"^pub struct AgentOutcome \{")],
    ),
    (
        "connect.rs",
        """//! Connect 原语：**引擎定契约，宿主接外部系统**。
//!
//! 引擎（零 tauri）只定义形状与纪律 —— `Connector::list/call` 两个方法 + 四个结构体；
//! 连什么是宿主环境的事（子进程、HTTP、三 scope 配置）。宿主没接任何外部能力时用
//! [`NoConnector`]（清单为空 ⇒ 提示词里不出现连接段）。
//!
//! ⚠️ 这段 banner 以前叫「Connect 原语」，但底下还塞着**整套提示词**（`AGENT_SYSTEM` /
//! `batch_hint` / `ask_hint` …）。2026-09-28 拆 `agent.rs` 时把提示词分出去成了 `prompt.rs`
//! —— 名字跟着内容走，一处 banner 不再覆盖两个域。
""",
        [("Connect 原语：引擎定契约，宿主接外部系统", None, r"^fn connect_note\(")],
    ),
    (
        "prompt.rs",
        """//! 系统提示词与提示注入：模型这一轮**看到什么**。
//!
//! 三块：
//! - 根（`AGENT_SYSTEM` / `agent_system_prompt`）—— 不可变，平台差异（Windows 检索提示词）
//!   靠整段 `#[cfg]` 套住，基础常量保持逐字节不变（测试钉着它）；
//! - 每轮注入（`policy_system_note` / `staged_execute_note` / `batch_hint` / `ask_hint`）——
//!   **必须说出来的事实**：写落到哪、`execute` 会不会看到旧文件、这轮允许几个动作、能不能提问。
//!   这些不是锦上添花：不说，模型就会拿 `execute` 去验证一个"暂存区里的改动"（真发生过，见 CLAUDE.md）；
//! - 观察文案（`ask_answer_note` / `ask_failed_note`）—— 没拿到答案时 fail-closed 的措辞。
//!
//! 从 `agent.rs` 拆出的（2026-09-28）。原先寄居在「Connect 原语」banner 底下，名不副实。
""",
        [(None, r"^pub const AGENT_SYSTEM: &str = r#\"", r"^fn ask_failed_note\(")],
    ),
    (
        "gate.rs",
        """//! 门禁：**机械验证 + 复核**（v0.3）。
//!
//! 两层，事实驱动（不做任务分类）：
//! - 窄层：写动作触发的单文件语法检查（跑在临时目录里的改动内容上 —— 确认模式下"磁盘没动"这个
//!   不变式不能破）；
//! - 全层：交付（`final`）触发的 `verify::run` + lint（只在 Apply 模式；Stage 模式如实报 `skipped`）。
//!
//! 全层不过就**拦住 final** 并把报告回灌（最多 `gate.max_full_attempts` 次，之后带着诚实的
//! "未通过"脚注放行）；放行前还有**干净上下文的反思 agent**（`engine::reflect`）复核产物。
//! 这两件事的对外形状就是本模块开头的 `CheckItem` / `VerifyOutcome`（原先单独占一段 banner，
//! 与门禁本体的代码相隔 3300 行 —— 2026-09-28 拆 `agent.rs` 时合成一个文件）。
""",
        [
            ("机械验证结论（v0.3 门禁的对外形状）", None, r"^impl VerifyOutcome \{"),
            ("门禁：机械验证 + 反思（v0.3）", None, r"^async fn gate_before_final\("),
        ],
    ),
    (
        "action.rs",
        """//! 动作解析：**模型输出 → 结构化动作**（以及波次调度）。
//!
//! 这是引擎与模型之间唯一的协议层：八个工具名 → `Action`，参数形状校验（缺字段 / 类型不对都
//! 在这里被翻译成**可读的纠偏**回灌，而不是让循环崩掉），批量的冲突分析与波次切分（同波次并发，
//! 冲突的串行）。解析不出来的那条路径同样在这里：截断（`finish_reason=length`）与格式错
//! 纠偏方向相反，`parse_failure_feedback` 分开说。
//!
//! 从 `agent.rs` 拆出的（2026-09-28，这一段原本 1824 行、是全文件最大的一坨）。
""",
        [("动作解析（模型输出 → 结构化动作）", None, r"^pub\(crate\) fn parse_failure_feedback\(")],
    ),
    (
        "tools.rs",
        """//! 工具执行：**项目目录 + 覆盖层**（路径全部封闭在项目内）。
//!
//! [`Ctx`] 是这条链的共享状态：覆盖层（本会话写过什么，Stage 模式下磁盘没动也要视图一致）、
//! 改动台账、找文件缓存、账本（去重）与上下文布局。步骤执行体（`crate::step_agent`）借的是
//! **同一份** `&mut Ctx` —— 不借同一份就得写 merge 与冲突处理，而收益只有"父能看子"。
//!
//! 四类动作的落地：read（窗口读 / 行内扫描）、write（整份或锚点替换，Stage 模式进暂存区）、
//! execute（前台 / 后台 / 句柄三种生命周期 + 执行前预检）、connect（交给宿主的 [`Connector`]）。
//! 另外还有循环需要的零件：历史折叠、停滞检测文案、计划收尾（`settle_steps`）。
//!
//! 从 `agent.rs` 拆出的（2026-09-28）。
""",
        [("工具执行（项目目录 + 覆盖层；路径全部封闭在项目内）", None, r"^fn settle_steps\(")],
    ),
]

DECLS = """
// ---------------------------------------------------------------------------
// 子模块：`agent.rs` 到此为止只是**门面 + 主循环**，实现按域住在下面这些文件里
// （2026-09-28 拆的；对外路径 `agent::X` 靠下面的再导出保持不变）。
// ---------------------------------------------------------------------------
mod action;
pub use action::*;
mod connect;
pub use connect::*;
mod gate;
pub use gate::*;
mod prompt;
pub use prompt::*;
mod tools;
pub use tools::*;
mod types;
pub use types::*;
"""

ITEM_RE = re.compile(
    r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(fn|struct|enum|const|static|type|trait)\s+([A-Za-z_]\w*)"
)
PRIV_RE = re.compile(r"^(async\s+)?(fn|struct|enum|const|static|type|trait)\s+([A-Za-z_]\w*)")


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
    t = rd(AGENT)
    nl = "\r\n" if "\r\n" in t else "\n"
    lines = t.split(nl)

    if "mod action;\n" .replace("\n", nl) in t:
        print("这活已经做完了（agent.rs 里已有子模块声明）")
        return 0

    before = items(t)
    # `mod x;` 声明不计入守恒（拆分本来就会新增）
    regions = []  # (mod_idx, s, e)
    for mi, (fname, doc, regs) in enumerate(MODS):
        for banner, start_pat, end_pat in regs:
            if banner is not None:
                bidx = [i for i, l in enumerate(lines) if l.strip() == "// " + banner]
                assert len(bidx) == 1, "!! banner %r 命中 %d" % (banner, len(bidx))
                b = bidx[0]
                assert re.match(r"^// =+$", lines[b - 1]) and lines[b + 1] == lines[b - 1], \
                    "!! banner %r 不是三元组：%r" % (banner, lines[b - 1:b + 2])
                s = b - 1
            else:
                hit = [i for i, l in enumerate(lines) if re.match(start_pat, l)]
                assert len(hit) == 1, "!! 起锚点 %r 命中 %d" % (start_pat, len(hit))
                s = hit[0]
                while s > 0 and (lines[s - 1].startswith("///") or lines[s - 1].startswith("//")
                                 or lines[s - 1].startswith("#[")):
                    s -= 1
            hit = [i for i, l in enumerate(lines) if re.match(end_pat, l)]
            assert len(hit) == 1, "!! 止锚点 %r 命中 %d" % (end_pat, len(hit))
            e = hit[0]
            while lines[e] != "}":
                e += 1
                assert e < len(lines), "!! 止锚点 %r 后找不到行首 }" % (end_pat,)
            regions.append([mi, s, e, fname, doc, banner])

    # 段不许重叠、必须升序
    regions.sort(key=lambda r: r[1])
    for a, b in zip(regions, regions[1:]):
        assert a[2] < b[1], "!! 两段重叠：%s %d-%d 与 %s %d-%d" % (a[3], a[1], a[2], b[3], b[1], b[2])

    # ---- 生成新模块 ----
    per_mod = {}
    for mi, s, e, fname, doc, banner in regions:
        body = lines[s:e + 1]
        # 可见性：私有顶层项 → pub(crate)；**结构体字段**与**固有 impl 的方法**同样要提
        # （E0616 字段私有 / E0624 方法私有 —— 真踩过；trait impl 的方法不许带可见性限定符，跳过）
        bumped = 0
        state = None
        for i, l in enumerate(body):
            if PRIV_RE.match(l):
                body[i] = "pub(crate) " + l
                bumped += 1
                # 注意：**不能** continue —— 这一行可能同时是 `struct X {` / `impl X {`，
                # 状态机还得看见它（第一版在这里 continue，于是 GateState / RoundSlot 的
                # 字段一个都没提，编译报 21 条 E0616）
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
        per_mod.setdefault(fname, dict(doc=doc, chunks=[]))
        per_mod[fname]["chunks"].append((s, e, body, bumped, banner))

    for fname, info in per_mod.items():
        chunks = []
        for s, e, body, bumped, banner in info["chunks"]:
            note = []
            if banner:
                note = ["// （原 `agent.rs` 的 banner：%s）" % banner, ""]
            chunks.append(note + body)
        text = info["doc"] + "\nuse super::*;\n\n" + "\n\n".join(
            "\n".join(c) for c in chunks
        ) + "\n"
        wr(os.path.join(AGENTDIR, fname), text)
        print("  + agent/%-10s %4d 行（%d 段，提升可见性 %d 项）"
              % (fname, len(text.splitlines()), len(info["chunks"]),
                 sum(c[3] for c in info["chunks"])))

    # ---- 从 agent.rs 删段（降序，避免下标漂移）----
    for mi, s, e, fname, doc, banner in sorted(regions, key=lambda r: -r[1]):
        del lines[s:e + 1]

    # ---- 插入模块声明（放在既有子模块声明区之前）----
    anchor = [i for i, l in enumerate(lines) if l.strip() == "pub use findings::Progress;"]
    assert len(anchor) == 1, "!! 找不到模块声明区锚点"
    out = nl.join(lines)
    out = out.replace("pub use findings::Progress;",
                      DECLS.strip().replace("\n", nl) + nl + nl + "pub use findings::Progress;", 1)
    # 收敛多余空行（rustfmt 也会，但先自己收一遍更干净）
    out = re.sub(r"(?:\r?\n){3,}", nl + nl, out)
    wr(AGENT, out)

    after = items(rd(AGENT))
    for fname in per_mod:
        after |= items(rd(os.path.join(AGENTDIR, fname)))
    lost, gained = before - after, after - before
    print("agent.rs → %d 行；%d 个新模块；顶层项 %d 个（丢 %d / 多 %d）"
          % (len(rd(AGENT).splitlines()), len(per_mod), len(after), len(lost), len(gained)))
    if lost:
        print("!! 丢掉的顶层项：", sorted(lost))
    if gained:
        print(".. 新增的顶层项：", sorted(gained))
    return 1 if lost else 0


if __name__ == "__main__":
    sys.exit(main())
