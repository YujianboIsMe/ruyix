"""规则注册表。

新增规则的唯一动作：在自己的模块里写类 → 加进本文件的 RULES / PROJECT_RULES。
引擎会读 `id / level / summary / doc / fixable`，所以类属性必须填全。
"""

from __future__ import annotations

from .architecture import PROJECT_RULES as _ARCH
from .correctness import RULES as _CORRECTNESS
from .hygiene import RULES as _HYGIENE
from .security import RULES as _SECURITY
from .structure import RULES as _STRUCTURE

_FILE_RULE_CLASSES = _STRUCTURE + _HYGIENE + _CORRECTNESS + _SECURITY
_PROJECT_RULE_CLASSES = _ARCH

ALL_RULES = [cls() for cls in _FILE_RULE_CLASSES]
ALL_PROJECT_RULES = [cls() for cls in _PROJECT_RULE_CLASSES]

# 引擎/配置生成的元规则（不是普通规则，单独列出便于文档与 --list-rules 展示）
META_RULES = [
    ("HX000", "parse-error", "文件无法读取或解析（语法错误）"),
    ("HX900", "suppression-without-reason", "豁免未说明理由"),
    ("HX901", "suppression-budget-exceeded", "生效豁免数超过预算"),
    ("HX902", "test-count-decreased", "测试用例数相对基线下降"),
    ("HX903", "unknown-suppression-code", "豁免指向不存在的规则 ID"),
    ("HX998", "internal-rule-error", "规则自身执行失败（linter 的问题）"),
]


def describe_all() -> list:
    rows = []
    for r in ALL_RULES + ALL_PROJECT_RULES:
        rows.append(
            {
                "id": r.id,
                "name": r.name,
                "level": r.level,
                "kind": "project" if hasattr(r, "check_project") else "file",
                "fixable": r.fixable,
                "doc": r.doc,
                "summary": r.summary,
                "fix_blocked_reason": r.fix_blocked_reason,
            }
        )
    for rid, name, summary in META_RULES:
        rows.append(
            {
                "id": rid,
                "name": name,
                "level": "error" if rid in ("HX000", "HX902", "HX903", "HX998") else "warning",
                "kind": "meta",
                "fixable": False,
                "doc": "docs/conventions/suppressions.md",
                "summary": summary,
                "fix_blocked_reason": "",
            }
        )
    return rows
