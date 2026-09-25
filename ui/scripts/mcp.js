/**
 * ruyix — MCP 面板（Model Context Protocol 客户端管理）
 *
 * 服务器配置（mcp.toml：全局 + 项目合并）→ 启停连接 → 工具发现 → 工具调用。
 * 与 agent.js 同款防御式写法：无 Tauri（浏览器直开）时显示"后端不可用"。
 */

(function () {
  "use strict";

  const L = (zh, en) => (window.I18N && I18N.getLang() === "en" ? en : zh);

  let servers = [];       // mcp_servers 的返回（配置 + 运行态）
  let tools = [];         // 当前选中服务器的工具
  let selected = null;    // 选中的服务器名

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

  // ============================================
  // 渲染
  // ============================================

  function renderServers() {
    const el = $("mcp-server-list");
    if (!el) return;
    if (!servers.length) {
      el.innerHTML = `<div class="mcp-empty">${L("未配置服务器，用下方表单添加", "No servers yet — add below")}</div>`;
      return;
    }
    el.innerHTML = servers.map((s) => {
      const sel = s.name === selected ? " mcp-server--sel" : "";
      const dot = s.running
        ? '<span class="mcp-dot mcp-dot--on"></span>'
        : '<span class="mcp-dot"></span>';
      const toolsTag = s.running && s.tools
        ? `<span class="mcp-tools-count">${s.tools} ${L("工具", "tools")}</span>`
        : "";
      return `<div class="mcp-server${sel}" data-name="${esc(s.name)}">` +
        `${dot}<div class="mcp-server-main">` +
        `<div class="mcp-server-name">${esc(s.name)}${toolsTag}</div>` +
        `<div class="mcp-server-cmd">${esc(s.command)}</div>` +
        (s.server_info ? `<div class="mcp-server-info">${esc(s.server_info)}</div>` : "") +
        `</div><button class="mcp-icon-btn mcp-del" data-name="${esc(s.name)}" ` +
        `title="${L("删除", "remove")}">✕</button></div>`;
    }).join("");
    el.querySelectorAll(".mcp-server").forEach((node) => {
      node.addEventListener("click", () => selectServer(node.dataset.name));
    });
    el.querySelectorAll(".mcp-del").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        removeServer(btn.dataset.name);
      });
    });
  }

  function renderTools() {
    const box = $("mcp-tool-section");
    if (!box) return;
    if (!selected) {
      box.style.display = "none";
      return;
    }
    box.style.display = "";
    const st = servers.find((s) => s.name === selected);
    $("mcp-selected-name").textContent = selected;
    $("mcp-start-btn").style.display = st?.running ? "none" : "";
    $("mcp-stop-btn").style.display = st?.running ? "" : "none";

    const list = $("mcp-tool-list");
    if (!st?.running) {
      list.innerHTML = `<div class="mcp-empty">${L("未连接 — 点击「连接」发现工具", "Not connected — click Connect to list tools")}</div>`;
      tools = [];
      renderToolSelect();
      return;
    }
    if (!tools.length) {
      list.innerHTML = `<div class="mcp-empty">${L("该服务器没有暴露工具", "No tools exposed")}</div>`;
    } else {
      list.innerHTML = tools.map((t) =>
        `<div class="mcp-tool"><code>${esc(t.name)}</code>` +
        `<div class="mcp-tool-desc">${esc(truncate(t.description, 80))}</div></div>`).join("");
    }
    renderToolSelect();
  }

  function renderToolSelect() {
    const sel = $("mcp-call-tool");
    if (!sel) return;
    sel.innerHTML = tools.length
      ? tools.map((t) => `<option value="${esc(t.name)}">${esc(t.name)}</option>`).join("")
      : `<option value="">—</option>`;
  }

  function renderResult(text, isError) {
    const el = $("mcp-call-result");
    if (!el) return;
    el.textContent = text || (isError ? L("（错误，无输出）", "(error, no output)") : L("（无输出）", "(no output)"));
    el.className = "mcp-call-result" + (isError ? " mcp-call-result--err" : "");
  }

  function truncate(s, n) {
    return String(s || "").length > n ? String(s).slice(0, n - 1) + "…" : String(s || "");
  }

  function setBusy(b) {
    const btn = $("mcp-call-btn");
    if (btn) btn.disabled = b;
    const start = $("mcp-start-btn");
    if (start) start.disabled = b;
  }

  // ============================================
  // 动作
  // ============================================

  async function refresh() {
    const invoke = getInvoke();
    if (!invoke) {
      const el = $("mcp-server-list");
      if (el) el.innerHTML = `<div class="mcp-empty">✗ ${L("后端不可用", "backend unavailable")}</div>`;
      return;
    }
    try {
      servers = await invoke("mcp_servers", { projectRoot: root() }) ?? [];
      if (selected && !servers.some((s) => s.name === selected)) {
        selected = null;
        tools = [];
      }
      renderServers();
      renderTools();
    } catch (err) {
      status(L("MCP 刷新失败: ", "MCP refresh failed: ") + err, "error");
    }
  }

  function selectServer(name) {
    selected = name;
    tools = [];
    renderServers();
    renderTools();
    const invoke = getInvoke();
    const st = servers.find((s) => s.name === name);
    if (invoke && st?.running) {
      invoke("mcp_tools", { name }).then((t) => {
        tools = t ?? [];
        renderTools();
      }).catch(() => {});
    }
  }

  async function addServer() {
    const invoke = getInvoke();
    if (!invoke) return status(L("后端不可用", "backend unavailable"), "error");
    const name = ($("mcp-add-name")?.value || "").trim();
    const command = ($("mcp-add-command")?.value || "").trim();
    if (!name || !command) return status(L("请填写名称与启动命令", "Enter a name and a launch command"), "error");
    try {
      // 简易解析：命令串按空白拆分（引号内包含空格的场景请用面板外手编 mcp.toml）
      const parts = command.match(/"[^"]*"|\S+/g) || [];
      const args = parts.slice(1).map((p) => p.replace(/^"|"$/g, ""));
      await invoke("mcp_add_server", {
        name, command: parts[0], args, projectRoot: root(),
      });
      $("mcp-add-name").value = "";
      $("mcp-add-command").value = "";
      status(L("MCP 服务器已添加: ", "MCP server added: ") + name);
      await refresh();
    } catch (err) {
      status(L("添加失败: ", "Add failed: ") + err, "error");
    }
  }

  async function removeServer(name) {
    const invoke = getInvoke();
    if (!invoke) return;
    try {
      await invoke("mcp_remove_server", { name, projectRoot: root() });
      if (selected === name) selected = null;
      status(L("MCP 服务器已删除: ", "MCP server removed: ") + name);
      await refresh();
    } catch (err) {
      status(L("删除失败: ", "Remove failed: ") + err, "error");
    }
  }

  async function startServer() {
    const invoke = getInvoke();
    if (!invoke || !selected) return;
    setBusy(true);
    try {
      const st = await invoke("mcp_start", { name: selected, projectRoot: root() });
      status(L(`MCP 已连接: ${st.name}（${st.tools} 工具）`, `MCP connected: ${st.name} (${st.tools} tools)`));
      await refresh();
      selectServer(selected);
    } catch (err) {
      status(L("连接失败: ", "Connect failed: ") + err, "error");
    } finally {
      setBusy(false);
    }
  }

  async function stopServer() {
    const invoke = getInvoke();
    if (!invoke || !selected) return;
    try {
      await invoke("mcp_stop", { name: selected, projectRoot: root() });
      tools = [];
      status(L("MCP 已断开: ", "MCP disconnected: ") + selected);
      await refresh();
    } catch (err) {
      status(L("断开失败: ", "Disconnect failed: ") + err, "error");
    }
  }

  async function callTool() {
    const invoke = getInvoke();
    if (!invoke || !selected) return;
    const tool = $("mcp-call-tool")?.value;
    if (!tool) return status(L("请先选择工具", "Select a tool first"), "error");
    const argsJson = $("mcp-call-args")?.value || "";
    renderResult(L("调用中…", "calling…"), false);
    setBusy(true);
    try {
      const out = await invoke("mcp_call_tool", { name: selected, tool, argsJson });
      renderResult(out.text, out.isError);
      status(out.isError ? L("MCP 工具返回错误", "MCP tool returned an error") : L("MCP 调用完成", "MCP call finished"));
    } catch (err) {
      renderResult(String(err), true);
      status(L("调用失败", "Call failed"), "error");
    } finally {
      setBusy(false);
    }
  }

  // ============================================
  // 命令栏接入：mcp [list | call <server> <tool> {json}]
  // ============================================

  async function handleCommand(rest) {
    const args = String(rest || "").trim().split(/\s+/).filter(Boolean);
    const invoke = getInvoke();
    if (!invoke) return status(L("后端不可用", "backend unavailable"), "error");
    if (!args.length || args[0] === "list") {
      try {
        const list = await invoke("mcp_servers", { projectRoot: root() });
        const lines = (list ?? []).map((s) =>
          `${s.running ? "●" : "○"} ${s.name} — ${s.command}${s.running ? ` (${s.tools} tools)` : ""}`);
        status(lines.length ? lines.join(" | ") : L("未配置 MCP 服务器", "No MCP server configured"));
      } catch (err) {
        status(L("MCP 查询失败: ", "MCP query failed: ") + err, "error");
      }
      return;
    }
    if (args[0] === "call") {
      const [_, name, tool, ...restArgs] = args;
      if (!name || !tool) return status(L("用法: mcp call <服务器> <工具> [参数JSON]", "usage: mcp call <server> <tool> [argsJSON]"), "error");
      const argsJson = restArgs.join(" ");
      try {
        const out = await invoke("mcp_call_tool", { name, tool, argsJson });
        if (typeof showCommandResult === "function") {
          showCommandResult(out.text);
        }
        status(out.isError ? L("MCP 工具返回错误", "MCP tool returned an error") : L("MCP 调用完成", "MCP call finished"));
      } catch (err) {
        status(L("调用失败: ", "Call failed: ") + err, "error");
      }
      return;
    }
    status(L("用法: mcp [list | call <服务器> <工具> {json}]", "usage: mcp [list | call <server> <tool> {json}]"), "error");
  }

  // ============================================
  // 初始化（main.js 调用）
  // ============================================

  function attach() {
    $("mcp-add-btn")?.addEventListener("click", addServer);
    $("mcp-start-btn")?.addEventListener("click", startServer);
    $("mcp-stop-btn")?.addEventListener("click", stopServer);
    $("mcp-call-btn")?.addEventListener("click", callTool);
    refresh();
  }

  window.McpUI = { attach, handleCommand };
})();
