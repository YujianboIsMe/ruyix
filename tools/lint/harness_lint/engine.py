"""lint 引擎：跑规则、汇总报告。

设计上最要紧的两条：

1. **规则抛异常不许静默吞掉。** linter 最阴的失效方式是"某条规则悄悄不跑了"，
   报告里一片绿。这里把规则异常转成 `HX998 内部错误`（error 级）显式报出来。
2. **豁免是一等公民**（见 suppressions.py）：必须带理由、要统计、要有预算。

模块分工：discovery（读文件）→ engine（跑规则）→ report（装与算）。
`run()` 是本模块唯一的编排入口，其余都是可单独测试的小函数。
"""

from __future__ import annotations

import os
import traceback
from dataclasses import dataclass
from typing import Iterable, List, Optional, Tuple

from . import graph as graph_mod
from .config import Config
from .discovery import (  # noqa: F401 —— 重新导出，保持既有 import 路径可用
    FileContext,
    ProjectContext,
    count_tests,
    is_test_file,
    iter_py_files,
    load_source,
)
from .report import (  # noqa: F401
    LintReport,
    _compute_new,
    internal_error,
    make_baseline,
    register_rule_levels,
    rule_level,
)
from .suppressions import apply_suppressions, collect_directives, parse_directive  # noqa: F401


# ---------------------------------------------------------------- 规则基类


class Rule:
    """文件级规则：只看一个文件就能判。"""

    id: str = ""
    name: str = ""
    level: str = "warning"
    summary: str = ""
    doc: str = ""
    # 能否自动修必须**显式声明**（抄 ESLint 的做法：不声明就是不能）
    fixable: bool = False
    fix_blocked_reason: Optional[str] = None
    requires_ast: bool = True

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:  # pragma: no cover
        return []


class ProjectRule:
    """项目级规则：需要跨文件信息（依赖图等）。"""

    id: str = ""
    name: str = ""
    level: str = "error"
    summary: str = ""
    doc: str = ""
    fixable: bool = False
    fix_blocked_reason: Optional[str] = None

    def check_project(self, ctx: ProjectContext) -> Iterable[Diagnostic]:  # pragma: no cover
        return []


# ---------------------------------------------------------------- 规则基类


class Rule:
    """文件级规则：只看一个文件就能判。"""

    id: str = ""
    name: str = ""
    level: str = "warning"
    summary: str = ""
    doc: str = ""
    # 能否自动修必须**显式声明**（抄 ESLint 的做法：不声明就是不能）
    fixable: bool = False
    fix_blocked_reason: Optional[str] = None
    requires_ast: bool = True

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:  # pragma: no cover
        return []


class ProjectRule:
    """项目级规则：需要跨文件信息（依赖图等）。"""

    id: str = ""
    name: str = ""
    level: str = "error"
    summary: str = ""
    doc: str = ""
    fixable: bool = False
    fix_blocked_reason: Optional[str] = None

    def check_project(self, ctx: ProjectContext) -> Iterable[Diagnostic]:  # pragma: no cover
        return []


def _execute_file_rules(rules: Iterable[object], files_ctx: List[FileContext], report) -> None:
    """跑文件级规则。**规则异常必须外显**，见模块 docstring 第 1 条。"""
    for fctx in files_ctx:
        for rule in rules:
            if rule.requires_ast and fctx.tree is None:
                continue
            try:
                for d in rule.check(fctx) or []:
                    report.diagnostics.append(d)
            except Exception:  # noqa: BLE001 —— 故意宽泛：任何规则异常都要变成可见的诊断
                report.diagnostics.append(
                    internal_error(rule.id, fctx.rel, traceback.format_exc())
                )


def _execute_project_rules(rules: Iterable[object], pctx: ProjectContext, report) -> None:
    for rule in rules:
        try:
            for d in rule.check_project(pctx) or []:
                report.diagnostics.append(d)
        except Exception:  # noqa: BLE001 —— 故意宽泛：项目级规则异常同样必须可见，不能让它带走整份报告
            report.diagnostics.append(internal_error(rule.id, "<project>", traceback.format_exc()))


@dataclass
class FullRun:
    """一次完整扫描的结果：报告 + 规则表 + **文件上下文** + 项目上下文。

    熵管理需要文件上下文（算代码行、文件行数分位、重复实现），
    所以把 `run()` 的内部产物原样暴露出来，而不是让它再扫一遍。
    """

    report: LintReport
    rules: List[object]
    files: List[FileContext]
    project: ProjectContext


def run_full(
    root: str,
    cfg: Optional[Config] = None,
    baseline: Optional[dict] = None,
    max_suppressions: Optional[int] = None,
) -> FullRun:
    """跑一遍全部规则，返回完整上下文。"""
    from .rules import ALL_PROJECT_RULES, ALL_RULES  # 延迟导入，避免循环依赖

    cfg = cfg or Config()
    root = os.path.abspath(root)
    report = LintReport(root=root, baseline=baseline, conventions_dir=cfg.resolved_conventions_dir)
    register_rule_levels(list(ALL_RULES) + list(ALL_PROJECT_RULES))

    loaded = [load_source(root, rel, cfg) for rel in iter_py_files(root, cfg)]
    files_ctx = [f.context for f in loaded if f.context is not None]
    ast_files = [f.ast_entry for f in loaded if f.ast_entry is not None]
    for item in loaded:
        report.diagnostics.extend(item.diagnostics)
        if item.context is None:
            report.files_skipped += 1
        elif item.context.is_test:
            report.test_count += count_tests(item.context.tree)
    report.files_scanned = len(files_ctx)

    _execute_file_rules([r for r in ALL_RULES if not cfg.is_ignored_rule(r.id)], files_ctx, report)
    pctx = ProjectContext(
        root=root,
        config=cfg,
        files=files_ctx,
        graph=graph_mod.build_graph(root, ast_files, cfg),
        conventions_dir=cfg.resolved_conventions_dir,
    )
    _execute_project_rules(
        [r for r in ALL_PROJECT_RULES if not cfg.is_ignored_rule(r.id)], pctx, report
    )

    apply_suppressions(report, files_ctx, cfg, max_suppressions)
    _compute_new(report, baseline)

    return FullRun(
        report=report,
        rules=list(ALL_RULES) + list(ALL_PROJECT_RULES),
        files=files_ctx,
        project=pctx,
    )


def run(
    root: str,
    cfg: Optional[Config] = None,
    baseline: Optional[dict] = None,
    max_suppressions: Optional[int] = None,
) -> Tuple[LintReport, List[object]]:
    """跑一遍全部规则。返回 (报告, 规则列表)。"""
    fr = run_full(root, cfg, baseline, max_suppressions)
    return fr.report, fr.rules
