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

// 面板模块（session / mcp / a2a / capability / config）读的是 window.state；
// 顶层 const 只进全局词法环境、不会挂到 window 上，这里显式导出，别删。
window.state = state;

// ============================================
// 初始化
// ============================================

async function initApp() {
  // 初始化多语言
  await I18N.init();

  // 应用已保存的语言（翻译 HTML 中的硬编码文案）
  refreshI18nUI();

  // 设置语言菜单
  setupMenuBar();
  setupConfigMenu();

  // 设置右键菜单
  setupContextMenu();

  // 初始状态：无项目，导航区显示项目列表
  showWelcomePage();

  setupWindowControls();
  setupCommandBar();
  setupResponsiveTitlebar();
  setupProjectSwitcher();
  setupNavigatorTabs();

  // 会话（多会话对话模型）：nav 会话列表 + 中央聊天 tab，逻辑自持在 session.js
  window.SessionUI?.attach();
  window.McpUI?.attach();
  window.A2aUI?.attach();
  window.ToolsUI?.attach();
  window.SkillsUI?.attach();
  window.ConfigUI?.attach();
  window.ServiceUI?.attach();
  setupCapabilityMenu();
  setupOutlineTabs();
  setupTextareaSync();
  setupTerminalList();
  setupKeyboardShortcuts();
  setupHelpMenu();
  setupServiceMenu();
  try {
    await autoOpenLastProject();
  } catch (err) {
    setStatus(I18N.t("project.restore_fail", { err }));
  }
}

// ============================================
// 语言菜单
// ============================================

/**
 * 菜单栏统一设置：所有带下拉菜单的 .menu-item 统一处理 toggle 行为
 */
function setupMenuBar() {
  const menuItems = document.querySelectorAll(".menu-item");
  menuItems.forEach((menu) => {
    const dropdown = menu.querySelector(".menu-dropdown");
    if (!dropdown) return;

    menu.addEventListener("click", (e) => {
      e.stopPropagation();
      // 点击下拉项时：不切换本下拉（子项自行处理关闭），只关闭其他下拉
      const onItem = !!e.target.closest(".menu-dropdown-item");
      document.querySelectorAll(".menu-dropdown").forEach((d) => {
        if (d !== dropdown) d.style.display = "none";
      });
      if (!onItem) {
        dropdown.style.display = dropdown.style.display === "none" ? "" : "none";
      }
    });
  });

  // 点击菜单外关闭所有下拉
  document.addEventListener("click", () => {
    document.querySelectorAll(".menu-dropdown").forEach((d) => {
      d.style.display = "none";
    });
  });

  // 语言切换
  const langDropdown = document.getElementById("menu-lang-dropdown");
  if (langDropdown) {
    langDropdown.querySelectorAll("[data-lang]").forEach((item) => {
      item.addEventListener("click", async () => {
        await I18N.setLang(item.dataset.lang);
        langDropdown.style.display = "none";
        refreshI18nUI();
        updateLangMenu();
      });
    });
    // 初始状态：当前语言项显示 ✔️
    updateLangMenu();
  }

  // 项目菜单项点击：统一走命令系统
  const projectDropdown = document.getElementById("menu-project-dropdown");
  if (projectDropdown) {
    projectDropdown.querySelectorAll("[data-action]").forEach((item) => {
      item.addEventListener("click", async () => {
        projectDropdown.style.display = "none";
        const action = item.dataset.action;
        if (action === "close-project") {
          await handleCommand("close project");
        } else if (action === "new-project") {
          const path = await pickFolder(I18N.t("project.pick_folder"));
          if (path) await handleCommand("open project " + path);
        } else if (action === "migrate-config") {
          await handleCommand("project migrate");
        }
      });
    });
  }

  updateProjectMenu();
}

/**
 * 配置菜单：全局 / 项目 / 运行 → 中央编辑区打开配置表单标签
 * （表单本体在 config.js：扫描配置项 → 表单 + 保存/应用/取消）
 */
function setupConfigMenu() {
  const dropdown = document.getElementById("menu-config-dropdown");
  if (!dropdown) return;
  dropdown.querySelectorAll("[data-config-scope]").forEach((item) => {
    item.addEventListener("click", async () => {
      dropdown.style.display = "none";
      await handleCommand("config form " + item.dataset.configScope);
    });
  });
}

/** 打开（或复用）配置表单标签 */
async function openConfigTab(scope) {
  return window.ConfigUI?.open(scope);
}

/** 保存配置表单（Ctrl+S）；应用/取消走各自按钮 */
async function saveConfigTab() {
  return window.ConfigUI?.save();
}

/**
 * 语言菜单选中标记：当前语言项后面显示 ✔️
 */
function updateLangMenu() {
  const current = I18N.getLang();
  document.querySelectorAll("#menu-lang-dropdown [data-lang]").forEach((item) => {
    const check = item.querySelector(".lang-check");
    if (check) check.style.display = item.dataset.lang === current ? "" : "none";
  });
}

/**
 * 根据项目打开状态切换"项目"菜单的子项显示
 */
function updateProjectMenu() {
  const newItem = document.querySelector('[data-action="new-project"]');
  const closeItem = document.querySelector('[data-action="close-project"]');
  if (!newItem || !closeItem) return;

  if (state.currentProject) {
    newItem.style.display = "none";
    closeItem.style.display = "";
  } else {
    newItem.style.display = "";
    closeItem.style.display = "none";
  }
}

// 欢迎页加载令牌：防止并发加载时旧请求覆盖新内容
let _welcomeLoadToken = 0;

/**
 * 按当前语言加载欢迎页内容（welcome-zh.html / welcome-en.html）
 * 独立文件便于维护；失败时保留现有内容
 */
async function loadWelcome() {
  const lang = I18N.getLang() || "zh-CN";
  const file = lang === "en" ? "welcome-en.html" : "welcome-zh.html";
  const token = ++_welcomeLoadToken;

  try {
    const base = window.location.origin || "https://ruyix.localhost";
    const resp = await fetch(`${base}/${file}`);
    if (!resp.ok) return;
    const html = await resp.text();
    if (token !== _welcomeLoadToken) return; // 已有更新的加载请求，丢弃本次结果
    const container = document.getElementById("welcome-content");
    if (container) container.innerHTML = html;
  } catch {
    // 加载失败：保留现有内容
  }
}

/**
 * 语言切换后刷新 UI 中所有可翻译内容
 */
function refreshI18nUI() {
  // ============================================
  // 通用：[data-i18n] 属性驱动
  // ============================================
  document.querySelectorAll("[data-i18n]").forEach((el) => {
    const key = el.dataset.i18n;
    if (!key) return;
    // 仅当元素没有子元素节点时设置 textContent
    // （有子元素的由子元素各自的 data-i18n 处理）
    const hasElementChild = Array.from(el.childNodes).some(
      (n) => n.nodeType === Node.ELEMENT_NODE
    );
    if (!hasElementChild) {
      el.textContent = I18N.t(key);
    }
  });

  // 状态栏
  setStatus(I18N.t("statusbar.ready"));

  // 标题栏（无项目时）
  if (!state.currentProject) {
    updateTitlebarTitle();
  }

  // 欢迎页：按当前语言加载对应文件（welcome-zh.html / welcome-en.html）
  loadWelcome();

  // 帮助页：正文是 markdown 源文件，切语言后若正显示则重新渲染
  const helpPage = document.getElementById("help-page");
  if (helpPage && helpPage.style.display !== "none") showHelpPage();
  // 帮助标签页的标题随语言走
  const helpTab = state.tabs.find((t) => t._isHelp);
  if (helpTab) {
    helpTab.name = I18N.t("help.title");
    renderTabs();
  }

  // 服务标签页：标题随语言走，表头/按钮也要重新过一遍 i18n
  const serviceTab = state.tabs.find((t) => t._isService);
  if (serviceTab) {
    serviceTab.name = I18N.t("service.title");
    renderTabs();
    window.ServiceUI?.render(serviceTab);
  }

  // 编辑器空状态
  const emptyEl = document.querySelector("#editor-empty p");
  if (emptyEl) emptyEl.textContent = I18N.t("editor.empty");

  // 输入框 placeholder（[data-i18n-placeholder] 属性驱动）
  document.querySelectorAll("[data-i18n-placeholder]").forEach((el) => {
    el.placeholder = I18N.t(el.dataset.i18nPlaceholder);
  });

  // title 提示（[data-i18n-title] 属性驱动）
  document.querySelectorAll("[data-i18n-title]").forEach((el) => {
    el.title = I18N.t(el.dataset.i18nTitle);
  });

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
    loadGitStatus();
  } else {
    loadProjectList();
  }

  // 标题栏最大化/还原按钮 title
  updateMaximizeIcon();

  // 项目菜单状态
  updateProjectMenu();
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

