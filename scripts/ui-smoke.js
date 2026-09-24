#!/usr/bin/env node
/**
 * UI 冒烟测试（融合计划 P4：ui-smoke + 面板场景）
 *
 * 为什么是自研脚本：本项目禁止 npm 依赖（无打包器），装不了 jsdom / playwright；
 * Tauri 窗口又无法从外部驱动。因此分两层覆盖：
 *   1) 契约静态断言 —— 前后端契约（DOM 锚点 / 命令动词 / 事件名 / 命令名 / i18n）
 *      的文本级一致性，任何一侧改名都会在这里红；
 *   2) 面板回放 —— 用微型 DOM stub 在 Node 里加载 ui/session.js、ui/config.js，
 *      驱动其真实交互路径（发消息 / 扫描配置 → 改 → 保存 / 应用 / 取消），断言渲染与出参。
 * 真实引擎链路（真调 LLM）由 harness-engine 的 example / 单测覆盖，不在本脚本。
 *
 * 用法：
 *   node scripts/ui-smoke.js            # 全部场景，有 FAIL 则退出码 1
 *
 * 场景（U = ui-smoke 规则编号）：
 *   U1 panel-dom      index.html 拥有全部面板 DOM 锚点 / 导航标签 / 脚本引用
 *   U2 panel-verb     command.js 的 agent/mcp/a2a/tools/skill 动词路由到各面板
 *   U3 event-contract sink.rs emit 的 agent://* 必需事件 ⊆ session.js 监听集；a2a://status 两端一致
 *   U4 cmd-contract   附录 A + mcp/a2a/tools/skills 命令全部注册；面板 invoke ⊆ 注册集
 *   U5 i18n-parity    zh-CN / en 的 key 集合一致，且覆盖 index.html 全部 data-i18n*
 *   U6 session-module session.js 暴露 SessionUI 且核心方法齐全
 *   U7 session-demo   无后端演示对话：新建会话 → 发消息 → 用户气泡 + 演示回复 + 列表刷新
 *   U8 session-close  会话列表渲染与空态、无后端环境芯片
 *   U9 config-form    配置表单的 DOM 锚点 / config.js 加载 / config 子动词路由
 *   U10 config-contract 表单命令注册 + SCHEMA 字段 i18n 齐全 + window.state 已导出
 *   U11 config-replay  扫描 → 表单（已知项 / 扫描项 / 继承提示 / 密钥掩码）
 *   U12 config-save    保存只提交改动行；应用提交整表（含未改动行）
 *   U13 config-cancel  取消重扫丢弃改动；无项目不扫 project；已打开项目必须能扫 project
 *   U14 apply-writeback 产物写回真实项目（三模式：确认=暂存面板人工写入，写入/自主=循环直写）：
 *                      暂存落盘唯一入口 applyStage 恒备份、stage.rs 路径封闭、apply.rs 三条安全约束在
 *   U15 outline-plan  任务计划进大纲区：plan 事件 → ✅ ⌛ ⛏️ ⚠️ 六态 + ❌ 失败 → 会话 tab 渲染
 *   U15b plan-settle  收尾落定：每个步骤都要有终态（沙漏是"等待执行"，run 结束还挂着就是骗人）；
 *                     口径要宽 —— 声明了但本来就存在的文件不算缺，有产出但对不上声明落 ⚠️ 不落 ⏹️
 *   U15c plan-execute 计划即执行（step.execute_plan，默认开）：游标在引擎手里，派发前发 running，
 *                     失败落 error 并停下给模型一轮干预（配 false 即回到旧的纯展示行为）
 *   U15d plan-persist 计划要活着跨过一次重启：工具循环不落 RunRecord（run_id 恒空），
 *                     计划（含终态）与验证/复核结论只能跟着助手消息落盘 —— Rust 侧不声明
 *                     这些字段，agent_session_save 的往返会当场把它们抹掉
 *   U16 agent-loop    会话 = 工具循环（Read/Write/Execute/Connect 四大原子能力），无问答/任务预分类；
 *                     Connect 必须真接上宿主（mod.rs 建连接器 → connect.rs 落 MCP/A2A）
 *   U19 cmd-discover  命令发现（v0.5）：本机有哪些命令必须实测后写进上下文 —— 一张数据表 +
 *                     两步探测（where/command -v 解析 + 过 shell 取版本，Windows 的 .cmd 不被
 *                     CreateProcess 认）+ 可用与不可用都写 + 主循环与子步骤都注入
 *   U20 env-install   环境准备（v0.5）：缺失工具按需安装 —— **不是新原语**，走 Connect；
 *                     引擎零分支（ENV_CONNECTOR_KIND 约定 + 缺失指向 connect），包管理器知识
 *                     全在宿主一张表；每次动作留记录（jsonl + UI 事件）；安装命令必须过 shell
 *   U21 exec-safety   执行前闸门 + 输出按代码页解码（v0.6）：`\`、`\admin-run\` 这类"不是命令"
 *                     的字符串不许再原样交给 cmd（真控制台里 start 系会弹桌面窗、输出全丢）；
 *                     中文 Windows 的 GBK 输出必须解成可读中文（否则模型读不懂自己的失败）
 *   U22 help-markdown 帮助正文的**源**是 markdown 文件（ui/help-zh.md / ui/help-en.md），打开时
 *                     用 markdown-it 渲染进 #help-body —— 不再是手写 HTML 表格，也不再塞进
 *                     只读代码编辑器；旧的字符串拼接（getHelpText）必须删干净
 *   U23 proc-lifecycle 永不退出的服务（v0.6）：**不是第五原语**，是 execute 的第三个生命周期
 *                     维度 —— 后台起 + 就绪判据是**一条命令**（引擎零 app 知识）+ 句柄 op=status/
 *                     log/stop。三个出口各带证据；起之前先探一次判据（别人已满足就别起，防孤儿）；
 *                     引擎持有就引擎收（run 结束按项目收，宿主退出全收），杀必连子进程树
 *   U24 external-link  agent 回的链接不能把 IDE 顶掉（P0）：整个 IDE 活在一个文档里，WebView2 对普通
 *                     导航的默认动作就是在**本 WebView 里**导航过去 —— 文档一换状态全没。主窗口因此必须
 *                     建在 Rust 里（配置窗口挂不上 on_navigation）；判定只有"是不是自家文档"三结局
 *                     （放行 / 交给系统浏览器 / 拒掉），**localhost 不是自家页面**；出口 open_external
 *                     只放行 http/https/mailto 并交给系统默认处理程序（不走 shell）；前端捕获阶段另有
 *                     一重拦截，判定表与后端逐条对齐
 *   U25 batch-calls   一轮多个调用（v0.7）：模型一次发一批互不依赖的调用（{"actions":[…]}），
 *                     引擎把**一批里的调用并发跑**（只有同一条路径上的写与读、托管进程的起停查按
 *                     声明顺序排），结果按同一顺序一起回灌 —— 省的是**轮次**（实测一次读 5 个文件
 *                     从 5 轮降到 1 轮）。
 *                     五环：批解析（两件外衣 / 上限不静默截断 / 控制动作当面拒）+ 不猜命令语义
 *                     （边界画在原语与路径上）+ 结果按声明顺序落位（错位是静默的）+ 回灌 results
 *                     数组逐条带 ok + 父子提示词各自自洽（子那份不许提 plan）
 *   U26 ask-user      第五个动作（v0.8）：需求歧义只能问委托人 —— 四个效果原语取不到"意图"
 *                     （read 读磁盘、execute 跑命令、connect 连机器，另一端都不是人），于是模型
 *                     只剩"猜"或"把问题塞进 final"（而 final 的语义是交付：把未开工记成已完成）。
 *                     模型侧叫 `ask_user`；引擎内部是控制动作（独占一轮 / 不进批 / 子步骤 Unsupported）；
 *                     答案以 kind=user 回灌且**不构成授权**；拿不到答案一律 fail-closed
 *   U28 session-persist 会话历史必须活过重启（v0.9）：文件一直在写（实测某项目里 27 个），
 *                     病根是**没加载** —— refreshList() 只在 attach() 末尾被调一次，而那一刻项目
 *                     往往还没打开 → 早退 → 面板永远"暂无会话"。五环：打开项目的收口点显式同步
 *                     （并把最近一条有消息的会话接上）+ 用户那句话立刻落盘 + 原子写（截断式写入
 *                     被中断会留半截 JSON，而 list 会跳过它）+ 坏文件只影响自己 + 空会话不落盘 /
 *                     删除幂等
 *   U29 persist-no-clobber 落盘不许**弄丢**会话内容（v0.10）：`persist()` 的回显同步只准同步
 *                     元数据（id/title/时间），绝不许 `Object.assign(s, saved)` —— 后端回显是
 *                     「请求发出那一刻」的快照，整体 assign 会把之后新 push 的消息从 `s.messages`
 *                     里抹掉。真凶现场：2026-09-21 20:13 那轮「飞机大战」盘上只剩
 *                     [用户任务, 写回回执]，助手回复连同计划快照一起没了 → 重启后大纲区的 ❌
 *                     与失败原因无从回看。另钉：每处落盘都要 await（同一会话共用同一个
 *                     `.json.tmp`，并发保存必有一次 rename 失败）
 *   U27 logo-assets   应用图标（RYX 字母标）有唯一真相源：几何只写在 tools/logo/build_logo.py，
 *                     由它同时产出 ui/logo*.svg 与 src-tauri/icons/*。钉四条：四个矢量变体齐
 *                     / logo.svg 与 logo-mark.svg 的轮廓逐字节相同（只改一份就是漂移）
 *                     / tauri.conf.json 的 bundle.icon 非空且文件真在（**缺它则换了图也不生效**，
 *                     窗口图标与 exe 资源都取自它）/ icon.ico 多尺寸
 *   U31 editor-virtual-render 编辑器只画可视窗口（v0.11）：5000 行的文件一打开就卡死 ——
 *                     gutter/backdrop 按**全文件行数**建 DOM（每行 ≈ 3.8 个节点，5000 行 1.9 万），
 *                     重建一次 200ms+，而它挂在自动保存上（打字停顿 1 秒跑一遍）。改法是只渲染
 *                     「可视窗口 + 上下各 24 行」，窗口外用两条零内容 spacer 撑出与原来
 *                     **逐像素相同**的滚动高度。三条不变量：占位高度之和 == 总行数×行高 /
 *                     textarea 必须拿到显式全文高度（backdrop 移出流后没人撑它）/ 水平滚动宽度
 *                     不许缩（textarea 对 scrollWidth 贡献恒为 0，靠一条零高「最宽行」占位撑住）。
 *                     另钉两条**静默退化**的样式契约：backdrop 必须绝对定位（回到 grid 叠加会让
 *                     layout 每次重问 textarea 的内在高度 → 25000 行重绘 1.4ms 变 110ms）、
 *                     textarea 必须 wrap="off"（软换行会让光标与高亮从折行处起错开）
 *                     P3 追加：高亮载荷是紧凑形状 { tags, lines }（名表只出现一次 + 每行扁平
 *                     三元组），且**行由前端按 tab.content 自己切** —— 后端 code.lines() 会吃掉
 *                     末尾空行，用后端的行拼 textarea 的值就会把文件末尾的换行弄丢（见下面那条
 *                     "末尾换行" 检查）
 *   U32 editor-layout-real  编辑器**真实布局**（v0.11）：整棵树里只能有一个滚动容器 ——
 *                     textarea 天生是滚动容器（Chromium 把作者写的 overflow:visible 当 auto），
 *                     正文一旦装不下自己那一格，它就会长出自己的滚动条，并与 .editor-view
 *                     那对叠成"水平和垂直都双滚动条"；同时它为了露出光标会**内部滚动**，
 *                     光标因此与背板字形错开。这个类别在 Node 的微型 DOM 桩里量不到
 *                     （没有布局引擎），所以挂一张真浏览器探针：scripts/editor-layout.js
 *                     用真实 index.html + styles.css + main.js 在无头 Edge 里开一页，
 *                     逐元素量 offset-client（只有真画出来的滚动条才占这几像素）。
 *                     本机没有 Edge/Chrome 时该脚本自行 SKIP。
 *   U33 ctx-copy-path   文件树右键的【复制路径 / 复制绝对路径】（v0.11）：相对路径相对**项目根**，
 *                     不是相对当前目录；项目根本身复制成 "." 而不是空串（复制出空串 = 剪贴板
 *                     被写成空、状态栏却报"已复制"，是最难发现的那种错）。两项必须对文件和
 *                     文件夹都出现 —— 挂上 file-only / folder-only 就等于在另一半节点上消失。
 *                     回放直接执行 main.js 里真实的 toRelativePath / copyToClipboard /
 *                     handleContextCopyPath 源码（切片 + new Function），不另抄一份。
 *   U34 autosave-no-ui   自动保存**只写盘**（v0.11）：着色是一趟全量 tree-sitter + 完整 IPC 往返，
 *                     大纲是一次 O(全文) 解析 + 整块 innerHTML 重建 —— 挂在每次打字停顿上，
 *                     等于把"停一下手"变成"跑一趟全量"。两者收敛到唯一出口
 *                     refreshEditorChrome，只挂切回标签页 / 失焦 / 显式保存；而且高亮没过期
 *                     就不再跑 IPC。输入只做本地"内容上屏"。
 *   U38 open-project-args `open project` / `open file` 的路径参数必须经**引号感知**的分词还原：
 *                     命令入口（handleCommand）是哑空白切分，而标题栏项目切换下拉发的是
 *                     转义过的 `open project "D:\\Projects\\x"`（escArg 翻倍 \ 与 "，约定
 *                     parseQuotedTokens 还原）—— 引号原样混进路径后端 exists() 必然 false，
 *                     实测 2026-09-23：下拉切项目从来没成功过。回放真加载 command.js 驱动
 *                     handleCommand：下拉形态转义串还原 / 手打带引号含空格路径 / 不带引号
 *                     原样透传 / UNC 打头双反斜杠不被转义规则吃掉，四个都要对。
 *   U39 failover-config  备用 LLM（故障切换网关）的**跨层键契约**：`config.js` 的 `ai_fallback`
 *                     段声明的字段顺序 = 落盘键 `ruyix.code.ai_fallback.<key>` = 宿主桥
 *                     `config_bridge.rs` 里 read 的键。三处只要有一边改名（例如把 ai_fallback
 *                     写成 ai_backup），界面照样长得对、保存也报成功，**配置却永远读不到** ——
 *                     正是 open-project 那类静默 bug 的形状。另外守两条单源纪律：
 *                     `llm_fallback` 整段在引擎 FORM_HIDDEN 里、不许被抄回 `config.js` 表单。
 *   U40 anthropic-format  LLM 协议二选一（OpenAI 兼容 / Anthropic Messages）的跨层契约：
 *                     ①`ai` 与 `ai_fallback` 两段各有一个 `api_format` 下拉、取值恰为
 *                     openai/anthropic；②引擎 `llm.rs` 真的实现了 anthropic 三件套
 *                     （`/v1/messages` 端点、`x-api-key`+`anthropic-version` 鉴权、
 *                     `extract_anthropic` 解析）；③`request_plan` **最先**判 anthropic ——
 *                     判晚了 anthropic 请求会被当成 OpenAI 端点包成 `/responses`；
 *                     ④合法值声明在引擎 `ENUM_KEYS` 且 `FORM_HIDDEN` 藏掉它（单源在宿主 ai 段）；
 *                     ⑤新增文案中英 parity（缺英文键界面会直接显示键名）。
 *   U41 session-trace  会话气泡要**看得见 agent 在干什么**（v0.0.5）：引擎一直在发
 *                     `agent://log`（每次工具调用一条 `第 N 轮 <tool> ✓ <摘要>`）与 `agent://stage`，
 *                     但原先的监听只改 `ph.text` 且不重渲染 —— 运行中的气泡于是永远停在 "…"。
 *                     四环：①两个事件必须被**渲染**成"一行一条"的轨迹（不是只监听）；
 *                     ②"一行显示不全用省略号"靠 `.session-trace-text` 的 nowrap+overflow+
 *                     text-overflow 三条声明同时在场，缺一条就退化成折行或撑宽；③轨迹要跟着
 *                     助手消息落盘（`SessionMsg.trace` 不声明就被 serde 抹掉，且必须有 default
 *                     否则老会话读不回来）；④回放一遍真事件路径，断言渲染出来的行、kind 分类、
 *                     落盘载荷里的 trace —— 顺带把"日志把气泡正文覆盖掉"那个老病钉死。
 *   U45 tab-context-menu 编辑器多标签的右键菜单（v0.11）：文件树早就拦掉了浏览器默认右键，
 *                     标签栏没拦 ⇒ 多标签上右键出来的是 WebView 的**系统菜单**（后退/刷新/
 *                     检查元素…）。判据：①`contextmenu` 必须 preventDefault（否则还是系统菜单）；
 *                     ②**右键不改激活标签**（目标由命中的 data-tab-id 决定，否则"关闭其他"会把
 *                     刚右键的那个也关掉）；③close/others/right/left/all 逐条对，单标签时
 *                     others/right/left 必须是空计划（菜单项据此隐藏）。切片真源码回放。
 *   U44 nav-layout-real  导航栏的**真实几何**（真浏览器）。方案（用户拍板）：**不做横向滚动** ——
 *                     长名走省略号，完整路径由行上的 `title` 悬停给出（两版横滚都被否：
 *                     `max-content` 撑宽会抖、`sticky` 钉按钮会压在长路径上）。判据：短内容与
 *                     长内容**都不该**出横条、长名确实走 `ellipsis`、行上有 `title` 且等于完整
 *                     路径、目录行的刷新按钮**右边缘对齐**且都在可视区内、长目录行的按钮不压名字、
 *                     纵向仍可滚。`scripts/nav-layout.js`，本机没 Edge/Chrome 时自行 SKIP。
 *   U43 terminal-layout-real  模拟终端的**真实几何**：`.xterm-screen` 的像素尺寸、"容器缩小时
 *                     终端跟不跟"、PTY 的 winsize 有没有同步 —— 桩里根本量不到。真机上出过事故
 *                     （终端永远停在 100×24、窗口怎么变都不动）⇒ 挂真浏览器探针
 *                     `scripts/terminal-layout.js`（真 index.html + styles.css + xterm.js +
 *                     main.js，用**真的 xterm**走"宽屏 → 缩小 → 放大"）。
 *   U42 session-trace-layout-real  轨迹的**真实几何**：Node 桩量不到"看起来是一行、
 *                     显示不下用省略号收尾"（没有布局引擎），所以挂一张真浏览器探针
 *                     `scripts/session-trace-layout.js` —— 真 index.html + styles.css +
 *                     session.js 在无头 Edge 里跑起来，逐元素量折行 / 溢出 / 横向滚动条 /
 *                     图标是否被挤到另一行。本机没有 Edge/Chrome 时该脚本自行 SKIP。
 */

"use strict";

const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const ROOT = path.resolve(__dirname, "..");
const read = (p) => fs.readFileSync(path.join(ROOT, p), "utf8");

/**
 * main.js 是否**真正**把 state 导出到 window（顶层 const 不挂 window，
 * 面板模块读的 window.state 全靠这一行）。行锚定 —— 否则注释里写一句同样文本也会算通过。
 */
const RE_STATE_EXPORT = /^[ \t]*window\.state[ \t]*=[ \t]*\w+[ \t]*;[ \t]*\r?$/m;

let failed = 0;
const results = [];

function check(id, name, ok, detail) {
  results.push({ id, name, ok, detail: ok ? "" : detail || "" });
  if (!ok) failed += 1;
}

/** 断言辅助：文本包含 */
function has(text, needle) {
  return text.includes(needle);
}

// ============================================
// 场景 1：契约静态断言（文本级，不执行任何前端代码）
// ============================================

