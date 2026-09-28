#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""agent/tests.rs（4116 行、109 个测试）按域拆成 hub + 子模块。

形状：`agent/tests.rs` 留下**夹具与共享 helper**（TempDir / run_loop / ScriptAsker / LogSink …）
+ 每个域的 `mod <域>;` 声明；测试本体按域搬进 `agent/tests/<域>.rs`，各自 `use super::*;`
（子模块能看到父模块的**私有项** —— 与 `agent/` 那一层的做法同源，`tool_loop.rs` 早就这么干）。

为什么这么切：测试名本身就是域（`preflight_*` / `anchor_edits_*` / `layout_*` / `dedup_*` …），
按域分文件之后"这条断言属于哪一层"一眼可见，而不是在 4000 行里按顺序找。

子模块名的一条讲究：**不许与 `agent::` 里的模块重名**（`:context` / `:ledger` 这种）——
子模块里的裸路径 `ledger::Ver` 会解析到**自己**（就近遮蔽），编译报"cannot find `Ver` in `ledger`"。
所以那里用域名词 `layout` / `dedup`，不用 `context` / `ledger`。

三条纪律：
1. **分类是显式的表**（下面的 `RULES`，按序匹配测试名子串），脚本断言**每个测试恰好命中一条**、
   总数 = 109 —— 漏一个或多个都当场退出，不靠"看着差不多"；
2. 测试在 hub 里就是**行首顶层项**，搬到子文件同样是行首顶层项 ⇒ **不需要任何缩进调整**
   （这也是为什么没有 `cargo fmt` 改坏字符串数据的风险）；
3. 夹具**全部留在 hub**（哪个域用都行），子模块靠 `use super::*` 拿；测试文件之间因此没有
   循环引用。
"""
import io
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
TDIR = os.path.join(ROOT, "crates", "harness-engine", "src", "agent")
HUB = os.path.join(TDIR, "tests.rs")
SUBDIR = os.path.join(TDIR, "tests")

# 域 → (匹配测试名的子串, 模块文档)
RULES = [
    ("parse", [
        "parse_action", "dsml_", "args_only_object", "content_only_model", "a_headless_dsml",
        "tool_calls_round", "several_tool_calls", "final_as_a_tool_call", "batch_off_refuses",
        "strict_mode_refuses", "a_copied_record", "the_real_run_shapes", "rollback_switch",
        "parse_feedback_distinguishes", "parameter_shapes_are_validated", "a_multiline_final",
        "a_batch_parses_in_declared_order", "an_oversized_batch_is_refused",
        "a_batch_is_refused_when_the_switch",
    ], """//! 动作解析：八种工具 → `Action`，以及解析不出来时的**纠偏**。
//!
//! 覆盖 `action.rs` 的协议面：工具名与参数形状、DSML 标记的剥与不剥、内容通道的关与回滚、
//! 截断 vs 格式错的两种纠偏方向、批量的声明序与控制动作拒绝。
//!
//! 夹具（`run_loop` / `ScriptAsker` / `TempDir` …）在父模块 `tests.rs` 里，这里 `use super::*` 取。
"""),
    ("prompt", [
        "system_prompt_defines", "platform_hint_matches", "confirm_mode_note_is_conditional",
        "staged_execute_note_gated", "system_prompt_documents_the_background",
        "first_user_message_carries", "discovered_command_list_disappears",
        "the_web_search_hint_states", "the_batch_hint_follows",
        "prompt_advertises_both_write_shapes", "the_prompt_teaches_keep_alive",
        "the_start_note_says_what_happens",
    ], """//! 提示词与注入：模型这一轮**看到什么**（对应 `prompt.rs`）。
//!
//! 四类断言：根提示词（四原语 / 平台检索提示词）、写入策略注释（确认模式那三条事实）、
//! 每轮注入的开关跟随（批量 / 联网 / 命令清单）、交付与托管进程的后果说明。
"""),
    ("execute", [
        "execute_denies_destructive", "preflight_", "tool_execute_refuses_garbage",
        "background_start_goes_through", "handle_ops_treat", "a_handle_lookup_must_not",
        "reap_warning_flags", "a_self_contradicting_ready", "a_missed_criterion_carries",
        "a_service_reported_as_running",
    ], """//! Execute 的三条生命周期：前台 / 后台 / 句柄（对应 `tools.rs` 的执行侧）。
//!
//! 预检的四类拒绝（破坏性模式、启动器、路径形状、没装的命令）、就绪判据的证据要求
//! （自相矛盾的判据要在**起之前**被拒）、句柄不许跨项目、run 结束的收尾对账。
"""),
    ("waves", [
        "waves_parallelize", "parallel_reads_land", "a_read_batch_costs_one_round",
        "a_write_followed_by_a_read", "two_commands_in_one_batch_really",
        "record_findings_rides_along",
    ], """//! 波次调度与并发（对应 `dispatch.rs`）。
