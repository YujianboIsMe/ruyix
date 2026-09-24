# v1.0 测试发现的 bug（用户实测，2026-09-25）

1. 英文界面，配置很多中文
2. 英文界面，Skill标签页，【暂无技能...]是中文，此外，tools, A2A, MCP都有中文
3. 打开配置后，再从标题栏切项目，配置标签页只显示一半宽度，还只位于右边。#config-view元素宽度变成原来的一半。
4. Terminal是固定的目标，应该改成可添加的。
5. 给agent发你好时，报错：[warn] [agent] 第 1 轮输出无法解析：content 里没有可执行的工具调用。本引擎严格只认工具调用：动作请调 read / write / execute / connect / plan / ask_user / final；如果你是要交付，请调用 final 工具（不要把答复写成 content 里的 JSON）。
[warn] [agent] 第 2 轮输出无法解析：content 里没有可执行的工具调用。本引擎严格只认工具调用：动作请调 read / write / execute / connect / plan / ask_user / final；如果你是要交付，请调用 final 工具（不要把答复写成 content 里的 JSON）。
[warn] [agent] 第 3 轮输出无法解析：content 里没有可执行的工具调用（另：本次输出里有 6 处模型自带的工具调用标记，已剥掉 —— 那种标记不要再发）。本引擎严格只认工具调用：动作请调 read / write / execute / connect / plan / ask_user / final；如果你是要交付，请调用 final 工具（不要把答复写成 content 里的 JSON）。
一直无限死循环。

---

# 处理记录（2026-09-25）

## ✅ 5. agent 无限死循环 —— 已修（功能性阻断，先修这条）

**根因（读代码得到的确定结论）**：严格模式（`ai.tool_protocol`，默认开）下，
「这一轮没有工具调用」只会**回一句纠正再 `continue`**，唯一的兜底是 `MAX_STEPS`（96 轮）
与墙钟闸（30 分钟）。于是「模型坚决不发工具调用」这类病会把整轮预算烧光 ——
用户看到的就是**一直转**（第 3 轮那 6 处自带标记说明模型**算出了动作**，只是用了它自己那套协议，
而服务端没把它解析进 `tool_calls`）。

**改法**（`crates/harness-engine/src/agent/tool_loop.rs` + `agent.rs`）：

1. **连续失败上限** `MAX_UNPARSEABLE_ROUNDS = 3`：连着三轮拿不到工具调用就**停下来并失败**
   （不是静默成功）。成功一轮即清零 —— 偶发一次格式烂不该累计成"病"。
2. **收尾诊断**（`unparseable_diagnosis`，纯函数可单测）给三样东西：连续几轮；最近一轮的
   content 片段（截 240 字符，**带自带的标记计数**）；**两条出路** —— 换支持 tools 的模型/端点，
   或一行回滚 `ruyix.code.ai.tool_protocol = false`。
3. **回归测试** `content_only_model_stops_after_the_cap_instead_of_spinning`：脚本化假 LLM
   连发 4 条纯聊天 → 断言 ①三轮内就停（不消费第 4 条）②返回 `Err` ③诊断里带「连续 3 轮」
   「tool_protocol」「最近一轮原文」。

**如果还要用现在这个模型**：把 `ruyix.code.ai.tool_protocol` 设成 `false`（动作改走 content 里的
JSON，这是 v0.0.6 之前的协议，那条路已实测可用）—— 代价是模型自带标记更容易泄露。
**注意**：日志里那 6 处标记说明该模型/网关**没把 `tools` 变成 `tool_calls`** —— 换一个支持
function calling 的端点会更省事。

## ✅ 1 & 2. 英文界面露出中文 —— 已修（79 处）+ 新增门禁

**根因**：i18n 的 en 侧**本来就没有中文**（501 个键里只有 `menu.lang.zh = 中文` 一处，那是语言名），
所以界面上的中文只可能来自**写死的字面量**：面板一直用 `L(zh, en)` 这套双语内联，但
`status("…")` / `setStatus("…")` / 模板占位 / 弹窗标题这些出口漏了很多。

**改法**：把 8 个前端文件里**会显示给用户**的中文全部改走 `L(zh, en)`（与既有面板同一套写法），
共 79 处 —— `capability.js`（工具/Skill 面板）、`mcp.js`、`a2a.js`、`session.js`、`main.js`
（运行目标面板 / 文件树占位 / 重命名弹窗）、`command.js`；`main.js` / `command.js` 补了同款
`const L = (zh, en) => …`。

**门禁**（`scripts/ui-smoke.js` 新增 **U49** `i18n-no-hardcoded-cjk`）：静态扫 8 个面板 JS 的
"显示出口"（status / setStatus / innerHTML / textContent / title / showPrompt / showConfirm），
裸写中文即红；注释、`L(...)` / `I18N.t(...)`、`data-i18n=` 属性不算违反。
它是**真的有用的**：加完这条门禁当场又抓出 4 处我第一遍漏掉的（`已记住并创建运行目标: .` 那一家）。