/**
 * 系统原生文件夹选择框（tauri-plugin-dialog）。
 * 返回所选路径；取消 / 无 Tauri / 插件异常时返回 null，
 * 命令栏 `open project <路径>` 仍是手动入口。
 */
async function pickFolder(title) {
  const invoke = getTauriInvoke();
  if (!invoke) return null;
  try {
    const picked = await invoke("plugin:dialog|open", {
      options: { directory: true, multiple: false, title },
    });
    if (Array.isArray(picked)) return picked[0] ?? null;
    return picked ?? null;
  } catch {
    return null;
  }
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

/** 标签页图标：专用视图优先，其余按文件名 */
function tabIcon(t) {
  if (t._isConfig) return "⚙️";
  if (t._isHelp) return "🔒";
  if (t._isSession) return "🤖";
  if (t._isTerminal) return "🖥️";
  if (t._isService) return "🔌";
  return fileIcon(t.name);
}

function renderTabs() {
  const bar = document.getElementById("tab-bar");
  if (!bar) return;

  bar.innerHTML = state.tabs
    .map((t) => {
      const icon = tabIcon(t);
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
  // 边界层：切换标签页前保存当前 tab；
  // 配置表单没有落盘路径，改为把编辑值暂存进标签（切回来不丢）
  if (window._saveBeforeSwitch) window._saveBeforeSwitch();
  window.ConfigUI?.stash();

  state.activeTabId = tabId;
  renderTabs();

  const tab = state.tabs.find((t) => t.id === tabId);
  if (!tab) return;

  showEditor();
  hideTerminalView();
  hideImageView();
  hideConfigView();
  hideServiceView();
  // 服务面板的秒级刷新随面板走：切走了就别再问后端
  window.ServiceUI?.close();
  if (tab._isConfig) {
    // 配置标签页 — 表单视图（扫描配置项 → 表单 + 保存/应用/取消）
    hideEditorView();
    hideSessionView();
    showConfigView();
    window.ConfigUI?.render(tab);
    return;
  }
  if (tab._isSession) {
    // 会话标签页 — 聊天 DOM 懒构建并移入容器
    hideEditorView();
    hideTerminalView();
    hideImageView();
    showSessionView();
    const container = document.getElementById("session-container");
    const chatEl = window.SessionUI?.ensureChatEl(tab);
    if (container && chatEl) {
      container.innerHTML = "";
      container.appendChild(chatEl);
      chatEl.querySelector(".session-msgs")?.scrollTo(0, 1e9);
    }
    // 进入会话即聚焦输入框（否则按键会落到底部命令栏）
    chatEl?.querySelector(".session-input")?.focus();
    // 大纲区显示该会话的任务计划（模型返回计划后：✅完成 ⌛等待 ⛏️进行中）
    window.SessionUI?.renderOutline(tab._session);
    return;
  }
  hideSessionView();
  if (tab._isHelp) {
    // 帮助标签页 — markdown 文档视图（不走代码编辑器，content 恒为空）
    hideEditorView();
    showHelpPage();
    return;
  }
  if (tab._isService) {
    // 服务标签页 — 托管进程面板（自己拉 proc_list，content 恒为空）
    hideEditorView();
    showServiceView();
    window.ServiceUI?.render(tab);
    return;
  }
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
  if (tab._isImage) {
    hideEditorView();
    showImageView();
    renderImagePreview(tab);
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
  if (tab._runTarget) {
    const textarea = document.getElementById("editor-textarea");
    if (textarea) textarea.readOnly = true;
  }
  updateOutline(tab);

  // Git 面板可见时刷新状态（文件可能刚被修改/保存）
  const gitPanel = document.getElementById("git-panel");
  if (gitPanel && gitPanel.style.display !== "none" && state.currentProject) {
    loadGitStatus();
  }
}

function closeTab(tabId) {
  const idx = state.tabs.findIndex((t) => t.id === tabId);
  if (idx === -1) return;

  const tab = state.tabs[idx];

  // 服务面板：停掉秒级刷新（否则关了标签页还在后台问后端）
  if (tab._isService) window.ServiceUI?.close();

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
    // 无项目时没有"编辑器空态"可回（欢迎页盖着编辑区），回到欢迎页
    if (!state.currentProject) showWelcomePage();
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
  // 编辑区上层视图归位：显示标签页 = 欢迎页/帮助页/Agent 控制台都让位
  // （标签栏在 editor-body 里，无项目状态下它整体隐藏，必须先放出来）
  const welcome = document.getElementById("welcome-content");
  const help = document.getElementById("help-page");
  const agent = document.getElementById("agent-page");
  const editorBody = document.getElementById("editor-body");
  if (welcome) welcome.style.display = "none";
  if (help) help.style.display = "none";
  if (agent) agent.style.display = "none";
  if (editorBody) editorBody.style.display = "";
  document.getElementById("editor-empty").style.display = "none";
  document.getElementById("editor-view").style.display = "";
}

function hideEditor() {
  document.getElementById("editor-empty").style.display = "";
  document.getElementById("editor-view").style.display = "none";
  document.getElementById("terminal-view").style.display = "none";
  document.getElementById("image-view").style.display = "none";
  document.getElementById("session-view").style.display = "none";
  const serviceView = document.getElementById("service-view");
  if (serviceView) serviceView.style.display = "none";
  document.getElementById("editor-gutter").innerHTML = "";
  document.getElementById("editor-code-backdrop").innerHTML = "";
  document.getElementById("editor-textarea").value = "";
}

async function highlightAndRender(tab, language) {
  const invoke = getTauriInvoke();
  if (!invoke) return;

  try {
    // 快照：请求返回时若内容已变化，丢弃过期的高亮结果
    const snapshot = tab.content;
    const lines = await invoke("highlight_code", { language, code: snapshot });
    if (tab.content !== snapshot) return;
    tab._highlighted = lines;
    tab._language = language;
    if (tab.id === state.activeTabId) {
      renderHighlightedCode(tab);
    }
  } catch {
    // 高亮失败：仅当该标签页仍处于激活状态时才渲染纯文本
    if (tab.id === state.activeTabId) {
      renderPlainCode(tab);
    }
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
    sql: "sql",
    java: "java",
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
    sql: "🛢️",
    java: "☕",
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
  } else if (ext === "java") {
    items = parseJavaOutline(tab.content);
  } else {
    content.innerHTML = `<div class="outline-placeholder">${I18N.t("outline.placeholder")}</div>`;
    return;
  }

  if (items.length === 0) {
    content.innerHTML = `<div class="outline-placeholder">${I18N.t("outline.none")}</div>`;
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

/** Java 类级别名（修饰符 + 声明的组合） */
const JAVA_MODIFIER =
  "(?:public|protected|private|static|final|abstract|synchronized|native|transient|volatile|strictfp|default|sealed|non-sealed)";
/** 类型声明：class / interface / enum / record（含修饰符前缀，如 public final） */
const JAVA_TYPE_DECL = new RegExp(
  `^(?:${JAVA_MODIFIER}\\s+)*(class|interface|enum|record)\\s+(\\w+)`
);
/** 注解类型声明：@interface Foo */
const JAVA_ANNOTATION_TYPE = new RegExp(
  `^(?:${JAVA_MODIFIER}\\s+)*@interface\\s+(\\w+)`
);
/** 构造方法：类名首字母大写 + 参数表，行尾是 { 或 throws（用于与普通调用区分） */
const JAVA_CONSTRUCTOR = new RegExp(
  `^(?:${JAVA_MODIFIER}\\s+)*([A-Z]\\w*)\\s*\\([^)]*\\)\\s*(?:throws\\s+[\\w,.\\s]+)?\\{?\\s*$`
);
/**
 * 方法声明：`修饰符 返回类型 名字(`
 * 要求“类型与名字之间必须有空格”，据此排除调用语句（`System.out.println(` 无空格、`foo(` 无类型）。
 */
const JAVA_METHOD = new RegExp(
  `^(?:${JAVA_MODIFIER}\\s+)*([\\w<>\\[\\],.?]+)\\s+(\\w+)\\s*\\(`
);
/**
 * 字段：必须带访问修饰符或 static/transient/volatile —— 这些不可能是局部变量。
 * 只写 final 的语句可能是方法内局部变量，故意不收。
 */
const JAVA_FIELD = new RegExp(
  `^(?:(?:public|protected|private|static|transient|volatile)\\s+)+(?:final\\s+)*` +
    `([\\w<>\\[\\],.?]+)\\s+(\\w+)\\s*(?:=[^;]*)?;\\s*$`
);
/** 语句关键字开头：不是声明，直接跳过（含 return/if 等误命中来源） */
const JAVA_STATEMENT_KEYWORD =
  /^(return|throw|break|continue|assert|if|for|while|switch|do|else|catch|case|import|package|new|yield|try)\b/;

function parseJavaOutline(content) {
  const items = [];
  const lines = content.split("\n");

  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    // 缩进层级：类体成员 = 2 级，方法体内部 ≥ 3 级（用于过滤局部变量）
    const indent = raw.match(/^(\s*)/)[1];
    const indentLen = indent.replace(/\t/g, "    ").length;
    const level = Math.min(Math.floor(indentLen / 4) + 1, 3);

    // 剥掉行首注解（@Override / @SuppressWarnings("x")），再判断内容
    const line = raw.replace(/^(?:@\w+(?:\([^)]*\))?\s*)+/, "").trim();
    if (!line || line.startsWith("//") || line.startsWith("*") || line.startsWith("/*")) {
      continue;
    }
    if (JAVA_STATEMENT_KEYWORD.test(line)) continue;

    let match;
    if ((match = raw.trim().match(JAVA_ANNOTATION_TYPE))) {
      items.push({ level, text: `@interface ${match[1]}`, line: i + 1 });
    } else if ((match = line.match(JAVA_TYPE_DECL))) {
      items.push({ level, text: `${match[1]} ${match[2]}`, line: i + 1 });
    } else if ((match = line.match(JAVA_CONSTRUCTOR))) {
      items.push({ level, text: `${match[1]}()`, line: i + 1 });
    } else if ((match = line.match(JAVA_METHOD))) {
      items.push({ level, text: `${match[2]}()`, line: i + 1 });
    } else if (level <= 2 && (match = line.match(JAVA_FIELD))) {
      items.push({ level, text: `${match[1]} ${match[2]}`, line: i + 1 });
    }
  }
  return items;
}

// ============================================
// 大纲区多标签（大纲 / Git）
// ============================================

function setupOutlineTabs() {
  const tabs = document.querySelectorAll(".outline-tab");
  tabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      tabs.forEach((t) => t.classList.remove("active"));
      tab.classList.add("active");

      const isGit = tab.dataset.outlineTab === "git";
      document.getElementById("outline-content").style.display = isGit ? "none" : "";
      document.getElementById("git-panel").style.display = isGit ? "" : "none";
      if (isGit) loadGitStatus();
    });
  });

  // Git 面板按钮 — 全部走命令系统统一入口
  document.getElementById("git-btn-commit")?.addEventListener("click", () => {
    const input = document.getElementById("git-commit-input");
    const msg = input.value.trim();
    if (!msg) {
      setStatus(I18N.t("git.need_msg"), "error");
      input.focus();
      return;
    }
    // 转义引号，保证消息作为一个参数传给 git
    const quoted = msg.replace(/"/g, '\\"');
    handleCommand(`git commit -m "${quoted}"`);
    input.value = "";
  });
  document.getElementById("git-btn-pull")?.addEventListener("click", () => handleCommand("git pull"));
  document.getElementById("git-btn-push")?.addEventListener("click", () => handleCommand("git push"));

  // unstaged 标题右侧 ➕：全部暂存
  document.getElementById("git-btn-stage-all")?.addEventListener("click", () => {
    handleCommand("git add -A");
  });

  // 非仓库引导视图：初始化仓库（初始化后 loadGitStatus 会自动切换为仓库视图）
  document.getElementById("git-btn-init")?.addEventListener("click", () => handleCommand("git init"));
  // 仓库视图顶部提示条：添加 / 更新远程仓库
  document.getElementById("git-btn-add-remote")?.addEventListener("click", addRemoteRepo);
}

/** 最近一次 git_status 结果：供"添加远程仓库"判断是否已有 origin / 是否仓库 */
let _gitStatusCache = null;

/** 加载 Git 状态：分支名 + 远程地址 + staged/unstaged 文件树 */
async function loadGitStatus() {
  const stagedEl = document.getElementById("git-staged-tree");
  const unstagedEl = document.getElementById("git-unstaged-tree");
  if (!stagedEl || !unstagedEl) return;

  const branchEl = document.getElementById("git-branch");
  const stagedHeader = document.getElementById("git-staged-header");
  const unstagedHeader = document.getElementById("git-unstaged-header");
  const remoteBanner = document.getElementById("git-remote-banner");

  const stageAllBtn = document.getElementById("git-btn-stage-all");

  if (!state.currentProject) {
    showGitRepoView();
    if (branchEl) branchEl.textContent = "";
    if (remoteBanner) remoteBanner.style.display = "none";
    if (stageAllBtn) stageAllBtn.style.display = "none";
    stagedEl.innerHTML = `<div class="git-empty">${I18N.t("git.no_project")}</div>`;
    unstagedEl.innerHTML = "";
    return;
  }

  const invoke = getTauriInvoke();
  if (!invoke) {
    showGitRepoView();
    if (remoteBanner) remoteBanner.style.display = "none";
    if (stageAllBtn) stageAllBtn.style.display = "none";
    stagedEl.innerHTML = `<div class="git-empty">${I18N.t("status.tauri_unavail")}</div>`;
    unstagedEl.innerHTML = "";
    return;
  }

  try {
    const status = await invoke("git_status", { projectRoot: state.currentProject.path });
    _gitStatusCache = status;

    // 非 git 仓库 → 展示仓库初始化引导（不是错误状态）
    if (!status.is_repo) {
      showGitInitView();
      return;
    }

    showGitRepoView();
    if (branchEl) {
      branchEl.textContent = status.remote
        ? I18N.t("git.branch_with_remote", { branch: status.branch, remote: status.remote })
        : I18N.t("git.branch", { branch: status.branch });
    }
    // 已初始化但未配置远程仓库 → 顶部提示 + 添加入口
    if (remoteBanner) remoteBanner.style.display = status.remote ? "none" : "";
    if (stagedHeader) stagedHeader.textContent = gitSectionTitle("git.staged", status.staged.length);
    if (unstagedHeader) unstagedHeader.textContent = gitSectionTitle("git.unstaged", status.unstaged.length);
    // 没有可暂存的内容时隐藏 ➕
    if (stageAllBtn) stageAllBtn.style.display = status.unstaged.length > 0 ? "" : "none";
    renderGitTree(stagedEl, status.staged, true);
    renderGitTree(unstagedEl, status.unstaged, false);
  } catch (err) {
    // 真正的异常（如未安装 git）→ 面板内提示
    showGitRepoView();
    if (branchEl) branchEl.textContent = "";
    if (remoteBanner) remoteBanner.style.display = "none";
    if (stagedHeader) stagedHeader.textContent = I18N.t("git.staged");
    if (unstagedHeader) unstagedHeader.textContent = I18N.t("git.unstaged");
    if (stageAllBtn) stageAllBtn.style.display = "none";
    stagedEl.innerHTML = `<div class="git-empty">⚠ ${escapeHtml(err)}</div>`;
    unstagedEl.innerHTML = "";
  }
}

/** 展示仓库视图（staged/unstaged），隐藏初始化引导 */
function showGitRepoView() {
  const initView = document.getElementById("git-init-view");
  const repoView = document.getElementById("git-repo-view");
  if (initView) initView.style.display = "none";
  if (repoView) repoView.style.display = "";
}

/** 展示仓库初始化引导视图（非 git 仓库）：隐藏 staged/unstaged 相关区域 */
function showGitInitView() {
  const initView = document.getElementById("git-init-view");
  const repoView = document.getElementById("git-repo-view");
  if (initView) initView.style.display = "";
  if (repoView) repoView.style.display = "none";
  if (_gitStatusCache) _gitStatusCache.is_repo = false;
}

/**
 * 添加远程仓库：走命令系统（git remote add / 已存在则 set-url 覆盖）
 * 地址由 showPrompt 弹窗收集，做前缀格式校验
 */
async function addRemoteRepo() {
  if (!state.currentProject) {
    setStatus(I18N.t("git.no_project"), "error");
    return;
  }
  if (!_gitStatusCache || !_gitStatusCache.is_repo) {
    setStatus(I18N.t("git.remote.need_init"), "error");
    return;
  }

  const url = await showPrompt(I18N.t("git.remote.prompt_title"), "");
  if (url === null) return;
  const trimmed = url.trim();
  if (!trimmed) return;

  // 基础格式校验：http(s) / ssh / git / file 协议或 scp 风格 git@host:path
  if (!/^(https?:\/\/|ssh:\/\/|git:\/\/|file:\/\/|git@)/i.test(trimmed)) {
    setStatus(I18N.t("git.remote.invalid"), "error");
    return;
  }

  // 已有 origin → set-url 覆盖；否则 add（P3 推荐）
  const verb = _gitStatusCache.remote ? "set-url" : "add";
  handleCommand(`git remote ${verb} origin "${trimmed}"`);
}

function gitSectionTitle(key, count) {
  return count > 0 ? `${I18N.t(key)} (${count})` : I18N.t(key);
}

/** 渲染 staged/unstaged 文件树。点击文件 → 暂存 / 取消暂存（走命令系统） */
function renderGitTree(el, files, isStaged) {
  if (!files || files.length === 0) {
    const emptyKey = isStaged ? "git.staged_empty" : "git.unstaged_empty";
    el.innerHTML = `<div class="git-empty">${I18N.t(emptyKey)}</div>`;
    return;
  }

  const root = buildGitTreeNodes(files);
  el.innerHTML = renderGitNodes(root.children, 0, isStaged);

  // 文件夹节点：点击展开/折叠；右侧 ➕ 暂存整个文件夹
  el.querySelectorAll(".git-dir").forEach((node) => {
    const children = node.nextElementSibling;
    if (!children || !children.classList.contains("git-children")) return;
    const icon = node.querySelector(".git-dir-icon");

    node.addEventListener("click", (e) => {
      if (e.target.closest(".git-stage-btn")) return;
      if (children.style.display !== "none") {
        children.style.display = "none";
        if (icon) icon.textContent = "📁";
      } else {
        children.style.display = "";
        if (icon) icon.textContent = "📂";
      }
    });
  });

  el.querySelectorAll(".git-file").forEach((item) => {
    // 右侧 ➕：暂存该文件/文件夹
    const stageBtn = item.querySelector(".git-stage-btn");
    if (stageBtn) {
      stageBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        stageGitPath(item.dataset.path);
      });
    }
    // 文件行点击：unstaged → 暂存；staged → 取消暂存
    if (!item.classList.contains("git-dir")) {
      item.addEventListener("click", () => {
        const path = item.dataset.path;
        const cmd = isStaged
          ? `git restore --staged -- "${path.replace(/"/g, '\\"')}"`
          : `git add -- "${path.replace(/"/g, '\\"')}"`;
        handleCommand(cmd);
      });
    }
  });
}

