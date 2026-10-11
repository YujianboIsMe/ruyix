"""引擎、反规避与输出契约的测试。

反规避（HX900/901/902/903）是'防止 Agent 让检查闭嘴'的闸门，必须单独测。
"""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lint_testlib import MINI_CONFIG, RuleTestCase, TempProject  # noqa: F401,E402

import json  # noqa: E402

from harness_lint.config import load as load_config  # noqa: E402
from harness_lint.engine import make_baseline, run  # noqa: E402
from harness_lint.render import render_report, report_to_json  # noqa: E402


class TestAntiEvasion(RuleTestCase):
    def test_hx900_noqa_without_reason(self):
        self.check({"m.py": "def f():\n    print(1)  # noqa: HX104\n"}, ["HX900"])

    def test_hx900_negative_with_reason(self):
        self.check(
            {"m.py": "def f():\n    print(1)  # noqa: HX104 reason=这是示例代码\n"},
            [],
            ["HX900"],
        )

    def test_hx900_only_asks_our_own_directives_for_a_reason(self):
        # 别的工具的 noqa（F401/E402/BLE001）不归我们管：要求它们写理由只会制造噪声，
        # 而噪声会让整条反馈链被忽略。口径必须与熵度量层一致。
        self.check(
            {"m.py": "import os  # noqa: F401\nimport sys  # noqa: E402\n"},
            [],
            ["HX900"],
        )

    def test_hx900_still_fires_for_our_bare_directives(self):
        # 我们自己的豁免（HX 前缀 or 不带 codes）依然必须写理由
        self.check({"m.py": "x = 1  # noqa: HX201\n"}, ["HX900"])

    def test_hx903_unknown_code(self):
        self.check({"m.py": "x = 1  # noqa: HX999 reason=拼错了\n"}, ["HX903"])

    def test_hx901_budget_exceeded(self):
        src = "def f():\n    print(1)  # noqa: HX104 reason=示例\n"
        proj = TempProject({"m.py": src})
        try:
            report = proj.lint(max_suppressions=0)
            self.assertIn("HX901", [d.rule for d in report.diagnostics])
        finally:
            proj.close()

    def test_hx901_negative_when_budget_allows(self):
        src = "def f():\n    print(1)  # noqa: HX104 reason=示例\n"
        proj = TempProject({"m.py": src})
        try:
            report = proj.lint(max_suppressions=5)
            self.assertNotIn("HX901", [d.rule for d in report.diagnostics])
        finally:
            proj.close()

    def test_hx902_test_count_decreased(self):
        proj = TempProject({"test_m.py": "def test_a():\n    assert 1\n"})
        try:
            baseline = {"by_rule": {}, "test_count": 3, "suppressions": 0}
            report = proj.lint(baseline=baseline)
            self.assertIn("HX902", [d.rule for d in report.diagnostics])
        finally:
            proj.close()

    def test_suppression_same_line_and_line_above(self):
        same_line = "def f():\n    print(1)  # noqa: HX104 reason=示例\n"
        line_above = "def f():\n    # noqa: HX104 reason=示例\n    print(1)\n"
        for label, src in (("同行", same_line), ("上一行", line_above)):
            with self.subTest(位置=label):
                self.check({"m.py": src}, [], ["HX104"])

    def test_noqa_on_blank_separated_line_does_not_suppress(self):
        # 中间隔了非注释行就不该生效，否则"随便找个地方写 noqa"就能关检查
        src = "def f():\n    # noqa: HX104 reason=示例\n    value = 1\n    print(value)\n"
        self.check({"m.py": src}, ["HX104"])


