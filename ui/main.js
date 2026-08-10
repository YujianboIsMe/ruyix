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
  // 初始化多语言
  await I18N.init();

  // 设置语言菜单
  setupLanguageMenu();

  // 设置右键菜单
  setupContextMenu();

  // 初始状态：无项目，导航区显示项目列表
  showWelcomePage();

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
    setStatus(I18N.t("project.restore_fail", { err }));
  }
}

// ============================================
// 语言菜单
// ============================================

function setupLanguageMenu() {
  const menuLang = document.getElementById("menu-lang");
  const dropdown = document.getElementById("menu-lang-dropdown");
  if (!menuLang || !dropdown) return;

  menuLang.addEventListener("click", (e) => {
    e.stopPropagation();
    dropdown.style.display = dropdown.style.display === "none" ? "" : "none";
  });

  document.addEventListener("click", () => {
    dropdown.style.display = "none";
  });

  dropdown.querySelectorAll("[data-lang]").forEach((item) => {
    item.addEventListener("click", async () => {
      await I18N.setLang(item.dataset.lang);
      dropdown.style.display = "none";
      refreshI18nUI();
    });
  });
}

/**
 * 语言切换后刷新 UI 中所有可翻译内容
 */
function refreshI18nUI() {
  // 状态栏
  setStatus(I18N.t("statusbar.ready"));

  // 标题栏（无项目时）
  if (!state.currentProject) {
    updateTitlebarTitle();
  }

  // 欢迎页
  const titleEl = document.querySelector(".welcome-title");
  const subtitleEl = document.querySelector(".welcome-subtitle");
  if (titleEl) titleEl.textContent = I18N.t("welcome.title");
  if (subtitleEl) subtitleEl.textContent = I18N.t("welcome.subtitle");

  // 编辑器空状态
  const emptyEl = document.querySelector("#editor-empty p");
  if (emptyEl) emptyEl.textContent = I18N.t("editor.empty");

  // 大纲头部
  const outlineHeader = document.querySelector(".outline-header");
  if (outlineHeader) outlineHeader.textContent = I18N.t("outline.header");

  // 命令栏 placeholder
  const cmdInput = document.getElementById("command-input");
  if (cmdInput) cmdInput.placeholder = I18N.t("command.prompt");

  // 导航标签
  const tabProjects = document.querySelector('.nav-tab[data-tab="projects"]');
  const tabFiles = document.querySelector('.nav-tab[data-tab="files"]');
  const tabTerminal = document.querySelector('.nav-tab[data-tab="terminal"]');
  const tabTarget = document.querySelector('.nav-tab[data-tab="target"]');
  if (tabProjects) tabProjects.textContent = I18N.t("nav.tab_projects");
  if (tabFiles) tabFiles.textContent = I18N.t("nav.tab_files");
  if (tabTerminal) tabTerminal.textContent = I18N.t("nav.tab_terminal");
  if (tabTarget) tabTarget.textContent = I18N.t("nav.tab_targets");

  // 文件树、项目列表、运行目标需要重新加载
  if (state.currentProject) {
    loadFileTree(state.currentProject.path);
    loadRunTargets();
  } else {
    loadProjectList();
  }

  // 标题栏最大化/还原按钮 title
  updateMaximizeIcon();
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
  // 全名匹配优先级（用于 .gitignore 等）
  const full = name.toLowerCase();
  if (full === ".gitignore" || full === "gitignore") return "🚫";

  const ext = name.split(".").pop()?.toLowerCase();
  const iconMap = {
    py: "🐍",
    rs: "🦀",
    html: "🌏",
    htm: "🌏",
    css: "🎨",
    js: "Ⓙ",
    mjs: "Ⓙ",
    cjs: "Ⓙ",
    md: "Ⓜ️",
    markdown: "Ⓜ️",
    png: "🖼️",
    jpg: "🖼️",
    jpeg: "🖼️",
    ico: "🖼️",
    icon: "🖼️",
    gitignore: "🚫",
    toml: "⚙️",
    json: "🧩",
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
      saveCurrentFile().catch((err) => setStatus(I18N.t("save.fail", { err }), "error"));
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
        saveCurrentFile().catch((err) => setStatus(I18N.t("save.fail", { err }), "error"));
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
    setStatus(I18N.t("save.no_path"), "error");
    return;
  }

  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_unavail"));
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

    setStatus(I18N.t("save.ok", { name: tab.name }));
  } catch (err) {
    setStatus(I18N.t("save.fail", { err }), "error");
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
  // 导航区保持可见；编辑区显示帮助内容
  const welcome = document.getElementById("welcome-content");
  const help = document.getElementById("help-page");
  const editorBody = document.getElementById("editor-body");
  if (welcome) welcome.style.display = "none";
  if (help) help.style.display = "";
  if (editorBody) editorBody.style.display = "none";
}