**没做的一截（如实说）**：**后端（Rust）返回的消息仍是中文**（`src-tauri/src/main.rs` 127 处、
`config.rs` 41 处）—— 它们在 English 界面下照样会露出来（典型的：配置表单保存/应用失败时的
报错、部分命令的执行错误）。这一截要动的是「错误码 + 前端文案表」的改造，不属于本次五个 bug 的
范围，**登记为下一批**（改动面大：168 条消息 + 前端映射表 + 一批既有测试的断言文案）。

## ✅ 3. 切项目后 `#config-view` 只剩一半宽、靠右 —— 已修（改成结构性互斥）

**根因（读代码 + 拿 HEAD 里未修的那份 main.js 复现，钉死了）**：`#editor-body` 是 flex 行、
8 块面板各 `flex:1`；而 `showConfigView()` 只关编辑器自己那两块，**不关 `#service-view`** ——
切项目时服务面板会跟着新项目**自己刷新显示**，于是两块并排各占一半宽度，`#config-view` 在 DOM
里靠后 ⇒ 出现在右半边。复现输出（未修的代码）：`showServiceView()` 之后
`[service-view, config-view]` 同时亮着；再来一块会话面板就是三块并排。

**改法**：收成唯一入口 `showPane(id)`（**先全关，再点亮一个**），`show*` / `hide*` 全部过它
（editor-empty / editor-view / session / config / service / proc-log / terminal / image）。
"只有一块可见"从此是结构性成立，不再依赖每个调用点记得关谁。

**门禁**：`ui-smoke` **U50 pane-exclusive** —— 把真实触发路径排成序列（配置 ↔ 服务交替 + 会话），
每步只许一块可见；并核对 `EDITOR_PANES` 覆盖 `#editor-body` 里全部 8 块面板（漏一个就留一条并排路径）。
这条在修之前**必红**。

- 相关代码：`ui/index.html:304`（`#config-view`，`style="display:none"`）、
  `ui/main.js:3753`（`showConfigView()`）、`:530`（切标签时调它）；项目切换会走 `teardownProject()`
  + `setNavigatorMode()`，重排的是 `#project-workspace` 里的 `project-layout`（左导航 + 右编辑区两栏）。
- 定位过程留档：先按"两块并排 ⇒ 各占一半、靠后者在右"的假设读代码，再拿**未修的 main.js**
  跑同一序列把复现输出打出来（上面那三行 ✗），最后才动手 —— 没有靠猜着调 CSS。
- 真几何（谁占多少像素）继续由既有浏览器探针守着：U32 编辑器布局 / U43 终端几何 / U47 宽行，
  它们在本轮 326 项里全绿，说明这次改动没有动到任何一块面板自身的尺寸。

## ✅ 4. 终端目标要可添加 —— 已做（与「运行目标」同一套形态）

**改法**：

- **后端**：`config::scan_target_file(file, section, project_root)` —— 把运行目标的扫描器**抽成一份共用的**
  （具名目标 = `<key>.name` / `<key>.cmd`，运行目标多一个 `.bind`），`load_run_targets` 与新增的
  `load_term_targets`（`term.toml`）都调它；新命令 `get_term_targets` 已注册。
  *为什么不让两份复制*：复制的那份漏改不会有任何测试失败，只会在某个面板上表现为"加了不显示"。
- **前端**：导航区「终端资源」加 ➕ 添加入口；用户条目由 `get_term_targets` 渲染（内置的
  PowerShell / Cmd / … 仍在前面，用户加的自己往下排）；每条带 🪟（新窗口）/ ✎（改）/ 🗑（删）。
  增删改**全部走命令系统**：`term add <名字>=<命令>`（同名即改，避免"改一次名字多出一条"）、
  `term del <key|名字>`、`term list`；写盘落项目配置 `ruyix.code.term.target<N>.name/.cmd`。
- **门禁**：`ui-smoke` **U51 terminal-targets** —— 添加入口存在 + 用户条目走 `get_term_targets` +
  写入全走命令系统（面板不许直接 `config_set`）+ 后端与运行目标**共用同一份扫描器** + 中英文案齐备。

- 原来的样子（留档）：终端列表是 `index.html` 里**写死的几条**（PowerShell / Cmd / WSL / Claude /
  Python / Node.js / Git Bash），`spawn_terminal` 直接拿命令起窗口 —— 想加一个自己的环境
  （某个 venv 的 shell、某台机器的 ssh、带一长串参数的工具）只能去改文件。这几条**保留**，
  用户加的排在它们后面。
