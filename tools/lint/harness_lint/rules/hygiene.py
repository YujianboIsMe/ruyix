"""卫生类规则：HX104 调试 print 残留 / HX105 未完成占位

HX104 是本套规则里**误报风险最高**的一条（CLI 入口用 print 输出结果是正当的），
所以它单独做了 main guard 豁免，并把建议的 applicability 明确标成 `MaybeIncorrect`
而不是 `MachineApplicable` —— 把 print 换成 logging 的写法不唯一（日志级别、要不要保留
f-string 插值都要人判断），谎称机器可自动应用会让 --fix 类工具改出合法但错误的结果。
"""

from __future__ import annotations

import ast
import io
import re
import tokenize
from typing import Iterable, List

from ..diagnostic import Diagnostic
from ..engine import FileContext, Rule
from ._util import describe_call, is_under_main_guard, iter_with_parents, literal_str

DOC_LOGGING = "docs/conventions/logging.md"
DOC_PLACEHOLDER = "docs/conventions/placeholders.md"

_PLACEHOLDER_WORDS = ("TODO", "FIXME", "XXX", "HACK")
# 必须是独立单词：早期用子串匹配，把注释里的 check_xxx 也当成标记词报了（真实误报）。
# 这里刻意用前后视断言而不是词边界元字符——元字符的反斜杠在多层转义里被变成过退格字符。
# （本行若出现标记词本身，会被自己的规则拦下，所以用中文描述代替。）
_PLACEHOLDER_RE = re.compile(
    "(?<![A-Za-z0-9_])(TODO|FIXME|XXX|HACK)(?![A-Za-z0-9_])",
    re.IGNORECASE,
)


class DebugPrint(Rule):
    id = "HX104"
    name = "debug-print"
    level = "warning"
    summary = "调试用 print 残留"
    doc = DOC_LOGGING
    fixable = False
    fix_blocked_reason = (
        "换成 logging 的写法不唯一（级别、是否保留 f-string 插值需人判断），"
        "因此只给 MaybeIncorrect 建议、不宣称可自动应用"
    )

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:
        if ctx.tree is None or not ctx.config.flag_print:
            return []
        out: List[Diagnostic] = []
        for node, parents in iter_with_parents(ctx.tree):
            if not isinstance(node, ast.Call):
                continue
            fn = node.func
            if not (isinstance(fn, ast.Name) and fn.id == "print"):
                continue
            if is_under_main_guard(node, parents):
                continue  # CLI 入口的 print 是正当输出，不报

            called = describe_call(node)
            args_preview = called[len("print(") : -1] if called.startswith("print(") else ""
            line = ctx.lines[node.lineno - 1] if node.lineno <= len(ctx.lines) else ""

            # 建议：把被调函数换成 logger.debug，参数原样保留。
            # 这样替换后仍是合法 Python，正好符合 MaybeIncorrect 的定义。
            # 最小替换：只把 `print(` 这一段换成 `logger.debug(`，参数原样保留。
            # 不替换整行——整行替换会把缩进也带进去，渲染与自动应用都会错位。
            col = node.col_offset + 1
            tail = line[node.col_offset :] if line else ""
            span = ctx.span(node.lineno, col, label="调试输出")
            if tail.startswith("print("):
                span.column_end = col + len("print(")
                span.suggested_replacement = "logger.debug("
                span.suggestion_applicability = "MaybeIncorrect"

            out.append(
                Diagnostic(
                    rule=self.id,
                    level=self.level,
                    message=f"调试用 print({args_preview}) 残留",
                    span=span,
                    doc=self.doc,
                    fixable=self.fixable,
                    fix_blocked_reason=self.fix_blocked_reason,
                )
                .with_why(
                    "调试残留会污染 stdout。当这个模块被用在 CLI 管道里时，"
                    "一行 print 就能把下游的机器可读输出弄脏，而且很难定位。"
                )
                .with_fix(
                    "改用 logging：`logger = logging.getLogger(__name__)`，"
                    "把 print(x) 换成 logger.debug(...)（调试）或 logger.info(...)（业务事件）。"
                    "确实需要向用户输出时，放在 main() 里用 print 是允许的（本规则不报 main guard 内）。"
                )
                .with_child(
                    "note",
                    "本条 suggestion 的 applicability = MaybeIncorrect：替换后仍是合法代码，"
                    "但日志级别与格式需要人确认。",
                )
            )
        return out


