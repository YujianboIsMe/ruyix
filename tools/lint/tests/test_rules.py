"""逐条规则的测试：每条规则都要有正例 + 反例。

反例部分就是**误报回归**——只测'能报出来'是不够的，误报率才是这类工具的生命线。
"""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lint_testlib import MINI_CONFIG, RuleTestCase, TempProject  # noqa: F401,E402
from harness_lint.config import load as load_config  # noqa: E402
from harness_lint.engine import run  # noqa: E402

class TestStructure(RuleTestCase):
    def test_hx101_positive(self):
        body = "\n".join(f"value_{i} = {i}" for i in range(30))
        self.check({"big.py": body + "\n"}, ["HX101"])

    def test_hx101_negative_small_file(self):
        self.check({"small.py": "x = 1\ny = 2\n"}, [], ["HX101"])

    def test_hx102_positive(self):
        lines = ["def long_one():"] + [f"    a{i} = {i}" for i in range(12)] + ["    return a0"]
        self.check({"m.py": "\n".join(lines) + "\n"}, ["HX102"])

    def test_hx102_negative_short_function(self):
        self.check({"m.py": "def f():\n    return 1\n"}, [], ["HX102"])


class TestTesting(RuleTestCase):
    def test_hx103_positive_no_assert(self):
        src = "def test_thing():\n    value = 1\n"
        self.check({"test_m.py": src}, ["HX103"])

    def test_hx103_positive_empty_stub(self):
        self.check({"test_m.py": "def test_thing():\n    pass\n"}, ["HX103"])

    def test_hx103_negative_assert_styles(self):
        for body in (
            "    assert f() == 1\n",
            "    self.assertEqual(f(), 1)\n",
            "    with self.assertRaises(ValueError):\n        f()\n",
        ):
            with self.subTest(body=body.strip()):
                self.check({"test_m.py": "def test_thing():\n" + body}, [], ["HX103"])

    def test_hx103_negative_non_test_function(self):
        self.check({"helper.py": "def compute():\n    return 1\n"}, [], ["HX103"])


class TestHygiene(RuleTestCase):
    def test_hx104_positive(self):
        self.check({"m.py": "def f():\n    print(1)\n"}, ["HX104"])

    def test_hx104_negative_main_guard_and_main_func(self):
        src = (
            "def main():\n"
            "    print('ok')\n"
            "\n"
            "if __name__ == '__main__':\n"
            "    print('starting')\n"
        )
        self.check({"m.py": src}, [], ["HX104"])

    def test_hx104_suggestion_is_minimal_replacement(self):
        proj = TempProject({"m.py": "def f():\n    print(1)\n"})
        try:
            cfg = load_config(proj.root)
            report, _ = run(proj.root, cfg)
            diag = next(d for d in report.diagnostics if d.rule == "HX104")
            self.assertEqual(diag.span.suggested_replacement, "logger.debug(")
            self.assertEqual(diag.span.suggestion_applicability, "MaybeIncorrect")
            # 替换必须是"最小外科式"：只覆盖 print( 这一段
            line = diag.span.text[diag.span.line_start - diag.span.text_start]
            a, b = diag.span.column_start - 1, diag.span.column_end - 1
            self.assertEqual(line[a:b], "print(")
        finally:
            proj.close()

    def test_hx105_positive_todo_and_stub(self):
        src = "# TODO: 待办\ndef f():\n    ...\n"
        self.check({"m.py": src}, ["HX105"])

    def test_hx105_negative_abstractmethod_and_notimplemented(self):
        src = (
            "import abc\n"
            "\n"
            "class B(abc.ABC):\n"
            "    @abc.abstractmethod\n"
            "    def run(self):\n"
            "        ...\n"
            "\n"
            "def todo_later():\n"
            "    raise NotImplementedError('see ISSUE-1')\n"
        )
        self.check({"m.py": src}, [], ["HX105"])


class TestCorrectness(RuleTestCase):
    def test_hx201_positive_bare_except(self):
        src = "def f():\n    try:\n        pass\n    except:\n        pass\n"
        self.check({"m.py": src}, ["HX201"])

    def test_hx201_positive_swallow(self):
        src = "def f():\n    try:\n        pass\n    except Exception:\n        pass\n"
        self.check({"m.py": src}, ["HX201"])

    def test_hx201_negative_typed_except_with_logging(self):
        src = (
            "import logging\n"
            "\n"
            "def f():\n"
            "    try:\n"
            "        pass\n"
            "    except (ValueError, KeyError):\n"
            "        logging.warning('skip')\n"
        )
        self.check({"m.py": src}, [], ["HX201"])

    def test_hx201_negative_except_exception_that_rethrows(self):
        src = "def f():\n    try:\n        pass\n    except Exception as e:\n        raise RuntimeError('x') from e\n"
        self.check({"m.py": src}, [], ["HX201"])

    def test_hx202_positive_token_prefix(self):
        self.check({"m.py": 'API_KEY = "sk-f5f78989471f49af8855cc019c63e230"\n'}, ["HX202"])

    def test_hx202_negative_placeholders_and_env(self):
        src = (
            "import os\n"
            "\n"
            "API_KEY = os.environ['API_KEY']\n"
            'PASSWORD = "your-password-here"\n'
            'TOKEN = "changeme"\n'
            'SECRET = "<填入>"\n'
            'DEBUG_SECRET = "example-secret-value"\n'
        )
        self.check({"m.py": src}, [], ["HX202"])

    def test_hx202_mask_does_not_leak_value(self):
        proj = TempProject({"m.py": 'API_KEY = "sk-f5f78989471f49af8855cc019c63e230"\n'})
        try:
            cfg = load_config(proj.root)
            report, _ = run(proj.root, cfg)
            diag = next(d for d in report.diagnostics if d.rule == "HX202")
            self.assertNotIn("f5f78989471f49af8855cc019c63e230", diag.message)
        finally:
            proj.close()

    def test_hx203_positive(self):
        self.check({"m.py": "def f(items=[]):\n    return items\n"}, ["HX203"])

    def test_hx203_negative_none_and_immutable(self):
        src = "def f(items=None, name='x', pair=(1, 2)):\n    return items\n"
        self.check({"m.py": src}, [], ["HX203"])

    def test_hx204_positive_machine_applicable(self):
        proj = TempProject({"m.py": "def f(x):\n    if x == None:\n        return 1\n"})
        try:
            cfg = load_config(proj.root)
            report, _ = run(proj.root, cfg)
            diag = next(d for d in report.diagnostics if d.rule == "HX204")
            self.assertEqual(diag.span.suggested_replacement, "is None")
            self.assertEqual(diag.span.suggestion_applicability, "MachineApplicable")
            self.assertTrue(diag.fixable)
            # 按列区间重建出的结果必须真的对（这是"可自动应用"的实质承诺）
            line = diag.span.text[diag.span.line_start - diag.span.text_start]
            a, b = diag.span.column_start - 1, diag.span.column_end - 1
            rebuilt = line[:a] + diag.span.suggested_replacement + line[b:]
            self.assertEqual(rebuilt, "    if x is None:")
        finally:
            proj.close()

    def test_hx204_negative_is_none(self):
        self.check({"m.py": "def f(x):\n    if x is None:\n        return 1\n"}, [], ["HX204"])

    def test_hx204_negative_multi_operator(self):
        self.check({"m.py": "def f(x):\n    if x == None != 1:\n        return 1\n"}, [], ["HX203"])
