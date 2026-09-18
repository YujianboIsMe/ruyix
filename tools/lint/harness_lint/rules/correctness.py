"""正确性类规则：HX201 裸 except / HX203 可变默认参数 / HX204 与 None 比较

HX204 是本套规则里唯一真正达到 `MachineApplicable` 级别的：`== None` → `is None`
是语义等价且无歧义的替换。为了让这个"机器可自动应用"不变成空话，它做了两层保护：
  1. 只在**单行**比较上给建议（多行无法安全切片）；
  2. 替换前先校验待替换区间的原文确实是 `== None` / `!= None`（防止 AST 位置与实际
     源码错位导致改坏代码）——把 `suggested_replacement` 当契约就得这么写。
"""

from __future__ import annotations

import ast
import re
from typing import Iterable, List, Optional, Tuple

from ..diagnostic import Diagnostic
from ..engine import FileContext, Rule
from ._util import literal_str, unparse

DOC_ERROR_HANDLING = "docs/conventions/error-handling.md"
DOC_PYTHON_PITFALLS = "docs/conventions/python-pitfalls.md"

_MUTABLE_KINDS = {
    ast.List: "列表",
    ast.Dict: "字典",
    ast.Set: "集合",
    ast.ListComp: "列表推导",
    ast.DictComp: "字典推导",
    ast.SetComp: "集合推导",
}
_MUTABLE_CALLS = {"list", "dict", "set", "bytearray"}
_COMPARISON_RE = re.compile(r"\s*(==|!=)\s*None\s*")


class BareExcept(Rule):
    id = "HX201"
    name = "bare-except"
    level = "error"
    summary = "裸 except 或 except 后吞掉异常"
    doc = DOC_ERROR_HANDLING
    fixable = False
    fix_blocked_reason = "该捕获哪些异常、捕获后如何处理，取决于业务语义"

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:
        if ctx.tree is None:
            return []
        out: List[Diagnostic] = []
        for node in ast.walk(ctx.tree):
            if not isinstance(node, ast.ExceptHandler):
                continue
            is_bare = node.type is None
            swallows = (
                isinstance(node.type, ast.Name)
                and node.type.id == "Exception"
                and len(node.body) == 1
                and isinstance(node.body[0], ast.Pass)
            )
            if not (is_bare or swallows):
                continue

            end = getattr(node, "end_lineno", node.lineno)
            span = ctx.span(node.lineno, node.col_offset + 1, end_lineno=end, label="异常处理")
            if is_bare:
                msg = "使用了裸 except（except:），会连 KeyboardInterrupt / SystemExit 一起吞掉"
                why = (
                    "裸 except 捕获一切异常，包括 Ctrl+C 触发的 KeyboardInterrupt 和 "
                    "sys.exit() 抛的 SystemExit。表现是“进程杀不掉”或“退出码永远是 0”，"
                    "CI 里尤其难查。"
                )
            else:
                msg = "except Exception 后直接 pass，异常被静默吞掉"
                why = (
                    "静默吞异常会把真实故障变成“看起来正常”：典型症状是测试全绿但数据不对。"
                    "它比直接报错难查得多，因为没有任何线索指向出错位置。"
                )
            out.append(
                Diagnostic(
                    rule=self.id,
                    level=self.level,
                    message=msg,
                    span=span,
                    doc=self.doc,
                    fixable=self.fixable,
                    fix_blocked_reason=self.fix_blocked_reason,
                )
                .with_why(why)
                .with_fix(
                    "捕获具体异常类型，并在捕获后三选一："
                    "① 记日志 logger.warning(...) ② 转成本层领域异常再抛（不要丢原始 traceback，"
                    "用 raise X from e）③ 明确注释为什么可以忽略。"
                    "例：except (ValueError, KeyError) as e: logger.warning(\"跳过非法条目: %s\", e)"
                )
                .with_child(
                    "note",
                    "确实必须保留这种写法时，加行内豁免："
                    "# noqa: HX201 reason=<为什么这里可以吞掉>（必须带理由，见 HX900）",
                )
            )
        return out


