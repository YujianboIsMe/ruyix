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
    case "refresh":
      await handleRefreshCommand(parts.slice(1));
      break;
    case "del":
    case "delete":
    case "remove":
    case "rm":
      await handleDeleteCommand(raw, _fromAi);
      break;
    case "rename":
    case "mv":
      await handleRenameCommand(parts.slice(1));
      break;
    case "run":
      await handleRunCommand(raw);
      break;
    case "help":
      openHelp();
      break;
    case "agent":
      if (!state.currentProject) {
        setStatus(I18N.t("cmd.agent.no_project"), "error");
        break;
      }
      window.SessionUI?.handleCommand(parts.slice(1).join(" "));
      break;
    case "mcp":
      window.McpUI?.handleCommand(parts.slice(1).join(" "));
      break;
    case "a2a":
      window.A2aUI?.handleCommand(parts.slice(1).join(" "));
      break;
    case "tools":
      window.ToolsUI?.handleCommand(parts.slice(1).join(" "));
      break;
    case "skill":
    case "skills":
      window.SkillsUI?.handleCommand(parts.slice(1).join(" "));
      break;
    case "git":
      await handleGitCommand(raw, _fromAi);
      break;
    case "project":
      await handleProjectCommand(raw);
      break;
    default:
      // 标准命令未命中 → 调用 AI 翻译（防止递归）
      if (!_fromAi) {
        await handleAiCommand(raw);
      } else {
        setStatus(I18N.t("status.unknown_cmd", { verb }), "error");
      }
  }
}

/**
 * run 命令：快捷添加运行目标
 * 语法: run <name>=<cmd>
 * 等价于:
 *   config add -p ruyix.code.run.target<N>.cmd=<cmd>
 *   config add -p ruyix.code.run.target<N>.name=<name>
 * 其中 <N> = 现有运行目标数量
 */
