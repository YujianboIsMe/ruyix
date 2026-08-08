/**
 * Darkhorse Code — Main JavaScript
 * 使用 Tauri 2 原生 API（window.__TAURI__），无需 npm 依赖
 */

// ============================================
// 全局状态
// ============================================

const state = {
  currentProject: null, // { name, path } | null
  tabs: [],             // [{ id, name, path, content }]
  activeTabId: null,
};

// ============================================
// 初始化
// ============================================

(async function init() {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
    return;
  }
  setupWindowControls();
  setupCommandBar();
  setupResponsiveTitlebar();
  setupNavigatorTabs();
  setupTextareaSync();
  await autoOpenLastProject();
})();

// ============================================
// Tauri API 辅助函数
// ============================================

function getTauriWindow() {
  if (window.__TAURI__ && window.__TAURI__.window) {
    return window.__TAURI__.window.appWindow;
  }
  return null;
}

function getTauriCore() {
  if (window.__TAURI__ && window.__TAURI__.core) {
    return window.__TAURI__.core;
  }
  return null;
}

function getTauriInvoke() {
  const core = getTauriCore();
  if (core && core.invoke) {
    return core.invoke.bind(core);
  }
  return null;
}

// ============================================
// 窗口控制 (最小化 / 最大化 / 关闭)
// ============================================

function setupWindowControls() {
  const btnMinimize = document.getElementById("btn-minimize");
  const btnMaximize = document.getElementById("btn-maximize");
  const btnClose = document.getElementById("btn-close");

  btnMinimize?.addEventListener("click", () => {
    const win = getTauriWindow();
    if (win) win.minimize();
  });

  btnMaximize?.addEventListener("click", () => {
    const win = getTauriWindow();
    if (win) win.toggleMaximize();
  });

  btnClose?.addEventListener("click", () => {
    const win = getTauriWindow();
    if (win) win.close();
  });

  // 双击标题栏 drag region 切换最大化
  const titlebar = document.getElementById("titlebar");
  titlebar?.addEventListener("dblclick", (e) => {
    if (
      e.target.hasAttribute("data-tauri-drag-region") ||
      e.target.closest("[data-tauri-drag-region]")
    ) {
      const win = getTauriWindow();
      if (win) win.toggleMaximize();
    }
  });

  updateMaximizeIcon();
}

async function updateMaximizeIcon() {
  const win = getTauriWindow();
  if (!win) return;

  try {
    const refresh = async () => {
      const maximized = await win.isMaximized();
      const btn = document.getElementById("btn-maximize");
      if (!btn) return;
      if (maximized) {
        btn.innerHTML = `
          <svg width="12" height="12" viewBox="0 0 12 12">
            <rect x="2" y="0" width="9" height="9" stroke="currentColor" stroke-width="1" fill="var(--bg-titlebar)"/>
            <rect x="0" y="2" width="9" height="9" stroke="currentColor" stroke-width="1" fill="none"/>
          </svg>`;
        btn.setAttribute("title", "还原");
      } else {
        btn.innerHTML = `
          <svg width="12" height="12" viewBox="0 0 12 12">
            <rect x="1" y="1" width="10" height="10" stroke="currentColor" stroke-width="1" fill="none"/>
          </svg>`;
        btn.setAttribute("title", "最大化");
      }
    };

    await refresh();
    win.onResized(() => refresh());
  } catch {
    // 静默处理
  }
}

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

  // 点击页面空白区域时聚焦命令栏（不劫持编辑器、输入框等可编辑区域）
  document.addEventListener("click", (e) => {
    const tag = e.target.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "BUTTON" || tag === "SELECT") return;
    if (e.target.isContentEditable || e.target.closest("[contenteditable]")) return;
    input.focus();
  });
}

// ============================================
// 命令解析与分发
// ============================================

/**
 * 解析并执行命令
 */
async function handleCommand(raw) {
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
    default:
      setStatus(`未知命令: ${verb}`, "error");
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

    // 高亮 Python 文件
    const ext = name.split(".").pop()?.toLowerCase();
    if (ext === "py") {
      await highlightAndRender(tab);
    } else {
      renderPlainCode(tab);
    }

    setStatus(`已打开: ${name}`);
  } catch (err) {
    setStatus(`打开文件失败: ${err}`, "error");
  }
}

