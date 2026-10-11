"""文件发现与加载：走目录、读文件、解析 AST、识别测试文件、承载上下文对象。

从 engine.py 拆出来有两个理由：
1. 满足 linter 自己的 HX101（单文件行数上限）——"规则也管作者的代码"；
2. 边界更清楚：本模块只负责"把源码变成可检查的上下文 + 报告读不到/解析不了
   （HX000）"，不参与任何规则判定。
"""

from __future__ import annotations

import ast
import fnmatch
import os
from dataclasses import dataclass, field
from typing import List, Optional

from .config import Config
from .diagnostic import Diagnostic, Span

SKIP_DIRS = {
    ".git",
    "__pycache__",
    ".venv",
    "venv",
    "env",
    "node_modules",
    ".harness-target",
    "target",
    "build",
    "dist",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "site-packages",
    ".tox",
    ".runs",
    ".idea",
    ".vscode",
}


@dataclass
class FileContext:
    root: str
    rel: str
    abspath: str
    source: str
    lines: List[str]
    tree: Optional[ast.AST]
    config: Config
    layer: str
    is_test: bool

    def span(
        self,
        lineno: int,
        column: int = 1,
        end_lineno: Optional[int] = None,
        end_column: Optional[int] = None,
        label: str = "",
        context_lines: int = 1,
    ) -> Span:
        """构造 Span，并自动带上用于渲染的源码行（含前后各 context_lines 行）。"""
        end_lineno = end_lineno or lineno
        lo = max(1, lineno - context_lines)
        hi = min(len(self.lines), end_lineno + context_lines)
        text = self.lines[lo - 1 : hi]
        return Span(
            file=self.rel,
            line_start=lineno,
            column_start=column,
            line_end=end_lineno,
            column_end=end_column,
            label=label,
            text=text,
            text_start=lo,
        )

    def head_text(self, lineno: int, n: int = 1) -> List[str]:
        return self.lines[lineno - 1 : lineno - 1 + n]


@dataclass
class ProjectContext:
    root: str
    config: Config
    files: List[FileContext] = field(default_factory=list)
    graph: object = None
    conventions_dir: str = ""

    def by_layer(self, layer: str) -> List[FileContext]:
        return [f for f in self.files if f.layer == layer]


@dataclass
class LoadedFile:
    """一个文件的加载结果：要么有 context，要么有诊断（读不了 / 解析不了）。"""

    context: Optional[FileContext] = None
    ast_entry: Optional[dict] = None
    diagnostics: List[Diagnostic] = field(default_factory=list)


def is_test_file(rel: str, cfg: Config) -> bool:
    rel_norm = rel.replace("\\", "/")
    for pat in cfg.test_file_patterns:
        if fnmatch.fnmatch(rel_norm, pat) or fnmatch.fnmatch(os.path.basename(rel_norm), pat):
            return True
    base = os.path.basename(rel_norm)
    return base.startswith("test_") or base.endswith("_test.py")


def iter_py_files(root: str, cfg: Config) -> List[str]:
    out: List[str] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS and not d.startswith(".harness")]
        for fn in filenames:
            if not fn.endswith(".py"):
                continue
            rel = os.path.relpath(os.path.join(dirpath, fn), root).replace("\\", "/")
            if cfg.is_ignored_path(rel):
                continue
            out.append(rel)
    out.sort()
    return out


def count_tests(tree: Optional[ast.AST]) -> int:
    if tree is None:
        return 0
    return sum(
        1
        for node in ast.walk(tree)
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name.startswith("test_")
    )


def load_source(root: str, rel: str, cfg: Config) -> LoadedFile:
    """读文件 + 解析 AST。读不了/解析不了的时候产出 HX000 诊断（绝不静默跳过）。"""
    abspath = os.path.join(root, rel)
    try:
        with open(abspath, encoding="utf-8") as fh:
            source = fh.read()
    except (OSError, UnicodeDecodeError) as e:
        diag = (
            Diagnostic(
                rule="HX000",
                level="error",
                message=f"无法读取文件：{e}",
                span=Span(file=rel, line_start=1, text=[]),
                fix_blocked_reason="文件不可读，需人工检查编码/权限",
            )
            .with_why("读不到的文件等于没被检查过；静默跳过会给出假的绿灯。")
            .with_fix("确认文件是 UTF-8 编码且可读")
        )
        return LoadedFile(diagnostics=[diag])

    lines = source.splitlines()
    try:
        tree = ast.parse(source, filename=rel)
    except SyntaxError as e:
        diag = (
            Diagnostic(
                rule="HX000",
                level="error",
                message=f"语法错误，无法解析：{e.msg}",
                span=Span(
                    file=rel,
                    line_start=e.lineno or 1,
                    column_start=e.offset or 1,
                    text=[lines[(e.lineno or 1) - 1]] if lines else [],
                    label="解析失败的位置",
                ),
                doc="（由 Python 解析器给出）",
                fix_blocked_reason="语法错误没有通用改法，必须看具体报错",
            )
            .with_why(f"解析器报错：{e.msg}（第 {e.lineno or '?'} 行）")
            .with_fix("先把这个语法错误改掉；改完后 lint 才能继续检查其余规则")
        )
        return LoadedFile(diagnostics=[diag])

    return LoadedFile(
        context=FileContext(
            root=root,
            rel=rel,
            abspath=abspath,
            source=source,
            lines=lines,
            tree=tree,
            config=cfg,
            layer=cfg.layer_of(rel),
            is_test=is_test_file(rel, cfg),
        ),
        ast_entry={"rel": rel, "tree": tree, "lines": lines},
    )
