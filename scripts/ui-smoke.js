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

  // U10 config-contract：表单命令注册齐全 + schema 字段的 i18n 齐全
  const cfgCmds = [...new Set([...configJs.matchAll(/"(config_form_\w+)"/g)].map((m) => m[1]))];
  const cfgUnreg = cfgCmds.filter((c) => !mainRs.includes(`${c},`));
  check("U10", "config-contract", cfgCmds.length === 3 && cfgUnreg.length === 0,
    `config.js 引用了未注册的命令: ${cfgUnreg.join(", ")}`);
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
  check("U10", "config-contract", schemaFields.length >= 16 && missField.length === 0,
    `SCHEMA 字段缺 i18n 键: ${missField.join(", ")}（解析到 ${schemaFields.length} 个字段）`);

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
      has(cfgBridgeRs, "discover.extra") &&
      has(configJs, '"discover.enabled"') &&
      has(configJs, '"discover.extra"'),
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
      has(cfgBridgeRs, "env.install_enabled") &&
      has(configJs, '"env.install_enabled"'),
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

  check("U23", "proc-lifecycle",
    has(configRs, "pub struct ProcConfig") &&
      has(configRs, "pub proc: ProcConfig") &&
      has(cfgBridgeRs, "ruyix.code.harness.proc.enabled") &&
      has(cfgBridgeRs, "proc.ready_timeout_secs") &&
      has(configJs, '"proc.enabled"') && has(configJs, '"proc.max"') &&
      has(configJs, '"proc.ready_timeout_secs"') &&
      has(mainRs, "harness_engine::proc::shutdown_all(false)"),
    "proc 配置没贯通或宿主退出未收尾：引擎 ProcConfig → 配置桥三键 → UI 三字段 → 退出全收");

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
    check("U11", "config-scan",
      calls.length === 1 && calls[0].cmd === "config_form_load" && calls[0].args.scope === "global",
      `form global 未按契约调用 config_form_load: ${JSON.stringify(calls)}`);
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

