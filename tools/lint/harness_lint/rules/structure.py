"""结构与测试类规则：HX101 / HX102 / HX103"""

from __future__ import annotations

import ast
from typing import Iterable, List

from ..diagnostic import Diagnostic
from ..engine import FileContext, Rule
from ._util import body_is_only, has_assertion

DOC_FILE_SIZE = "docs/conventions/file-size.md"
DOC_TESTING = "docs/conventions/testing.md"


class FileTooLong(Rule):
    id = "HX101"
    name = "file-too-long"
    level = "warning"
    summary = "单文件行数超过上限"
    doc = DOC_FILE_SIZE
    fixable = False
    fix_blocked_reason = "按什么职责切分需要理解模块边界，机器无法代判"

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:
        limit = ctx.config.max_file_lines
        n = len(ctx.lines)
        if n <= limit:
            return []
        over = n - limit
        return [
            Diagnostic(
                rule=self.id,
                level=self.level,
                message=f"文件 {n} 行，超过上限 {limit} 行（超出 {over} 行）",
                span=ctx.span(1, 1, label=f"{n} 行"),
                doc=self.doc,
                fixable=self.fixable,
                fix_blocked_reason=self.fix_blocked_reason,
            )
            .with_why(
                f"文件越长，一次能完整读进上下文的概率越低，改错位置的爆炸半径越大。"
                f"经验上超过 {limit} 行后，人和模型都开始“只看局部改局部”。"
            )
            .with_fix(
                "按职责拆分：把与 IO 无关的纯函数抽到独立模块、把数据类抽到 types 层。"
                "**不允许形式拆分**——把 a.py 改成 a.py + a_part2.py（文件变小了但耦合没变）"
                "不算修好，同一模块的代码应当在一起。"
            )
        ]


class FunctionTooLong(Rule):
    id = "HX102"
    name = "function-too-long"
    level = "warning"
    summary = "单函数行数超过上限"
    doc = DOC_FILE_SIZE
    fixable = False
    fix_blocked_reason = "提取哪些步骤为独立函数取决于语义，机器无法代判"

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:
        if ctx.tree is None:
            return []
        limit = ctx.config.max_function_lines
        out: List[Diagnostic] = []
        for node in ast.walk(ctx.tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            end = getattr(node, "end_lineno", None)
            if end is None:
                continue
            n = end - node.lineno + 1
            if n <= limit:
                continue
            out.append(
                Diagnostic(
                    rule=self.id,
                    level=self.level,
                    message=f"函数 {node.name}() 有 {n} 行，超过上限 {limit} 行",
                    span=ctx.span(
                        node.lineno,
                        node.col_offset + 1,
                        end_lineno=end,
                        label=f"{n} 行",
                        context_lines=0,
                    ),
                    doc=self.doc,
                    fixable=self.fixable,
                    fix_blocked_reason=self.fix_blocked_reason,
                )
                .with_why(
                    f"{n} 行的函数通常混着多个抽象层级（取数据、算业务、打日志、处理异常），"
                    f"改一处必须读懂全部，测试也很难只覆盖其中一段。"
                )
                .with_fix(
                    "按“读起来像一段话”切成若干小函数：每个函数只做一件事，名字能当注释用。"
                    "典型切法：把 try/except 里的主体、循环体、条件分支各自的处理抽出去。"
                )
            )
        return out


class TestWithoutAssertion(Rule):
    id = "HX103"
    name = "test-without-assertion"
    level = "error"
    summary = "测试函数没有任何断言"
    doc = DOC_TESTING
    fixable = False
    fix_blocked_reason = "断言什么取决于被测函数的预期行为"

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:
        if ctx.tree is None or not ctx.is_test:
            return []
        out: List[Diagnostic] = []
        for node in ast.walk(ctx.tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if not node.name.startswith("test_"):
                continue
            if has_assertion(node):
                continue
            end = getattr(node, "end_lineno", node.lineno)
            only_stub = body_is_only(node, (ast.Pass,))
            name = node.name
            out.append(
                Diagnostic(
                    rule=self.id,
                    level=self.level,
                    message=(
                        f"测试 {name}() 没有任何断言"
                        + (
                            "（函数体只有 pass）"
                            if only_stub
                            else "（只调用了被测代码，没有校验结果）"
                        )
                    ),
                    span=ctx.span(
                        node.lineno,
                        node.col_offset + 1,
                        end_lineno=end,
                        label=name,
                        context_lines=0,
                    ),
                    doc=self.doc,
                    fixable=self.fixable,
                    fix_blocked_reason=self.fix_blocked_reason,
                )
                .with_why(
                    "没有断言的测试永远通过，却会抬高覆盖率数字——这是“覆盖率幻觉”的主要来源。"
                    "它比没有测试更危险：你以为被保护了，其实没有。"
                )
                .with_fix(
                    f"给 {name}() 加断言：正常路径用 assert / assertEqual 校验返回值；"
                    "再补一条边界或异常路径（空输入、单元素、越界、除零、非法类型），"
                    "异常路径用 pytest.raises 或 self.assertRaises。"
                )
            )
        return out


RULES = (FileTooLong, FunctionTooLong, TestWithoutAssertion)
