"""harness-lint：自定义 lint 规则 + 诊断内嵌修复指令，让 Agent 能自我纠正。

包内结构：
    diagnostic.py  诊断数据模型（字段语义对齐 rustc 的 JSON 诊断）
    config.py      .harnesslint.toml
    graph.py       导入图与分层判定（跨文件规则的事实来源）
    engine.py      遍历 / 跑规则 / 豁免记账 / 反规避 / 报告
    rules/         规则实现（12 条检查规则 + 6 条元规则）
    render.py      人读四段式渲染 + rustc 形状的 JSON
    cli.py         命令行入口
    flake8_plugin.py  flake8 插件薄包装（让规则进入真实 Python 生态）
"""

__version__ = "0.1.0"

__all__ = ["__version__"]
