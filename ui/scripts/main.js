/**
 * 全局 L（双语内联文案）定义在 command.js —— 它比 session.js / mcp.js / 本文件都先加载。
 * 这里**不能**再声明一次：经典脚本共享同一个全局作用域，重复的顶层 const 会让**本文件整份
 * 不执行**（Uncaught SyntaxError: Identifier 'L' has already been declared），界面随之死掉。
 * 门禁：ui-smoke U56 / U57（scripts/startup-probe.js）。
 */

/**
 * ruyix — Main JavaScript
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

/**
 * 载入高亮插件（v1.0.0 的语法高亮**就是**插件）：
 * 主题 CSS 由插件下发并注入 `<style>`；语言/扩展名/图标给前端建表。
 *
 * 为什么 CSS 走"注入"而不是留在 `styles.css` 里：那样"配色"就还是编译期的一部分 ——
 * 换主题要重编译，纯净模式下也删不掉。现在的契约是：**样式只可能来自插件**，
 * 一条 `.tok-*` 都没有 = 纯文本显示（这就是纯净模式的正常样子，不是故障）。
 */
async function loadHighlightPlugins() {
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    const payload = await invoke("highlight_plugins");
    state.highlightPlugins = payload || { langs: [] };
    let el = document.getElementById("plugin-theme");
    if (!el) {
      el = document.createElement("style");
      el.id = "plugin-theme";
      document.head.appendChild(el);
    }
    el.textContent = payload?.css || "";
    if (!String(payload?.css || "").trim()) {
      // 零配色 = 没有插件认领（纯净模式 / 插件目录被清空）。**说出来**，否则用户只会觉得"高亮坏了"。
      setStatus(
        L(
          `语法高亮：没有插件提供配色（模式 ${payload?.mode || "?"}；插件目录 <便携根>/plugins/highlight/）`,
          `syntax highlighting: no plugin provides colors (mode ${payload?.mode || "?"}; see <root>/plugins/highlight/)`
        )
      );
    }
  } catch (err) {
    // 读不到就当没有插件：**降级不是崩**（高亮丢了不影响读写文件）
    state.highlightPlugins = { langs: [] };
    console.warn("[highlight] 插件注册表读取失败:", err);
  }
}

