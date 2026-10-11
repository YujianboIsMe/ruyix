"""熵管理的度量层（传感器）。

职责边界写死在这里：**只测量，不判定**。

- 阈值、趋势判断、修不修、开不开 PR 全在控制侧（Rust 的 `entropy.rs`）。
- 本模块产出的是**一条可比较的时间序列记录**，追加进 `history.jsonl`。

为什么这么切（而不是把阈值也写这里）：

1. 测量是**确定性、便宜、可复现**的 —— 扫全库不需要模型，也不该需要；
2. 策略和测量分开后，调阈值不用动测量代码，两侧的单测也能各自打；
3. 对上你笔记里那个控制论框架：本模块是**传感器**，Rust 侧是**控制器+执行器**，
   `history.jsonl` 是反馈信号。混在一起就没法单独验证"感知准不准"。

指标为什么必须带**密度**（每千行）而不是绝对数：绝对数会随代码量自然增长，
"代码写得多 = 分低"的评分没有意义。密度才是可比的。
"""

from __future__ import annotations

import ast
import hashlib
import json
import os
import re
from dataclasses import asdict, dataclass, field
from statistics import median
from typing import Dict, List, Optional, Tuple

from .config import Config

# ---------------------------------------------------------------- 评分权重


@dataclass
class Weights:
    """质量评分的权重（全部可在 `.harnesslint.toml` 的 `[entropy]` 里覆盖）。

    两条防作弊的硬约束，写在权重设计里而不是写在文档里：

    - **豁免是负分项**（而且是双倍/三倍）——否则"靠加 noqa 让检查闭嘴"就能提分；
    - **测试归零单独扣分** ——否则删测试能让分母变小、密度变好看。
    """

    error_density: float = 3.0  # 每千行 error
    warning_density: float = 1.0  # 每千行 warning
    suppression_density: float = 2.0  # 每千行"我们自己的"豁免
    no_reason_density: float = 10.0  # 每千行无理由豁免（重罚）
    duplicate_density: float = 5.0  # 每组重复实现 / 千行
    placeholder_density: float = 2.0  # 每处 TODO / 空桩 / 千行
    no_tests: float = 2.0  # 有代码却一个测试都没有


# ---------------------------------------------------------------- 指标


@dataclass
class Metrics:
    ts: str
    files: int = 0
    code_lines: int = 0
    violations_total: int = 0
    violations_error: int = 0
    violations_warning: int = 0
    density: float = 0.0  # 违规 / 千行
    error_density: float = 0.0
    warning_density: float = 0.0
    suppression_density: float = 0.0
    no_reason_density: float = 0.0
    duplicate_density: float = 0.0
    placeholder_density: float = 0.0
    by_rule: Dict[str, int] = field(default_factory=dict)
    suppressions_total: int = 0
    suppressions_used: int = 0  # 真的抑制了我们的诊断的条数（可判定"是不是活的"）
    suppressions_without_reason: int = 0
    test_count: int = 0
    test_density: float = 0.0  # 测试用例 / 千行
    file_lines_p50: float = 0.0
    file_lines_p90: float = 0.0
    max_file_lines: int = 0
    over_limit_files: int = 0
    duplicate_groups: int = 0
    duplicate_functions: int = 0
    duplicate_lines: int = 0
    placeholder_hits: int = 0
    score: float = 100.0

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: dict) -> "Metrics":
        known = {f for f in cls.__dataclass_fields__}  # type: ignore[attr-defined]
        return cls(**{k: v for k, v in d.items() if k in known})

    def row(self, keys: List[str]) -> List[str]:
        out = []
        for k in keys:
            v = getattr(self, k, "")
            if isinstance(v, float):
                out.append(f"{v:.2f}")
            else:
                out.append(str(v))
        return out


# ---------------------------------------------------------------- 评分


def is_our_suppression(item: dict) -> bool:
    """这条豁免是不是"我们发的"？

    `# noqa` 是全生态共享的命名空间：`BLE001`、`F401` 是 ruff/flake8 的规则，
    它们没抑制我们的任何诊断，不该计入我们的熵。
    只有带 HX 前缀的、以及不带 codes 的（那样会抑制该行全部诊断）才算。
    """
    codes = item.get("codes") or []
    if not codes:
        return True
    return any(str(c).upper().startswith("HX") for c in codes)


def score_of(m: Metrics, w: Weights) -> float:
    """质量分：100 分满分，越高越好。**是密度的函数，不是绝对数的函数。**"""
    # 全部惩罚项都是**密度**（每千行），所以它们同量纲、可加、可解释。
    # 绝对数会随代码量自然增长，"代码写得多 = 分低"的评分没有意义。
    penalty = (
        w.error_density * m.error_density
        + w.warning_density * m.warning_density
        + w.suppression_density * m.suppression_density
        + w.no_reason_density * m.no_reason_density
        + w.duplicate_density * m.duplicate_density
        + w.placeholder_density * m.placeholder_density
        + (w.no_tests if m.test_count == 0 and m.code_lines > 0 else 0.0)
    )
    return round(max(0.0, min(100.0, 100.0 - penalty)), 2)


