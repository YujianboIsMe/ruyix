# MCP 与 A2A 集成设计（v0.1）

> 2026-09-19。同批变更：**智搜（rag.rs，qdrant-edge + 在线 embedding）彻底移除** ——
> 本地嵌入模型不可得，语义索引对个人 IDE 属重量依赖；代码搜索回归编辑器原生能力。
> 移除面：`src-tauri/src/rag.rs`、main.rs 的 10 个 `rag_*` 命令、菜单栏「智搜」与
> 启用弹窗、`search` 命令动词、i18n 23 键、`qdrant-edge` 依赖、`doc/rag.md`。
> 腾出的定位由 MCP（工具生态）与 A2A（agent 协作）承接。

## 1. MCP（Model Context Protocol）客户端

**定位**：ruyix 作为 MCP **host/client**，把本地 MCP 服务器的工具接进 IDE。

**协议面**（工具子集，resources/prompts 留待后续）：

```
ruyix (mcp.rs McpManager)
  └─ spawn 子进程（stdio，JSON-RPC 2.0，每行一个 JSON）
       ├─ initialize（protocolVersion 2024-11-05）→ notifications/initialized
       ├─ tools/list → ToolInfo[]（缓存于连接对象）
       └─ tools/call(name, arguments) → content[]（text 拼接）+ isError
```

- 读线程逐行匹配 `id → oneshot`；请求超时 120s、握手 30s；服务器退出唤醒全部挂起请求。
- `Conn` Drop 即 kill 子进程 —— IDE 退出不留孤儿进程。
- Windows 用 `CREATE_NO_WINDOW`（与 PTY/运行目标同语义）。

**配置**：`~/.ruyix/code/mcp.toml`（全局）+ `<root>/.ruyix/code/mcp.toml`（项目，
同名覆盖），与 projects.toml 同一套结构化文件惯例；配置编辑器排除显示
（`SCOPE_EXCLUDED_FILES`）。

**命令**（7 个）：`mcp_servers / mcp_add_server / mcp_remove_server / mcp_start /
mcp_stop / mcp_tools / mcp_call_tool`。

**前端**：导航「MCP」面板（`ui/mcp.js`）——服务器列表（启停态/工具数）、添加表单、
连接/断开、工具列表、调用表单与结果区；命令动词 `mcp [list | call <srv> <tool> {json}]`。

**测试**：配置合并纯函数 + tools 解析；live 测试 `mcp_live_python_echo_server`
（python 起最小 MCP 服务器，真·stdio 往返，默认 ignored：`cargo test -p ruyix mcp_live -- --ignored`）。

## 2. A2A（Agent-to-Agent）客户端

**定位**：ruyix 作为 A2A **client**，发现远端 agent 并委托任务。

**协议面**：

```
发现：GET {base}/.well-known/agent-card.json（0.3+）→ 回退 agent.json（0.2.x）
委托：POST {base}/  JSON-RPC message/send {message:{role:"user", parts:[{kind:"text"}]}}
     → 响应 = task（轮询 tasks/get 至终态）或 message（同步直答，即完成）
终态：completed | failed | canceled | rejected | unknown
文本：artifacts[].parts[].text 优先，其次 status.message，最后 message.parts
```

- 轮询 1.5s × 200（约 5 分钟），进度经 `a2a://status` 事件推给面板；SSE
  （tasks/subscribe）留待后续。
- messageId 用 `ruyix-<pid>-<seq>`（无 uuid 依赖，足够唯一）。

**配置**：`~/.ruyix/code/a2a.toml`（卡片摘要缓存：name/url/description/version/skills）。

**命令**（4 个）：`a2a_agents / a2a_discover / a2a_remove / a2a_send`。

**前端**：导航「A2A」面板（`ui/a2a.js`）——agent 卡片列表（技能 chips）、发现表单、
任务委托（状态行 + 结果区）；命令动词 `a2a [list | send <name> <text>]`。

**测试**：card 映射 / 文本提取优先级 / 终态判定纯函数；`send_task` 走本地
TcpListener 手工 HTTP mock 真往返（hermetic，默认跑）。

## 3. 依赖与验收

- 依赖**零新增**（mcp/a2a 用 std + serde + reqwest + tokio）；qdrant-edge **移除**。
- 门禁：fmt ✓ / clippy 0 warning / 引擎 159+8 ignored / ruyix 48+1 ignored /
  check-style 0 error / ui-smoke 23 项（含 MCP/A2A 锚点、命令注册、事件契约、
  mcp/a2a 动词断言）。
- 浏览器验收：两面板渲染/无后端提示正常，菜单栏「智搜」已移除。

## 4. 后续（P5 候选）

- MCP 工具注入 agent 引擎（plan/generate 的工具调用循环）与 ai.rs 命令翻译。
- A2A server 侧：把 harness-engine 的 plan→generate→verify 管道暴露为 A2A endpoint。
- MCP 的 Streamable HTTP 传输；A2A 的 SSE 流式订阅。