// ============================================
// 标签页管理
// ============================================

function renderTabs() {
  const bar = document.getElementById("tab-bar");
  if (!bar) return;

  bar.innerHTML = state.tabs
    .map(
      (t) => `
    <div class="tab-item${t.id === state.activeTabId ? " active" : ""}"
         data-tab-id="${t.id}" title="${t.path}">
      <span class="tab-name">${escapeHtml(t.name)}</span>
      <span class="tab-close" data-close="${t.id}">&times;</span>
    </div>`
    )
    .join("");

  // 点击切换标签
  bar.querySelectorAll(".tab-item").forEach((el) => {
    el.addEventListener("click", (e) => {
      if (e.target.dataset.close) return; // 关闭按钮单独处理
      switchTab(el.dataset.tabId);
    });
  });

  // 关闭按钮
  bar.querySelectorAll(".tab-close").forEach((el) => {
    el.addEventListener("click", (e) => {
      e.stopPropagation();
      closeTab(el.dataset.close);
    });
  });
}

function switchTab(tabId) {
  state.activeTabId = tabId;
  renderTabs();

  const tab = state.tabs.find((t) => t.id === tabId);
  if (!tab) return;

  showEditor();
  if (tab._isTerminal) {
    // 终端标签页：恢复文本区域为只读
    const textarea = document.getElementById("editor-textarea");
    if (textarea) textarea.readOnly = true;
    return; // 内容和 gutter 已在 renderTerminalOutput 中设置
  }
  if (tab._highlighted) {
    renderHighlightedCode(tab);
  } else {
    renderPlainCode(tab);
  }
}

function closeTab(tabId) {
  const idx = state.tabs.findIndex((t) => t.id === tabId);
  if (idx === -1) return;

  state.tabs.splice(idx, 1);

  if (state.tabs.length === 0) {
    state.activeTabId = null;
    renderTabs();
    hideEditor();
  } else {
    // 激活相邻标签
    const next = state.tabs[Math.min(idx, state.tabs.length - 1)];
    switchTab(next.id);
  }
}

// ============================================
// 编辑区渲染
// ============================================

function showEditor() {
  document.getElementById("editor-empty").style.display = "none";
  document.getElementById("editor-view").style.display = "";
}

function hideEditor() {
  document.getElementById("editor-empty").style.display = "";
  document.getElementById("editor-view").style.display = "none";
  document.getElementById("editor-gutter").innerHTML = "";
  document.getElementById("editor-code-backdrop").innerHTML = "";
  document.getElementById("editor-textarea").value = "";
}

async function highlightAndRender(tab) {
  const invoke = getTauriInvoke();
  if (!invoke) return;

  try {
    const lines = await invoke("highlight_python", { code: tab.content });
    tab._highlighted = lines;
    tab._isPython = true;
    if (tab.id === state.activeTabId) {
      renderHighlightedCode(tab);
    }
  } catch {
    renderPlainCode(tab);
  }
}

function renderHighlightedCode(tab) {
  const lines = tab._highlighted;
  if (!lines) {
    renderPlainCode(tab);
    return;
  }

  const gutter = document.getElementById("editor-gutter");
  const backdrop = document.getElementById("editor-code-backdrop");
  const textarea = document.getElementById("editor-textarea");

  let gutterHtml = "";
  let codeHtml = "";
  let rawLines = [];

  for (const line of lines) {
    const n = line.line_number;
    gutterHtml += `<div class="gutter-line">${n}</div>`;

    let lineHtml = "";
    let pos = 0;
    for (const span of line.spans) {
      if (span.start_col > pos) {
        lineHtml += escapeHtml(line.text.slice(pos, span.start_col));
      }
      lineHtml +=
        `<span class="tok-${span.tag}">` +
        escapeHtml(line.text.slice(span.start_col, span.end_col)) +
        "</span>";
      pos = span.end_col;
    }
    if (pos < line.text.length) {
      lineHtml += escapeHtml(line.text.slice(pos));
    }

    codeHtml += `<div class="code-line">${lineHtml || " "}</div>`;
    rawLines.push(line.text);
  }

  gutter.innerHTML = gutterHtml;
  backdrop.innerHTML = codeHtml;
  textarea.value = rawLines.join("\n");
  textarea.readOnly = false;
}