# ---------------------------------------------------------------- 重复实现检测

_PLACEHOLDER_RE = re.compile(r"(?<![A-Za-z0-9_])(TODO|FIXME|XXX|HACK)(?![A-Za-z0-9_])")


class _Normalizer(ast.NodeTransformer):
    """把局部名字和常量抹掉，只留结构 —— 目的是判"是不是同一段实现"。

    刻意保留属性名（`obj.foo` 的 `foo`）和方法名：它们承载语义，
    一起抹掉会把"看起来像但意思不同"的代码合并进同一组。
    """

    def visit_Name(self, node: ast.Name) -> ast.AST:  # noqa: N802
        node.id = "_"
        return node

    def visit_Constant(self, node: ast.Constant) -> ast.AST:  # noqa: N802
        node.value = None
        node.kind = None
        return node

    def visit_arg(self, node: ast.arg) -> ast.AST:  # noqa: N802
        node.arg = "_"
        node.annotation = None
        return node


def _body_without_docstring(body: List[ast.stmt]) -> List[ast.stmt]:
    if (
        body
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
        and isinstance(body[0].value.value, str)
    ):
        return body[1:]
    return body


def function_signature(node: ast.AST) -> str:
    """函数体的结构指纹（归一化后 AST 的 sha1 前 12 位）。"""
    body = _body_without_docstring(list(getattr(node, "body", [])))
    normalized = _Normalizer().visit(ast.Module(body=body, type_ignores=[]))
    ast.fix_missing_locations(normalized)
    dumped = ast.dump(normalized, annotate_fields=False)
    return hashlib.sha1(dumped.encode("utf-8")).hexdigest()[:12]


@dataclass
class DuplicateGroup:
    signature: str
    lines: int
    members: List[Tuple[str, str, int]]  # (file, func, lineno)