/** ➕ 点击：暂存一个文件或文件夹（走命令系统） */
function stageGitPath(path) {
  handleCommand(`git add -- "${path.replace(/"/g, '\\"')}"`);
}

/**
 * 将 git 状态返回的平铺文件列表构建为文件夹树。
 * 节点: { name, path, isDir, kind, children }（kind 仅文件有）
 */
function buildGitTreeNodes(files) {
  const root = { children: [] };
  for (const f of files) {
    const segs = f.path.split("/");
    let node = root;
    for (let i = 0; i < segs.length - 1; i++) {
      let child = node.children.find((c) => c.isDir && c.name === segs[i]);
      if (!child) {
        child = {
          name: segs[i],
          path: segs.slice(0, i + 1).join("/"),
          isDir: true,
          kind: null,
          children: [],
        };
        node.children.push(child);
      }
      node = child;
    }
    node.children.push({
      name: segs[segs.length - 1],
      path: f.path,
      isDir: false,
      kind: f.kind,
      children: [],
    });
  }

  // 目录在前，其余按名称排序
  const sortRec = (n) => {
    if (!n.children.length) return;
    n.children.sort((a, b) => (b.isDir - a.isDir) || a.name.localeCompare(b.name));
    n.children.forEach(sortRec);
  };
  sortRec(root);
  return root;
}

