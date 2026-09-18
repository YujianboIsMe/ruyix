"""豁免（noqa / disable-file）：解析、生效、记账、反规避元规则。

单独成模块的原因：这块是"防止 Agent 让检查闭嘴"的核心闸门，混在引擎里容易被当成
附属逻辑，而它其实是**产品级承诺**（豁免必须可复核、可统计、有预算）。
"""

from __future__ import annotations

import io
import re
import tokenize
from typing import Dict, List, Optional, Tuple

from .diagnostic import Diagnostic, Span
from .report import known_rule_codes


# ---------------------------------------------------------------- 抑制指令

# 规则 ID 的形状：至少两个字母 + 结尾数字（HX201 / BLE001 / F401）。用它把
# "规则 ID 列表" 和 "后面的散文说明" 分开。
# 规则 ID 的真实形状：1~4 个字母 + 1~4 位数字。
# 早先写成「至少两个字母」把 F401 / E402 / N802 排除在外，于是它们落到"无 codes"分支，
# 被当成我们自己的豁免 —— 幽灵豁免就是这么来的。
_RULE_CODE_RE = re.compile(r"^[A-Za-z]{1,4}[0-9]{1,4}$")

# 指令起始标记：`# noqa` 或 `# harnesslint:`，允许出现在注释中段
# 指令起始标记（完全不用反斜杠转义：`\s` / `\b` 在多层转义里被搞坏过）
_DIRECTIVE_RE = re.compile(r"#[ \t]*(noqa|harnesslint:)")


def parse_directive(line: str) -> Optional[dict]:
    """解析一行里的豁免指令。

    支持两种写法：
        x = 1  # noqa: HX104 reason=调试残留待清理
        # harnesslint: disable-file HX105 reason=示例文件故意留 TODO

    指令可以出现在注释的**任意位置**（对齐 flake8 的全行扫描行为）：
        # 1. 注释里的 TODO / FIXME  # noqa: HX105 reason=规则文档必须列出它检测的词
    早期只认注释开头的指令，导致上面这种写法**静默失效**——用户以为豁免了，其实没有。
    """
    # 指令必须出现在注释开头，或前面只有空白。
    # 否则 `# noqa`（反引号里引述语法）这种文档性文字会被当成真指令 ——
    # 它比刷假告警更糟：那行上的真诊断会被静默压掉。
    found = None
    for cand in _DIRECTIVE_RE.finditer(line):
        if cand.start() == 0 or line[cand.start() - 1] in " 	":
            found = cand
            break
    if found is None:
        return None
    marker = found.group(1)
    body = line[found.end() :].strip()
    # `noqa` 后面紧跟字母数字说明只是恰好出现在单词里（例如 "noqabcd"），不算指令
    if marker == "noqa" and body[:1].isalnum():
        return None
    if marker.startswith("harnesslint"):
        kind = "file"
        for prefix in ("disable-file", "disable"):
            if body.startswith(prefix):
                body = body[len(prefix) :].strip()
                break
        else:
            return None
    else:
        kind = "line"
    if body.startswith(":"):
        body = body[1:].strip()

    reason = None
    m = re.search(r"(?:reason[ \t]*=|because[ \t]+)(.+)$", body)
    if m:
        reason = m.group(1).strip()
        body = body[: m.start()].strip()

    # 只取**开头连续的"规则 ID 形状"token** 作为 codes，遇到第一个不像 ID 的词就停。
    # 踩过的坑：`# noqa: BLE001 —— 故意宽泛：...` 里那串散文被当成了十几个规则 ID，
    # 于是刷出十几条 HX903 假告警。
    # 按空白**和逗号**切：`# noqa: F401,E402` 是常见写法（逗号后常不带空格），
    # 只按空白切会把 'F401,E402' 整体当成一个 token 而认不出来，
    # 于是 codes 变成空、被误判成"我们自己的豁免"。
    tokens = re.split(r"[,\s]+", body)
    codes = []
    rest = len(tokens)
    for i, tok in enumerate(tokens):
        candidate = tok.rstrip(",;")
        if _RULE_CODE_RE.match(candidate):
            codes.append(candidate.upper())
        else:
            rest = i
            break

    # 尾部散文视为「理由」：豁免必须有解释，但不必强制写成 reason= 形式
    if reason is None and rest < len(tokens):
        prose = " ".join(tokens[rest:]).strip(" ,;、")
        reason = prose or None

    return {"kind": kind, "codes": set(codes), "reason": reason}