async function initApp() {
  // 初始化多语言
  await I18N.init();
  // 高亮插件：主题 CSS + 语言表（失败即降级为纯文本，见 loadHighlightPlugins）
  await loadHighlightPlugins();

  // 应用已保存的语言（翻译 HTML 中的硬编码文案）
  refreshI18nUI();

  // 设置语言菜单
  setupMenuBar();
  setupConfigMenu();

  // 设置右键菜单
  setupContextMenu();
  setupTabContextMenu();

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
  // 记忆面板的菜单接线：绑在启动期而不是面板建起来之后（否则菜单是死按钮，见 memory.js 的 attach）
  window.MemoryUI?.attach();
  window.ServiceUI?.attach();
  window.ProcLogUI?.attach();
  // 外链闸门（前端这一重）：agent 回的链接一律交给系统浏览器，绝不让 WebView 自己导航过去
  window.ExternalLinks?.install();
  setupCapabilityMenu();
  setupOutlineTabs();
  setupTextareaSync();
  setupEditorVirtualScroll();
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

  // 输出标签页：标签页名 + 面板内的固定文案（回到最新/清屏/复制全部）
  if (state.tabs.some((t) => t._isProcLog)) window.ProcLogUI?.relabel();

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
// 标签页管理
// ============================================

/** 标签页图标：专用视图优先，其余按文件名 */
function tabIcon(t) {
  if (t._isConfig) return "⚙️";
  if (t._isHelp) return "🔒";
  if (t._isSession) return "🤖";
  if (t._isTerminal) return "🖥️";
  if (t._isService) return "🔌";
  if (t._isProcLog) return "📜";
  if (t._isMemory) return "🧠";
  // 宽行只读视图：换一把锁 —— 一眼看出"这个文件只是给你看，不给改"
  if (t._wideReadOnly) return "🔒";
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
  hideProcLogView();
  // 服务面板的秒级刷新随面板走：切走了就别再问后端
  window.ServiceUI?.close();
  // 输出标签页的增量轮询同理（pane 留着，切回来内容还在）
  window.ProcLogUI?.blur();
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
  if (tab._isProcLog) {
    // 输出标签页 — 托管进程日志的实时跟随（自己增量读日志，content 恒为空）
    hideEditorView();
    showProcLogView();
    window.ProcLogUI?.render(tab);
    return;
  }
  if (tab._isMemory) {
    // 记忆标签页 — 项目记忆面板（自己拉 mem_*，content 恒为空）
    hideEditorView();
    showMemoryView();
    window.MemoryUI?.render(tab);
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
      // 切回来时容器尺寸可能已经不是当初那个了（面板拖动 / 窗口改过大小），再适配一次
      fitTerminal(tab);
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
  // 切回来：补上打字期间丢掉的着色与大纲（打字时不跑全量，见 refreshEditorChrome）
  refreshEditorChrome(tab);

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

  // 记忆面板：停掉展开的修订链（本面板无轮询）
  if (tab._isMemory) window.MemoryUI?.close();

  // 输出标签页：停轮询 + 释放终端 + 摘掉面板
  if (tab._isProcLog) window.ProcLogUI?.close(tab);

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
    okBtn.textContent = I18N.t("modal.ok") || "OK";
    cancelBtn.textContent = I18N.t("modal.cancel") || "Cancel";
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
    okBtn.textContent = I18N.t("modal.ok") || "OK";
    cancelBtn.textContent = I18N.t("modal.cancel") || "Cancel";
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

// ============================================
// 运行目标
// ============================================

async function loadRunTargets() {
  const list = document.getElementById("run-targets-list");
  if (!list) return;

  const invoke = getTauriInvoke();
  if (!invoke) {
    list.innerHTML = '<span class="run-targets-empty">' + L('Tauri API 不可用', 'Tauri API unavailable') + '</span>';
    return;
  }

  try {
    const projectRoot = state.currentProject?.path || undefined;
    const targets = await invoke("get_run_targets", { projectRoot });

    if (!targets || targets.length === 0) {
      list.innerHTML = `<span class="run-targets-empty">${L("暂无运行目标", "No run targets yet")}</span>`;
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
          ${t.bind ? `<div class="run-target-bind" title="${L('绑定文件', 'bound file')}">🔗 ${escapeHtml(t.bind)}</div>` : ""}
        </div>
        <span class="run-target-edit" title="${L('修改运行目标', 'edit run target')}">&#9998;</span>
        <span class="run-target-del" title="${L('删除运行目标', 'delete run target')}">&#128465;</span>
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
    list.innerHTML = `<span class="run-targets-empty">${L(`加载失败: ${err}`, `Load failed: ${err}`)}</span>`;
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

function showSessionView() {
  showPane("session-view");
}

function hideSessionView() {
  const el = document.getElementById("session-view");
  if (el) el.style.display = "none";
}

function showImageView() {
  showPane("image-view");
}

function hideImageView() {
  document.getElementById("image-view").style.display = "none";
}

function showConfigView() {
  // 注意这里**不能**只关 editor-empty / editor-view：切项目时服务面板会自己刷新并显示，
  // 漏掉它就会两块并排、配置只占一半宽（bug 3）。统一入口一次关干净。
  showPane("config-view");
}

function hideConfigView() {
  const el = document.getElementById("config-view");
  if (el) el.style.display = "none";
}

function showMemoryView() {
  showPane("memory-view");
}

function hideMemoryView() {
  const el = document.getElementById("memory-view");
  if (el) el.style.display = "none";
}

function showServiceView() {
  showPane("service-view");
}

function hideServiceView() {
  const el = document.getElementById("service-view");
  if (el) el.style.display = "none";
}

function showProcLogView() {
  showPane("proc-log-view");
}

function hideProcLogView() {
  const el = document.getElementById("proc-log-view");
  if (el) el.style.display = "none";
}

// ============================================
// 状态栏
// ============================================

function setStatus(message, level = "info") {
  const el = document.querySelector("#statusbar .status-item");
  // 后端来的错误文案是中文，这里**唯一**收口翻一次（见 ui/scripts/errors.js 与 ui-smoke U52）：
  // 显示层是唯一收口，所以翻译放这里就够 —— 不必让 168 处调用点各自记得翻。
  const shown = window.BackendMsg ? BackendMsg.translate(message) : message;
  if (el) {
    el.textContent = shown;
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