/** 渲染树节点为 HTML（文件夹默认折叠） */
function renderGitNodes(nodes, depth, isStaged) {
  return nodes
    .map((n) => {
      const escPath = escapeHtml(n.path);
      const pad = 12 + depth * 14;
      if (n.isDir) {
        return `
      <div class="git-file git-dir" data-path="${escPath}" title="${escPath}" style="padding-left:${pad}px">
        <span class="git-dir-icon">📁</span>
        <span class="git-file-name">${escapeHtml(n.name)}</span>
        ${isStaged ? "" : `<span class="git-stage-btn" title="${escapeHtml(I18N.t("git.stage"))}">➕</span>`}
      </div>
      <div class="git-children" style="display:none">${renderGitNodes(n.children, depth + 1, isStaged)}</div>`;
      }
      return `
      <div class="git-file" data-path="${escPath}" title="${escPath}" style="padding-left:${pad}px">
        <span class="git-file-status git-st-${escapeHtml(n.kind.toLowerCase())}">${escapeHtml(n.kind)}</span>
        <span class="git-file-name">${escapeHtml(n.name)}</span>
        ${isStaged ? "" : `<span class="git-stage-btn" title="${escapeHtml(I18N.t("git.stage"))}">➕</span>`}
      </div>`;
    })
    .join("");
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

    // Tab 键插入缩进（而非切换焦点）
    textarea.addEventListener("keydown", (e) => {
      if (e.key === "Tab" && !textarea.readOnly) {
        e.preventDefault();
        const start = textarea.selectionStart;
        const end = textarea.selectionEnd;
        textarea.setRangeText("\t", start, end, "end");
        textarea.selectionStart = textarea.selectionEnd = start + 1;
        // 触发 input 事件以便 setupTextareaSync 同步内容
        textarea.dispatchEvent(new Event("input", { bubbles: true }));
      }
    });
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
  if (!tab || tab._isTerminal || tab._isImage || tab._isSession) return;
  if (tab._isConfig) return saveConfigTab(tab);
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
// 服务页（agent 用 background 起的托管进程；面板读引擎同一份表）
// ============================================
function setupServiceMenu() {
  const btn = document.getElementById("menu-service");
  if (!btn) return;

  btn.addEventListener("click", () => window.ServiceUI?.open());
  btn.style.cursor = "pointer";
}

// ============================================
// 帮助页（正文源文件 = ui/help-zh.md / help-en.md，由 markdown-it 渲染）
// ============================================
function setupHelpMenu() {
  const btn = document.getElementById("menu-help");
  if (!btn) return;

  btn.addEventListener("click", () => openHelp());
  btn.style.cursor = "pointer";

  // 帮助页返回按钮
  document.getElementById("btn-help-back")?.addEventListener("click", () => hideHelpPage());
}

/** markdown-it 渲染器懒构造（vendor 自 ui/markdown-it.min.js） */
let _mdRenderer = null;

/** markdown → HTML；无渲染器时退化为转义文本（保留换行） */
function markdownToHtml(text) {
  if (!_mdRenderer && typeof window.markdownit === "function") {
    _mdRenderer = window.markdownit({ html: false, breaks: true, linkify: true });
  }
  if (_mdRenderer) return _mdRenderer.render(text ?? "");
  return escapeHtml(text ?? "").replace(/\n/g, "<br>");
}

/** 帮助文档缓存：{ lang, html }；按语言缓存，切语言时重新加载 */
let _helpCache = { lang: null, html: "" };
// 加载令牌：防止并发加载时旧请求覆盖新内容
let _helpLoadToken = 0;

/** 按当前语言加载帮助文档（help-zh.md / help-en.md）并渲染为 HTML */
async function loadHelpDoc() {
  const lang = I18N.getLang() || "zh-CN";
  if (_helpCache.lang === lang) return _helpCache.html;

  const file = lang === "en" ? "help-en.md" : "help-zh.md";
  const token = ++_helpLoadToken;

  try {
    const base = window.location.origin || "https://ruyix.localhost";
    const resp = await fetch(`${base}/${file}`);
    if (!resp.ok) return "";
    const md = await resp.text();
    if (token !== _helpLoadToken) return ""; // 已有更新的加载请求，丢弃本次结果
    _helpCache = { lang, html: markdownToHtml(md) };
    return _helpCache.html;
  } catch {
    return ""; // 加载失败：保留现有内容
  }
}

/** 帮助入口。**必须 await showHelpPage**：正文要现取 md 源文件再渲染，
 *  不返回 promise 的话调用方（含回放测试）拿到的是"还没填内容"的空容器。 */
async function openHelp() {
  if (!state.currentProject) {
    await showHelpPage();
  } else {
    openHelpTab();
  }
}

async function showHelpPage() {
  // 导航区保持可见；编辑区显示帮助内容
  const welcome = document.getElementById("welcome-content");
  const help = document.getElementById("help-page");
  const editorBody = document.getElementById("editor-body");
  const agent = document.getElementById("agent-page");

  const html = await loadHelpDoc();
  const body = document.getElementById("help-body");
  if (body && html) body.innerHTML = html;

  if (welcome) welcome.style.display = "none";
  if (help) help.style.display = "";
  if (editorBody) editorBody.style.display = "none";
  if (agent) agent.style.display = "none";
}

function hideHelpPage() {
  const help = document.getElementById("help-page");
  if (help) help.style.display = "none";
  // 帮助标签页打开着 → 关闭它，回到相邻标签（或欢迎页）
  const helpTab = state.tabs.find((t) => t._isHelp);
  if (helpTab) {
    closeTab(helpTab.id);
    return;
  }
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
    name: I18N.t("help.title"),
    path: "",
    content: "",
    _isHelp: true,
  };
  state.tabs.push(tab);
  renderTabs();
  switchTab(tab.id);
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
    // 丢弃过期的高亮数据；语言由扩展名决定保持不变，保存后据此重新高亮
    tab._highlighted = null;

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
    // 保存后重新高亮（自动保存路径；blur/切标签页保存同样走这里）
    if (tab._language) {
      await highlightAndRender(tab, tab._language);
    }
    // 增量索引
  } catch {
    // 静默失败，定时器下次会重试
  }
}

