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

async function initApp() {
  setupWindowControls();
  setupCommandBar();
  setupResponsiveTitlebar();
  setupNavigatorTabs();
  setupTextareaSync();
  setupTerminalList();
  setupKeyboardShortcuts();
  setupHelpMenu();
  try {
    await autoOpenLastProject();
  } catch (err) {
    setStatus(`恢复项目失败: ${err}`, "error");
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initApp);
} else {
  initApp();
}

// ============================================
// Tauri API 辅助函数
// ============================================

function getTauriWindow() {
  const w = window.__TAURI__?.window;
  if (!w) return null;
  if (w.appWindow) return w.appWindow;
  if (typeof w.getCurrentWindow === "function") return w.getCurrentWindow();
  if (typeof w.getCurrent === "function") return w.getCurrent();
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

  btnMinimize?.addEventListener("click", async () => {
    const win = getTauriWindow();
    if (win) { win.minimize(); return; }
    // fallback: invoke window plugin
    const invoke = getTauriInvoke();
    if (invoke) {
      try { await invoke("plugin:window|minimize"); } catch {}
    }
  });

  btnMaximize?.addEventListener("click", async () => {
    const win = getTauriWindow();
    if (win) { win.toggleMaximize(); return; }
    const invoke = getTauriInvoke();
    if (invoke) {
      try { await invoke("plugin:window|toggle_maximize"); } catch {}
    }
  });

  btnClose?.addEventListener("click", async () => {
    const win = getTauriWindow();
    if (win) { win.close(); return; }
    const invoke = getTauriInvoke();
    if (invoke) {
      try { await invoke("plugin:window|close"); } catch {}
    }
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
// 标签页管理
// ============================================

function renderTabs() {
  const bar = document.getElementById("tab-bar");
  if (!bar) return;

  bar.innerHTML = state.tabs
    .map((t) => {
      const icon = t._isHelp ? "🔒" : t._isTerminal ? "🖥️" : fileIcon(t.name);
      return `
    <div class="tab-item${t.id === state.activeTabId ? " active" : ""}"
         data-tab-id="${t.id}" title="${t.path}">
      <span class="tab-icon">${icon}</span>
      <span class="tab-name">${escapeHtml(t.name)}</span>
      <span class="tab-close" data-close="${t.id}">&times;</span>
    </div>`;
    })
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
  // 边界层：切换标签页前保存当前 tab
  if (window._saveBeforeSwitch) window._saveBeforeSwitch();

  state.activeTabId = tabId;
  renderTabs();

  const tab = state.tabs.find((t) => t.id === tabId);
  if (!tab) return;

  showEditor();
  hideTerminalView();
  if (tab._isTerminal) {
    // xterm.js 终端标签页 — 重新挂载到容器中
    hideEditorView();
    showTerminalView();
    if (tab._term) {
      const container = document.getElementById("terminal-container");
      container.innerHTML = "";
      tab._term.open(container);
      tab._term.focus();
    }
    return;
  }
  if (tab._highlighted) {
    renderHighlightedCode(tab);
  } else {
    renderPlainCode(tab);
  }
  if (tab._isHelp) {
    const textarea = document.getElementById("editor-textarea");
    if (textarea) textarea.readOnly = true;
  }
  updateOutline(tab);
}

function closeTab(tabId) {
  const idx = state.tabs.findIndex((t) => t.id === tabId);
  if (idx === -1) return;

  const tab = state.tabs[idx];

  // PTY 终端清理
  if (tab._isTerminal) {
    const invoke = getTauriInvoke();
    if (invoke) {
      invoke("pty_close", { tabId: tab.id }).catch(() => {});
    }
    if (tab._ptyUnlisten) tab._ptyUnlisten();
    if (tab._term) tab._term.dispose();
    hideTerminalView();
  }

  state.tabs.splice(idx, 1);

  if (state.tabs.length === 0) {
    state.activeTabId = null;
    renderTabs();
    hideEditor();
  } else {
    const next = state.tabs[Math.min(idx, state.tabs.length - 1)];
    switchTab(next.id);
  }
}

function hideEditorView() {
  document.getElementById("editor-view").style.display = "none";
}
// 编辑区渲染
// ============================================

function showEditor() {
  document.getElementById("editor-empty").style.display = "none";
  document.getElementById("editor-view").style.display = "";
}

function hideEditor() {
  document.getElementById("editor-empty").style.display = "";
  document.getElementById("editor-view").style.display = "none";
  document.getElementById("terminal-view").style.display = "none";
  document.getElementById("editor-gutter").innerHTML = "";
  document.getElementById("editor-code-backdrop").innerHTML = "";
  document.getElementById("editor-textarea").value = "";
}

async function highlightAndRender(tab, language) {
  const invoke = getTauriInvoke();
  if (!invoke) return;

  try {
    const lines = await invoke("highlight_code", { language, code: tab.content });
    tab._highlighted = lines;
    tab._language = language;
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

/** 文件扩展名 → arborium 语言名 */
function extToLanguage(ext) {
  const map = {
    py: "python",
    rs: "rust",
    html: "html",
    htm: "html",
    css: "css",
    js: "javascript",
    mjs: "javascript",
    cjs: "javascript",
    md: "markdown",
    markdown: "markdown",
  };
  return map[ext] || null;
}

/** 文件名 → 标签页图标 */
function fileIcon(name) {
  const ext = name.split(".").pop()?.toLowerCase();
  const iconMap = {
    py: "🐍",
    rs: "⛓️",
    html: "🌏",
    htm: "🌏",
    css: "🏁",
    js: "📔",
    mjs: "📔",
    cjs: "📔",
  };
  return iconMap[ext] || "📄";
}

// ============================================
// 大纲
// ============================================

function updateOutline(tab) {
  const content = document.getElementById("outline-content");
  if (!content) return;

  const ext = tab.name.split(".").pop()?.toLowerCase();
  let items;

  if (ext === "md" || ext === "markdown") {
    items = parseMarkdownOutline(tab.content);
  } else if (ext === "rs") {
    items = parseRustOutline(tab.content);
  } else if (ext === "py") {
    items = parsePythonOutline(tab.content);
  } else {
    content.innerHTML =
      '<div class="outline-placeholder">大纲（支持 Markdown / Rust / Python）</div>';
    return;
  }

  if (items.length === 0) {
    content.innerHTML = '<div class="outline-placeholder">无大纲</div>';
    return;
  }

  let html = "";
  for (const h of items) {
    const indent = (h.level - 1) * 16;
    html +=
      `<div class="outline-item" style="padding-left:${indent}px" data-line="${h.line}">` +
      `<span class="outline-dot">•</span>` +
      `${escapeHtml(h.text)}` +
      `</div>`;
  }
  content.innerHTML = html;

  content.querySelectorAll(".outline-item").forEach((el) => {
    el.addEventListener("click", () => {
      const line = parseInt(el.dataset.line, 10);
      const lineHeight = 20;
      const scrollTop = Math.max(0, (line - 1) * lineHeight);
      document.querySelector(".editor-view")?.scrollTo(0, scrollTop);
    });
  });
}

function parseMarkdownOutline(content) {
  const headings = [];
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const match = lines[i].match(/^(#{1,3})\s+(.+)$/);
    if (match) {
      headings.push({
        level: match[1].length,
        text: match[2].trim(),
        line: i + 1,
      });
    }
  }
  return headings;
}

function parseRustOutline(content) {
  const items = [];
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    // 计算缩进层级（每 4 空格或 1 tab 为一级，上限 3）
    const indent = raw.match(/^(\s*)/)[1];
    const indentLen = indent.replace(/\t/g, "    ").length;
    const level = Math.min(Math.floor(indentLen / 4) + 1, 3);

    const trimmed = raw.trimStart();
    let match;
    if ((match = trimmed.match(/^fn\s+(\w+)/))) {
      items.push({ level, text: `fn ${match[1]}()`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+fn\s+(\w+)/))) {
      items.push({ level, text: `fn ${match[1]}()`, line: i + 1 });
    } else if ((match = trimmed.match(/^struct\s+(\w+)/))) {
      items.push({ level, text: `struct ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+struct\s+(\w+)/))) {
      items.push({ level, text: `struct ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^enum\s+(\w+)/))) {
      items.push({ level, text: `enum ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+enum\s+(\w+)/))) {
      items.push({ level, text: `enum ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^trait\s+(\w+)/))) {
      items.push({ level, text: `trait ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+trait\s+(\w+)/))) {
      items.push({ level, text: `trait ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^impl\b\s*(.+)/))) {
      const detail = match[1].trim().replace(/\s*\{\s*$/, "");
      items.push({ level, text: `impl ${detail}`, line: i + 1 });
    } else if ((match = trimmed.match(/^mod\s+(\w+)/))) {
      items.push({ level, text: `mod ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+mod\s+(\w+)/))) {
      items.push({ level, text: `mod ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^macro_rules!\s*(\w+)/))) {
      items.push({ level, text: `macro ${match[1]}!`, line: i + 1 });
    }
  }
  return items;
}

function parsePythonOutline(content) {
  const items = [];
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    const indent = raw.match(/^(\s*)/)[1];
    const indentLen = indent.replace(/\t/g, "    ").length;
    const level = Math.min(Math.floor(indentLen / 4) + 1, 3);

    const trimmed = raw.trimStart();
    let match;
    if ((match = trimmed.match(/^class\s+(\w+)/))) {
      items.push({ level, text: `class ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^async\s+def\s+(\w+)/))) {
      items.push({ level, text: `async def ${match[1]}()`, line: i + 1 });
    } else if ((match = trimmed.match(/^def\s+(\w+)/))) {
      items.push({ level, text: `def ${match[1]}()`, line: i + 1 });
    }
  }
  return items;
}

// ============================================
// 键盘快捷键
// ============================================

function setupKeyboardShortcuts() {
  const isSaveShortcut = (e) =>
    (e.ctrlKey || e.metaKey) &&
    (e.code === "KeyS" || e.key === "s" || e.key === "S" || e.keyCode === 83);

  const handleSave = (e) => {
    if (isSaveShortcut(e)) {
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();
      saveCurrentFile().catch((err) => setStatus(`保存失败: ${err}`, "error"));
    }
  };

  // 策略 1: window 捕获阶段 — 最早拦截 WebView2 可能的行为
  window.addEventListener("keydown", handleSave, true);

  // 策略 2: document 冒泡阶段 — 兜底
  document.addEventListener("keydown", handleSave, false);

  // 策略 3: 编辑器 textarea 直连 — 编辑时焦点在此处
  const textarea = document.getElementById("editor-textarea");
  if (textarea) {
    textarea.addEventListener("keydown", handleSave);
  }

  // 策略 4: 监听 Rust 端原生菜单快捷键事件（WebView2 拦截 JS Ctrl+S 时的兜底方案）
  try {
    const tauriEvent = window.__TAURI__?.event;
    if (tauriEvent && typeof tauriEvent.listen === "function") {
      tauriEvent.listen("menu-save", () => {
        saveCurrentFile().catch((err) => setStatus(`保存失败: ${err}`, "error"));
      });
    }
  } catch {
    // 浏览器开发模式忽略
  }
}

async function saveCurrentFile() {
  const tab = state.tabs.find((t) => t.id === state.activeTabId);
  if (!tab || tab._isTerminal) return;
  if (!tab.path) {
    setStatus("无法保存: 文件路径未知", "error");
    return;
  }

  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus("Tauri API 不可用");
    return;
  }

  try {
    await invoke("write_file", { path: tab.path, content: tab.content });

    // 清除修改标记
    tab._modified = false;
    renderTabs();

    // 如果之前有语法高亮，保存后重新高亮
    if (tab._language) {
      await highlightAndRender(tab, tab._language);
    }

    setStatus(`已保存: ${tab.name}`);
  } catch (err) {
    setStatus(`保存失败: ${err}`, "error");
  }
}

// ============================================
// 帮助页
// ============================================
function setupHelpMenu() {
  const btn = document.getElementById("menu-help");
  if (!btn) return;

  btn.addEventListener("click", () => openHelp());
  btn.style.cursor = "pointer";

  // 帮助页返回按钮
  document.getElementById("btn-help-back")?.addEventListener("click", () => hideHelpPage());
}

function openHelp() {
  if (!state.currentProject) {
    showHelpPage();
  } else {
    openHelpTab();
  }
}

function showHelpPage() {
  document.getElementById("welcome-page").style.display = "none";
  document.getElementById("help-page").style.display = "";
}

function hideHelpPage() {
  document.getElementById("help-page").style.display = "none";
  document.getElementById("welcome-page").style.display = "";
}

function openHelpTab() {
  // 已存在帮助标签页则切换
  const existing = state.tabs.find((t) => t._isHelp);
  if (existing) {
    switchTab(existing.id);
    return;
  }

  const tab = {
    id: "help-" + Date.now().toString(),
    name: "帮助",
    path: "",
    content: HELP_TEXT,
    _isHelp: true,
  };
  state.tabs.push(tab);
  renderTabs();
  switchTab(tab.id);
  renderPlainCode(tab);

  const textarea = document.getElementById("editor-textarea");
  if (textarea) textarea.readOnly = true;
}

// ============================================
// 编辑器 textarea 同步
// ============================================

function setupTextareaSync() {
  const textarea = document.getElementById("editor-textarea");
  if (!textarea) return;

  let debounceTimer = null;

  // ============================================
  // 核心层：输入事件 + 防抖（1 秒后自动保存）
  // ============================================
  textarea.addEventListener("input", () => {
    const tab = state.tabs.find((t) => t.id === state.activeTabId);
    if (!tab || tab._isTerminal || tab._isHelp) return;

    const newContent = textarea.value;
    if (tab.content === newContent) return;

    tab.content = newContent;
    tab._highlighted = null;
    tab._language = null;

    if (newContent.split("\n").length <= 1000) {
      renderPlainCode(tab);
    }

    if (!tab._modified) {
      tab._modified = true;
      renderTabs();
    }

    // 防抖：重置计时器
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      doAutoSave(tab);
    }, 1000);
  });

  // ============================================
  // 边界层：失焦 / 切换标签页时立刻保存
  // ============================================
  textarea.addEventListener("blur", () => {
    clearTimeout(debounceTimer);
    const tab = state.tabs.find((t) => t.id === state.activeTabId);
    if (tab && tab._modified && !tab._isTerminal && !tab._isHelp) {
      doAutoSave(tab);
    }
  });

  // 供外部调用：切换标签页前保存当前 tab
  window._saveBeforeSwitch = () => {
    clearTimeout(debounceTimer);
    const tab = state.tabs.find((t) => t.id === state.activeTabId);
    if (tab && tab._modified && !tab._isTerminal && !tab._isHelp) {
      doAutoSave(tab);
    }
  };

  // ============================================
  // 兜底层：每 5 分钟长间隔定时器
  // ============================================
  setInterval(() => {
    const tab = state.tabs.find((t) => t.id === state.activeTabId);
    if (tab && tab._modified && !tab._isTerminal && !tab._isHelp) {
      doAutoSave(tab);
    }
  }, 5 * 60 * 1000);
}

async function doAutoSave(tab) {
  if (!tab || !tab.path || !tab._modified) return;
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    await invoke("write_file", { path: tab.path, content: tab.content });
    tab._modified = false;
    renderTabs();
    // 后台静默保存不弹提示，但保存失败时提示
  } catch {
    // 静默失败，定时器下次会重试
  }
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
  const help = document.getElementById("help-page");
  if (welcome) welcome.style.display = "";
  if (project) project.style.display = "none";
  if (help) help.style.display = "none";

  // 清空标签页和编辑器
  state.tabs = [];
  state.activeTabId = null;
  renderTabs();
  hideEditor();
}

function showProjectWorkspace() {
  const welcome = document.getElementById("welcome-page");
  const project = document.getElementById("project-workspace");
  const help = document.getElementById("help-page");
  if (welcome) welcome.style.display = "none";
  if (project) project.style.display = "";
  if (help) help.style.display = "none";

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
    const result = await invoke("run_target", { cmd, projectRoot: state.currentProject?.path });

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
// 终端资源列表
// ============================================

function setupTerminalList() {
  const list = document.getElementById("terminal-list");
  if (!list) return;

  // 事件委托：在 <ul> 上统一监听
  list.addEventListener("click", (e) => {
    const li = e.target.closest("li");
    if (!li) return;

    const nameEl = li.querySelector(".terminal-name");
    const name = nameEl ? nameEl.textContent.trim() : "";
    const cmd = li.dataset.cmd;
    if (!cmd) return;

    // 点击 🪟 图标 → 新窗口打开
    if (e.target.closest(".terminal-new-window")) {
      const windowCmd = li.dataset.cmdWindow || cmd;
      spawnInNewWindow(name, windowCmd);
      return;
    }

    // 点击文字 → 模拟终端（PTY）
    spawnTerminal(name, cmd);
  });
}

/**
 * 在新操作系统窗口中启动终端程序（不经过 PTY）
 */
async function spawnInNewWindow(name, cmd) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus("Tauri API 不可用");
    return;
  }

  try {
    await invoke("spawn_terminal", { cmd, projectRoot: state.currentProject?.path });
    setStatus(`${name} 已在新窗口中启动`);
  } catch (err) {
    setStatus(`启动失败: ${err}`, "error");
  }
}

async function spawnTerminal(name, cmd) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus("Tauri API 不可用");
    return;
  }

  if (typeof Terminal === "undefined") {
    setStatus("xterm.js 未加载", "error");
    return;
  }

  const tabId = "term-" + Date.now().toString();

  const tab = {
    id: tabId,
    name,
    path: "",
    content: cmd,
    _isTerminal: true,
  };
  state.tabs.push(tab);
  renderTabs();
  switchTab(tabId);

  // 隐藏编辑器，显示终端容器
  showTerminalView();

  setStatus(`正在启动 ${name}...`);

  try {
    // 启动 PTY
    await invoke("pty_spawn", { cmd, tabId, projectRoot: state.currentProject?.path });

    // 创建 xterm.js 终端
    const term = new Terminal({
      rows: 24,
      cols: 100,
      cursorBlink: true,
      fontFamily: '"Cascadia Code", "Fira Code", "JetBrains Mono", "Consolas", monospace',
      fontSize: 13,
      theme: {
        background: "#1e1e1e",
        foreground: "#d4d4d4",
        cursor: "#ffffff",
        selectionBackground: "#264f78",
      },
    });

    const container = document.getElementById("terminal-container");
    container.innerHTML = "";
    term.open(container);
    term.focus();

    // 保存 xterm 实例到 tab
    tab._term = term;

    // 用户输入 → PTY
    term.onData((data) => {
      invoke("pty_write", { tabId, data }).catch(() => {});
    });

    // PTY 输出 → 终端显示
    const unlisten = await listenToPty(tabId, term);

    // PTY 退出
    const unlistenExit = await listenToPtyExit(tabId, term, name);

    tab._ptyUnlisten = () => { unlisten(); unlistenExit(); };

    setStatus(`${name} 已启动`);
  } catch (err) {
    setStatus(`启动失败: ${err}`, "error");
  }
}

/** 监听 PTY 输出事件 */
async function listenToPty(tabId, term) {
  const eventName = `pty-out-${tabId}`;
  // Tauri 2 事件监听
  if (window.__TAURI__ && window.__TAURI__.event) {
    return window.__TAURI__.event.listen(eventName, (event) => {
      term.write(event.payload);
    });
  }
  // fallback: 使用 core.invoke 轮询 (shouldn't happen)
  return () => {};
}

/** 监听 PTY 退出事件 */
async function listenToPtyExit(tabId, term, name) {
  const eventName = `pty-exit-${tabId}`;
  if (window.__TAURI__ && window.__TAURI__.event) {
    return window.__TAURI__.event.listen(eventName, () => {
      term.write(`\r\n[${name} 已退出]\r\n`);
    });
  }
  return () => {};
}

function showTerminalView() {
  document.getElementById("editor-empty").style.display = "none";
  document.getElementById("editor-view").style.display = "none";
  document.getElementById("terminal-view").style.display = "";
}

function hideTerminalView() {
  document.getElementById("terminal-view").style.display = "none";
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
  icon.textContent = entry.is_dir ? "📁" : fileIcon(entry.name);
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
  if (!invoke) {
    setStatus("Tauri API 不可用");
    return;
  }

  try {
    const lastPath = await invoke("get_last_project");
    if (!lastPath) {
      setStatus("没有上次项目记录");
      return;
    }
    setStatus(`正在恢复项目: ${lastPath}`);
    await openProject(lastPath);
  } catch (err) {
    setStatus(`恢复项目失败: ${err}`, "error");
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
