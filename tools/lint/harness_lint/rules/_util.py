"""规则实现的共用工具。

这里只放"多条规则都要用"的东西，避免规则文件里出现重复的大段 AST 遍历代码。
"""

from __future__ import annotations

import ast
import math
from typing import Iterator, List, Optional, Tuple


def iter_with_parents(tree: ast.AST) -> Iterator[Tuple[ast.AST, List[ast.AST]]]:
    """深度优先遍历，同时给出祖先链（由外到内）。"""
    stack: List[Tuple[ast.AST, List[ast.AST]]] = [(tree, [])]
    while stack:
        node, parents = stack.pop()
        yield node, parents
        for child in ast.iter_child_nodes(node):
            stack.append((child, parents + [node]))


def enclosing_function(parents: List[ast.AST]) -> Optional[ast.AST]:
    for p in reversed(parents):
        if isinstance(p, (ast.FunctionDef, ast.AsyncFunctionDef)):
            return p
    return None


def is_under_main_guard(node: ast.AST, parents: List[ast.AST]) -> bool:
    """是否位于 `if __name__ == "__main__":` 里，或某个叫 main 的函数里。

    这条判断是 HX104 的**主要误报防线**：CLI 入口用 print 输出结果是正当行为，
    把这些也报成"调试残留"会让规则立刻失去可信度。
    """
    fn = enclosing_function(parents)
    if fn is not None and getattr(fn, "name", "") in {"main", "cli", "entrypoint"}:
        return True
    for p in parents:
        if isinstance(p, ast.If) and "__main__" in ast.dump(p.test):
            return True
    return False


def literal_str(node: Optional[ast.AST]) -> Optional[str]:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    return None


def shannon_entropy(s: str) -> float:
    """香农熵（bit/字符）。用来在没有明显变量名提示时判断"这串东西像不像密钥"。"""
    if not s:
        return 0.0
    counts = {}
    for ch in s:
        counts[ch] = counts.get(ch, 0) + 1
    n = len(s)
    return -sum((c / n) * math.log2(c / n) for c in counts.values())


def body_is_only(node: ast.AST, kinds: Tuple[type, ...]) -> bool:
    """函数体是否（除去 docstring 后）只由指定种类的语句构成。"""
    body = getattr(node, "body", [])
    if body and isinstance(body[0], ast.Expr) and literal_str(body[0].value) is not None:
        body = body[1:]
    return bool(body) and all(isinstance(s, kinds) for s in body)


# 在 self./cls. 上调用这些前缀的方法，按"断言辅助方法"处理（check_xxx / verify_xxx
# 是测试里极常见的自定义断言封装）。限定接收者是 self/cls，避免把生产代码里的
# check_xxx() 误当成断言。
_HELPER_PREFIXES = ("assert", "check", "verify")
_DIRECT_CALLS = ("fail", "expect")


def has_assertion(node: ast.AST) -> bool:
    """函数体内是否存在断言。

    覆盖的写法：
      - `assert x`（含 pytest 的 `assert x == y`）
      - `self.assertEqual(...)` / `self.assertRaises(...)`（unittest 风格）
      - `self.fail(...)` / `pytest.fail(...)` / `expect(...)`
      - `self.check_xxx(...)` / `self.verify_yyy(...)`（自定义断言封装）

    最后一类是为了避免**把"委托给辅助方法的测试"全判成没断言**——那会让这条规则
    在真实代码库上几乎不可用。代价是辅助方法内部若真的不断言，本规则看不出来。
    """
    for sub in ast.walk(node):
        if isinstance(sub, ast.Assert):
            return True
        if not isinstance(sub, ast.Call):
            continue
        fn = sub.func
        if isinstance(fn, ast.Attribute):
            recv = fn.value
            if fn.attr.startswith(_HELPER_PREFIXES) or fn.attr in _DIRECT_CALLS:
                return True
            if isinstance(recv, ast.Name) and recv.id in ("self", "cls"):
                if fn.attr.startswith(_HELPER_PREFIXES):
                    return True
        elif isinstance(fn, ast.Name):
            if fn.id.startswith(_HELPER_PREFIXES) or fn.id in _DIRECT_CALLS:
                return True
    return False


def describe_call(node: ast.Call) -> str:
    """把调用表达式还原成短文本（用于诊断里的"怎么改"举例）。"""
    try:
        return ast.unparse(node)
    except Exception:  # pragma: no cover - 需要 3.9+，不会走到
        return "<call>"


def unparse(node: ast.AST) -> str:
    try:
        return ast.unparse(node)
    except Exception:  # pragma: no cover
        return "<expr>"