// ============================================
// 标题栏 — 项目切换下拉
// ============================================

/**
 * 标题栏项目名 → 快速切换下拉：
 * 点击弹出项目列表（get_projects），当前项目 ✔ 标记，点击即切换（走命令系统）。
 */
function setupProjectSwitcher() {
  const trigger = document.getElementById("project-switch-trigger");
  const dropdown = document.getElementById("project-switch-dropdown");
  if (!trigger || !dropdown) return;

  trigger.addEventListener("click", async (e) => {
    e.stopPropagation(); // 阻止 setupMenuBar 的全局关闭监听立即收起
    // 打开本下拉前先关闭其他下拉
    document.querySelectorAll(".menu-dropdown").forEach((d) => {
      if (d !== dropdown) d.style.display = "none";
    });
    if (dropdown.style.display === "none") {
      await renderProjectSwitchDropdown(dropdown);
      dropdown.style.display = "";
    } else {
      dropdown.style.display = "none";
    }
  });

  dropdown.addEventListener("click", async (e) => {
    const item = e.target.closest(".project-switch-item");
    if (!item || !item.dataset.path) return;
    e.stopPropagation();
    dropdown.style.display = "none";
    // 转义引号与反斜杠，保证命令解析（parseQuotedTokens）还原
    const escArg = (s) => String(s).replace(/\\/g, "\\\\").replace(/"/g, '\\"');
    await handleCommand(`open project "${escArg(item.dataset.path)}"`);
  });
}

/**
 * 渲染切换下拉：项目列表，当前项目高亮 + ✔
 */
async function renderProjectSwitchDropdown(dropdown) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    dropdown.innerHTML = `<div class="menu-dropdown-item">${I18N.t("status.tauri_unavail")}</div>`;
    return;
  }

  try {
    const projects = await invoke("get_projects");
    if (!projects || projects.length === 0) {
      dropdown.innerHTML = `<div class="menu-dropdown-item">${I18N.t("projectlist.empty")}</div>`;
      return;
    }

    const cur = state.currentProject ? state.currentProject.path : null;
    dropdown.innerHTML = projects
      .map((p) => {
        const name = p.name || p.path.split(/[/\\]/).pop() || p.path;
        const lang = p.lang || "unknown";
        const isCur = !!cur && samePath(cur, p.path);
        return `
        <div class="project-switch-item${isCur ? " current" : ""}" data-path="${escapeHtml(p.path)}">
          <span class="project-icon" title="${escapeHtml(I18N.t(`lang.${lang}`))}">${projectLangIcon(lang)}</span>
          <span class="project-switch-name">${escapeHtml(name)}</span>
          <span class="project-switch-path" title="${escapeHtml(p.path)}">${escapeHtml(p.path)}</span>
          <span class="project-switch-check">${isCur ? "✔️" : ""}</span>
        </div>`;
      })
      .join("");
  } catch (err) {
    dropdown.innerHTML = `<div class="menu-dropdown-item">${I18N.t("projectlist.load_error", { err })}</div>`;
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

  // 项目切换下拉箭头：仅打开项目时显示
  const arrow = document.getElementById("project-switch-arrow");
  if (arrow) arrow.style.display = state.currentProject ? "" : "none";
}

/**
 * 更新操作系统窗口标题（Alt+Tab / 任务栏可见）。
 * 打开项目时显示项目名，否则显示 Darkhorse Code。
 */
