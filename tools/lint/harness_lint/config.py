"""配置：`.harnesslint.toml`

用标准库 `tomllib`（3.11+）读，零依赖。所有阈值都可配 —— 硬编码阈值是"规则写太死、
把正常代码判违规"的头号来源。
"""

from __future__ import annotations

import fnmatch
import os
import tomllib
from dataclasses import dataclass, field
from typing import Dict, List

CONFIG_FILENAME = ".harnesslint.toml"

# 默认分层：对齐 OpenAI Harness 工程里的 Types → Config → Repo → Service → Runtime → UI 单向依赖
DEFAULT_LAYERS: List[str] = ["types", "config", "repo", "service", "runtime", "ui"]

# 层名 → 目录名的常见同义词。目录名匹配不到层时**不报**（宁可漏报不误报）
DEFAULT_LAYER_DIRS: Dict[str, List[str]] = {
    "types": ["types", "models", "schemas", "entity", "entities", "domain"],
    "config": ["config", "configs", "settings"],
    "repo": ["repo", "repos", "repository", "dao", "persistence", "store", "storage"],
    "service": ["service", "services", "usecase", "usecases", "application", "core"],
    "runtime": ["runtime", "infra", "infrastructure", "handler", "handlers", "api", "server", "middleware"],
    "ui": ["ui", "cli", "web", "views", "templates", "frontend"],
}


@dataclass
class Config:
    # 结构
    max_file_lines: int = 200
    max_function_lines: int = 50
    # 卫生
    flag_print: bool = True
    flag_placeholders: bool = True
    # 安全
    secret_entropy_threshold: float = 3.2
    min_secret_len: int = 16
    # 架构
    layers: List[str] = field(default_factory=lambda: list(DEFAULT_LAYERS))
    layer_dirs: Dict[str, List[str]] = field(default_factory=lambda: dict(DEFAULT_LAYER_DIRS))
    # 严格模式：只允许依赖紧邻下层（跳过中间层算越层）。默认关闭——"越层"在不少
    # 合法设计里是正常的，默认开启会引入误报，而误报的代价远大于漏报。
    layers_strict: bool = False
    # 反规避
    require_noqa_reason: bool = True
    max_suppressions: int = -1  # -1 = 不限制；自我纠正循环里会设成 0
    # 开关
    ignore: List[str] = field(default_factory=list)  # 规则 ID 或 glob（HX1*）
    ignore_paths: List[str] = field(default_factory=list)  # 路径 glob
    # 目录约定
    conventions_dir: str = ""
    # 熵管理（度量层；阈值与策略在 harness 的 entropy.rs 里）
    dup_min_lines: int = 6
    dup_min_occurrences: int = 2
    entropy_history: str = ""  # 空 = <root>/.harness-entropy/history.jsonl
    entropy_max_keep: int = 200
    entropy_weight_overrides: Dict[str, float] = field(default_factory=dict)
    # 测试识别
    test_file_patterns: List[str] = field(
        default_factory=lambda: ["test_*.py", "*_test.py", "tests/*.py", "test/**/*.py"]
    )

    def entropy_weights(self):
        """评分权重。延迟导入 entropy 以避免循环依赖。"""
        from .entropy import Weights

        w = Weights()
        for k, v in self.entropy_weight_overrides.items():
            if hasattr(w, k):
                setattr(w, k, float(v))
        return w

    def is_ignored_rule(self, rule_id: str) -> bool:
        for pat in self.ignore:
            if rule_id == pat or fnmatch.fnmatch(rule_id, pat):
                return True
        return False

    def is_ignored_path(self, rel_path: str) -> bool:
        rel = rel_path.replace("\\", "/")
        for pat in self.ignore_paths:
            if fnmatch.fnmatch(rel, pat) or fnmatch.fnmatch(os.path.basename(rel), pat):
                return True
        return False

    def layer_of(self, rel_path: str) -> str:
        """按路径第一段（或文件名）判断所属层；判不出返回空串。"""
        rel = rel_path.replace("\\", "/")
        head = rel.split("/")[0]
        # 按 layers 顺序找，保证 types 先于 service 之类不会互相抢
        for layer in self.layers:
            names = self.layer_dirs.get(layer, [layer])
            for n in names:
                if head == n or rel.split("/")[0] == n:
                    return layer
        return ""

    def layer_index(self, layer: str) -> int:
        try:
            return self.layers.index(layer)
        except ValueError:
            return -1

    @property
    def resolved_conventions_dir(self) -> str:
        if self.conventions_dir:
            return os.path.abspath(self.conventions_dir)
        return os.path.join(os.path.dirname(os.path.abspath(__file__)), "conventions")


def load(root: str, explicit: str = "") -> Config:
    """从 root 目录读 `.harnesslint.toml`；不存在就用默认值（不报错）。"""
    cfg = Config()
    path = explicit or os.path.join(root, CONFIG_FILENAME)
    if not os.path.isfile(path):
        return cfg
    with open(path, "rb") as f:
        data = tomllib.load(f)
    lint = data.get("lint", {})
    arch = data.get("architecture", {})
    noqa = data.get("noqa", {})

    for key, attr in [
        ("max_file_lines", "max_file_lines"),
        ("max_function_lines", "max_function_lines"),
        ("secret_entropy_threshold", "secret_entropy_threshold"),
        ("min_secret_len", "min_secret_len"),
        ("flag_print", "flag_print"),
        ("flag_placeholders", "flag_placeholders"),
        ("ignore", "ignore"),
        ("ignore_paths", "ignore_paths"),
        ("conventions_dir", "conventions_dir"),
        ("test_file_patterns", "test_file_patterns"),
    ]:
        if key in lint:
            setattr(cfg, attr, lint[key])
    if "layers" in arch:
        cfg.layers = arch["layers"]
    if "layer_dirs" in arch:
        cfg.layer_dirs = {k: list(v) for k, v in arch["layer_dirs"].items()}
    if "strict" in arch:
        cfg.layers_strict = bool(arch["strict"])
    ent = data.get("entropy", {})
    for key, attr in [
        ("dup_min_lines", "dup_min_lines"),
        ("dup_min_occurrences", "dup_min_occurrences"),
        ("history", "entropy_history"),
        ("max_keep", "entropy_max_keep"),
    ]:
        if key in ent:
            setattr(cfg, attr, ent[key])
    if "weights" in ent:
        cfg.entropy_weight_overrides = {k: float(v) for k, v in ent["weights"].items()}
    if "require_reason" in noqa:
        cfg.require_noqa_reason = noqa["require_reason"]
    if "max_suppressions" in noqa:
        cfg.max_suppressions = noqa["max_suppressions"]
    return cfg
