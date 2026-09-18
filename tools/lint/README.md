# harness-lint

自定义 lint 规则 + **诊断内嵌修复指令**，让 Agent 能自我纠正。
纯标准库（Python ≥ 3.11），零第三方依赖。

> 它要管的不是"语言层面的错"（那是 ruff / clippy / 编译器的活），
> 而是**项目约定**：分层依赖方向、测试必须有断言、异常不许吞、豁免必须带理由……
> 这些编译器管不到，但恰恰是"能跑但烂"的主要来源。

## 用法

```bash
# 人读：带源码上下文的四段式报告
python -m harness_lint .

# 机读：对齐 rustc 诊断形状的 JSON（harness 与 CI 用这个）
python -m harness_lint . --json

# 看看这个项目在管哪些约定
python -m harness_lint . --list-rules

# 老项目接入：先写基线，之后只对"新增违规"负责
python -m harness_lint . --write-baseline baseline.json
python -m harness_lint . --baseline baseline.json --strict

# 测试（46 个：每条规则正例 + 反例）
python -m unittest discover -s tests -v
```

退出码：`0` 通过 / `1` 有新增违规 / `2` 用法或内部错误。

## 误报修复记录：HX302 的两副面孔

误报率是这类工具的生命线（**一次误报就会让人学会忽略所有报错**）。v0.6 做知识库的
开/关对照实验时，主判据被下面第一类误报污染，才把它们查出来（当时的表现是
"关知识库那次报 3 条循环依赖，开的那次 0 条"——差点被当成"知识库修好了分层方向"）。

### ① 解析错：包内 `__init__.py` 的重导出被判成自环（已修）

最小复现：

```
probe/
  api/__init__.py    ← from .api import create_user
  api/api.py         ← def create_user(...)
  service/service.py
  main.py
```

修前：`{"by_rule": {"HX302": 1}}` → `循环依赖：api/__init__.py → api/__init__.py`

**实际没有循环**：`api/__init__.py` 里的 `from .api import X` 指的是 `api/api.py`，
而旧实现忽略相对导入的 `level` 与"文件所在包"，直接拿 `api` 从仓库根找 → 命中
`api/__init__.py`（包自己）。三层结构里"包内重导出"是最常见的写法，所以这条误报会成片出现。

修法（`graph.py`）：相对导入按 **CPython 语义**解析 —— `level=1` 指当前包，往上每层减一，
再拼 `module`；同时**自环不进图**（`src == dst` 的边不是依赖也不是环）。

### ② 判定过宽：延迟导入被打成环（已收窄）

修完 ① 之后，本仓库自检反而冒出 **5 条 HX302** —— 全是这种形状：

```
engine.py → rules/__init__.py → rules/correctness.py → engine.py
config.py → entropy.py → config.py
```

这两处的代码里**明明写着** `from .rules import ...  # 延迟导入，避免循环依赖`。
也就是说：环早被作者用**函数体内导入**打破了，导入期不会再出问题 ——
把它们报成 error 属于判定过宽（"把已经修好的代码再报一遍"）。

修法：`find_cycles` 默认**只用模块级导入成图**；环上只要有一条边是延迟导入就不报。
配套两条纪律：

1. **延迟导入照样算 HX301**（分层方向）：延迟只解决"导入期"，依赖方向一点没变。
   有单测钉着（`test_positive_deferred_import_still_counts_as_reverse_dependency`）。
2. 诊断里写明这条边界（"为什么这条不报"必须能被看见），别让人以为规则漏了。

### 回归验证（修完必须验"没有放过真违规"）

| 对象 | 修前 | 修后 |
|---|---|---|
| 本仓库自检 | HX302 ×5（全是延迟导入假环） | **0**，其余 9 条 warning 一条不少 |
| `examples/demo_app`（故意留了反向依赖与真环做演示） | HX301 ×1 + HX302 ×1 | **照旧报出**（没有因为修误报而放过真违规） |
| 三层结构产物（`api/__init__.py` 重导出） | HX302 ×3（假环） | **0** |
| 单测 | 71 | **79**（新增 3 条误报回归 + 真环仍报 + 延迟导入方向仍报） |

> 教训：**误报修复必须同时给出"真违规仍然报"的反向验证**，否则"修误报"很容易变成
> "把哨兵关掉"。

## 作为 flake8 插件用