function runStaticChecks() {
  const html = read("ui/index.html");
  const commandJs = read("ui/command.js");
  const mainRs = read("src-tauri/src/main.rs");
  const sinkRs = read("src-tauri/src/agent/sink.rs");

  // U1 agent-dom：面板依赖的全部锚点（agent.js 通过 $("id") 访问）
  const anchors = [
    "session-new-btn", "session-list", "agent-env-chips",
    "session-view", "session-container",
    // MCP / A2A 面板（mcp.js / a2a.js）
    "mcp-server-list", "mcp-add-name", "mcp-add-command", "mcp-add-btn",
    "mcp-tool-section", "mcp-selected-name", "mcp-start-btn", "mcp-stop-btn",
    "mcp-tool-list", "mcp-call-tool", "mcp-call-args", "mcp-call-btn",
    "mcp-call-result", "a2a-agent-list", "a2a-discover-url", "a2a-discover-btn",
    "a2a-task-section", "a2a-selected-name", "a2a-task-input", "a2a-send-btn",
    "a2a-state-line", "a2a-result",
    // 工具 / SKILL 面板（capability.js）
    "tools-list", "tools-add-name", "tools-add-command", "tools-add-desc",
    "tools-add-btn", "skills-list", "skill-new-btn", "skill-del-btn",
    "skill-edit-name", "skill-edit-desc", "skill-edit-content", "skill-save-btn",
  ];
  const missing = anchors.filter((id) => !has(html, `id="${id}"`));
  check("U1", "agent-dom", missing.length === 0,
    `index.html 缺少锚点: ${missing.join(", ")}`);
  check("U1", "agent-dom",
    has(html, 'data-tab="sessions"') && has(html, 'id="nav-panel-sessions"') &&
      has(html, 'data-tab="capability"') && has(html, 'id="nav-panel-capability"') &&
      ["mcp", "a2a", "tools", "skills"].every((c) =>
        has(html, `data-cap-tab="${c}"`) && has(html, `id="cap-sub-${c}"`)),
    "index.html 缺少 agent/capability 导航标签、能力面板或四个子面板");
  check("U1", "agent-dom",
    has(html, 'id="menu-capability"') &&
      ["mcp", "a2a", "tools", "skills"].every((c) => has(html, `data-cap="${c}"`)),
    "index.html 缺少「能力」菜单或其四个子菜单项");
  check("U1", "agent-dom",
    has(html, 'src="session.js"') && has(html, 'src="mcp.js"') && has(html, 'src="a2a.js"') &&
      has(html, 'src="capability.js"'),
    "index.html 未加载 session.js / mcp.js / a2a.js / capability.js");

  // U2 agent-verb：命令栏 `agent/mcp/a2a` 动词路由
  check("U2", "agent-verb",
    has(commandJs, 'case "agent":') && has(commandJs, 'case "mcp":') &&
      has(commandJs, 'case "a2a":') &&
      /SessionUI\?\.handleCommand\(/.test(commandJs) &&
      /McpUI\?\.handleCommand\(/.test(commandJs) &&
      /A2aUI\?\.handleCommand\(/.test(commandJs) &&
      /ToolsUI\?\.handleCommand\(/.test(commandJs) &&
      /SkillsUI\?\.handleCommand\(/.test(commandJs) &&
      has(commandJs, '"mcp"') && has(commandJs, '"a2a"') &&
      has(commandJs, '"tools"') && has(commandJs, '"skill"'),
    "command.js 缺少 agent/mcp/a2a/tools/skill 动词的 case 分支 / 清单项 / 面板路由");

  // U3 event-contract：会话模型折叠显示进度 —— stage/log 必须被 session.js 监听
  // （plan/step 同样必需：大纲区任务列表靠它们驱动；lint/repair 允许无人监听：细节在 run 产物中回看）
  const sessionJs = read("ui/session.js");
  const emitted = new Set([...sinkRs.matchAll(/emit\(\s*"agent:\/\/(\w+)"/g)].map((m) => m[1]));
  const listened = new Set([...sessionJs.matchAll(/"agent:\/\/(\w+)"/g)].map((m) => m[1]));
  const required = ["stage", "log", "plan", "step", "verify", "reflect"].filter((e) => !listened.has(e));
  check("U3", "event-contract", required.length === 0,
    `session.js 未监听核心进度事件: [${required}]（后端可发: [${[...emitted]}]）`);
  // 后端真发 verify/reflect（否则"前端监听了"只是摆设）
  const gateEvents = ["verify", "reflect"].filter((e) => !emitted.has(e));
  check("U3", "event-contract", gateEvents.length === 0,
    `sink.rs 未发出质量门禁事件: [${gateEvents}]`);
  // a2a://status：main.rs emit 与 a2a.js listen 一致
  const mcpJs = read("ui/mcp.js");
  const a2aJs = read("ui/a2a.js");
  const a2aEmitted = [...mainRs.matchAll(/"a2a:\/\/(\w+)"/g)].map((m) => m[1]);
  const a2aListened = [...a2aJs.matchAll(/"a2a:\/\/(\w+)"/g)].map((m) => m[1]);
  check("U3", "event-contract",
    a2aEmitted.every((e) => a2aListened.includes(e)),
    `后端发出但前端未监听的 a2a 事件: [${a2aEmitted.filter((e) => !a2aListened.includes(e))}]`);

  // U4 cmd-contract：附录 A 的命令全部注册；前端 invoke ⊆ 注册集
  const appendixA = [
    "agent_run", "agent_reply", "agent_plan", "agent_generate", "agent_verify",
    "agent_lint", "agent_repair", "agent_cancel", "agent_runs", "agent_run_load",
    "agent_run_delete", "agent_read_artifact", "agent_env_probe",
    "agent_session_list", "agent_session_load", "agent_session_save",
    "agent_session_new", "agent_session_delete",
    "agent_stage_preview", "agent_stage_apply",
  ];
  const mcpA2aCmds = [
    "mcp_servers", "mcp_add_server", "mcp_remove_server", "mcp_start",
    "mcp_stop", "mcp_tools", "mcp_call_tool",
    "a2a_agents", "a2a_discover", "a2a_remove", "a2a_send",
    "tools_list", "tools_add", "tools_remove", "tools_probe",
    "skills_list", "skills_save", "skills_remove",
  ];
  const registered = new Set(
    [...mainRs.matchAll(/(?:^|\s)(?:agent::)?((?:agent|mcp|a2a|tools|skills)_\w+)\s*,/gm)].map((m) => m[1]),
  );
  const notRegistered = [...appendixA, ...mcpA2aCmds].filter((c) => !registered.has(c));
  check("U4", "cmd-contract", notRegistered.length === 0,
    `main.rs 未注册: ${notRegistered.join(", ")}`);
  // 三个面板 JS 里出现的命令名必须都在注册集内
  const capJs = read("ui/capability.js");
  const invoked = new Set([
    ...[...sessionJs.matchAll(/"(agent_\w+)"/g)].map((m) => m[1]),
    ...[...mcpJs.matchAll(/"(mcp_\w+)"/g)].map((m) => m[1]),
    ...[...a2aJs.matchAll(/"(a2a_\w+)"/g)].map((m) => m[1]),
    ...[...capJs.matchAll(/"((?:tools|skills)_\w+)"/g)].map((m) => m[1]),
  ]);
  const unknownCmd = [...invoked].filter((c) => !registered.has(c));
  check("U4", "cmd-contract", unknownCmd.length === 0,
    `面板 JS 引用了未注册命令: ${unknownCmd.join(", ")}`);

  // U5 i18n-parity：两语言 key 集合一致；index.html 引用的 key 必须存在
  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const en = JSON.parse(read("ui/lang/en.json"));
  const zhKeys = new Set(Object.keys(zh));
  const enKeys = new Set(Object.keys(en));
  const onlyZh = [...zhKeys].filter((k) => !enKeys.has(k));
  const onlyEn = [...enKeys].filter((k) => !zhKeys.has(k));
  check("U5", "i18n-parity", onlyZh.length === 0 && onlyEn.length === 0,
    `仅 zh 有: [${onlyZh}]；仅 en 有: [${onlyEn}]`);
  const refd = [...html.matchAll(/data-i18n(?:-placeholder|-title)?="([^"]+)"/g)].map((m) => m[1]);
  const missKey = [...new Set(refd)].filter((k) => !zhKeys.has(k));
  check("U5", "i18n-parity", missKey.length === 0,
    `index.html 引用但语言文件缺失: ${missKey.join(", ")}`);

  // U33 ctx-copy-path：菜单项 DOM 契约 —— 两项都得在，且**不能**挂 file-only / folder-only
  const ctxMenu = (html.match(/<div id="context-menu"[\s\S]*?<\/div>\s*<\/div>/) || [""])[0];
  const ctxItems = [...ctxMenu.matchAll(/<div class="([^"]*)"[^>]*data-action="([^"]+)"/g)]
    .map((m) => ({ cls: m[1], action: m[2] }));
  for (const action of ["copy-path", "copy-full-path"]) {
    const it = ctxItems.find((i) => i.action === action);
    check("U33", "ctx-copy-path", !!it, `右键菜单缺少 data-action="${action}"`);
    if (!it) continue;
    check("U33", "ctx-copy-path",
      !it.cls.includes("file-only") && !it.cls.includes("folder-only"),
      `[${action}] 挂了 file-only/folder-only —— 文件和文件夹上都要能看到它（class="${it.cls}"）`);
  }
  const ctxMainJs = read("ui/main.js");
  check("U33", "ctx-copy-path",
    /case\s+"copy-path":/.test(ctxMainJs) && /case\s+"copy-full-path":/.test(ctxMainJs),
    "main.js 的右键 switch 没有 copy-path / copy-full-path 两个分支");

  // U34 autosave-no-ui：自动保存**只写盘**。着色（全量 tree-sitter + 一趟 IPC）与大纲
  // （O(全文) 解析 + 重建 DOM）不许挂在每次打字停顿上，两者只能从 refreshEditorChrome 出去
  const autoStart = ctxMainJs.indexOf("async function doAutoSave(tab) {");
  const autoEnd = autoStart >= 0 ? ctxMainJs.indexOf("\n}\n", autoStart) : -1;
  check("U34", "autosave-no-ui", autoStart >= 0 && autoEnd > autoStart,
    "main.js 里定位不到 doAutoSave（切片锚点失效）");
  const autoBody = autoStart >= 0 && autoEnd > autoStart
    ? ctxMainJs.slice(autoStart, autoEnd) : "";
  check("U34", "autosave-no-ui",
    autoBody.includes("write_file") && !autoBody.includes("highlightAndRender") &&
      !autoBody.includes("updateOutline"),
    "doAutoSave 只该有 write_file —— 带 UI 刷新就等于每次打字停顿跑一趟全量 tree-sitter");
  const debStart = ctxMainJs.indexOf("debounceTimer = setTimeout(() => {");
  const debEnd = debStart >= 0 ? ctxMainJs.indexOf("}, 1000);", debStart) : -1;
  const debBody = debStart >= 0 && debEnd > debStart ? ctxMainJs.slice(debStart, debEnd) : "";
  check("U34", "autosave-no-ui",
    debBody.includes("doAutoSave") && !debBody.includes("updateOutline") &&
      !debBody.includes("highlightAndRender"),
    "自动保存的防抖回调里只该有 doAutoSave，不该带 UI 刷新");
  for (const fn of ["highlightAndRender", "updateOutline"]) {
    const calls = [...ctxMainJs.matchAll(new RegExp(`(?<!function )\\b${fn}\\(`, "g"))].length;
    check("U34", "autosave-no-ui", calls === 1,
      `${fn} 的调用点应只有 refreshEditorChrome 里那一处，实际 ${calls} 处`);
  }
  check("U34", "autosave-no-ui",
    [...ctxMainJs.matchAll(/refreshEditorChrome\(/g)].length >= 3,
    "refreshEditorChrome 要挂在切回标签页 / 失焦 / 显式保存三个时机上");
  check("U34", "autosave-no-ui",
    /if \(tab\._language && !tab\._highlighted\)/.test(ctxMainJs),
    "refreshEditorChrome 得先判断高亮是否过期 —— 没过期就别再跑一趟 IPC");

  // U35 web-search-toggle：🌏 联网是**逐模型**的能力（引擎 llm::model_caps，
  // 实测 v4-pro 真检索、v4-flash 一次都不检索）。三条不许退：
  // ① 按钮存在且走 runtime 作用域写 llm.web_search（本次会话生效、不落盘）；
  // ② 模型没这能力时必须禁用 —— 不给一个按下去没反应的按钮；
  // ③ 模型下拉框只在真拿到厂商列表时才升级成 select。
  const sessJs = read("ui/session.js");
  check("U35", "web-search-toggle", /data-web/.test(sessJs),
    "session.js 工具栏里没有 🌏 联网按钮（data-web）");
  const webStart = sessJs.indexOf('wrap.querySelector("[data-web]")');
  const webEnd = webStart >= 0 ? sessJs.indexOf("data-send]", webStart) : -1;
  const webBody = webStart >= 0 && webEnd > webStart ? sessJs.slice(webStart, webEnd) : "";
  check("U35", "web-search-toggle", webStart >= 0 && webEnd > webStart,
    "session.js 里定位不到 🌏 的处理块（切片锚点失效）");
  check("U35", "web-search-toggle",
    webBody.includes('"config_form_apply"') && webBody.includes('scope: "runtime"') &&
      webBody.includes('key: "llm.web_search"'),
    "🌏 要把 llm.web_search 写进 runtime 作用域 —— 落盘会让一次试探变成长期配置");
  check("U35", "web-search-toggle",
    /if \(!caps\.web_search\)/.test(webBody) && /webBtn\.disabled = true/.test(webBody),
    "模型没有联网能力时必须禁用按钮（模型能力矩阵在引擎里，前端不抄一份）");
  check("U35", "web-search-toggle", webBody.includes("ai_model_caps"),
    "按钮状态要问后端 ai_model_caps，不许前端自己猜模型能不能联网");
  check("U35", "web-search-toggle",
    /session-web--on/.test(sessJs) && /session-web--on/.test(read("ui/styles.css")),
    "开/关要有可见区别（session-web--on 的 class 与样式都要有）");
  const cfgJs2 = read("ui/config.js");
  check("U35", "web-search-toggle",
    /key: "model".*dynamic: "models"/.test(cfgJs2),
    "ai.model 要标成动态选项（选项来自厂商 /models）");
  check("U35", "web-search-toggle",
    /f\.dynamic === "models" && modelChoices && modelChoices\.length/.test(cfgJs2),
    "只有真拿到厂商列表才升级成下拉框 —— 拿不到就退回文本框，别给一份可能跑不通的清单");
  check("U35", "web-search-toggle", /ai_list_models/.test(cfgJs2),
    "模型列表要从后端 ai_list_models 取，不许前端硬编码");

  // U9 config-form：配置表单的 DOM 锚点 / 脚本 / config 子动词路由
  const configJs = read("ui/config.js");
  const cfgAnchors = [
    "config-view", "config-scope-title", "config-dir", "config-dirty", "config-hint",
    "config-btn-save", "config-btn-apply", "config-btn-cancel", "config-body",
  ];
  const missCfg = cfgAnchors.filter((id) => !has(html, `id="${id}"`));
  check("U9", "config-form", missCfg.length === 0,
    `index.html 缺少配置表单锚点: ${missCfg.join(", ")}`);
  check("U9", "config-form",
    has(html, 'src="config.js"') && has(configJs, "window.ConfigUI =") &&
      has(commandJs, "ConfigUI?.handleCommand("),
    "config.js 未加载 / 未暴露 ConfigUI / config.js 子动词未在 command.js 路由");

  // U10 config-contract：表单命令注册齐全 + 静态 schema 字段的 i18n 齐全
  //
  // 注意 harness 段的字段**不再手写在 config.js 里** —— 它由后端 `config_schema`
  // 给出（键表单源化）。所以这里守三件事：①引用的命令都注册了；
  // ②harness 键确实没被抄回前端（抄回去就等于又开始漂移）；③已有文案没被批量删掉。
  const cfgCmds = [...new Set([...configJs.matchAll(/"(config_\w+)"/g)].map((m) => m[1]))];
  const cfgUnreg = cfgCmds.filter((c) => !mainRs.includes(`${c},`));
  check("U10", "config-contract",
    cfgCmds.includes("config_form_load") && cfgCmds.includes("config_schema") &&
      cfgUnreg.length === 0,
    `config.js 引用的命令未全部注册: ${cfgUnreg.join(", ")}`);
  const schemaText = configJs.slice(
    configJs.indexOf("const SCHEMA"), configJs.indexOf("const NUMERIC"));
  const schemaFields = [];
  let curSection = "";
  for (const line of schemaText.split("\n")) {
    const s = line.match(/section:\s*"([\w.]+)"/);
    if (s) curSection = s[1];
    const k = line.match(/key:\s*"([\w.]+)"/);
    if (k) schemaFields.push(`${curSection}.${k[1]}`);
  }
  const missField = schemaFields.filter(
    (p) => !zhKeys.has(`config.field.${p}`) || !zhKeys.has(`config.desc.${p}`));
  check("U10", "config-contract", schemaFields.length >= 5 && missField.length === 0,
    `静态 SCHEMA 字段缺 i18n 键: ${missField.join(", ")}（解析到 ${schemaFields.length} 个字段）`);
  // harness 段的键一旦被抄回前端，这条立刻红
  check("U10", "config-contract",
    !/key:\s*"(llm|verify|gate|reflect|agent|step|lint|sandbox|kb|discover|env|proc|ask)\./
      .test(schemaText),
    "harness 段的键又被手写回 config.js 了（应由引擎 schema 提供）");
  // 已有文案是产品资产：新键暂无文案时回落到显示键路径，但不许被批量删掉
  const harpLabels = [...zhKeys].filter((k) => k.startsWith("config.field.harness."));
  check("U10", "config-contract", harpLabels.length >= 20,
    `harness 段的 i18n 文案只剩 ${harpLabels.length} 条`);

  // U10 config-contract：面板模块读 window.state，main.js 必须真的把它导出。
  // （顶层 const 只进全局词法环境，不挂 window —— 漏导出时 root() 恒为 null，
  //   表现为"已打开项目却提示未打开项目"。stub 会伪造 window.state，所以必须静态断言。）
  const stateReaders = ["config.js", "mcp.js", "a2a.js", "capability.js"]
    .filter((f) => has(read("ui/" + f), "window.state"));
  check("U10", "config-contract",
    stateReaders.length > 0 && RE_STATE_EXPORT.test(read("ui/main.js")),
    `main.js 未导出 window.state，但 ${stateReaders.join(", ")} 依赖它（项目作用域会全部失效）`);

  // U14 apply-writeback：产物写回真实项目的链路契约（Agent 工具循环 + 三模式）。
  //   确认模式 — 循环改动只暂存 .ruyix/stage/，会话自动打开差异面板，
  //               写入只能由 data-confirm 触发（agent_stage_apply，恒备份）
  //   写入/自主模式 — 循环直写项目（引擎 WritePolicy::Apply，覆盖前备份）
  // 这里守住：暂存命令已注册并各只出现一次、面板三环齐全、后端三条硬约束。
  check("U14", "apply-writeback",
    ["agent_apply_preview", "agent_apply_run", "agent_stage_preview", "agent_stage_apply"]
      .every((c) => mainRs.includes(`agent::${c},`)),
    "main.rs 未注册写回命令（apply / stage 预览与落盘）");
  check("U14", "apply-writeback",
    ["confirm", "write", "auto"].every((m) => has(sessionJs, `value="${m}"`)) &&
      has(sessionJs, "openStagePanel") && has(sessionJs, "renderApplyPanel") &&
      has(sessionJs, "doApply") && has(sessionJs, "applyStage") &&
      !has(sessionJs, 'type="radio"'),
    "写回三模式不完整（缺下拉选项/暂存面板/落盘链路），或残留旧的单选按钮");
  // 覆盖会让文件变短时必须显式提示 —— 模型"顺手重写整个文件"删掉既有依赖
  // 是这条链路最危险的失败模式，不能静默放过
  check("U14", "apply-writeback", has(sessionJs, "session-apply-loss"),
    "session.js 缺少「覆盖将减少 N 行」的告警（模型重写文件删内容时会无提示）");
  check("U14", "apply-writeback",
    (sessionJs.match(/"agent_stage_apply"/g) || []).length === 1 &&
      has(sessionJs, "applyStage") && has(sessionJs, "data-confirm") && has(sessionJs, "backup: true"),
    "暂存落盘必须收敛到唯一入口 applyStage（agent_stage_apply 仅一次、恒备份、由 data-confirm 触发）");
  const agentModRs = read("src-tauri/src/agent/mod.rs");
  check("U14", "apply-writeback",
    has(agentModRs, 'Some("write") | Some("auto") => engine::agent::WritePolicy::Apply'),
    "确认模式必须映射 Stage、写入/自主映射 Apply（mod.rs 的策略映射缺失）");
  const applyRs = read("src-tauri/src/agent/apply.rs");
  check("U14", "apply-writeback",
    has(applyRs, "拒绝写回") && has(applyRs, "is_safe_rel") && has(applyRs, ".ruyix/backups"),
    "apply.rs 缺安全约束：路径封闭 / 拒绝写进沙箱 / 写前备份");
  const stageRs = read("src-tauri/src/agent/stage.rs");
  check("U14", "apply-writeback",
    has(stageRs, "is_safe_stage_rel") && has(stageRs, ".ruyix"),
    "stage.rs 缺安全约束：暂存路径封闭（含拒绝写进 .ruyix 自身）");

  // U22 help-markdown：帮助正文的**源**是 markdown 文件（ui/help-zh.md / ui/help-en.md），
  // 打开时用 markdown-it 渲染进 #help-body —— 不是手写的 HTML 表格，也不是只读代码编辑器。
  // 守住：双语源文件存在且真是 markdown（含表格）、旧的字符串拼接已删除、
  // main.js 有「加载 md → 渲染 → 填容器」链路、帮助 tab 走文档视图而非 renderPlainCode。
  const helpMd = ["ui/help-zh.md", "ui/help-en.md"].map((p) => ({
    p, text: fs.existsSync(path.join(ROOT, p)) ? read(p) : "",
  }));
  check("U22", "help-markdown", helpMd.every((f) => f.text.length > 0),
    `帮助源 markdown 缺失或为空: ${helpMd.filter((f) => !f.text).map((f) => f.p).join(", ")}`);
  const badMd = helpMd.filter((f) => !/^#\s.+$/m.test(f.text) || !/\|\s*-{3,}\s*\|/.test(f.text));
  check("U22", "help-markdown", badMd.length === 0,
    `帮助源文件不是有效的 markdown（缺标题或表格）: ${badMd.map((f) => f.p).join(", ")}`);
  check("U22", "help-markdown", !has(commandJs, "getHelpText"),
    "command.js 仍在拼接纯文本帮助（getHelpText）——正文应只维护 md 源文件");
  const mainJs = read("ui/main.js");
  check("U22", "help-markdown",
    has(mainJs, "async function loadHelpDoc()") && has(mainJs, "markdownToHtml") &&
      has(mainJs, "window.markdownit") && has(mainJs, "help-zh.md") && has(mainJs, "help-en.md"),
    "main.js 缺少帮助文档链路（按语言加载 md → markdown-it 渲染）");
  check("U22", "help-markdown",
    has(html, 'id="help-body"') && !has(html, "help-table"),
    "index.html 应只留 #help-body 容器（正文由渲染结果填充），不再内嵌手写表格");
  check("U22", "help-markdown",
    has(mainJs, "if (tab._isHelp)") && has(mainJs, "showHelpPage()") &&
      !has(mainJs, "content: getHelpText()"),
    "帮助标签页必须走 markdown 文档视图（_isHelp → showHelpPage），不再塞进只读编辑器");

  // U23 service-panel：agent 用 background 起的常驻服务必须**在 UI 里看得见、停得掉**。
  //   （起得来却看不见 = 用户只能开任务管理器按 pid 找；这是本轮要治的病）
  //   引擎侧：进程表按 pid 存 + 带墙钟启动时刻（面板要显示"几点起的、跑了多久"）
  //   后端：proc_list / proc_stop 已注册
  //   前端：菜单锚点 + 编辑区视图 + 三线表三字段（PID / 启动时间 / 完整命令行）
  const managedRs = read("crates/harness-engine/src/proc.rs");
  check("U23", "service-panel",
    /HashMap<u32,\s*Managed>/.test(managedRs) && has(managedRs, "started_at_ms") &&
      has(managedRs, "pub fn stop_pid"),
    "proc.rs 的进程表必须按 pid 存、带墙钟启动时刻、支持按 pid 停（面板手里只有 pid）");
  check("U23", "service-panel",
    ["proc_list", "proc_stop"].every((c) => mainRs.includes(`${c},`)),
    "main.rs 未注册 proc_list / proc_stop（服务面板读不到也停不掉）");
  check("U23", "service-panel",
    ["menu-service", "service-view", "service-body", "service-btn-refresh"]
      .every((id) => has(html, `id="${id}"`)) && has(html, 'src="service.js"'),
    "index.html 缺少服务菜单 / 服务视图容器 / service.js");
  const serviceJs = read("ui/service.js");
  check("U23", "service-panel",
    ["service.th_pid", "service.th_started", "service.th_cmd"].every((k) => has(serviceJs, k)),
    "服务表的字段必须是 PID / 启动时间 / 完整命令行三项");
  check("U23", "service-panel",
    has(serviceJs, '"proc_list"') && has(serviceJs, '"proc_stop"') &&
      has(serviceJs, "{ pid }") && has(serviceJs, "data-stop"),
    "服务面板的读取/停止链路不完整（proc_list / proc_stop({pid}) / 行内停止按钮）");
  check("U23", "service-panel",
    /getFullYear\(\)/.test(serviceJs) && /getSeconds\(\)/.test(serviceJs) &&
      /\$\{h\}h\$\{m\}m\$\{s\}s/.test(serviceJs) && has(serviceJs, "isDead"),
    "启动时间要显示到秒并带 h/m/s 时长；已退出的进程不该再占一行（没有可管的）");
  check("U23", "service-panel",
    has(mainJs, "tab._isService") && has(mainJs, "showServiceView") &&
      has(mainJs, "ServiceUI?.close") && has(commandJs, 'case "service":') &&
      /_isService\)\s*return/.test(mainJs),
    "main.js 缺服务标签页分支（含标签图标）/ 切走时没停秒级刷新，或命令栏缺 service 动词");

  // U24 external-link：agent 回的链接不能把 IDE 顶掉（P0）。
  //   真凶：agent 输出走 markdown-it + linkify → 真 `<a href>`，而 WebView2 对普通导航的默认动作
  //   就是**在本 WebView 里导航过去**；整个 IDE 活在一个文档里（标签页只是 DOM 状态）→ 文档一换，
  //   标签栏 / 文件树 / 会话全跟着没。所以：
  //     · 主窗口必须在 Rust 里建 —— 只有 Builder 挂得上 on_navigation（配置窗口没有闸门）；
  //     · 判定只有一条"是不是自家文档"，三结局：放行 / 交给系统浏览器 / 拒掉；
  //     · 出口是 open_external（白名单 http/https/mailto + 系统默认处理程序，不走 shell）；
  //     · 前端另有一重捕获阶段拦截，判定表与后端一致（改一处必须同步另一处）。
  const confJson = read("src-tauri/tauri.conf.json");
  const extJs = read("ui/external.js");
  check("U24", "external-link",
    has(mainRs, "fn build_main_window") && has(mainRs, "on_navigation") &&
      has(mainRs, "on_new_window") && !/"windows"\s*:\s*\[\s*\{/.test(confJson),
    "主窗口必须建在 Rust 里并挂 on_navigation（tauri.conf.json 的 app.windows 不能再有窗口：配置窗口没有导航闸门）");
  check("U24", "external-link",
    ["fn nav_verdict", "fn is_app_host", "fn is_document_path", "Nav::External", "Nav::Refuse"]
      .every((s) => has(mainRs, s)),
    "导航判定必须落在 main.rs::nav_verdict（放行 / 交给系统浏览器 / 拒掉 三结局）");
  check("U24", "external-link",
    has(mainRs, "fn open_external") && has(mainRs, "fn check_open_url") &&
      has(mainRs, "ShellExecuteW") && has(mainRs, "open_external,"),
    "外链出口必须是 open_external（白名单 + 系统默认处理程序），且已注册到 invoke_handler");
  check("U24", "external-link",
    has(html, 'src="external.js"') && has(mainJs, "ExternalLinks?.install") &&
      has(commandJs, 'case "url":') && has(commandJs, '"open_external"'),
    "前端外链链路不完整（external.js 未加载/未安装、命令栏缺 open url、或没接上 open_external）");
  check("U24", "external-link",
    has(extJs, "OPENABLE") && has(extJs, "preventDefault") && has(extJs, "auxclick") &&
      has(extJs, "isOwnDocument") && has(extJs, "open url "),
    "external.js 的拦截不完整（白名单 / preventDefault / 中键 / 自家文档判定 / 出口）");
  // 真窗口证明必须留在仓库里能跑：这条 P0 的病根只在**运行中的 WebView** 上显形，
  // 静态断言再全也证明不了"页面真的没被导航走"，所以那份 CDP 探针是这条契约的另一半
  const probeJs = read("scripts/nav-guard-probe.mjs");
  check("U24", "external-link",
    has(probeJs, "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS") && has(probeJs, "Runtime.evaluate") &&
      has(probeJs, "markdownit") && has(probeJs, "location.href"),
    "真窗口证明脚本不见了或不完整（必须真连 CDP、真读回 location.href、真点 markdown 渲出来的链接）");

  // U16 agent-loop：会话 = 工具循环（Read/Write/Execute/Connect 四原语），不做问答/任务预分类。
  // 三环：session.js 走 agent_reply 并带模式与历史；mod.rs 调 engine::agent::run 并接连接器；
  // 引擎 agent.rs 定义四原语 + 终止协议 + 破坏性命令拒绝；connect.rs 落 MCP/A2A 两条真实通路。
  const intentRs = read("crates/harness-engine/src/agent.rs");
  check("U16", "agent-loop",
    has(sessionJs, '"agent_reply"') && has(sessionJs, "history, mode, projectRoot") &&
      has(agentModRs, "engine::agent::run") &&
      !has(agentModRs, "intent::classify"),
    "会话未走工具循环：session.js 缺 agent_reply 调用参数，或 mod.rs 未调 engine::agent::run / 残留意图分类");
  check("U16", "agent-loop",
    ["read", "write", "execute", "connect"].every((t) => has(intentRs, `"${t}"`)) &&
      has(intentRs, "execute_allowed") && has(intentRs, "MAX_STEPS") &&
      has(intentRs, "trait Connector"),
    "引擎 agent.rs 缺四大原语 / 破坏性命令拒绝 / 轮次上限 / Connector 契约");
  // Connect 必须真接上宿主，否则四个原语里有一个是空壳
  const connRs = read("src-tauri/src/agent/connect.rs");
  check("U16", "agent-loop",
    has(agentModRs, "connect::RuyixConnector") && has(agentModRs, "McpManager") &&
      has(connRs, "impl Connector for RuyixConnector") &&
      has(connRs, "call_tool") && has(connRs, "send_task"),
    "Connect 原语没接上宿主：mod.rs 未建 RuyixConnector / 未传 McpManager，或 connect.rs 缺 MCP/A2A 通路");

  // U15 outline-plan：模型返回计划 → 大纲区任务列表（✅ ⌛ ⛏️ ⚠️ 六态 + ❌ 失败 / ⏹️ 未完成）。
  // 链路五环缺一不可：引擎发 plan 事件 → sink emit → session.js 监听并渲染 → main.js 会话分支调用
  // → run 收尾时 settle_steps 把每个步骤都落到终态（否则没轮到派发的步骤会永远停在
  //   ⌛「等待执行」，实测 run agent-20260920-091436 就是 2/3 + 永久沙漏）。
  const uiMainJs = read("ui/main.js");
  const stylesCss = read("ui/styles.css");
  check("U15", "outline-plan",
    has(sinkRs, 'emit("agent://plan"') &&
      has(sessionJs, "STEP_EMOJI") &&
      ["✅", "⌛", "⛏️", "❌", "⏹️", "⚠️"].every((e) => has(sessionJs, e)) &&
      /SessionUI\?\.renderOutline\(/.test(uiMainJs) &&
      has(stylesCss, "outline-plan-step") && has(stylesCss, "outline-plan-step--error") &&
      has(stylesCss, "outline-plan-step--skipped") &&
      has(stylesCss, "outline-plan-step--partial"),
    "任务计划未接入大纲区：sink 未发 agent://plan / session.js 缺六态 emoji 渲染 / main.js 未调 renderOutline / 缺样式");

  // U15b plan-settle：run 收尾必须把计划落定（沙漏是"等待执行"，run 结束还挂着就是骗人）。
  // 四环：引擎有 settle_steps → 收尾处真调用（**带上项目根**，见下）且带上 delivered
  //（交付与否决定能不能算完成）→ 交付分支真的把 delivered 置真（否则所有步骤都被判成
  // "本次未完成"）→ 推断口径要把"未对齐"与"跳过"分开。
  //
  // 收尾判据的两条纪律，缺一条就会像实测 run agent-20260920-142405 那样
  // 8 步判出 6 个"跳过"（其中 4 步其实写了一半以上、还有一步声明的文件本来就存在）：
  //   ① 声明了、本 run 没写，但**磁盘上已经有**的文件不算缺（proj.join(f).exists()）
  //   ② 有产出但对不上声明 → `partial`（未对齐），不是 `skipped`
  check("U15", "plan-settle",
    has(intentRs, "fn settle_steps(") &&
      /settle_steps\(\s*sink,\s*&plan_steps,\s*&ctx\.overlay,\s*proj,\s*delivered,\s*&step_states,?\s*\)/.test(intentRs) &&
      /proj\.join\(f\)\.exists\(\)/.test(intentRs) &&
      /"partial"/.test(intentRs) &&
      /out\.answer = text;\s*\n\s*delivered = true;/.test(intentRs) &&
      /"skipped"/.test(intentRs) &&
      /不冒充完成|settle_steps/.test(intentRs),
    "计划收尾未落定：agent.rs 缺 settle_steps / 收尾未调用或没带项目根 / 未把「实体存在」与「未对齐」分开 / delivered 未在 final 分支置真 —— 界面会留永久沙漏或虚报跳过");

  // U15b2 plan-partial：前端必须认这个新终态。
  // 最阴的一条：applyStepEvent 的分支是 running / done / skipped / **else → error**，
  // 少了 partial 那一支，引擎报"未对齐"会被界面渲染成 ❌ 失败 —— 又是拿比事实重的词骗人。
  check("U15", "plan-partial",
    has(sessionJs, "p.status === \"partial\"") &&
      /partial:\s*"⚠️"/.test(sessionJs) &&
      /outlineCounts/.test(sessionJs) &&
      has(stylesCss, "outline-plan-step--partial"),
    "未对齐（partial）终态没接上 UI：session.js 的 applyStepEvent 会把 partial 落到 error / 缺 emoji / 缺样式");

  // U15c plan-execute：计划即执行（v0.4，`step.execute_plan`，**默认开**）。
  // 默认开是因为关着的时候进度只能靠"文件是否落地"推断（实测会大面积误判成"跳过"）；
  // 引擎自己知道每步跑没跑完，这个事实才该是默认路径。退路仍在：配 false 即旧行为。
  // 调度权必须留在**引擎**手里：游标（plan_cursor）由引擎维护 → 派发前先发 running
  // （否则 run 期间一直挂 ⌛）→ 真调用 run_step，且把父的 `&mut ctx` 传进去（覆盖层不分裂）
  // → 失败落 error 并停下给模型一轮干预（step_failure_feedback），而不是闷头跑下一步。
  check("U15", "plan-execute",
    has(intentRs, "plan_cursor") &&
      has(intentRs, "MAX_PLAN_RESETS") &&
      has(intentRs, "step_failure_feedback") &&
      /run_step\(cfg, &mut ctx, &inp, cancel, deadline, sink\)/.test(intentRs) &&
      has(intentRs, "cfg.step.execute_plan"),
    "计划执行未接上主循环：引擎缺游标 / 未传共享 Ctx 派发 run_step / 失败未回灌干预轮");

  // U15d plan-persist：计划必须活着跨过一次重启。
  // 会话跑的是**工具循环**（agent_reply），它不落 RunRecord、run_id 恒为空 ——
  // 计划（含终态）与验证/复核结论都只能跟着助手消息落盘。Rust 侧漏声明字段最隐蔽：
  // UI 明明挂上去了，agent_session_save 的往返会把它抹掉（UI 拿返回值 Object.assign
  // 覆盖自己的 messages），表现出来就是"重开旧会话，大纲区的任务列表整个消失"。
  // 四环：Rust 有字段 → UI 落盘前挂快照 → hydratePlan 先从消息恢复 → 老会话回退到 run_id 路径。
  const sessionsRs = read("src-tauri/src/agent/sessions.rs");
  check("U15", "plan-persist",
    has(sessionsRs, "pub plan: Option<PlanSnap>") &&
      has(sessionsRs, "pub struct PlanStepState") &&
      has(sessionsRs, "pub verify: Vec<VerifyOutcome>") &&
      has(sessionsRs, "pub reflect: Vec<Reflection>") &&
      has(sessionJs, "function planSnapshot(") &&
      has(sessionJs, "function planFromSnapshot(") &&
      has(sessionJs, "placeholder.plan = snap") &&
      has(sessionJs, "m.plan?.steps?.length") &&
      has(sessionJs, "agent_run_load"),
    "计划跨重启会丢：sessions.rs 未声明 plan/verify/reflect / session.js 未挂快照或未从消息恢复");

  // U19 cmd-discover：命令发现（v0.5）——「本机有什么命令」必须**实测后真的进模型上下文**。
  // 实测 run agent-20260920-152312 有 5~6 轮纯耗在探 mvn / java 在不在；引擎早就探过，只是
  // 从没写进上下文。四环缺一不可：① 一张数据表（加工具是加一行，不是加一个分支）② 两步探测
  // （`where` / `command -v` 解析出"在不在"，再过 shell 取版本 —— Windows 上 `.cmd` 不被
  // CreateProcess 认，实测 `Command::new("mvn")` 直接 not found）③ 渲染时**可用与不可用都写**
  // （只说"有 mvn"治不了空转）④ 主循环与子步骤**都**注入（子步骤上下文隔离，父那份到不了）。
  // 宿主侧还要能关掉、能追加工具，否则用户无从干预。
  const discoverRs = read("crates/harness-engine/src/discover.rs");
  const execRs = read("crates/harness-engine/src/exec.rs");
  const stepRs = read("crates/harness-engine/src/step_agent.rs");
  const cfgBridgeRs = read("src-tauri/src/agent/config_bridge.rs");
  check("U19", "cmd-discover",
    has(discoverRs, "pub const TOOLS") &&
      has(discoverRs, "pub fn discover(") &&
      has(discoverRs, "pub fn render_note(") &&
      has(discoverRs, "本机**没有**") &&
      has(discoverRs, "exec::resolve_bin(") &&
      has(execRs, "pub fn resolve_bin(") &&
      has(execRs, "pub fn bin_version(") &&
      has(intentRs, "discover::render_note(") &&
      has(stepRs, "discover::render_note(") &&
      has(cfgBridgeRs, "engine::config::schema()") &&
      zhKeys.has("config.field.harness.discover.enabled") &&
      zhKeys.has("config.field.harness.discover.extra"),
    "命令发现没接上：工具表 / 两步探测（where+shell）/ 可用与不可用都写 / 主循环与子步骤都注入 / 配置项 缺一不可");

  // U20 env-install：环境准备（v0.5）—— 缺工具时的按需安装。它**不是第五种原语、不占新的 connect
  // 形态**：引擎侧零改动，宿主在清单里多摆一个 `kind="env"` 的目标并在 `call` 里认它即可 ——
  // 这是"能力进连接器/数据，不进引擎分支"的样板。必须钉住：① 引擎只留约定常量（ENV_CONNECTOR_KIND）
  // 与"缺失指向 connect"② 宿主连接器把 env 目标摆进清单、且在 `call` 里**先按名字分流**（否则落进
  // MCP 的"没登记"分支）③ 包管理器知识全在宿主的一张表（加平台是加一行）④ 每次动作**留记录**
  // （jsonl + UI 事件）⑤ 开关能一行回退（关掉后清单里不再出现 env，模型彻底看不见）
  // ⑥ 安装命令必须**过 shell**（`.cmd` 不能被 CreateProcess 直接执行）。
  const connectRs = read("src-tauri/src/agent/connect.rs");
  const envSetupRs = read("src-tauri/src/agent/env_setup.rs");
  check("U20", "env-install",
    has(intentRs, "ENV_CONNECTOR_KIND") &&
      has(connectRs, "env_setup::ENV_SERVER") &&
      has(connectRs, "async fn install_env(") &&
      has(envSetupRs, "pub enum Pm") &&
      has(envSetupRs, "pub fn pick_manager(") &&
      has(envSetupRs, "pub fn install(") &&
      has(envSetupRs, "installs.jsonl") &&
      has(envSetupRs, "discover::invalidate_cache()") &&
      has(execRs, "pub fn run_line(") &&
      has(cfgBridgeRs, "engine::config::schema()") &&
      zhKeys.has("config.field.harness.env.install_enabled"),
    "环境准备没接上：宿主 env 目标（清单 + call 分流）/ 包管理器表 / 留记录 / 走 shell / 缓存失效 / 开关 缺一不可");

  // U21 exec-safety：执行前闸门 + 输出按代码页解码（v0.6）。两个病根各钉一遍：
  // ① **闸门**：`\`、`\admin-run\` 这类"根本不是命令"的字符串以前会被原样交给 cmd；真控制台里
  //    `start` 系还会把失败变成**桌面弹窗**、stdout/stderr 一律拿不到 —— 现在三类在**执行前**拒掉，
  //    并回一条可读纠正（附本机实测可用清单 —— 同一张工具表，既喂上下文也当验证器）。
  // ② **编码**：中文 Windows 上 cmd/java/mvn/git 输出是 GBK，`from_utf8_lossy` 解成乱码，
  //    模型读不懂"不是内部或外部命令"，只能换个更离谱的命令继续试（"命令总是不对"的直接原因）。
  //    必须一处实现多处消费：引擎 run_with_cap + 宿主 run_target / probe_tool / git.rs。
  const gitRs = read("src-tauri/src/git.rs");
  const capabilityRs = read("src-tauri/src/capability.rs");
  check("U21", "exec-safety",
    has(execRs, "pub fn decode_output(") &&
      has(execRs, "GetACP") &&
      has(execRs, "decode_output(&out_buf") &&
      has(mainRs, "harness_engine::exec::decode_output") &&
      has(gitRs, "harness_engine::exec::decode_output") &&
      has(capabilityRs, "harness_engine::exec::decode_output"),
    "输出编码没接上：引擎 decode_output（按活动代码页）+ 宿主三处复用，缺一不可");
  check("U21", "exec-safety",
    has(intentRs, "pub(crate) fn preflight_execute(") &&
      has(intentRs, "const SHELL_BUILTINS") &&
      has(intentRs, "const LAUNCHERS") &&
      has(intentRs, "SHELL_ESCAPES") &&
      has(discoverRs, "pub fn is_available(") &&
      has(discoverRs, "pub fn available_names(") &&
      /preflight_execute\(proj, cmd\)/.test(intentRs),
    "执行前闸门没接上：三类拦截（启动器 / 路径式垃圾 / 没装的命令）+ 可用清单 + 接进 tool_execute");

  // U23 proc-lifecycle：永不退出的服务（v0.6）。**不加第五原语** —— 四个原语编码的是"效果"
  // （读 / 写 / 执行 / 连接），而"活多久"是**时长**，它不属于任何一个效果。所以给 execute 补
  // 第三个维度：前台 / 后台起 / 句柄操作。实测那次 17 轮空转的病根不是模型笨：服务其实起来了
  // （日志里有 `Started AdminApplication`），证据落在项目根的 log 里、模型看不见，而上一轮的
  // `java` 孤儿还占着 8083 → 每次重启都拿到假的"端口被占" → 自我强化的失败循环。
  //
  // 六环缺一不可：① 托管进程表（项目归属，收尾能划界）② 就绪判据**是一条命令**（退出码 0 即就绪
  // —— 引擎不认识它测的是什么，这就是"零 app 知识"）③ 起之前**先探一次**判据：别人已满足就拒绝
  // 并附证据（这条是整轮最值钱的守卫，直接掐掉孤儿重演）④ 三个出口都带证据（就绪给命中行 / 已退出
  // 给退出码+日志尾 / 未就绪给"没命中"的证据）⑤ 日志落**项目内** `.ruyix/proc/<handle>.log`
  // （路径由引擎给，模型不写重定向；不走管道 → 不会被写满阻塞）⑥ 引擎持有就引擎收：
  // run 结束按**项目**收（越界会踩到并行/嵌套的别人），宿主退出**全收**；杀必连子进程树
  // （只杀 mvn 不杀 java = 又造一个占端口的孤儿）。
  const procRs = read("crates/harness-engine/src/proc.rs");
  const libRs = read("crates/harness-engine/src/lib.rs");
  const configRs = read("crates/harness-engine/src/config.rs");

  check("U23", "proc-lifecycle",
    has(libRs, "pub mod proc;") &&
      has(procRs, "struct Managed") &&
      has(procRs, "pub fn start(") &&
      has(procRs, "pub fn status(") &&
      has(procRs, "pub fn log_tail(") &&
      has(procRs, "pub fn stop(") &&
      has(procRs, "pub fn shutdown_for(") &&
      has(procRs, "pub fn shutdown_all(") &&
      has(procRs, "MAX_BACKGROUND_HARD"),
    "托管进程模块没立起来：进程表 / start-status-log-stop / 项目级与全局收尾 / 并发硬顶 缺一不可");

  check("U23", "proc-lifecycle",
    has(procRs, "fn probe_ready(") &&
      has(procRs, ".join(\".ruyix\").join(\"proc\")") &&
      has(procRs, "pub const LOG_TAIL_LINES") &&
      has(execRs, "pub fn kill_tree(") &&
      has(execRs, "pub fn shell_command(") &&
      has(procRs, "exec::shell_command(") &&
      has(execRs, "raw_arg(") &&
      has(execRs, "\"/S\""),
    "就绪判据 / 日志落项目内 / 连子进程树杀 / shell 构造只有一份（raw_arg + /S /C 是带引号命令行能跑通的前提）—— 缺一样就回到孤儿占端口的老路");

  check("U23", "proc-lifecycle",
    has(intentRs, "ExecBg(crate::proc::StartSpec)") &&
      has(intentRs, "pub(crate) enum ProcOp") &&
      has(intentRs, "fn parse_execute(") &&
      has(intentRs, "shutdown_for(proj") &&
      has(stepRs, "StepAction::ExecBg(") &&
      has(stepRs, "tool_exec_bg") &&
      has(stepRs, "tool_proc"),
    "execute 的生命周期维度没接上：解析入口 / 句柄三操作 / run 结束按项目收 / 子步骤同款派发");

  check("U23", "proc-lifecycle",
    has(intentRs, "\"background\"") && has(stepRs, "background") &&
      has(stepRs, "ready_cmd") && has(stepRs, "handle"),
    "两份提示词都要写后台模式：父子上下文隔离，缺一份每个步骤都会各自重新用 start 去绕");

  // keep_alive 的诚实性（run agent-20260921-0950）：模型照提示词起了服务、看到就绪就报"已启动"，
  // 而提示词里**根本没有 keep_alive 这个词**，run 一结束引擎就把它收掉了 ——
  // 用户 netstat 一看是空的，session 存档末尾只多了一行"本次 run 收掉了 1 个托管进程"。
  // 三处一起管：提示词教会 + start 告示说透后果 + 交付前把 final 打回一次（只一次）。
  check("U23", "proc-lifecycle",
    has(intentRs, "keep_alive") && has(intentRs, "活不活得过本次 run") &&
      has(intentRs, "谎报"),
    "提示词必须教会 keep_alive 及其后果（不带就活不过本次 run，报「已启动」是谎报）");
  check("U23", "proc-lifecycle",
    has(intentRs, "fn reap_warning") && has(intentRs, "proc_warned") &&
      has(intentRs, "本次 run 结束时收掉了"),
    "缺交付前对账：未声明 keep_alive 的托管进程会在 run 结束被收掉，必须先把 final 打回一次并说清后果");
  check("U23", "proc-lifecycle",
    has(intentRs, "在用户看到时已经是空的") && has(intentRs, "服务】面板"),
    "start 告示没把后果说透：未声明时要讲清「run 一结束就收掉、报已启动是空的」，声明了要讲面板可停");

  check("U23", "proc-lifecycle",
    has(configRs, "pub struct ProcConfig") &&
      has(configRs, "pub proc: ProcConfig") &&
      has(cfgBridgeRs, "engine::config::schema()") &&
      zhKeys.has("config.field.harness.proc.enabled") &&
      zhKeys.has("config.field.harness.proc.max") &&
      zhKeys.has("config.field.harness.proc.ready_timeout_secs") &&
      has(mainRs, "harness_engine::proc::shutdown_all(false)"),
    "proc 配置没贯通或宿主退出未收尾：引擎 ProcConfig → 配置桥（照 schema）→ UI 三字段 → 退出全收");

  // U25 batch-calls：一轮多个调用（v0.7）。病根是"轮次全花在一次一个调用上"——一次模型往返
  // 只换回一个文件内容，而每轮都要把上下文重发一遍（轮数还近似平方地涨 token）。
  // 口径：**要并发就一起并发**（读 / 写 / 执行 / 连接都并），只有两种结构性冲突保序：
  // 同一条路径上的写与读、托管进程的生命周期操作。
  check("U25", "batch-calls",
    has(intentRs, "fn parse_actions(") &&
      has(intentRs, '"actions"') &&
      has(intentRs, "fn parse_one(") &&
      has(intentRs, "最多 {max} 个调用"),
    "批解析没立起来：actions / calls 两件外衣要认、单动作不许被误伤、超上限必须报上限让它拆批（静默截断 = 丢调用）");

  check("U25", "batch-calls",
    has(intentRs, "fn conflicts(") &&
      has(intentRs, "fn waves_by(") &&
      has(intentRs, "fn batch_waves(") &&
      has(intentRs, "fn shape_of(") &&
      has(stepRs, "batch_waves_for_step("),
    "波次模型没立起来：冲突判定与分层必须是同一个内核（父子共用 waves_by），否则父子对「哪条能并发」的判定会分叉");

  check("U25", "batch-calls",
    has(intentRs, "fn run_wave(") &&
      has(intentRs, "std::thread::scope") &&
      has(intentRs, "struct JoinAll") &&
      has(intentRs, "fn flush_write_disk(") &&
      has(intentRs, "fn read_group("),
    "波内并发没落地：只读/写入/执行要各起线程、连接要 join_all；写入落盘只许一个实现（flush_write_disk）");

  check("U25", "batch-calls",
    has(intentRs, "fn before_of(") &&
      has(intentRs, "fn record(") &&
      has(intentRs, "ensure_backup_dir("),
    "写入拆分不对：磁盘阶段可并发、记账（覆盖层/变更表）必须在主线程按声明顺序补 —— 内存状态只有一份");

  check("U25", "batch-calls",
    has(configRs, "pub batch: bool") &&
      has(configRs, "pub batch_max: usize") &&
      has(configRs, "pub batch_parallel: bool") &&
      has(cfgBridgeRs, "engine::config::schema()") &&
      zhKeys.has("config.field.harness.agent.batch") &&
      zhKeys.has("config.field.harness.agent.batch_max") &&
      zhKeys.has("config.field.harness.agent.batch_parallel"),
    "批开关没贯通：引擎 AgentConfig 三字段 → 配置桥（照 schema）→ UI 三字段，缺一样用户就没法一行回退");

  check("U25", "batch-calls",
    has(intentRs, "fn batch_hint(") &&
      has(intentRs, "has_plan") &&
      has(intentRs, "一批最多") &&
      has(stepRs, "batch_hint(cfg.agent.batch_max, false)") &&
      has(stepRs, "parse_step_actions("),
    "提示词没教会批协议（模型不会凭空发明 actions 字段）；子步骤那份必须 has_plan=false —— STEP_SYSTEM 里没有清单工具，提了它会去找一个不存在的能力");

  check("U25", "batch-calls",
    has(intentRs, "cfg.agent.batch_max, cfg.agent.batch") &&
      has(intentRs, "batch_parallel") &&
      has(intentRs, "并发跑"),
    "开关与接受判定没绑在一起：提示词教不教批、引擎收不收批必须同一个开关（不虚报能力）；提示词还要说清「一批并发跑、只有同文件与托管进程保序」");

  // U36 param-shapes + history-fold（v0.11）：原子**能力集不变**（仍是 read/write/execute/
  // connect 四个），变的是参数形状与历史管理。四条不许退：
  // ① read 能只取一段（offset/limit）；② write 能只传改动（edits 锚点），且匹配规则
  // **复用 repair::replace_unique 而不是 apply_edits** —— 后者自己写盘，会绕过覆盖层与
  // 备份/暂存记账（确认模式下磁盘本就不该动，那是"read 看到新内容 / execute 看到旧内容"
  // 那场 65 轮误侦察的根）；③ 工具循环把超出保留窗口的老轮次折成一行事实（否则上面两处
  // 省下的 token 会被"历史每轮重发"复利吃掉）；④ 提示词判据**成对写**，且老那句"必须是
  // 整份内容"必须消失 —— 留着它，模型看见新形状也不会用（同 WEB_SEARCH_HINT 的教训）。
  // 放在这里（U25 之后）而不是 U35 旁边：`intentRs` / `stepRs` / `configRs` 都是这一带
  // 才 `read()` 出来的 const，提前用会撞 TDZ。
  check("U36", "param-shapes-history-fold",
    has(intentRs, "struct ReadSpec") &&
      has(intentRs, "pub offset: Option<usize>") &&
      has(intentRs, "pub limit: Option<usize>") &&
      has(intentRs, "fn window_of(") &&
      has(intentRs, "接着读用 offset="),
    "read 的窗口形态没落地：表头必须写出「共 N 行 / 还有 M 行，接着读用 offset=X」，否则模型不知道缺口在哪、怎么接上");

  check("U36", "param-shapes-history-fold",
    has(intentRs, "struct WriteSpec") &&
      has(intentRs, "enum WriteBody") &&
      has(intentRs, "Edits(Vec<AgentEdit>)") &&
      has(intentRs, "fn resolve_write(") &&
      has(intentRs, "fn apply_write("),
    "write 的两副面孔没落地：content（整份）与 edits（锚点）要归约到同一条落盘通道");

  check("U36", "param-shapes-history-fold",
    has(intentRs, "repair::replace_unique(") && !has(intentRs, "repair::apply_edits("),
    "锚点匹配必须复用 repair::replace_unique（恰好一次 + 行尾归一化）；不许调 repair::apply_edits —— 它自己写盘，会绕过覆盖层与备份/暂存记账");

  check("U36", "param-shapes-history-fold",
    has(intentRs, "fn fold_history(") &&
      has(intentRs, "struct RoundSlot") &&
      has(intentRs, "cfg.agent.history_trim") &&
      has(intentRs, "cfg.agent.history_keep_rounds") &&
      has(intentRs, "正文已从上下文移除"),
    "历史折叠没落地：老轮次要压成一行事实（且写明正文去哪了、要看就重新 read），否则 read/write 省下的 token 会被「历史每轮重发」复利吃掉");

  check("U36", "param-shapes-history-fold",
    has(configRs, "pub history_trim: bool") &&
      has(configRs, "pub history_keep_rounds: usize") &&
      has(cfgBridgeRs, '"agent.history_keep_rounds"') &&
      zhKeys.has("config.field.harness.agent.history_trim") &&
      zhKeys.has("config.field.harness.agent.history_keep_rounds"),
    "折叠开关没贯通：引擎 AgentConfig 两字段 → 配置桥（0 轮 = 连当前这轮都折掉，必须按非法值拒）→ UI 两字段，缺一样用户就没法一行回滚");

  check("U36", "param-shapes-history-fold",
    has(intentRs, "该用窗口") &&
      has(intentRs, "别用窗口") &&
      has(intentRs, "别用 edits") &&
      has(intentRs, "别用 content") &&
      !has(intentRs, "交回的必须是整份内容") &&
      !has(intentRs, "再用 write 交回整份新内容") &&
      has(stepRs, "edits") &&
      has(stepRs, "offset") &&
      !has(stepRs, "交回的必须是整份内容"),
    "提示词的判据必须成对写（该用/别用），且老那句「必须是整份内容」要消失 —— 留着它模型看见新形状也不会用；子步骤那份要各自自洽");

  // U37 config-index：配置页十几个块、几十行，一屏装不下 → 正文只给"块标题条"，
  // 右侧大纲区渲染同一份**块索引**（标题 + 字段数）。四条不许退：
  // ① 索引带双守卫（ownerIs + activeTabId）—— 大纲区只有一块 DOM，文件大纲与会话任务计划
  //    都写它；后台标签不许覆盖前台的（session.js::renderOutline 的同一课）；
  // ② **折叠只切 class，不重建 DOM** —— controls 是 render 那一刻抓下来的节点引用
  //    （下标 = row.idx），重建 = 用户改过的值在 collect() 眼里变回初始值 → 保存时静默
  //    漏提交，而且界面看着完全正常。这是本次改动最容易踩的坑；
  // ③ 折叠在 CSS 里是 display:none（节点还在，collect 读得到），不是 visibility/height:0
  //    （那会留下不可见却仍能 Tab 聚焦的控件）；
  // ④ 每个 harness 子段都有**可读标题**（zh + en）—— 缺了就会显示 `harness.llm` 这种原始
  //    键名，那正是"看得累"的根因之一。子段名不是手写死表，而是从 AppConfig 解析出来：
  //    加了配置组却不加文案，这条立刻红。
  const renderBody = configJs.slice(
    configJs.indexOf("function render(tabRef)"), configJs.indexOf("function readValue("));
  check("U37", "config-index",
    has(configJs, "function renderOutline(") &&
      has(configJs, "el.dataset.configTab !== t.id") &&
      has(configJs, "window.state?.activeTabId !== t.id"),
    "配置块索引没带双守卫：缺了它会用后台标签的索引盖掉前台的文件大纲 / 会话计划");

  // 归属标记要在建 DOM **之前**打 —— 放到函数尾巴上，applyFold / renderOutline 会被
  // 自己的 ownerIs 挡掉（表现：折叠态要等下一次渲染才生效，第一次点没反应）
  const ownerAt = renderBody.indexOf("view.dataset.configTab = tab.id");
  const buildAt = renderBody.indexOf("body.innerHTML");
  check("U37", "config-index",
    ownerAt >= 0 && buildAt >= 0 && ownerAt < buildAt &&
      has(renderBody, "applyFold(tab)") && has(renderBody, "renderOutline(tab)") &&
      has(renderBody, "data-section="),
    "render 未接上折叠与索引（或归属标记排在建 DOM 之后）：块上还要有 data-section，索引点击靠它定位");

  const foldBody = configJs.slice(
    configJs.indexOf("function applyFold("), configJs.indexOf("function sectionAtTop("));
  const revealBody = configJs.slice(
    configJs.indexOf("function revealBlock("), configJs.indexOf("function renderControl("));
  check("U37", "config-index",
    foldBody.length > 0 && revealBody.length > 0 &&
      has(foldBody, "classList.toggle(") && !has(foldBody, "innerHTML") &&
      !has(revealBody, "innerHTML"),
    "折叠路径里出现 innerHTML 了：重建 DOM = controls 引用失效 = 用户改过的值在 collect() 眼里变回初始值（保存静默漏提交）");

  check("U37", "config-index",
    /\.config-section--folded \.config-section-rows\s*\{[^}]*display:\s*none/.test(stylesCss),
    "折叠必须落成 display:none（visibility:hidden / height:0 会留下不可见但仍能 Tab 聚焦的控件）");

  check("U37", "config-index",
    has(configJs, '".config-section-head"') && has(configJs, '"[data-config-fold-all]"') &&
      has(configJs, '"[data-config-section]"') &&
      has(configJs, 'addEventListener("click", (e) => {') &&
      /\.outline-item--active\s*\{/.test(stylesCss) && has(configJs, "syncOutlineActive("),
    "块标题条点击 / 索引点击 / 全局开关 / 滚动高亮没接全（高亮缺了，索引就只是另一份要读完的清单）");

  // 子段标题：从 AppConfig 的字段推出"有哪些配置块"，逐个要求 zh + en 文案。
  // 叶子键（#[serde(default = "...")]，无点路径）归到 general 一块。
  const appCfgSrc = configRs.slice(
    configRs.indexOf("pub struct AppConfig"), configRs.indexOf("impl Default for AppConfig"));
  const hiddenSrc = configRs.slice(
    configRs.indexOf("const FORM_HIDDEN"),
    configRs.indexOf("];", configRs.indexOf("const FORM_HIDDEN")));
  // 隐藏项是前缀匹配：**不带点**的（如 `entropy`）盖住整段 → 这个块不会出现在表单里；
  // 带点的（如 `llm.api_key`）只藏单个键，`llm` 这个块照样要有标题。
  const hiddenTop = new Set(
    [...hiddenSrc.matchAll(/"([\w.]+)"/g)]
      .map((m) => m[1])
      .filter((s) => !s.includes(".")));
  const groups37 = [];
  let isLeaf = false;
  let hasLeaf = false;
  for (const line of appCfgSrc.split("\n")) {
    const t = line.trim();
    if (t.startsWith("#[serde(default = ")) isLeaf = true;
    else if (t === "#[serde(default)]") isLeaf = false;
    const f = line.match(/^\s+pub (\w+):/);
    if (!f) continue;
    if (isLeaf) hasLeaf = true;
    else groups37.push(f[1]);
    isLeaf = false;
  }
  const wanted = groups37.filter((g) => !hiddenTop.has(g))
    .map((g) => `config.section.harness.${g}`);
  if (hasLeaf) wanted.push("config.section.harness.general");
  const missTitle = wanted.filter((k) => !zhKeys.has(k) || !enKeys.has(k));
  check("U37", "config-index",
    wanted.length >= 12 && missTitle.length === 0,
    `配置块缺可读标题（会退化成 harness.llm 这种原始键名）: ${missTitle.join(", ")}（解析出 ${wanted.length} 块）`);

  const ui37Keys = ["config.outline_head", "config.outline_expand_all",
    "config.outline_collapse_all", "config.outline_empty", "config.fold_tip"];
  const miss37 = ui37Keys.filter((k) => !zhKeys.has(k) || !enKeys.has(k));
  check("U37", "config-index", miss37.length === 0,
    `索引交互文案缺键（两语言都要）: ${miss37.join(", ")}`);

  // U26 ask-user：第五个动作（v0.8）—— **需求歧义只能问委托人**。
  // 病根：四个效果原语取不到"意图"（read 读磁盘、execute 跑命令、connect 连机器，
  // 另一端都不是人），于是模型只剩两条烂路 —— 猜（做错到 final 才发现），或把问题塞进 final
  // （final 的语义是**交付**：未开工被记成已完成，而且用户答完是全新一轮、游标与覆盖层全丢）。
  // 口径与纪律：模型侧叫 `ask_user`；引擎内部是控制动作（独占一轮、不进批、子步骤 Unsupported）；
  // 答案以 `kind=user` 的观察回灌且**不构成授权**；拿不到答案一律 fail-closed（绝不假设同意）。
  const askRs = read("src-tauri/src/agent/ask.rs");
  const sessRs = read("src-tauri/src/agent/sessions.rs");
  check("U26", "ask-user",
    has(intentRs, "Ask(AskSpec)") &&
      has(intentRs, '"ask_user" | "ask"') &&
      has(intentRs, "不许有") &&
      has(intentRs, "最多 5 个") &&
      has(intentRs, "越界"),
    "第五个动作没进动作集：解析要认 ask_user（含 ask 别名），并挡住伪造答案（answer/granted 一律拒）与非法选项");
  check("U26", "ask-user",
    has(intentRs, "ask_user 不能放进批里") &&
      has(intentRs, 'StepAction::Unsupported("ask_user")'),
    "控制动作纪律没落：ask_user 必须独占一轮（不进批）；子步骤没有交互权（Unsupported 回一条，不整步失败）");
  check("U26", "ask-user",
    has(intentRs, "pub trait Asker") &&
      has(intentRs, "pub struct NoAsker") &&
      has(intentRs, "pub struct AskSpec") &&
      has(intentRs, "pub struct AskRecord") &&
      has(intentRs, "pub async fn run_with_ask(") &&
      has(intentRs, "fn ask_failed_note(") &&
      has(intentRs, "fn ask_answer_note("),
    "提问通道没落地：Asker 契约 + NoAsker（headless 空实现）+ 两条观察文案（答到 / 没答到）");
  check("U26", "ask-user",
    has(intentRs, "不构成任何门禁的授权") && has(intentRs, "ask_failed_note"),
    "回答的语义必须写清：**它不是授权** —— 否则 ask_user 就成了绕过暂存确认（「用户看过改动内容才落盘」）的后门");
  check("U26", "ask-user",
    has(askRs, 'emit("agent://ask"') &&
      has(askRs, "fn deliver(") &&
      has(askRs, "fn drop_all(") &&
      has(mainRs, "agent_ask_answer") &&
      has(sessionJs, '"agent_ask_answer"') &&
      has(sessionJs, '"agent://ask"'),
    "宿主通道没接通：事件 agent://ask + 投答案命令 + 取消时清空待答（否则用户取消后循环还挂到超时）");
  check("U26", "ask-user",
    has(sessionJs, "function askHtml(") &&
      has(sessionJs, "function showAskCard(") &&
      has(sessionJs, "function answerAsk(") &&
      has(sessionJs, "session-ask-why") &&
      has(sessionJs, "data-ask-send") &&
      has(sessionJs, "expired"),
    "问题卡没落地：question / **why**（为什么问）/ 选项 / 自由输入都要有；失效要照实说，不假装成功");
  check("U26", "ask-user",
    has(configRs, "pub struct AskConfig") &&
      has(configRs, "pub max_per_run: u32") &&
      has(cfgBridgeRs, "engine::config::schema()") &&
      zhKeys.has("config.field.harness.ask.enabled") &&
      zhKeys.has("config.field.harness.ask.timeout_secs") &&
      zhKeys.has("config.field.harness.ask.max_per_run"),
    "配置三键没贯通（引擎 → 宿主桥 → 表单）：能不能问 / 等多久算没人回答 / 最多问几次都要能一行回退");
  check("U26", "ask-user",
    has(sessRs, "pub struct AskSnap") && has(sessRs, "pub ask: Vec<AskSnap>"),
    "提问留痕没进会话存档：不声明字段会被 agent_session_save 的往返顺手抹掉（与 plan/verify/reflect 同一个坑）");

  // U27 logo-assets：应用图标（RYX 字母标）。
  //   病根是"图形资产没有真相源"：矢量源和 Tauri 要的那一整套 PNG/ICO 手工各维护一份，
  //   改一次就要重新导出十几个尺寸 → 必然漂移；而 `tauri.conf.json` 里 `bundle.icon` 整个
  //   缺失时**图标换了也不生效**（窗口图标取自它，exe 资源也是它）。
  //   所以这条契约钉四件事：四个矢量变体都在 / 两处字母轮廓必须逐字节相同（防止只手改一份）
  //   / bundle.icon 非空且每个文件真的在磁盘上 / Windows ico 是多尺寸（单尺寸 ico 在任务栏会糊）。
  const LOGO_FILES = ["ui/logo.svg", "ui/logo-light.svg", "ui/logo-mark.svg", "ui/logo-mono.svg"];
  const logoSvg = read("ui/logo.svg");
  const markSvg = read("ui/logo-mark.svg");
  // 注意正则要限定 ` d="`：`id="tile"` 里也含 `d="` 子串，宽松匹配会把渐变 id 当成路径。
  const dOf = (s) => (s.match(/\sd="[^"]+"/g) || []).join("|");
  check("U27", "logo-assets",
    LOGO_FILES.every((f) => fs.existsSync(path.join(ROOT, f)) && read(f).length > 200) &&
      has(logoSvg, 'aria-label="RYX"') && has(logoSvg, 'stroke-linecap="round"') &&
      (logoSvg.match(/<path d=/g) || []).length === 3,
    "缺少矢量源：ui/logo{,-light,-mark,-mono}.svg 必须齐（RYX 三笔，圆头描边）");
  check("U27", "logo-assets",
    dOf(logoSvg) === dOf(markSvg) && has(markSvg, 'stroke="url(#mark)"'),
    "logo.svg 与 logo-mark.svg 的字母轮廓必须逐字节相同 —— 各改一份就是漂移的开始");
  const bundleIcon = (confJson.match(/"icon"\s*:\s*\[([^\]]+)\]/s) || [, ""])[1]
    .split(",").map((s) => s.trim().replace(/"/g, "")).filter(Boolean);
  check("U27", "logo-assets",
    bundleIcon.length >= 3 &&
      bundleIcon.every((p) => fs.existsSync(path.join(ROOT, "src-tauri", p))) &&
      bundleIcon.some((p) => p.endsWith(".ico")) && bundleIcon.some((p) => p.endsWith(".icns")),
    `tauri.conf.json 的 bundle.icon 缺失或指向不存在的文件：${JSON.stringify(bundleIcon)}（缺它则窗口与 exe 都用不上这套图）`);
  const ico = fs.readFileSync(path.join(ROOT, "src-tauri/icons/icon.ico"));
  check("U27", "logo-assets",
    ico.readUInt16LE(0) === 0 && ico.readUInt16LE(2) === 1 && ico.readUInt16LE(4) >= 4,
    `icon.ico 不是合法的多尺寸图标（目录项 ${ico.readUInt16LE(4)} 个）：单尺寸在任务栏会糊`);
  check("U27", "logo-assets",
    !fs.existsSync(path.join(ROOT, "src-tauri/icons/ios")) &&
      !fs.existsSync(path.join(ROOT, "src-tauri/icons/android")) &&
      fs.existsSync(path.join(ROOT, "tools/logo/build_logo.py")),
    "本仓库只做桌面端：cargo tauri icon 顺带生成的 ios/ android/ 不该入库；生成器 tools/logo/build_logo.py 必须在");
  check("U27", "logo-assets",
    has(html, 'rel="icon"') && has(html, 'href="logo.svg"'),
    "index.html 没挂 favicon（浏览器直开 UI 时页签上是空白图标）");

  // U28 session-persistence：会话历史必须活过重启（v0.9）。
  // 病根不是"存不上" —— 文件一直在写（实测 cloud-shop 里 27 个、最新那条就是当天最后一次对话）；
  // 是**没加载**：`refreshList()` 原先只在 `attach()` 末尾被调一次，而那一刻项目往往还没打开 →
  // `root()` 为空 → 早退，之后再没人叫它 → 面板永远写着"暂无会话"，用户看到的就是"历史全丢"。
  // 五环：① 打开项目的收口点显式同步一次（并把最近一条有消息的会话接上）；
  // ② 用户那句话立刻落盘（run 崩了也不至于整段消失）；③ 原子写（截断式写入被中断会留半截
  // JSON，而 list 会跳过它 = 那段对话整段消失）；④ 坏文件只影响自己（不许把列表打空）；
  // ⑤ 空会话不落盘（不留幽灵条目）+ 删除幂等。
  check("U28", "session-persistence",
    has(commandJs, "SessionUI?.syncForProject?.(") &&
      has(sessionJs, "async function syncForProject(") &&
      has(sessionJs, "openSessionById(newest.id)"),
    "打开项目时没同步会话列表：启动那次 attach() 早退之后没人再叫它 —— 文件在盘上，面板却永远「暂无会话」");
  check("U28", "session-persistence",
    has(sessionJs, "先落盘用户这句话"),
    "用户那句话没有立刻落盘：run 跑一半崩掉 / 被强杀，整段对话就没了");
  check("U28", "session-persistence",
    has(sessRs, "原子写") && has(sessRs, 'with_extension("json.tmp")') && has(sessRs, "fs::rename"),
    "会话不是原子写：截断式写入被中断会留下半截 JSON，而 list 会跳过它 = 那段历史整段消失");
  check("U28", "session-persistence",
    has(sessRs, "坏文件只影响它自己"),
    "坏文件容错没被钉住：一条坏文件不许把整个列表打空（那是「历史全丢」的另一条可能路径）");
  check("U28", "session-persistence",
    has(sessRs, "删除会话。**幂等**") &&
      !has(sessRs, "会话不存在") &&
      has(agentModRs, "空会话不落盘"),
    "空会话与删除：新建不许落空文件（面板里全是幽灵条目）、删除要幂等（文件不在也算成功）");

  // U29 persist-no-clobber：落盘不许**弄丢**会话内容（v0.10）。
  // `persist()` 里那句 `Object.assign(s, saved)` 看着无害（"让内存跟上后端"），实则把
  // **请求发出那一刻**的快照盖回活对象：请求之后才 push 的消息不在那份快照里，于是被抹掉。
  // 会话内容的事实源是内存里那份 `s`，后端只是持久化通道 —— 回显只准同步元数据。
  check("U29", "persist-no-clobber",
    has(sessionJs, "s.updated_at = saved.updated_at") &&
      !/^[ \t]*Object\.assign\(s, saved\);[ \t]*\r?$/m.test(sessionJs),
    "persist 把后端回显整体 assign 回了会话对象：回显是「请求发出那一刻」的快照，会把之后新 push 的消息（运行中的助手回复）从 s.messages 里抹掉 —— 2026-09-21 那轮「飞机大战」的回复连同计划快照就是这么没的");
  check("U29", "persist-no-clobber",
    !/^[ \t]*persist\(s\);[ \t]*\r?$/m.test(sessionJs),
    "有落盘调用没 await：同一会话两次落盘共用同一个 .json.tmp，并发时必有一次 rename 失败（用户看到「会话保存失败」，盘上留下的是旧内容）");

  // U30 proc-log：托管进程的**输出**要看得到（服务面板的第二只眼）。
  //   病根不是"没记日志"（`proc::start` 一 spawn 就把 stdout+stderr 重定向进
  //   `.ruyix/proc/pN.log`，`ProcInfo.log` 连绝对路径都带回来了）—— 缺的是"读它"的入口。
  //   三条硬约束：
  //     · 切分点只落在换行上：日志按字节存、解码走"严格 UTF-8 失败整体回退活动代码页"，
  //       切在半个中文字符中间 = **整段**乱码（不是局部花屏）；
  //     · 末行没写完不发（`tail -f` 的行为），但进程已结束就照发 —— 否则"启动失败、
  //       最后一行没换行"永远看不到；
  //     · xterm 的 `convertEol` 必须有：日志是 LF，而 xterm 里 `\n` 只下移不回列首，
  //       不转换整屏输出会斜成阶梯。
  const procLogJs = read("ui/proc-log.js");
  const zhJson = JSON.parse(read("ui/lang/zh-CN.json"));
  const enJson = JSON.parse(read("ui/lang/en.json"));
  check("U30", "proc-log",
    has(managedRs, "pub struct LogChunk") && has(managedRs, "pub fn read_log_chunk") &&
      has(managedRs, "next_offset") && has(managedRs, "truncated_head") &&
      has(managedRs, "fn skip_to_line_start"),
    "proc.rs 缺增量读日志的入口（LogChunk / read_log_chunk / 行首对齐）");
  check("U30", "proc-log",
    /fn proc_log_read\(/.test(mainRs) && has(mainRs, "proc_log_read,"),
    "main.rs 没注册 proc_log_read —— 前端拿不到日志");
  check("U30", "proc-log",
    ["menu-service", "service-view", "proc-log-view", "proc-log-panes"]
      .every((id) => has(html, `id="${id}"`)) && has(html, 'src="proc-log.js"'),
    "index.html 缺少输出视图容器（proc-log-view / proc-log-panes）或没挂 proc-log.js");
  check("U30", "proc-log",
    has(procLogJs, "convertEol") && has(procLogJs, '"proc_log_read"') &&
      has(procLogJs, "offset: tab._offset") && !has(procLogJs, "terminal-container"),
    "输出面板：convertEol 没开（LF 会让输出斜成阶梯）/ 增量读没带 offset / 复用了 PTY 的终端容器");
  check("U30", "proc-log",
    has(mainJs, "tab._isProcLog") && has(mainJs, "showProcLogView") &&
      has(mainJs, "ProcLogUI?.blur") && has(mainJs, "ProcLogUI?.close(tab)") &&
      /_isProcLog\)\s*return/.test(mainJs),
    "main.js 缺输出标签页分支：切走不停轮询 / 关标签页不 dispose（后台会一直问后端）");
  check("U30", "proc-log",
    has(read("ui/service.js"), "data-output") && has(read("ui/service.js"), "openLog") &&
      has(commandJs, '"log"') && has(commandJs, "ServiceUI?.openLog"),
    "服务表缺「输出」入口，或命令栏没有 `service log <pid>`");
  check("U30", "proc-log",
    has(zhJson["proc_log.truncated"] || "", "{n}") &&
      has(enJson["proc_log.truncated"] || "", "{n}") &&
      !!zhJson["service.btn_output"] && !!enJson["service.btn_output"] &&
      !!zhJson["proc_log.ended"] && !!enJson["proc_log.ended"],
    "输出面板的文案没进语言包（中英都要，且「跳过前 {n} 字节」要带占位符）");

  // U17 verify-gate：机械验证门禁（v0.3）——"有改动 → 交付前必有验证结论；未通过不放行"。
  // 四环缺一不可：窄层（暂存内容语法检查）→ 全量层（复用 verify::run）→ 失败分支拒绝交付并回灌
  // → 预算上限（不许无限修）；前端还要把结论显示出来，否则用户看不到"这轮验没验"。
  check("U17", "verify-gate",
    has(intentRs, "gate_before_final") && has(intentRs, "staged_syntax_checks") &&
      has(intentRs, "verify::run") && has(intentRs, "max_full_attempts") &&
      has(intentRs, "full_failures"),
    "引擎缺门禁：agent.rs 少了 gate_before_final / 窄层 / 全量层 / 预算上限");
  check("U17", "verify-gate",
    /"failed" => \{[\s\S]{0,200}?return Some\(observation\)/.test(intentRs),
    "验证失败没有拒绝交付：failed 分支必须 return Some(观察) 把模型打回");
  check("U17", "verify-gate",
    has(sessionJs, "gateHtml") && has(sessionJs, "session-gate") &&
      has(stylesCss, ".session-gate") && has(agentModRs, "verifications"),
    "验证/复核结论没显示：session.js 缺 gateHtml/样式，或 ReplyAgent 没透传 verifications");

  // U18 reflect-clean-context：复核必须是**另一个干净上下文**的只读 agent。
  // 三条钉子：自己建 messages（不接会话历史）／只认 read（没有写入通路）／判据由事实选 + 失败降级。
  const reflectRs = read("crates/harness-engine/src/reflect.rs");
  check("U18", "reflect-clean-context",
    has(reflectRs, "pub const REFLECT_SYSTEM") && has(reflectRs, "ChatMessage::system(REFLECT_SYSTEM)") &&
      !has(reflectRs, "tail_history") && !has(reflectRs, "history"),
    "复核上下文不干净：reflect.rs 必须自己建 messages，不得接会话历史");
  check("U18", "reflect-clean-context",
    has(reflectRs, "复核只允许 read") && !has(reflectRs, "WritePolicy") && !has(reflectRs, "tool_write"),
    "复核不是只读：reflect.rs 里出现了写入通路（第二个写手会带来冲突）");
  check("U18", "reflect-clean-context",
    has(reflectRs, "select_rubric") && has(reflectRs, "degraded(") && has(intentRs, "suspect()"),
    "反思缺判据选择（有无改动 → rubric）或降级路径（复核失败不许把 run 判死）");

  // U31 editor-virtual-render（样式侧）：这两处**静默退化**——改了不报错，只会悄悄变慢/错位。
  check("U31", "editor-virtual-render",
    /\.editor-code-backdrop\s*\{[^}]*position:\s*absolute/.test(stylesCss),
    "backdrop 不再是绝对定位：它和 textarea 同处一个 grid 单元时，backdrop 一变 layout 就要重新" +
      "问 textarea 的「内在高度」→ 把全文每行重排一遍（实测 25000 行每次窗口重绘 110ms，绝对定位后 1.4ms）");
  check("U31", "editor-virtual-render",
    /\.editor-code-container\s*\{[^}]*position:\s*relative/.test(stylesCss) &&
      !/\.editor-code-container\s*\{[^}]*display:\s*grid/.test(stylesCss),
    "代码区容器丢了 position:relative 或退回了 grid：backdrop 的定位基准与「移出尺寸计算」都靠这两条");
  check("U31", "editor-virtual-render",
    has(html, 'wrap="off"'),
    'textarea 缺 wrap="off"：它默认软换行，长行会在内部折成两行而 backdrop 按一行画，光标与高亮从折行处起错开');
  check("U31", "editor-virtual-render",
    has(mainJs, "EDITOR_LINE_H = 20") && has(mainJs, "EDITOR_VPAD = 16") &&
      /\.code-line\s*\{[^}]*height:\s*20px/.test(stylesCss) &&
      /\.editor-textarea\s*\{[^}]*padding:\s*8px 16px/.test(stylesCss),
    "虚拟化的行高/内边距常量与 CSS 脱节：占位高度会整体偏移（滚动条长度与行号位置全错）");
  // 三个渲染入口必须**都**走 setEditorContent：漏一个就会出现「textarea 有显式高度
  // 但 overlay 没虚拟化」或反过来的半吊子状态（编辑器塌成 2 行 / 回车滚不动）。
  const editorBodyOf = (name) => {
    const i = mainJs.indexOf("function " + name + "(");
    if (i < 0) return "";
    const j = mainJs.indexOf("\nfunction ", i + 1);
    return mainJs.slice(i, j < 0 ? mainJs.length : j);
  };
  const entryPoints = ["renderHighlightedCode", "renderPlainCode", "renderTerminalOutput"];
  check("U31", "editor-virtual-render",
    entryPoints.every((n) => editorBodyOf(n).includes("setEditorContent(")),
    "渲染入口没走 setEditorContent：" +
      entryPoints.filter((n) => !editorBodyOf(n).includes("setEditorContent(")).join(" / ") +
      "（三者必须都走，它俩才是「显式高度 + 窗口渲染」成套出现的地方）");
  // P3：行必须由**前端**按 tab.content 自己切。后端回的是 `code.lines()` 的行（吃掉末尾空行），
  // 拿它去拼 textarea 的值 = 文件末尾换行被静默吃掉，按键后固化、写回真丢。
  check("U31", "editor-virtual-render",
    editorBodyOf("renderHighlightedCode").includes("tab.content") &&
      editorBodyOf("renderHighlightedCode").includes('split("\\n")'),
    "renderHighlightedCode 不再按 tab.content 自己切行：行数一旦取自后端，textarea 的值就会与 " +
      "tab.content 差一个末尾换行（写回时把文件末尾的换行删掉）");
  // P3：片段是扁平三元组 [start, end, tagIdx]，渲染直接按下标读、不建每 span 的对象
  check("U31", "editor-virtual-render",
    editorBodyOf("editorLineHtml").includes("k + 2 < flat.length") &&
      editorBodyOf("editorLineHtml").includes("m.tags"),
    "editorLineHtml 没按「扁平三元组 + 名表」渲染：退回逐 span 建对象（5000 行上万次分配），" +
      "或名表没接上（tag 会渲染成 undefined）");
  // U32：滚动条契约。编辑区**只能有 .editor-view 一个**滚动容器。
  // textarea 天生是滚动容器（Chromium 把作者写的 overflow: visible 当 auto），它的正文
  // 一旦装不下自己那一格，就会①长出自己的滚动条（与 .editor-view 叠成双滚动条）
  // ②为了露出光标而内部滚动（光标落在框边、与背板字形错开）。
  check("U32", "editor-layout-real",
    /\.editor-textarea\s*\{[^}]*overflow:\s*hidden/.test(stylesCss),
    "textarea 没关掉自己的滚动条（.editor-textarea { overflow: hidden }）：它天生是滚动容器，" +
      "正文比格子宽时会与 .editor-view 叠成双滚动条");
  check("U32", "editor-layout-real",
    editorBodyOf("paintEditorWindow").includes("syncCodeWidth(") &&
      editorBodyOf("setEditorContent").includes("widthSynced: false"),
    "没把「代码区宽度 = 最宽行宽度」这套接上（paintEditorWindow 里调 syncCodeWidth、" +
      "setEditorContent 里给 widthSynced 初值）：backdrop 绝对定位后没人再撑宽容器，" +
      "容器只剩视口宽 → textarea 的正文装不下 → 双滚动条 + 光标错位");
  check("U32", "editor-layout-real",
    editorBodyOf("syncCodeWidth").includes("widthSynced") &&
      editorBodyOf("setupEditorVirtualScroll").includes("widthSynced = false"),
    "syncCodeWidth 少了「本次内容已同步」的短路标志，或视口变化（ResizeObserver）时没重置它：" +
      "前者会让滚动路径每帧都量一次宽度（甚至改宽 → textarea 全文重排），" +
      "后者会在窗口行数变化后留着一个过时的宽度");
}

// ============================================
// 场景 2：会话对话（微型 DOM stub：动态构建的聊天 DOM + tab 系统桩）
// ============================================

/** stub 元素：支持动态子元素查询（聊天 DOM 由 createElement 组装） */
function makeEl(id) {
  const subs = new Map();
  const el = {
    id,
    style: {},
    innerHTML: "",
    textContent: "",
    className: "",
    value: "",
    disabled: false,
    scrollTop: 0,
    scrollHeight: 0,
    dataset: {},
    children: [],
    listeners: {},
    classList: {
      add() {}, remove() {}, toggle() {}, contains() { return false; },
    },
    addEventListener(type, fn) {
      (this.listeners[type] = this.listeners[type] || []).push(fn);
    },
    querySelector(sel) {
      if (!subs.has(sel)) subs.set(sel, makeEl(this.id + " " + sel));
      return subs.get(sel);
    },
    querySelectorAll() {
      return [];
    },
    appendChild(child) {
      this.children.push(child);
    },
    click() {
      for (const fn of this.listeners.click || []) fn({});
    },
    focus() {},
    scrollTo() {},
  };
  return el;
}

async function runSessionChecks() {
  const elements = new Map();
  const el = (id) => {
    if (!elements.has(id)) elements.set(id, makeEl(id));
    return elements.get(id);
  };
  const activeTabs = { switched: [], rendered: 0 };

  const sandbox = {
    window: { SessionUI: null, state: { tabs: [], currentProject: null } },
    document: {
      getElementById: el,
      createElement: (tag) => makeEl("<" + tag + ">"),
      querySelector() {
        return null;
      },
    },
    // main.js 的 tab 系统桩（openSession 依赖）
    state: { tabs: [], currentProject: null },
    renderTabs() {
      activeTabs.rendered += 1;
    },
    switchTab(id) {
      activeTabs.switched.push(id);
    },
    closeTab() {},
    setTimeout(fn) {
      fn(); // 演示回复立即送达
      return 0;
    },
    clearTimeout() {},
    setInterval() {
      return 0;
    },
    clearInterval() {},
  };
  sandbox.window.state = sandbox.state;

  const saved = ["window", "document", "setTimeout", "clearTimeout", "setInterval",
    "clearInterval", "state", "renderTabs", "switchTab", "closeTab"].map((k) => [k, globalThis[k]]);
  Object.assign(globalThis, sandbox);
  try {
    // eslint-disable-next-line no-new-func
    new Function(read("ui/session.js"))();
    const SessionUI = sandbox.window.SessionUI;

    // ---- U6：模块导出 + attach 初始态 ----
    const fns = ["attach", "handleCommand", "ensureChatEl", "projectClosed",
      "newSession", "openSession", "sendMessage"];
    check("U6", "session-module",
      !!SessionUI && fns.every((f) => typeof SessionUI[f] === "function"),
      `session.js 未完整暴露 SessionUI（缺: ${fns.filter((f) => typeof SessionUI?.[f] !== "function")}）`);
    SessionUI.attach();
    check("U6", "session-attach",
      has(el("agent-env-chips").innerHTML, "后端不可用") &&
        has(el("session-list").innerHTML, "暂无会话"),
      "attach 后初始视图不正确（环境芯片/会话空态）");

    // ---- U7：演示对话（新建会话 → 发消息 → 双向气泡 + 列表刷新）----
    const s = await SessionUI.newSession(false);
    check("U7", "session-new",
      !!s && sandbox.state.tabs.length === 1 && sandbox.state.tabs[0]._isSession === true,
      "newSession 未创建会话 tab");
    const tab = sandbox.state.tabs[0];
    const wrap = SessionUI.ensureChatEl(tab);
    check("U7", "session-new", !!wrap &&
      has(wrap.querySelector("[data-msgs]").innerHTML, "和 agent 聊聊"),
      "聊天 DOM 未构建或空态文案缺失");
    await SessionUI.sendMessage(s, wrap, "冒烟任务：实现 truncate_words 并补测试");
    const msgs = wrap.querySelector("[data-msgs]").innerHTML;
    check("U7", "session-demo",
      has(msgs, "冒烟任务：实现") && has(msgs, "session-msg--user"),
      "发送后未见用户气泡");
    check("U7", "session-demo",
      has(msgs, "演示回复") && has(msgs, "session-msg--agent"),
      "无后端演示回复缺失");
    check("U7", "session-demo",
      s.messages.length === 2 && has(s.title, "冒烟任务"),
      `会话状态不正确（messages=${s.messages.length}，title="${s.title}"）`);
    check("U7", "session-demo",
      has(el("session-list").innerHTML, s.id),
      "发送后会话列表未刷新");

    // ---- U8：项目关闭收口（会话随项目走）----
    SessionUI.projectClosed();
    check("U8", "session-close",
      sandbox.state.tabs.length === 0 &&
        has(el("session-list").innerHTML, "暂无会话"),
      "projectClosed 未收掉会话 tab 或列表未回空态");
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
}

// ============================================
// 场景 3：配置表单回放（微型 DOM stub 驱动 ui/config.js）
//   U11 config-replay  扫描 → 表单渲染（已知项 + 扫描项 + 继承提示）
//   U12 config-save    保存只提交改动行；应用提交整表（刷运行时对象）
//   U13 config-cancel  取消丢弃改动并重扫
// ============================================

/** 从渲染出的 HTML 里解析出表单控件（config.js 用 querySelectorAll("[data-row]") 取） */
function parseControls(html) {
  const decode = (s) =>
    String(s ?? "")
      .replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">")
      .replace(/&quot;/g, '"').replace(/&#39;/g, "'");
  const out = [];
  const re = /<(input|select)\b[^>]*?data-row="(\d+)"[^>]*>/g;
  let m;
  while ((m = re.exec(html))) {
    const attrs = m[0];
    if (m[1] === "select") {
      const end = html.indexOf("</select>", re.lastIndex);
      const inner = html.slice(re.lastIndex, end);
      const sel = inner.match(/<option value="([^"]*)" selected>/);
      out.push({ id: "select-" + m[2], value: sel ? decode(sel[1]) : "", type: "select" });
    } else {
      const type = (attrs.match(/type="([^"]*)"/) || [, "text"])[1];
      out.push({
        id: "input-" + m[2],
        type,
        value: decode((attrs.match(/value="([^"]*)"/) || [, ""])[1]),
        checked: /\schecked>/.test(attrs),
      });
    }
  }
  return out;
}

async function runConfigChecks() {
  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const elements = new Map();
  const el = (id) => {
    if (!elements.has(id)) elements.set(id, makeEl(id));
    return elements.get(id);
  };
  // config.js 每次 render 只调一次 querySelectorAll("[data-row]")，这里按次返回新对象，
  // 并把它挂到 _controls 上给测试取用（对象必须与 config.js 内部持有的是同一批）
  const bodyEl = el("config-body");
  bodyEl.querySelectorAll = (sel) => {
    if (sel !== "[data-row]") return [];
    bodyEl._controls = parseControls(bodyEl.innerHTML);
    return bodyEl._controls;
  };

  // 视图归属标记（config.js 用 dataset.configTab 判定 DOM 属于哪个标签）
  el("config-view").dataset = {};

  const dump = {
    scope: "global",
    dir: "C:\\Users\\test\\.ruyix\\code",
    entries: [
      { section: "ai", key: "model", full_key: "ruyix.code.ai.model", value: "old-model", inherited: null },
      { section: "ai", key: "api_key", full_key: "ruyix.code.ai.api_key", value: "", inherited: { scope: "global", value: "sk-hidden" } },
      { section: "ui", key: "lang", full_key: "ruyix.code.ui.lang", value: "", inherited: { scope: "global", value: "zh-CN" } },
      { section: "custom", key: "flag", full_key: "ruyix.code.custom.flag", value: "true", inherited: null },
    ],
  };
  const calls = [];
  const report = { saved: 1, removed: 0, applied: 0 };
  const appState = { tabs: [], activeTabId: null, currentProject: null };
  const i18n = {
    getLang: () => "zh-CN",
    t(key, params) {
      let s = zh[key] ?? key;
      for (const [k, v] of Object.entries(params || {})) s = s.replaceAll(`{${k}}`, String(v));
      return s;
    },
  };

  // 契约对齐：面板读 window.state，而 main.js 的顶层 const 不会挂到 window 上，
  // 必须靠 `window.state = state` 显式导出。这里按真实契约对齐 stub —— 没导出就不给
  // window.state，让回放如实暴露"项目作用域失效"，而不是被 stub 凭空喂好掩盖掉。
  const exportsState = RE_STATE_EXPORT.test(read("ui/main.js"));
  if (!exportsState) {
    // 没有 window.state 时 config.js 会直接抛错，先明确记一条失败，别让整个场景崩掉
    check("U11", "config-module", false,
      "main.js 未导出 window.state：面板拿不到 state / currentProject（项目作用域全失效）");
    return;
  }

  // 引擎配置 schema 的假实现（真实来源是 harness_engine::config::schema()）。
  // 刻意覆盖全部 kind + 一个 ui:false 的键 —— 后者用来验证前端确实按**引擎的**
  // 判断过滤，而不是自己在那边维护一份"该显示什么"的名单。
  const engineSchema = [
    { path: "workspace_root", kind: "text", default: "C:/x/runs", ui: true, options: [] },
    { path: "llm.temperature", kind: "float", default: "0.2", ui: true, options: [] },
    { path: "llm.max_tokens", kind: "int", default: "8192", ui: true, options: [] },
    { path: "llm.api_key", kind: "text", default: "", ui: false, options: [] },
    { path: "sandbox.mode", kind: "text", default: "require", ui: true,
      options: ["require", "prefer", "off"] },
    { path: "sandbox.image", kind: "text", default: "rust:1", ui: true, options: [] },
    { path: "lint.enabled", kind: "bool", default: "true", ui: true, options: [] },
    { path: "lint.package_dir", kind: "text", default: "", ui: true, options: [] },
    { path: "lint.max_repair_rounds", kind: "int", default: "3", ui: true, options: [] },
    { path: "kb.enabled", kind: "bool", default: "false", ui: true, options: [] },
    { path: "kb.top_k", kind: "int", default: "4", ui: true, options: [] },
    { path: "step.execute_plan", kind: "bool", default: "true", ui: true, options: [] },
    { path: "step.max_steps", kind: "int", default: "96", ui: true, options: [] },
    { path: "agent.max_elapsed_secs", kind: "int", default: "1800", ui: true, options: [] },
    { path: "discover.extra", kind: "list", default: "", ui: true, options: [] },
    { path: "proc.enabled", kind: "bool", default: "true", ui: true, options: [] },
    { path: "proc.max", kind: "int", default: "4", ui: true, options: [] },
    { path: "proc.ready_timeout_secs", kind: "int", default: "60", ui: true, options: [] },
    { path: "ask.enabled", kind: "bool", default: "true", ui: true, options: [] },
    { path: "ask.timeout_secs", kind: "int", default: "300", ui: true, options: [] },
    { path: "ask.max_per_run", kind: "int", default: "4", ui: true, options: [] },
  ];

  const sandbox = {
    I18N: i18n,
    window: Object.assign(
      exportsState ? { state: appState } : {},
      { I18N: i18n, ConfigUI: null },
    ),
    document: {
      getElementById: el,
      querySelector() {
        return null;
      },
      querySelectorAll() {
        return [];
      },
    },
    getTauriInvoke: () => async (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "config_form_load") return dump;
      if (cmd === "config_schema") return engineSchema;
      // 模拟后端写回：空值 = 删键（与 config.rs 的增量语义一致），供重扫读回
      for (const e of args.entries) {
        const i = dump.entries.findIndex((x) => x.section === e.section && x.key === e.key);
        if (!e.value) {
          if (i >= 0) dump.entries.splice(i, 1);
        } else if (i >= 0) {
          dump.entries[i].value = e.value;
        } else {
          dump.entries.push({
            section: e.section, key: e.key, value: e.value, inherited: null,
            full_key: `ruyix.code.${e.section}.${e.key}`,
          });
        }
      }
      return report;
    },
    setStatus() {},
    renderTabs() {},
    switchTab(id) {
      appState.activeTabId = id;
      const t = appState.tabs.find((x) => x.id === id);
      if (t && t._isConfig) sandbox.window.ConfigUI.render(t);
    },
    showConfirm: async () => true,
  };

  const saved = ["window", "document", "getTauriInvoke", "setStatus", "renderTabs",
    "switchTab", "showConfirm", "I18N"].map((k) => [k, globalThis[k]]);
  Object.assign(globalThis, sandbox);
  try {
    // eslint-disable-next-line no-new-func
    new Function(read("ui/config.js"))();
    const ConfigUI = sandbox.window.ConfigUI;
    const fns = ["attach", "handleCommand", "render", "stash", "open", "save", "apply", "cancel"];
    check("U11", "config-module", !!ConfigUI && fns.every((f) => typeof ConfigUI[f] === "function"),
      `config.js 未完整暴露 ConfigUI（缺: ${fns.filter((f) => typeof ConfigUI?.[f] !== "function")}）`);
    ConfigUI.attach();

    // ---- U11：命令系统入口 → 扫描 → 表单 ----
    await ConfigUI.handleCommand("form global");
    const scanCalls = calls.filter((c) => c.cmd === "config_form_load");
    check("U11", "config-scan",
      scanCalls.length === 1 && scanCalls[0].args.scope === "global" &&
        calls.some((c) => c.cmd === "config_schema"),
      `form global 未按契约调用 config_form_load / config_schema: ${JSON.stringify(calls.map((c) => c.cmd))}`);
    const tab = appState.tabs[0];
    check("U11", "config-open",
      !!tab && tab._isConfig === true && tab.configScope === "global" &&
        appState.activeTabId === tab.id,
      "未创建 / 未激活配置标签");
    const html = el("config-body").innerHTML;
    check("U11", "config-render",
      has(html, "AI 助手") && has(html, "API 密钥") && has(html, "ai.toml"),
      "表单未渲染已知 section / 字段 / 文件名");
    check("U11", "config-render",
      has(html, "ruyix.code.custom.flag") && has(html, "扫描"),
      "扫描到的未知键未渲染成通用行");
    const ctrls = el("config-body")._controls;
    check("U11", "config-render",
      ctrls.length >= 17 && el("config-btn-save").disabled === true,
      `控件数 ${ctrls.length}（应 ≥17）；无改动时保存按钮应为禁用`);
    // harness 段由引擎 schema 生成：ui:true 的要出现、ui:false 的不许出现
    // （"该给用户看哪些键"是引擎的判断，前端不自己维护这份名单）
    check("U11", "config-render",
      has(html, "ruyix.code.harness.sandbox.mode") && has(html, "harness.sandbox") &&
        !has(html, 'title="ruyix.code.harness.sandbox.engine"'),
      "harness 段未按引擎 schema 渲染 / 未过滤 ui:false 的键 / 未按 path 首段分组");
    const bits = {
      既有值: has(html, "old-model"),
      继承提示: has(html, "继承自 global: zh-CN"),
      密钥掩码: has(html, "继承自 global: ••••••") && !has(html, "sk-hidden"),
      密码框: has(html, 'type="password"'),
      开关勾选: /\schecked>/.test(html),
    };
    check("U11", "config-render", Object.values(bits).every(Boolean),
      `既有值 / 继承提示 / 密钥掩码 / 密码框 / 开关勾选: ${JSON.stringify(bits)}`);

    // ---- U12：保存只提交改动行 ----
    const modelIdx = ctrls.findIndex((c) => c.value === "old-model");
    ctrls[modelIdx].value = "new-model";
    el("config-view").listeners.input[0]();
    check("U12", "config-dirty",
      ConfigUI.isDirty() === true && el("config-btn-save").disabled === false,
      "改动后未标脏 / 保存按钮未启用");
    calls.length = 0;
    await ConfigUI.save();
    const saveArgs = calls.find((c) => c.cmd === "config_form_save")?.args;
    check("U12", "config-save",
      !!saveArgs && saveArgs.entries.length === 1 &&
        saveArgs.entries[0].key === "model" && saveArgs.entries[0].value === "new-model",
      `保存应只提交 1 行改动，实际: ${JSON.stringify(saveArgs?.entries)}`);
    check("U12", "config-save",
      calls.some((c) => c.cmd === "config_form_load") &&
        has(el("config-body").innerHTML, "new-model") && ConfigUI.isDirty() === false,
      "保存后未重新扫描 / 未刷新出新值 / 脏标记未清");

    // ---- U12：应用提交整表（把值刷进运行时对象） ----
    calls.length = 0;
    await ConfigUI.apply();
    const applyArgs = calls.find((c) => c.cmd === "config_form_apply")?.args;
    check("U12", "config-apply",
      !!applyArgs && applyArgs.entries.length >= 17 &&
        applyArgs.entries.some((e) => e.key === "flag" && e.value === "true"),
      `应用应提交整表（含未改动行），实际 ${applyArgs?.entries.length} 行`);

    // ---- U13：取消丢弃改动 ----
    const ctrls2 = el("config-body")._controls;
    const idx2 = ctrls2.findIndex((c) => c.value === "new-model");
    ctrls2[idx2].value = "oops";
    el("config-view").listeners.input[0]();
    calls.length = 0;
    await ConfigUI.cancel();
    check("U13", "config-cancel",
      calls.length > 0 && calls.every((c) => c.cmd === "config_form_load") &&
        !has(el("config-body").innerHTML, "oops"),
      `取消应只重扫、不写盘，实际: ${JSON.stringify(calls.map((c) => c.cmd))}`);
    check("U13", "config-cancel", ConfigUI.isDirty() === false, "取消后应回到干净状态");

    // ---- 未打开项目的项目作用域 ----
    calls.length = 0;
    await ConfigUI.handleCommand("form project");
    check("U13", "config-no-project", calls.length === 0,
      "无项目时不应发起 project 作用域的扫描");

    // ---- 回归：已打开项目时项目作用域必须能扫 ----
    // （曾因 main.js 没导出 window.state → root() 恒为 null → 已打开项目却提示"未打开项目"）
    appState.currentProject = { name: "ai-gateway", path: "D:/Projects/Go/ai-gateway" };
    calls.length = 0;
    await ConfigUI.handleCommand("form project");
    const pCall = calls.find((c) => c.cmd === "config_form_load");
    check("U13", "config-project-open",
      !!pCall && pCall.args.scope === "project" &&
        pCall.args.projectRoot === "D:/Projects/Go/ai-gateway",
      `已打开项目时应按 project 作用域扫描（含 projectRoot），实际: ${JSON.stringify(calls)}`);
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
}

/**
 * U22 help-replay：帮助页回放 —— 打开帮助必须真的拿到 md 源文件、真的过渲染器、
 * 真的把 HTML 填进容器。静态契约只能证明"代码里写了"，这里跑一遍真实链路。
 *
 * 回放方式：从 main.js 里切出帮助页整段源码（markdownToHtml → openHelpTab），
 * 配最小 DOM stub + 真 markdown-it + 假 fetch（喂 ui/help-*.md 的真实内容）。
 */
async function runHelpChecks() {
  const mainJs = read("ui/main.js");
  const start = mainJs.indexOf("/** markdown-it 渲染器懒构造");
  const endMark = mainJs.indexOf("// 编辑器 textarea 同步");
  const end = endMark > 0 ? mainJs.lastIndexOf("// ====", endMark) : -1;
  if (start < 0 || end <= start) {
    check("U22", "help-replay", false,
      "main.js 中定位不到帮助页代码段（区间标记变了，请同步本回放）");
    return;
  }
  const helpSrc = mainJs.slice(start, end);

  // 真渲染器：vendor 的 markdown-it.min.js（UMD，需浏览器式全局 window）
  const mdCtx = {};
  mdCtx.window = mdCtx;
  mdCtx.self = mdCtx;
  require("vm").createContext(mdCtx);
  require("vm").runInContext(read("ui/markdown-it.min.js"), mdCtx);
  const markdownit = mdCtx.window.markdownit;

  const elements = new Map();
  const el = (id) => {
    if (!elements.has(id)) elements.set(id, makeEl(id));
    return elements.get(id);
  };
  let lang = "zh-CN";
  const appState = { tabs: [], activeTabId: null, currentProject: null };
  const closed = [];
  const fetched = [];
  const sandbox = {
    I18N: {
      getLang: () => lang,
      t: (k) => (k === "help.title" ? (lang === "en" ? "Help" : "帮助") : k),
    },
    window: {
      location: { origin: "https://ruyix.localhost" },
      markdownit,
    },
    document: { getElementById: el },
    state: appState,
    escapeHtml: (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;"),
    renderTabs() {},
    switchTab(id) {
      appState.activeTabId = id;
    },
    closeTab(id) {
      closed.push(id);
    },
    showProjectWorkspace() {},
    showWelcomePage() {},
    fetch: async (url) => {
      fetched.push(url);
      const file = String(url).split("/").pop();
      const text = fs.existsSync(path.join(ROOT, "ui", file))
        ? read(path.join("ui", file))
        : "";
      return { ok: text.length > 0, text: async () => text };
    },
  };

  const saved = ["window", "document", "state", "fetch", "I18N"].map(
    (k) => [k, globalThis[k]]
  );
  Object.assign(globalThis, sandbox);
  try {
    // eslint-disable-next-line no-new-func
    const help = new Function(`${helpSrc}
      return { loadHelpDoc, showHelpPage, hideHelpPage, openHelp, openHelpTab };`)();

    // ---- 无项目：菜单/命令 → 帮助页（markdown 渲染进 #help-body）----
    // openHelp 是同步派发（内部 showHelpPage 才 await 加载），回放要等一轮宏任务
    const tick = () => new Promise((r) => setImmediate(r));
    help.openHelp();
    await tick();
    await tick();
    let body = el("help-body").innerHTML;
    check("U22", "help-replay",
      fetched.length === 1 && /help-zh\.md$/.test(fetched[0]),
      `打开帮助应按当前语言取 md 源文件，实际请求: ${JSON.stringify(fetched)}`);
    check("U22", "help-replay",
      body.includes("<h1>帮助</h1>") && body.includes("<table>") &&
        body.includes("<code>open project") && !body.includes("&lt;h1&gt;"),
      `帮助正文不是 markdown 渲染结果（无 h1/table/code，或整段被转义）: ${body.slice(0, 80)}`);
    check("U22", "help-replay",
      el("help-page").style.display === "" && el("editor-body").style.display === "none",
      "帮助页未切换到前台（help-page 未显示 / editor-body 未让位）");

    // ---- 切英文：同一入口必须换源文件并重新渲染 ----
    lang = "en";
    await help.showHelpPage();
    body = el("help-body").innerHTML;
    check("U22", "help-replay",
      fetched.length === 2 && /help-en\.md$/.test(fetched[1]) && body.includes("<h1>Help</h1>"),
      `切语言后未重新加载/渲染英文帮助: ${JSON.stringify(fetched)}`);
    lang = "zh-CN";
    await help.showHelpPage();

    // ---- 已打开项目：帮助进标签页，且不再塞纯文本给只读编辑器 ----
    appState.currentProject = { name: "demo", path: "D:/Projects/Rust/ruyix" };
    await help.openHelp();
    const tab = appState.tabs.find((t) => t._isHelp);
    check("U22", "help-replay",
      !!tab && tab.content === "" && appState.activeTabId === tab.id,
      "帮助标签页未创建/未激活，或仍在往 content 里塞帮助文本（应为 markdown 视图）");

    // ---- 返回：关闭帮助标签页（而不是把文档留在编辑区后面）----
    help.hideHelpPage();
    check("U22", "help-replay",
      closed.includes(tab?.id) && el("help-page").style.display === "none",
      "帮助页返回按钮未关闭帮助标签页");
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
}

/**
 * U23 service-replay：服务面板回放 —— 真的加载 ui/service.js，喂两条托管进程，
 * 断言三列表格、时间格式（到秒 + h/m/s 时长）、按 pid 停止、以及"离开面板就停刷新"。
 */
async function runServiceChecks() {
  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const elements = new Map();
  const el = (id) => {
    if (!elements.has(id)) elements.set(id, makeEl(id));
    return elements.get(id);
  };
  // service.js 用 querySelectorAll("[data-i18n]") 刷新表头/按钮文案；
  // 按 innerHTML 解析出节点对象（与 config 回放同一个套路，对象要能真的被写回）
  const bodyEl = el("service-body");
  bodyEl.querySelectorAll = (sel) => {
    if (sel !== "[data-i18n]") return [];
    const out = [];
    const re = /data-i18n="([^"]+)"/g;
    let m;
    while ((m = re.exec(bodyEl.innerHTML))) {
      out.push({ dataset: { i18n: m[1] }, textContent: "" });
    }
    bodyEl._i18n = out;
    return out;
  };

  const appState = { tabs: [], activeTabId: null, currentProject: null };
  const calls = [];
  const timers = { started: 0, cleared: 0, ms: 0 };
  // 8616000ms = 2h23m36s —— 与"已启动多久"的显示格式对着写，别让格式悄悄变了
  const procs = [
    {
      handle: "p1", pid: 38420, cmd: "mvn spring-boot:run", log: "p1.log",
      ready_cmd: null, state: "ready", keep_alive: false,
      elapsed_ms: 8616000, started_at_ms: Date.parse("2026-09-20T19:07:00"),
    },
    {
      handle: "p2", pid: 38421, cmd: "npm run dev", log: "p2.log",
      ready_cmd: null, state: "running", keep_alive: false,
      elapsed_ms: 5000, started_at_ms: Date.parse("2026-09-20T21:22:00"),
    },
    // 已退出的不该进面板：它没有"管理"可言
    {
      handle: "p3", pid: 39999, cmd: "java -jar gone.jar", log: "p3.log",
      ready_cmd: null, state: "exited(7)", keep_alive: false,
      elapsed_ms: 1000, started_at_ms: Date.parse("2026-09-20T21:20:00"),
    },
  ];

  // 真实契约：i18n.js 挂的是 window.I18N，service.js 的 T() 认这个 ——
  // stub 不给 window.I18N 就等于让回放跑在一个"多语言没加载"的世界里
  const i18n = { getLang: () => "zh-CN", t: (k) => zh[k] ?? k };
  const sandbox = {
    I18N: i18n,
    window: { state: appState, ServiceUI: null, I18N: i18n },
    document: { getElementById: el, querySelector: () => null },
    state: appState,
    renderTabs() {},
    switchTab(id) {
      appState.activeTabId = id;
    },
    setStatus() {},
    getTauriInvoke: () => async (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "proc_list") return procs;
      if (cmd === "proc_stop") {
        const i = procs.findIndex((p) => p.pid === args.pid);
        if (i >= 0) {
          procs[i] = { ...procs[i], state: "stopped" };
          return procs[i];
        }
        throw new Error("no such pid");
      }
      return null;
    },
    setInterval(fn, ms) {
      timers.started += 1;
      timers.ms = ms;
      return 1;
    },
    clearInterval() {
      timers.cleared += 1;
    },
  };

  // getTauriInvoke 必须一并还原：它是**全局约定**（`getInvoke()` 优先取它），
  // 漏还原就等于给后面所有场景静默换掉 invoke 实现 —— 实测 U41 因此拿到别人桩的 null
  const saved = ["window", "document", "state", "setInterval", "clearInterval", "I18N",
    "getTauriInvoke"]
    .map((k) => [k, globalThis[k]]);
  Object.assign(globalThis, sandbox);
  try {
    // eslint-disable-next-line no-new-func
    new Function(read("ui/service.js"))();
    const ServiceUI = sandbox.window.ServiceUI;
    check("U23", "service-replay", !!ServiceUI && typeof ServiceUI.open === "function",
      "service.js 未暴露 ServiceUI.open");

    ServiceUI.open();
    const tab = appState.tabs.find((t) => t._isService);
    check("U23", "service-replay",
      !!tab && appState.activeTabId === tab.id && tab.content === "",
      "菜单点「服务」未打开/激活服务标签页（或往 content 里塞了东西）");

    await ServiceUI.pull();
    const html = bodyEl.innerHTML;
    check("U23", "service-replay",
      html.includes('<table class="service-table">') &&
        html.includes("38420") && html.includes("mvn spring-boot:run") &&
        html.includes("npm run dev") && html.includes('data-stop="38420"'),
      `服务表没把 pid / 完整命令行 / 停止按钮渲染出来: ${html.slice(0, 120)}`);
    check("U23", "service-replay",
      /2026-09-20 19:07:00/.test(html) && html.includes("(2h23m36s)"),
      "启动时间要显示 年月日 时分秒，并在括号里带 h/m/s 的已启动时长");
    check("U23", "service-replay",
      !html.includes("39999") && !html.includes("gone.jar"),
      "已退出的进程不该进面板（没有可管理的对象）");
    const heads = bodyEl._i18n || [];
    check("U23", "service-replay",
      heads.some((n) => n.dataset.i18n === "service.th_started" && n.textContent === "启动时间") &&
        heads.some((n) => n.dataset.i18n === "service.th_pid" && n.textContent === "PID"),
      `表头文案没过 i18n: ${JSON.stringify(heads.map((n) => [n.dataset.i18n, n.textContent]))}`);

    // 刷新是秒级的（时长要"在走"），且离开面板必须停
    ServiceUI.render();
    check("U23", "service-replay", timers.started === 1 && timers.ms === 1000,
      `服务面板应当起一个 1 秒的刷新定时器，实际: ${JSON.stringify(timers)}`);

    // 按 pid 停 —— 面板手里的一手证据就是 pid，不该先翻译成引擎自造的 handle
    await ServiceUI.stop(38420);
    const stopCall = calls.find((c) => c.cmd === "proc_stop");
    check("U23", "service-replay",
      !!stopCall && stopCall.args.pid === 38420,
      `停止必须按 pid 发（实际: ${JSON.stringify(calls.filter((c) => c.cmd === "proc_stop"))}）`);
    await ServiceUI.pull();
    check("U23", "service-replay", !bodyEl.innerHTML.includes("38420"),
      "停掉之后这一行应当从面板上消失");

    ServiceUI.close();
    check("U23", "service-replay", timers.cleared >= 1,
      "离开/关闭服务面板必须停掉秒级刷新（否则后台一直问后端）");
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
}

/**
 * U31 editor-virtual-render：编辑器虚拟化 —— 真加载 ui/main.js，喂一个 5000 行的 tab，
 * 断言「只画了可视窗口」并且「几何不变量没破」。
 *
 * 为什么断言这三样：
 *   · 窗口行数 —— 这是这次改动**唯一的目的**（原来 N 行就建 N 行 DOM，5000 行 1.9 万节点，
 *     重建一次 200ms+，而它挂在自动保存上，打字停顿 1 秒跑一遍）；
 *   · 占位高度守恒 —— topPad + 窗口行 + botPad 必须恒等于「总行数 × 行高」。破了就是
 *     滚动条长度/位置漂移，用户一眼能看出来；
 *   · textarea 拿到显式高度 —— backdrop 移出流之后没人给它撑高（原来靠 grid 行高被拉伸），
 *     不设就只剩 2 行高，点击可视区下半部分点不到它。
 * 另外两条样式契约（backdrop 绝对定位、textarea 不软换行）在 runStaticChecks 里钉住，
 * 因为它们**退化时不会报错**，只会悄悄把重绘成本抬回 110ms / 让光标与高亮错位。
 */
async function runEditorChecks() {
  const N = 5000; // 卡死就是从这量级开始的
  const big = [];
  for (let i = 0; i < N; i++) big.push("    let value_" + i + " = compute(" + i + ") * 2;");
  const bigText = big.join("\n");

  const elements = new Map();
  const el = (id) => {
    if (!elements.has(id)) elements.set(id, makeEl(id));
    return elements.get(id);
  };

  const sandbox = {
    window: {},
    document: {
      getElementById: el,
      createElement: (tag) => makeEl("<" + tag + ">"),
      querySelector: () => null,
      addEventListener() {},
      // main.js 尾部按 readyState 决定「立刻 initApp」还是「等 DOMContentLoaded」。
      // 报 loading 就会只挂监听不执行 —— 我们只要那批函数声明，不要跑整个应用初始化。
      readyState: "loading",
    },
    setTimeout,
    clearTimeout,
    // 故意不给 getComputedStyle / requestAnimationFrame / ResizeObserver：
    // main.js 必须能在"量不到任何尺寸"的环境里退化运行（真机上窗口被隐藏时也是这个分支）
    I18N: { t: (k) => k, init: async () => {}, getLang: () => "zh-CN" },
  };
  const saved = ["window", "document", "setTimeout", "clearTimeout", "I18N", "state", "getComputedStyle"]
    .map((k) => [k, globalThis[k]]);
  Object.assign(globalThis, sandbox);
  delete globalThis.getComputedStyle;
  try {
    // 往源码尾部追加导出：main.js 的函数在 new Function 作用域里，外面拿不到
    const src =
      read("ui/main.js") +
      "\nwindow.__ed = { renderPlainCode: renderPlainCode, renderHighlightedCode: renderHighlightedCode," +
      " setupEditorVirtualScroll: setupEditorVirtualScroll, model: () => editorModel };\n";
    // eslint-disable-next-line no-new-func
    new Function(src)();

    const api = sandbox.window.__ed;
    check("U31", "editor-virtual-render",
      !!api && typeof api.renderPlainCode === "function" && typeof api.setupEditorVirtualScroll === "function",
      "main.js 没暴露编辑器渲染入口（renderPlainCode / setupEditorVirtualScroll 改名了？）");
    if (!api) return;

    const view = el("editor-view");
    const gutter = el("editor-gutter");
    const backdrop = el("editor-code-backdrop");
    const ta = el("editor-textarea");
    const rowsIn = (html) => (html.match(/class="gutter-line">/g) || []).length;
    const padsIn = (html) =>
      [...html.matchAll(/class="editor-virt-pad" style="height:(\d+)px"/g)].map((m) => +m[1]);

    // ---- 大文件：只画窗口 ----
    api.renderPlainCode({ id: "t1", name: "big.rs", content: bigText });
    const rows = rowsIn(gutter.innerHTML);
    const pad = padsIn(backdrop.innerHTML);
    check("U31", "editor-virtual-render",
      rows > 0 && rows < 200,
      `5000 行的文件应该只渲染几十行，实际 ${rows} 行（虚拟化没生效）`);
    check("U31", "editor-virtual-render",
      pad.length === 2 && pad[0] + rows * 20 + pad[1] === N * 20,
      `占位高度不守恒（滚动高度会漂）: top=${pad[0] ?? "无"} 窗口=${rows}行 bot=${pad[1] ?? "无"}，` +
        `应等于 ${N * 20}`);
    check("U31", "editor-virtual-render",
      ta.style.height === N * 20 + 16 + "px",
      `textarea 没拿到显式全文高度 —— backdrop 移出流后没人撑高，编辑器会塌成 2 行、下半区点不动: ${ta.style.height}`);
    check("U31", "editor-virtual-render",
      ta.value === bigText,
      "textarea 里必须仍然是**全文**（它才是编辑的真身，只有 backdrop/gutter 是虚拟的）");
    check("U31", "editor-virtual-render",
      backdrop.innerHTML.includes("editor-virt-keeper"),
      "宽度占位行没进窗口：宽行不在窗口里时水平滚动条会缩掉、scrollLeft 被钳住（实测缩 55~1370px）");

    // ---- 滚动：窗口跟着走，且几何不变量仍成立 ----
    api.setupEditorVirtualScroll();
    const scrolls = view.listeners.scroll || [];
    view.scrollTop = Math.floor(N / 2) * 20;
    if (scrolls.length) scrolls[scrolls.length - 1]();
    await new Promise((r) => setTimeout(r, 40)); // rAF 在 Node 里退化成 setTimeout
    const rows2 = rowsIn(gutter.innerHTML);
    const pad2 = padsIn(backdrop.innerHTML);
    const firstNo = +((gutter.innerHTML.match(/class="gutter-line">(\d+)</) || [])[1] || 0);
    check("U31", "editor-virtual-render",
      scrolls.length > 0,
      "没给 #editor-view 挂 scroll 监听：滚下去窗口不会跟着换，下面全是空白");
    check("U31", "editor-virtual-render",
      firstNo > 1 && rows2 === rows && pad2.length === 2 && pad2[0] + rows2 * 20 + pad2[1] === N * 20,
      `滚动后窗口没换或占位不守恒: 首行号=${firstNo} 行数=${rows2} top=${pad2[0]} bot=${pad2[1]}`);

    // ---- 小文件：不该白算宽度占位 ----
    api.renderPlainCode({ id: "t2", name: "small.rs", content: "a\nbb\nccc" });
    check("U31", "editor-virtual-render",
      !backdrop.innerHTML.includes("editor-virt-keeper") && rowsIn(gutter.innerHTML) === 3 &&
        ta.style.height === 3 * 20 + 16 + "px",
      "3 行的小文件不该留宽度占位（会白算一遍全文列宽），高度仍要对");

    // ---- 高亮路径：紧凑载荷（名表 + 扁平三元组）要能切成 tok-* ----
    api.renderHighlightedCode({
      id: "t3",
      name: "x.rs",
      content: "let a = 1;",
      _highlighted: { tags: ["keyword", "number"], lines: [[0, 3, 0, 8, 9, 1]] },
    });
    check("U31", "editor-virtual-render",
      backdrop.innerHTML.includes('class="tok-keyword">let</span>') &&
        backdrop.innerHTML.includes('class="tok-number">1</span>'),
      "高亮片段没按「名表 + 扁平三元组」切成 tok-* span（渲染入口改道时把高亮丢了）");

    // ---- 含非 ASCII 的行：片段偏移是 **UTF-16 码元**（后端已换算），不是字节 ----
    // `let s = "中文"; // 注释` 的码元下标：`"中文"` = 8..12、`// 注释` = 14..19。
    // 同一组数字若按「字节」报，字符串就是 8..16、注释是 18..27 → 切出来的东西整体跑偏
    // （着色起点前移、顺带把旁边字染上）。这条钉住"后端换算 + 前端直接 slice"这对单位约定。
    const cjk = 'let s = "中文"; // 注释';
    api.renderHighlightedCode({
      id: "t5",
      name: "c.rs",
      content: cjk,
      _highlighted: {
        tags: ["keyword", "string", "comment"],
        lines: [[0, 3, 0, 8, 12, 1, 14, 19, 2]],
      },
    });
    check("U31", "editor-virtual-render",
      backdrop.innerHTML.includes('class="tok-string">"中文"</span>') &&
        backdrop.innerHTML.includes('class="tok-comment">// 注释</span>'),
      "含非 ASCII 的行按 UTF-16 码元切不准（偏移被当成字节了？）。实际渲染：" +
        backdrop.innerHTML);

    // ---- 末尾换行：textarea 的值必须与 tab.content 逐字节相同 ----
    // 后端用 code.lines() 收行、**吃掉末尾空行**（这里只回 1 行），前端必须自己按
    // tab.content 切出 2 行。用后端的行去拼 textarea 的值 = 少一个末尾 \n，
    // 用户一按键 `tab.content = textarea.value` 就把差值固化，写回时真丢。
    const withNl = "fn a() {}\n";
    api.renderHighlightedCode({
      id: "t4",
      name: "n.rs",
      content: withNl,
      _highlighted: { tags: ["keyword"], lines: [[0, 2, 0]] },
    });
    check("U31", "editor-virtual-render",
      ta.value === withNl,
      "末尾换行被吃掉：textarea 的值 " +
        JSON.stringify(ta.value) +
        " != tab.content " +
        JSON.stringify(withNl) +
        "（用户一按键就把它固化进内容，写回时文件末尾的换行真没了）");
    check("U31", "editor-virtual-render",
      ta.style.height === 2 * 20 + 16 + "px",
      "后端少回的那一行没补上：textarea 的值/高度与 tab.content 对不上（滚动高度会短一行）");
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
}

/**
 * U32 editor-layout-real：编辑器**真实布局**——交给 scripts/editor-layout.js 在无头
 * Edge/Chrome 里量（Node 的 DOM 桩没有布局引擎，滚动条 / textarea 内部滚动这类问题
 * 在桩里根本不存在，所以只能交给真浏览器）。
 *
 * 那个脚本找不到浏览器时会打印 SKIP 并以 0 退出；本项据此记一条"跳过"并**显式说出来**
 * —— "没跑"与"通过"必须能分辨。
 */
function runEditorLayoutProbe() {
  const script = path.join(ROOT, "scripts", "editor-layout.js");
  const r = spawnSync(process.execPath, [script], { encoding: "utf8", timeout: 240000 });
  const out = ((r.stdout || "") + "\n" + (r.stderr || "")).trim();
  if (/^SKIP:/m.test(out)) {
    console.log("  · U32 跳过：" + (out.split("\n")[0] || "").replace(/^SKIP:\s*/, ""));
    return;
  }
  const brief = out
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => /^(FAIL|editor-layout|滚动条)/.test(l))
    .join(" ⏐ ");
  check("U32", "editor-layout-real",
    r.status === 0,
    "真浏览器布局探针未通过（退出码 " + r.status + "）：" + (brief || out.slice(0, 500)));
}

/**
 * U42 session-trace-layout-real：轨迹的**真实几何**（真浏览器）。
 *
 * U41 证明得了"代码里写了 nowrap/overflow/text-overflow"，证明不了"看起来是一行 + 省略号"
 * —— 那是布局引擎的事。所以把真 index.html + styles.css + session.js 丢进无头 Edge，
 * 跑起来再逐元素量（折行 / 溢出 / 横向滚动条 / 图标是否被挤到另一行）。
 * 本机没 Edge/Chrome 时该脚本自行 SKIP。
 */
function runSessionTraceLayoutProbe() {
  const script = path.join(ROOT, "scripts", "session-trace-layout.js");
  const r = spawnSync(process.execPath, [script], { encoding: "utf8", timeout: 240000 });
  const out = ((r.stdout || "") + "\n" + (r.stderr || "")).trim();
  if (/^SKIP:/m.test(out)) {
    console.log("  · U42 跳过：" + (out.split("\n")[0] || "").replace(/^SKIP:\s*/, ""));
    return;
  }
  const brief = out
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => /^(FAIL|session-trace-layout|轨迹行)/.test(l))
    .join(" ⏐ ");
  check("U42", "session-trace-layout-real",
    r.status === 0,
    "轨迹几何探针未通过（退出码 " + r.status + "）：" + (brief || out.slice(0, 500)));
}

/**
 * U45 tab-context-menu：编辑器多标签的右键菜单（v0.11）。
 *
 * 病根：文件树早就把浏览器默认右键拦掉了（setupContextMenu），**标签栏没拦** —— 于是编辑器多标签
 * 上右键出来的是 WebView 的**系统菜单**（后退/刷新/检查元素…），与 IDE 的标签操作无关。
 *
 * 三条判据，都拿真源码回放（切片 `closePlan` + `setupTabContextMenu` 到沙箱里跑，不另抄一份）：
 *   ① `contextmenu` 必须 `preventDefault`（不拦就还是系统菜单）+ 显示自家菜单；
 *   ② **右键不改激活标签**：目标由命中的那个 `data-tab-id` 决定 —— 否则"关闭其他"会把用户
 *      刚右键的那个也关掉（最容易被忽略的边界）；
 *   ③ 关闭策略逐条对：close / others / right / left / all，以及"只有一个标签"时
 *      others/right/left 得到**空计划**（菜单项应当被隐藏，而不是点了没反应）。
 */
async function runTabContextMenuChecks() {
  const mainSrc = read("ui/main.js");
  const start = mainSrc.indexOf("function closePlan(ids, targetId, mode) {");
  const end = mainSrc.indexOf("\nfunction setupContextMenu(", start);
  check("U45", "tab-context-menu", start >= 0 && end > start,
    "main.js 里定位不到 closePlan…setupTabContextMenu 这段源码（切片锚点失效）");
  if (start < 0 || end <= start) return;
  const funcs = mainSrc.slice(start, end);

  // 菜单 DOM 契约：五个关闭动作 + 两个复制动作都在；文案键两语言都有
  const html = read("ui/index.html");
  const menuHtml = (html.match(/<div id="tab-context-menu"[\s\S]*?\n  <\/div>/) || [""])[0];
  const acts = [...menuHtml.matchAll(/data-tab-action="([^"]+)"/g)].map((m) => m[1]);
  for (const a of ["close", "others", "right", "left", "all", "copy-path", "copy-full-path"]) {
    check("U45", "tab-context-menu", acts.includes(a), `标签右键菜单缺少 data-tab-action="${a}"`);
  }
  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const en = JSON.parse(read("ui/lang/en.json"));
  for (const k of ["ctx.tab_close", "ctx.tab_close_others", "ctx.tab_close_right", "ctx.tab_close_left", "ctx.tab_close_all"]) {
    check("U45", "tab-context-menu", !!zh[k] && !!en[k], `缺文案键 ${k}（zh 或 en）`);
  }

  // ---- 回放 ----
  const ROWS = ["close", "others", "right", "left", "all", "copy-path", "copy-full-path"].map((a) => ({
    dataset: { tabAction: a },
    style: {},
  }));
  const captured = { bar: null, menu: null, outside: null };
  const menuEl = {
    style: {},
    querySelectorAll: () => ROWS,
  };
  const barEl = {};
  const docStub = {
    getElementById: (id) => (id === "tab-context-menu" ? menuEl : id === "tab-bar" ? barEl : null),
    addEventListener: (evt, fn) => {
      captured.outside = fn;
    },
  };
  const state = {
    tabs: [
      { id: "t1", name: "a.rs", path: "D:\\p\\a.rs" },
      { id: "t2", name: "b.rs", path: "D:\\p\\b.rs" },
      { id: "t3", name: "c.rs", path: "D:\\p\\c.rs" },
    ],
    activeTabId: "t1",
  };
  const closed = [];
  const copied = [];
  let hid = 0;
  const mod = new Function(
    "document",
    "state",
    "closeTab",
    "hideContextMenu",
    "handleContextCopyPath",
    `${funcs}\nreturn { closePlan, setupTabContextMenu };`
  )(
    docStub,
    state,
    (id) => closed.push(id),
    () => {
      hid++;
      menuEl.style.display = "none";
    },
    async (p, abs) => copied.push([p, abs])
  );

  // closePlan 的边界（纯函数，逐条）
  const ids = ["t1", "t2", "t3"];
  const expectPlan = (mode, want, msg) => {
    const got = JSON.stringify(mod.closePlan(ids, "t2", mode));
    check("U45", "tab-context-menu", got === JSON.stringify(want), `${msg}：期望 ${JSON.stringify(want)}，得到 ${got}`);
  };
  expectPlan("close", ["t2"], "close 应只关目标");
  expectPlan("others", ["t1", "t3"], "others 应关掉除目标外的全部");
  expectPlan("right", ["t3"], "right 应只关目标右侧");
  expectPlan("left", ["t1"], "left 应只关目标左侧");
  expectPlan("all", ["t1", "t2", "t3"], "all 应关全部");
  expectPlan("nope", [], "未知动作给空计划");
  check("U45", "tab-context-menu",
    mod.closePlan(["t1"], "t1", "others").length === 0 &&
      mod.closePlan(["t1"], "t1", "right").length === 0 &&
      mod.closePlan(["t1"], "t1", "left").length === 0 &&
      mod.closePlan(ids, "nope-id", "close").length === 0,
    "只有一个标签（或目标不在列表里）时，others/right/left 必须是空计划（菜单项据此隐藏）");

  // 装上菜单，模拟右键第 2 个标签
  const barHandlers = {};
  barEl.addEventListener = (evt, fn) => {
    barHandlers[evt] = fn;
  };
  const menuHandlers = {};
  menuEl.addEventListener = (evt, fn) => {
    menuHandlers[evt] = fn;
  };
  mod.setupTabContextMenu();
  check("U45", "tab-context-menu", !!barHandlers.contextmenu && !!menuHandlers.click,
    "setupTabContextMenu 没把 contextmenu / click 挂上");

  let prevented = 0;
  barHandlers.contextmenu({
    preventDefault: () => prevented++,
    stopPropagation: () => {},
    clientX: 120,
    clientY: 40,
    target: { closest: () => ({ dataset: { tabId: "t2" } }) },
  });
  check("U45", "tab-context-menu", prevented === 1,
    "contextmenu 没有 preventDefault —— 那就还是 WebView 的系统菜单（本单的原始问题）");
  check("U45", "tab-context-menu", menuEl.style.display !== "none" && menuEl.style.left === "120px",
    `菜单没在该出现的时候出现（display=${menuEl.style.display} left=${menuEl.style.left}）`);
  check("U45", "tab-context-menu", state.activeTabId === "t1",
    `右键**不该**改激活标签（右键 t2 后 activeTabId=${state.activeTabId}）—— 否则"关闭其他"会先把刚右键的那个关掉`);

  // 三个标签时：五个关闭项都该可见
  const shown = ROWS.filter((r) => r.style.display !== "none").map((r) => r.dataset.tabAction);
  check("U45", "tab-context-menu",
    ["close", "others", "right", "left", "all", "copy-path"].every((a) => shown.includes(a)),
    `三个标签、目标在中间时关闭项应全可见，实际可见：${shown.join(",")}`);

  // 只有一个标签（伪标签：没有 path）→ others/right/left 隐藏、复制项也隐藏
  state.tabs = [{ id: "s1", name: "会话", path: "" }];
  barHandlers.contextmenu({
    preventDefault: () => {},
    stopPropagation: () => {},
    clientX: 0,
    clientY: 0,
    target: { closest: () => ({ dataset: { tabId: "s1" } }) },
  });
  const shownOne = ROWS.filter((r) => r.style.display !== "none").map((r) => r.dataset.tabAction);
  check("U45", "tab-context-menu",
    shownOne.includes("close") && shownOne.includes("all") &&
      !shownOne.includes("others") && !shownOne.includes("right") && !shownOne.includes("left"),
    `单标签时只该留 close/all，实际：${shownOne.join(",")}`);
  check("U45", "tab-context-menu",
    !shownOne.includes("copy-path") && !shownOne.includes("copy-full-path"),
    `没有路径的伪标签（会话/配置/服务）不该出现复制项，实际：${shownOne.join(",")}`);

  // 点"关闭其他"：关的是 t1/t3，**必须保住** t2
  state.tabs = [
    { id: "t1", name: "a.rs", path: "D:\\p\\a.rs" },
    { id: "t2", name: "b.rs", path: "D:\\p\\b.rs" },
    { id: "t3", name: "c.rs", path: "D:\\p\\c.rs" },
  ];
  barHandlers.contextmenu({
    preventDefault: () => {},
    stopPropagation: () => {},
    clientX: 0,
    clientY: 0,
    target: { closest: () => ({ dataset: { tabId: "t2" } }) },
  });
  await menuHandlers.click({
    stopPropagation: () => {},
    target: { closest: () => ROWS.find((r) => r.dataset.tabAction === "others") },
  });
  check("U45", "tab-context-menu", JSON.stringify(closed) === JSON.stringify(["t1", "t3"]),
    `"关闭其他"应关掉 [t1,t3] 而保住右键的 t2，实际关了 ${JSON.stringify(closed)}`);
  check("U45", "tab-context-menu", hid > 0, "点完菜单项要把它收起来");

  // 点复制项：走既有 handleContextCopyPath（相对/绝对各一）
  closed.length = 0;
  await menuHandlers.click({
    stopPropagation: () => {},
    target: { closest: () => ROWS.find((r) => r.dataset.tabAction === "copy-full-path") },
  });
  check("U45", "tab-context-menu",
    copied.length === 1 && copied[0][0] === "D:\\p\\b.rs" && copied[0][1] === true,
    `复制绝对路径没走 handleContextCopyPath，实际：${JSON.stringify(copied)}`);

  // 空白处右键：也拦掉默认菜单（但不弹菜单）
  menuEl.style.display = "";
  let prevented2 = 0;
  barHandlers.contextmenu({
    preventDefault: () => prevented2++,
    stopPropagation: () => {},
    target: { closest: () => null },
  });
  check("U45", "tab-context-menu", prevented2 === 1 && menuEl.style.display === "none",
    "标签栏空白处右键也该拦掉系统菜单，并且不弹菜单");

  // 静态契约：init 里真的调了
  check("U45", "tab-context-menu", /^\s*setupTabContextMenu\(\);/m.test(mainSrc),
    "initApp 里没调用 setupTabContextMenu —— 菜单永远装不上");
}

/**
 * U44 nav-layout-real：导航栏**真实几何**（真浏览器）。
 *
 * 方案（用户拍板）：**导航区不做横向滚动** —— 长名走省略号 `text-overflow: ellipsis`，
 * 完整路径由行上的 `title` 悬停给出。两版横滚都被否过：`min-width: max-content` 撑宽之后
 * hover/滚动条一出现树就抖；`position: sticky` 钉住的行内按钮会压在滚动中的长路径上。
 *
 * 探针在无头 Edge 里量：短/长内容都不出横条、长名确实走 ellipsis、行上有 title（= 完整路径）、
 * 目录行的刷新按钮右边缘对齐且都在可视区内、长目录行的按钮不压名字、纵向仍可滚。
 * 见 scripts/nav-layout.js。本机没 Edge/Chrome 时自行 SKIP。
 */
function runNavLayoutProbe() {
  const script = path.join(ROOT, "scripts", "nav-layout.js");
  const r = spawnSync(process.execPath, [script], { encoding: "utf8", timeout: 240000 });
  const out = ((r.stdout || "") + "\n" + (r.stderr || "")).trim();
  if (/^SKIP:/m.test(out)) {
    console.log("  · U44 跳过：" + (out.split("\n")[0] || "").replace(/^SKIP:\s*/, ""));
    return;
  }
  const brief = out
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => /^(FAIL|nav-layout|短内容|长内容)/.test(l))
    .join(" ⏐ ");
  check("U44", "nav-layout-real",
    r.status === 0,
    "导航栏几何探针未通过（退出码 " + r.status + "）：" + (brief || out.slice(0, 500)));
}

/**
 * U43 terminal-layout-real：模拟终端**真实几何**（真浏览器 + 真 xterm）。
 *
 * U32/U42 证明得了"编辑器/轨迹的几何"，证明不了终端那一块：`.xterm-screen` 的像素尺寸、
 * "容器缩小时终端跟不跟"、PTY 的 winsize 有没有跟着改 —— 全都只存在于布局引擎里。
 * 真机上出过一次事故（用户报）：终端永远停在创建时那 100×24（763×368px），窗口怎么变都不动，
 * 因为①没有 resize 路径②容器被 `min-width: fit-content` 撑住不让缩。
 * 探针在无头 Edge 里用**真的 xterm** 走一遍"宽屏 → 缩小 → 放大"，见 scripts/terminal-layout.js。
 * 本机没 Edge/Chrome 时该脚本自行 SKIP（"没跑"与"通过"必须能分辨）。
 */
function runTerminalLayoutProbe() {
  const script = path.join(ROOT, "scripts", "terminal-layout.js");
  const r = spawnSync(process.execPath, [script], { encoding: "utf8", timeout: 240000 });
  const out = ((r.stdout || "") + "\n" + (r.stderr || "")).trim();
  if (/^SKIP:/m.test(out)) {
    console.log("  · U43 跳过：" + (out.split("\n")[0] || "").replace(/^SKIP:\s*/, ""));
    return;
  }
  const brief = out
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => /^(FAIL|terminal-layout|宽屏|窄屏)/.test(l))
    .join(" ⏐ ");
  check("U43", "terminal-layout-real",
    r.status === 0,
    "终端几何探针未通过（退出码 " + r.status + "）：" + (brief || out.slice(0, 500)));
}

/**
 * U30 proc-log-replay：输出面板回放 —— 真加载 ui/service.js + ui/proc-log.js，配上假 xterm
 * 与假后端，走一遍"服务表点输出 → 增量跟随 → 进程退出收尾"。
 *
 * 断言的是**喂进终端的文字**与**带回去的 offset**，不是"有没有调用某个函数"：
 * 增量读这件事，错了就错在"重读 / 漏读 / 乱码"上，只有看文字才看得出来。
 */
async function runProcLogChecks() {
  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const i18n = {
    getLang: () => "zh-CN",
    t: (k, params) => {
      let s = zh[k] ?? k;
      for (const [pk, pv] of Object.entries(params || {})) s = s.split(`{${pk}}`).join(pv);
      return s;
    },
  };
  const flush = () => new Promise((r) => setImmediate(r));

  // 假 xterm：只记事（构造参数 + 写进去的文字）。真要断言的正是"喂了什么进去"。
  const termOpts = [];
  let lastTerm = null;
  class FakeTerm {
    constructor(opts) {
      termOpts.push(opts);
      this.opts = opts;
      this.cols = 80;
      this.rows = 24;
      this.disposed = false;
      this.buffer = {
        active: {
          length: 2,
          baseY: 1,
          viewportY: 1,
          getLine: (i) => ({
            translateToString: () =>
              i === 0 ? "Started AdminApplication in 1.168 seconds" : "Tomcat started on port 8083",
          }),
        },
      };
      lastTerm = this;
    }
    open() {}
    write(t) {
      this.written = (this.written || "") + t;
    }
    scrollToBottom() {}
    clear() {
      this.written = "";
    }
    focus() {}
    dispose() {
      this.disposed = true;
    }
    onScroll() {}
    resize(c, r) {
      this.cols = c;
      this.rows = r;
    }
  }

  const elements = new Map();
  const el = (id) => {
    if (!elements.has(id)) elements.set(id, makeEl(id));
    return elements.get(id);
  };
  const panesRoot = el("proc-log-panes");
  panesRoot.insertAdjacentHTML = (where, html) => {
    panesRoot.innerHTML += html;
  };

  const appState = { tabs: [], activeTabId: null, currentProject: null };
  const calls = [];
  const timers = { started: 0, cleared: 0 };
  const procs = [
    {
      handle: "p1", pid: 38420, cmd: "mvn spring-boot:run", log: "p1.log",
      ready_cmd: null, state: "ready", keep_alive: false,
      elapsed_ms: 1000, started_at_ms: Date.parse("2026-09-20T19:07:00"),
    },
  ];
  // 后端剧本（按调用次序发）：首读带 truncated_head → 空手 → more=true（一次没读完）→
  // 正常一段 → **退出那一刻还留着没读完的尾巴** → 尾段。最后两条专门守着一个真实的坑：
  // 服务退出前会把缓冲一次性冲出来，若"看到 exited 就收尾"，死因那几行恰好会被丢掉。
  const script = [
    { text: "Started AdminApplication in 1.168 seconds\n", start: 1024, next_offset: 1063,
      size: 1063, truncated_head: true, more: false, state: "ready" },
    { text: "", start: 1063, next_offset: 1063, size: 1063, truncated_head: false, more: false,
      state: "ready" },
    { text: "line-A\n", start: 1063, next_offset: 1070, size: 1200, truncated_head: false,
      more: true, state: "ready" },
    { text: "line-B\n", start: 1070, next_offset: 1077, size: 1077, truncated_head: false,
      more: false, state: "running" },
    { text: "boom\n", start: 1077, next_offset: 1082, size: 2000, truncated_head: false,
      more: true, state: "exited(7)" },
    { text: "Caused by: port 8083 already in use\n", start: 1082, next_offset: 1119,
      size: 1119, truncated_head: false, more: false, state: "exited(7)" },
  ];
  let seat = 0;
  let throwOnRead = false;

  const sandbox = {
    I18N: i18n,
    window: { I18N: i18n, Terminal: FakeTerm, state: appState, ServiceUI: null, ProcLogUI: null },
    document: {
      getElementById: el,
      createElement: (tag) => makeEl("<" + tag + ">"),
      querySelector: () => null,
    },
    state: appState,
    renderTabs() {},
    switchTab(id) {
      appState.activeTabId = id;
    },
    setStatus() {},
    getTauriInvoke: () => async (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "proc_list") return procs;
      if (cmd === "proc_log_read") {
        if (throwOnRead) throw new Error("没有 pid=999 这个托管进程");
        return script[Math.min(seat++, script.length - 1)];
      }
      return null;
    },
    setInterval() {
      timers.started += 1;
      return 1;
    },
    clearInterval() {
      timers.cleared += 1;
    },
  };

  // getTauriInvoke 必须一并还原：它是**全局约定**（`getInvoke()` 优先取它），
  // 漏还原就等于给后面所有场景静默换掉 invoke 实现 —— 实测 U41 因此拿到别人桩的 null
  const saved = ["window", "document", "state", "setInterval", "clearInterval", "I18N",
    "getTauriInvoke"]
    .map((k) => [k, globalThis[k]]);
  Object.assign(globalThis, sandbox);
  try {
    // eslint-disable-next-line no-new-func
    new Function(read("ui/service.js"))();
    // eslint-disable-next-line no-new-func
    new Function(read("ui/proc-log.js"))();
    const ProcLogUI = sandbox.window.ProcLogUI;
    const ServiceUI = sandbox.window.ServiceUI;
    check("U30", "proc-log-replay", !!ProcLogUI && typeof ProcLogUI.open === "function",
      "proc-log.js 未暴露 ProcLogUI.open");

    // 入口 = 服务面板的 `service log <pid>`（pid 是用户手里唯一的一手证据）
    const opened = await ServiceUI.openLog(38420);
    const tab = appState.tabs.find((t) => t._isProcLog);
    check("U30", "proc-log-replay",
      !!opened && !!tab && appState.activeTabId === tab.id && tab.content === "",
      `service log <pid> 没打开输出标签页（或往 content 里塞了东西）: ${JSON.stringify(tab)}`);

    ProcLogUI.render(tab);
    await flush();
    const reads = calls.filter((c) => c.cmd === "proc_log_read");
    check("U30", "proc-log-replay",
      reads.length >= 1 && reads[0].args.pid === 38420 && reads[0].args.offset === null,
      `首读必须不带 offset（后端据此只回看尾部 128KB）: ${JSON.stringify(reads[0] || null)}`);
    check("U30", "proc-log-replay",
      termOpts.length === 1 && termOpts[0].convertEol === true && termOpts[0].disableStdin === true,
      `xterm 必须以 convertEol 打开（否则 LF 会让输出斜成阶梯）、并且是只读的: ${JSON.stringify(termOpts[0] || null)}`);
    const w1 = (lastTerm && lastTerm.written) || "";
    check("U30", "proc-log-replay",
      w1.includes("Started AdminApplication in 1.168 seconds") &&
        w1.includes("已跳过前面 1024 字节"),
      `首屏没把后端给的那段写进终端、或没提示"前面被跳过了": ${JSON.stringify(w1)}`);
    check("U30", "proc-log-replay",
      tab._offset === 1063,
      `offset 没跟着后端的 next_offset 走（下一轮会重读整段）: ${tab._offset}`);

    // 续读：带着上一轮的 next_offset —— 增量，不重读
    await ProcLogUI.poll(tab);
    const reads2 = calls.filter((c) => c.cmd === "proc_log_read");
    check("U30", "proc-log-replay",
      reads2.length === 2 && reads2[1].args.offset === 1063,
      `续读要带上一轮的 next_offset（不是 null、也不是 0）: ${JSON.stringify(reads2.map((r) => r.args.offset))}`);
    check("U30", "proc-log-replay",
      !!lastTerm && lastTerm.written === w1,
      "后端返回空 text 时不该往终端里写任何东西");

    // more=true：一轮没读完就接着读，别等下一个 tick
    await ProcLogUI.poll(tab);
    const reads3 = calls.filter((c) => c.cmd === "proc_log_read");
    check("U30", "proc-log-replay",
      reads3.length === 4 && reads3[2].args.offset === 1063 && reads3[3].args.offset === 1070,
      `more=true 要**立刻**接着读（否则大数据量要等好几个 tick）: ${JSON.stringify(reads3.map((r) => r.args.offset))}`);
    check("U30", "proc-log-replay",
      !!lastTerm && lastTerm.written.includes("line-A\nline-B\n"),
      `两段续读的文字要按序落进终端: ${JSON.stringify(lastTerm && lastTerm.written)}`);

    // 进程退出：**先把没读完的读干净再收尾**（退出那一刻的尾巴往往正是死因），
    // 收尾之后不再问后端
    await ProcLogUI.poll(tab);
    const before = calls.length;
    const wEnd = (lastTerm && lastTerm.written) || "";
    check("U30", "proc-log-replay",
      timers.cleared >= 1 && /进程已退出/.test(wEnd),
      `进程退出后要停掉轮询并说明"不会再有新输出": ${JSON.stringify({ timers, w: wEnd })}`);
    check("U30", "proc-log-replay",
      wEnd.includes("boom\n") && wEnd.includes("Caused by: port 8083 already in use\n"),
      `退出那一刻还没读完的尾巴不许丢（那正是死因）: ${JSON.stringify(wEnd)}`);
    await ProcLogUI.poll(tab);
    check("U30", "proc-log-replay", calls.length === before,
      "进程已经结束了还在问后端（`_ended` 之后不该再发请求）");

    // 读不到 = 被停止（条目已从进程表移除）：这不是故障，是终态
    throwOnRead = true;
    const tab2 = ProcLogUI.open({ pid: 999, cmd: "java -jar gone.jar", state: "running" });
    ProcLogUI.render(tab2);
    await flush();
    check("U30", "proc-log-replay",
      /日志读取结束/.test((lastTerm && lastTerm.written) || ""),
      `读不到日志时要收尾并说明原因: ${JSON.stringify(lastTerm && lastTerm.written)}`);

    // 生命周期：切走停轮询、关标签页 dispose
    const startedBefore = timers.started;
    ProcLogUI.render(appState.tabs.find((t) => t._isProcLog));
    ProcLogUI.blur();
    check("U30", "proc-log-replay", timers.cleared >= 2,
      "切走到别的标签页必须停掉输出轮询（否则后台一直问后端）");
    const paneSel = `[data-pane="${tab2.id}"]`;
    const pane2 = panesRoot.querySelector(paneSel);
    let removed = 0;
    if (pane2) pane2.remove = () => { removed += 1; };
    ProcLogUI.close(tab2);
    check("U30", "proc-log-replay",
      !!lastTerm && lastTerm.disposed === true && removed === 1,
      "关标签页要 dispose 终端并摘掉面板（不然每开一次就漏一个 xterm 实例）");
    check("U30", "proc-log-replay", startedBefore >= 1,
      "render 没起轮询定时器（那就只剩首读，后面不会有新内容）");
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
}

/**
 * U24 external-link-replay：外链闸门回放 —— 真加载 ui/external.js，喂几条链接按下"点击"，
 * 断言：点下去既不导航 WebView（preventDefault）也不静默吃掉，而是走命令系统交给系统浏览器；
 * 白名单之外的 scheme 与自家文档里的非文档路径被拦下并给出说明；锚点与自家文档放行。
 */
async function runExternalLinkChecks() {
  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const i18n = {
    getLang: () => "zh-CN",
    t: (k, params) => {
      let s = zh[k] ?? k;
      for (const [pk, pv] of Object.entries(params || {})) s = s.split(`{${pk}}`).join(pv);
      return s;
    },
  };
  // 自家站点 = Windows 上 Tauri 自定义协议的落地形式
  const location = { origin: "http://tauri.localhost", href: "http://tauri.localhost/index.html" };
  const calls = [];
  const statuses = [];
  const listeners = [];
  const sandbox = {
    I18N: i18n,
    window: { I18N: i18n, location },
    document: { addEventListener: (type, fn, capture) => listeners.push({ type, fn, capture }) },
    handleCommand: (raw) => calls.push(raw),
    setStatus: (msg) => statuses.push(msg),
  };

  const saved = ["window", "document", "handleCommand", "setStatus", "I18N"].map((k) => [k, globalThis[k]]);
  Object.assign(globalThis, sandbox);
  try {
    // eslint-disable-next-line no-new-func
    new Function(read("ui/external.js"))();
    const EL = sandbox.window.ExternalLinks;
    check("U24", "external-link-replay", !!EL && typeof EL.install === "function",
      "external.js 未暴露 ExternalLinks.install");

    EL.install();
    EL.install(); // 幂等：装两次不该挂两遍监听
    check("U24", "external-link-replay",
      listeners.filter((l) => l.type === "click").length === 1 &&
        listeners.some((l) => l.type === "click" && l.capture === true) &&
        listeners.some((l) => l.type === "auxclick"),
      `拦截必须只挂在捕获阶段的 click 上一次（中键 auxclick 也要）: ${JSON.stringify(listeners.map((l) => [l.type, l.capture]))}`);
    const click = listeners.find((l) => l.type === "click").fn;

    // 按下一次点击；inner=true 表示点的是 <a> 里的子节点（真实聊天里的 <code> 就是这样）
    const clickOn = (href, inner) => {
      const a = { nodeType: 1, tagName: "A", getAttribute: () => href, parentNode: null };
      const target = inner ? { nodeType: 1, tagName: "SPAN", parentNode: a } : a;
      let prevented = false;
      click({ target, preventDefault: () => { prevented = true; } });
      return prevented;
    };

    // ① 用户踩到的那个 P0：agent 起了服务，回一句 http://localhost:8080
    let prevented = clickOn("http://localhost:8080/actuator/health", true);
    check("U24", "external-link-replay",
      prevented && calls.length === 1 && calls[0] === "open url http://localhost:8080/actuator/health",
      `localhost 链接必须拦下并交给系统浏览器（prevented=${prevented}, calls=${JSON.stringify(calls)}）`);

    // ② 外站同理（注意 URL 规范化会补上结尾的 /）
    prevented = clickOn("https://newest-ai.com");
    check("U24", "external-link-replay",
      prevented && calls[1] === "open url https://newest-ai.com/",
      `外站链接同样交给系统浏览器: ${JSON.stringify(calls)}`);

    // ③ 白名单之外：拦下 + 说明；**不**交给系统、更不执行
    calls.length = 0;
    statuses.length = 0;
    prevented = clickOn("javascript:alert(1)");
    check("U24", "external-link-replay",
      prevented && calls.length === 0 && statuses.length === 1 && statuses[0].includes("javascript"),
      `javascript: 必须拦下且不交给系统: prevented=${prevented} calls=${JSON.stringify(calls)} status=${JSON.stringify(statuses)}`);
    prevented = clickOn("file:///C:/Windows/System32/calc.exe");
    check("U24", "external-link-replay",
      prevented && calls.length === 0 && statuses.length === 2 && statuses[1].includes("file"),
      `file: 必须拦下: calls=${JSON.stringify(calls)} status=${JSON.stringify(statuses)}`);

    // ④ 同源但非文档（chat 里的相对链接 src/main.rs 解析出来就是它）：拦下 ——
    //    导航过去只是一张 404 白页，和跳去外站一样丢状态；而它也没有"交给浏览器"的去处
    calls.length = 0;
    statuses.length = 0;
    prevented = clickOn("src/main.rs");
    check("U24", "external-link-replay",
      prevented && calls.length === 0 && statuses.length === 1 && statuses[0].includes("内部路径"),
      `同源非文档必须拦下并说明: prevented=${prevented} calls=${JSON.stringify(calls)} status=${JSON.stringify(statuses)}`);

    // ⑤ 同文档锚点与自家文档：放行 —— 拦了就成了"点了没反应"
    calls.length = 0;
    statuses.length = 0;
    check("U24", "external-link-replay",
      clickOn("#sessions") === false && clickOn("index.html") === false &&
        calls.length === 0 && statuses.length === 0,
      `锚点 / 自家文档必须放行: ${JSON.stringify({ calls, statuses })}`);

    // ⑥ 没有 href 的不是链接（会话里的 run 徽章就是这个形状）
    check("U24", "external-link-replay", clickOn(null) === false,
      "没有 href 的元素不该被当成链接");
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
}

/**
 * U33 ctx-copy-path：文件树右键【复制路径 / 复制绝对路径】回放 —— 执行 main.js 里**真实**的
 * toRelativePath / copyToClipboard / handleContextCopyPath 源码（切片 + new Function），不另抄一份。
 */
async function runContextMenuChecks() {
  const mainSrc = read("ui/main.js");
  const start = mainSrc.indexOf("function toRelativePath(fullPath) {");
  const anchor = mainSrc.indexOf("async function handleContextCopyPath(");
  const end = anchor >= 0 ? mainSrc.indexOf("\n}\n", anchor) : -1;
  check("U33", "ctx-copy-path", start >= 0 && end > start,
    "main.js 里定位不到 toRelativePath…handleContextCopyPath 这段源码（切片锚点失效）");
  if (start < 0 || end <= start) return;

  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const statuses = [];
  const I18N = {
    t: (k, params) => {
      let s = zh[k] ?? k;
      for (const [pk, pv] of Object.entries(params || {})) s = s.split(`{${pk}}`).join(pv);
      return s;
    },
  };
  const state = { currentProject: { path: "D:\\proj" } };
  let clip = null;
  let failClip = false;
  let execText = null;
  let lastTa = null;
  const navigatorStub = {
    clipboard: {
      writeText: async (t) => {
        if (failClip) throw new Error("denied");
        clip = t;
      },
    },
  };
  const documentStub = {
    createElement: () => {
      lastTa = { style: {}, value: "", select() {}, remove() {} };
      return lastTa;
    },
    body: { appendChild() {} },
    execCommand: () => {
      execText = lastTa ? lastTa.value : null;
      return true;
    },
  };
  const setStatus = (msg, kind) => statuses.push({ msg, kind });
  const body = mainSrc.slice(start, end + 3);
  const factory = new Function("state", "I18N", "setStatus", "navigator", "document",
    `${body}\nreturn { toRelativePath, handleContextCopyPath };`);
  const mod = factory(state, I18N, setStatus, navigatorStub, documentStub);

  const rel = mod.toRelativePath("D:\\proj\\src\\main.rs");
  check("U33", "ctx-copy-path", rel === "src\\main.rs",
    `相对路径要相对项目根，实际 "${rel}"`);
  check("U33", "ctx-copy-path", mod.toRelativePath("D:\\proj") === "",
    "项目根自身的相对路径应是空串");
  state.currentProject.path = "D:/proj";
  check("U33", "ctx-copy-path", mod.toRelativePath("D:\\proj\\src\\main.rs") === "src\\main.rs",
    "项目根写成正斜杠时也要认（否则静默退化成复制绝对路径）");
  state.currentProject.path = "D:\\proj";

  await mod.handleContextCopyPath("D:\\proj\\src\\main.rs", false);
  check("U33", "ctx-copy-path", clip === "src\\main.rs",
    `【复制路径】应写相对路径，实际 "${clip}"`);
  await mod.handleContextCopyPath("D:\\proj\\src\\main.rs", true);
  check("U33", "ctx-copy-path", clip === "D:\\proj\\src\\main.rs",
    `【复制绝对路径】应写完整路径，实际 "${clip}"`);

  // 项目根本身：复制出空串 = 剪贴板被清空却报"已复制"，是最难发现的那种错
  clip = null;
  await mod.handleContextCopyPath("D:\\proj", false);
  check("U33", "ctx-copy-path", clip === ".",
    `项目根复制相对路径应得 "."，实际 ${JSON.stringify(clip)}`);

  // 非安全上下文没有 async clipboard：必须退回 execCommand，不能静默失败
  failClip = true;
  clip = null;
  await mod.handleContextCopyPath("D:\\proj\\a.txt", false);
  check("U33", "ctx-copy-path", execText === "a.txt",
    `剪贴板 API 不可用时要退回 execCommand，实际写入 "${execText}"`);
  const allOk = statuses.every((s) => s.kind !== "error");
  check("U33", "ctx-copy-path", statuses.length === 4 && allOk,
    `前 4 次复制都该报成功: ${JSON.stringify(statuses)}`);

  // 两条路都断：必须报 error，不能假装复制成功
  documentStub.execCommand = () => false;
  statuses.length = 0;
  const ok = await mod.handleContextCopyPath("D:\\proj\\a.txt", false);
  check("U33", "ctx-copy-path",
    ok === false && statuses.length === 1 && statuses[0].kind === "error",
    `复制失败要报 error（不能假装成功）: ok=${ok} ${JSON.stringify(statuses)}`);
}

// ============================================
// 入口
// ============================================

/**
 * U38 open-project-args：`open project` 的路径参数回放。
 * 真加载 ui/command.js，驱动 handleCommand 走完整分发（哑空白切分入口 → handleOpenCommand），
 * 断言后端 invoke("open_project") 收到的是**还原后**的路径：
 * ① 下拉形态（main.js escArg 转义过的带引号串）→ 引号剥掉、\\ 还原成单反斜杠；
 * ② 手打带引号 + 含空格路径 → 整段还原；
 * ③ 不带引号的普通路径 → 原样透传（老行为不许变）；
 * ④ 不带引号的 UNC → 打头 \\ 不被转义规则吃掉；
 * ⑤ 空参数 → 用法提示，不发 invoke。
 * 每条用例独立（清 currentProject / 计数），互不串场。
 */
async function runOpenProjectArgsChecks() {
  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const i18n = {
    getLang: () => "zh-CN",
    t: (k, params) => {
      let s = zh[k] ?? k;
      for (const [pk, pv] of Object.entries(params || {})) s = s.split(`{${pk}}`).join(pv);
      return s;
    },
  };
  const statuses = [];
  const invokes = []; // 只记 open_project
  const shown = [];
  const sandbox = {
    I18N: i18n,
    window: { I18N: i18n, SessionUI: null },
    state: { tabs: [], activeTabId: null, currentProject: null },
    samePath: (a, b) => String(a) === String(b),
    getTauriInvoke: () => async (cmd, args) => {
      if (cmd === "open_project") invokes.push(args);
      return { name: "proj", path: args.path, lang: "rust" };
    },
    setStatus: (msg, kind) => statuses.push({ msg, kind }),
    showProjectWorkspace: () => shown.push(true),
    updateTitlebarTitle: () => {},
  };
  const saved = ["window", "state", "samePath", "getTauriInvoke", "setStatus",
    "showProjectWorkspace", "updateTitlebarTitle", "I18N"]
    .map((k) => [k, globalThis[k]]);
  Object.assign(globalThis, sandbox);
  try {
    // eslint-disable-next-line no-new-func
    new Function(read("ui/command.js") +
      "\n;globalThis.__CmdAPI = { handleCommand };")();
    const handleCommand = globalThis.__CmdAPI.handleCommand;

    // 单条用例独立跑：清场（currentProject 置空避免走 teardownProject 那条大链路）
    const open = async (cmd) => {
      sandbox.state.currentProject = null;
      invokes.length = 0;
      statuses.length = 0;
      shown.length = 0;
      await handleCommand(cmd);
    };

    // ① 标题栏项目切换下拉发的就是这条（main.js escArg 转义后的形态）—— 用户踩到的 bug 本体
    await open('open project "D:\\\\Projects\\\\Go\\\\ai-gateway"');
    check("U38", "open-project-args",
      invokes.length === 1 && invokes[0].path === "D:\\Projects\\Go\\ai-gateway" &&
        shown.length === 1,
      `下拉形态的转义路径必须还原成单反斜杠且不带引号: ${JSON.stringify(invokes)}`);

    // ② 手打带引号 + 含空格
    await open('open project "D:\\My Docs\\hello world"');
    check("U38", "open-project-args",
      invokes.length === 1 && invokes[0].path === "D:\\My Docs\\hello world",
      `带引号含空格的路径要整段还原: ${JSON.stringify(invokes)}`);

    // ③ 不带引号（命令栏手打的常态）→ 行为不许变
    await open("open project D:\\Projects\\Go\\ai-gateway");
    check("U38", "open-project-args",
      invokes.length === 1 && invokes[0].path === "D:\\Projects\\Go\\ai-gateway",
      `不带引号的路径原样透传: ${JSON.stringify(invokes)}`);

    // ④ 不带引号的 UNC —— 打头双反斜杠不许被转义规则吃掉（这正是"引号感知重切"
    //    只对带引号的路径启用的原因）
    await open("open project \\\\server\\share\\proj");
    check("U38", "open-project-args",
      invokes.length === 1 && invokes[0].path === "\\\\server\\share\\proj",
      `UNC 打头双反斜杠必须原样保留: ${JSON.stringify(invokes)}`);

    // ⑤ 空参数 → 用法提示，不发 invoke
    await open("open project");
    check("U38", "open-project-args",
      invokes.length === 0 && statuses.some((s) => String(s.msg).includes("open project")),
      `空参数要给用法提示且不发 invoke: ${JSON.stringify({ invokes, statuses })}`);
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
    delete globalThis.__CmdAPI;
  }
}

/**
 * U39 failover-config：备用 LLM（故障切换）的跨层键契约。
 *
 * 纯静态、跨模块：前端 SCHEMA 声明的字段 → 落盘键 `ruyix.code.<section>.<key>`
 * （config.js 的 `"ruyix.code." + section + "." + key`）→ 宿主桥 read 的键。
 * 这条链上任何一侧单边改名，界面都长得完全正常（字段渲染得出、保存也报成功），
 * 只有"配了不生效"——所以必须把两端的字符串钉在一起。
 *
 * **只扫桥的生产代码**（`#[cfg(test)]` 之前）：反向验证时实测过 —— 本文件新增的桥单测里
 * 就写着同样的键名，整文件 `includes` 会搜到测试里的那一份，于是把生产代码的键改错
 * 门禁照样绿。"绿得没有理由"就是这么来的。
 */
async function runFailoverChecks() {
  const configJs = read("ui/config.js");
  const bridgeRs = read("src-tauri/src/agent/config_bridge.rs");
  const engineCfg = read("crates/harness-engine/src/config.rs");
  const prodEnd = bridgeRs.indexOf("#[cfg(test)]");
  const bridgeProd = prodEnd > 0 ? bridgeRs.slice(0, prodEnd) : bridgeRs;

  // 切片本身要可信：切没了（或切错位置）下面几条会退化成"永远绿"
  check("U39", "failover-config",
    bridgeProd.includes("fn build_app_config") && bridgeProd.includes("apply_ai_fallback_keys"),
    "桥的生产代码切片没取到（门禁会退化成永远绿）");

  // ① ai_fallback 段的字段清单与顺序（顺序即表单渲染序，也是桥要读的清单）
  const fallbackSec = configJs.slice(
    configJs.indexOf('section: "ai_fallback"'),
    configJs.indexOf('section: "ui"'));
  const fbKeys = [...fallbackSec.matchAll(/key:\s*"([\w]+)"/g)].map((m) => m[1]);
  check("U39", "failover-config",
    JSON.stringify(fbKeys) ===
      JSON.stringify(["api_url", "api_key", "model", "api_format"]),
    `ai_fallback 段字段与约定不符: ${JSON.stringify(fbKeys)}`);

  // ② 前端每个字段，宿主桥都必须 read 同一个键
  const notBridged = fbKeys.filter((k) => !bridgeProd.includes(`"ruyix.code.ai_fallback.${k}"`));
  check("U39", "failover-config", notBridged.length === 0,
    `宿主桥没读这些键（界面配了也不生效）: ${notBridged.join(", ")}`);

  // ③ 主用段也必须能选协议（否则用户改不了主用协议，只能改备用）
  const aiSec = configJs.slice(configJs.indexOf('section: "ai"'),
    configJs.indexOf('section: "ai_fallback"'));
  check("U39", "failover-config",
    /key:\s*"api_format"/.test(aiSec) && bridgeProd.includes('"ruyix.code.ai.api_format"'),
    "主用 ai 段缺 api_format（前端或宿主桥任一侧缺失）");

  // ④ 单源纪律：备用 LLM 整段由宿主 ai_fallback 段管，引擎 FormHidden 藏掉，
  //    且不许被抄回 config.js（抄回去 = 同一样东西摆两处，正是 D8 要消灭的）
  const hiddenSec = engineCfg.slice(
    engineCfg.indexOf("const FORM_HIDDEN"),
    engineCfg.indexOf("fn is_hidden"));
  check("U39", "failover-config",
    hiddenSec.includes('"llm_fallback"') && !/key:\s*"llm_fallback/.test(configJs),
    "llm_fallback 应在引擎 FORM_HIDDEN 里，且不许被抄进 config.js 表单");

  // ⑤ 引擎默认不配备用（None）—— 无备用时行为必须与旧版一致，不能凭空多出个端点
  const defaultSec = engineCfg.slice(engineCfg.indexOf("impl Default for AppConfig"),
    engineCfg.indexOf("fn schema()"));
  check("U39", "failover-config",
    /llm_fallback:\s*None/.test(defaultSec),
    "AppConfig::default() 必须是 llm_fallback: None（默认不切换 = 行为等同旧版）");
}

/**
 * U40 anthropic-format：LLM 协议二选一（openai / anthropic）的跨层契约。
 *
 * 新增一套协议最怕"配置项在、实现没跟上"或"实现只在一条分支上"：
 * 用户在表单里选了 anthropic，请求却仍按 OpenAI 拼 → 4xx 打回，且报错文案看不出原因。
 * 这里把「表单可选项 = 引擎合法值 = 引擎真实现的协议要素」三段钉在一起。
 */
async function runAnthropicFormatChecks() {
  const configJs = read("ui/config.js");
  const llmRs = read("crates/harness-engine/src/llm.rs");
  const engineCfg = read("crates/harness-engine/src/config.rs");
  const zh = JSON.parse(read("ui/lang/zh-CN.json"));
  const en = JSON.parse(read("ui/lang/en.json"));

  // ① 两段各一个 api_format 二选一下拉，取值恰为 openai,anthropic
  const opts = [...configJs.matchAll(/key:\s*"api_format"[^}]*options:\s*\[([^\]]*)\]/g)]
    .map((m) => m[1].split(",").map((s) => s.trim().replace(/^"|"$/g, "")).join(","));
  check("U40", "anthropic-format",
    opts.length === 2 && opts.every((o) => o === "openai,anthropic"),
    `ai / ai_fallback 各需一个 api_format 二选一（openai,anthropic）: ${JSON.stringify(opts)}`);

  // ② 引擎真的实现了 anthropic 三件套：端点 / 鉴权 / 解析
  const need = ["/v1/messages", "x-api-key", "anthropic-version",
    "fn extract_anthropic", "AuthScheme::Anthropic"];
  const lack = need.filter((p) => !llmRs.includes(p));
  check("U40", "anthropic-format", lack.length === 0,
    `llm.rs 缺 anthropic 协议要素: ${lack.join(", ")}`);

  // ③ request_plan 必须最先判 anthropic —— 判晚了会被 web_search_on 包成 /responses
  const rpStart = llmRs.indexOf("fn request_plan");
  const rp = llmRs.slice(rpStart, rpStart + 900);
  check("U40", "anthropic-format",
    rpStart > 0 && rp.indexOf("anthropic") > -1 &&
      rp.indexOf("anthropic") < rp.indexOf("web_search_on") &&
      rp.includes("AuthScheme::Anthropic"),
    "request_plan 必须最先判 api_format==anthropic（否则 anthropic 请求被送进 /responses）");

  // ④ 合法值声明在引擎 ENUM_KEYS，且 harness 表单藏掉它（单源在宿主 ai / ai_fallback 段）
  const enumSec = engineCfg.slice(
    engineCfg.indexOf("const ENUM_KEYS"),
    engineCfg.indexOf("const FORM_HIDDEN"));
  const hiddenSec = engineCfg.slice(
    engineCfg.indexOf("const FORM_HIDDEN"),
    engineCfg.indexOf("fn is_hidden"));
  check("U40", "anthropic-format",
    /"llm\.api_format",\s*&\["openai",\s*"anthropic"\]/.test(enumSec) &&
      hiddenSec.includes('"llm.api_format"'),
    "引擎 ENUM_KEYS 要声明 llm.api_format 合法值，且 FORM_HIDDEN 要藏掉它（避免与宿主段重复）");

  // ⑤ 新增文案中英 parity（缺英文键界面会直接显示键名）
  const needI18n = ["config.section.ai_fallback", "config.field.ai.api_format",
    "config.field.ai_fallback.api_format", "config.desc.ai.api_format",
    "config.desc.ai_fallback.api_format"];
  const lackI18n = needI18n.filter((k) => !(k in zh) || !(k in en));
  check("U40", "anthropic-format", lackI18n.length === 0,
    `缺 i18n 键（中/英须同步）: ${lackI18n.join(", ")}`);
}

/**
 * styles.css 里某个选择器的规则体（先剥注释 —— 注释里提一句 `.foo` 不该算数，
 * 这条纪律是编辑器那边"用一句含类名的注释骗过门禁"抓出来的）。
 */
function cssRule(css, sel) {
  const code = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const i = code.indexOf(sel + " {");
  if (i < 0) return "";
  const j = code.indexOf("}", i);
  return j < 0 ? "" : code.slice(i + sel.length, j);
}

/**
 * U41 session-trace：会话气泡的执行轨迹 —— 静态契约 + 真事件回放 + 落盘往返。
 *
 * 回放方式：微型 DOM stub + `window.__TAURI__` 桩（`event.listen` 收下回调、
 * `core.invoke` 按命令给应答，`agent_reply` 故意**永不兑现** —— 那正是"跑着呢"这一帧），
 * 真的走 `sendMessage` → 事件回调 → 渲染 → 落盘，而不是另抄一份渲染逻辑来断言。
 */
async function runSessionTraceChecks() {
  // ---- 静态契约：三处缺一不可 ----
  const sessJs = read("ui/session.js");
  const css = read("ui/styles.css");
  const sessRs = read("src-tauri/src/agent/sessions.rs");

  check("U41", "trace-render",
    sessJs.includes("function traceRowHtml") && sessJs.includes("function traceHtml") &&
      sessJs.includes("function pushTrace"),
    "session.js 缺执行轨迹的渲染/追加函数（事件被监听却没人画 = 气泡继续停在「…」）");

  const rule = cssRule(css, ".session-trace-text");
  check("U41", "trace-ellipsis",
    rule.includes("white-space: nowrap") && rule.includes("overflow: hidden") &&
      rule.includes("text-overflow: ellipsis"),
    "`.session-trace-text` 少了 nowrap/overflow/text-overflow 之一：" +
      "「一行显示不全就用省略号」会退化成折行或把气泡撑宽");

  check("U41", "trace-persist",
    /pub trace: Vec<TraceSnap>/.test(sessRs) && /pub struct TraceSnap\b/.test(sessRs),
    "sessions.rs 未声明 SessionMsg.trace / TraceSnap —— 结构体不声明，落盘往返就把它抹掉");
  check("U41", "trace-persist",
    /#\[serde\(default, skip_serializing_if = "Vec::is_empty"\)\]\s*\n\s*pub trace: Vec<TraceSnap>/
      .test(sessRs),
    "trace 字段缺 default（老会话读不回来）或缺 skip_serializing_if（每个旧文件都被改写一遍）");

  // ---- 真事件回放 ----
  const elements = new Map();
  const el = (id) => {
    if (!elements.has(id)) elements.set(id, makeEl(id));
    return elements.get(id);
  };
  const handlers = new Map();   // agent://* → 回调
  const calls = [];             // {cmd, args}
  let finishReply = null;       // 兑现 agent_reply = 这一轮跑完
  const core = {
    invoke(cmd, args) {
      calls.push({ cmd, args });
      if (cmd === "agent_reply") {
        return new Promise((res) => { finishReply = res; }); // 挂着 = 还在跑
      }
      if (cmd === "agent_session_new") {
        return Promise.resolve({
          id: "sess_trace", title: "", created_at: "t", updated_at: "t", messages: [],
        });
      }
      if (cmd === "agent_session_list") return Promise.resolve([]);
      if (cmd === "agent_env_probe") return Promise.resolve(null);
      return Promise.resolve({});
    },
  };
  const sandbox = {
    window: { SessionUI: null, state: { tabs: [], currentProject: null } },
    document: {
      getElementById: el,
      createElement: (tag) => makeEl("<" + tag + ">"),
      querySelector: () => null,
      querySelectorAll: () => [],
    },
    state: { tabs: [], currentProject: { path: "C:/probe" } },
    // 真机链路是 getInvoke() → getTauriInvoke()（main.js）→ window.__TAURI__.core.invoke
    // —— 按同一条链铺桩，免得测的是"另一条路"（而且不还回去就会污染后面的场景）
    getTauriInvoke: () => core.invoke.bind(core),
    renderTabs() {}, switchTab() {}, closeTab() {}, setStatus() {},
    setTimeout(fn) { fn(); return 0; },
    clearTimeout() {}, setInterval() { return 0; }, clearInterval() {},
  };
  sandbox.window.state = sandbox.state;
  sandbox.window.__TAURI__ = {
    event: {
      listen(name, cb) { handlers.set(name, cb); return Promise.resolve(() => {}); },
    },
    core,
  };

  const saved = ["window", "document", "setTimeout", "clearTimeout", "setInterval",
    "clearInterval", "state", "getTauriInvoke", "renderTabs", "switchTab", "closeTab",
    "setStatus"]
    .map((k) => [k, globalThis[k]]);
  Object.assign(globalThis, sandbox);
  try {
    // eslint-disable-next-line no-new-func
    new Function(sessJs)();
    const SessionUI = sandbox.window.SessionUI;
    SessionUI.attach();
    const s = await SessionUI.newSession(false);
    const wrap = SessionUI.ensureChatEl(sandbox.state.tabs[0]);
    // 发送后**不 await**：agent_reply 先挂着，这一帧就是"跑着呢"
    const running = SessionUI.sendMessage(s, wrap, "把会话气泡变成看得见的执行轨迹");
    for (let i = 0; i < 20; i++) await Promise.resolve();

    const ph = s.messages[s.messages.length - 1];
    check("U41", "trace-live-frame",
      ph && ph.role === "assistant" && ph.text === "正在执行…" && handlers.has("agent://log"),
      `占位气泡/监听未就位（role=${ph?.role} text=${JSON.stringify(ph?.text)}）`);

    const msgs = wrap.querySelector("[data-msgs]");
    check("U41", "trace-slot",
      msgs.innerHTML.includes("session-trace") && msgs.innerHTML.includes("正在准备"),
      "跑起来的那一刻气泡里没有轨迹小节（用户又只能看见三个点）");

    // 真事件：阶段 → 计划 → 一次成功调用（长行）→ 一次失败调用 → 收尾
    const LONG = "ui/session.js（905-1145）" + "很长的补充说明".repeat(12);
    const fire = (name, payload) => handlers.get(name)({ payload });
    fire("agent://stage", { stage: "agent", status: "start", detail: "工具循环（最多 40 轮，写入策略：Confirm）" });
    fire("agent://plan", { steps: [{ id: 1, title: "读现有渲染" }, { id: 2, title: "加轨迹" }] });
    fire("agent://log", { level: "info", msg: "[agent] 第 1 轮 read ✓ " + LONG });
    fire("agent://log", { level: "warn", msg: "[agent] 第 2 轮 write ✗ 锚点在文件里出现 2 次" });
    fire("agent://log", { level: "ok", msg: "[agent] 完成，共 2 轮" });

    const html = msgs.innerHTML;
    check("U41", "trace-rows",
      html.includes("session-trace-row--think") && html.includes("session-trace-row--do") &&
        html.includes("session-trace-row--fail") && html.includes("session-trace-row--done"),
      "四类轨迹行没都画出来（think/do/fail/done 各一种图标与配色）");
    check("U41", "trace-log-prefix",
      html.includes("第 1 轮 read ✓") && !html.includes("[agent] 第 1 轮"),
      "工具行没剥掉 `[agent] ` 前缀（那是日志前缀，不是给用户看的内容）");
    check("U41", "trace-long-title",
      html.includes(`title="第 1 轮 read ✓ ${LONG}"`),
      "长行的全文没进 title —— 省略号收尾后用户就再也看不到完整路径了");
    check("U41", "trace-plan-line",
      html.includes("列了 2 步计划：读现有渲染 → 加轨迹"),
      "plan 事件没进轨迹（模型亲口说的计划是最像「思考」的一手事实）");

    // 老病：日志把气泡正文覆盖掉。正文必须原样留着，过程只进轨迹。
    check("U41", "trace-text-intact",
      ph.text === "正在执行…" && !html.includes(`<div class="session-bubble session-bubble--md"><p>第 1 轮`),
      "日志行把助手气泡的正文覆盖了（原先就是这条路径让气泡变成日志滚动条）");

    const kinds = (ph.trace ?? []).map((t) => t.kind).join(",");
    check("U41", "trace-kinds",
      kinds === "think,think,do,fail,done",
      `轨迹分类/时序不对（实际 ${kinds || "空"}）—— 期望 阶段、计划、调用、失败、收尾`);

    // ---- 收尾：让 agent_reply 兑现，走完 finally（答复回填 + 落盘）----
    finishReply({ answer: "改好了：气泡下面多了一块轨迹", verifications: [], reflections: [], asks: [] });
    await running;
    const after = msgs.innerHTML;
    check("U41", "trace-after-run",
      ph.text === "改好了：气泡下面多了一块轨迹" && after.includes("session-trace-row--do") &&
        !after.includes('data-trace-live="1"'),
      "跑完之后轨迹不该消失（只是从 live 变成静态块），正文要换成真答复");

    const save = calls.filter((c) => c.cmd === "agent_session_save").pop();
    let savedTrace = null;
    try {
      savedTrace = JSON.parse(save.args.sessionJson).messages.pop().trace;
    } catch {
      savedTrace = null;
    }
    check("U41", "trace-saved",
      Array.isArray(savedTrace) && savedTrace.length === 5 && savedTrace[2].kind === "do",
      "轨迹没进落盘载荷 —— 重开会话看不到这轮干了什么（这就是 U15d 那条纪律）");

    // 越界 kind：`kind` 是**会被持久化、从盘上的会话文件读回来**的字符串（输入不是常量）。
    // 用 `TRACE_ICON[t.kind]` 这种真值判断会顺着**原型链**命中 `constructor` 之类 ——
    // 于是图标那一格被塞进函数源码、class 拼成 `session-trace-row--constructor`。
    ph.trace.push({ kind: "constructor", text: "越界 kind 该按 think 画" });
    SessionUI.openSession(s); // 复用已开 tab → fillMsgs 全量重渲染（不用再造一个事件）
    check("U41", "trace-kind-whitelist",
      !msgs.innerHTML.includes("native code") &&
        msgs.innerHTML.includes("越界 kind 该按 think 画") &&
        msgs.innerHTML.includes("session-trace-row--think"),
      "白名单外的 kind 没被挡住：原型链上的名字（constructor 等）会被当成合法 kind，" +
        "图标格里漏出函数源码、class 也拼成垃圾");
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete globalThis[k];
      else globalThis[k] = v;
    }
  }
}

async function main() {
  // 逐个场景 try —— 单个场景崩溃时记一条 FAIL 并继续，别让整份报告消失
  const scenarios = [
    ["U1", "static-checks", runStaticChecks],
    ["U6", "session-replay", runSessionChecks],
    ["U9", "config-replay", runConfigChecks],
    ["U22", "help-replay", runHelpChecks],
    ["U31", "editor-virtual-render", runEditorChecks],
    ["U32", "editor-layout-real", runEditorLayoutProbe],
    ["U23", "service-replay", runServiceChecks],
    ["U30", "proc-log-replay", runProcLogChecks],
    ["U24", "external-link-replay", runExternalLinkChecks],
    ["U33", "ctx-copy-path", runContextMenuChecks],
    ["U38", "open-project-args", runOpenProjectArgsChecks],
    ["U39", "failover-config", runFailoverChecks],
    ["U40", "anthropic-format", runAnthropicFormatChecks],
    ["U41", "session-trace", runSessionTraceChecks],
    ["U42", "session-trace-layout-real", runSessionTraceLayoutProbe],
    ["U43", "terminal-layout-real", runTerminalLayoutProbe],
    ["U44", "nav-layout-real", runNavLayoutProbe],
    ["U45", "tab-context-menu", runTabContextMenuChecks],
  ];
  for (const [id, name, fn] of scenarios) {
    try {
      await fn();
    } catch (err) {
      // 只印第一行会藏掉出事的位置（场景一崩就得重跑一遍才能定位）→ 连调用点一起给
      const where = err && err.stack
        ? err.stack.split("\n").slice(1, 9).map((s) => s.trim()).join(" ⏐ ")
        : "";
      check(id, name, false, `场景崩溃: ${(err && err.message) || err}｜${where}`);
    }
  }

  const width = 16;
  for (const r of results) {
    const tag = r.ok ? "PASS" : "FAIL";
    const line = `${tag}  ${r.id} ${r.name.padEnd(width)}`;
    if (r.ok) {
      console.log(line);
    } else {
      console.log(`${line}  ${r.detail}`);
    }
  }
  const passed = results.length - failed;
  console.log(`\nui-smoke: ${passed}/${results.length} 项通过` +
    (failed ? `，${failed} 项失败` : ""));
  process.exit(failed ? 1 : 0);
}

main();