class PlaceholderLeft(Rule):
    id = "HX105"
    name = "placeholder-left"
    level = "warning"
    summary = "留有未完成的实现（TODO / 空桩函数）"
    doc = DOC_PLACEHOLDER
    fixable = False
    fix_blocked_reason = "把桩实现补完需要业务语义，机器无法代判"

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:  # noqa: HX102 reason=本函数是两条独立检查（注释标记 / 空桩）的入口，拆成两个函数会让"一个规则类只做一件事"的对应关系变模糊
        if not ctx.config.flag_placeholders:
            return []
        out: List[Diagnostic] = []

        # 1. 注释里的 TODO / FIXME / XXX / HACK  # noqa: HX105 reason=规则文档里必须列出它检测的标记词，这里不是未完成的待办
        try:
            for tok in tokenize.generate_tokens(io.StringIO(ctx.source).readline):
                if tok.type != tokenize.COMMENT:
                    continue
                text = tok.string
                m = _PLACEHOLDER_RE.search(text)
                if not m:
                    continue
                hit = m.group(1).upper()
                lineno = tok.start[0]
                out.append(
                    Diagnostic(
                        rule=self.id,
                        level=self.level,
                        message=f"注释里留有 {hit} 标记未处理",
                        span=ctx.span(lineno, tok.start[1] + 1, label=hit, context_lines=1),
                        doc=self.doc,
                        fixable=self.fixable,
                        fix_blocked_reason=self.fix_blocked_reason,
                    )
                    .with_why(
                        f"{hit} 是“以后再说”的记号。生成产物如果是交付物，这些标记会一路带到生产；"
                        "即便只是临时脚本，也应当把边界写清楚。"
                    )
                    .with_fix(
                        "要么现在把这件事做完（删掉标记），要么把它变成可追踪的待办"
                        "（写进任务清单而不是注释）。确实需要保留时，"
                        "写成 `# harnesslint: disable-file HX105 reason=<原因>`。"
                    )
                )
        except (tokenize.TokenError, IndentationError, SyntaxError):
            # 词法解析失败就跳过注释检查：语法错误由 HX000 单独报，这里不重复报一遍
            pass

        # 2. 空桩函数：函数体只有 ... 或 pass（@abstractmethod 除外）
        if ctx.tree is not None:
            for node in ast.walk(ctx.tree):
                if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    continue
                decorators = [
                    d.attr if isinstance(d, ast.Attribute) else getattr(d, "id", "")
                    for d in node.decorator_list
                ]
                if "abstractmethod" in decorators or "abstractstaticmethod" in decorators:
                    continue
                body = node.body
                if (
                    body
                    and isinstance(body[0], ast.Expr)
                    and literal_str(body[0].value) is not None
                ):
                    body = body[1:]
                if len(body) != 1:
                    continue
                only = body[0]
                is_ellipsis = (
                    isinstance(only, ast.Expr)
                    and isinstance(only.value, ast.Constant)
                    and only.value.value is Ellipsis
                )
                is_pass = isinstance(only, ast.Pass)
                if not (is_ellipsis or is_pass):
                    continue
                end = getattr(node, "end_lineno", node.lineno)
                marker = "..." if is_ellipsis else "pass"
                out.append(
                    Diagnostic(
                        rule=self.id,
                        level=self.level,
                        message=f"函数 {node.name}() 是空桩实现（函数体只有 {marker}）",
                        span=ctx.span(
                            node.lineno,
                            node.col_offset + 1,
                            end_lineno=end,
                            label="未实现",
                            context_lines=0,
                        ),
                        doc=self.doc,
                        fixable=self.fixable,
                        fix_blocked_reason=self.fix_blocked_reason,
                    )
                    .with_why(
                        "空桩会静默返回 None：调用方拿到 None 继续往下跑，"
                        "错误会飘到很远的地方才爆出来，排查成本极高。"
                    )
                    .with_fix(
                        f"实现 {node.name}() 的真实逻辑；如果这一步还没准备好，"
                        "就显式 raise NotImplementedError 并写明原因，让失败尽早发生在调用点。"
                    )
                )
        return out


RULES = (DebugPrint, PlaceholderLeft)