function hideHelpPage() {
  const help = document.getElementById("help-page");
  if (help) help.style.display = "none";
  // 恢复到适合当前项目状态的视图
  if (state.currentProject) {
    showProjectWorkspace();
  } else {
    showWelcomePage();
  }
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
    content: getHelpText(),
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
      updateOutline(tab);
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
  // project-workspace 始终可见，无需切换 display
  const welcome = document.getElementById("welcome-content");
  const help = document.getElementById("help-page");
  const editorBody = document.getElementById("editor-body");
  if (welcome) welcome.style.display = "";
  if (help) help.style.display = "none";
  if (editorBody) editorBody.style.display = "none";

  // 导航区：只显示项目列表 tab
  setNavigatorMode("projects");

  // 清空标签页和编辑器
  state.tabs = [];
  state.activeTabId = null;
  renderTabs();
  hideEditor();

  // 加载项目列表
  loadProjectList();
}

function showProjectWorkspace() {
  const welcome = document.getElementById("welcome-content");
  const help = document.getElementById("help-page");
  const editorBody = document.getElementById("editor-body");
  if (welcome) welcome.style.display = "none";
  if (help) help.style.display = "none";
  if (editorBody) editorBody.style.display = "";

  // 导航区：显示项目标签
  setNavigatorMode("project");

  // 加载项目文件树和运行目标
  if (state.currentProject) {
    loadFileTree(state.currentProject.path);
    loadRunTargets();
  }
}

/**
 * 设置导航区标签显示模式
 * @param {"projects" | "project"} mode
 */
function setNavigatorMode(mode) {
  const tabProjects = document.querySelector('.nav-tab[data-tab="projects"]');
  const tabFiles = document.querySelector('.nav-tab[data-tab="files"]');
  const tabTerminal = document.querySelector('.nav-tab[data-tab="terminal"]');
  const tabTarget = document.querySelector('.nav-tab[data-tab="target"]');

  const panelProjects = document.getElementById("nav-panel-projects");
  const panelFiles = document.getElementById("nav-panel-files");
  const panelTerminal = document.getElementById("nav-panel-terminal");
  const panelTarget = document.getElementById("nav-panel-target");

  if (mode === "projects") {
    // 显示项目列表 tab，隐藏其他
    if (tabProjects) tabProjects.style.display = "";
    if (tabFiles) tabFiles.style.display = "none";
    if (tabTerminal) tabTerminal.style.display = "none";
    if (tabTarget) tabTarget.style.display = "none";

    // 停用所有 tab/panel，激活项目列表
    document.querySelectorAll(".nav-tab").forEach(t => t.classList.remove("active"));
    document.querySelectorAll(".nav-panel").forEach(p => p.classList.remove("active"));
    if (tabProjects) tabProjects.classList.add("active");
    if (panelProjects) panelProjects.classList.add("active");
  } else {
    // 显示项目相关 tab，隐藏项目列表 tab
    if (tabProjects) tabProjects.style.display = "none";
    if (tabFiles) tabFiles.style.display = "";
    if (tabTerminal) tabTerminal.style.display = "";
    if (tabTarget) tabTarget.style.display = "";

    // 停用所有 tab/panel，激活文件 tab
    document.querySelectorAll(".nav-tab").forEach(t => t.classList.remove("active"));
    document.querySelectorAll(".nav-panel").forEach(p => p.classList.remove("active"));
    if (tabFiles) tabFiles.classList.add("active");
    if (panelFiles) panelFiles.classList.add("active");
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

      // 切换到项目列表标签时刷新
      if (tab.dataset.tab === "projects") {
        loadProjectList();
      }
    });
  });
}

// ============================================
// 右键菜单
// ============================================

let _ctxPath = null;
let _ctxIsDir = false;

