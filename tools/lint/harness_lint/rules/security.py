"""安全类规则：HX202 硬编码密钥

这条规则的定位是 **宁可漏报不误报**。判断"这串字符串是不是密钥"没有确定答案，
所以用了多重门槛叠加（变量名像密钥 + 长度 + 香农熵 + 已知令牌前缀），并显式排除
占位符文案。代价是漏掉一些弱密钥（比如包含 test/dummy 字样的真密钥），换来的是
不会在正常代码里刷出一堆"假密钥"——后者的危害更大：一旦用户学会忽略这条告警，
真正的密钥泄漏也会被忽略。
"""

from __future__ import annotations

import ast
import re
from typing import Iterable, List, Optional, Tuple

from ..diagnostic import Diagnostic
from ..engine import FileContext, Rule
from ._util import literal_str, shannon_entropy

DOC_SECURITY = "docs/conventions/security.md"

# 变量名/关键字像密钥
_SECRET_NAME_RE = re.compile(
    r"(passwd|password|pwd|secret|token|api[_-]?key|apikey|access[_-]?key"
    r"|private[_-]?key|credential|auth[_-]?key|client[_-]?secret)",
    re.IGNORECASE,
)

# 明显是占位符/测试值 → 一律不报（FP 的主要来源就在这里）
_PLACEHOLDER_MARKERS = (
    "xxx",
    "yyy",
    "zzz",
    "placeholder",
    "changeme",
    "change_me",
    "example",
    "sample",
    "dummy",
    "fake",
    "todo",
    "your",
    "redacted",
    "***",
    "<",
    ">",
    "${",
    "{{",
    "env.",
    "getenv",
)

# 一眼能认出来的真令牌前缀（即使变量名不像也能认）
_TOKEN_PREFIXES = (
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "xoxb-",
    "glpat-",
    "hf_",
    "AIza",
    "AKIA",
)


class HardcodedSecret(Rule):
    id = "HX202"
    name = "hardcoded-secret"
    level = "error"
    summary = "疑似硬编码密钥/令牌"
    doc = DOC_SECURITY
    fixable = False
    fix_blocked_reason = "替换成环境变量读取需要部署侧的配合，机器无法代判"

    def check(self, ctx: FileContext) -> Iterable[Diagnostic]:
        if ctx.tree is None:
            return []
        out: List[Diagnostic] = []
        for node in ast.walk(ctx.tree):
            for name, value_node, lineno, column in _assignments(node):
                value = literal_str(value_node)
                if value is None:
                    continue
                if not _is_candidate(name, value):
                    continue
                looks, why = _looks_like_secret(value, ctx.config.secret_entropy_threshold, ctx.config.min_secret_len)
                if not looks:
                    continue
                shown = _mask(value)
                span = ctx.span(lineno, column, label=f"{name} = {shown}")
                out.append(
                    Diagnostic(
                        rule=self.id,
                        level=self.level,
                        message=f"疑似硬编码密钥：{name} = {shown}",
                        span=span,
                        doc=self.doc,
                        fixable=self.fixable,
                        fix_blocked_reason=self.fix_blocked_reason,
                    )
                    .with_why(
                        f"判定依据：{why}。硬编码密钥会随代码进版本库、进日志、进镜像层，"
                        "一旦泄漏只能全量轮换——而密钥本身在 git 历史里是删不掉的。"
                    )
                    .with_fix(
                        "从环境变量或密钥管理服务读取：\n"
                        f"    {name} = os.environ[\"{name.upper()}\"]\n"
                        "并把该变量加入 .env.example（写占位值）与部署配置；"
                        "泄漏过的密钥必须轮换，不能只从代码里删掉。"
                    )
                    .with_child(
                        "note",
                        "本规则刻意偏保守：变量名不像密钥且没有已知令牌前缀时不会报，"
                        "所以漏报是可能的。带 reason 的行内豁免见 docs/conventions/suppressions.md",
                    )
                )
        return out


def _assignments(node: ast.AST) -> Iterable[Tuple[str, ast.AST, int, int]]:
    """产出 (名字, 值节点, 行号, 列号)。覆盖赋值、注解赋值、关键字参数、下标赋值。"""
    if isinstance(node, ast.Assign):
        for t in node.targets:
            for name, col in _target_names(t):
                yield name, node.value, node.lineno, col
    elif isinstance(node, ast.AnnAssign) and node.value is not None:
        for name, col in _target_names(node.target):
            yield name, node.value, node.lineno, col
    elif isinstance(node, ast.Call):
        for kw in node.keywords:
            if kw.arg:
                yield kw.arg, kw.value, getattr(kw.value, "lineno", node.lineno), getattr(
                    kw.value, "col_offset", node.col_offset
                ) + 1
    elif isinstance(node, ast.Dict):
        for k, v in zip(node.keys, node.values):
            key = literal_str(k)
            if key:
                yield (
                    key,
                    v,
                    getattr(v, "lineno", node.lineno),
                    getattr(v, "col_offset", node.col_offset) + 1,
                )


def _target_names(target: ast.AST) -> Iterable[Tuple[str, int]]:
    if isinstance(target, ast.Name):
        yield target.id, target.col_offset + 1
    elif isinstance(target, ast.Attribute):
        yield target.attr, target.col_offset + 1
    elif isinstance(target, ast.Subscript):
        key = literal_str(target.slice)
        if key:
            yield key, target.col_offset + 1
    elif isinstance(target, (ast.Tuple, ast.List)):
        for elt in target.elts:
            yield from _target_names(elt)


def _is_candidate(name: str, value: str) -> bool:
    if _SECRET_NAME_RE.search(name):
        return True
    return any(value.startswith(p) for p in _TOKEN_PREFIXES)


def _looks_like_secret(value: str, threshold: float, min_len: int) -> Tuple[bool, str]:
    if len(value) < min_len:
        return False, ""
    low = value.lower()
    if any(m in low for m in _PLACEHOLDER_MARKERS):
        return False, ""
    if len(set(value)) <= 3 or re.fullmatch(r"(.)\1+", value):
        return False, ""
    for p in _TOKEN_PREFIXES:
        if value.startswith(p):
            return True, f"匹配已知令牌前缀 {p!r}"
    ent = shannon_entropy(value)
    if ent >= threshold:
        return True, f"香农熵 {ent:.2f} ≥ 阈值 {threshold}（长度 {len(value)}）"
    return False, ""


def _mask(value: str) -> str:
    """诊断里不回显完整密钥：既避免二次泄漏，也避免它被写进日志/报告。"""
    if len(value) <= 8:
        return "*" * len(value)
    return f"{value[:4]}{'*' * 6}{value[-4:]}（共 {len(value)} 字符）"


RULES = (HardcodedSecret,)
