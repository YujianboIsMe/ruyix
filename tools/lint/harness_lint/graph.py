"""导入图与分层判定。

**为什么单独一层做分析**：`HX301 分层依赖方向` 与 `HX302 循环依赖` 需要跨文件信息，
是全部规则里唯一不能"看一个文件就能判"的。把它们的事实来源（import 图）与判定逻辑
分开，规则本身就只有几行，测试也能直接对图下断言。

**本模块只产出"事实"**（谁导入了谁、解析到哪个文件、哪些环）；分层方向的判定在
`layering.py`。两者变化的理由不同：事实随代码变，策略随架构约定变。

两个刻意的保守选择（对齐"宁可漏报不误报"）：
1. **判不出层就不报**。目录名匹配不上任何层（`misc/`、`examples/`）→ 返回空层，规则跳过。
2. **模块名解析不到文件就不进图**。循环依赖只对"确实解析到本仓库内文件"的边建图，
   否则第三方包/动态导入会造出假环。
3. **自环不进图**（`src == dst` 的边不是依赖，也构不成环）。

> **v0.6 修掉的一个真误报**：相对导入的基准是**包**，不是"文件所在目录"。
> 旧实现拿 `module` 直接去仓库根下找，于是 `api/__init__.py` 里的
> `from .api import create_user` 被解析成 `api/__init__.py` **自己** ——
> 报出 `循环依赖：api/__init__.py → api/__init__.py` 这种假环。
> 三层结构里"包内 `__init__.py` 重导出"是最常见的写法，所以这条误报能成片出现
> （v0.6 的知识库开/关对照实验就被它污染过，见 `doc/需求-Harness-v0.6-本地知识库.md` §10.4）。
"""

from __future__ import annotations

import ast
import os
from dataclasses import dataclass, field
from typing import Dict, List, Optional

from .config import Config


@dataclass
class ImportRef:
    """一条 import 语句（解析前的原始事实）。"""

    file: str  # 相对 lint 根的路径
    lineno: int
    column: int  # 1-based
    source_line: str
    module: str  # 点分模块名；纯相对导入时是点之后的剩余部分（可能为空）
    level: int  # 相对导入的点的个数（0 = 绝对导入）
    names: List[str] = field(default_factory=list)  # from X import a, b
    is_from: bool = False
    # 是否在**函数体内**（延迟导入）。破坏环的标准做法就是它：
    # 导入期不再执行，因此不再有"部分初始化模块"的隐患。
    # 但依赖方向本身还在，所以 HX301（分层方向）照样要算它。
    deferred: bool = False

    @property
    def display(self) -> str:
        if self.is_from:
            return f"from {'.' * self.level}{self.module} import {', '.join(self.names)}"
        return f"import {'.' * self.level}{self.module}"


@dataclass
class Edge:
    src_file: str
    dst_file: str
    lineno: int
    deferred: bool = False


class Graph:
    """模块/文件级依赖图。"""

    def __init__(self) -> None:
        self.edges: List[Edge] = []
        self.imports: List[ImportRef] = []
        self.unresolved: List[ImportRef] = []
        # 解析成"自己导入自己"的语句：不是依赖、不是环，但也不是"解析不了"，
        # 单独记一份，免得丢掉事实（诊断里可以提示"这条 import 指向本文件"）。
        self.self_edges: List[ImportRef] = []

    def targets(self, src_file: str) -> List[Edge]:
        return [e for e in self.edges if e.src_file == src_file]

    def find_cycles(self, limit: int = 5, include_deferred: bool = False) -> List[List[str]]:
        """返回最多 limit 个环（每个环是文件路径列表，首尾不重复）。

        **默认只用模块级导入成图**（`include_deferred=False`）：环上只要有一条边是
        函数体内的延迟导入，导入期就不会出问题 —— 那正是打破环的标准做法
        （本仓库的引擎↔规则表就是这么处理的，如果照报，等于把"已经修好的代码"再报一遍）。
        HX301 分层方向不受影响：延迟导入**照样算方向违规**（依赖关系本身没变）。
        """
        adj: Dict[str, List[str]] = {}
        for e in self.edges:
            if e.deferred and not include_deferred:
                continue
            adj.setdefault(e.src_file, []).append(e.dst_file)

        cycles: List[List[str]] = []
        seen_keys = set()
        color: Dict[str, int] = {}  # 0 未访问 1 在栈上 2 完成
        stack: List[str] = []

        def dfs(node: str) -> None:
            if len(cycles) >= limit:
                return
            color[node] = 1
            stack.append(node)
            for nxt in adj.get(node, []):
                if len(cycles) >= limit:
                    break
                c = color.get(nxt, 0)
                if c == 1:
                    # 回边 → 栈上从 nxt 到当前节点构成一个环
                    idx = stack.index(nxt)
                    cyc = stack[idx:]
                    key = tuple(sorted(cyc))
                    if key not in seen_keys:
                        seen_keys.add(key)
                        cycles.append(list(cyc))
                elif c == 0:
                    dfs(nxt)
            stack.pop()
            color[node] = 2

        for node in list(adj.keys()):
            if color.get(node, 0) == 0:
                dfs(node)
        return cycles