function renderPlainCode(tab) {
  const lines = tab.content.split("\n");

  const gutter = document.getElementById("editor-gutter");
  const backdrop = document.getElementById("editor-code-backdrop");
  const textarea = document.getElementById("editor-textarea");

  let gutterHtml = "";
  let codeHtml = "";

  for (let i = 0; i < lines.length; i++) {
    gutterHtml += `<div class="gutter-line">${i + 1}</div>`;
    codeHtml += `<div class="code-line">${escapeHtml(lines[i]) || " "}</div>`;
  }

  gutter.innerHTML = gutterHtml;
  backdrop.innerHTML = codeHtml;
  textarea.value = tab.content;
  textarea.readOnly = false;
}

function escapeHtml(s) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

// ============================================
// 编辑器 textarea 同步
// ============================================

function setupTextareaSync() {
  const textarea = document.getElementById("editor-textarea");
  if (!textarea) return;

  // 用户编辑时，同步内容到 tab
  textarea.addEventListener("input", () => {
    const tab = state.tabs.find((t) => t.id === state.activeTabId);
    if (!tab) return;

    const newContent = textarea.value;
    if (tab.content === newContent) return;

    tab.content = newContent;
    tab._highlighted = null;
    tab._isPython = false;

    // 实时光栅渲染 < 1000 行时更新 backdrop
    if (newContent.split("\n").length <= 1000) {
      renderPlainCode(tab);
    }

    // 标记为已修改
    if (!tab._modified) {
      tab._modified = true;
      renderTabs();
    }
  });
}

// ============================================
// 标题栏 — 项目路径显示 (响应式)
// ============================================

function updateTitlebarTitle() {
  const titleEl = document.getElementById("titlebar-title");
  if (!titleEl) return;

  if (state.currentProject) {
    titleEl.textContent = state.currentProject.path;
    titleEl.classList.add("has-project");
  } else {
    titleEl.textContent = "Darkhorse Code";
    titleEl.classList.remove("has-project");
  }
}

function setupResponsiveTitlebar() {
  const centerEl = document.querySelector(".titlebar-center");
  const titleEl = document.getElementById("titlebar-title");
  if (!centerEl || !titleEl) return;

  // 隐藏的测量元素，用于精确测量文本宽度
  const measureEl = document.createElement("span");
  measureEl.style.cssText =
    "position:absolute;visibility:hidden;white-space:nowrap;" +
    "font-size:12px;font-family:inherit;pointer-events:none;";
  document.body.appendChild(measureEl);

  const update = () => {
    if (!state.currentProject) {
      titleEl.textContent = "Darkhorse Code";
      return;
    }

    // 可用宽度 = 标题栏中间区域的宽度 - 内边距
    const available = centerEl.clientWidth - 24;
    if (available <= 0) return;

    // 测量完整路径的像素宽度
    measureEl.textContent = state.currentProject.path;
    const fullWidth = measureEl.getBoundingClientRect().width;

    if (fullWidth > available) {
      // 空间不够 → 只显示项目名
      titleEl.textContent = state.currentProject.name;
    } else {
      // 空间足够 → 显示完整路径
      titleEl.textContent = state.currentProject.path;
    }
  };

  new ResizeObserver(update).observe(centerEl);
}

// ============================================
// 工作区 UI 更新
// ============================================

// ============================================
// 工作区状态切换
// ============================================

function showWelcomePage() {
  const welcome = document.getElementById("welcome-page");
  const project = document.getElementById("project-workspace");
  if (welcome) welcome.style.display = "";
  if (project) project.style.display = "none";

  // 清空标签页和编辑器
  state.tabs = [];
  state.activeTabId = null;
  renderTabs();
  hideEditor();
}

function showProjectWorkspace() {
  const welcome = document.getElementById("welcome-page");
  const project = document.getElementById("project-workspace");
  if (welcome) welcome.style.display = "none";
  if (project) project.style.display = "";

  // 加载项目文件树和运行目标
  if (state.currentProject) {
    loadFileTree(state.currentProject.path);
    loadRunTargets();
  }
}

// ============================================
// 导航区标签页切换
// ============================================