function setupContextMenu() {
  const menu = document.getElementById("context-menu");
  const fileTree = document.getElementById("file-tree");
  if (!menu || !fileTree) return;

  // 禁止浏览器默认右键菜单（仅文件树区域）
  fileTree.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    const node = e.target.closest(".tree-node");
    if (!node) return;
    _ctxPath = node.dataset.path;
    _ctxIsDir = node.dataset.isDir === "true";
    showContextMenu(menu, e.clientX, e.clientY, _ctxIsDir);
  });

  // 菜单项点击
  menu.addEventListener("click", async (e) => {
    const item = e.target.closest(".context-menu-item");
    if (!item || item.classList.contains("context-menu-sep")) return;
    const action = item.dataset.action;
    hideContextMenu(menu);
    if (!_ctxPath) return;

    switch (action) {
      case "delete":
        await deleteFileOrFolder(_ctxPath);
        break;
      case "rename":
        await renameFileOrFolder(_ctxPath);
        break;
      case "newfile":
        await createFileInFolder(_ctxPath);
        break;
      case "run":
        await handleContextRun(_ctxPath);
        break;
      case "try-run":
        await handleContextTryRun(_ctxPath);
        break;
    }
  });

  // 点击外部关闭
  document.addEventListener("click", () => hideContextMenu(menu));
}

async function showContextMenu(menu, x, y, isDir) {
  // 文件夹 vs 文件
  menu.querySelectorAll(".context-menu-folder-only").forEach(el => el.style.display = isDir ? "" : "none");
  const fileItems = menu.querySelectorAll(".context-menu-file-only");
  const runItem = menu.querySelector('[data-action="run"]');
  const tryRunItem = menu.querySelector('[data-action="try-run"]');

  if (isDir) {
    fileItems.forEach(el => el.style.display = "none");
  } else if (_ctxPath && state.currentProject) {
    // 查询该文件的执行状态
    try {
      const invoke = getTauriInvoke();
      if (invoke) {
        const status = await invoke("get_execute_status", {
          path: _ctxPath,
          projectRoot: state.currentProject.path
        });
        if (status.known === true) {
          if (runItem) runItem.style.display = "";
          if (tryRunItem) tryRunItem.style.display = "none";
        } else if (status.known === false) {
          fileItems.forEach(el => el.style.display = "none");
        } else {
          // unknown
          if (runItem) runItem.style.display = "none";
          if (tryRunItem) tryRunItem.style.display = "";
        }
      }
    } catch {
      fileItems.forEach(el => el.style.display = "none");
    }
  }

  menu.style.left = x + "px";
  menu.style.top = y + "px";
  menu.style.display = "";
}

/** 右键"运行"：已有目标→执行，无目标→创建 */
async function handleContextRun(fullPath) {
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    const status = await invoke("get_execute_status", {
      path: fullPath,
      projectRoot: state.currentProject?.path
    });
    if (status.has_target && status.target_name) {
      // 情况1: 已有目标 → 执行
      const targets = await invoke("get_run_targets", { projectRoot: state.currentProject?.path });
      const target = targets.find(t => (t.name || t.key) === status.target_name);
      if (target && target.cmd) {
        await runTargetCmd(status.target_name, target.cmd);
      }
    } else {
      // 情况2: 无目标 → 自动创建
      const name = fullPath.split(/[/\\]/).pop() || fullPath;
      const ext = name.split(".").pop() || "";
      // 尝试从 execute.toml 获取命令模板（AI 试跑时可能已存储）
      // 默认: 用扩展名推测
      const cmdMap = { py: "python {file}", rs: "cargo run", js: "node {file}" };
      const cmd = (cmdMap[ext] || "python {file}").replace("{file}", fullPath);
      await handleCommand("run " + name + "=" + cmd, true);
    }
  } catch (err) {
    setStatus("运行失败: " + err, "error");
  }
}

/** 右键"试跑"：询问 LLM 是否可以运行 */
async function handleContextTryRun(fullPath) {
  setStatus(I18N.t("status.thinking"));
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    const result = await invoke("ai_execute_check", { path: fullPath });
    if (!result || !result.trim()) {
      setStatus("试跑失败: AI 返回为空，请重试", "error");
      return;
    }
    const ext = (fullPath.split(".").pop() || "").toLowerCase();
    const upper = result.trim().toUpperCase();

    if (upper.startsWith("YES")) {
      // 可运行 — 提取命令模板
      let cmdTemplate = upper.startsWith("YES|") ? result.slice(result.indexOf("|") + 1).trim() : ("python {file}");
      // 替换占位符
      const cmd = cmdTemplate.replace(/\{file\}/gi, fullPath);
      const name = fullPath.split(/[/\\]/).pop() || fullPath;
      // 记录 + 自动创建运行目标
      await invoke("set_execute_entry", { ext, canRun: true });
      await handleCommand("run " + name + "=" + cmd, true);
      setStatus("已记住并创建运行目标: ." + ext + " → " + cmd);
    } else if (upper.startsWith("CONDITIONAL")) {
      const detail = upper.startsWith("CONDITIONAL|") ? result.slice(result.indexOf("|") + 1).trim() : result;
      setStatus("功能待开发: " + detail, "error");
    } else {
      // NO 或其他
      if (ext) await invoke("set_execute_entry", { ext, canRun: false });
      setStatus("已记住: ." + ext + " 不可运行 (AI: " + result.slice(0, 60) + ")");
    }
  } catch (err) {
    setStatus("试跑失败: " + err, "error");
  }
}