async function handleRunCommand(raw) {
  const rest = raw.slice("run".length).trim();
  if (!rest) {
    setStatus(I18N.t("cmd.run.usage"));
    return;
  }

  // ============================================
  // 删除语法: run del/delete/remove/rm <name>
  // ============================================
  const delMatch = rest.match(/^(del|delete|remove|rm)\s+(.+)$/i);
  if (delMatch) {
    const targetName = delMatch[2].trim();
    if (!targetName) {
      setStatus(I18N.t("cmd.run.del_usage"), "error");
      return;
    }
    if (!state.currentProject) {
      setStatus(I18N.t("cmd.run.no_project"), "error");
      return;
    }
    const invoke = getTauriInvoke();
    if (!invoke) {
      setStatus(I18N.t("status.tauri_unavail"));
      return;
    }
    try {
      const projectRoot = state.currentProject?.path || undefined;
      const targets = await invoke("get_run_targets", { projectRoot });
      // 按 name 或 key 查找
      const target = (targets || []).find(
        (t) => t.name === targetName || t.key === targetName
      );
      if (!target) {
        setStatus(I18N.t("cmd.run.del_not_found", { name: targetName }), "error");
        return;
      }
      const cmdKey = `ruyix.code.run.${target.key}.cmd`;
      const nameKey = `ruyix.code.run.${target.key}.name`;
      const bindKey = `ruyix.code.run.${target.key}.bind`;
      await executeConfigAction("remove", "p", cmdKey);
      await executeConfigAction("remove", "p", nameKey);
      // bind 可能不存在，忽略错误
      try { await executeConfigAction("remove", "p", bindKey); } catch {}
      setStatus(I18N.t("cmd.run.del_ok", { name: targetName }));
      loadRunTargets();
    } catch (err) {
      setStatus(I18N.t("cmd.run.del_fail", { err }), "error");
    }
    return;
  }

  // ============================================
  // 编辑语法: run edit <name>
  // ============================================
  const editMatch = rest.match(/^edit\s+(.+)$/i);
  if (editMatch) {
    const targetName = editMatch[1].trim();
    if (!state.currentProject) {
      setStatus(I18N.t("cmd.run.no_project"), "error");
      return;
    }
    const invoke = getTauriInvoke();
    if (!invoke) {
      setStatus(I18N.t("status.tauri_unavail"));
      return;
    }
    try {
      const projectRoot = state.currentProject?.path || undefined;
      const targets = await invoke("get_run_targets", { projectRoot });
      const target = (targets || []).find(
        (t) => t.name === targetName || t.key === targetName
      );
      if (!target) {
        setStatus(I18N.t("cmd.run.del_not_found", { name: targetName }), "error");
        return;
      }

      const cmdKey = `ruyix.code.run.${target.key}.cmd`;
      const nameKey = `ruyix.code.run.${target.key}.name`;

      // 提示用户输入新名称
      const newName = await showPrompt("修改运行目标名称", target.name);
      if (!newName) return;

      // 提示用户输入新命令
      const newCmd = await showPrompt("修改运行目标命令", target.cmd || "");
      if (!newCmd) return;

      await executeConfigAction("update", "p", nameKey, newName);
      await executeConfigAction("update", "p", cmdKey, newCmd);
      setStatus(I18N.t("cmd.run.edit_ok", { name: newName }));
      loadRunTargets();
    } catch (err) {
      setStatus(I18N.t("cmd.run.edit_fail", { err }), "error");
    }
    return;
  }

  // ============================================
  // 添加语法: run <name>=<cmd>
  // ============================================
  const eqIdx = rest.indexOf("=");
  if (eqIdx < 0) {
    setStatus(I18N.t("cmd.run.no_eq"), "error");
    return;
  }

  const name = rest.slice(0, eqIdx).trim();
  const cmd = rest.slice(eqIdx + 1).trim();

  if (!name) {
    setStatus(I18N.t("cmd.run.empty_name"), "error");
    return;
  }
  if (!cmd) {
    setStatus(I18N.t("cmd.run.empty_cmd"), "error");
    return;
  }

  if (!state.currentProject) {
    setStatus(I18N.t("cmd.run.no_project"), "error");
    return;
  }

  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  // 用户手动创建：以用户提供的名称作为 key
  const cmdKey = `ruyix.code.run.${name}.cmd`;
  const nameKey = `ruyix.code.run.${name}.name`;

  // 检查是否已存在同名目标
  try {
    const projectRoot = state.currentProject?.path || undefined;
    const existing = await invoke("config_get", {
      scope: "project",
      key: cmdKey,
      projectRoot,
    });
    if (existing != null) {
      setStatus(I18N.t("cmd.run.exists", { name }), "error");
      return;
    }
  } catch {
    // 忽略查询错误
  }

  // 依次写入两个配置项
  try {
    await executeConfigAction("add", "p", cmdKey, cmd);
  } catch (err) {
    setStatus(I18N.t("cmd.run.add_cmd_fail", { err }), "error");
    return;
  }

  try {
    await executeConfigAction("add", "p", nameKey, name);
  } catch (err) {
    setStatus(I18N.t("cmd.run.add_name_fail", { err }), "error");
    return;
  }

  setStatus(I18N.t("cmd.run.success", { name, cmd }));
  loadRunTargets();
}

/**
 * project 命令：项目属性管理
 * 语法:
 *   project lang <语言> <项目路径>                设置项目语言（路径取 lang 之后的剩余部分）
 *   project edit "<项目路径>" "<名称>" <语言>     修改项目名称与图标（路径不可修改）
 *   project delete <项目路径>                     从项目列表删除（路径取剩余部分，可含空格）
 *   project migrate                              迁移旧版项目配置（纯路径列表 → name/path/lang 条目）
 */