def collect_directives(fctx) -> List[dict]:
    """只从真正的注释里收集豁免指令。

    **必须走 tokenize**：文档字符串/字符串字面量里出现的 `# noqa: HX101` 只是示例文本，
    逐行扫 '#' 会把它们当成真指令 —— 既刷假告警（HX900/HX903），更糟的是
    能把真实诊断悄悄压掉。这个 bug 是拿 linter 检查它自己的源码时抓到的。
    """
    out: List[dict] = []
    try:
        for tok in tokenize.generate_tokens(io.StringIO(fctx.source).readline):
            if tok.type != tokenize.COMMENT:
                continue
            d = parse_directive(tok.string)
            if not d:
                continue
            lineno, col = tok.start
            prefix = fctx.lines[lineno - 1][:col] if lineno <= len(fctx.lines) else ""
            out.append(
                {
                    "file": fctx.rel,
                    "line": lineno,
                    "kind": d["kind"],
                    "codes": sorted(d["codes"]),
                    "reason": d["reason"],
                    "used": False,
                    "raw": tok.string.strip(),
                    # 纯注释行（前面只有空白）才允许豁免"下一行"
                    "comment_only": prefix.strip() == "",
                }
            )
    except (tokenize.TokenError, IndentationError, SyntaxError):
        # 词法失败就没有豁免信息可用（语法错误本身由 HX000 报）
        pass
    return out


def _is_our_directive(item: dict) -> bool:
    """这条豁免是不是"我们发的"？口径与熵度量层一致。

    只认 HX 前缀的，以及不带 codes 的（那种会抑制该行全部诊断）。
    """
    codes = item.get("codes") or []
    if not codes:
        return True
    return any(str(c).upper().startswith("HX") for c in codes)