/** 全路径 → 相对于项目根的路径 */
function toRelativePath(fullPath) {
  if (!state.currentProject) return fullPath;
  const root = state.currentProject.path + '\\';
  return fullPath.startsWith(root) ? fullPath.slice(root.length) : fullPath;
}

function hideContextMenu(menu) {
  menu.style.display = "none";
}

// ============================================
// 自定义弹窗（替换浏览器 confirm / prompt）
// ============================================

/**
 * 确认弹窗，返回 true/false
 */
function showConfirm(title, message) {
  return new Promise((resolve) => {
    const overlay = document.getElementById("modal-overlay");
    const titleEl = document.getElementById("modal-title");
    const bodyEl = document.getElementById("modal-body");
    const inputRow = document.getElementById("modal-input-row");
    const okBtn = document.getElementById("modal-btn-ok");
    const cancelBtn = document.getElementById("modal-btn-cancel");

    titleEl.textContent = title;
    bodyEl.textContent = message;
    inputRow.style.display = "none";
    okBtn.textContent = I18N.t("modal.ok") || "确认";
    cancelBtn.textContent = I18N.t("modal.cancel") || "取消";
    overlay.style.display = "";

    const cleanup = () => { overlay.style.display = "none"; };
    const onOk = () => { cleanup(); resolve(true); };
    const onCancel = () => { cleanup(); resolve(false); };

    okBtn.onclick = onOk;
    cancelBtn.onclick = onCancel;
    overlay.onclick = (e) => { if (e.target === overlay) onCancel(); };

    const onKey = (e) => {
      if (e.key === "Escape") { document.removeEventListener("keydown", onKey); onCancel(); }
      if (e.key === "Enter") { document.removeEventListener("keydown", onKey); onOk(); }
    };
    document.addEventListener("keydown", onKey);

    document.getElementById("modal-input")?.blur();
    okBtn.focus();
  });
}

/**
 * 输入弹窗，返回输入的字符串或 null
 */
function showPrompt(title, defaultValue) {
  return new Promise((resolve) => {
    const overlay = document.getElementById("modal-overlay");
    const titleEl = document.getElementById("modal-title");
    const bodyEl = document.getElementById("modal-body");
    const inputRow = document.getElementById("modal-input-row");
    const inputEl = document.getElementById("modal-input");
    const okBtn = document.getElementById("modal-btn-ok");
    const cancelBtn = document.getElementById("modal-btn-cancel");

    titleEl.textContent = title;
    bodyEl.textContent = "";
    inputRow.style.display = "";
    inputEl.value = defaultValue || "";
    inputEl.select();
    okBtn.textContent = I18N.t("modal.ok") || "确认";
    cancelBtn.textContent = I18N.t("modal.cancel") || "取消";
    overlay.style.display = "";

    const cleanup = () => { overlay.style.display = "none"; };
    const onOk = () => { cleanup(); resolve(inputEl.value.trim() || null); };
    const onCancel = () => { cleanup(); resolve(null); };

    okBtn.onclick = onOk;
    cancelBtn.onclick = onCancel;
    overlay.onclick = (e) => { if (e.target === overlay) onCancel(); };

    const onKey = (e) => {
      if (e.key === "Escape") { document.removeEventListener("keydown", onKey); onCancel(); }
      if (e.key === "Enter") { document.removeEventListener("keydown", onKey); onOk(); }
    };
    document.addEventListener("keydown", onKey);

    inputEl.focus();
  });
}

async function deleteFileOrFolder(fullPath) {
  const ok = await showConfirm('删除', I18N.t('cmd.delete.confirm', { path: fullPath }));
  if (!ok) return;
  await handleCommand('del ' + toRelativePath(fullPath), true);
}

async function renameFileOrFolder(fullPath) {
  const oldName = fullPath.split(/[/\\]/).pop() || fullPath;
  const newName = await showPrompt('重命名', oldName);
  if (!newName || newName === oldName) return;
  await handleCommand('rename ' + toRelativePath(fullPath) + ' ' + newName, true);
}

async function createFileInFolder(fullPath) {
  const name = await showPrompt('新建文件', '');
  if (!name) return;
  await handleCommand('new file ' + toRelativePath(fullPath) + '\\' + name, true);
}