class TestEngineRobustness(RuleTestCase):
    def test_broken_rule_surfaces_hx998_not_silent(self):
        from harness_lint import engine as engine_mod  # noqa: F401
        from harness_lint import rules as rules_mod

        class Exploding:
            id = "HX777"
            name = "exploding"
            level = "warning"
            summary = "故意抛异常"
            doc = ""
            fixable = False
            fix_blocked_reason = None
            requires_ast = False

            def check(self, ctx):
                raise RuntimeError("boom")

        saved = rules_mod.ALL_RULES
        rules_mod.ALL_RULES = list(saved) + [Exploding()]
        try:
            report = self.check({"m.py": "x = 1\n"}, ["HX998"])
            self.assertTrue(any("boom" in str(c.message) for d in report.diagnostics if d.rule == "HX998" for c in d.children))
        finally:
            rules_mod.ALL_RULES = saved
            _ = engine_mod

    def test_syntax_error_reported_as_hx000(self):
        self.check({"m.py": "def f(:\n    pass\n"}, ["HX000"])

    def test_ok_project_has_no_diagnostics(self):
        """误报回归：一个写得好好的小项目，不该有任何诊断。"""
        good = {
            "util/calc.py": (
                "def add(a, b):\n"
                "    return a + b\n"
                "\n"
                "def safe_div(a, b):\n"
                "    if b == 0:\n"
                "        raise ZeroDivisionError('division by zero')\n"
                "    return a / b\n"
            ),
            "test_calc.py": (
                "import unittest\n"
                "from util.calc import add, safe_div\n"
                "\n"
                "class TestCalc(unittest.TestCase):\n"
                "    def test_add(self):\n"
                "        self.assertEqual(add(2, 3), 5)\n"
                "\n"
                "    def test_safe_div_zero(self):\n"
                "        with self.assertRaises(ZeroDivisionError):\n"
                "            safe_div(1, 0)\n"
            ),
        }
        report = self.check(good, [], ["HX101", "HX102", "HX103", "HX104", "HX105", "HX201", "HX202", "HX203", "HX204"])
        self.assertEqual(report.diagnostics, [], f"好代码不该被报：{[d.rule for d in report.diagnostics]}")
        self.assertTrue(report.is_ok())


class TestOutputContract(RuleTestCase):
    def test_json_shape_matches_rustc_diagnostic(self):
        proj = TempProject({"m.py": "def f(x):\n    if x == None:\n        return 1\n"})
        try:
            cfg = load_config(proj.root)
            report, _ = run(proj.root, cfg)
            data = report_to_json(report)
            diag = next(d for d in data["diagnostics"] if d["rule"] == "HX204")
            # rustc 兼容字段
            for key in ("message", "code", "level", "spans", "children"):
                self.assertIn(key, diag)
            self.assertEqual(diag["code"]["code"], "HX204")
            span = diag["spans"][0]
            for key in ("file_name", "line_start", "column_start", "suggested_replacement", "suggestion_applicability"):
                self.assertIn(key, span)
            # 扩展字段
            self.assertIn("rule", diag)
            self.assertIn("fix_blocked_reason", diag)
            self.assertTrue(diag["doc_resolved"], "doc 必须解析到真实文件路径")
        finally:
            proj.close()

    def test_text_render_contains_four_parts(self):
        proj = TempProject({"m.py": "def f(x):\n    if x == None:\n        return 1\n"})
        try:
            cfg = load_config(proj.root)
            report, _ = run(proj.root, cfg)
            text = render_report(report)
            for part in ("为什么：", "怎么改：", "参考：", "自动修复：", "豁免："):
                self.assertIn(part, text)
        finally:
            proj.close()

    def test_baseline_plan_mode(self):
        proj = TempProject({"m.py": "# TODO: 待办\ndef f():\n    ...\n"})
        try:
            report = proj.lint(baseline={"by_rule": {"HX105": 2}, "test_count": 0, "suppressions": 0})
            self.assertTrue(report.is_ok(), "存量违规不该判失败")
            self.assertEqual(report.new_by_rule, {})
        finally:
            proj.close()

    def test_write_baseline_shape(self):
        proj = TempProject({"m.py": "# TODO: 待办\n"})
        try:
            base = make_baseline(proj.lint())
            self.assertEqual(json.loads(json.dumps(base))["by_rule"], {"HX105": 1})
            self.assertIn("test_count", base)
        finally:
            proj.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
