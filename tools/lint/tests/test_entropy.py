"""熵度量层的测试。

重点不在"能算出数"，而在三件容易悄悄错的事：
1. **口径**：密度用代码行（排除空行/纯注释），不是总行数；
2. **防作弊**：加违规、加豁免、删测试都不能让分数变高；
3. **归属与识别**：别的工具的 `noqa` 不算我们的豁免；注释里"引述"语法不是真指令。

第 3 条是真踩出来的三个 bug（见 `doc/开发约定与坑.md`），所以这里逐条钉死。
"""

from __future__ import annotations

import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lint_testlib import TempProject  # noqa: E402

from harness_lint import engine, entropy  # noqa: E402
from harness_lint.config import load as load_config  # noqa: E402

TS = "2026-01-01T00:00:00+08:00"


def full_of(proj):
    cfg = load_config(proj.root)
    return engine.run_full(proj.root, cfg, None, None), cfg


def metrics_of(proj) -> entropy.Metrics:
    full, cfg = full_of(proj)
    return entropy.compute(full, cfg, TS)


# 结构相同、名字和常量不同的两个函数 → 应当被判为重复实现
DUP_SRC = '''
def alpha(values):
    total = 0
    for item in values:
        if item > 0:
            total = total + item
        else:
            total = total - item
    return total
'''
DUP_SRC_SAME_SHAPE = '''
def beta(rows):
    acc = 0
    for cell in rows:
        if cell > 0:
            acc = acc + cell
        else:
            acc = acc - cell
    return acc
'''
DIFFERENT_SHAPE = '''
def gamma(rows):
    acc = 0
    while rows:
        cell = rows.pop()
        if cell > 0:
            acc = acc + cell
        else:
            acc = acc - cell
    return acc
'''


class TestCaliber(unittest.TestCase):
    """口径：密度必须用代码行算。"""

    def test_code_lines_exclude_blank_and_comment_only_lines(self):
        proj = TempProject(
            {"a.py": "# 注释\n\nx = 1\n\n# 又一行注释\ny = 2\n", "b.py": "z = 3\n"}
        )
        try:
            m = metrics_of(proj)
            assert m.code_lines == 3, f"代码行应为 3（x/y/z），实际 {m.code_lines}"
            assert m.files == 2, m.files
        finally:
            proj.close()

    def test_density_is_per_thousand_code_lines(self):
        body = "\n".join(f"    v{i} = {i}" for i in range(10))
        proj = TempProject({"m.py": "def f(x=[]):\n" + body + "\n"})
        try:
            m = metrics_of(proj)
            assert m.code_lines == 11, m.code_lines
            assert m.violations_total >= 1, "可变默认参数应当被报出来"
            expected = m.violations_total / (m.code_lines / 1000.0)
            assert abs(m.density - round(expected, 3)) < 0.01, (m.density, expected)
        finally:
            proj.close()

    def test_percentile_handles_empty_and_single(self):
        assert entropy.percentile([], 0.5) == 0.0
        assert entropy.percentile([7], 0.9) == 7.0
        assert entropy.percentile([1, 2, 3, 4], 0.5) == 2.5