// ============================================
// 项目列表（无项目时显示在导航区）
// ============================================

async function loadProjectList() {
  const list = document.getElementById("project-list");
  if (!list) return;

  const invoke = getTauriInvoke();
  if (!invoke) {
    list.innerHTML = '<span class="project-list-empty">Tauri API 不可用</span>';
    return;
  }

  try {
    const projects = await invoke("get_projects");

    if (!projects || projects.length === 0) {
      list.innerHTML = '<span class="project-list-empty">暂无历史项目<br>请使用 open project &lt;路径&gt; 打开项目</span>';
      return;
    }

    list.innerHTML = projects
      .map((path) => {
        const name = path.split(/[/\\]/).pop() || path;
        return `
        <div class="project-list-item" data-path="${escapeHtml(path)}">
          <span class="project-icon">📁</span>
          <div class="project-info">
            <div class="project-name">${escapeHtml(name)}</div>
            <div class="project-path">${escapeHtml(path)}</div>
          </div>
        </div>`;
      })
      .join("");

    // 点击打开项目
    list.querySelectorAll(".project-list-item").forEach((el) => {
      el.addEventListener("click", () => {
        const path = el.dataset.path;
        if (path) openProject(path);
      });
    });
  } catch (err) {
    list.innerHTML = `<span class="project-list-empty">加载失败: ${err}</span>`;
  }
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
    setStatus(I18N.t("status.tauri_unavail"));
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
    setStatus(I18N.t("run.executing", { name }));
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
    setStatus(I18N.t("run.done", { name, code: result.exit_code ?? "?" }));
  } catch (err) {
    renderTerminalOutput(tab, `> ${cmd}\n\n[错误] ${err}`);
    setStatus(I18N.t("run.fail", { err }), "error");
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
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  try {
    await invoke("spawn_terminal", { cmd, projectRoot: state.currentProject?.path });
    setStatus(I18N.t("terminal.spawned", { name }));
  } catch (err) {
    setStatus(I18N.t("terminal.spawn_fail", { err }), "error");
  }
}

async function spawnTerminal(name, cmd) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  if (typeof Terminal === "undefined") {
    setStatus(I18N.t("terminal.xterm_missing"), "error");
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

  setStatus(I18N.t("terminal.starting", { name }));

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

    setStatus(I18N.t("terminal.started", { name }));
  } catch (err) {
    setStatus(I18N.t("terminal.spawn_fail", { err }), "error");
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

  // 图标（目录用单图标，文件用文件图标）
  const icon = document.createElement("span");
  if (entry.is_dir) {
    icon.className = "tree-icon tree-icon--folder";
    icon.textContent = "➡️";
  } else {
    icon.className = "tree-icon tree-icon--file";
    icon.textContent = fileIcon(entry.name);
  }
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
    // 走命令系统：计算相对路径，统一入口
    const relPath = toRelativePath(entry.path);
    node.addEventListener("dblclick", () => handleCommand("open file " + relPath));
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
  const icon = node.querySelector(".tree-icon--folder");
  // 用 data-expanded 追踪展开状态
  const expanded = node.dataset.expanded === "true";

  // 如果已经展开，折叠
  if (expanded) {
    node.dataset.expanded = "false";
    if (icon) icon.textContent = "➡️";
    childrenContainer.style.display = "none";
    return;
  }

  // 展开：先显示加载中
  node.dataset.expanded = "true";

  // 懒加载子节点
  let hasChildren = childrenContainer.children.length > 0;
  if (!hasChildren) {
    const invoke = getTauriInvoke();
    if (!invoke) return;

    try {
      const entries = await invoke("list_dir", { path: entry.path });
      if (entries.length > 0) {
        for (const child of entries) {
          renderTreeEntry(child, childrenContainer, depth + 1);
        }
        hasChildren = true;
      }
    } catch {
      const err = document.createElement("span");
      err.className = "file-tree-placeholder";
      err.textContent = I18N.t("filetree.error");
      childrenContainer.appendChild(err);
    }
  }

  // 根据子节点情况设置图标
  if (icon) {
    icon.textContent = hasChildren ? "⬇️" : "🈳";
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
    // 无 Tauri API → 直接显示项目列表模式
    showWelcomePage();
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  try {
    const lastPath = await invoke("get_last_project");
    if (!lastPath) {
      showWelcomePage();
      setStatus(I18N.t("project.welcome"));
      return;
    }
    setStatus(I18N.t("project.restoring", { path: lastPath }));
    await openProject(lastPath);
  } catch (err) {
    showWelcomePage();
    setStatus(I18N.t("project.restore_fail", { err }), "error");
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
