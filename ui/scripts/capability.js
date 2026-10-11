/**
 * ruyix — 能力面板：工具（命令行白名单）与 SKILL（技能文档）
 *
 * 「能力」四类中的两类在此管理（MCP 见 mcp.js，A2A 见 a2a.js）：
 * - 工具：agent 可用的命令行命令白名单（tools.toml：全局 + 项目），
 *   支持 `<command> --version` 本机探测
 * - SKILL：可注入 prompt 的 markdown 技能文档（skills.toml）
 */

(function () {
  "use strict";

  const L = (zh, en) => (window.I18N && I18N.getLang() === "en" ? en : zh);

  const probeState = {};   // name → {available, version}（探测结果缓存）

  function $(id) {
    return document.getElementById(id);
  }

  function esc(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
    }[c]));
  }

  function getInvoke() {
    if (typeof getTauriInvoke === "function") return getTauriInvoke();
    return window.__TAURI__?.core?.invoke?.bind(window.__TAURI__.core) ?? null;
  }

  function root() {
    return window.state?.currentProject?.path ?? null;
  }

  function status(msg, kind) {
    if (typeof setStatus === "function") setStatus(L(msg, msg), kind);
  }

  function truncate(s, n) {
    return String(s || "").length > n ? String(s).slice(0, n - 1) + "…" : String(s || "");
  }

  // ============================================
  // 工具白名单
  // ============================================

  function renderTools(tools) {
    const el = $("tools-list");
    if (!el) return;
    if (!tools.length) {
      el.innerHTML = `<div class="mcp-empty">${L("白名单为空 — agent 只能使用名单内的命令", "Empty — agent may only use whitelisted commands")}</div>`;
      return;
    }
    el.innerHTML = tools.map((t) => {
      const cmd = t.command && t.command !== t.name ? ` <code>${esc(t.command)}</code>` : "";
      const p = probeState[t.name];
      const probeTag = !p ? "" : (p.available
        ? `<span class="mcp-tools-count">✓ ${esc(truncate(p.version, 24))}</span>`
        : `<span class="a2a-state-line--err mcp-tools-count">✗ ${L("未安装", "missing")}</span>`);
      return `<div class="mcp-server">` +
        `<span class="mcp-dot${t.enabled ? " mcp-dot--on" : ""}"></span>` +
        `<div class="mcp-server-main">` +
        `<div class="mcp-server-name">${esc(t.name)}${cmd}${probeTag}</div>` +
        (t.description ? `<div class="mcp-server-info">${esc(truncate(t.description, 60))}</div>` : "") +
        (t.args_hint ? `<div class="mcp-server-cmd">${esc(t.args_hint)}</div>` : "") +
        `</div>` +
        `<button class="mcp-icon-btn tools-probe" data-name="${esc(t.name)}" title="${L("探测", "probe")}">🔍</button>` +
        `<button class="mcp-icon-btn mcp-del tools-del" data-name="${esc(t.name)}" title="${L("删除", "remove")}">✕</button>` +
        `</div>`;
    }).join("");
    el.querySelectorAll(".tools-probe").forEach((b) =>
      b.addEventListener("click", (e) => {
        e.stopPropagation();
        probeTool(b.dataset.name);
      }));
    el.querySelectorAll(".tools-del").forEach((b) =>
      b.addEventListener("click", (e) => {
        e.stopPropagation();
        removeTool(b.dataset.name);
      }));
  }

  async function refreshTools() {
    const invoke = getInvoke();
    if (!invoke) {
      const el = $("tools-list");
      if (el) el.innerHTML = `<div class="mcp-empty">✗ ${L("后端不可用", "backend unavailable")}</div>`;
      return;
    }
    try {
      renderTools(await invoke("tools_list", { projectRoot: root() }) ?? []);
    } catch (err) {
      status(L("工具列表刷新失败: ", "Tool list refresh failed: ") + err, "error");
    }
  }

  async function addTool() {
    const invoke = getInvoke();
    if (!invoke) return status(L("后端不可用", "backend unavailable"), "error");
    const name = ($("tools-add-name")?.value || "").trim();
    if (!name) return status(L("请填写工具名", "Enter a tool name"), "error");
    try {
      await invoke("tools_add", {
        name,
        command: ($("tools-add-command")?.value || "").trim() || null,
        description: ($("tools-add-desc")?.value || "").trim() || null,
        projectRoot: root(),
      });
      $("tools-add-name").value = "";
      $("tools-add-command").value = "";
      $("tools-add-desc").value = "";
      status(L("工具已添加: ", "Tool added: ") + name);
      await refreshTools();
    } catch (err) {
      status(L("添加失败: ", "Add failed: ") + err, "error");
    }
  }

  async function removeTool(name) {
    const invoke = getInvoke();
    if (!invoke) return;
    try {
      await invoke("tools_remove", { name, projectRoot: root() });
      delete probeState[name];
      status(L("工具已删除: ", "Tool removed: ") + name);
      await refreshTools();
    } catch (err) {
      status(L("删除失败: ", "Remove failed: ") + err, "error");
    }
  }

  async function probeTool(name) {
    const invoke = getInvoke();
    if (!invoke) return;
    status(L("探测中: ", "Probing: ") + name);
    try {
      const p = await invoke("tools_probe", { name });
      probeState[name] = p;
      renderTools(await invoke("tools_list", { projectRoot: root() }) ?? []);
      status(p.available ? `${name} ✓ ${p.version}` : `${name} ✗ ${L("本机未安装", "not installed")}`);
    } catch (err) {
      status(L("探测失败: ", "Probe failed: ") + err, "error");
    }
  }

  // ============================================
  // SKILL
  // ============================================

  let skills = [];
  let selectedSkill = null;

  function renderSkills() {
    const el = $("skills-list");
    if (!el) return;
    if (!skills.length) {
      el.innerHTML = `<div class="mcp-empty">${L("暂无技能 — 下方新建", "No skills yet — create below")}</div>`;
      return;
    }
    el.innerHTML = skills.map((s) =>
      `<div class="mcp-server${s.name === selectedSkill ? " mcp-server--sel" : ""}" data-name="${esc(s.name)}">` +
      `<span class="mcp-dot${s.enabled ? " mcp-dot--on" : ""}"></span>` +
      `<div class="mcp-server-main">` +
      `<div class="mcp-server-name">📘 ${esc(s.name)}</div>` +
      (s.description ? `<div class="mcp-server-info">${esc(truncate(s.description, 60))}</div>` : "") +
      `</div></div>`).join("");
    el.querySelectorAll(".mcp-server").forEach((node) =>
      node.addEventListener("click", () => loadSkill(node.dataset.name)));
  }

  function loadSkill(name) {
    const s = skills.find((x) => x.name === name);
    if (!s) return;
    selectedSkill = name;
    $("skill-edit-name").value = s.name;
    $("skill-edit-desc").value = s.description || "";
    $("skill-edit-content").value = s.content || "";
    renderSkills();
  }

  function newSkill() {
    selectedSkill = null;
    $("skill-edit-name").value = "";
    $("skill-edit-desc").value = "";
    $("skill-edit-content").value = "";
    renderSkills();
    $("skill-edit-name")?.focus();
  }

  async function refreshSkills(keepSelection) {
    const invoke = getInvoke();
    if (!invoke) {
      const el = $("skills-list");
      if (el) el.innerHTML = `<div class="mcp-empty">✗ ${L("后端不可用", "backend unavailable")}</div>`;
      return;
    }
    try {
      skills = await invoke("skills_list", { projectRoot: root() }) ?? [];
      if (!keepSelection) selectedSkill = null;
      if (selectedSkill && !skills.some((s) => s.name === selectedSkill)) selectedSkill = null;
      renderSkills();
    } catch (err) {
      status(L("技能列表刷新失败: ", "Skill list refresh failed: ") + err, "error");
    }
  }

  async function saveSkill() {
    const invoke = getInvoke();
    if (!invoke) return status(L("后端不可用", "backend unavailable"), "error");
    const name = ($("skill-edit-name")?.value || "").trim();
    if (!name) return status(L("请填写技能名", "Enter a skill name"), "error");
    try {
      await invoke("skills_save", {
        name,
        description: $("skill-edit-desc")?.value || "",
        content: $("skill-edit-content")?.value || "",
        projectRoot: root(),
      });
      selectedSkill = name;
      status(L("技能已保存: ", "Skill saved: ") + name);
      await refreshSkills(true);
    } catch (err) {
      status(L("保存失败: ", "Save failed: ") + err, "error");
    }
  }

  async function deleteSkill() {
    const invoke = getInvoke();
    if (!invoke || !selectedSkill) return status(L("请先在列表中选择技能", "Select a skill in the list first"), "error");
    try {
      await invoke("skills_remove", { name: selectedSkill, projectRoot: root() });
      status(L("技能已删除: ", "Skill deleted: ") + selectedSkill);
      newSkill();
      await refreshSkills();
    } catch (err) {
      status(L("删除失败: ", "Remove failed: ") + err, "error");
    }
  }

  // ============================================
  // 命令栏接入
  // ============================================

  async function toolsCommand(rest) {
    const args = String(rest || "").trim().split(/\s+/).filter(Boolean);
    const invoke = getInvoke();
    if (!invoke) return status(L("后端不可用", "backend unavailable"), "error");
    if (!args.length || args[0] === "list") {
      try {
        const list = await invoke("tools_list", { projectRoot: root() });
        status((list ?? []).map((t) => `${t.enabled ? "●" : "○"} ${t.name}`).join(" | ") || L("工具白名单为空", "empty"));
      } catch (err) {
        status(L("查询失败: ", "Query failed: ") + err, "error");
      }
      return;
    }
    if (args[0] === "add") {
      // tools add <name> [command] [描述…]（描述里含空格时建议手编 toml）
      const name = args[1];
      if (!name) return status(L("用法: tools add <名称> [命令] [描述]", "usage: tools add <name> [command] [description]"), "error");
      try {
        await invoke("tools_add", {
          name,
          command: args[2] || null,
          description: args.slice(3).join(" ") || null,
          projectRoot: root(),
        });
        status(L("工具已添加: ", "Tool added: ") + name);
      } catch (err) {
        status(L("添加失败: ", "Add failed: ") + err, "error");
      }
      return;
    }
    if (args[0] === "probe") {
      if (!args[1]) return status(L("用法: tools probe <名称>", "usage: tools probe <name>"), "error");
      return probeTool(args[1]);
    }
    if (args[0] === "remove" || args[0] === "rm") {
      if (!args[1]) return status(L("用法: tools remove <名称>", "usage: tools remove <name>"), "error");
      return removeTool(args[1]);
    }
    status(L("用法: tools [list | add <名> [命令] [描述] | probe <名> | remove <名>]", "usage: tools [list | add <name> [cmd] [desc] | probe <name> | remove <name>]"), "error");
  }

  async function skillsCommand(rest) {
    const args = String(rest || "").trim().split(/\s+/).filter(Boolean);
    const invoke = getInvoke();
    if (!invoke) return status(L("后端不可用", "backend unavailable"), "error");
    if (!args.length || args[0] === "list") {
      try {
        const list = await invoke("skills_list", { projectRoot: root() });
        status((list ?? []).map((s) => `📘 ${s.name}`).join(" | ") || L("暂无技能", "no skills"));
      } catch (err) {
        status(L("查询失败: ", "Query failed: ") + err, "error");
      }
      return;
    }
    status(L("技能编辑请用左侧「技能」面板（skills.toml 同步生效）", "Edit skills in the Skills panel on the left (synced with skills.toml)"), "error");
  }

  // ============================================
  // 初始化 + 能力菜单联动
  // ============================================

  function attach() {
    $("tools-add-btn")?.addEventListener("click", addTool);
    $("skill-new-btn")?.addEventListener("click", newSkill);
    $("skill-save-btn")?.addEventListener("click", saveSkill);
    $("skill-del-btn")?.addEventListener("click", deleteSkill);
    refreshTools();
    refreshSkills();
  }

  window.ToolsUI = { attach, handleCommand: toolsCommand };
  window.SkillsUI = { attach: refreshSkills, handleCommand: skillsCommand };
})();
