"""测试公共设施：临时项目、断言辅助。

**每条规则都要有正例 + 反例**：只测"能报出来"是不够的，误报率才是这类工具的生命线
（一次误报就会让用户学会忽略所有告警）。反例部分就是误报回归。

跑法（零依赖，标准库就够）：
    python -m unittest discover -s tests -v
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from harness_lint.config import load as load_config  # noqa: E402
from harness_lint.engine import run  # noqa: E402
from harness_lint.render import render_report, report_to_json  # noqa: E402

MINI_CONFIG = """
[lint]
max_file_lines = 20
max_function_lines = 8
"""


class TempProject:
    """临时项目：写入文件、跑 lint、取规则列表。"""

    def __init__(self, files: dict, config: str = MINI_CONFIG):
        self.root = tempfile.mkdtemp(prefix="harness-lint-test-")
        for rel, content in files.items():
            path = os.path.join(self.root, rel)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w", encoding="utf-8") as f:
                f.write(content)
        if config:
            with open(os.path.join(self.root, ".harnesslint.toml"), "w", encoding="utf-8") as f:
                f.write(config)

    def lint(self, baseline=None, max_suppressions=None):
        cfg = load_config(self.root)
        report, _ = run(self.root, cfg, baseline, max_suppressions)
        return report

    def rules(self, **kw):
        return sorted({d.rule for d in self.lint(**kw).diagnostics})

    def close(self):
        shutil.rmtree(self.root, ignore_errors=True)


class RuleTestCase(unittest.TestCase):
    def check(self, files: dict, expected: list, unexpected: list = None, config=MINI_CONFIG):
        proj = TempProject(files, config)
        try:
            found = proj.rules()
            for rule in expected:
                self.assertIn(rule, found, f"应报出 {rule}，实际 {found}")
            for rule in unexpected or []:
                self.assertNotIn(rule, found, f"不该报 {rule}（误报），实际 {found}")
            return proj.lint()
        finally:
            proj.close()
