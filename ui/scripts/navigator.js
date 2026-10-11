/**
 * ruyix — 导航区（项目列表 + 文件树）
 *
 * 这个模块管：导航区的两种形态（无项目时的项目列表 / 有项目时的文件树）、项目语言图标、
 * 项目编辑弹窗、IDE 状态桶（暂存/备份/会话），以及启动时自动打开上次项目。
 *
 * 经典脚本（非 ES module）：与 main.js 共享同一个全局作用域，顶层 function 声明仍是
 * 全局的（initApp 按原次序裸名调用）；应用状态一律读 window.state（main.js 里显式导出）。
 * 门禁：ui-smoke U1/U48/U56。
 */


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
  window.state.tabs = [];
  window.state.activeTabId = null;
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
  if (window.state.currentProject) {
    loadFileTree(window.state.currentProject.path);
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
};

/** 语言 key → 图标 */
function projectLangIcon(lang) {
  const entry = PROJECT_LANGS.find((l) => l.key === lang);
  return entry ? entry.icon : PROJECT_LANGS[0].icon;
}

/**
 * IDE 状态桶（v1.0.0）：项目侧的暂存 / 备份 / 会话 / 验证产物都在**便携根**的
 * `projects/<key>/` 里 —— 它们属于 IDE，不属于项目，所以不在用户仓库里。
 *
 * 这里把它们列出来（体积 + "是不是孤儿"），并给一个整桶删除的入口。
 * 删除走**命令系统**（`bucket delete <key>`，确认框在命令处理器里），不在这里直接 invoke ——
 * 界面上的写操作只许有一条道，否则 AI 集成就会出现盲点。
 */
async function renderProjectBuckets(list) {
  const invoke = getTauriInvoke();
  if (!invoke) return;

  let buckets = [];
  let known = new Set();
  try {
    buckets = (await invoke("project_buckets")) || [];
    known = new Set(
      ((await invoke("get_projects")) || []).map((p) => String(p.path || "").toLowerCase())
    );
  } catch (err) {
    list.insertAdjacentHTML(
      "beforeend",
      `<div class="bucket-section">
        <div class="bucket-header">${escapeHtml(I18N.t("bucket.title"))}</div>
        <span class="project-list-empty">${escapeHtml(I18N.t("bucket.load_fail", { err }))}</span>
      </div>`
    );
    return;
  }

  const humanBytes = (n) => {
    let v = Number(n) || 0;
    const units = ["B", "KB", "MB", "GB"];
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i += 1;
    }
    return `${i ? v.toFixed(1) : v.toFixed(0)} ${units[i]}`;
  };

  const rows = buckets
    .map((b) => {
      const claimed = b.project_path || "";
      // 孤儿 = 桶还在，但它自证的那个项目不在项目列表里（或那个路径已经不见了）
      const orphan = !claimed || !b.exists || !known.has(claimed.toLowerCase());
      const badge = !claimed
        ? I18N.t("bucket.orphan")
        : !b.exists
          ? I18N.t("bucket.missing")
          : orphan
            ? I18N.t("bucket.orphan")
            : "";
      return `<div class="bucket-list-item" data-key="${escapeHtml(b.key)}">
          <span class="bucket-key" title="${escapeHtml(b.path)}">${escapeHtml(b.key)}</span>
          <span class="bucket-size">${humanBytes(b.bytes)}</span>
          ${badge ? `<span class="bucket-badge">${escapeHtml(badge)}</span>` : ""}
          <span class="bucket-del" title="${escapeHtml(I18N.t("bucket.del"))}">🗑️</span>
        </div>`;
    })
    .join("");

  const body = buckets.length
    ? `<div class="bucket-list">${rows}</div>`
    : `<span class="project-list-empty">${escapeHtml(I18N.t("bucket.empty"))}</span>`;
  list.insertAdjacentHTML(
    "beforeend",
    `<div class="bucket-section">
      <div class="bucket-header" title="${escapeHtml(I18N.t("bucket.hint"))}">${escapeHtml(
        I18N.t("bucket.title")
      )}</div>
      ${body}
    </div>`
  );

  list.querySelectorAll(".bucket-list-item").forEach((el) => {
    const del = el.querySelector(".bucket-del");
    if (!del) return;
    del.addEventListener("click", async (e) => {
      e.stopPropagation();
      await handleCommand(`bucket delete ${el.dataset.key}`);
    });
  });
}

/** 命令层刷新钩子：`bucket list` 之后重新渲染项目面板（与 `loadProjectList` 同一份渲染） */
window.refreshProjectBuckets = () => loadProjectList();

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
      // 没有项目也要列状态桶：全成孤儿正是最该被看见的情况
      await renderProjectBuckets(list);
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

    // 项目列表下面接一段「IDE 状态桶」：项目侧状态都住在便携根里（v1.0.0），看得见、收得掉
    await renderProjectBuckets(list);
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
    okBtn.textContent = I18N.t("modal.ok") || "OK";
    cancelBtn.textContent = I18N.t("modal.cancel") || "Cancel";

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
    tree.innerHTML = '<span class="file-tree-placeholder">' + L('Tauri API 不可用', 'Tauri API unavailable') + '</span>';
    return;
  }

  try {
    const entries = await invoke("list_dir", { path: dirPath });

    // 项目根目录本身也渲染为根节点，右键可在根目录下创建文件/文件夹
    const rootEntry = {
      path: dirPath,
      name: window.state.currentProject?.name || (dirPath.split(/[/\\]/).pop() || dirPath),
      is_dir: true
    };
    const rootNode = renderTreeEntry(rootEntry, tree, 0);
    const rootChildren = rootNode.nextElementSibling;
    if (entries.length === 0) {
      const ph = document.createElement("span");
      ph.className = "file-tree-placeholder";
      ph.textContent = L("空目录", "Empty folder");
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
    tree.innerHTML = `<span class="file-tree-placeholder">${L(`读取失败: ${err}`, `Read failed: ${err}`)}</span>`;
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
      ph.textContent = L("空目录", "Empty folder");
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
  // **悬停显示全名 / 完整路径**：行按可视宽度排、长名走省略号（导航区不做横向滚动 ——
  // 试过两版横滚都被否：钉住按钮会压字、内容撑宽会抖），被省略号吃掉的部分只能靠 title 补。
  node.title = entry.path || entry.name;

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
// 模块导出（经典脚本：这些函数本身也仍是全局的，见文件头）
// ============================================

window.NavigatorUI = {
  setupNavigatorTabs,
  setNavigatorMode,
  showWelcomePage,
  showProjectWorkspace,
  loadWelcome,
  loadProjectList,
  renderProjectBuckets,
  showProjectEditModal,
  projectLangIcon,
  loadFileTree,
  restoreExpandedPaths,
  refreshTreeNode,
  renderTreeEntry,
  toggleTreeNode,
  autoOpenLastProject,
};
