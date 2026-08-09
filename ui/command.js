/**
 * Darkhorse Code — Command System
 * 命令栏、命令解析与分发、以及各命令的实现。
 * 注意：本文件必须先于 main.js 加载——命令函数引用 main.js 中的
 * UI 基础设施函数（setStatus / state / getTauriInvoke / renderTabs 等）作为全局。
 */

// ============================================
// 命令栏
// ============================================

function setupCommandBar() {
  const input = document.getElementById("command-input");
  if (!input) return;

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      const cmd = input.value.trim();
      if (cmd) {
        handleCommand(cmd);
        input.value = "";
      }
    }
  });

  // 点击页面空白区域时聚焦命令栏（不劫持编辑器、终端、输入框等可编辑区域）
  document.addEventListener("click", (e) => {
    const tag = e.target.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "BUTTON" || tag === "SELECT") return;
    if (e.target.isContentEditable || e.target.closest("[contenteditable]")) return;
    if (e.target.closest("#terminal-container")) return;
    input.focus();
  });
}

// ============================================
// 命令解析与分发
// ============================================

/**
 * 解析并执行命令
 */
async function handleCommand(raw, _fromAi = false) {
  const parts = raw.split(/\s+/);
  const verb = parts[0]?.toLowerCase();

  switch (verb) {
    case "open":
      await handleOpenCommand(parts.slice(1));
      break;
    case "close":
      await handleCloseCommand(parts.slice(1));
      break;
    case "config":
      await handleConfigCommand(raw);
      break;
    case "new":
      await handleNewCommand(parts.slice(1));
      break;
    case "del":
    case "delete":
    case "remove":
    case "rm":
      await handleDeleteCommand(raw);
      break;
    case "help":
      openHelp();
      break;
    default:
      // 标准命令未命中 → 调用 AI 翻译（防止递归）
      if (!_fromAi) {
        await handleAiCommand(raw);
      } else {
        setStatus(`未知命令: ${verb}`, "error");
      }
  }
}

/**
 * AI 命令：将自然语言翻译为标准命令后执行
 */
async function handleAiCommand(raw) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus("Tauri API 不可用");
    return;
  }

  setStatus("正在思考...");
  try {
    const result = await invoke("ai_translate", { input: raw });

    // AI 返回了 (不支持) 提示
    if (result.startsWith("不支持")) {
      setStatus(result, "error");
      return;
    }

    // 闲聊回复（不是标准命令动词开头）→ 直接显示
    const firstWord = result.split(/\s+/)[0]?.toLowerCase();
    if (!["open", "close", "config", "new", "help", "del", "delete", "remove", "rm"].includes(firstWord)) {
      setStatus(result);
      return;
    }

    // AI 返回的标准命令，逐行执行（禁止递归 AI）
    const lines = result.split("\n").map((l) => l.trim()).filter((l) => l && !l.startsWith("#"));
    for (const line of lines) {
      await handleCommand(line, true);
    }
  } catch (err) {
    setStatus(`AI 命令失败: ${err}`, "error");
  }
}

/**
 * 处理 open 命令及其子命令
 *   open project <path>  — 打开项目
 *   open file <path>     — 打开文件 (待实现)
 */
async function handleOpenCommand(args) {
  if (args.length === 0) {
    setStatus("用法: open project <路径>  或  open file <路径>");
    return;
  }

  const sub = args[0]?.toLowerCase();
  const targetPath = args.slice(1).join(" ");

  switch (sub) {
    case "project":
      if (!targetPath) {
        setStatus("用法: open project <项目文件夹路径>");
        return;
      }
      await openProject(targetPath);
      break;
    case "file":
      if (!targetPath) {
        setStatus("用法: open file <文件路径>");
        return;
      }
      await openFile(targetPath);
      break;
    default:
      setStatus(`未知子命令: open ${sub}。可用: project, file`);
  }
}

/**
 * 处理 close 命令及其子命令
 *   close project       — 关闭项目
 *   close all           — 关闭所有文件
 *   close <index>       — 按序号关闭（0-based，负数为从右数，-1 是最后一个）
 *   close other/others  — 关闭除当前外的所有文件
 *   close left          — 关闭当前文件左边的所有文件
 *   close right         — 关闭当前文件右边的所有文件
 */
async function handleCloseCommand(args) {
  if (args.length === 0) {
    setStatus("用法: close project | all | <index> | other | left | right");
    return;
  }

  const sub = args[0];

  // 判断是否为数字（支持负数，如 -1 表示最后一个）
  if (/^-?\d+$/.test(sub)) {
    closeByIndex(parseInt(sub, 10));
    return;
  }

  switch (sub.toLowerCase()) {
    case "project":
      closeProject();
      break;
    case "all":
      closeAll();
      break;
    case "other":
    case "others":
      closeOthers();
      break;
    case "left":
      closeLeft();
      break;
    case "right":
      closeRight();
      break;
    default:
      setStatus(`未知子命令: close ${sub}。可用: project, all, <index>, other, left, right`);
  }
}

