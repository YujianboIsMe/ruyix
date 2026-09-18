"""flake8 插件薄包装。

这一层存在的意义：证明这些规则**不是 harness 私有的一次性脚本**，而是能进入真实
Python 生态的自定义 linter —— 任何项目 `pip install` 之后 `flake8 --select=HX` 就能用。

刻意只跑**文件级规则**：flake8 一次只喂一个文件的 AST，跨文件的 HX301 / HX302
（分层依赖、循环依赖）在这里没有上下文，硬凑会给出错误的判定，所以留给 `harness-lint`
命令跑。宁可这里少报，也不要在编辑器里刷出假告警。
"""

from __future__ import annotations

import os
from typing import Iterator, Tuple

from . import __version__
from .config import load as load_config
from .engine import FileContext, is_test_file, parse_directive
from .rules import ALL_RULES


def _suppressed(lines, rule_id: str, first: int, last: int) -> bool:
    """该诊断所在行（含跨行范围）上是否有针对它的 noqa / disable-file。"""
    for lineno in range(first, max(first, last) + 1):
        if 1 <= lineno <= len(lines):
            directive = parse_directive(lines[lineno - 1])
            if directive and (not directive["codes"] or rule_id in directive["codes"]):
                return True
    return False


class HarnessLintPlugin:
    """flake8 AST 插件协议：`__init__(self, tree, filename)` + `run()` 产出 4 元组。

    flake8 按**参数名**注入实参，所以 `tree` / `filename` 两个名字必须照写。
    """

    name = "harness-lint"
    version = __version__
    off_by_default = False

    def __init__(self, tree, filename):
        self.tree = tree
        self.filename = filename

    def run(self) -> Iterator[Tuple[int, int, str, type]]:
        try:
            with open(self.filename, encoding="utf-8") as f:
                source = f.read()
        except (OSError, UnicodeDecodeError):
            return

        # 以"当前工作目录"为根：flake8 的调用约定就是从项目根跑
        root = os.getcwd()
        rel = os.path.relpath(os.path.abspath(self.filename), root).replace("\\", "/")
        cfg = load_config(root)
        lines = source.splitlines()

        ctx = FileContext(
            root=root,
            rel=rel,
            abspath=os.path.abspath(self.filename),
            source=source,
            lines=lines,
            tree=self.tree,
            config=cfg,
            layer=cfg.layer_of(rel),
            is_test=is_test_file(rel, cfg),
        )

        for rule in ALL_RULES:
            if cfg.is_ignored_rule(rule.id):
                continue
            try:
                produced = list(rule.check(ctx) or [])
            except Exception:  # noqa: BLE001 —— 插件里不能让一条规则崩掉整个 flake8
                continue
            for d in produced:
                last = d.span.line_end if d.span else d.line
                if _suppressed(lines, d.rule, d.line, last or d.line):
                    continue
                col = max(0, (d.span.column_start if d.span else 1) - 1)
                # 消息必须以规则 ID 开头：flake8 从文本头部解析 code
                yield (d.line, col, f"{d.rule} {d.message}", type(self))