```bash
uv venv .venv && uv pip install flake8 -e .
cd examples/demo_app && ../../.venv/Scripts/python.exe -m flake8 --select=HX .
```

> 插件形态只跑**文件级规则**：flake8 一次只喂一个文件的 AST，跨文件的
> HX301 / HX302（分层依赖、循环依赖）跑不了，需要项目级上下文。

## 规则

| ID | 规则 | 级别 |
|---|---|---|
| HX101 / HX102 | 单文件 / 单函数行数超上限 | warning |
| HX103 | 测试函数没有任何断言 | error |
| HX104 | 调试 `print` 残留（main guard 内豁免） | warning |
| HX105 | TODO / 空桩实现 | warning |
| HX201 | 裸 `except` / `except Exception: pass` | error |
| HX202 | 疑似硬编码密钥（回显掩码） | error |
| HX203 | 可变默认参数 | warning |
| HX204 | `== None`（应写 `is None`） | warning |
| HX301 / HX302 | 分层依赖方向违规 / 循环依赖 | error |
| HX900 / HX901 / HX902 / HX903 | 豁免无理由 / 豁免超预算 / 测试数下降 / 豁免指向不存在的规则 | error / warning |
| HX000 / HX998 | 文件读不了或解析不了 / 规则自身执行失败 | error |

## 诊断为什么长这样

一条诊断必须**自包含**——只给它，不给规则文档、不给 linter 源码，也应该能改对：

```
service/order.py:17:5  HX201  error  使用了裸 except…
  为什么：裸 except 捕获一切异常，包括 Ctrl+C 触发的 KeyboardInterrupt……
  怎么改：捕获具体异常类型，并在捕获后三选一：① 记日志 ② 转成本层领域异常再抛 ③ 明确注释……
  参考：docs/conventions/error-handling.md（<解析到的真实路径>）
  自动修复：不可用 —— 该捕获哪些异常、捕获后如何处理，取决于业务语义
```

字段语义对齐 rustc 的 JSON 诊断（`code` / `level` / `spans` / `suggested_replacement` /
`suggestion_applicability` / `children`），其中 **`suggestion_applicability` 四级语义**
用来回答"这条建议能不能让工具自动改"：

| 值 | 含义 |
|---|---|
| `MachineApplicable` | 肯定是你想要的，应自动应用（本项目只有 `== None → is None` 用） |
| `MaybeIncorrect` | 可能想要，应用后仍是合法代码 |
| `HasPlaceholders` | 含占位符，不能自动应用 |
| `Unspecified` | 未知 / 不给建议 |

## 反规避

Agent 面对 lint 的天然倾向是**让检查闭嘴**而不是把代码改对。所以：

- 豁免必须带理由（`# noqa: HX201 reason=...`），否则 `HX900`；
- 豁免有**预算**（harness 侧默认 0 条）；
- 支持**基线对比**，只对新增违规负责——而不是"存量也必须全绿"；
- 测试用例数相对基线下滑即报错（`HX902`）；
- 规则 ID 拼错等于偷偷关掉检查（`HX903` 只校验 `HX` 前缀，别的工具的 code 不予置评）。

## 配置

项目根放 `.harnesslint.toml`：

```toml
[lint]
max_file_lines = 200
max_function_lines = 50
flag_print = true
require_noqa_reason = true
ignore = ["HX105"]           # 整体关闭某条规则
ignore_paths = ["examples/*"] # 故意违规的语料目录

[architecture]
layers = ["types", "config", "repo", "service", "runtime", "ui"]
strict = false                # true = 只允许依赖"紧邻的下层"
```

约定文档在 `harness_lint/conventions/`（诊断里的"参考"指向这里，保证路径真实存在）。

## 结构

```
harness_lint/
  diagnostic.py    诊断数据模型（字段语义对齐 rustc）
  config.py        .harnesslint.toml
  discovery.py     读文件 / 解析 AST / 上下文对象（含 HX000）
  engine.py        跑规则（规则异常外显成 HX998，绝不静默吞掉）
  report.py        报告与统计、基线、增量
  suppressions.py  豁免：解析 / 生效 / 记账 / 反规避
  graph.py         导入图与分层判定（跨文件规则的事实来源）
  render.py        人读渲染（四段式）与 JSON 输出
  cli.py           CLI
  flake8_plugin.py flake8 插件
  rules/           规则实现
  conventions/     约定文档（诊断"参考"的目标）
```
