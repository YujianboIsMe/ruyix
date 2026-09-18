"""架构类规则：HX301 分层依赖方向 / HX302 循环依赖

这两条是本套规则里唯一需要跨文件信息的，事实来源在 `graph.py`。判定刻意保守：
**任何一侧判不出层就不报** —— 架构规则一旦误报，用户对整个 linter 的信任会一起崩掉。
"""

from __future__ import annotations

from typing import Dict, Iterable, List

from ..diagnostic import Diagnostic, Span
from ..engine import ProjectContext, ProjectRule

DOC_LAYERING = "docs/conventions/layering.md"


def _chain(cfg) -> str:
    return " → ".join(cfg.layers)


class LayerViolation(ProjectRule):
    id = "HX301"
    name = "layer-violation"
    level = "error"
    summary = "分层依赖方向违规"
    doc = DOC_LAYERING
    fixable = False
    fix_blocked_reason = "消除越层通常要在中间层补方法，涉及跨文件结构调整"

    def check_project(self, ctx: ProjectContext) -> Iterable[Diagnostic]:
        if ctx.graph is None:
            return []
        from ..layering import layer_violations

        out: List[Diagnostic] = []
        for v in layer_violations(ctx.config, ctx.graph):
            ref = v["ref"]
            src_layer, dst_layer = v["src_layer"], v["dst_layer"]
            line = ref.source_line
            span = Span(
                file=ref.file,
                line_start=ref.lineno,
                column_start=ref.column,
                label=f"{src_layer} → {dst_layer}",
                text=[line],
            )
            if v["kind"] == "reverse":
                msg = (
                    f"反向依赖：{src_layer} 层引用了 {dst_layer} 层"
                    f"（依赖方向必须向下，低层不得依赖高层）"
                )
                why = (
                    f"{src_layer} 层处在依赖链的下游位置，被它依赖的 {dst_layer} 层"
                    "是更上层的东西。这会让“换实现/加缓存/改协议”这种下层改动"
                    f"被迫去改 {dst_layer} 层——依赖方向反了，改动成本会沿着箭头反向传导。"
                )
            else:
                msg = (
                    f"越层依赖：{src_layer} 层直接引用了 {dst_layer} 层"
                    f"（严格模式下只允许依赖紧邻下层）"
                )
                why = (
                    f"跳过了中间层，等于把中间层存在的意义（隔离、转换、复用）绕过去了："
                    f"{dst_layer} 的实现细节会直接渗到 {src_layer}，中间层再想插东西也插不进来。"
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
                    f"把这次调用改成经过中间层：① 在 {src_layer} 与 {dst_layer} 之间那一层"
                    f"暴露一个方法（内部再调 {dst_layer}）② 把这条 import 改成依赖那一层。"
                    "如果中间层确实缺这个能力，先加中间层方法再改调用方，不要反向 import。"
                )
                .with_child(
                    "note",
                    f"允许的依赖方向（单向，从左到右为低层到高层）：{_chain(ctx.config)}。"
                    "层与目录的对应关系在 .harnesslint.toml 的 [architecture] 里配置。",
                )
            )
        return out


class CircularImport(ProjectRule):
    id = "HX302"
    name = "circular-import"
    level = "error"
    summary = "模块间循环依赖"
    doc = DOC_LAYERING
    fixable = False
    fix_blocked_reason = "打破环要判断哪一侧的依赖该被抽走，机器无法代判"

    def check_project(self, ctx: ProjectContext) -> Iterable[Diagnostic]:
        if ctx.graph is None:
            return []
        cycles = ctx.graph.find_cycles(limit=5)
        # 建 (src,dst) → 行号 索引，好在环上定位一行
        edge_line: Dict[tuple, int] = {}
        for e in ctx.graph.edges:
            edge_line.setdefault((e.src_file, e.dst_file), e.lineno)

        out: List[Diagnostic] = []
        for cyc in cycles:
            chain = cyc + [cyc[0]]
            first = cyc[0]
            lineno = 1
            for a, b in zip(cyc, cyc[1:] + [cyc[0]]):
                if (a, b) in edge_line:
                    first, lineno = a, edge_line[(a, b)]
                    break
            line = ""
            for f in ctx.files:
                if f.rel == first and 1 <= lineno <= len(f.lines):
                    line = f.lines[lineno - 1]
                    break
            out.append(
                Diagnostic(
                    rule=self.id,
                    level=self.level,
                    message="循环依赖：" + " → ".join(chain),
                    span=Span(
                        file=first,
                        line_start=lineno,
                        column_start=1,
                        label="环上的第一跳",
                        text=[line] if line else [],
                    ),
                    doc=self.doc,
                    fixable=self.fixable,
                    fix_blocked_reason=self.fix_blocked_reason,
                )
                .with_why(
                    "循环依赖让模块无法被单独导入/单独测试；导入顺序一变就可能出现"
                    "部分初始化的模块（ImportError 或 None 属性），是典型的偶发故障来源。"
                )
                .with_fix(
                    "三选一：① 把双方都需要的定义（数据结构、常量、协议）抽到 types 层，"
                    "两边都依赖它 ② 让其中一侧只依赖抽象（传函数/对象进来，而不是 import）"
                    "③ 把其中一侧的导入延迟到函数内部（能解决导入期问题，但不改变依赖方向，"
                    "属于治标）。"
                )
                .with_child("note", f"环路径：{' → '.join(chain)}")
                .with_child(
                    "note",
                    "判定边界：只有环上**每条边都是模块级 import** 才报。任何一条边写在函数体内"
                    "（延迟导入）就不报 —— 那已经不构成导入期隐患，也正是打破环的标准做法；"
                    "但依赖方向本身没变，所以 HX301 分层判定**照样算它**。",
                )
            )
        return out


PROJECT_RULES = (LayerViolation, CircularImport)