//!
//! 无冲突 ⇒ 并发、有冲突 ⇒ 下一波；并发跑完**结果顺序仍按声明序**（模型靠下标对齐自己的动作）。
"""),
    ("read_write", [
        "tool_read_file_dir_overlay", "write_policies_stage_vs_apply", "read_window_returns",
        "anchor_edits_", "edits_shape_reaches", "read_may_reach_the_state_root",
    ], """//! Read / Write 两条原语（对应 `tools.rs`）：窗口读、覆盖层视图、路径封闭，
//! 以及写入策略（Stage 只进暂存区 / Apply 落盘）与锚点替换的原子性（含 CRLF 文件）。
"""),
    ("ask", [
        "ask_user", "ask_without_an_answer", "without_an_asker", "ask_count_is_capped",
        "ask_off_means",
    ], """//! 提问通道 `ask_user`：伪造的答案要被拒、没答到一律 fail-closed、次数有上限、
//! 关闭时引擎自己拒绝（不许指望模型自律）。
"""),
    ("gate", [
        "executed_command_output_reaches", "^gate_", "verify_outcome_never_reports",
        "lint_item_maps_errors",
    ], """//! 门禁（对应 `gate.rs`）：Stage 模式跳过全量验证**并说明原因**、验证不过拦住交付、
//! 过了就放行、预算用尽带着诚实的脚注放行；复核员要能看到命令取证。
"""),
    ("connect", [
        "connect_routes", "connect_failure_and_no_connector", "connect_note_renders",
    ], """//! Connect 原语：三种形态路由到宿主、失败与"没接任何外部能力"的措辞、
//! 连接清单进提示词的渲染（对应 `connect.rs`）。
"""),
    ("plan", [
        "plan_progress_marks", "settle_steps_", "execute_plan_",
    ], """//! 计划与步骤：大纲的 ✅/⛏️/⚠️/⏹️ 判据（`settle_steps` 必须给每个步骤一个终态、
//! 且不许在没有交付的 run 上标"完成"），以及 `step_agent` 的独立上下文与失败回交（v0.4）。
"""),
    ("history", [
        "tail_history_filters", "fold_history_keeps", "old_tool_results_are_folded",
        "history_trim_off_restores", "stall_counts_only", "superseded_findings_leave",
        "findings_over_cap_spill", "repeated_read_of_same_range",
    ], """//! 历史折叠与进展记忆（对应 `findings.rs` + 折叠逻辑）：摘要保留"结论与做过什么"、
//! 停滞只数**没有进展**的轮次、findings 超限带指针溢出、重复读同一个区间要被点出来。
"""),
    ("layout", [
        "layout_", "the_shipped_default_config_shows", "the_scheduler_decides",
        "a_budget_overflow_forces", "an_exhausted_horizon_falls_back",
    ], """//! 上下文布局与重基线调度（v1.2 P1/P4，对应 `context.rs` / `scheduler.rs`）：
//! 开关关时逐轮重建 system（对照组）、开时根段逐字节不变 + 易变尾在最后 + 公共前缀占比上升；
//! 以及"什么时候压"由目标函数决定（预算越界是**强制**压、视野未知走 ski-rental 兜底）。
"""),
    ("dedup", [
        "dedup_", "a_library_hit_is_not_progress", "capsule_",
    ], """//! 去重账本与 capsule 侧存（v1.2 P2/P3，对应 `ledger.rs` / `keys.rs` / `capsule.rs`）：