def iter_imports(rel_path: str, tree: ast.AST, source_lines: List[str]) -> List[ImportRef]:
    """抽出文件里的全部 import 语句。

    **函数体内的 import 标成 `deferred`**（延迟导入）：它照样是一条依赖（HX301 要算），
    但不会在导入期执行，因此不参与"循环依赖"判定（HX302 不算）—— 见 `find_cycles`。
    """
    out: List[ImportRef] = []

    def visit(node: ast.AST, deferred: bool) -> None:
        for child in ast.iter_child_nodes(node):
            # 进函数体就算延迟导入；类体不算（类体 import 在导入期执行）
            inner = deferred or isinstance(
                child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)
            )
            if isinstance(child, ast.Import):
                for alias in child.names:
                    out.append(
                        ImportRef(
                            file=rel_path,
                            lineno=child.lineno,
                            column=child.col_offset + 1,
                            source_line=_line(source_lines, child.lineno),
                            module=alias.name,
                            level=0,
                            names=[alias.name],
                            is_from=False,
                            deferred=inner,
                        )
                    )
            elif isinstance(child, ast.ImportFrom):
                out.append(
                    ImportRef(
                        file=rel_path,
                        lineno=child.lineno,
                        column=child.col_offset + 1,
                        source_line=_line(source_lines, child.lineno),
                        module=child.module or "",
                        level=child.level or 0,
                        names=[a.name for a in child.names],
                        is_from=True,
                        deferred=inner,
                    )
                )
            visit(child, inner)

    visit(tree, False)
    out.sort(key=lambda r: (r.lineno, r.column))
    return out


def _line(lines: List[str], lineno: int) -> str:
    if 1 <= lineno <= len(lines):
        return lines[lineno - 1]
    return ""


def package_dir_parts(rel_path: str) -> List[str]:
    """文件所在**包**的目录片段（相对 lint 根）。

    `pkg/__init__.py` → `["pkg"]`（`__init__.py` 所在目录就是包本身）
    `pkg/sub/mod.py`  → `["pkg", "sub"]`
    `top.py`          → `[]`（顶层模块不属于任何包）
    """
    parts = rel_path.replace("\\", "/").split("/")
    return parts[:-1]


def relative_module_path(rel_path: str, level: int, module: str) -> Optional[str]:
    """相对导入 → 相对仓库根的点分模块名（解析不到返回 None）。

    相对导入的基准是**包**：`level` 个点里，**第一个点指当前包**，
    之后每个点再往上一层（CPython: `level=1` = 当前包，`level=2` = 父包）。

    `pkg/sub/mod.py` 里 `from ..x import y` → level=2 → 包 `pkg.sub` 上一级 = `pkg`
                                      → `pkg.x` → 解析到 `pkg/x.py` 或 `pkg/x/__init__.py`

    旧实现忽略 level 与包上下文，直接拿 `module` 从仓库根找 —— 那是"绝对导入"的算法，
    套到相对导入上就会把 `pkg/__init__.py` 里的 `from .pkg import x` 解析成包自己（假自环）。
    """
    pkg = package_dir_parts(rel_path)
    up = max(0, level - 1)
    if up > len(pkg):
        return None  # 越出仓库根，不猜
    base = pkg[: len(pkg) - up]
    tail = [p for p in module.split(".") if p] if module else []
    parts = base + tail
    return ".".join(parts) or None


def module_to_file(root: str, dotted: str) -> Optional[str]:
    """把点分模块名映射到仓库内的文件（相对路径）。解析不到返回 None。"""
    if not dotted:
        return None
    parts = dotted.split(".")
    cand = os.path.join(root, *parts)
    for path in (cand + ".py", os.path.join(cand, "__init__.py")):
        if os.path.isfile(path):
            return os.path.relpath(path, root).replace("\\", "/")
    return None


def build_graph(root: str, files: List[Dict], cfg: Config) -> Graph:
    """files: [{"rel": str, "tree": ast, "lines": [str]}]"""
    g = Graph()
    for f in files:
        refs = iter_imports(f["rel"], f["tree"], f["lines"])
        g.imports.extend(refs)
        for ref in refs:
            if ref.level > 0:
                # 相对导入：基准是**包**（要带 level 与文件所在包一起算），
                # 不是"拿 module 从根目录找" —— 后者会把包内重导出解析成包自己（假自环）。
                dotted = relative_module_path(f["rel"], ref.level, ref.module)
                target = module_to_file(root, dotted) if dotted else None
            else:
                target = module_to_file(root, ref.module)
            if not target:
                g.unresolved.append(ref)
                continue
            if target == f["rel"]:
                # 自环不是依赖：`mod.py` 里的 `import mod` 解析成自己，既不是反向依赖也不是环。
                # 旧实现会把它报成 `循环依赖：x → x`。
                g.self_edges.append(ref)
                continue
            g.edges.append(
                Edge(src_file=f["rel"], dst_file=target, lineno=ref.lineno, deferred=ref.deferred)
            )
    return g
