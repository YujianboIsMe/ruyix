# v0.0.5 — 会话执行轨迹

Windows x64 绿色版：下载 `ruyix.exe` 双击即用，无需安装。

---

## 中文

### 本版重点：AI 执行过程看得见了

以前 agent 跑的时候，对话气泡里只有三个点，看不到它在做什么。现在思考与执行
**交错**成一条时间线，一行一条，宽度不够就用省略号收尾（悬停可看全文）：

```
[思考] 正在读取 src/main.rs
[执行] 第 3 轮 write 完成 —— 已写入 ui/styles.css
[失败] 第 4 轮 execute 退出码 1
[完成] 全部结束，共 6 轮
```

轨迹随会话一起落盘，重启后仍然可见。

引擎与提示词零改动 —— 这些行全部来自引擎自己已经发出的事件（每轮工具调用日志、
阶段通告、模型亲口声明的计划），没有编造思维链。

### 本版还包含

**Agent 引擎**（`crates/harness-engine`，零 Tauri 依赖）
- 「规划 → 生成 → 规约 → 验证 → 自修复」闭环；工具循环四种原子能力：读 / 写 / 执行 / 连接
- 计划即执行：父循环一轮 = 一个步骤，调度权归引擎；步骤终态按事实判定，新增「未对齐」态
- 干净上下文的复核 agent（只读、结构化结论、结论回灌主循环），命令输出对复核员可见
- 一轮多个原语调用（波次并发）；`ask_user` 用于需求歧义
- 托管进程：agent 起的常驻服务有生命周期出口，看得见、也停得掉

**编辑器**
- 虚拟化渲染 —— 重建量不再随文件变长
- 高亮载荷瘦身 8.5 倍；5000 行的高亮索引线性化
- 修掉双滚动条；含中文 / emoji 的行高亮不再整体错位
- 语法高亮：tree-sitter + arborium（Python、Rust、HTML、CSS、JavaScript、Markdown、SQL、Java）

**模型接入**
- LLM 调用层重做：故障切换（可配 AI 网关）+ Anthropic 协议支持
- 服务端联网搜索：DeepSeek 端点自动走 `/responses`；联网开关**逐模型**按能力决定

**配置与其它**
- 全局 / 项目 / 运行 三作用域配置页；引擎配置键单源化（schema 派生的桥与表单）
- 服务面板；终端输出增量跟随（`service log <pid>`）
- 会话历史活过重启；帮助正文改为 markdown 源文件驱动
- 新的 RYX 应用图标

### 下载与运行

| 项 | 说明 |
|---|---|
| 平台 | Windows x64 |
| 产物 | `ruyix.exe`（约 34 MB，绿色单文件） |
| 安装 | 无需安装，双击运行 |
| 依赖 | 系统 WebView2（Windows 10 / 11 通常自带） |
| 数据 | `<项目>/.ruyix/`、`~/.ruyix/` |

本版**不提供安装包**（NSIS / MSI）：打包需要从 GitHub 拉取 NSIS 工具链，本次未能完成，
只发布绿色版 exe。不影响独立运行。

### 已知问题

- 程序元数据里的版本号仍是 `0.1.0`、产品名仍为 `Darkhorse Code`，尚未与本版号对齐
- 同一会话再跑一轮时，上一轮已展开的写回差异面板会被重渲染冲掉（改动前就存在）

### 质量状态

全部门禁通过：前端静态契约与真事件回放、无头浏览器几何探针、clippy 零警告、
`harness-engine` 322 + 宿主 120 单测全绿。

---

## English

### Highlight: you can now see what the AI is doing

Previously, while the agent was running, the message bubble showed nothing but three dots.
Now thinking and execution are interleaved into a single timeline, one line each, with an
ellipsis when a line does not fit (hover to see the full text):

```
[think] reading src/main.rs
[do]    round 3: write ok — wrote ui/styles.css
[fail]  round 4: execute exited with code 1
[done]  finished, 6 rounds in total
```

The trace is persisted with the session and survives a restart.

The engine and its prompts were left untouched — every line comes from events the engine
already emitted (per-round tool-call logs, stage announcements, the model's own declared
plan). No chain-of-thought is invented.

### Also in this release

**Agent engine** (`crates/harness-engine`, zero Tauri dependency)
- Plan → generate → specify → verify → self-repair loop; four atomic capabilities:
  read / write / execute / connect
- Plan-as-execution: one parent-loop round equals one step, scheduling owned by the engine;
  step end-states decided from facts, with a new "misaligned" state
- A clean-context reviewer agent (read-only, structured verdict fed back into the main loop);
  command output is visible to the reviewer
- Multiple primitive calls per round (wave concurrency); `ask_user` for ambiguous requirements
- Supervised processes: long-running services started by the agent now have a lifecycle exit —
  visible and stoppable

**Editor**
- Virtualized rendering — rebuild cost no longer grows with file length
- Highlight payload 8.5x smaller; highlight index linearized for 5000-line files
- Fixed the double scrollbar; highlight offsets no longer shift on lines with CJK or emoji
- Syntax highlighting: tree-sitter + arborium (Python, Rust, HTML, CSS, JavaScript, Markdown, SQL, Java)

**Model access**
- Reworked LLM call layer: failover (configurable AI gateway) + Anthropic protocol support
- Server-side web search: DeepSeek endpoints use `/responses` automatically; the toggle is
  **per-model**, decided by capability

**Configuration and more**
- Global / project / run scopes in the config page; engine config keys single-sourced
  (bridge and form derived from the schema)
- Service panel; terminal output followed incrementally (`service log <pid>`)
- Session history survives restarts; help content is now driven by markdown source files
- New RYX application icon

### Download and run

| Item | Detail |
|---|---|
| Platform | Windows x64 |
| Artifact | `ruyix.exe` (~34 MB, portable single file) |
| Install | Not required — double-click to run |
| Requires | System WebView2 (usually preinstalled on Windows 10 / 11) |
| Data | `<project>/.ruyix/`, `~/.ruyix/` |

**No installer is provided** (NSIS / MSI) in this release: packaging requires pulling the
NSIS toolchain from GitHub, which did not complete here. The portable exe is unaffected.

### Known issues

- Binary metadata still reports version `0.1.0` and product name `Darkhorse Code`,
  not yet aligned with this release number
- Running a second round in the same session re-renders the message list and discards an
  already-expanded write-back diff panel (pre-existing behaviour)

### Quality status

All gates pass: frontend static contracts and real-event replays, headless-browser geometry
probes, clippy with zero warnings, and 322 engine + 120 host unit tests green.