function setupNavigatorTabs() {
  const tabs = document.querySelectorAll(".nav-tab");
  tabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      const panelId = "nav-panel-" + tab.dataset.tab;

      // 切换标签激活状态
      tabs.forEach((t) => t.classList.remove("active"));
      tab.classList.add("active");

      // 切换面板
      document.querySelectorAll(".nav-panel").forEach((p) => p.classList.remove("active"));
      document.getElementById(panelId)?.classList.add("active");

      // 切换到运行目标标签时刷新
      if (tab.dataset.tab === "target") {
        loadRunTargets();
      }
    });
  });
}

// ============================================
// 运行目标
// ============================================

async function loadRunTargets() {
  const list = document.getElementById("run-targets-list");
  if (!list) return;

  const invoke = getTauriInvoke();
  if (!invoke) {
    list.innerHTML = '<span class="run-targets-empty">Tauri API 不可用</span>';
    return;
  }

  try {
    const projectRoot = state.currentProject?.path || undefined;
    const targets = await invoke("get_run_targets", { projectRoot });

    if (!targets || targets.length === 0) {
      list.innerHTML = '<span class="run-targets-empty">暂无运行目标</span>';
      return;
    }

    list.innerHTML = targets
      .map(
        (t) => `
      <div class="run-target-item" data-cmd="${escapeHtml(t.cmd || "")}" data-name="${escapeHtml(t.name || t.key)}">
        <div class="run-target-name">
          <span class="run-icon">&#9654;</span>
          ${escapeHtml(t.name || t.key)}
        </div>
        <div class="run-target-cmd">${escapeHtml(t.cmd || "（无命令）")}</div>
      </div>`
      )
      .join("");

    // 点击运行
    list.querySelectorAll(".run-target-item").forEach((el) => {
      el.addEventListener("click", () => {
        const cmd = el.dataset.cmd;
        const name = el.dataset.name;
        if (cmd) runTargetCmd(name, cmd);
      });
    });
  } catch (err) {
    list.innerHTML = `<span class="run-targets-empty">加载失败: ${err}</span>`;
  }
}

/**
 * 运行目标：在编辑区打开终端标签页执行命令
 */
async function runTargetCmd(name, cmd) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus("Tauri API 不可用");
    return;
  }

  // 检查是否已打开同名标签
  const existing = state.tabs.find((t) => t._runTarget === cmd);
  if (existing) {
    switchTab(existing.id);
    return;
  }

  // 创建终端标签页
  const tab = {
    id: "run-" + Date.now().toString(),
    name,
    path: "",
    content: cmd,
    _isTerminal: true,
    _runTarget: cmd,
  };
  state.tabs.push(tab);
  renderTabs();
  switchTab(tab.id);

  // 显示加载中
  showEditor();
  renderTerminalOutput(tab, `> ${cmd}\n\n正在执行...`);

  try {
    setStatus(`正在运行: ${name}`);
    const result = await invoke("run_target", { cmd });

    let output = `> ${cmd}\n`;

    if (result.stdout) {
      output += result.stdout;
      if (!result.stdout.endsWith("\n")) output += "\n";
    }
    if (result.stderr) {
      output += result.stderr;
      if (!result.stderr.endsWith("\n")) output += "\n";
    }

    if (result.exit_code != null) {
      output += `\n[进程退出，代码: ${result.exit_code}]`;
    } else {
      output += `\n[进程结束]`;
    }

    renderTerminalOutput(tab, output);
    setStatus(`${name} 执行完毕 (exit: ${result.exit_code ?? "?"})`);
  } catch (err) {
    renderTerminalOutput(tab, `> ${cmd}\n\n[错误] ${err}`);
    setStatus(`运行失败: ${err}`, "error");
  }
}

/**
 * 渲染终端输出到标签页
 */
function renderTerminalOutput(tab, text) {
  const backdrop = document.getElementById("editor-code-backdrop");
  const textarea = document.getElementById("editor-textarea");
  const gutter = document.getElementById("editor-gutter");

  const lines = text.split("\n");
  let gutterHtml = "";
  let codeHtml = "";
  for (let i = 0; i < lines.length; i++) {
    gutterHtml += `<div class="gutter-line">${i + 1}</div>`;
    codeHtml += `<div class="code-line terminal-line">${escapeHtml(lines[i]) || " "}</div>`;
  }

  gutter.innerHTML = gutterHtml;
  backdrop.innerHTML = codeHtml;
  textarea.value = text;
  textarea.readOnly = true;
}

// ============================================
// 文件树
// ============================================

