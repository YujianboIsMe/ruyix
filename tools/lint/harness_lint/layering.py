"""分层判定：把 import 图翻译成"哪条依赖违反了方向"。

**为什么和 `graph.py` 分开**：`graph.py` 只负责"事实"（哪些文件、导入了谁、解析到哪个文件），
判定（哪些层、允许什么方向、哪条算违规）是**策略**，两者变化的理由完全不同 ——
事实随代码变，策略随架构约定变。分开之后各自的测试也能各测各的：
`test_architecture.py` 测判定，图解析只测解析。

判定刻意保守（对齐"宁可漏报不误报"）：
- 依赖方向必须**向下**（`layers` 里 index 大的可以依赖 index 小的，反向即违规）；
- 可选严格模式（`layers_strict = true`）：只允许依赖**紧邻下层**，跳过中间层算越层；
- **任何一侧判不出层就不报**（目录名匹配不上任何层 → 跳过）。
- **延迟导入照样算**：函数体内的 import 不参与"循环依赖"判定，但方向问题与它在哪一层无关。
"""

from __future__ import annotations

from typing import Dict, List

from .config import Config
from .graph import Graph, ImportRef


def layer_of_import(cfg: Config, ref: ImportRef) -> str:
    """判断这条 import 指向哪一层。

    做法：把模块名按 `.` 拆开，取**第一个恰好等于某个层目录名**的片段。
    这样 `myapp.repo.order_repo`、`repo.order_repo`、`order_repo` 都能落到 repo。
    相对导入（`from ..service import x`）只看 module 部分，同样适用。
    """
    parts = ref.module.split(".") if ref.module else []
    for seg in parts:
        for layer in cfg.layers:
            names = cfg.layer_dirs.get(layer, [layer])
            if seg in names:
                return layer
    return ""


def layer_violations(cfg: Config, graph: Graph) -> List[Dict]:
    """返回分层违规事实列表。"""
    out: List[Dict] = []
    for ref in graph.imports:
        src_layer = cfg.layer_of(ref.file)
        dst_layer = layer_of_import(cfg, ref)
        if not src_layer or not dst_layer or src_layer == dst_layer:
            continue
        si, di = cfg.layer_index(src_layer), cfg.layer_index(dst_layer)
        if si < 0 or di < 0:
            continue
        if di > si:
            out.append(
                {
                    "kind": "reverse",
                    "src_layer": src_layer,
                    "dst_layer": dst_layer,
                    "ref": ref,
                    "gap": di - si,
                }
            )
        elif getattr(cfg, "layers_strict", False) and (si - di) > 1:
            out.append(
                {
                    "kind": "skip",
                    "src_layer": src_layer,
                    "dst_layer": dst_layer,
                    "ref": ref,
                    "gap": si - di,
                }
            )
    return out