// ============================================
// new 命令
// ============================================

/** 类型子命令 → 扩展名映射 */
const TYPE_EXT = { py: ".py", rs: ".rs", md: ".md", c: ".c" };

async function handleNewCommand(args) {
  if (!state.currentProject) {
    setStatus("请先打开项目 (open project <路径>)", "error");
    return;
  }
  if (args.length < 2) {
    setStatus("用法: new <py|rs|md|c|file|folder|dir> <相对路径>");
    return;
  }

  const sub = args[0]?.toLowerCase();
  const rawPath = args.slice(1).join(" ");

  // 禁止绝对路径
  if (/^\/|^[A-Za-z]:[/\\]/i.test(rawPath)) {
    setStatus("您没有权限！", "error");
    return;
  }

  const base = state.currentProject.path;

  // 类型子命令：自动追加扩展名
  if (TYPE_EXT[sub]) {
    const fullPath = `${base}\\${rawPath}${TYPE_EXT[sub]}`;
    await createNewFile(fullPath);
    return;
  }

  // 通用子命令
  if (sub === "file") {
    const fullPath = `${base}\\${rawPath}`;
    await createNewFile(fullPath);
  } else if (sub === "folder" || sub === "dir") {
    const invoke = getTauriInvoke();
    if (!invoke) return;
    const fullPath = `${base}\\${rawPath}`;
    try {
      await invoke("create_dir", { path: fullPath });
      setStatus(`已创建目录: ${rawPath}`);
      loadFileTree(base);
    } catch (err) {
      setStatus(`创建目录失败: ${err}`, "error");
    }
  } else {
    setStatus(`未知类型: ${sub}。可用: py, rs, md, c, file, folder, dir`);
  }
}

async function createNewFile(fullPath) {
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    await invoke("create_file", { path: fullPath });
    setStatus(`已创建: ${fullPath}`);
    // 自动打开新文件
    await openFile(fullPath);
    // 刷新文件树
    if (state.currentProject) {
      loadFileTree(state.currentProject.path);
    }
  } catch (err) {
    setStatus(`创建失败: ${err}`, "error");
  }
}

async function handleDeleteCommand(raw) {
  if (!state.currentProject) {
    setStatus("请先打开项目 (open project <路径>)", "error");
    return;
  }

  // 提取相对路径（去掉动词）
  const rawPath = raw.replace(/^\S+\s+/, "").trim();
  if (!rawPath) {
    setStatus("用法: del|delete|remove|rm <相对路径>");
    return;
  }

  // 禁止绝对路径
  if (/^\/|^[A-Za-z]:[/\\]/i.test(rawPath)) {
    setStatus("您没有权限！", "error");
    return;
  }

  const fullPath = `${state.currentProject.path}\\${rawPath}`;
  if (!window.confirm(`确认删除？\n\n${fullPath}\n\n此操作不可撤销。`)) {
    return;
  }

  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    await invoke("delete_path", { path: fullPath });
    setStatus(`已删除: ${rawPath}`);
    // 关闭已打开的标签页
    const tab = state.tabs.find((t) => t.path === fullPath);
    if (tab) closeTab(tab.id);
    loadFileTree(state.currentProject.path);
  } catch (err) {
    setStatus(`删除失败: ${err}`, "error");
  }
}

// ============================================
// config 命令
// ============================================

/**
 * 解析 config 命令
 * 格式: config <action> [-g|-p|-r] [key] 或 config <action> [-g|-p|-r] key="value"
 * 默认 scope 为 -r (runtime)
 */
async function handleConfigCommand(raw) {
  // 去掉 "config " 前缀
  const rest = raw.slice("config".length).trim();
  if (!rest) {
    setStatus(
      "用法: config <add|get|update|remove|delete> [-g|-p|-r] [darkhorse.code.<section>.<key>[=value]]"
    );
    return;
  }

  // 解析: action scope key=value
  // 分词：保留引号内容
  const tokens = parseConfigTokens(rest);
  if (tokens.length === 0) {
    setStatus("用法: config <add|get|update|remove|delete> ...");
    return;
  }

  const actions = ["add", "get", "update", "remove", "delete"];
  let idx = 0;

  // 子命令
  const action = tokens[idx]?.toLowerCase();
  if (!actions.includes(action)) {
    setStatus(`未知 config 子命令: ${action}。可用: ${actions.join(", ")}`);
    return;
  }
  idx++;

  // scope 标志
  let scope = "r";
  if (tokens[idx] === "-g" || tokens[idx] === "-p" || tokens[idx] === "-r") {
    scope = tokens[idx].slice(1);
    idx++;
  }

  // key[=value]
  const kv = tokens.slice(idx).join(" "); // 剩余的合并
  const eqIdx = kv.indexOf("=");
  const key = eqIdx >= 0 ? kv.slice(0, eqIdx).trim() : kv.trim();
  const value = eqIdx >= 0 ? kv.slice(eqIdx + 1).trim() : null;

  // 去掉 value 外层的引号
  let cleanValue = value;
  if (cleanValue) {
    if (
      (cleanValue.startsWith('"') && cleanValue.endsWith('"')) ||
      (cleanValue.startsWith("'") && cleanValue.endsWith("'"))
    ) {
      cleanValue = cleanValue.slice(1, -1);
    }
  }

  if (!key) {
    setStatus("缺少配置键 (格式: darkhorse.code.<section>.<key>)");
    return;
  }

  await executeConfigAction(action, scope, key, cleanValue);
}

