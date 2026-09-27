/**
 * ruyix — 终端（PTY 标签页 + 尺寸自适应）
 *
 * 这个模块管：终端标签页的创建与 PTY 事件转发、终端目标列表的渲染、以及「模拟终端跟
 * 着窗口变」的尺寸自适应（量单元格 → 算行列 → 同改 xterm 与 PTY winsize）。
 *
 * 经典脚本（非 ES module）：与 main.js 共享同一个全局作用域，顶层 function 声明仍是
 * 全局的（initApp 按原次序裸名调用）；应用状态一律读 window.state（main.js 里显式导出）。
 * 门禁：ui-smoke U31/U51/U56。
 */


/**
 * 渲染终端输出到标签页
 */
function renderTerminalOutput(tab, text) {
  // 与代码渲染同一个入口：长输出（几千行）同样只画窗口
  setEditorContent({
    texts: text.split("\n"),
    spans: null,
    text: text,
    lineClass: "terminal-line",
    readOnly: true,
  });
  tab.content = text; // 同步更新 tab 内容，确保切换标签页后输出不丢失
}

// ============================================
// 终端资源列表
// ============================================

/**
 * 渲染**用户添加的**终端目标（`ruyix.code.term.*`，与运行目标同一套形状）。
 *
 * 为什么要有它（bug 4）：终端列表以前是 `index.html` 里写死的几条 —— 想加一个自己的环境
 * （某个 venv 的 shell、某台机器的 ssh、带一长串参数的工具 shell）只能去改文件。
 * 现在用户加的条目落进项目配置，面板里增删改，点一下就是一条 PTY。
 *
 * 数据来源：`get_term_targets`（后端按 `term.toml` 分组扫出来，与 `get_run_targets` 同一份扫描器）。
 * 写入全部走命令系统（`term add` / `term del`），这里只读、只画。
 */
async function renderTerminalTargets() {
  const list = document.getElementById("terminal-list");
  const invoke = getTauriInvoke();
  if (!list || !invoke) return;
  let targets = [];
  try {
    targets = (await invoke("get_term_targets", {
      projectRoot: window.state.currentProject?.path || undefined,
    })) || [];
  } catch {
    targets = [];   // 读不到就只留内置那几条（不打扰用户）
  }
  list.querySelectorAll(".terminal-user").forEach((el) => el.remove());
  for (const t of targets) {
    const li = document.createElement("li");
    li.className = "terminal-user";
    li.dataset.cmd = t.cmd || "";
    li.dataset.name = t.name || t.key;
    li.dataset.key = t.key;
    li.innerHTML =
      `<span class="terminal-name">${escapeHtml(t.name || t.key)}</span>` +
      `<span class="terminal-new-window" title="${escapeHtml(I18N.t("terminal.new_window"))}">🪟</span>` +
      `<span class="terminal-edit" title="${escapeHtml(I18N.t("terminal.edit"))}">&#9998;</span>` +
      `<span class="terminal-del" title="${escapeHtml(I18N.t("terminal.del"))}">&#128465;</span>`;
    li.querySelector(".terminal-edit")?.addEventListener("click", async (e) => {
      e.stopPropagation();
      const name = await showPrompt(I18N.t("terminal.edit_name"), t.name || t.key);
      if (!name) return;
      const cmd = await showPrompt(I18N.t("terminal.edit_cmd"), t.cmd || "");
      if (!cmd) return;
      await handleCommand(`term add ${name}=${cmd}`);
    });
    li.querySelector(".terminal-del")?.addEventListener("click", async (e) => {
      e.stopPropagation();
      await handleCommand(`term del ${t.key}`);
    });
    list.appendChild(li);
  }
}

/** 命令层 / 面板刷新钩子（`term list` 走这里） */
window.refreshTerminalTargets = () => renderTerminalTargets();

function setupTerminalList() {
  const list = document.getElementById("terminal-list");
  if (!list) return;

  // ➕ 添加终端（bug 4）：先问名字再问命令，两步 showPrompt —— 与「运行目标」的编辑一个手感。
  // 写盘走命令系统（`term add 名字=命令`），这里不直接 invoke。
  document.getElementById("terminal-add")?.addEventListener("click", async () => {
    if (!window.state.currentProject) {
      setStatus(I18N.t("terminal.no_project"), "error");
      return;
    }
    const name = await showPrompt(I18N.t("terminal.add_name"), "");
    if (!name) return;
    const cmd = await showPrompt(I18N.t("terminal.add_cmd"), "");
    if (!cmd) return;
    await handleCommand(`term add ${name}=${cmd}`);
  });

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
    await invoke("spawn_terminal", { cmd, projectRoot: window.state.currentProject?.path });
    setStatus(I18N.t("terminal.spawned", { name }));
  } catch (err) {
    setStatus(I18N.t("terminal.spawn_fail", { err }), "error");
  }
}

/**
 * 终端自适应（模拟终端必须跟着窗口变）。
 *
 * 病根：`new Terminal({rows:24, cols:100})` 只在创建时定死尺寸，之后**没有任何 resize 路径**
 * —— 前端从不调用后端已有的 `pty_resize`，也没有观察容器尺寸。于是 `.xterm-screen` 永远是
 * 100×24 的像素块（fontSize 13 → 763×368），窗口再大也只用左下角一块，窗口变小还会把外层撑出
 * 横向滚动条；PTY 的 winsize 也是旧的，shell 按旧宽度折行 → `ls` 输出错位。
 *
 * 三段拆开，各管一件事：量单元格（[`cellMetrics`]）→ 算行列（[`fitDims`]，纯函数，好测）→
 * 落尺寸（[`fitTerminal`]：同时改 xterm 与 PTY）。
 */