class MutableDefaultArg(Rule):
    id = "HX203"
    name = "mutable-default-arg"
    level = "warning"
    summary = "函数默认参数是可变对象"
    doc = DOC_PYTHON_PITFALLS
    fixable = False
    fix_blocked_reason = "改成 None 哨兵后函数体内要同步处理，机器无法代判"

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:
        if ctx.tree is None:
            return []
        out: List[Diagnostic] = []
        for node in ast.walk(ctx.tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            for argname, default, lineno in _defaults(node):
                kind = _mutable_kind(default)
                if kind is None:
                    continue
                out.append(
                    Diagnostic(
                        rule=self.id,
                        level=self.level,
                        message=f"参数 {argname} 的默认值是可变对象（{kind}）",
                        span=ctx.span(lineno, 1, label=unparse(default)),
                        doc=self.doc,
                        fixable=self.fixable,
                        fix_blocked_reason=self.fix_blocked_reason,
                    )
                    .with_why(
                        "默认值只在函数定义时求值一次，之后被所有调用共享："
                        f"任何一次调用往里面写了东西，后续调用都会看见（{node.name}() 的"
                        "行为会随调用次数悄悄改变）。"
                    )
                    .with_fix(
                        f"改成 None 哨兵并在函数体开头初始化：\n"
                        f"    def {node.name}(..., {argname}=None):\n"
                        f"        if {argname} is None:\n"
                        f"            {argname} = []"
                    )
                )
        return out


def _defaults(node: ast.AST) -> Iterable[Tuple[str, ast.AST, int]]:
    args = node.args  # type: ignore[attr-defined]
    pos = list(args.posonlyargs) + list(args.args)
    defaults = list(args.defaults)
    for arg, default in zip(pos[len(pos) - len(defaults) :], defaults):
        yield arg.arg, default, getattr(default, "lineno", node.lineno)
    for arg, default in zip(args.kwonlyargs, args.kw_defaults):
        if default is not None:
            yield arg.arg, default, getattr(default, "lineno", node.lineno)


def _mutable_kind(node: ast.AST) -> Optional[str]:
    for cls, name in _MUTABLE_KINDS.items():
        if isinstance(node, cls):
            return name
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
        if node.func.id in _MUTABLE_CALLS:
            return f"{node.func.id}()"
    return None


class NoneComparison(Rule):
    id = "HX204"
    name = "none-comparison"
    level = "warning"
    summary = "用 == / != 与 None 比较"
    doc = DOC_PYTHON_PITFALLS
    fixable = True  # 有 MachineApplicable 建议 → 真的可以自动改
    fix_blocked_reason = None

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:  # noqa: HX102 reason=判定 + 列区间计算强耦合（建议的列必须来自同一份 AST），拆开会让"可自动应用"的校验与生成分家
        if ctx.tree is None:
            return []
        out: List[Diagnostic] = []
        for node in ast.walk(ctx.tree):
            if not isinstance(node, ast.Compare):
                continue
            if len(node.ops) != 1 or len(node.comparators) != 1:
                continue
            op = node.ops[0]
            if not isinstance(op, (ast.Eq, ast.NotEq)):
                continue
            comp = node.comparators[0]
            if not (isinstance(comp, ast.Constant) and comp.value is None):
                continue

            opname = "==" if isinstance(op, ast.Eq) else "!="
            replacement = "is None" if isinstance(op, ast.Eq) else "is not None"

            d = (
                Diagnostic(
                    rule=self.id,
                    level=self.level,
                    message=f"用 {opname} None 比较（应写 {replacement}）",
                    span=ctx.span(node.lineno, node.col_offset + 1, label=f"{opname} None"),
                    doc=self.doc,
                    fixable=self.fixable,
                    fix_blocked_reason=self.fix_blocked_reason,
                )
                .with_why(
                    f"`{opname} None` 调用的是类型的 __eq__，可以被重载成任何行为"
                    "（numpy 数组、ORM 字段代理、pandas 都会给出意料之外的结果，"
                    "甚至返回元素级数组让 if 报 ambiguous）。`is` 判断的是同一性，不可被重载。"
                )
                .with_fix(f"把 {opname} None 改成 {replacement}（语义等价，本规则可自动应用）")
            )

            # 只有单行比较才能安全切片替换
            same_line = (
                node.lineno == getattr(node, "end_lineno", node.lineno)
                and node.left.end_lineno == node.lineno
                and comp.lineno == node.lineno
            )
            if same_line:
                line = ctx.lines[node.lineno - 1] if node.lineno <= len(ctx.lines) else ""
                raw = line[(node.left.end_col_offset or 0) : (comp.end_col_offset or 0)]
                # 从操作符本身开始替换：把 left 与操作符之间的空白留在原文里，
                # 否则替换会吃掉那个空格（实测渲染出 `if nameis None:`）
                op_start = (node.left.end_col_offset or 0) + (len(raw) - len(raw.lstrip()))
                end_off = comp.end_col_offset or 0
                segment = line[op_start:end_off]
                # 关键保护：先确认这段原文确实就是 `== None`，再给"可自动应用"的承诺
                if _COMPARISON_RE.fullmatch(segment):
                    d.span.column_start = op_start + 1
                    d.span.column_end = end_off + 1
                    d.span.suggested_replacement = replacement
                    d.span.suggestion_applicability = "MachineApplicable"
            if d.span.suggested_replacement is None:
                d.span.suggestion_applicability = "Unspecified"
                d.with_child(
                    "note",
                    "多行表达式上暂不给自动修复建议：无法在不理解换行的前提下安全切片",
                )
            out.append(d)
        return out


RULES = (BareExcept, MutableDefaultArg, NoneComparison)