class TestScore(unittest.TestCase):
    """评分：越高越好，且**不允许靠作弊提分**。"""

    def base(self) -> entropy.Metrics:
        return entropy.Metrics(ts=TS, code_lines=1000, files=10, test_count=5)

    def test_score_is_clamped_to_0_100(self):
        w = entropy.Weights()
        clean = self.base()
        assert entropy.score_of(clean, w) == 100.0
        awful = entropy.Metrics(ts=TS, code_lines=1000, error_density=9999, density=9999)
        assert entropy.score_of(awful, w) == 0.0

    def test_more_violations_never_raises_score(self):
        w = entropy.Weights()
        prev = None
        for density in (0.0, 1.0, 5.0, 20.0):
            m = self.base()
            m.density = density
            m.error_density = density
            s = entropy.score_of(m, w)
            if prev is not None:
                assert s <= prev, (density, s, prev)
            prev = s

    def test_more_suppressions_never_raises_score(self):
        w = entropy.Weights()
        m = self.base()
        m.suppression_density = 0.0
        s0 = entropy.score_of(m, w)
        m.suppression_density = 10.0
        assert entropy.score_of(m, w) < s0, "豁免多必须扣分，否则加 noqa 就能提分"

    def test_reasonless_suppression_hurts_more_than_reasoned_one(self):
        w = entropy.Weights()
        a = self.base()
        a.suppression_density = 5.0
        a.no_reason_density = 5.0
        b = self.base()
        b.suppression_density = 5.0
        b.no_reason_density = 0.0
        assert entropy.score_of(a, w) < entropy.score_of(b, w)

    def test_zero_tests_is_penalized(self):
        w = entropy.Weights()
        with_tests = self.base()
        without = self.base()
        without.test_count = 0
        assert entropy.score_of(without, w) < entropy.score_of(with_tests, w)

    def test_duplicates_and_placeholders_cost_points(self):
        w = entropy.Weights()
        m = self.base()
        s0 = entropy.score_of(m, w)
        m.duplicate_density = 2.0
        m.placeholder_density = 3.0
        assert entropy.score_of(m, w) < s0

    def test_weights_are_overridable_from_config(self):
        proj = TempProject({"m.py": "def f(x=[]):\n    return x\n"})
        try:
            cfg = load_config(proj.root)
            strict = cfg.entropy_weights()
            assert strict.warning_density == 1.0, strict
            cfg.entropy_weight_overrides = {"warning_density": 100.0}
            tuned = cfg.entropy_weights()
            assert tuned.warning_density == 100.0, tuned
            full = engine.run_full(proj.root, cfg, None, None)
            assert entropy.score_of(entropy.compute(full, cfg, TS), tuned) == 0.0
        finally:
            proj.close()


class TestDuplicates(unittest.TestCase):
    """重复实现：理论上"冗余工具函数"的可度量代理。"""

    def test_same_shape_different_names_is_a_duplicate(self):
        proj = TempProject({"a.py": DUP_SRC, "b.py": DUP_SRC_SAME_SHAPE})
        try:
            m = metrics_of(proj)
            assert m.duplicate_groups == 1, m.duplicate_groups
            assert m.duplicate_functions == 2, m.duplicate_functions
            assert m.duplicate_lines > 0, m.duplicate_lines
        finally:
            proj.close()

    def test_different_shape_is_not_a_duplicate(self):
        proj = TempProject({"a.py": DUP_SRC, "b.py": DIFFERENT_SHAPE})
        try:
            m = metrics_of(proj)
            assert m.duplicate_groups == 0, m.duplicate_groups
        finally:
            proj.close()

    def test_one_off_function_is_not_a_duplicate(self):
        proj = TempProject({"a.py": DUP_SRC})
        try:
            assert metrics_of(proj).duplicate_groups == 0
        finally:
            proj.close()

    def test_tiny_bodies_are_ignored(self):
        # 两行的小函数到处都是形状相同的，报出来只会是噪声
        proj = TempProject({"a.py": "def f(x):\n    return x\n", "b.py": "def g(y):\n    return y\n"})
        try:
            assert metrics_of(proj).duplicate_groups == 0
        finally:
            proj.close()