async function handleProjectCommand(raw) {
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_browser"));
    return;
  }

  const rest = raw.slice("project".length).trim();
  if (!rest) {
    setStatus(I18N.t("cmd.project.usage"));
    return;
  }

  // project lang <lang> <path>
  const langMatch = rest.match(/^lang\s+(\S+)\s+(.+)$/i);
  if (langMatch) {
    const lang = langMatch[1].toLowerCase();
    const path = langMatch[2].trim();
    try {
      await invoke("set_project_lang", { path, lang });
      setStatus(I18N.t("project.lang.ok", { lang: I18N.t(`lang.${lang}`) || lang }));
      if (typeof loadProjectList === "function") await loadProjectList();
    } catch (err) {
      setStatus(I18N.t("project.lang.fail", { err }), "error");
    }
    return;
  }

  // project edit "<path>" "<name>" <lang>
  const editMatch = rest.match(/^edit\s+(.+)$/i);
  if (editMatch) {
    // parseQuotedTokens：保留 Windows 路径反斜杠，引号内可含空格
    const args = parseQuotedTokens(editMatch[1]);
    if (args.length !== 3) {
      setStatus(I18N.t("cmd.project.edit_usage"));
      return;
    }
    const [path, name, lang] = args;
    try {
      await invoke("update_project", { path, name, lang: lang.toLowerCase() });
      setStatus(I18N.t("project.edit.ok", { name }));
      if (typeof loadProjectList === "function") await loadProjectList();
    } catch (err) {
      setStatus(I18N.t("project.edit.fail", { err }), "error");
    }
    return;
  }

  // project delete <path>（路径取剩余部分，自动去除外层引号）
  const delMatch = rest.match(/^delete\s+(.+)$/i);
  if (delMatch) {
    let path = delMatch[1].trim();
    if (
      (path.startsWith('"') && path.endsWith('"')) ||
      (path.startsWith("'") && path.endsWith("'"))
    ) {
      path = path.slice(1, -1);
    }
    try {
      await invoke("delete_project", { path });
      setStatus(I18N.t("project.delete.ok", { path }));
      if (typeof loadProjectList === "function") await loadProjectList();
    } catch (err) {
      setStatus(I18N.t("project.delete.fail", { err }), "error");
    }
    return;
  }

  // project migrate
  if (/^migrate$/i.test(rest)) {
    try {
      const count = await invoke("migrate_projects");
      if (count > 0) {
        setStatus(I18N.t("project.migrate.ok", { count }));
      } else {
        setStatus(I18N.t("project.migrate.none"));
      }
      if (typeof loadProjectList === "function") await loadProjectList();
    } catch (err) {
      setStatus(I18N.t("project.migrate.fail", { err }), "error");
    }
    return;
  }

  setStatus(I18N.t("cmd.project.usage"));
}

/**
 * AI 命令：将自然语言翻译为标准命令后执行
 */
async function handleAiCommand(raw) {
  console.log("[AI] 输入:", raw);
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  // ============================================
  // 二级：先尝试 Lua 脚本（零网络开销）
  // ============================================
  if (state.currentProject) {
    try {
      const luaResult = await invoke("lua_translate", {
        input: raw,
        projectRoot: state.currentProject.path
      });
      console.log("[AI] Lua 返回:", luaResult);
      if (luaResult) {
        // Lua 命中 → 直接执行命令，不再调用 LLM
        const lines = luaResult.split("\n").map(l => l.trim()).filter(l => l && !l.startsWith("#"));
        console.log("[AI] Lua 命令:", lines);
        for (const line of lines) {
          await handleCommand(line, true);
        }
        setStatus("⚡ Lua: " + luaResult);
        return;
      }
    } catch (err) {
      console.warn("[AI] Lua 异常:", err);
      // Lua 失败 → 静默降级到 LLM
    }
  }

  // ============================================
  // 三级：LLM API 调用
  // ============================================
  setStatus(I18N.t("status.thinking"));
  try {
    const result = await invoke("ai_translate", {
      input: raw,
      projectRoot: state.currentProject?.path || undefined
    });
    console.log("[AI] LLM 原始返回:", JSON.stringify(result));

    // AI 返回了 (不支持) 提示
    if (result.startsWith("不支持")) {
      console.log("[AI] → 不支持");
      setStatus(result, "error");
      return;
    }

    // 闲聊回复（不是标准命令动词开头）→ 直接显示
    const firstWord = result.split(/\s+/)[0]?.toLowerCase();
    if (!["open", "close", "config", "new", "run", "help", "agent", "mcp", "a2a", "tools", "skill", "skills", "del", "delete", "remove", "rm", "rename", "mv", "git"].includes(firstWord)) {
      console.log("[AI] → 闲聊:", result);
      setStatus(result);
      return;
    }

    // AI 返回的标准命令，逐行执行（禁止递归 AI）
    const lines = result.split("\n").map((l) => l.trim()).filter((l) => l && !l.startsWith("#"));
    console.log("[AI] → 执行命令:", lines);
    for (const line of lines) {
      await handleCommand(line, true);
    }
  } catch (err) {
    console.error("[AI] LLM 异常:", err);
    setStatus(I18N.t("project.ai_fail", { err }), "error");
  }
}

/**
 * 处理 open 命令及其子命令
 *   open project <path>  — 打开项目
 *   open file <path>     — 打开文件 (待实现)
 */
