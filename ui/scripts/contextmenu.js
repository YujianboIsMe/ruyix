/**
 * ruyix — 右键菜单（文件树 + 编辑器标签栏）
 *
 * 这个模块管：文件树/标签栏右键菜单的构建与显示、菜单动作（复制路径 / 复制绝对路径 /
 * 运行 / 试跑 / 新建 / 重命名 / 删除），以及标签页的关闭策略（纯函数 closePlan）。
 *
 * 经典脚本（非 ES module）：与 main.js 共享同一个全局作用域，顶层 function 声明仍是
 * 全局的（initApp 按原次序裸名调用）；应用状态一律读 window.state（main.js 里显式导出）。
 * 门禁：ui-smoke U33/U45/U56。
 */


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

/**
 * 标签页右键菜单（编辑器多标签）。
 *
 * 为什么要自己实现：WebView 默认在**整块文档**上弹系统右键菜单（后退 / 刷新 / 检查元素…）。
 * 文件树早就拦掉了（`setupContextMenu`），标签栏没拦 —— 于是编辑器多标签上右键出来的是系统那套，
 * 与 IDE 的标签操作无关。这里在 `contextmenu` 上拦掉默认菜单，换成自家菜单；菜单项全部走**既有**
 * 的 `closeTab` / `handleContextCopyPath`，不新增后端调用。
 *
 * 两条不变量：
 *   ① 目标标签由**事件命中的那个**决定，绝不改激活标签 —— 否则"关闭其他"会把用户刚右键的
 *      那一个也关掉（最容易被忽略、也最难受的边界）；
 *   ② 关闭策略收敛在纯函数 [`closePlan`] 里（"关闭右侧"在末尾、"关闭其他"只有一个标签时
 *      都必须得到空计划），这样边界能在 ui-smoke 里逐条回放。
 */

/** 关闭策略（纯函数）：给定标签序列与目标，算出要关掉哪些 id。 */
function closePlan(ids, targetId, mode) {
  const i = ids.indexOf(targetId);
  if (i < 0) return [];
  switch (mode) {
    case "close":
      return [targetId];
    case "others":
      return ids.filter((id) => id !== targetId);
    case "right":
      return ids.slice(i + 1);
    case "left":
      return ids.slice(0, i);
    case "all":
      return ids.slice();
    default:
      return [];
  }
}