/** 单元格像素尺寸。优先问 xterm 自己的 render service（FitAddon 也是这么拿的），
 *  拿不到就量它维护的单字符测量元素。两条都没有就返回 null —— 量不出来就别乱改尺寸。 */
function cellMetrics(term) {
  const cell = term?._core?._renderService?.dimensions?.css?.cell;
  if (cell && cell.width > 0 && cell.height > 0) {
    return { w: cell.width, h: cell.height };
  }
  const el = term?.element?.querySelector?.(".xterm-char-measure-element");
  if (el) {
    const r = el.getBoundingClientRect();
    if (r.width > 0 && r.height > 0) return { w: r.width, h: r.height };
  }
  return null;
}

/** 可容纳的行列数（纯函数）。至少 2 列 1 行：0 会让 xterm 抛错，把终端直接搞死。 */
function fitDims(availW, availH, cell) {
  const cols = Math.max(2, Math.floor(availW / cell.w));
  const rows = Math.max(1, Math.floor(availH / cell.h));
  return { cols, rows };
}

/** 把某个终端标签调整到容器大小。**幂等**：行列没变就什么都不做（避免无谓的 SIGWINCH 重绘）。
 *  返回 true 表示这次真的改了尺寸。 */
function fitTerminal(tab) {
  const term = tab && tab._term;
  const container = document.getElementById("terminal-container");
  if (!term || !container || !container.isConnected) return false;
  const cell = cellMetrics(term);
  if (!cell) return false;
  // 可用像素 = 容器尺寸 − `.xterm` 自己的 padding − 回滚条宽度（都量，不猜）
  const box = container.getBoundingClientRect();
  let padW = 0;
  let padH = 0;
  if (term.element && typeof getComputedStyle === "function") {
    const cs = getComputedStyle(term.element);
    padW = (parseFloat(cs.paddingLeft) || 0) + (parseFloat(cs.paddingRight) || 0);
    padH = (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0);
  }
  const vp = term.element?.querySelector?.(".xterm-viewport");
  const scrollbar = vp ? Math.max(0, vp.offsetWidth - vp.clientWidth) : 0;
  const availW = box.width - padW - scrollbar;
  const availH = box.height - padH;
  // 标签页被切走时容器是 display:none（尺寸为 0）：这时**别动**，等切回来再量
  if (availW <= 0 || availH <= 0) return false;
  const { cols, rows } = fitDims(availW, availH, cell);
  if (cols === term.cols && rows === term.rows) return false;
  term.resize(cols, rows);
  // **PTY 也要跟着改**：不改的话 shell 仍按旧宽度折行（`ls` 的输出会错位）。
  // 注意 Tauri 的 invoke 形参名：后端是 `tab_id`，JS 侧必须写 `tabId`。
  const invoke = getTauriInvoke();
  if (invoke && tab.id) {
    Promise.resolve(invoke("pty_resize", { tabId: tab.id, rows, cols })).catch(() => {});
  }
  return true;
}

/** 尺寸变化 → 重新适配。容器是共用的一个，所以只盯**当前激活的**终端标签。
 *  ResizeObserver 覆盖了窗口缩放、面板拖动、标签切换（容器尺寸都会变）。 */
let terminalFitObserver = null;
let terminalFitTimer = null;
function scheduleTerminalFit() {
  if (terminalFitTimer) clearTimeout(terminalFitTimer);
  // 拖拽窗口时尺寸事件很密：延迟一点，等停下来再算（每次 resize 都会让 shell 重画）
  terminalFitTimer = setTimeout(() => {
    terminalFitTimer = null;
    const tab = window.state.tabs.find((t) => t.id === window.state.activeTabId && t._isTerminal);
    if (tab) fitTerminal(tab);
  }, 60);
}
function watchTerminalResize() {
  if (terminalFitObserver || typeof ResizeObserver !== "function") return;
  const container = document.getElementById("terminal-container");
  if (!container) return;
  terminalFitObserver = new ResizeObserver(() => scheduleTerminalFit());
  terminalFitObserver.observe(container);
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
  window.state.tabs.push(tab);
  renderTabs();
  switchTab(tabId);

  // 隐藏编辑器，显示终端容器
  showTerminalView();

  setStatus(I18N.t("terminal.starting", { name }));

  try {
    // 启动 PTY
    await invoke("pty_spawn", { cmd, tabId, projectRoot: window.state.currentProject?.path });

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

    // **打开就按容器实际大小定一次尺寸**，并开始盯尺寸变化（见 fitTerminal）：
    // 不这么做，终端就停在构造时的 100×24，窗口多大都只用左下角一块。
    fitTerminal(tab);
    watchTerminalResize();

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
  showPane("terminal-view");   // 统一入口：其余面板一起关掉（bug 3）
}

function hideTerminalView() {
  hidePane("terminal-view");
}
// ============================================
// 模块导出（经典脚本：这些函数本身也仍是全局的，见文件头）
// ============================================

window.TerminalUI = {
  setupTerminalList,
  renderTerminalTargets,
  renderTerminalOutput,
  spawnTerminal,
  spawnInNewWindow,
  fitTerminal,
  cellMetrics,
  fitDims,
  scheduleTerminalFit,
  watchTerminalResize,
  listenToPty,
  listenToPtyExit,
  showTerminalView,
  hideTerminalView,
};