async function handleOpenCommand(args) {
  if (args.length === 0) {
    setStatus(I18N.t("cmd.open.usage"));
    return;
  }

  const sub = args[0]?.toLowerCase();
  const targetPath = args.slice(1).join(" ");

  switch (sub) {
    case "project":
      if (!targetPath) {
        setStatus(I18N.t("cmd.open.project_usage"));
        return;
      }
      await openProject(targetPath);
      break;
    case "file":
      if (!targetPath) {
        setStatus(I18N.t("cmd.open.file_usage"));
        return;
      }
      await openFile(targetPath);
      break;
    default:
      setStatus(I18N.t("cmd.open.unknown", { sub }));
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
    setStatus(I18N.t("cmd.close.usage"));
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
      setStatus(I18N.t("cmd.close.unknown", { sub }));
  }
}

// ============================================
// 路径安全校验
// ============================================

/**
 * 校验并拼接项目相对路径。返回完整路径，失败返回 null（已弹 setStatus）。
 */
function resolveProjectPath(rawPath) {
  if (!state.currentProject) {
    setStatus(I18N.t("cmd.run.no_project"), "error");
    return null;
  }
  if (/^\/|^[A-Za-z]:[/\\]/i.test(rawPath)) {
    setStatus(I18N.t("status.no_permission"), "error");
    return null;
  }
  return state.currentProject.path + "\\" + rawPath;
}

// ============================================
// new 命令
// ============================================

/** 类型子命令 → 扩展名映射 */
const TYPE_EXT = { py: ".py", rs: ".rs", md: ".md", c: ".c" };

async function handleNewCommand(args) {
  if (args.length < 2) {
    setStatus(I18N.t("cmd.new.usage"));
    return;
  }

  const sub = args[0]?.toLowerCase();
  const rawPath = args.slice(1).join(" ");

  const base = resolveProjectPath(rawPath);
  if (!base) return;

  // 类型子命令：自动追加扩展名
  if (TYPE_EXT[sub]) {
    await createNewFile(base + TYPE_EXT[sub]);
    return;
  }

  // 通用子命令
  if (sub === "file") {
    await createNewFile(base);
  } else if (sub === "folder" || sub === "dir") {
    const invoke = getTauriInvoke();
    if (!invoke) return;
    try {
      await invoke("create_dir", { path: base });
      setStatus(I18N.t("cmd.new.dir_created", { path: rawPath }));
      loadFileTree(state.currentProject.path);
    } catch (err) {
      setStatus(I18N.t("cmd.new.dir_failed", { err }), "error");
    }
  } else {
    setStatus(I18N.t("cmd.new.unknown", { sub }));
  }
}

// ============================================
// refresh 命令 — 从磁盘刷新文件树
// ============================================

/**
 * refresh            刷新整个文件树（项目根目录）
 * refresh <相对路径>  刷新指定文件夹（保持展开状态）
 */
async function handleRefreshCommand(args) {
  if (!state.currentProject) {
    setStatus(I18N.t("cmd.run.no_project"), "error");
    return;
  }

  // 无参数：整树刷新（含根目录新增/删除的文件）
  if (args.length === 0 || args.every((a) => a === "")) {
    try {
      await loadFileTree(state.currentProject.path);
      setStatus(I18N.t("cmd.refresh.ok"));
    } catch (err) {
      setStatus(I18N.t("cmd.refresh.failed", { err }), "error");
    }
    return;
  }

  // 带参数：定向刷新指定文件夹
  const rawPath = args.join(" ");
  const base = resolveProjectPath(rawPath);
  if (!base) return;

  try {
    const ok = await refreshTreeNode(base);
    if (ok) {
      setStatus(I18N.t("cmd.refresh.dir_ok", { path: rawPath }));
    } else {
      setStatus(I18N.t("cmd.refresh.not_found", { path: rawPath }), "error");
    }
  } catch (err) {
    setStatus(I18N.t("cmd.refresh.failed", { err }), "error");
  }
}

async function createNewFile(fullPath) {
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    await invoke("create_file", { path: fullPath });
    setStatus(I18N.t("cmd.new.file_created", { path: fullPath }));
    // 自动打开新文件
    await openFile(fullPath);
    // 刷新文件树
    if (state.currentProject) {
      loadFileTree(state.currentProject.path);
    }
  } catch (err) {
    setStatus(I18N.t("cmd.new.file_failed", { err }), "error");
  }
}

