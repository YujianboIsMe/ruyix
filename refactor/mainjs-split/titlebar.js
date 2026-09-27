/**
 * ruyix — 标题栏（窗口控制 + 项目切换 + 标题）
 *
 * 这个模块管：最小化/最大化/关闭三个窗口按钮、标题栏项目名与路径的自适应显示、
 * 点项目名弹出的快速切换下拉，以及操作系统窗口标题（Alt+Tab / 任务栏）。
 *
 * 经典脚本（非 ES module）：与 main.js 共享同一个全局作用域，顶层 function 声明仍是
 * 全局的（initApp 按原次序裸名调用）；应用状态一律读 window.state（main.js 里显式导出）。
 * 门禁：ui-smoke U1/U56/U57。
 */


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

    const cur = window.state.currentProject ? window.state.currentProject.path : null;
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

  if (window.state.currentProject) {
    titleEl.textContent = window.state.currentProject.path;
    titleEl.classList.add("has-project");
  } else {
    titleEl.textContent = "ruyix";
    titleEl.classList.remove("has-project");
  }

  // 项目切换下拉箭头：仅打开项目时显示
  const arrow = document.getElementById("project-switch-arrow");
  if (arrow) arrow.style.display = window.state.currentProject ? "" : "none";
}

/**
 * 更新操作系统窗口标题（Alt+Tab / 任务栏可见）。
 * 打开项目时显示项目名，否则显示 ruyix。
 */
async function updateWindowTitle() {
  const win = getTauriWindow();
  if (!win || typeof win.setTitle !== "function") return;
  try {
    const title = window.state.currentProject
      ? window.state.currentProject.name
      : "ruyix";
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
    if (!window.state.currentProject) {
      titleEl.textContent = "ruyix";
      return;
    }

    // 可用宽度 = 标题栏中间区域的宽度 - 内边距
    const available = centerEl.clientWidth - 24;
    if (available <= 0) return;

    // 测量完整路径的像素宽度
    measureEl.textContent = window.state.currentProject.path;
    const fullWidth = measureEl.getBoundingClientRect().width;

    if (fullWidth > available) {
      // 空间不够 → 只显示项目名
      titleEl.textContent = window.state.currentProject.name;
    } else {
      // 空间足够 → 显示完整路径
      titleEl.textContent = window.state.currentProject.path;
    }
  };

  new ResizeObserver(update).observe(centerEl);
}
// ============================================
// 模块导出（经典脚本：这些函数本身也仍是全局的，见文件头）
// ============================================

window.TitleBarUI = {
  setupWindowControls,
  setupResponsiveTitlebar,
  setupProjectSwitcher,
  updateTitlebarTitle,
  updateWindowTitle,
  updateMaximizeIcon,
};