//! 纯调用重复执行会被拦下、自己写完再读必须重执行、有副作用的调用永不参与、
//! 内存上限把最老的整条挤掉后要真重跑、命中召回走磁盘且**不重跑工具**。
"""),
]

HDR = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(fn|struct|enum|const|static|type|trait|impl)\b")
FN = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_]\w*)")


def rd(p):
    return io.open(p, encoding="utf-8", newline="").read()


def wr(p, t):
    io.open(p, "w", encoding="utf-8", newline="").write(t)


def main():
    raw = rd(HUB)
    nl = "\r\n" if "\r\n" in raw else "\n"
    lines = raw.split(nl)
    if os.path.exists(SUBDIR):
        print("这活已经做完了（%s 已在）" % os.path.basename(SUBDIR))
        return 0

    # ---- 切出顶层项（属性/文档 + 项本体）----
    # 边界纪律（第一版在这里踩过：把块的终点延到"下一个块的起点前一行"，结果**偷走了下一个项的
    # 属性行** —— `#[test]` 落进别的域、测试变成普通函数，还连带 E0428/E0119/E0034 一片）：
    #   ① 每个项 = [起始（含属性/文档）, 本体结束]，**不向两侧扩**；
    #   ② 行覆盖断言：没有一行属于两个项，且第一个项之后的非空行必须全被覆盖。
    heads = [i for i, l in enumerate(lines) if HDR.match(l)]
    items = []
    for h in heads:
        # 先看它是"分号收尾的声明"还是"花括号包起来的定义"：`struct TempDir(PathBuf);`
        # 这种元组结构体没有花括号，若一律去找行首 `}` 会一路吞到**下一个项**（真踩）。
        k = h
        e = None
        while k < len(lines):
            line = lines[k]
            if line.count("{") and line.count("{") == line.count("}"):
                e = k          # 单行定义的项（`fn f() { … }`）
                break
            if line.rstrip().endswith("{"):
                m = k          # 多行定义：从这行开始找配平的行首 `}`
                while m < len(lines) and lines[m] != "}":
                    m += 1
                assert m < len(lines), "!! 第 %d 行起的项找不到行首 }" % (h + 1)
                e = m
                break
            if line.rstrip().endswith(";"):
                e = k
                break
            k += 1
        assert e is not None, "!! 第 %d 行起的项没有可识别的结尾" % (h + 1)
        s = h
        while s > 0 and (lines[s - 1].startswith("#[") or lines[s - 1].startswith("///")
                         or lines[s - 1].startswith("//")):
            s -= 1
        items.append([s, e])
    # 段横幅/独立注释块夹在项之间、不属于任何项 —— 物理上删掉会把"v0.8：第五个动作
    # ask_user（…）"、"托管进程的交付对账"这类信息一起丢掉，所以**挂给下一个项**
    # （于是它跟着那个域的文件走）。形状不一（三行 `// ====`、单行 `// ---- …`），
    # 所以按"**没被任何项覆盖的连续 `//` 注释行**"识别，不猜形状。
    covered = set()
    for s, e in items:
        covered.update(range(s, e + 1))
    runs, cur = [], None
    for i, l in enumerate(lines):
        if i not in covered and re.match(r"^//(?![!/])", l):
            cur = i if cur is None else cur
        else:
            if cur is not None:
                runs.append((cur, i - 1))
                cur = None
    if cur is not None:
        runs.append((cur, len(lines) - 1))
    for a, b in runs:
        nxt = [it for it in items if it[0] > b]
        assert nxt, "!! 第 %d-%d 行的注释块后面没有项" % (a + 1, b + 1)
        nxt[0][0] = min(nxt[0][0], a)

    mark = [0] * len(lines)
    for s, e in items:
        for i in range(s, e + 1):
            assert mark[i] == 0, "!! 第 %d 行被两个项覆盖" % (i + 1)
            mark[i] = 1
    stray = [(i + 1, l) for i, l in enumerate(lines)
             if not mark[i] and l.strip() and i > items[0][0]]
    assert not stray, "!! 有非空行没归属任何项：%r" % stray[:3]

    tests, others = {}, []
    for s, e in items:
        name = None
        for i in range(s, e + 1):
            m = FN.match(lines[i])
            if m:
                name = m.group(1)
                break
        is_test = any(lines[i].strip() == "#[test]" for i in range(s, e + 1))
        if is_test and name:
            tests[name] = (s, e)
        else:
            others.append((s, e))

    assert len(tests) == 109, "!! 测试数 %d（期望 109）" % len(tests)

    # ---- 分类（按序匹配；每个恰好命中一条）----
    assign, table = {}, {}
    for name in tests:
        hit = [mod for mod, keys, _ in RULES if any(re.search(k, name) for k in keys)]
        assert len(hit) == 1, "!! %s 命中 %s（应为 1 条）" % (name, hit)
        assign[name] = hit[0]
        table.setdefault(hit[0], []).append(name)
    for mod, keys, _doc in RULES:
        print("  %-11s %3d 个测试" % (mod, len(table.get(mod, []))))
    assert sum(len(v) for v in table.values()) == 109

    # ---- 写子模块 ----
    os.makedirs(SUBDIR, exist_ok=True)
    for mod, keys, doc in RULES:
        names = table.get(mod, [])
        if not names:
            continue
        chunks = []
        for n in sorted(names, key=lambda n: tests[n][0]):
            s, e = tests[n]
            body = lines[s:e + 1]
            while body and body[-1].strip() == "":
                body.pop()
            chunks.append(nl.join(body))
        wr(os.path.join(SUBDIR, mod + ".rs"),
           (doc + "\nuse super::*;\n\n").replace("\n", nl) + (nl + nl).join(chunks) + nl)

    # ---- 重写 hub：只留夹具 + mod 声明 ----
    keep = []
    for s, e in others:
        body = lines[s:e + 1]
        while body and body[-1].strip() == "":
            body.pop()
        keep.append(nl.join(body))
    decls = ["// 测试按域分文件（2026-09-28 拆）：夹具与共享 helper 留在本文件，",
             "// 每个域的测试在 `tests/<域>.rs`（`use super::*` 取本文件的夹具）。"]
    decls += ["mod %s;" % mod for mod, keys, _doc in RULES if table.get(mod)]
    # 文件开头（`use super::*;` 之类）不是"顶层项"，必须显式保留；夹具跟在声明之后。
    pre = lines[:items[0][0]] if items else []
    out = (nl.join(pre) + nl + nl + nl.join(decls) + nl + nl
           + (nl + nl).join(keep) + nl)
    out = re.sub(r"(?:\r?\n){3,}", nl + nl, out)
    wr(HUB, out)

    moved = sum(len(v) for v in table.values())
    print("hub %d 行；子模块 %d 个（%d 个测试搬走，未匹配 %d）"
          % (len(rd(HUB).splitlines()), len(table), moved, 109 - moved))
    # 逐字守恒：搬走的行数 + 留下的行数 == 原行数（允许空行收敛）
    return 0


if __name__ == "__main__":
    sys.exit(main())