async function handleDeleteCommand(raw, skipConfirm = false) {
  // 提取相对路径（去掉动词）
  const rawPath = raw.replace(/^\S+\s+/, "").trim();
  if (!rawPath) {
    setStatus(I18N.t("cmd.delete.usage"));
    return;
  }

  const fullPath = resolveProjectPath(rawPath);
  if (!fullPath) return;

  if (!skipConfirm) {
    const ok = await showConfirm("删除", I18N.t("cmd.delete.confirm", { path: fullPath }));
    if (!ok) return;
  }

  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    await invoke("delete_path", { path: fullPath });
    setStatus(I18N.t("cmd.delete.success", { path: rawPath }));
    // 关闭已打开的标签页
    const tab = state.tabs.find((t) => t.path === fullPath);
    if (tab) closeTab(tab.id);
    loadFileTree(state.currentProject.path);
  } catch (err) {
    setStatus(I18N.t("cmd.delete.failed", { err }), "error");
  }
}

// ============================================
// rename / mv 命令
// ============================================

async function handleRenameCommand(args) {
  if (args.length < 2) {
    setStatus(I18N.t("cmd.rename.usage"), "error");
    return;
  }

  const oldRaw = args[0];
  const newRaw = args.slice(1).join(" ");

  // 非法字符检测
  const illegal = /[<>:"/\\|?*]/;
  if (illegal.test(newRaw)) {
    setStatus(I18N.t("cmd.rename.illegal_chars"), "error");
    return;
  }

  const oldPath = resolveProjectPath(oldRaw);
  if (!oldPath) return;

  const parentDir = oldPath.replace(/[/\\][^/\\]*$/, "");
  const newPath = parentDir + "\\" + newRaw;

  // 目标路径检查
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    const exists = await invoke("path_exists", { path: newPath });
    if (exists) {
      setStatus(I18N.t("cmd.rename.exists", { path: newRaw }), "error");
      return;
    }
  } catch {}

  try {
    await invoke("rename_path", { from: oldPath, to: newPath });
    const oldName = oldPath.split(/[/\\]/).pop();
    setStatus(I18N.t("cmd.rename.ok", { old: oldName, new: newRaw }));
    // 关闭旧标签
    const tab = state.tabs.find((t) => t.path === oldPath);
    if (tab) closeTab(tab.id);
    if (state.currentProject) loadFileTree(state.currentProject.path);
  } catch (err) {
    setStatus(I18N.t("cmd.rename.fail", { err }), "error");
  }
}

// ============================================
// git 命令
// ============================================

/**
 * git 命令：直接执行系统 git 命令（不设计子命令 DSL，原样透传给 git）
 * 语法: git <任意 git 子命令>，例如 git status / git add -A / git commit -m "x"
 * - AI 路径（_fromAi=true）：输出到编辑区终端标签页
 * - GUI / 命令栏路径（_fromAi=false）：状态栏提示 + 刷新 Git 面板
 */
async function handleGitCommand(raw, _fromAi = false) {
  const rest = raw.replace(/^\S+\s*/, "").trim();
  if (!rest) {
    setStatus(I18N.t("git.usage"), "error");
    return;
  }
  if (!state.currentProject) {
    setStatus(I18N.t("git.no_project"), "error");
    return;
  }
  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  if (_fromAi) {
    await runGitInTab(rest);
    return;
  }

  // GUI 路径：静默执行
  setStatus(I18N.t("git.executing", { cmd: rest }));
  try {
    const out = await invoke("git_run", { args: rest, projectRoot: state.currentProject.path });
    if (out.exit_code === 0) {
      setStatus(I18N.t("git.done", { cmd: rest }));
    } else {
      const errMsg = (out.stderr || out.stdout || "").trim().split("\n")[0] || "未知错误";
      setStatus(I18N.t("git.fail", { cmd: rest, err: errMsg }), "error");
    }
  } catch (err) {
    setStatus(I18N.t("git.fail", { cmd: rest, err }), "error");
  }
  if (typeof loadGitStatus === "function") await loadGitStatus();
}

/**
 * 在编辑区打开输出标签页执行 git 命令（AI 路径）
 */
