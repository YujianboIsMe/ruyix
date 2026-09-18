"""命令行入口。

退出码约定（供 CI 与 harness 自我纠正循环使用）：
    0  = 通过（没有新增的 error；strict 模式下也没有新增 warning）
    1  = 有新增违规
    2  = 用法/内部错误（不是代码问题）

`--baseline` 是"存量豁免、增量严格"的实现：老项目接进来时先写一份基线，
之后只对**新增**违规负责。没有它，任何规则都上不了已有代码库。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from typing import List, Optional

from . import __version__, engine, entropy
from .config import load as load_config
from .engine import LintReport, make_baseline, run
from .render import render_report, report_to_json
from .rules import describe_all


def _merge(reports: List[LintReport], root: str, conventions_dir: str) -> LintReport:
    merged = LintReport(root=root, conventions_dir=conventions_dir)
    for r in reports:
        merged.files_scanned += r.files_scanned
        merged.files_skipped += r.files_skipped
        merged.test_count += r.test_count
        merged.diagnostics.extend(r.diagnostics)
        merged.suppressions.extend(r.suppressions)
    merged.diagnostics.sort(key=lambda d: (d.file, d.line, d.rule))
    return merged


def run_entropy(args) -> int:
    """熵指标：算快照 →（可选）追加历史 → 与参考基线一并输出。

    **不做判定**：退没退化、要不要修、要不要开 PR 都是控制侧（Rust）的事。
    这里只保证"测得准、记得下、可比"。
    """
    root = os.path.abspath(args.paths[0] if args.paths else ".")
    cfg = load_config(root, args.config or "")
    if args.entropy_history:
        cfg.entropy_history = args.entropy_history
    path = entropy.default_history_path(root, cfg)

    history = entropy.load_history(path)
    ref = entropy.reference_of(history, args.entropy_window)
    best = entropy.best_of(history, args.entropy_window)

    from datetime import datetime

    ts = datetime.now().astimezone().isoformat(timespec="seconds")
    full = engine.run_full(root, cfg, None, None)
    m = entropy.compute(full, cfg, ts)

    if args.entropy_write:
        entropy.append_history(path, m, cfg.entropy_max_keep)
        history = entropy.load_history(path)

    if args.entropy_json:
        print(
            json.dumps(
                {
                    "metrics": m.to_dict(),
                    "reference": ref.to_dict() if ref else None,
                    "reference_best": best.to_dict() if best else None,
                    "history_path": path,
                    "history_len": len(history),
                },
                ensure_ascii=False,
                indent=2,
            )
        )
    else:
        print(entropy.render_report(m, ref, args.entropy_window))
        if args.entropy_write:
            print(f"\n已追加到 {path}（历史共 {len(history)} 条）")
        else:
            print(f"\n（未写历史；加 --entropy-write 追加到 {path}）")
    return 0


def main(argv: Optional[List[str]] = None) -> int:  # noqa: HX102 reason=CLI 的参数解析到退出码是一整条线性流程，拆开只会把 argv/args 在函数间传一遍
    parser = argparse.ArgumentParser(
        prog="harness-lint",
        description="自定义 lint 规则：诊断内嵌修复指令，让 Agent 能自我纠正",
    )
    parser.add_argument("paths", nargs="*", help="要检查的目录（默认当前目录）")
    parser.add_argument("--json", action="store_true", help="输出机器可读 JSON（形状对齐 rustc 诊断）")
    parser.add_argument("--config", help="显式指定 .harnesslint.toml 路径")
    parser.add_argument("--baseline", help="基线文件：只对新增违规负责")
    parser.add_argument("--write-baseline", help="把当前结果写成基线文件后退出（存量豁免用）")
    parser.add_argument(
        "--max-suppressions",
        type=int,
        help="允许生效的豁免数上限（自我纠正循环里用 0 堵住'加 noqa 糊过去'）",
    )
    parser.add_argument("--strict", action="store_true", help="warning 也算失败")
    parser.add_argument("--conventions-dir", help="conventions 文档目录（默认用包内自带）")
    parser.add_argument("--list-rules", action="store_true", help="列出全部规则后退出")
    parser.add_argument("--entropy", action="store_true", help="只算熵指标（度量层，不做判定）")
    parser.add_argument(
        "--entropy-history",
        default="",
        help="历史文件路径（默认 <root>/.harness-entropy/history.jsonl）",
    )
    parser.add_argument("--entropy-write", action="store_true", help="把本次快照追加进历史")
    parser.add_argument("--entropy-json", action="store_true", help="熵指标用 JSON 输出")
    parser.add_argument(
        "--entropy-window",
        type=int,
        default=3,
        help="参考基线取最近 N 条的中位数（默认 3）",
    )
    parser.add_argument("--version", action="version", version=f"harness-lint {__version__}")
    args = parser.parse_args(argv)

    if args.entropy:
        return run_entropy(args)
    if args.list_rules:
        print(json.dumps(describe_all(), ensure_ascii=False, indent=2))
        return 0

    roots = [os.path.abspath(p) for p in (args.paths or ["."])]
    for r in roots:
        if not os.path.isdir(r):
            print(f"harness-lint: 不是目录: {r}", file=sys.stderr)
            return 2

    baseline = None
    if args.baseline:
        if not os.path.isfile(args.baseline):
            print(f"harness-lint: 基线文件不存在: {args.baseline}", file=sys.stderr)
            return 2
        with open(args.baseline, encoding="utf-8") as f:
            baseline = json.load(f)

    reports: List[LintReport] = []
    conventions_dir = ""
    for root in roots:
        cfg = load_config(root, args.config or "")
        if args.conventions_dir:
            cfg.conventions_dir = os.path.abspath(args.conventions_dir)
        conventions_dir = cfg.resolved_conventions_dir
        # baseline 统一在合并后算一次，避免多目录时 HX902 被重复追加
        rep, _ = run(root, cfg, None, args.max_suppressions)
        reports.append(rep)

    merged = _merge(reports, roots[0] if len(roots) == 1 else os.getcwd(), conventions_dir)
    from .engine import _compute_new  # 内部函数：合并后统一算增量

    _compute_new(merged, baseline)

    if args.write_baseline:
        with open(args.write_baseline, "w", encoding="utf-8") as f:
            json.dump(make_baseline(merged), f, ensure_ascii=False, indent=2)
        print(f"harness-lint: 基线已写入 {args.write_baseline}", file=sys.stderr)
        return 0

    if args.json:
        print(json.dumps(report_to_json(merged, args.strict), ensure_ascii=False, indent=2))
    else:
        print(render_report(merged, args.strict))

    return 0 if merged.is_ok(args.strict) else 1


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