/**
 * 加载指定目录到文件树根节点
 */
async function loadFileTree(dirPath) {
  const tree = document.getElementById("file-tree");
  if (!tree) return;

  tree.innerHTML = "";

  const invoke = getTauriInvoke();
  if (!invoke) {
    tree.innerHTML = '<span class="file-tree-placeholder">Tauri API 不可用</span>';
    return;
  }

  try {
    const entries = await invoke("list_dir", { path: dirPath });
    if (entries.length === 0) {
      tree.innerHTML = '<span class="file-tree-placeholder">空目录</span>';
      return;
    }
    for (const entry of entries) {
      renderTreeEntry(entry, tree, 0);
    }
  } catch (err) {
    tree.innerHTML = `<span class="file-tree-placeholder">读取失败: ${err}</span>`;
  }
}

/**
 * 渲染单个树节点
 */
function renderTreeEntry(entry, container, depth) {
  const node = document.createElement("div");
  node.className = "tree-node";
  node.style.paddingLeft = depth * 16 + "px";
  node.dataset.path = entry.path;
  node.dataset.isDir = entry.is_dir;

  // 箭头（仅目录显示）
  const arrow = document.createElement("span");
  arrow.className = entry.is_dir ? "tree-arrow" : "tree-arrow hidden";
  arrow.textContent = "▶";
  node.appendChild(arrow);

  // 图标
  const icon = document.createElement("span");
  icon.className = entry.is_dir ? "tree-icon tree-icon--folder" : "tree-icon tree-icon--file";
  icon.textContent = entry.is_dir ? "📁" : "📄";
  node.appendChild(icon);

  // 名称
  const name = document.createElement("span");
  name.className = "tree-name";
  name.textContent = entry.name;
  node.appendChild(name);

  // 子节点容器（仅目录有）
  let childrenContainer = null;
  if (entry.is_dir) {
    childrenContainer = document.createElement("div");
    childrenContainer.className = "tree-children";
    childrenContainer.style.display = "none";
  }

  // 点击展开/折叠（目录）
  if (entry.is_dir) {
    node.addEventListener("click", () => toggleTreeNode(node, entry, childrenContainer, depth));
  } else {
    // 双击打开文件
    node.addEventListener("dblclick", () => openFile(entry.path));
  }

  container.appendChild(node);
  if (childrenContainer) {
    container.appendChild(childrenContainer);
  }
}

/**
 * 展开/折叠目录节点（懒加载）
 */
async function toggleTreeNode(node, entry, childrenContainer, depth) {
  const arrow = node.querySelector(".tree-arrow");

  // 如果已经展开，折叠
  if (arrow.classList.contains("expanded")) {
    arrow.classList.remove("expanded");
    childrenContainer.style.display = "none";
    return;
  }

  // 展开
  arrow.classList.add("expanded");

  // 懒加载子节点
  if (childrenContainer.children.length === 0) {
    const invoke = getTauriInvoke();
    if (!invoke) return;

    try {
      const entries = await invoke("list_dir", { path: entry.path });
      for (const child of entries) {
        renderTreeEntry(child, childrenContainer, depth + 1);
      }
    } catch {
      const err = document.createElement("span");
      err.className = "file-tree-placeholder";
      err.textContent = "读取失败";
      childrenContainer.appendChild(err);
    }
  }

  childrenContainer.style.display = "";
}

// ============================================
// 状态栏
// ============================================

/**
 * @param {"info"|"error"} level
 */
// ============================================
// 启动时自动打开上次项目
// ============================================

async function autoOpenLastProject() {
  const invoke = getTauriInvoke();
  if (!invoke) return;

  try {
    const lastPath = await invoke("get_last_project");
    if (lastPath) {
      setStatus("正在恢复上次项目...");
      await openProject(lastPath);
    }
  } catch {
    // 启动时静默失败
  }
}

// ============================================
// 状态栏
// ============================================

function setStatus(message, level = "info") {
  const el = document.querySelector("#statusbar .status-item");
  if (el) {
    el.textContent = message;
    el.style.color = level === "error" ? "#f48771" : "";
    // 1.5 秒后恢复默认颜色
    if (level === "error") {
      clearTimeout(setStatus._timeout);
      setStatus._timeout = setTimeout(() => {
        el.style.color = "";
      }, 3000);
    }
  }
}
