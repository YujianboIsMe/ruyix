/**
 * ruyix — A2A 面板（Agent-to-Agent 远端协作）
 *
 * agent 发现（agent card）→ 任务委托（message/send + 轮询）→ 结果回显。
 * 轮询进度经 a2a://status 事件推送（后端发送，前端只读展示）。
 */

(function () {
  "use strict";

  const L = (zh, en) => (window.I18N && I18N.getLang() === "en" ? en : zh);

  let agents = [];        // a2a_agents 的返回
  let selected = null;    // 选中的 agent 名
  let running = false;    // 是否有任务在跑

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

  function renderAgents() {
    const el = $("a2a-agent-list");
    if (!el) return;
    if (!agents.length) {
      el.innerHTML = `<div class="mcp-empty">${L("未注册远端 agent，用下方 URL 发现", "No remote agents — discover below")}</div>`;
      return;
    }
    el.innerHTML = agents.map((a) => {
      const sel = a.name === selected ? " mcp-server--sel" : "";
      const skills = (a.skills || []).slice(0, 4).map((s) =>
        `<span class="a2a-skill-chip">${esc(s)}</span>`).join("");
      return `<div class="mcp-server${sel}" data-name="${esc(a.name)}">` +
        `<div class="mcp-server-main">` +
        `<div class="mcp-server-name">🤖 ${esc(a.name)}` +
        (a.version ? `<span class="mcp-tools-count">${esc(a.version)}</span>` : "") + `</div>` +
        `<div class="mcp-server-cmd">${esc(a.url)}</div>` +
        (a.description ? `<div class="mcp-server-info">${esc(a.description)}</div>` : "") +
        `${skills}</div>` +
        `<button class="mcp-icon-btn mcp-del" data-name="${esc(a.name)}" ` +
        `title="${L("删除", "remove")}">✕</button></div>`;
    }).join("");
    el.querySelectorAll(".mcp-server").forEach((node) => {
      node.addEventListener("click", () => selectAgent(node.dataset.name));
    });
    el.querySelectorAll(".mcp-del").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        removeAgent(btn.dataset.name);
      });
    });
  }

  function renderTask() {
    const box = $("a2a-task-section");
    if (!box) return;
    box.style.display = selected ? "" : "none";
    if (!selected) return;
    const name = $("a2a-selected-name");
    if (name) name.textContent = selected;
    const send = $("a2a-send-btn");
    if (send) send.disabled = running;
  }

  function renderStateLine(text, kind) {
    const el = $("a2a-state-line");
    if (!el) return;
    el.textContent = text || "";
    el.className = "a2a-state-line" + (kind ? ` a2a-state-line--${kind}` : "");
  }

  function renderResult(text) {
    const el = $("a2a-result");
    if (el) el.textContent = text || L("（无输出）", "(no output)");
  }

  // ============================================
  // 动作
  // ============================================

  async function refresh() {
    const invoke = getInvoke();
    if (!invoke) {
      const el = $("a2a-agent-list");
      if (el) el.innerHTML = `<div class="mcp-empty">✗ ${L("后端不可用", "backend unavailable")}</div>`;
      return;
    }
    try {
      agents = await invoke("a2a_agents", { projectRoot: root() }) ?? [];
      if (selected && !agents.some((a) => a.name === selected)) selected = null;
      renderAgents();
      renderTask();
    } catch (err) {
      status(L("A2A 刷新失败: ", "A2A refresh failed: ") + err, "error");
    }
  }

  function selectAgent(name) {
    selected = name;
    renderAgents();
    renderTask();
    renderStateLine("");
    renderResult("");
  }

  async function discover() {
    const invoke = getInvoke();
    if (!invoke) return status(L("后端不可用", "backend unavailable"), "error");
    const url = ($("a2a-discover-url")?.value || "").trim();
    if (!url) return status(L("请填写 agent 基址 URL", "Enter the agent base URL"), "error");
    renderStateLine(L("发现中…", "discovering…"), "run");
    try {
      const cfg = await invoke("a2a_discover", { url });
      $("a2a-discover-url").value = "";
      status(L(`A2A 已注册: ${cfg.name}${cfg.version ? " " + cfg.version : ""}`, `A2A agent registered: ${cfg.name}${cfg.version ? " " + cfg.version : ""}`));
      selected = cfg.name;
      await refresh();
      renderStateLine("");
    } catch (err) {
      renderStateLine("");
      status(L("发现失败: ", "Discovery failed: ") + err, "error");
    }
  }

  async function removeAgent(name) {
    const invoke = getInvoke();
    if (!invoke) return;
    try {
      await invoke("a2a_remove", { name, projectRoot: root() });
      if (selected === name) selected = null;
      status(L("A2A 已删除: ", "A2A agent removed: ") + name);
      await refresh();
    } catch (err) {
      status(L("删除失败: ", "Remove failed: ") + err, "error");
    }
  }

  async function sendTask() {
    const invoke = getInvoke();
    if (!invoke || !selected || running) return;
    const text = ($("a2a-task-input")?.value || "").trim();
    if (!text) return status(L("请填写任务内容", "Enter the task text"), "error");
    running = true;
    renderTask();
    renderStateLine(L("发送中…", "sending…"), "run");
    renderResult("");
    try {
      const res = await invoke("a2a_send", { name: selected, text, projectRoot: root() });
      const stateText = {
        completed: L("✓ 完成", "✓ completed"),
        failed: L("✗ 失败", "✗ failed"),
        canceled: L("已取消", "canceled"),
        rejected: L("被拒绝", "rejected"),
      }[res.state] || res.state;
      renderStateLine(`${res.agent} · ${stateText}`, res.state === "completed" ? "ok" : "err");
      renderResult(res.text);
      status(L("A2A 任务结束: ", "A2A task finished: ") + res.state);
    } catch (err) {
      renderStateLine(L("✗ 出错", "✗ error"), "err");
      renderResult(String(err));
      status(L("A2A 发送失败", "A2A send failed"), "error");
    } finally {
      running = false;
      renderTask();
    }
  }

  // ============================================
  // 命令栏接入：a2a [list | send <name> <text>]
  // ============================================

  async function handleCommand(rest) {
    const args = String(rest || "").trim().split(/\s+/).filter(Boolean);
    const invoke = getInvoke();
    if (!invoke) return status(L("后端不可用", "backend unavailable"), "error");
    if (!args.length || args[0] === "list") {
      try {
        const list = await invoke("a2a_agents", { projectRoot: root() });
        const lines = (list ?? []).map((a) => `🤖 ${a.name} — ${a.url}`);
        status(lines.length ? lines.join(" | ") : L("未注册 A2A agent", "No A2A agent registered"));
      } catch (err) {
        status(L("A2A 查询失败: ", "A2A query failed: ") + err, "error");
      }
      return;
    }
    if (args[0] === "send") {
      const name = args[1];
      const text = args.slice(2).join(" ");
      if (!name || !text) return status(L("用法: a2a send <agent名> <任务文本>", "usage: a2a send <agent> <task text>"), "error");
      try {
        const res = await invoke("a2a_send", { name, text, projectRoot: root() });
        if (typeof showCommandResult === "function") {
          showCommandResult(res.text);
        }
        status(L("A2A 任务结束: ", "A2A task finished: ") + res.state);
      } catch (err) {
        status(L("发送失败: ", "Send failed: ") + err, "error");
      }
      return;
    }
    status(L("用法: a2a [list | send <agent名> <文本>]", "usage: a2a [list | send <agent> <text>]"), "error");
  }

  // ============================================
  // 初始化
  // ============================================

  function attach() {
    $("a2a-discover-btn")?.addEventListener("click", discover);
    $("a2a-send-btn")?.addEventListener("click", sendTask);
    // 远端轮询进度（后端事件；浏览器模式静默跳过）
    try {
      const listen = window.__TAURI__?.event?.listen;
      if (listen) {
        listen("a2a://status", (ev) => {
          const p = ev.payload ?? {};
          if (!p.name || p.name !== selected) return;
          if (p.max) {
            renderStateLine(`${p.state} · ${L("轮询", "poll")} ${p.poll}/${p.max}`, "run");
          } else if (p.state) {
            renderStateLine(p.state, "run");
          }
        }).catch(() => {});
      }
    } catch {
      // 浏览器模式无 Tauri 事件
    }
    refresh();
  }

  window.A2aUI = { attach, handleCommand };
})();