def find_duplicates(files, cfg: Config) -> List[DuplicateGroup]:
    """找出"结构完全相同"的函数体分组（≥ dup_min_lines 行、≥ 2 处）。

    这是理论笔记里"冗余的工具函数、重复的实现"那类漂移的可度量代理。
    """
    buckets: Dict[str, DuplicateGroup] = {}
    for fctx in files:
        if fctx.tree is None:
            continue
        for node in ast.walk(fctx.tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            lines = (node.end_lineno or node.lineno) - node.lineno + 1
            body_len = None
            if node.body:
                body_len = (node.body[-1].end_lineno or node.lineno) - (
                    node.body[0].lineno
                ) + 1
            if body_len is None or body_len < cfg.dup_min_lines:
                continue
            sig = function_signature(node)
            g = buckets.setdefault(sig, DuplicateGroup(sig, body_len, []))
            g.members.append((fctx.rel, node.name, node.lineno))
            lines = max(lines, g.lines)
    return [g for g in buckets.values() if len(g.members) >= cfg.dup_min_occurrences]


# ---------------------------------------------------------------- 计算


def _count_code_lines(lines: List[str]) -> int:
    n = 0
    for line in lines:
        s = line.strip()
        if s and not s.startswith("#"):
            n += 1
    return n


def percentile(values: List[int], p: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    k = (len(ordered) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(ordered) - 1)
    return round(ordered[lo] + (ordered[hi] - ordered[lo]) * (k - lo), 1)


def compute(full, cfg: Config, ts: str) -> Metrics:
    """把一次全库扫描（`engine.run_full` 的结果）压成一条指标记录。"""
    report = full.report
    files = list(full.files)

    code_lines = sum(_count_code_lines(f.lines) for f in files)
    per_file_lines = [len(f.lines) for f in files]
    kloc = max(code_lines, 1) / 1000.0

    # report.suppressions 是豁免条目列表（汇总形状只在 to_json 里），这里自己数。
    # 只数"我们自己的"——别的工具的 noqa 不计入本项目的熵。
    ours = [i for i in (report.suppressions or []) if is_our_suppression(i)]
    supp_total = len(ours)
    supp_used = sum(1 for i in ours if i.get("used"))
    supp_bad = sum(1 for i in ours if not i.get("reason"))

    by_rule: Dict[str, int] = dict(report.by_rule())
    placeholder_hits = int(by_rule.get("HX105", 0))

    dup = find_duplicates(files, cfg)

    m = Metrics(
        ts=ts,
        files=len(files),
        code_lines=code_lines,
        violations_total=report.total(),
        violations_error=report.errors(),
        violations_warning=report.warnings(),
        density=round(report.total() / kloc, 3),
        error_density=round(report.errors() / kloc, 3),
        warning_density=round(report.warnings() / kloc, 3),
        suppression_density=round(supp_total / kloc, 3),
        no_reason_density=round(supp_bad / kloc, 3),
        duplicate_density=round(len(dup) / kloc, 3),
        placeholder_density=round(placeholder_hits / kloc, 3),
        by_rule=by_rule,
        suppressions_total=supp_total,
        suppressions_used=supp_used,
        suppressions_without_reason=supp_bad,
        test_count=report.test_count,
        test_density=round(report.test_count / kloc, 3),
        file_lines_p50=percentile(per_file_lines, 0.5),
        file_lines_p90=percentile(per_file_lines, 0.9),
        max_file_lines=max(per_file_lines) if per_file_lines else 0,
        over_limit_files=int(by_rule.get("HX101", 0)),
        duplicate_groups=len(dup),
        duplicate_functions=sum(len(g.members) for g in dup),
        duplicate_lines=sum(g.lines * (len(g.members) - 1) for g in dup),
        placeholder_hits=placeholder_hits,
    )
    m.score = score_of(m, cfg.entropy_weights())
    return m


# ---------------------------------------------------------------- 历史（时间序列）


def default_history_path(root: str, cfg: Config) -> str:
    rel = cfg.entropy_history or os.path.join(".harness-entropy", "history.jsonl")
    return rel if os.path.isabs(rel) else os.path.join(os.path.abspath(root), rel)


def load_history(path: str) -> List[Metrics]:
    if not os.path.isfile(path):
        return []
    out: List[Metrics] = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(Metrics.from_dict(json.loads(line)))
            except (json.JSONDecodeError, TypeError):
                # 坏行不阻断整条时间序列：跳过并继续（历史文件是追加写的，
                # 半行数据不该让整个趋势分析失效）
                continue
    return out


def append_history(path: str, m: Metrics, max_keep: int) -> str:
    """追加一条快照，并裁掉超出的最旧记录。返回实际写入的路径。"""
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    keep = load_history(path) if max_keep > 0 else []
    keep.append(m)
    if max_keep > 0 and len(keep) > max_keep:
        keep = keep[-max_keep:]
    if max_keep > 0:
        with open(path, "w", encoding="utf-8", newline="\n") as fh:
            for item in keep:
                fh.write(json.dumps(item.to_dict(), ensure_ascii=False) + "\n")
    else:
        with open(path, "a", encoding="utf-8", newline="\n") as fh:
            fh.write(json.dumps(m.to_dict(), ensure_ascii=False) + "\n")
    return path


def reference_of(history: List[Metrics], window: int) -> Optional[Metrics]:
    """取参考基线：**最近 window 条的中位数**，而不是上一条。

    单周噪声（比如某天刚好合并了一个大文件）不该被当成"熵增"。中位数抗单点抖动，
    而且完全可解释。
    """
    if not history:
        return None
    tail = history[-window:] if window > 0 else history[-1:]
    if len(tail) == 1:
        return tail[0]
    numeric = [f for f in Metrics.__dataclass_fields__ if f != "by_rule"]  # type: ignore[attr-defined]
    base = Metrics(ts=tail[-1].ts)
    for f in numeric:
        values = [getattr(m, f, 0) for m in tail]
        if all(isinstance(v, (int, float)) for v in values):
            med = median(values)
            setattr(base, f, int(med) if isinstance(getattr(tail[0], f), int) else round(med, 3))
    base.by_rule = dict(tail[-1].by_rule)
    return base


def best_of(history, window: int) -> Optional[Metrics]:
    """窗口内**质量分最高**的那条快照。

    用途：判断"是不是离自己最近的最好水平越来越远"。
    只有中位数基线会漏掉一种情况——同一个退化状态持续 N 条之后被吸收成新常态，
    于是永久退化不再报警。而熵管理要阻止的恰恰是永久退化。

    刻意只在**窗口内**取最好（不是全历史）：否则项目早期的漂亮分数会变成
    永远够不到的打卡线，信号立刻退化成误报机器。
    """
    if not history:
        return None
    tail = history[-window:] if window > 0 else history[-1:]
    return max(tail, key=lambda m: m.score)


def render_report(m: Metrics, ref: Optional[Metrics], window: int) -> str:
    """人读的一段摘要（给日志和 PR 正文用）。"""
    lines = [
        f"质量分 {m.score:.2f}",
        f"  文件 {m.files} · 代码行 {m.code_lines} · 测试用例 {m.test_count}",
        f"  违规 {m.violations_total}（error {m.violations_error} / warning {m.violations_warning}）"
        f" · 密度 {m.density:.3f}/千行 · error 密度 {m.error_density:.3f}/千行",
        f"  豁免 {m.suppressions_total}（生效 {m.suppressions_used} / 无理由 {m.suppressions_without_reason}）"
        f" · 密度 {m.suppression_density:.3f}/千行",
        f"  重复实现 {m.duplicate_groups} 组 / {m.duplicate_functions} 个函数"
        f" / 约 {m.duplicate_lines} 行冗余",
        f"  文件行数 p50 {m.file_lines_p50} / p90 {m.file_lines_p90} / max {m.max_file_lines}"
        f" · 超限文件 {m.over_limit_files}",
    ]
    if ref is not None:
        lines.append(
            f"  参考基线（近 {window} 条中位数）：质量分 {ref.score:.2f} · "
            f"密度 {ref.density:.3f} · 豁免 {ref.suppressions_total} · 测试 {ref.test_count}"
        )
    return "\n".join(lines)
