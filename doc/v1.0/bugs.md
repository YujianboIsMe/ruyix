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

### ✅ 5 的真病因与真修法（2026-09-25，用户点破）

上面那三条（上限 + 诊断 + 回归测试）是**刹车**，不是修。用户一句话点到位：
**"不支持 anthropic 接口 tool_use 你改了吗？如果是 anthropic 接口时，走 anthropic 的工具调用，
这才是正确的改法"** —— 他是对的，代码里当时就这么写着：

- `llm.rs::extract_anthropic`：`// anthropic 的 tool_use 块本次不映射：这条路保持老协议`
- `llm.rs::anthropic_parts`：**一个 `tools` 都不声明**（`body["tools"]` 只出现在 OpenAI 的
  `/chat/completions` 分支）

⇒ `api_format = anthropic` + 严格模式（默认开）= 模型**根本拿不到工具**，只能把动作写进正文，
而正文里的动作一律作废 —— 于是每轮都"没有工具调用"，直到烧完预算。**这就是"发一句你好
无限死循环"的真病因**，与模型无关、与我们自己的请求有关。同理还有 `/responses`（web_search 那条路）：
那里**只声明了 `web_search`**，一个函数工具都没有 —— 同一个病的另一处。

**修法**（三条协议各自形状，一处都不能想当然）：

| | 请求里怎么声明 | 响应里怎么解析 | 回灌形态 |
|---|---|---|---|
| OpenAI `/chat/completions` | `{type:"function", function:{name, description, parameters}}` | `choices[0].message.tool_calls[]`（`arguments` 是 JSON **字符串**） | `role="tool"` + `tool_call_id` |
| **anthropic** `/v1/messages` | `{name, description, input_schema}` —— **没有** `function` 外壳 | `content[]` 里 `type=="tool_use"` 的块（`input` 是**对象**，id 在 `tool_use.id`） | assistant 用 `tool_use` 块；结果**没有** `role="tool"`，要包成 user 消息里的 `tool_result` + `tool_use_id` |
| **`/responses`** | `{type:"function", name, description, parameters}` —— **扁平**，与上面那个不同 | `output[]` 里 `type=="function_call"`（`call_id` / `arguments` 是字符串） | 文本回放（不需要 `function_call_output`） |

**判据**（`crates/harness-engine/src/llm.rs::protocol_tests`，6 条，跑 `cargo test -p harness-engine`）：

- anthropic 声明：`tools[].input_schema.required` 到位、**没有** `function` 外壳（混错就是 400）；
- anthropic 不声明工具时**不发空 `tools` 数组**（有的网关对 `tools: []` 直接 400）；
- anthropic 解析：`tool_use` 块 → 可用调用（参数从对象转成我们的 JSON 字符串、id 取 `tool_use.id`）；
- anthropic 回灌：assistant 用 `tool_use` 块、结果挂 user 消息的 `tool_result`；
- `/responses` 声明：`web_search` 与函数工具**共处一个数组**、扁平形态；
- `/responses` 解析：`function_call` 项 → 可用调用（且内部 `ws_call_id=` 标记不被当成查询词）。

**刹车留着**：`MAX_UNPARSEABLE_ROUNDS`（连续 3 轮）与诊断现在管的是**别的原因**（真不发工具调用的
模型/网关），仍是对的兜底 —— 但它不再是这条路的主治。

### ✅ 真机验证（2026-09-25，用用户自己的端点）

用户环境实测：`--example llm_tool_probe -- --config target/debug/global/ai.toml`
（配置：`api_format = "anthropic"`、`api_url = https://api.deepseek.com/anthropic`、
`model = deepseek-v4-flash`、`tool_protocol = "true"` —— **正是这个 bug 的现场**）。

| 臂 | 任务 | 结果 |
|---|---|---|
| A | `你好`（用户实测的复现句） | **5 轮工具调用（标准协议）· 0 轮无法解析**；`完成，共 5 轮` |
| B | `读一下 hello.txt 第一行并原样告诉我`（**必须用工具**） | 第 1 轮就 `read hello.txt`；答复里含文件里那句 `probe-line-42` ⇒ 工具真跑了、结果真回到模型 |

修之前是这个样子（用户原日志）：第 1/2/3… 轮一路「输出无法解析：content 里没有可执行的工具调用」，
直到烧完预算 —— **同一条端点、同一个模型、同一句话**。

探针（`crates/harness-engine/examples/llm_tool_probe.rs`，已入库）把判据预注册在文件头：
A 臂不许出现「无法解析」且必须出现「工具调用（标准协议）」；B 臂还要求答复里含文件里的密语
（只跑 A 臂是不够的：模型"不发工具、直接用 final 交付"也能让 A 过 —— 那说明不了工具通不通）。
两条臂都退 0 才算过。**以后换端点/换模型，先跑它，不要靠感觉。**


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

### ✅ 收尾：后端（Rust）消息也翻了 —— 显示层收口 + U52 覆盖门禁

**问题**：前端清完之后还剩一截 —— **后端返回的消息是中文**（宿主 + 引擎），英文界面下照样会露出来
（典型：配置表单保存/应用失败的报错、部分命令的执行错误）。

**为什么没改后端**：把它改成「错误码 + 参数」是一大改 —— 168 处调用点、一批既有测试的断言文案、
宿主与引擎两套口径。而**显示层只有一个收口**（`setStatus`），所以翻译落在那里：

- 新增 `ui/errors.js`：三层翻译（精确对照 → 句型规则 → 认不出就**原样返回**，信息不丢）。
  句型规则配两张词典（动词 70+ / 名词 50+，短语**逐词替换**），所以 `读取产物目录失败 {}: {e}`
  这种带插值的句子也能翻成 `read the artifact directory failed: {}: {e}`。
- 接在 `setStatus`（状态栏唯一出口）；前端自己拼的 `"X 失败: " + 后端消息` 由"**从左往右**找
  第一个能翻干净的分界"处理（取最后一个分号会错 —— 后端那句自己可能带冒号）。
- **覆盖门禁 U52**：逐条扫后端源码里 `Err(...)` / `map_err` / `ok_or` / `bail!` 里的中文字面量，
  要求 `translate(原文) !== 原文`。**没有这条门禁，这张表会在第一次"随手加个错误"时开始腐烂，
  而且没有任何症状**（只在英文界面下偶尔冒出一句中文）。

**过程留痕（都能复跑）**：第一版规则里 `\\s` 被双重转义成"字面反斜杠 + s"，**规则全都没匹配上**，
门禁直接报 106 条"翻不出来" —— 这坑正是写门禁的理由（不然表现只是"英文界面偶尔冒中文"）。
随后 141 → 115 → 106 → 88 → 41 → 7 → **0**，每一步都由门禁数出来。
最终**逐条全覆盖**（330/330），且没有任何 Rust 侧改动（既有测试一行没动）。

**这条路的边界（如实说）**：翻译键是**中文原文**，所以后端改文案时会有两条纪律 ——
① 只改中文措辞而不动语义，门禁会红并直接把那条打印出来（补一条词典/对照即可）；
② 想彻底摆脱"文本即键"，将来可以换"错误码 + 前端文案表"，那时这张表可以整体退场。

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