class TestSuppressionOwnership(unittest.TestCase):
    """`# noqa` 是全生态共享命名空间：别的工具的 code 不算我们的熵。"""

    def test_only_hx_or_bare_directives_are_ours(self):
        assert not entropy.is_our_suppression({"codes": ["F401"]})
        assert not entropy.is_our_suppression({"codes": ["BLE001", "E402"]})
        assert not entropy.is_our_suppression({"codes": ["N802"]})
        assert entropy.is_our_suppression({"codes": ["HX201"]})
        assert entropy.is_our_suppression({"codes": []}), "无 codes 的全行豁免算我们的"

    def test_foreign_noqa_is_not_counted_as_our_suppression(self):
        proj = TempProject(
            {
                "m.py": "import os  # noqa: F401\nimport sys  # noqa: E402\n"
                "raise SystemExit  # noqa: HX105 reason=演示\n"
            }
        )
        try:
            m = metrics_of(proj)
            assert m.suppressions_total == 1, (
                f"我们自己的豁免应当只有 1 条，实际 {m.suppressions_total}"
            )
        finally:
            proj.close()

    def test_comma_separated_codes_are_parsed(self):
        proj = TempProject({"m.py": "import os  # noqa: F401,E402\n"})
        try:
            m = metrics_of(proj)
            assert m.suppressions_total == 0, "F401,E402 是别的工具的 code，不该算我们的"
        finally:
            proj.close()

    def test_prose_quoting_the_syntax_is_not_a_directive(self):
        # 注释里"引述"语法（反引号包起来）不是真指令 —— 否则那行上的真诊断会被静默压掉
        proj = TempProject(
            {"m.py": "# 用法：`# noqa: HX201 reason=<原因>` 写在违规那一行\nx = 1\n"}
        )
        try:
            m = metrics_of(proj)
            assert m.suppressions_total == 0, f"引述不算指令，实际 {m.suppressions_total}"
        finally:
            proj.close()


class TestHistory(unittest.TestCase):
    """时间序列：记不下、比不了，趋势就无从谈起。"""

    def setUp(self):
        self.dir = os.path.join(
            os.environ.get("TEMP", "/tmp"), f"ent-hist-{os.getpid()}-{id(self)}"
        )
        os.makedirs(self.dir, exist_ok=True)
        self.path = os.path.join(self.dir, "history.jsonl")

    def tearDown(self):
        for name in os.listdir(self.dir):
            os.remove(os.path.join(self.dir, name))
        os.rmdir(self.dir)

    def snap(self, score: float, density: float) -> entropy.Metrics:
        return entropy.Metrics(ts=TS, score=score, density=density, code_lines=1000)

    def test_append_and_load_roundtrip(self):
        entropy.append_history(self.path, self.snap(90, 1), 10)
        entropy.append_history(self.path, self.snap(80, 2), 10)
        hist = entropy.load_history(self.path)
        assert [h.score for h in hist] == [90, 80], hist
        assert hist[0].density == 1, hist[0]

    def test_max_keep_trims_oldest(self):
        for i in range(5):
            entropy.append_history(self.path, self.snap(100 - i, 1), 3)
        hist = entropy.load_history(self.path)
        assert len(hist) == 3, len(hist)
        assert [h.score for h in hist] == [98, 97, 96], [h.score for h in hist]

    def test_bad_lines_do_not_break_the_series(self):
        entropy.append_history(self.path, self.snap(90, 1), 0)
        with open(self.path, "a", encoding="utf-8") as fh:
            fh.write("{ 这不是 json\n\n")
        entropy.append_history(self.path, self.snap(85, 1.5), 0)
        hist = entropy.load_history(self.path)
        assert [h.score for h in hist] == [90, 85], hist

    def test_unknown_keys_are_tolerated(self):
        with open(self.path, "w", encoding="utf-8") as fh:
            fh.write(json.dumps({"ts": TS, "score": 77, "future_field": 1}) + "\n")
        hist = entropy.load_history(self.path)
        assert hist[0].score == 77, hist[0]

    def test_reference_is_median_of_window(self):
        hist = [self.snap(s, 1) for s in (100, 60, 90)]
        ref = entropy.reference_of(hist, 3)
        assert ref is not None
        assert ref.score == 90, f"中位数应为 90（100/60/90），实际 {ref.score}"

    def test_reference_of_empty_history_is_none(self):
        assert entropy.reference_of([], 3) is None

    def test_reference_window_limits_lookback(self):
        hist = [self.snap(10, 1), self.snap(100, 1), self.snap(100, 1), self.snap(100, 1)]
        assert entropy.reference_of(hist, 3).score == 100, "窗口取近 3 条，应忽略最旧的 10"