async function runGitInTab(rest) {
  const invoke = getTauriInvoke();
  if (!invoke) return;

  const verb = rest.split(/\s+/)[0] || "git";
  const tab = {
    id: "git-" + Date.now().toString(),
    name: "git " + verb,
    path: "",
    content: "git " + rest,
    _runTarget: true,
  };
  state.tabs.push(tab);
  renderTabs();
  switchTab(tab.id);
  renderTerminalOutput(tab, `> git ${rest}\n\n${I18N.t("runtarget.executing")}`);

  try {
    const result = await invoke("git_run", { args: rest, projectRoot: state.currentProject?.path });
    let output = `> git ${rest}\n`;
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
    if (result.exit_code === 0) {
      setStatus(I18N.t("git.done", { cmd: rest }));
    } else {
      setStatus(I18N.t("git.fail", { cmd: rest, err: "exit " + result.exit_code }), "error");
    }
  } catch (err) {
    renderTerminalOutput(tab, `> git ${rest}\n\n[错误] ${err}`);
    setStatus(I18N.t("git.fail", { cmd: rest, err }), "error");
  }
  if (typeof loadGitStatus === "function") await loadGitStatus();
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
    setStatus(I18N.t("cmd.config.sub_usage"));
    return;
  }

  // 配置表单子动词（扫描配置项 → 表单）在 config.js，路由过去
  const first = rest.split(/\s+/)[0].toLowerCase();
  if (["form", "save", "apply", "cancel"].includes(first)) {
    await window.ConfigUI?.handleCommand(rest);
    return;
  }

  // 解析: action scope key=value
  // 分词：保留引号内容
  const tokens = parseConfigTokens(rest);
  if (tokens.length === 0) {
    setStatus(I18N.t("cmd.config.sub_usage"));
    return;
  }

  const actions = ["add", "get", "update", "remove", "delete"];
  let idx = 0;

  // 子命令
  const action = tokens[idx]?.toLowerCase();
  if (!actions.includes(action)) {
    setStatus(I18N.t("cmd.config.unknown", { action, actions: actions.join(", ") }));
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
    setStatus(I18N.t("cmd.config.missing_key"));
    return;
  }

  await executeConfigAction(action, scope, key, cleanValue);
}

/**
 * 分词：按空格分割但保留引号内内容。
 * 与 parseConfigTokens 不同：引号内反斜杠原样保留（兼容 Windows 路径），
 * 仅 \" 和 \\ 作为转义处理。
 */
function parseQuotedTokens(s) {
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
        if (
          s[i] === "\\" &&
          i + 1 < s.length &&
          (s[i + 1] === quote || s[i + 1] === "\\")
        ) {
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
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  const scopeMap = { g: "global", p: "project", r: "runtime" };

  // 从 state 取项目根路径（确保不为空字符串）
  const projectRoot = state.currentProject?.path || undefined;
  if (projectRoot === undefined && state.currentProject) {
    setStatus(I18N.t("cmd.config.internal_err"), "error");
    return;
  }

  // -p 需要已打开项目
  if (scope === "p" && !projectRoot) {
    setStatus(I18N.t("cmd.config.no_project"), "error");
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
            setStatus(I18N.t("cmd.config.exists", { key, val: existing }), "error");
            return;
          }
        } catch {
          // get 失败视为不存在
        }
        try {
          await invoke("config_set", { scope: scopeMap[scope], key, value, projectRoot });
          setStatus(I18N.t("cmd.config.add_ok", { key, val: value }));
        } catch (err) {
          setStatus(I18N.t("cmd.config.add_fail", { err }), "error");
        }
      }
      break;

    case "get":
      {
        try {
          const val = await invoke("config_get", { scope: scopeMap[scope], key, projectRoot });
          if (val == null) {
            setStatus(I18N.t("cmd.config.not_found", { key }), "error");
          } else {
            setStatus(`${key} = ${val}`);
          }
        } catch (err) {
          setStatus(I18N.t("cmd.config.get_fail", { err }), "error");
        }
      }
      break;

    case "update":
      {
        try {
          await invoke("config_set", { scope: scopeMap[scope], key, value, projectRoot });
          setStatus(I18N.t("cmd.config.update_ok", { key, val: value }));
        } catch (err) {
          setStatus(I18N.t("cmd.config.update_fail", { err }), "error");
        }
      }
      break;

    case "remove":
    case "delete":
      {
        try {
          await invoke("config_delete", { scope: scopeMap[scope], key, projectRoot });
          setStatus(I18N.t("cmd.config.delete_ok", { key }));
        } catch (err) {
          setStatus(I18N.t("cmd.delete.failed", { err }), "error");
        }
      }
      break;
  }
}

// ============================================
// close 命令实现
// ============================================

