"""跨文件规则（HX301 分层方向 / HX302 循环依赖）的测试。

从 `test_rules.py` 拆出来：那两条规则要看整份项目的 import 图，
测试用例数、结构都跟"单文件规则"不是一类东西；而且 `test_rules.py` 已经撞上
HX101（单文件行数上限 260）—— 拆开顺带解决，也让"事实来源（graph.py）↔ 判定"这一对
在测试层面同样分得开。

**每条都要有正例 + 反例**：这两条规则的误报代价特别大（误报一次，
用户对整个 linter 的信任会一起崩掉），所以反例（误报回归）比正例还重要。
"""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lint_testlib import MINI_CONFIG, RuleTestCase, TempProject  # noqa: F401,E402

LAYER_CONFIG = MINI_CONFIG + """
[architecture]
layers = ["types", "config", "repo", "service", "runtime", "ui"]
strict = false
"""


class TestLayerDirection(RuleTestCase):
    """HX301：依赖方向必须向下。"""

    def test_positive_reverse_dependency(self):
        self.check(
            {
                "repo/r.py": "from service.s import run\n",
                "service/s.py": "def run():\n    return 1\n",
            },
            ["HX301"],
            config=LAYER_CONFIG,
        )

    def test_negative_downward_dependency(self):
        self.check(
            {
                "service/s.py": "from repo.r import Repo\n",
                "repo/r.py": "class Repo:\n    pass\n",
            },
            [],
            ["HX301"],
            config=LAYER_CONFIG,
        )

    def test_negative_unknown_layer_is_not_reported(self):
        # utils/ 不属于任何层 → 判不出就不报（宁可漏报不误报）
        self.check(
            {
                "utils/u.py": "from service.s import run\n",
                "service/s.py": "def run():\n    return 1\n",
            },
            [],
            ["HX301"],
            config=LAYER_CONFIG,
        )

    def test_strict_mode_flags_skipping(self):
        cfg = LAYER_CONFIG.replace("strict = false", "strict = true")
        self.check(
            {
                "ui/c.py": "from repo.r import Repo\n",
                "repo/r.py": "class Repo:\n    pass\n",
            },
            ["HX301"],
            config=cfg,
        )

    def test_positive_deferred_import_still_counts_as_reverse_dependency(self):
        """函数体内的延迟导入**照样**是方向违规 —— 延迟只解决"导入期"，不改变依赖方向。"""
        self.check(
            {
                "repo/r.py": "def save():\n    from service.s import run\n    return run()\n",
                "service/s.py": "def run():\n    return 1\n",
            },
            ["HX301"],
            ["HX302"],
            config=LAYER_CONFIG,
        )

    def test_negative_package_reexport_points_downward(self):
        """包内 `__init__.py` 重导出：`from .api import x` 是 api/api.py，方向向下 → 不报。"""
        self.check(
            {
                "api/__init__.py": 'from .api import create_user\n\n__all__ = ["create_user"]\n',
                "api/api.py": (
                    "from service.service import UserService\n\n\n"
                    "def create_user(name):\n"
                    "    return UserService().create(name)\n"
                ),
                "service/service.py": (
                    "class UserService:\n"
                    "    def create(self, name):\n"
                    "        return name\n"
                ),
            },
            [],
            ["HX301", "HX302"],
            config=LAYER_CONFIG,
        )


class TestCircularImport(RuleTestCase):
    """HX302：循环依赖只对**模块级**导入成环（延迟导入已把导入期问题解掉）。"""

    def test_positive_cycle(self):
        self.check(
            {"a.py": "import b\n", "b.py": "import a\n"},
            ["HX302"],
            config=LAYER_CONFIG,
        )

    def test_negative_no_cycle(self):
        self.check(
            {"a.py": "import b\n", "b.py": "value = 1\n"},
            [],
            ["HX302"],
            config=LAYER_CONFIG,
        )

    def test_negative_deferred_import_breaks_the_cycle(self):
        """环上有一条边是函数体内的导入 → 导入期不会出问题，不报。

        这就是本仓库 engine ↔ rules 的写法（engine 在函数内 `from .rules import ...`，
        规则模块在顶层 `from ..engine import Rule`）。旧实现会把这份**已经修好的**代码
        报成 5 条 error。
        """
        self.check(
            {
                "a.py": "value = 1\n\n\ndef use_b():\n    import b\n    return b.value\n",
                "b.py": "import a\n\nvalue = 1\n",
            },
            [],
            ["HX302"],
            config=LAYER_CONFIG,
        )

    def test_negative_package_reexport_is_not_a_self_loop(self):
        """`api/__init__.py` 里 `from .api import x` 指的是 `api/api.py`，不是包自己。

        旧实现忽略相对导入的 level 与包上下文，直接拿 `api` 从仓库根找 → 命中
        `api/__init__.py` → 报 `循环依赖：api/__init__.py → api/__init__.py`。
        三层结构里"包内重导出"是最常见的写法，所以这条误报会成片出现
        （v0.6 的知识库开/关对照实验就是被它污染的）。
        """
        self.check(
            {
                "api/__init__.py": 'from .api import create_user\n\n__all__ = ["create_user"]\n',
                "api/api.py": (
                    "from service.service import UserService\n\n\n"
                    "def create_user(name):\n"
                    "    return UserService().create(name)\n"
                ),
                "service/service.py": (
                    "class UserService:\n"
                    "    def create(self, name):\n"
                    "        return name\n"
                ),
            },
            [],
            ["HX302"],
            config=LAYER_CONFIG,
        )

    def test_positive_real_cycle_through_package_reexport(self):
        """重导出**真的**成环时要报出来，而且报的是两个节点的真环（不是一个自环）。"""
        proj = TempProject(
            {
                "repo/__init__.py": "from .repo import Repo\n",
                # repo/repo.py 又从包里导入 → repo/__init__ → repo/repo → repo/__init__
                "repo/repo.py": "from repo import Repo\n",
            },
            LAYER_CONFIG,
        )
        try:
            diags = [d for d in proj.lint().diagnostics if d.rule == "HX302"]
            self.assertTrue(diags, "真环必须报出来")
            msg = diags[0].message
            self.assertIn("repo/__init__.py", msg)
            self.assertIn("repo/repo.py", msg)
            self.assertNotEqual(
                "循环依赖：repo/__init__.py → repo/__init__.py",
                msg,
                "报成自环说明相对导入又被解析到包自己了",
            )
        finally:
            proj.close()

    def test_negative_same_package_relative_import(self):
        """同一个包内的相对导入（`from .helper import f`）不是环。"""
        self.check(
            {
                "pkg/__init__.py": 'from .helper import f\n\n__all__ = ["f"]\n',
                "pkg/helper.py": "def f():\n    return 1\n",
            },
            [],
            ["HX302"],
            config=LAYER_CONFIG,
        )