/**
 * 分词：按空格分割但保留引号内内容
 */
function parseConfigTokens(s) {
  const tokens = [];
  let i = 0;
  while (i < s.length) {
    // 跳过空白
    while (i < s.length && s[i] === " ") i++;
    if (i >= s.length) break;

    // 引号包裹
    if (s[i] === '"' || s[i] === "'") {
      const quote = s[i];
      i++;
      let tok = "";
      while (i < s.length && s[i] !== quote) {
        if (s[i] === "\\" && i + 1 < s.length) {
          tok += s[i + 1];
          i += 2;
        } else {
          tok += s[i];
          i++;
        }
      }
      i++; // 跳过闭合引号
      tokens.push(tok);
    } else {
      let tok = "";
      while (i < s.length && s[i] !== " ") {
        tok += s[i];
        i++;
      }
      tokens.push(tok);
    }
  }
  return tokens;
}

/**
 * 执行配置操作
 */
async function executeConfigAction(action, scope, key, value) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus("Tauri API 不可用");
    return;
  }

  const scopeMap = { g: "global", p: "project", r: "runtime" };

  // 从 state 取项目根路径（确保不为空字符串）
  const projectRoot = state.currentProject?.path || undefined;
  if (projectRoot === undefined && state.currentProject) {
    setStatus(`内部错误: 项目已打开但 path 为空 (${JSON.stringify(state.currentProject)})`, "error");
    return;
  }

  // -p 需要已打开项目
  if (scope === "p" && !projectRoot) {
    setStatus("未打开项目，无法使用项目配置 (-p)。请先 open project <路径>", "error");
    return;
  }

  switch (action) {
    case "add":
      {
        try {
          const existing = await invoke("config_get", {
            scope: scopeMap[scope],
            key,
            projectRoot,
          });
          if (existing != null) {
            setStatus(`配置已存在: ${key} = ${existing}`, "error");
            return;
          }
        } catch {
          // get 失败视为不存在
        }
        try {
          await invoke("config_set", { scope: scopeMap[scope], key, value, projectRoot });
          setStatus(`已添加: ${key} = ${value}`);
        } catch (err) {
          setStatus(`添加失败: ${err}`, "error");
        }
      }
      break;

    case "get":
      {
        try {
          const val = await invoke("config_get", { scope: scopeMap[scope], key, projectRoot });
          if (val == null) {
            setStatus(`配置不存在: ${key}`, "error");
          } else {
            setStatus(`${key} = ${val}`);
          }
        } catch (err) {
          setStatus(`读取失败: ${err}`, "error");
        }
      }
      break;

    case "update":
      {
        try {
          await invoke("config_set", { scope: scopeMap[scope], key, value, projectRoot });
          setStatus(`已更新: ${key} = ${value}`);
        } catch (err) {
          setStatus(`更新失败: ${err}`, "error");
        }
      }
      break;

    case "remove":
    case "delete":
      {
        try {
          await invoke("config_delete", { scope: scopeMap[scope], key, projectRoot });
          setStatus(`已删除: ${key}`);
        } catch (err) {
          setStatus(`删除失败: ${err}`, "error");
        }
      }
      break;
  }
}

// ============================================
// close 命令实现
// ============================================

function closeProject() {
  if (!state.currentProject) {
    setStatus("没有打开的项目");
    return;
  }

  const name = state.currentProject.name;
  state.currentProject = null;
  updateTitlebarTitle();
  showWelcomePage();
  setStatus(`已关闭项目: ${name}`);
}

function closeAll() {
  if (state.tabs.length === 0) {
    setStatus("没有打开的文件");
    return;
  }
  state.tabs = [];
  state.activeTabId = null;
  renderTabs();
  hideEditor();
  setStatus("已关闭所有文件");
}

/**
 * 按序号关闭标签页。
 * 正数: 0-based 从左到右
 * 负数: -1-based 从右到左（-1 = 最后一个）
 */