/**
 * 清场当前项目：逐个关闭终端 PTY、清空标签页、回到欢迎页。
 * 关闭项目与切换项目（openProject）共用。
 */
function teardownProject() {
  // 逐个关闭终端标签页的 PTY 进程，避免关闭/切换项目后进程泄漏
  const invoke = getTauriInvoke();
  if (invoke) {
    state.tabs.forEach((t) => {
      if (t._isTerminal) {
        invoke("pty_close", { tabId: t.id }).catch(() => {});
        if (t._ptyUnlisten) t._ptyUnlisten();
        if (t._term) t._term.dispose();
      }
    });
  }

  state.currentProject = null;
  if (typeof updateProjectMenu === "function") updateProjectMenu();
  updateTitlebarTitle();
  if (typeof updateWindowTitle === "function") updateWindowTitle();
  showWelcomePage();
}

function closeProject() {
  if (!state.currentProject) {
    setStatus(I18N.t("close.project.none"));
    return;
  }

  const name = state.currentProject.name;
  teardownProject();
  setStatus(I18N.t("close.project.ok", { name }));
}

function closeAll() {
  if (state.tabs.length === 0) {
    setStatus(I18N.t("close.all.none"));
    return;
  }
  state.tabs = [];
  state.activeTabId = null;
  renderTabs();
  hideEditor();
  setStatus(I18N.t("close.all.ok"));
}

/**
 * 按序号关闭标签页。
 * 正数: 0-based 从左到右
 * 负数: -1-based 从右到左（-1 = 最后一个）
 */
function closeByIndex(index) {
  if (state.tabs.length === 0) {
    setStatus(I18N.t("close.all.none"));
    return;
  }
  const i = index >= 0 ? index : state.tabs.length + index;
  if (i < 0 || i >= state.tabs.length) {
    setStatus(I18N.t("close.index.range", { index, total: state.tabs.length }));
    return;
  }
  const tab = state.tabs[i];
  closeTab(tab.id);
  setStatus(I18N.t("close.one.ok", { name: tab.name }));
}

/** 关闭除当前活动标签外的所有标签 */
function closeOthers() {
  if (state.tabs.length <= 1) {
    setStatus(I18N.t("close.other.none"));
    return;
  }
  const active = state.tabs.find((t) => t.id === state.activeTabId);
  state.tabs = active ? [active] : [];
  renderTabs();
  setStatus(I18N.t("close.other.ok"));
}

/** 关闭当前活动标签左边的所有标签 */
function closeLeft() {
  const idx = state.tabs.findIndex((t) => t.id === state.activeTabId);
  if (idx <= 0) {
    setStatus(I18N.t("close.left.none"));
    return;
  }
  const removed = state.tabs.slice(0, idx).map((t) => t.name).join(", ");
  state.tabs = state.tabs.slice(idx);
  renderTabs();
  setStatus(I18N.t("close.left.ok", { names: removed }));
}

/** 关闭当前活动标签右边的所有标签 */
function closeRight() {
  const idx = state.tabs.findIndex((t) => t.id === state.activeTabId);
  if (idx < 0 || idx >= state.tabs.length - 1) {
    setStatus(I18N.t("close.right.none"));
    return;
  }
  const removed = state.tabs.slice(idx + 1).map((t) => t.name).join(", ");
  state.tabs = state.tabs.slice(0, idx + 1);
  renderTabs();
  setStatus(I18N.t("close.right.ok", { names: removed }));
}

/**
 * 调用后端打开项目
 */
async function openProject(path) {
  const invoke = getTauriInvoke();

  if (!invoke) {
    // 浏览器开发模式 — 模拟打开项目
    setStatus(I18N.t("status.tauri_browser"));
    return;
  }

  // 已是当前项目：直接返回
  if (state.currentProject && samePath(state.currentProject.path, path)) {
    setStatus(I18N.t("open.project.already"));
    return;
  }

  // 切换项目：先清场当前项目（关 PTY、清标签页、回欢迎页）
  if (state.currentProject) {
    teardownProject();
  }

  try {
    setStatus(I18N.t("open.project.opening"));
    const info = await invoke("open_project", { path });

    // 保存项目信息
    state.currentProject = info;

    // 更新项目菜单状态
    if (typeof updateProjectMenu === "function") updateProjectMenu();

    // 切换为项目工作区视图
    showProjectWorkspace();
    updateTitlebarTitle();
    if (typeof updateWindowTitle === "function") updateWindowTitle();
    setStatus(I18N.t("open.project.ok", { path: info.path }));
  } catch (err) {
    setStatus(I18N.t("open.project.fail", { err }), "error");
  }
}

