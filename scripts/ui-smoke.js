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
 */

"use strict";

const fs = require("fs");
const path = require("path");

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
  check("U10", "config-contract", schemaFields.length >= 6 && missField.length === 0,
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
      { section: "ui", key: "emoji", full_key: "ruyix.code.ui.emoji", value: "true", inherited: null },
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

  const saved = ["window", "document", "state", "setInterval", "clearInterval", "I18N"]
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

  const saved = ["window", "document", "state", "setInterval", "clearInterval", "I18N"]
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

// ============================================
// 入口
// ============================================

async function main() {
  // 逐个场景 try —— 单个场景崩溃时记一条 FAIL 并继续，别让整份报告消失
  const scenarios = [
    ["U1", "static-checks", runStaticChecks],
    ["U6", "session-replay", runSessionChecks],
    ["U9", "config-replay", runConfigChecks],
    ["U22", "help-replay", runHelpChecks],
    ["U23", "service-replay", runServiceChecks],
    ["U30", "proc-log-replay", runProcLogChecks],
    ["U24", "external-link-replay", runExternalLinkChecks],
  ];
  for (const [id, name, fn] of scenarios) {
    try {
      await fn();
    } catch (err) {
      const line = err && err.stack ? err.stack.split("\n")[0] : String(err);
      check(id, name, false, `场景崩溃: ${line}`);
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