function setupTabContextMenu() {
  const menu = document.getElementById("tab-context-menu");
  const bar = document.getElementById("tab-bar");
  if (!menu || !bar) return;
  let targetId = null;

  bar.addEventListener("contextmenu", (e) => {
    // 标签栏整块都拦（空白处右键也不该弹系统菜单）；点了具体标签才开菜单
    e.preventDefault();
    e.stopPropagation();
    const el = e.target && e.target.closest ? e.target.closest(".tab-item") : null;
    if (!el || !el.dataset.tabId) {
      hideContextMenu(menu);
      return;
    }
    targetId = el.dataset.tabId;
    const ids = window.state.tabs.map((t) => t.id);
    const target = window.state.tabs.find((t) => t.id === targetId);
    menu.querySelectorAll("[data-tab-action]").forEach((row) => {
      const mode = row.dataset.tabAction;
      // **每一项恒在**，干不了的那几项**置灰禁用**而不是消失 —— 菜单形状稳定、位置记得住。
      // （旧实现把"计划为空"的那项整条隐藏，于是"右键最左标签时【关闭左侧】不见了"被当成
      // 缺功能报上来：菜单项会随标签位置忽隐忽现，用户记住的位置下一秒就没了。）
      const usable =
        mode === "copy-path" || mode === "copy-full-path"
          ? !!(target && target.path)
          : closePlan(ids, targetId, mode).length > 0;
      row.style.display = "";
      row.classList.toggle("context-menu-item--disabled", !usable);
      row.setAttribute("aria-disabled", usable ? "false" : "true");
    });
    menu.style.left = e.clientX + "px";
    menu.style.top = e.clientY + "px";
    menu.style.display = "";
  });

  menu.addEventListener("click", async (e) => {
    const item = e.target && e.target.closest ? e.target.closest("[data-tab-action]") : null;
    if (!item) return;
    // 禁用项点了什么都不做（CSS 的 pointer-events:none 挡在前面，这里是兜底）
    if (item.classList.contains("context-menu-item--disabled")) return;
    e.stopPropagation();
    const mode = item.dataset.tabAction;
    hideContextMenu(menu);
    const tab = window.state.tabs.find((t) => t.id === targetId);
    if (!tab) return;
    if (mode === "copy-path" || mode === "copy-full-path") {
      await handleContextCopyPath(tab.path, mode === "copy-full-path");
      return;
    }
    // 计划先算好再逐个关：closeTab 会改 state.tabs，边遍历边算会漏
    for (const id of closePlan(window.state.tabs.map((t) => t.id), targetId, mode)) closeTab(id);
  });

  // 点菜单外关掉（与文件树那套一致）
  document.addEventListener("click", () => hideContextMenu(menu));
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
      _ctxIsRoot = samePath(_ctxPath, window.state.currentProject?.path);
    } else if (window.state.currentProject) {
      _ctxPath = window.state.currentProject.path;
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
      case "copy-path":
        await handleContextCopyPath(_ctxPath, false);
        break;
      case "copy-full-path":
        await handleContextCopyPath(_ctxPath, true);
        break;
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

/**
 * 折叠"多余的"分隔线（2026-09-28）。
 *
 * 为什么需要它：分隔线在 HTML 里是**静态写死**的，而菜单项会按上下文隐藏（文件 vs 文件夹、
 * 项目根 vs 子目录）。于是"中间两个条目一藏，两条线就贴到一起" —— 菜单里出现两根并列的灰条
 * （用户报「there must be only 1 bar」）。
 *
 * 规矩：**可见序列里，分隔线只许出现在两个可见菜单项之间**。所以按可见邻居判：
 * - 前面没有可见项（开头）、或前面紧邻的就是另一条分隔线 ⇒ 多余；
 * - 后面没有可见项（收尾）⇒ 只是装饰，也去掉（看着像漏了一块）。
 * 相邻两条只留**前面**那条 —— 两条之间什么都没夹，留一条就够。
 */
function collapseSeparators(menu) {
  const kids = Array.from(menu.children);
  const shown = (el) => el.style.display !== "none";
  const isSep = (el) => el.classList.contains("context-menu-sep");
  // 先把线摆回"显示"，再按可见邻居决定去留（上一轮可能把它们藏过）
  kids.forEach((el) => {
    if (isSep(el)) el.style.display = "";
  });
  let kept = false; // 上一条可见元素是不是"已留下的分隔线"
  kids.forEach((el, i) => {
    if (!shown(el)) return;
    if (!isSep(el)) {
      kept = false;
      return;
    }
    const prev = kids.slice(0, i).filter(shown).pop();
    const rest = kids.slice(i + 1).filter(shown);
    const okHere =
      !kept &&
      prev &&
      !isSep(prev) &&
      rest.some((x) => !isSep(x));
    if (!okHere) el.style.display = "none";
    else kept = true;
  });
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
  } else if (_ctxPath && window.state.currentProject) {
    // 查询该文件的执行状态
    try {
      const invoke = getTauriInvoke();
      if (invoke) {
        const status = await invoke("get_execute_status", {
          path: _ctxPath,
          projectRoot: window.state.currentProject.path
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

  // 分隔线**最后**按可见性折叠：文件/文件夹条目是在上面那些分支里才隐藏的，
  // 早一步折叠等于什么都没做（实测：文件夹菜单照样两根灰条并排）。
  collapseSeparators(menu);

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
      projectRoot: window.state.currentProject?.path
    });

    // 情况1: 已有目标绑定该文件 → 执行（多脚本时取先命中的一个，通常按 dev/start 排序）
    if (status.has_target && status.target_name) {
      const targets = await invoke("get_run_targets", { projectRoot: window.state.currentProject?.path });
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
    setStatus(L("运行失败: ", "Run failed: ") + err, "error");
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
      setStatus(L("试跑失败: AI 返回为空，请重试", "Try-run failed: the AI returned nothing, please retry"), "error");
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
      setStatus(L("已记住文件名: ", "Remembered file name: ") + fileName + " → " + cmd);
    } else if (upper.startsWith("YES")) {
      // 可运行 — 提取命令模板
      let cmdTemplate = upper.startsWith("YES|") ? result.slice(result.indexOf("|") + 1).trim() : ("python {file}");
      const cmd = cmdTemplate.replace(/\{file\}/gi, fullPath);
      await invoke("set_execute_entry", { path: fullPath, canRun: true, asFile: false });
      await autoCreateRunTarget(fileName, cmd, fullPath);
      setStatus(L("已记住并创建运行目标: .", "Remembered and created a run target: .") + ext + " → " + cmd);
    } else if (upper.startsWith("CONDITIONAL")) {
      const detail = upper.startsWith("CONDITIONAL|") ? result.slice(result.indexOf("|") + 1).trim() : result;
      setStatus(L("功能待开发: ", "Not implemented yet: ") + detail, "error");
    } else {
      // NO 或其他
      await invoke("set_execute_entry", { path: fullPath, canRun: false, asFile: false });
      setStatus(L("已记住: .", "Remembered: .") + ext + L(" 不可运行 (AI: ", " cannot be run (AI: ") + result.slice(0, 60) + ")");
    }
  } catch (err) {
    setStatus(L("试跑失败: ", "Try-run failed: ") + err, "error");
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
  if (!invoke || !window.state.currentProject || !specs || specs.length === 0) return [];

  const relPath = toRelativePath(fullPath);
  const dirName = relPath.split(/[/\\]/).slice(-2, -1)[0] || "";

  // 既有目标名与最大索引
  let index = 0;
  const used = new Set();
  try {
    const targets = (await invoke("get_run_targets", { projectRoot: window.state.currentProject.path })) || [];
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
    setStatus(L("自动创建运行目标失败: ", "Failed to create run targets: ") + err, "error");
  }
  return created;
}

/** 全路径 → 相对于项目根的路径 */
function toRelativePath(fullPath) {
  if (!window.state.currentProject) return fullPath;
  // 分隔符归一化只用于**比较**：后端 list_dir 在 Windows 上给反斜杠，而项目根也可能来自
  // 正斜杠的输入。不归一化就会静默退化成"复制绝对路径"——不报错，只是复制的东西不对。
  const norm = (p) => p.replace(/\//g, "\\").replace(/\\+$/, "");
  const rawRoot = window.state.currentProject.path.replace(/[/\\]+$/, "");
  const rawFull = (fullPath || "").replace(/[/\\]+$/, "");
  if (norm(rawFull) === norm(rawRoot)) return "";
  // 归一化是等长替换，切片下标可以直接用在原串上 —— 复制出来的仍是原始分隔符
  return norm(rawFull).startsWith(norm(rawRoot) + "\\")
    ? rawFull.slice(rawRoot.length + 1)
    : fullPath;
}

/** 写剪贴板。优先 async clipboard，失败退回 execCommand（非安全上下文里没有前者） */
async function copyToClipboard(text) {
  const nav = typeof navigator !== "undefined" ? navigator : null;
  try {
    if (nav && nav.clipboard && typeof nav.clipboard.writeText === "function") {
      await nav.clipboard.writeText(text);
      return true;
    }
  } catch {
    // 权限被拒 / 无焦点：走下面兜底
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.top = "-1000px";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok === true;
  } catch {
    return false;
  }
}

/**
 * 右键"复制路径 / 复制绝对路径"
 * @param {string} fullPath 树节点上的绝对路径
 * @param {boolean} absolute true=完整磁盘路径，false=相对项目根（项目根本身复制成 "."）
 */
async function handleContextCopyPath(fullPath, absolute) {
  const text = absolute ? fullPath : (toRelativePath(fullPath) || ".");
  const ok = await copyToClipboard(text);
  if (ok) setStatus(I18N.t("ctx.copied", { path: text }));
  else setStatus(I18N.t("ctx.copy_fail", { path: text }), "error");
  return ok;
}

function hideContextMenu(menu) {
  menu.style.display = "none";
}

async function deleteFileOrFolder(fullPath) {
  const ok = await showConfirm('删除', I18N.t('cmd.delete.confirm', { path: fullPath }));
  if (!ok) return;
  await handleCommand('del ' + toRelativePath(fullPath), true);
}

async function renameFileOrFolder(fullPath) {
  const oldName = fullPath.split(/[/\\]/).pop() || fullPath;
  const newName = await showPrompt(L('重命名', 'Rename'), oldName);
  if (!newName || newName === oldName) return;
  await handleCommand('rename ' + toRelativePath(fullPath) + ' ' + newName, true);
}

async function createFileInFolder(fullPath) {
  const name = await showPrompt(L('新建文件', 'New file'), '');
  if (!name) return;
  const rel = toRelativePath(fullPath);
  await handleCommand('new file ' + (rel ? rel + '\\' : '') + name, true);
}

async function createFolderInFolder(fullPath) {
  const name = await showPrompt(L('新建文件夹', 'New folder'), '');
  if (!name) return;
  const rel = toRelativePath(fullPath);
  await handleCommand('new folder ' + (rel ? rel + '\\' : '') + name, true);
}
// ============================================
// 模块导出（经典脚本：这些函数本身也仍是全局的，见文件头）
// ============================================

window.ContextMenuUI = {
  setupContextMenu,
  setupTabContextMenu,
  showContextMenu,
  hideContextMenu,
  handleContextRun,
  handleContextTryRun,
  autoCreateRunTarget,
  createRunTargets,
  toRelativePath,
  copyToClipboard,
  handleContextCopyPath,
  samePath,
  closePlan,
  deleteFileOrFolder,
  renameFileOrFolder,
  createFileInFolder,
  createFolderInFolder,
};