/**
 * 调用后端打开文件，创建标签页并高亮
 */
async function openFile(path) {
  const fullPath = resolveProjectPath(path);
  if (!fullPath) return;

  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_browser"));
    return;
  }

  // 检查是否已打开
  const existing = state.tabs.find((t) => t.path === fullPath);
  if (existing) {
    switchTab(existing.id);
    setStatus(I18N.t("open.file.switched", { name: existing.name }));
    return;
  }

  // 提取文件名和扩展名
  const name = fullPath.split(/[/\\]/).pop() || fullPath;
  const ext = name.split(".").pop()?.toLowerCase();

  // 图片文件：读取 base64 数据用于预览
  if (typeof isImageExt === "function" && isImageExt(ext)) {
    try {
      setStatus(I18N.t("open.file.reading"));
      const data = await invoke("read_file_base64", { path: fullPath });
      const tab = {
        id: Date.now().toString(),
        name,
        path: data.path || fullPath,
        content: "",
        _isImage: true,
        _imageMime: data.mime,
        _imageBase64: data.base64,
      };
      state.tabs.push(tab);
      renderTabs();
      switchTab(tab.id);
      setStatus(I18N.t("open.file.ok", { name }));
    } catch (err) {
      setStatus(I18N.t("open.file.fail", { err }), "error");
    }
    return;
  }

  try {
    setStatus(I18N.t("open.file.reading"));
    const file = await invoke("read_file", { path: fullPath });

    // 创建标签页
    const tab = { id: Date.now().toString(), name, path: file.path, content: file.content };
    state.tabs.push(tab);
    renderTabs();
    switchTab(tab.id);

    // 语法高亮：根据扩展名确定语言
    const lang = extToLanguage(ext);
    if (lang) {
      await highlightAndRender(tab, lang);
    } else {
      renderPlainCode(tab);
    }

    setStatus(I18N.t("open.file.ok", { name }));
  } catch (err) {
    setStatus(I18N.t("open.file.fail", { err }), "error");
  }
}

function getHelpText() {
  const t = (k) => I18N.t(k);
  return `${t("help.title")}
====================

${t("help.section_commands")}
--------
${t("help.cmd_open_project")}        ${t("help.desc_open_project")}
${t("help.cmd_open_file")}           ${t("help.desc_open_file")}
${t("help.cmd_close_project")}              ${t("help.desc_close_project")}
${t("help.cmd_close_all")}                  ${t("help.desc_close_all")}
${t("help.cmd_close_index")}               ${t("help.desc_close_index")}
${t("help.cmd_close_other")}                ${t("help.desc_close_other")}
${t("help.cmd_close_left")}                 ${t("help.desc_close_left")}
${t("help.cmd_close_right")}                ${t("help.desc_close_right")}
${t("help.cmd_new_type")}       ${t("help.desc_new_type")}
${t("help.cmd_new_file")}          ${t("help.desc_new_file")}
${t("help.cmd_new_folder")}    ${t("help.desc_new_folder")}
${t("help.cmd_refresh")}          ${t("help.desc_refresh")}
${t("help.cmd_del")} ${t("help.desc_del")}
${t("help.cmd_rename")}             ${t("help.desc_rename")}
${t("help.cmd_run")}             ${t("help.desc_run")}
${t("help.cmd_config")}
                           ${t("help.desc_config")} ${t("help.desc_config_scope")}
${t("help.cmd_project_lang")} ${t("help.desc_project_lang")}
${t("help.cmd_project_edit")} ${t("help.desc_project_edit")}
${t("help.cmd_project_delete")} ${t("help.desc_project_delete")}
${t("help.cmd_project_migrate")}   ${t("help.desc_project_migrate")}

${t("help.section_shortcuts")}
------
${t("help.shortcut_save")}

${t("help.section_highlight")}
------------
Python                     .py
Rust                       .rs
HTML（含嵌入 CSS/JS）       .html, .htm
CSS                        .css
JavaScript                 .js, .mjs, .cjs
Markdown                   .md, .markdown
SQL                        .sql

${t("help.section_contact")}
--------
${t("help.contact_text")}
`;}