def apply_suppressions(  # noqa: HX102 reason=豁免的过滤与记账必须在一个函数里完成（先过滤再统计，顺序错了计数就不对），拆开会让这个不变量跨函数
    report: LintReport,
    files_ctx: List[FileContext],
    cfg: Config,
    max_suppressions: Optional[int],
) -> None:
    """按 noqa / disable-file 过滤诊断，并给豁免记账。"""
    by_file: Dict[str, FileContext] = {f.rel: f for f in files_ctx}
    known_codes = set(known_rule_codes())

    # 1. 收集全部指令 —— 只在真正的 COMMENT token 上解析（见 collect_directives）
    line_dirs: Dict[Tuple[str, int], List[dict]] = {}
    file_dirs: Dict[str, List[dict]] = {}
    # 只有"整行都是注释"的行才允许豁免下一行——否则会变成"随便找个地方写 noqa 就能关检查"
    comment_only: set = set()
    for fctx in files_ctx:
        for item in collect_directives(fctx):
            key = (fctx.rel, item["line"])
            if item["kind"] == "line":
                line_dirs.setdefault(key, []).append(item)
                if item["comment_only"]:
                    comment_only.add(key)
            else:
                file_dirs.setdefault(fctx.rel, []).append(item)

    # 2. 过滤
    kept: List[Diagnostic] = []
    for d in report.diagnostics:
        suppressed_by = None
        if d.span:
            # ① 同一行上的 noqa（最常见）
            for item in line_dirs.get((d.file, d.line), []):
                if not item["codes"] or d.rule in item["codes"]:
                    suppressed_by = item
                    break
            # ② 紧邻的上一行，且那一行是纯注释行（写成独立注释的形式）
            if suppressed_by is None and (d.file, d.line - 1) in comment_only:
                for item in line_dirs.get((d.file, d.line - 1), []):
                    if not item["codes"] or d.rule in item["codes"]:
                        suppressed_by = item
                        break
            if suppressed_by is None:
                for item in file_dirs.get(d.file, []):
                    if not item["codes"] or d.rule in item["codes"]:
                        suppressed_by = item
                        break
        if suppressed_by is not None and d.rule != "HX000":
            suppressed_by["used"] = True
            continue
        kept.append(d)
    report.diagnostics = kept

    # 3. 记账 + 反规避元诊断
    all_items = [it for v in line_dirs.values() for it in v] + [it for v in file_dirs.values() for it in v]
    all_items.sort(key=lambda x: (x["file"], x["line"]))
    report.suppressions = all_items

    for item in all_items:
        loc = Span(file=item["file"], line_start=item["line"], text=[item["raw"]])
        # `# noqa` 是全生态共享命名空间：`F401`/`E402`/`BLE001` 是别的工具的规则，
        # 那些豁免不归我们管（metric 侧也是这么划的）。要求它们写理由只会制造噪声，
        # 而噪声会让整条反馈链被忽略。
        ours = _is_our_directive(item)
        # HX900 豁免必须说明理由
        if cfg.require_noqa_reason and ours and not item["reason"]:
            report.diagnostics.append(
                Diagnostic(
                    rule="HX900",
                    level="warning" if item["used"] else "note",
                    message="豁免没有说明理由",
                    span=loc,
                    doc="docs/conventions/suppressions.md",
                    fix_blocked_reason="理由只能由人给出，机器猜不出",
                )
                .with_why(
                    "没有理由的 `# noqa` 无法被复核，也无法在规则升级后被重新评估；"
                    "Agent 面对 lint 最常见的糊弄手段就是加裸 noqa——这条规则就是给它记账。"
                )
                .with_fix(
                    "补上理由，写成 `# noqa: HX201 reason=<为什么这里可以豁免>`；"
                    "如果给不出理由，说明应该改代码而不是豁免"
                )
            )
        # HX903 豁免指向不存在的规则。
        # 只检查 HX 前缀：`# noqa` 是全生态共享的命名空间，BLE001 / F401 之类属于别的
        # 工具，我们无权判定它们"不存在"——否则一个项目同时用 ruff 就会满屏假告警。
        for code in item["codes"]:
            if code.startswith("HX") and code not in known_codes:
                report.diagnostics.append(
                    Diagnostic(
                        rule="HX903",
                        level="error",
                        message=f"豁免指向了不存在的规则 ID：{code}",
                        span=loc,
                        doc="docs/conventions/suppressions.md",
                        fix_blocked_reason="需要确认正确规则 ID 后改配置",
                    )
                    .with_why(
                        "规则 ID 拼错等于偷偷关掉检查（你以为豁免了 X，其实什么都没豁免）；"
                        "也可能是规则改名后旧的豁免残留成了死配置。"
                    )
                    .with_fix(
                        f"确认要豁免的规则 ID 后改正；现有规则：{', '.join(sorted(known_codes))}"
                    )
                )

    # HX901 豁免预算
    budget = max_suppressions if max_suppressions is not None else cfg.max_suppressions
    if budget is not None and budget >= 0 and report.suppressed_total > budget:
        report.diagnostics.append(
            Diagnostic(
                rule="HX901",
                level="error",
                message=f"生效的豁免数 {report.suppressed_total} 超过预算 {budget}",
                span=Span(file=all_items[0]["file"] if all_items else "-", line_start=1, text=[]),
                doc="docs/conventions/suppressions.md",
                fix_blocked_reason="需要改代码消除违规，而不是追加豁免",
            )
            .with_why(
                "豁免预算是反规避的核心闸门：允许无限豁免，'自我纠正循环'就会收敛到"
                "让检查闭嘴，而不是把代码改对。"
            )
            .with_fix(
                "按 违反了什么 → 为什么 → 怎么改 修掉对应违规；确实必须保留的豁免要在"
                "提交说明里写清理由并调整预算"
            )
        )
