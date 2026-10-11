"""报告与统计：LintReport、规则严重级索引、基线、增量计算。

从 engine.py 拆出来是为了满足 linter 自己的 HX101（单文件行数上限）——
"用规则约束自己"的要求。拆分的边界是：**报告只负责"装与算"，
引擎只负责"跑"**，两者不互相调用内部状态。
"""

from __future__ import annotations

import traceback

from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence

from .diagnostic import Diagnostic, Span


# ---------------------------------------------------------------- 报告


@dataclass
class LintReport:
    root: str
    files_scanned: int = 0
    files_skipped: int = 0
    diagnostics: List[Diagnostic] = field(default_factory=list)
    suppressions: List[dict] = field(default_factory=list)
    test_count: int = 0
    baseline: Optional[dict] = None
    new_by_rule: Dict[str, int] = field(default_factory=dict)
    conventions_dir: str = ""

    # ---- 统计 ----
    # 这三个和 Rust 侧 LintReport 的同名方法一一对应，避免两侧口径漂移
    def total(self) -> int:
        return len(self.diagnostics)

    def errors(self) -> int:
        return self.by_level().get("error", 0)

    def warnings(self) -> int:
        return self.by_level().get("warning", 0)

    def by_rule(self) -> Dict[str, int]:
        out: Dict[str, int] = {}
        for d in self.diagnostics:
            out[d.rule] = out.get(d.rule, 0) + 1
        return dict(sorted(out.items()))

    def by_level(self) -> Dict[str, int]:
        out: Dict[str, int] = {"error": 0, "warning": 0, "note": 0, "help": 0}
        for d in self.diagnostics:
            out[d.level] = out.get(d.level, 0) + 1
        return out

    @property
    def new_errors(self) -> int:
        return sum(
            n
            for rule, n in self.new_by_rule.items()
            if rule_level(rule) == "error"
        )

    @property
    def new_warnings(self) -> int:
        return sum(
            n for rule, n in self.new_by_rule.items() if rule_level(rule) == "warning"
        )

    @property
    def suppressed_total(self) -> int:
        return sum(1 for s in self.suppressions if s.get("used"))

    def is_ok(self, strict: bool = False) -> bool:
        if self.new_errors > 0:
            return False
        if strict and self.new_warnings > 0:
            return False
        return True

    def to_json(self, strict: bool = False) -> dict:
        return {
            "root": self.root,
            "files_scanned": self.files_scanned,
            "files_skipped": self.files_skipped,
            "test_count": self.test_count,
            "counts": self.by_level(),
            "by_rule": self.by_rule(),
            "new_by_rule": self.new_by_rule,
            "new_errors": self.new_errors,
            "new_warnings": self.new_warnings,
            "suppressions": {
                "total": len(self.suppressions),
                "used": self.suppressed_total,
                "without_reason": sum(1 for s in self.suppressions if not s.get("reason")),
                "items": self.suppressions,
            },
            "baseline": self.baseline,
            # 诊断本体：形状对齐 rustc，便于其它工具消费
            "diagnostics": [d.to_json() for d in self.diagnostics],
            "ok": self.is_ok(strict),
        }


_RULE_LEVELS: Dict[str, str] = {}


def rule_level(rule_id: str) -> str:
    return _RULE_LEVELS.get(rule_id, "warning")


def known_rule_codes() -> Dict[str, str]:
    """已注册规则的 ID → 严重级。豁免校验（HX903）用它判断"规则是否存在"。"""
    return dict(_RULE_LEVELS)


def register_rule_levels(rules: Sequence[object]) -> None:
    for r in rules:
        rid = getattr(r, "id", "")
        if rid:
            _RULE_LEVELS[rid] = getattr(r, "level", "warning")


def internal_error(rule_id: str, where: str, tb: str) -> Diagnostic:
    tail = "\n".join(tb.strip().splitlines()[-4:])
    return (
        Diagnostic(
            rule="HX998",
            level="error",
            message=f"规则 {rule_id} 执行失败（这条不是代码问题，是 linter 自身的问题）",
            span=Span(file=where, line_start=1, text=[]),
            doc="docs/conventions/lint-internals.md",
            fix_blocked_reason="需要修 linter 规则本身，不能靠改被检查的代码绕过",
        )
        .with_why("规则运行时抛异常，被引擎捕获并显式上报（静默吞掉会造成假绿）")
        .with_fix("把下面这段 traceback 交给 linter 维护者，并按需给该规则加反例测试")
        .with_child("note", tail)
    )


def _compute_new(report: LintReport, baseline: Optional[dict]) -> None:
    base_counts: Dict[str, int] = {}
    if baseline:
        base_counts = dict(baseline.get("by_rule", {}))
    report.new_by_rule = {
        rule: n - base_counts.get(rule, 0)
        for rule, n in report.by_rule().items()
        if n - base_counts.get(rule, 0) > 0
    }

    # HX902 测试数量不得减少（防"删测试换通过"）
    if baseline and "test_count" in baseline:
        prev = int(baseline["test_count"])
        if report.test_count < prev:
            report.diagnostics.append(
                Diagnostic(
                    rule="HX902",
                    level="error",
                    message=f"测试用例数从 {prev} 降到 {report.test_count}",
                    span=Span(file="<project>", line_start=1, text=[]),
                    doc="docs/conventions/testing.md",
                    fix_blocked_reason="要恢复测试，不是改统计口径",
                )
                .with_why(
                    "让检查通过的最省事手段就是删掉测试；把测试数纳入基线对比可以堵住这条路。"
                )
                .with_fix(
                    "把被删/被改名的测试补回来；如果是合理的测试重构（合并用例），"
                    "需要人工确认后重写基线（--write-baseline）"
                )
            )
    report.new_by_rule = {
        rule: n - base_counts.get(rule, 0)
        for rule, n in report.by_rule().items()
        if n - base_counts.get(rule, 0) > 0
    }


def make_baseline(report: LintReport) -> dict:
    return {
        "by_rule": report.by_rule(),
        "test_count": report.test_count,
        "suppressions": report.suppressed_total,
    }
