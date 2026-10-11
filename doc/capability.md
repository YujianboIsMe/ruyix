# 能力（Capability）体系设计（v0.1）

> 2026-09-19。**能力 = agent 可用外部资源的统称**，四类：
>
> | 类 | 含义 | 模块 / 持久化 | 面板 |
> |---|---|---|---|
> | **MCP** | MCP 服务器工具生态（stdio JSON-RPC） | `mcp.rs` / `mcp.toml` | 导航「MCP」 |
> | **A2A** | 远端 agent 协作（发现 + 任务委托） | `a2a.rs` / `a2a.toml` | 导航「A2A」 |
> | **工具** | **命令行命令白名单**（pandoc / pdflatex / graphviz 等，agent 只允许名单内命令） | `capability.rs` / `tools.toml` | 导航「工具」 |
> | **SKILL** | 可注入 prompt 的 markdown 技能文档 | `capability.rs` / `skills.toml` | 导航「技能」 |
>
> 统一入口：顶部菜单**「能力 ▾」**（MCP / A2A / 工具 / SKILL 四个子菜单，
> 点击激活对应导航面板）。MCP/A2A 的协议细节见 `doc/mcp-a2a.md`。

## 1. 概念与动机

「能力」回答一个问题：**这个 agent 能用什么**。四类能力各自独立配置、
独立面板，但共享同一套持久化纪律与合并语义——

- 全局：`~/.ruyix/code/<name>.toml`；项目：`<root>/.ruyix/code/<name>.toml`
- 条目按 `name` 对齐，**项目覆盖全局同名**（全局在前保序）
- upsert 写回原则：该名字已存在于项目文件 → 写项目级，否则写全局
- 四个 toml 均列入配置编辑器的 `SCOPE_EXCLUDED_FILES`（由专门面板管理，
  平铺编辑器不显示、不写回）

## 2. 工具（命令行白名单）

`tools.toml` 条目：

```toml
[[tools]]
name = "graphviz"
command = "dot"            # 缺省 = name（name 是显示名，command 是可执行）
args_hint = "dot -Tpng in.dot -o out.png"
description = "Graphviz 图形渲染"
enabled = true
```

- **语义**：agent 生成/验证阶段只允许使用白名单内命令（当前为登记与探测；
  引擎侧硬校验在后续接入 generate/verify 时落地——校验点：`exec.rs` 的
  spawn 前 name→白名单比对）。
- **探针**：`tools_probe` 以 `<command> --version` 探测本机可用性（4s 超时、
  `CREATE_NO_WINDOW`、退出码 0 或有版本输出即算可用），面板显示 ✓ 版本 / ✗ 未安装。
- 命令：`tools_list / tools_add / tools_remove / tools_probe`。
- 命令栏动词：`tools [list | add <名> [命令] [描述] | probe <名> | remove <名>]`。

## 3. SKILL（技能文档）

`skills.toml` 条目（markdown 正文内联，纯配置、无外部文件依赖）：

```toml
[[skills]]
name = "pdf-build"
description = "用 pandoc 编译 PDF"
enabled = true
content = """
# 步骤
1. pandoc in.md -o out.pdf
"""
```

- **语义**：面向 agent 的说明书——注入 prompt 的上下文块（注入点后续接
  引擎 kb / plan 的上下文组装；与 kb 的关系：kb 面向「项目既有代码检索」，
  SKILL 面向「沉淀的操作知识」，互补不互替）。
- 面板：列表（启用态）+ 编辑器（名称 / 一句话描述 / markdown 正文 / 保存），
  新建 / 删除；改名 = 新条目（name 是唯一标识）。
- 命令：`skills_list / skills_save / skills_remove`；命令栏动词 `skill list`
  （编辑走面板）。

## 4. 顶部「能力」菜单与导航

- 菜单栏「能力 ▾」（项目 / 配置 / **能力** / 语言 / 帮助），四个子菜单
  `data-cap="mcp|a2a|tools|skills"`，点击 → 激活导航「能力」面板并切到对应子页。
- 导航区**单个**「能力」tab：面板内部以子标签（MCP / A2A / 工具 / 技能，
  `.cap-sub-tab` / `#cap-sub-*`）切换四个子面板（`ui/scripts/capability.js`，
  `ToolsUI` / `SkillsUI`）——四个独立 tab 会让导航膨胀到 9 个，收拢为一个。
- **智能体（agent）只在打开项目后可用**（引擎任务依赖项目上下文）：
  `setNavigatorMode("projects")` 隐藏「智能体」tab 并收起控制台；
  命令栏 `agent` 动词同样门控（未开项目 → 提示先开项目）。
  能力面板不受限（工具白名单 / SKILL 有全局层，无项目也可管理）。

## 5. 测试与门禁

- capability.rs 单测：`effective_command` 回落、两层文件读写与合并、
  markdown 多行正文 roundtrip；live 探针测试（python，默认 ignored）。
- ui-smoke（24 项）：工具/SKILL 面板锚点、能力菜单四子项、tools_/skills_
  命令注册、`tools`/`skill` 动词路由。
- 浏览器实测：菜单联动激活、两面板渲染、无后端提示正常。

## 6. 后续（P5 候选）

- 引擎接入：generate/verify 的命令白名单硬校验；SKILL 注入 plan 上下文。
- 能力总览视图（四类聚合 + 启用统计）；SKILL 从模板导入。