async function updateWindowTitle() {
  const win = getTauriWindow();
  if (!win || typeof win.setTitle !== "function") return;
  try {
    const title = state.currentProject
      ? state.currentProject.name
      : "Darkhorse Code";
    await win.setTitle(title);
  } catch {
    // 静默失败
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

  // 更新窗口标题为默认值
  updateWindowTitle();

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
  // 智能体只在打开项目后可用（引擎任务依赖项目上下文）
  const tabAgent = document.querySelector('.nav-tab[data-tab="sessions"]');

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
    if (tabAgent) tabAgent.style.display = "none";
    window.SessionUI?.projectClosed?.();

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
    if (tabAgent) tabAgent.style.display = "";

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

function setupCapabilityMenu() {
  // 面板内子标签（MCP / A2A / 工具 / 技能）
  document.querySelectorAll(".cap-sub-tab").forEach((sub) => {
    sub.addEventListener("click", () => switchCapabilitySub(sub.dataset.capTab));
  });
  // 顶部「能力」菜单：激活能力面板并切到对应子页
  document.querySelectorAll("#menu-capability-dropdown [data-cap]").forEach((item) => {
    item.addEventListener("click", () => {
      document.querySelector('.nav-tab[data-tab="capability"]')?.click();
      switchCapabilitySub(item.dataset.cap);
    });
  });
}

function switchCapabilitySub(sub) {
  document.querySelectorAll(".cap-sub-tab").forEach((s) =>
    s.classList.toggle("active", s.dataset.capTab === sub));
  document.querySelectorAll(".cap-sub-panel").forEach((p) =>
    p.classList.toggle("active", p.id === `cap-sub-${sub}`));
}

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
let _ctxIsRoot = false;

/** Windows 路径归一化比较（分隔符、尾斜杠、大小写无关） */
function samePath(a, b) {
  if (!a || !b) return false;
  const norm = (p) => p.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
  return norm(a) === norm(b);
}

function setupContextMenu() {
  const menu = document.getElementById("context-menu");
  const fileTree = document.getElementById("file-tree");
  if (!menu || !fileTree) return;

  // 禁止浏览器默认右键菜单（仅文件树区域）
  fileTree.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    let node = e.target.closest(".tree-node");
    if (!node) {
      // 空白区域：优先归属到已展开目录，否则视为项目根目录
      const cc = e.target.closest(".tree-children");
      node = cc ? cc.previousElementSibling : null;
    }
    if (node) {
      _ctxPath = node.dataset.path;
      _ctxIsDir = node.dataset.isDir === "true";
      _ctxIsRoot = samePath(_ctxPath, state.currentProject?.path);
    } else if (state.currentProject) {
      _ctxPath = state.currentProject.path;
      _ctxIsDir = true;
      _ctxIsRoot = true;
    } else {
      return;
    }
    showContextMenu(menu, e.clientX, e.clientY, _ctxIsDir);
  });

  // 菜单项点击
  menu.addEventListener("click", async (e) => {
    const item = e.target.closest(".context-menu-item");
    if (!item || item.classList.contains("context-menu-sep")) return;
    const action = item.dataset.action;
    hideContextMenu(menu);
    if (!_ctxPath) return;

    // 项目根目录不允许删除/重命名（菜单项已隐藏，此处兜底）
    if (_ctxIsRoot && (action === "delete" || action === "rename")) {
      setStatus(I18N.t("status.no_permission"), "error");
      return;
    }

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
      case "newfolder":
        await createFolderInFolder(_ctxPath);
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
  // 项目根目录只允许创建，不允许删除/重命名
  menu.querySelectorAll('[data-action="delete"], [data-action="rename"]').forEach(el => el.style.display = _ctxIsRoot ? "none" : "");
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

/** 右键"运行"：已有目标→执行，无目标→按清单内容创建运行目标 */
async function handleContextRun(fullPath) {
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    const status = await invoke("get_execute_status", {
      path: fullPath,
      projectRoot: state.currentProject?.path
    });

    // 情况1: 已有目标绑定该文件 → 执行（多脚本时取先命中的一个，通常按 dev/start 排序）
    if (status.has_target && status.target_name) {
      const targets = await invoke("get_run_targets", { projectRoot: state.currentProject?.path });
      const target = targets.find(t => (t.name || t.key) === status.target_name);
      if (target && target.cmd) {
        await runTargetCmd(status.target_name, target.cmd, target.bind);
      }
      return;
    }

    // 情况2: 清单文件 → 按文件内容生成（package.json 的每个 scripts 各一个运行目标）
    const specs = status.suggested_targets || [];
    if (specs.length > 0) {
      const created = await createRunTargets(fullPath, specs);
      setStatus(I18N.t("run.auto.created", { count: created.length, names: created.join("、") }));
      return;
    }

    // 情况3: 是清单文件但没有可运行脚本（如 package.json 里没有 scripts）
    if (status.known === true) {
      const fileName = fullPath.split(/[/\\]/).pop() || fullPath;
      setStatus(I18N.t("run.auto.no_script", { file: fileName }), "error");
      return;
    }

    // 情况4: 未知 → 按模板创建单个运行目标（兜底）
    const name = fullPath.split(/[/\\]/).pop() || fullPath;
    const dot = name.lastIndexOf(".");
    const ext = dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
    // 命令模板：优先用后端建议，其次文件名匹配，最后扩展名匹配
    const cmdMap = {
      py: "python {file}", rs: "cargo run", js: "node {file}",
      "Cargo.toml": "cargo run", Makefile: "make"
    };
    let cmd = status.suggested_cmd || cmdMap[name] || cmdMap[ext] || "python {file}";
    cmd = cmd.replace(/\{file\}/gi, fullPath);
    await autoCreateRunTarget(name, cmd, fullPath);
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
    const fileName = fullPath.split(/[/\\]/).pop() || fullPath;
    const ext = (fullPath.split(".").pop() || "").toLowerCase();
    const upper = result.trim().toUpperCase();

    if (upper.startsWith("FILE_YES")) {
      // 因文件名而可运行（清单文件：Cargo.toml、package.json 等）
      let cmdTemplate = upper.startsWith("FILE_YES|") ? result.slice(result.indexOf("|") + 1).trim() : ("python {file}");
      const cmd = cmdTemplate.replace(/\{file\}/gi, fullPath);
      await invoke("set_execute_entry", { path: fullPath, canRun: true, asFile: true });
      await autoCreateRunTarget(fileName, cmd, fullPath);
      setStatus("已记住文件名: " + fileName + " → " + cmd);
    } else if (upper.startsWith("YES")) {
      // 可运行 — 提取命令模板
      let cmdTemplate = upper.startsWith("YES|") ? result.slice(result.indexOf("|") + 1).trim() : ("python {file}");
      const cmd = cmdTemplate.replace(/\{file\}/gi, fullPath);
      await invoke("set_execute_entry", { path: fullPath, canRun: true, asFile: false });
      await autoCreateRunTarget(fileName, cmd, fullPath);
      setStatus("已记住并创建运行目标: ." + ext + " → " + cmd);
    } else if (upper.startsWith("CONDITIONAL")) {
      const detail = upper.startsWith("CONDITIONAL|") ? result.slice(result.indexOf("|") + 1).trim() : result;
      setStatus("功能待开发: " + detail, "error");
    } else {
      // NO 或其他
      await invoke("set_execute_entry", { path: fullPath, canRun: false, asFile: false });
      setStatus("已记住: ." + ext + " 不可运行 (AI: " + result.slice(0, 60) + ")");
    }
  } catch (err) {
    setStatus("试跑失败: " + err, "error");
  }
}

/**
 * IDE 自动创建运行目标（单个）。
 * key 使用 target<N> 格式，同时设置 bind 字段（相对路径）。
 */
async function autoCreateRunTarget(name, cmd, fullPath) {
  const created = await createRunTargets(fullPath, [{ name, cmd }]);
  return created[0] || null;
}

/**
 * 按后端建议批量创建运行目标（每个 spec 一个）。
 * - key 使用 target<N>（遍历已有 target* 取 max+1，逐个递增）
 * - bind 统一写绑定文件的相对路径
 * - 名称冲突时加父目录前缀（如 admin-web:dev），再冲突则加 #N
 * @param {string} fullPath 绑定的清单文件绝对路径
 * @param {Array<{name: string, cmd: string}>} specs 后端给出的建议目标
 * @returns {Promise<string[]>} 实际创建的目标名称
 */
async function createRunTargets(fullPath, specs) {
  const invoke = getTauriInvoke();
  if (!invoke || !state.currentProject || !specs || specs.length === 0) return [];

  const relPath = toRelativePath(fullPath);
  const dirName = relPath.split(/[/\\]/).slice(-2, -1)[0] || "";

  // 既有目标名与最大索引
  let index = 0;
  const used = new Set();
  try {
    const targets = (await invoke("get_run_targets", { projectRoot: state.currentProject.path })) || [];
    let maxIdx = -1;
    for (const t of targets) {
      used.add(t.name || t.key);
      const m = (t.key || "").match(/^target(\d+)$/);
      if (m) maxIdx = Math.max(maxIdx, parseInt(m[1], 10));
    }
    index = maxIdx + 1;
  } catch {
    index = 0;
  }

  const created = [];
  try {
    for (const spec of specs) {
      let name = spec.name;
      if (used.has(name)) name = dirName ? `${dirName}:${spec.name}` : name;
      if (used.has(name)) {
        let n = 2;
        while (used.has(`${name}#${n}`)) n++;
        name = `${name}#${n}`;
      }
      used.add(name);

      const targetKey = `target${index}`;
      await executeConfigAction("add", "p", `ruyix.code.run.${targetKey}.cmd`, spec.cmd);
      await executeConfigAction("add", "p", `ruyix.code.run.${targetKey}.name`, name);
      await executeConfigAction("add", "p", `ruyix.code.run.${targetKey}.bind`, relPath);
      created.push(name);
      index += 1;
    }
    loadRunTargets();
  } catch (err) {
    setStatus("自动创建运行目标失败: " + err, "error");
  }
  return created;
}

/** 全路径 → 相对于项目根的路径 */
function toRelativePath(fullPath) {
  if (!state.currentProject) return fullPath;
  const root = state.currentProject.path.replace(/[/\\]+$/, "");
  if ((fullPath || "").replace(/[/\\]+$/, "") === root) return "";
  return fullPath.startsWith(root + "\\") ? fullPath.slice(root.length + 1) : fullPath;
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
  const rel = toRelativePath(fullPath);
  await handleCommand('new file ' + (rel ? rel + '\\' : '') + name, true);
}

async function createFolderInFolder(fullPath) {
  const name = await showPrompt('新建文件夹', '');
  if (!name) return;
  const rel = toRelativePath(fullPath);
  await handleCommand('new folder ' + (rel ? rel + '\\' : '') + name, true);
}

// ============================================
// 项目列表（无项目时显示在导航区）
// ============================================

/**
 * 项目语言定义：key 与后端 PROJECT_LANGS 对应，图标与标签（i18n lang.*）
 */
const PROJECT_LANGS = [
  { key: "unknown", icon: "Ⓤ" },
  { key: "mix", icon: "Ⓜ" },
  { key: "java", icon: "Ⓙ" },
  { key: "c", icon: "Ⓒ" },
  { key: "python", icon: "Ⓟ" },
  { key: "rust", icon: "Ⓡ" },
  { key: "web", icon: "Ⓦ" },
  { key: "golang", icon: "Ⓖ" },
  { key: "document", icon: "Ⓓ" },
  { key: "kotlin", icon: "Ⓚ" },
];

/** 语言 key → 图标 */
function projectLangIcon(lang) {
  const entry = PROJECT_LANGS.find((l) => l.key === lang);
  return entry ? entry.icon : PROJECT_LANGS[0].icon;
}

async function loadProjectList() {
  const list = document.getElementById("project-list");
  if (!list) return;

  const invoke = getTauriInvoke();
  if (!invoke) {
    list.innerHTML = `<span class="project-list-empty">${I18N.t("status.tauri_unavail")}</span>`;
    return;
  }

  try {
    const projects = await invoke("get_projects");

    if (!projects || projects.length === 0) {
      list.innerHTML = `<span class="project-list-empty">${I18N.t("projectlist.empty")}<br>${I18N.t("projectlist.hint")}</span>`;
      return;
    }

    list.innerHTML = projects
      .map((p) => {
        const name = p.name || p.path.split(/[/\\]/).pop() || p.path;
        const lang = p.lang || "unknown";
        const icon = projectLangIcon(lang);
        return `
        <div class="project-list-item" data-path="${escapeHtml(p.path)}" data-name="${escapeHtml(name)}" data-lang="${escapeHtml(lang)}">
          <span class="project-icon" title="${escapeHtml(I18N.t(`lang.${lang}`))}">${icon}</span>
          <div class="project-info">
            <div class="project-name">${escapeHtml(name)}</div>
            <div class="project-path" title="${escapeHtml(p.path)}">${escapeHtml(p.path)}</div>
          </div>
          <span class="project-item-edit" title="${escapeHtml(I18N.t("projectlist.edit"))}">✍️</span>
          <span class="project-item-del" title="${escapeHtml(I18N.t("projectlist.del"))}">🗑️</span>
        </div>`;
      })
      .join("");

    // 点击行打开项目；✍️ 修改项目；🗑️ 从列表删除（都走命令系统）
    list.querySelectorAll(".project-list-item").forEach((el) => {
      el.addEventListener("click", () => {
        const path = el.dataset.path;
        if (path) openProject(path);
      });

      const editBtn = el.querySelector(".project-item-edit");
      if (editBtn) {
        editBtn.addEventListener("click", async (e) => {
          e.stopPropagation();
          const result = await showProjectEditModal(
            el.dataset.path,
            el.dataset.name,
            el.dataset.lang
          );
          if (result) {
            // 转义引号与反斜杠，保证命令解析（parseQuotedTokens）还原
            const escArg = (s) =>
              String(s).replace(/\\/g, "\\\\").replace(/"/g, '\\"');
            await handleCommand(
              `project edit "${escArg(result.path)}" "${escArg(result.name)}" ${result.lang}`
            );
          }
        });
      }

      const delBtn = el.querySelector(".project-item-del");
      if (delBtn) {
        delBtn.addEventListener("click", async (e) => {
          e.stopPropagation();
          const ok = await showConfirm(
            I18N.t("project.delete.confirm_title"),
            I18N.t("project.delete.confirm", { path: el.dataset.path })
          );
          if (ok) await handleCommand(`project delete ${el.dataset.path}`);
        });
      }
    });
  } catch (err) {
    list.innerHTML = `<span class="project-list-empty">${I18N.t("projectlist.load_error", { err })}</span>`;
  }
}

/**
 * 修改项目弹窗：可改名称与图标（语言），路径只读。
 * 返回 { path, name, lang } 或 null（取消）。
 */
function showProjectEditModal(path, name, lang) {
  return new Promise((resolve) => {
    const overlay = document.getElementById("project-edit-modal");
    const nameInput = document.getElementById("project-edit-name");
    const pathText = document.getElementById("project-edit-path");
    const iconWrap = document.getElementById("project-edit-icons");
    if (!overlay || !nameInput || !pathText || !iconWrap) {
      resolve(null);
      return;
    }

    let selected = lang || "unknown";
    nameInput.value = name || "";
    pathText.textContent = path || "";
    pathText.title = path || "";

    // 渲染图标选择（10 种语言）
    iconWrap.innerHTML = "";
    PROJECT_LANGS.forEach((l) => {
      const btn = document.createElement("span");
      btn.className = "project-icon-option" + (l.key === selected ? " selected" : "");
      btn.textContent = l.icon;
      btn.title = I18N.t(`lang.${l.key}`);
      btn.addEventListener("click", () => {
        selected = l.key;
        iconWrap
          .querySelectorAll(".project-icon-option")
          .forEach((el) => el.classList.remove("selected"));
        btn.classList.add("selected");
      });
      iconWrap.appendChild(btn);
    });

    const okBtn = document.getElementById("project-edit-ok");
    const cancelBtn = document.getElementById("project-edit-cancel");
    okBtn.textContent = I18N.t("modal.ok") || "确认";
    cancelBtn.textContent = I18N.t("modal.cancel") || "取消";

    const cleanup = () => {
      overlay.style.display = "none";
      document.removeEventListener("keydown", onKey);
    };
    const onOk = () => {
      const newName = nameInput.value.trim();
      if (!newName) {
        setStatus(I18N.t("project.edit.name_empty"), "error");
        return;
      }
      cleanup();
      resolve({ path: path || "", name: newName, lang: selected });
    };
    const onCancel = () => {
      cleanup();
      resolve(null);
    };

    okBtn.onclick = onOk;
    cancelBtn.onclick = onCancel;
    overlay.onclick = (e) => {
      if (e.target === overlay) onCancel();
    };

    const onKey = (e) => {
      if (e.key === "Escape") onCancel();
      if (e.key === "Enter") onOk();
    };
    document.addEventListener("keydown", onKey);

    overlay.style.display = "";
    nameInput.focus();
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
      <div class="run-target-item" data-cmd="${escapeHtml(t.cmd || "")}" data-name="${escapeHtml(t.name || t.key)}" data-bind="${escapeHtml(t.bind || "")}">
        <div class="run-target-info">
          <div class="run-target-name">
            <span class="run-icon">&#9654;</span>
            ${escapeHtml(t.name || t.key)}
          </div>
          <div class="run-target-cmd">${escapeHtml(t.cmd || "（无命令）")}</div>
          ${t.bind ? `<div class="run-target-bind" title="绑定文件">🔗 ${escapeHtml(t.bind)}</div>` : ""}
        </div>
        <span class="run-target-edit" title="修改运行目标">&#9998;</span>
        <span class="run-target-del" title="删除运行目标">&#128465;</span>
      </div>`
      )
      .join("");

    // 点击运行
    list.querySelectorAll(".run-target-item").forEach((el) => {
      el.addEventListener("click", () => {
        const cmd = el.dataset.cmd;
        const name = el.dataset.name;
        const bind = el.dataset.bind;
        if (cmd) runTargetCmd(name, cmd, bind);
      });
    });

    // 点击编辑图标 → 通过命令系统修改
    list.querySelectorAll(".run-target-edit").forEach((editEl) => {
      editEl.addEventListener("click", (e) => {
        e.stopPropagation(); // 阻止冒泡到父级触发运行
        const item = editEl.closest(".run-target-item");
        const name = item?.dataset.name;
        if (name) handleCommand("run edit " + name, true);
      });
    });

    // 点击删除图标 → 通过命令系统删除
    list.querySelectorAll(".run-target-del").forEach((delEl) => {
      delEl.addEventListener("click", (e) => {
        e.stopPropagation(); // 阻止冒泡到父级触发运行
        const item = delEl.closest(".run-target-item");
        const name = item?.dataset.name;
        if (name) handleCommand("run del " + name, false);
      });
    });
  } catch (err) {
    list.innerHTML = `<span class="run-targets-empty">加载失败: ${err}</span>`;
  }
}

/**
 * 运行目标：在编辑区打开终端标签页执行命令
 * @param {string} name 目标名（标签页标题）
 * @param {string} cmd 要执行的命令
 * @param {string} [bind] 绑定的清单文件（项目相对路径），决定工作目录
 */
async function runTargetCmd(name, cmd, bind) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  // 检查是否已打开同名标签（命令 + 绑定文件都相同才算同一个运行）
  const bindKey = bind || "";
  const existing = state.tabs.find(
    (t) => t._runTarget === cmd && (t._runTargetBind || "") === bindKey
  );
  if (existing) {
    switchTab(existing.id);
    return;
  }

  // 创建运行结果标签页（非终端，输出渲染到编辑器视图）
  const tab = {
    id: "run-" + Date.now().toString(),
    name,
    path: "",
    content: cmd,
    _runTarget: cmd,
    _runTargetBind: bindKey,
  };
  state.tabs.push(tab);
  renderTabs();
  switchTab(tab.id);

  // 显示加载中
  renderTerminalOutput(tab, `> ${cmd}\n\n正在执行...`);

  try {
    setStatus(I18N.t("run.executing", { name }));
    const result = await invoke("run_target", {
      cmd,
      projectRoot: state.currentProject?.path,
      bind: bind || undefined,
    });

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
  tab.content = text; // 同步更新 tab 内容，确保切换标签页后输出不丢失
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

function showSessionView() {
  document.getElementById("editor-empty").style.display = "none";
  document.getElementById("editor-view").style.display = "none";
  document.getElementById("session-view").style.display = "";
}

function hideSessionView() {
  const el = document.getElementById("session-view");
  if (el) el.style.display = "none";
}

function showImageView() {
  document.getElementById("image-view").style.display = "";
}

function hideImageView() {
  document.getElementById("image-view").style.display = "none";
}

function showConfigView() {
  document.getElementById("editor-empty").style.display = "none";
  document.getElementById("editor-view").style.display = "none";
  document.getElementById("config-view").style.display = "";
}

function hideConfigView() {
  const el = document.getElementById("config-view");
  if (el) el.style.display = "none";
}

function showServiceView() {
  document.getElementById("editor-empty").style.display = "none";
  document.getElementById("editor-view").style.display = "none";
  document.getElementById("service-view").style.display = "";
}

function hideServiceView() {
  const el = document.getElementById("service-view");
  if (el) el.style.display = "none";
}

/** 支持的图片扩展名集合 */
const IMAGE_EXTENSIONS = new Set([
  "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "ico", "icon"
]);

/** 判断扩展名是否为图片 */
function isImageExt(ext) {
  return IMAGE_EXTENSIONS.has(ext?.toLowerCase());
}

/** 渲染图片预览（1:1 原始尺寸） */
function renderImagePreview(tab) {
  const container = document.getElementById("image-container");
  const img = document.getElementById("image-preview");
  if (!container || !img) return;

  let url;

  // 方式1: 优先使用后端返回的 base64 数据
  if (tab._imageBase64 && tab._imageMime) {
    url = "data:" + tab._imageMime + ";base64," + tab._imageBase64;
  }

  // 方式2: 尝试使用 Tauri 2 的 convertFileSrc 获取资产 URL
  if (!url) {
    try {
      const tauriCore = getTauriCore();
      if (tauriCore && typeof tauriCore.convertFileSrc === "function") {
        url = tauriCore.convertFileSrc(tab.path);
      }
    } catch {
      // 忽略，走 fallback
    }
  }

  // 方式3: fallback — file:// 协议
  if (!url) {
    url = "file:///" + tab.path.replace(/\\/g, "/");
  }

  img.src = url;
  img.alt = tab.name;
  img.title = tab.name + " (1:1)";

  // 图片加载成功后更新状态栏显示尺寸信息
  img.onload = () => {
    setStatus(tab.name + " — " + img.naturalWidth + " × " + img.naturalHeight + " (1:1)");
  };
  img.onerror = () => {
    setStatus(I18N.t("open.file.fail", { err: "无法加载图片: " + tab.name }), "error");
  };
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

  // 保存已展开的目录路径，重建后恢复
  const expandedPaths = new Set();
  tree.querySelectorAll(".tree-node[data-expanded='true']").forEach(node => {
    expandedPaths.add(node.dataset.path);
  });

  tree.innerHTML = "";

  const invoke = getTauriInvoke();
  if (!invoke) {
    tree.innerHTML = '<span class="file-tree-placeholder">Tauri API 不可用</span>';
    return;
  }

  try {
    const entries = await invoke("list_dir", { path: dirPath });

    // 项目根目录本身也渲染为根节点，右键可在根目录下创建文件/文件夹
    const rootEntry = {
      path: dirPath,
      name: state.currentProject?.name || (dirPath.split(/[/\\]/).pop() || dirPath),
      is_dir: true
    };
    const rootNode = renderTreeEntry(rootEntry, tree, 0);
    const rootChildren = rootNode.nextElementSibling;
    if (entries.length === 0) {
      const ph = document.createElement("span");
      ph.className = "file-tree-placeholder";
      ph.textContent = "空目录";
      rootChildren.appendChild(ph);
    } else {
      for (const entry of entries) {
        renderTreeEntry(entry, rootChildren, 1);
      }
    }
    // 根节点默认展开
    rootNode.dataset.expanded = "true";
    const rootIcon = rootNode.querySelector(".tree-icon--folder");
    if (rootIcon) rootIcon.textContent = entries.length > 0 ? "⬇️" : "🈳";
    rootChildren.style.display = "";

    // 恢复之前展开的目录
    if (expandedPaths.size > 0) {
      await restoreExpandedPaths(tree, expandedPaths);
    }
  } catch (err) {
    tree.innerHTML = `<span class="file-tree-placeholder">读取失败: ${err}</span>`;
  }
}

/** 恢复展开状态：按路径深度排序，浅层先展开 */
async function restoreExpandedPaths(tree, expandedPaths) {
  const sorted = [...expandedPaths].sort((a, b) =>
    a.split(/[/\\]/).length - b.split(/[/\\]/).length
  );

  for (const path of sorted) {
    let foundNode = null;
    tree.querySelectorAll(".tree-node").forEach(n => {
      if (n.dataset.path === path) foundNode = n;
    });
    if (!foundNode || foundNode.dataset.isDir !== "true" || foundNode.dataset.expanded === "true") continue;

    const cc = foundNode.nextElementSibling;
    if (cc && cc.classList.contains("tree-children")) {
      const depth = parseInt(foundNode.style.paddingLeft) / 16 || 0;
      await toggleTreeNode(foundNode, { path, is_dir: true }, cc, depth);
    }
  }
}

/**
 * 从磁盘刷新指定文件夹节点（不重建整棵树，保留子级展开状态）。
 * 未找到该文件夹节点时返回 false。
 */
async function refreshTreeNode(fullPath) {
  const tree = document.getElementById("file-tree");
  if (!tree) return false;

  let node = null;
  tree.querySelectorAll(".tree-node").forEach((n) => {
    if (n.dataset.isDir === "true" && samePath(n.dataset.path, fullPath)) node = n;
  });
  if (!node) return false;
  const cc = node.nextElementSibling;
  if (!cc || !cc.classList.contains("tree-children")) return false;

  const invoke = getTauriInvoke();
  if (!invoke) return false;

  const entries = await invoke("list_dir", { path: node.dataset.path });

  // 保存子级展开状态，刷新后恢复
  const expandedPaths = new Set();
  cc.querySelectorAll(".tree-node[data-expanded='true']").forEach((n) => {
    expandedPaths.add(n.dataset.path);
  });

  const depth = parseInt(node.style.paddingLeft) / 16 || 0;
  cc.innerHTML = "";

  if (node.dataset.expanded === "true") {
    if (entries.length === 0) {
      const ph = document.createElement("span");
      ph.className = "file-tree-placeholder";
      ph.textContent = "空目录";
      cc.appendChild(ph);
    } else {
      for (const entry of entries) {
        renderTreeEntry(entry, cc, depth + 1);
      }
    }
  }
  // 折叠状态：保持容器为空，下次展开时懒加载新数据

  // 更新文件夹图标（➡️ 折叠 / ⬇️ 展开有子项 / 🈳 展开为空）
  const icon = node.querySelector(".tree-icon--folder");
  if (icon) {
    if (node.dataset.expanded !== "true") {
      icon.textContent = "➡️";
    } else {
      icon.textContent = entries.length > 0 ? "⬇️" : "🈳";
    }
  }

  // 恢复子级展开状态
  if (expandedPaths.size > 0) {
    await restoreExpandedPaths(cc, expandedPaths);
  }
  return true;
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

  // 刷新按钮（仅目录）：从磁盘重新读取该文件夹，走命令系统
  if (entry.is_dir) {
    const refreshBtn = document.createElement("button");
    refreshBtn.className = "tree-refresh";
    refreshBtn.textContent = "🔄";
    refreshBtn.title = I18N.t("tree.refresh");
    refreshBtn.addEventListener("click", (e) => {
      e.stopPropagation(); // 防止触发展开/折叠
      const rel = toRelativePath(entry.path);
      handleCommand("refresh" + (rel ? " " + rel : ""), true);
    });
    node.appendChild(refreshBtn);
  }

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
  return node;
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

  // 懒加载子节点（占位符不算子节点）
  let hasChildren = childrenContainer.querySelector(".tree-node") !== null;
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
    // 已有其他实例运行时，不自动打开上次项目（重复打开同一项目毫无意义）
    if (await invoke("is_another_instance")) {
      showWelcomePage();
      setStatus(I18N.t("project.other_instance"));
      return;
    }
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