function closeByIndex(index) {
  if (state.tabs.length === 0) {
    setStatus("没有打开的文件");
    return;
  }
  const i = index >= 0 ? index : state.tabs.length + index;
  if (i < 0 || i >= state.tabs.length) {
    setStatus(`序号超出范围: ${index}（共 ${state.tabs.length} 个文件）`);
    return;
  }
  const tab = state.tabs[i];
  closeTab(tab.id);
  setStatus(`已关闭: ${tab.name}`);
}

/** 关闭除当前活动标签外的所有标签 */
function closeOthers() {
  if (state.tabs.length <= 1) {
    setStatus("没有其他文件可关闭");
    return;
  }
  const active = state.tabs.find((t) => t.id === state.activeTabId);
  state.tabs = active ? [active] : [];
  renderTabs();
  setStatus("已关闭其他文件");
}

/** 关闭当前活动标签左边的所有标签 */
function closeLeft() {
  const idx = state.tabs.findIndex((t) => t.id === state.activeTabId);
  if (idx <= 0) {
    setStatus("当前文件左侧没有文件");
    return;
  }
  const removed = state.tabs.slice(0, idx).map((t) => t.name).join(", ");
  state.tabs = state.tabs.slice(idx);
  renderTabs();
  setStatus(`已关闭左侧文件: ${removed}`);
}

/** 关闭当前活动标签右边的所有标签 */
function closeRight() {
  const idx = state.tabs.findIndex((t) => t.id === state.activeTabId);
  if (idx < 0 || idx >= state.tabs.length - 1) {
    setStatus("当前文件右侧没有文件");
    return;
  }
  const removed = state.tabs.slice(idx + 1).map((t) => t.name).join(", ");
  state.tabs = state.tabs.slice(0, idx + 1);
  renderTabs();
  setStatus(`已关闭右侧文件: ${removed}`);
}

/**
 * 调用后端打开项目
 */
async function openProject(path) {
  const invoke = getTauriInvoke();

  if (!invoke) {
    // 浏览器开发模式 — 模拟打开项目
    setStatus("Tauri API 不可用 (浏览器模式)");
    return;
  }

  try {
    setStatus("正在打开项目...");
    const info = await invoke("open_project", { path });

    // 保存项目信息
    state.currentProject = info;

    // 切换为项目工作区视图
    showProjectWorkspace();
    updateTitlebarTitle();
    setStatus(`已打开项目: ${info.path}`);
  } catch (err) {
    setStatus(`打开项目失败: ${err}`, "error");
  }
}

/**
 * 调用后端打开文件，创建标签页并高亮
 */
async function openFile(path) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus("Tauri API 不可用 (浏览器模式)");
    return;
  }

  // 检查是否已打开
  const existing = state.tabs.find((t) => t.path === path);
  if (existing) {
    switchTab(existing.id);
    setStatus(`已切换到: ${existing.name}`);
    return;
  }

  try {
    setStatus("正在读取文件...");
    const file = await invoke("read_file", { path });

    // 提取文件名
    const name = file.path.split(/[/\\]/).pop() || file.path;

    // 创建标签页
    const tab = { id: Date.now().toString(), name, path: file.path, content: file.content };
    state.tabs.push(tab);
    renderTabs();
    switchTab(tab.id);

    // 语法高亮：根据扩展名确定语言
    const ext = name.split(".").pop()?.toLowerCase();
    const lang = extToLanguage(ext);
    if (lang) {
      await highlightAndRender(tab, lang);
    } else {
      renderPlainCode(tab);
    }

    setStatus(`已打开: ${name}`);
  } catch (err) {
    setStatus(`打开文件失败: ${err}`, "error");
  }
}

const HELP_TEXT = `Darkhorse Code 帮助
====================

命令系统
--------
open project <路径>        打开项目文件夹
open file <路径>           打开文件
close project              关闭当前项目
close all                  关闭所有标签页
close <序号>               按序号关闭标签页（负数从右数，-1 最后一个）
close other                关闭除当前外的所有标签页
close left                 关闭当前左侧所有标签页
close right                关闭当前右侧所有标签页
new py|rs|md|c <名称>       创建文件（自动加扩展名）
new file <相对路径>          创建文件
new folder|dir <相对路径>    创建目录
del|delete|remove|rm <相对路径> 删除文件/目录（弹窗确认）
config add|get|update|remove|delete [-g|-p|-r] <key>[=<value>]
                           配置管理（默认 -r 运行时）

快捷键
------
Ctrl+S                     保存当前文件

语法高亮支持
------------
Python                     .py
Rust                       .rs
HTML（含嵌入 CSS/JS）       .html, .htm
CSS                        .css
JavaScript                 .js, .mjs, .cjs
Markdown                   .md, .markdown

联系方式
--------
CSDN 关注 醒过来摸鱼，私信即可。
`;
