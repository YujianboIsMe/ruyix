"""渲染：同一份诊断出两种形态。

- **人读**（默认）：带源码上下文的块状输出，四段式（违反了什么 / 为什么 / 怎么改 / 参考）。
  这是给"人"和"Agent"看的主形态 —— 设计目标是**不查任何其它文档就能动手改**。
- **机读**（`--json`）：整个报告一份 JSON，诊断字段对齐 rustc 的 JSON 诊断形状。

两种形态共用同一份 `Diagnostic` 对象，所以不可能出现"报告说 A、JSON 说 B"。
"""

from __future__ import annotations

import os
from typing import List, Optional

from .diagnostic import LEVEL_ORDER, Diagnostic
from .engine import LintReport

_LEVEL_TAG = {
    "error": "error",
    "warning": "warn",
    "note": "note",
    "help": "help",
}


def resolve_doc(doc: Optional[str], conventions_dir: str) -> Optional[str]:
    """把逻辑文档路径解析成真实文件路径；解析不到返回 None（不假装文档存在）。"""
    if not doc or not conventions_dir:
        return None
    name = doc
    prefix = "docs/conventions/"
    if name.startswith(prefix):
        name = name[len(prefix) :]
    path = os.path.join(conventions_dir, name)
    return path if os.path.isfile(path) else None


def render_diagnostic(d: Diagnostic, conventions_dir: str = "", color: bool = False) -> str:
    span = d.span
    where = f"{d.file}:{d.line}" if span else "-"
    if span and span.column_start:
        where += f":{span.column_start}"

    head = f"{where}  {d.rule}  {_LEVEL_TAG.get(d.level, d.level)}  {d.message}"
    lines: List[str] = [head]

    if span and span.text:
        width = len(str(max(span.line_start, span.text_start + len(span.text) - 1)))
        lines.append("  " + " " * width + " ╭")
        for i, text in enumerate(span.text):
            no = span.text_start + i  # 由 Span 自己记录，避免 line_start==1 时错位
            mark = "│"
            lines.append(f"  {str(no).rjust(width)} {mark} {text}")
            if no == span.line_start and span.column_end and span.column_end > span.column_start:
                caret = " " * max(0, span.column_start - 1) + "^" * max(
                    1, min(span.column_end, len(text) + 1) - span.column_start
                )
                label = f" {span.label}" if span.label else ""
                lines.append(f"  {' ' * width} {mark} {caret}{label}")
        lines.append("  " + " " * width + " ╰")

    body: List[str] = []
    for c in d.children:
        tag = "为什么" if c.level == "note" else ("怎么改" if c.level == "help" else c.level)
        body.append((tag, c.message))
    if d.doc:
        resolved = resolve_doc(d.doc, conventions_dir)
        body.append(("参考", f"{d.doc}" + (f"（{resolved}）" if resolved else "（文档缺失，待补）")))
    if d.fixable:
        body.append(("自动修复", "可用（有 MachineApplicable 建议）"))
    elif d.fix_blocked_reason:
        body.append(("自动修复", f"不可用 —— {d.fix_blocked_reason}"))

    for tag, msg in body:
        parts = str(msg).split("\n")
        lines.append(f"  {tag}：{parts[0]}")
        for extra in parts[1:]:
            lines.append(f"      {extra}")

    if span and span.suggested_replacement:
        # 只渲染**主行**，并把替换落在准确列区间上重建出行内容。
        # （之前把所有上下文行都当 "-" 打出来，看着像整块被替换成一行，误导。）
        lines.append(f"  建议（{span.suggestion_applicability}）：")
        idx = span.line_start - span.text_start
        if 0 <= idx < len(span.text):
            primary = span.text[idx]
            a = max(0, span.column_start - 1)
            b = max(a, (span.column_end or len(primary) + 1) - 1)
            fixed = primary[:a] + span.suggested_replacement + primary[b:]
            lines.append(f"      - {primary}")
            lines.append(f"      + {fixed}")
        else:
            lines.append(f"      替换为 {span.suggested_replacement}")

    lines.append(f"  豁免：单行 `# noqa: {d.rule} reason=<原因>`（见 docs/conventions/suppressions.md）")
    return "\n".join(lines)


def render_report(report: LintReport, strict: bool = False, color: bool = False) -> str:
    out: List[str] = []
    counts = report.by_level()
    out.append(
        "harness-lint  "
        f"扫描 {report.files_scanned} 个文件"
        + (f"（跳过 {report.files_skipped} 个无法解析）" if report.files_skipped else "")
        + f"  error {counts['error']}  warning {counts['warning']}"
        + (f"  测试用例 {report.test_count} 个" if report.test_count else "")
    )
    if report.suppressions:
        no_reason = sum(1 for s in report.suppressions if not s.get("reason"))
        out.append(
            f"豁免 {len(report.suppressions)} 条（生效 {report.suppressed_total} 条"
            + (f"，其中 {no_reason} 条没有理由" if no_reason else "")
            + ")"
        )
    if report.baseline:
        out.append(
            "对比基线：新增 error "
            f"{report.new_errors}  新增 warning {report.new_warnings}"
        )
    out.append("")

    ordered = sorted(
        report.diagnostics,
        key=lambda d: (LEVEL_ORDER.get(d.level, 9), d.file, d.line, d.rule),
    )
    if not ordered:
        out.append("没有发现问题。")
    for d in ordered:
        out.append(render_diagnostic(d, report.conventions_dir, color))
        out.append("")

    if report.by_rule():
        out.append("按规则汇总：")
        for rule, n in report.by_rule().items():
            out.append(f"  {rule}  {n}")
        out.append("")

    verdict = "PASS" if report.is_ok(strict) else "FAIL"
    out.append(f"结论：{verdict}" + ("（strict 模式：warning 也算失败）" if strict else ""))
    return "\n".join(out)


def report_to_json(report: LintReport, strict: bool = False) -> dict:
    """JSON 形态：在 rustc 形状之外补一个 doc_resolved，保证"参考"是真实路径。"""
    data = report.to_json(strict)
    for item in data.get("diagnostics", []):
        item["doc_resolved"] = resolve_doc(item.get("doc"), report.conventions_dir)
    data["conventions_dir"] = report.conventions_dir
    return data
