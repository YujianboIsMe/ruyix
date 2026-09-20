/**
 * ruyix — 会话（session）：与 agent 的多轮对话。
 *
 * 模型：nav「会话」面板列出项目的会话（后端 agent_session_*，持久化在
 * `<root>/.ruyix/code/agent/sessions/`）；点开会话在中央编辑区打开一个聊天
 * tab（同文件/终端 tab 的生命周期）。会话内每条消息走 agent_reply —— 引擎的
 * Agent 工具循环（Read/Write/Execute/Connect 四大原子能力）：模型自己决定读什么、
 * 改什么、跑什么命令、连哪个外部能力（MCP 工具 / 远端 Agent），问答和任务在同一个
 * 循环里完成。模型声明 plan 时大纲区
 * 显示任务列表（三态 emoji）。产物写入按工具栏三模式分派：确认（暂存+人工勾选）/
 * 写入/自主（直写+备份）。无后端（浏览器直开）时走演示回复。
 */

(function () {
  "use strict";

  const L = (zh, en) => (window.I18N && I18N.getLang() === "en" ? en : zh);

  let sessions = [];        // 列表缓存（agent_session_list）
  let busy = false;         // 引擎单任务（与命令桥同一取消位模型）
  let runningPlaceholder = null; // 进行中 run 的助手占位消息（事件流实时刷新）
  let runSession = null;    // 正在跑 run 的会话（agent://plan / agent://step 事件的归属）

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
    return state?.currentProject?.path ?? null;
  }

  function status(msg, kind) {
    if (typeof setStatus === "function") setStatus(L(msg, msg), kind);
  }

  function truncate(s, n) {
    return String(s || "").length > n ? String(s).slice(0, n - 1) + "…" : String(s || "");
  }

  function nowHms() {
    return new Date().toTimeString().slice(0, 8);
  }

  // ============================================
  // 会话列表（nav 面板）
  // ============================================

  function renderList() {
    const el = $("session-list");
    if (!el) return;
    if (!sessions.length) {
      el.innerHTML = `<div class="agent-history-empty">${L("暂无会话", "No sessions yet")}</div>`;
      return;
    }
    el.innerHTML = sessions.map((s) =>
      `<div class="agent-history-item" data-open="${esc(s.id)}">` +
      `<span class="agent-dot ${s.messages.length ? "agent-dot--done" : ""}"></span>` +
      `<span class="agent-history-task">${esc(truncate(s.title || s.id, 26))}</span>` +
      `<span class="agent-history-time">${s.messages.length} ${L("条", "msgs")}</span>` +
      `<span class="mcp-icon-btn session-del" data-del="${esc(s.id)}" ` +
      `title="${L("删除", "remove")}">✕</span></div>`).join("");
    el.querySelectorAll("[data-open]").forEach((node) =>
      node.addEventListener("click", (e) => {
        if (e.target.dataset.del) return;
        openSessionById(node.dataset.open);
      }));
    el.querySelectorAll(".session-del").forEach((b) =>
      b.addEventListener("click", (e) => {
        e.stopPropagation();
        deleteSession(b.dataset.del);
      }));
  }

  async function refreshList() {
    const invoke = getInvoke();
    if (!invoke || !root()) {
      renderList();
      return;
    }
    try {
      sessions = (await invoke("agent_session_list", { projectRoot: root() })) ?? [];
      renderList();
    } catch (err) {
      status("会话列表刷新失败: " + err, "error");
    }
  }

  async function newSession(autofocus) {
    const invoke = getInvoke();
    let s;
    if (invoke && root()) {
      try {
        s = await invoke("agent_session_new", { projectRoot: root() });
        sessions.unshift(s);
      } catch (err) {
        return status("新建会话失败: " + err, "error");
      }
    } else {
      // 无后端：内存会话（演示对话）
      s = {
        id: "sess_demo_" + Date.now(),
        title: "",
        created_at: nowHms(),
        updated_at: nowHms(),
        messages: [],
      };
      sessions.unshift(s);
    }
    renderList();
    openSession(s, autofocus);
    return s;
  }

  async function deleteSession(id) {
    const invoke = getInvoke();
    // 关掉对应 tab（若开着）
    const tab = (state?.tabs ?? []).find((t) => t._isSession && t._session.id === id);
    if (tab && typeof closeTab === "function") closeTab(tab.id);
    if (invoke && root()) {
      try {
        sessions = (await invoke("agent_session_delete", { id, projectRoot: root() })) ?? [];
      } catch (err) {
        return status("删除失败: " + err, "error");
      }
    } else {
      sessions = sessions.filter((s) => s.id !== id);
    }
    renderList();
    status("会话已删除");
  }

  // ============================================
  // 中央会话 tab（消息流 + 输入）
  // ============================================

  function openSessionById(id) {
    const cached = sessions.find((s) => s.id === id);
    const invoke = getInvoke();
    if (cached) {
      openSession(cached);
    } else if (invoke && root()) {
      invoke("agent_session_load", { id, projectRoot: root() })
        .then((s) => openSession(s))
        .catch((err) => status("打开会话失败: " + err, "error"));
    }
  }

  function openSession(s) {
    if (!state) return;
    const exist = state.tabs.find((t) => t._isSession && t._session.id === s.id);
    if (exist) {
      exist._session = s; // 刷新数据（消息可能更新）
      if (exist._sessionEl) fillMsgs(exist._sessionEl, s);
      if (typeof switchTab === "function") switchTab(exist.id);
      return;
    }
    const tab = {
      id: "chat-" + s.id,
      name: truncate(s.title || L("新会话", "new chat"), 16),
      path: "",
      _isSession: true,
      _session: s,
      _sessionEl: null,
    };
    state.tabs.push(tab);
    if (typeof renderTabs === "function") renderTabs();
    if (typeof switchTab === "function") switchTab(tab.id);
  }

  /** 构建聊天 DOM（首次挂载；switchTab 时移入 #session-container） */
  function buildChatEl(s) {
    const wrap = document.createElement("div");
    wrap.className = "session-chat";
    wrap.innerHTML =
      `<div class="session-msgs" data-msgs></div>` +
      `<div class="session-input-row">` +
      `<textarea class="session-input" rows="2" placeholder="${L("交代工作，Ctrl+Enter 发送", "Describe the work — Ctrl+Enter to send")}"></textarea>` +
      `<div class="session-toolbar">` +
      `<select class="session-mode-select" data-mode>` +
      `<option value="confirm" selected title="${L("运行后自动打开差异确认框，人工勾选写入", "Open a diff panel after each run; you confirm what to write")}">🤝 ${L("确认模式", "Confirm")}</option>` +
      `<option value="write" title="${L("Agent 改动直接写入项目（覆盖前备份到 .ruyix/backups）", "Agent writes land directly (backed up to .ruyix/backups first)")}">✍️ ${L("写入模式", "Write")}</option>` +
      `<option value="auto" title="${L("同写入模式：改动直接落盘并自行动验证", "Same as write mode: changes land directly and the agent verifies itself")}">🚀 ${L("自主模式", "Autonomous")}</option>` +
      `</select>` +
      `<button class="agent-btn agent-btn--run session-send" data-send>▶</button>` +
      `<button class="agent-btn agent-btn--cancel session-cancel" data-cancel disabled>✕</button>` +
      `</div></div>`;
    fillMsgs(wrap, s);

    const input = wrap.querySelector(".session-input");
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        wrap.querySelector("[data-send]").click();
      }
    });
    // 模式随会话记忆（重开 tab 不丢；不持久化到磁盘 —— 每次新对话该想一下用哪种）
    const modeSel = wrap.querySelector("[data-mode]");
    if (s._mode) modeSel.value = s._mode;
    modeSel.addEventListener("change", () => {
      s._mode = modeSel.value;
    });
    wrap.querySelector("[data-send]").addEventListener("click", () => {
      const text = input.value.trim();
      if (!text) return;
      if (busy) return status("已有任务在跑，请先取消或等待", "error");
      input.value = "";
      sendMessage(s, wrap, text);
    });
    wrap.querySelector("[data-cancel]").addEventListener("click", () => {
      const invoke = getInvoke();
      if (invoke) invoke("agent_cancel").catch(() => {});
      setBusy(false);
      s.messages.push({
        role: "system", text: L("已取消", "canceled"), ts: nowHms(),
        run_id: null, status: "canceled",
      });
      fillMsgs(wrap, s);
      persist(s);
    });
    return wrap;
  }

  function fillMsgs(wrap, s) {
    const el = wrap.querySelector("[data-msgs]");
    if (!s.messages.length) {
      el.innerHTML = `<div class="session-empty">${L("和 agent 聊聊：描述要做的事 —— 它读文件、改代码、跑验证，交付前再由干净上下文的复核 agent 过一遍", "Chat with the agent: describe the work — it reads, writes, verifies, and a clean-context reviewer checks it before delivery")}</div>`;
      return;
    }
    el.innerHTML = s.messages.map(msgHtml).join("");
    el.scrollTop = el.scrollHeight;
  }

  /**
   * 验证 / 复核小节（v0.3 质量门禁）。
   *
   * 引擎每跑一次机械验证、每做一次复核都会推事件，run 结束时 ReplyAgent 里也带一份 ——
   * 有内容才渲染，没跑过就不占位。用户要能一眼看出"这轮到底验过没有"。
   */
  function gateHtml(m) {
    const vers = m.verify ?? [];
    const refs = m.reflect ?? [];
    if (!vers.length && !refs.length) return "";
    const rows = [];
    for (const v of vers) {
      const layer = v.layer === "full" ? L("全量验证", "full verify") : L("语法检查", "syntax");
      rows.push(
        `<div class="session-gate-row session-gate-row--${esc(v.status ?? "")}">` +
          `<span class="session-gate-tag">🔎 ${layer}</span>` +
          `<span class="session-gate-text">${esc(v.verdict ?? "")}</span></div>`);
      for (const c of (v.checks ?? []).filter((x) => x.status === "failed")) {
        rows.push(`<div class="session-gate-item">✗ ${esc(c.kind)} ${esc(c.target)}：${esc(c.reason ?? "")}</div>`);
      }
    }
    for (const r of refs) {
      const bad = r.verdict === "suspect";
      const label = r.note
        ? L("复核未完成", "review incomplete")
        : bad ? L("复核发现问题", "review found issues") : L("复核通过", "review ok");
      rows.push(
        `<div class="session-gate-row session-gate-row--${bad ? "failed" : "passed"}">` +
          `<span class="session-gate-tag">🧠 ${L("复核", "review")}</span>` +
          `<span class="session-gate-text">${esc(label)}${r.summary ? "：" + esc(r.summary) : ""}</span></div>`);
      for (const f of r.findings ?? []) {
        rows.push(
          `<div class="session-gate-item">[${esc(f.severity || "?")}] ${esc(f.claim ?? "")}` +
            (f.evidence ? `<span class="session-gate-ev">${esc(f.evidence)}</span>` : "") +
            `</div>`);
      }
      if (r.note) rows.push(`<div class="session-gate-item">${esc(r.note)}</div>`);
    }
    return `<div class="session-gate">${rows.join("")}</div>`;
  }

  /** 状态 slug → 展示文案（引擎给的是英文 slug，界面要说人话） */
  function statusLabel(s) {
    switch (s) {
      case "verified": return L("已验证", "verified");
      case "planned": return L("已规划", "planned");
      case "generated": return L("已生成", "generated");
      case "syntax-only": return L("仅语法检查", "syntax only");
      case "reviewed": return L("已复核", "reviewed");
      case "verify-failed": return L("验证未通过", "verify failed");
      case "canceled": return L("已取消", "canceled");
      case "failed": return L("失败", "failed");
      default: return s;
    }
  }

  /** 一次 run 的"验没验"结论 → 消息头上的状态 slug（没跑过就不挂徽章） */
  function gateStatus(m) {
    const vers = m.verify ?? [];
    const refs = m.reflect ?? [];
    if (!vers.length && !refs.length) return null;
    if (vers.some((v) => v.status === "failed")) return "verify-failed";
    if (vers.some((v) => v.layer === "full" && v.status === "passed")) return "verified";
    if (vers.length) return "syntax-only";
    return "reviewed";
  }

  // Agent 输出是 markdown：markdown-it 渲染（vendor 自 ui/markdown-it.min.js，
  // html:false —— 产物里的原生 HTML 一律转义，与 esc 同一安全底线）
  const md = window.markdownit
    ? window.markdownit({ html: false, breaks: true, linkify: true })
    : null;

  /** markdown → HTML；无渲染器时退化为转义文本（保留换行） */
  function mdHtml(text) {
    if (md) return md.render(text ?? "");
    return "<p>" + esc(text ?? "").replace(/\n/g, "<br>") + "</p>";
  }

  function msgHtml(m) {
    if (m.role === "system") {
      return `<div class="session-msg session-msg--system">${esc(m.text)}</div>`;
    }
    const mine = m.role === "user";
    const meta = [];
    if (m.run_id) meta.push(`<a class="session-run-id" data-run="${esc(m.run_id)}">${esc(m.run_id)}</a>`);
    if (m.status) {
      const okSet = ["verified", "planned", "generated", "syntax-only", "reviewed"];
      const ok = okSet.includes(m.status);
      meta.push(`<span class="session-status ${ok ? "session-status--ok" : "session-status--err"}">${esc(statusLabel(m.status))}</span>`);
    }
    const bubble = mine
      ? `<div class="session-bubble">${esc(m.text)}</div>`
      : `<div class="session-bubble session-bubble--md">${mdHtml(m.text)}</div>`;
    return `<div class="session-msg ${mine ? "session-msg--user" : "session-msg--agent"}">` +
      bubble +
      (mine ? "" : gateHtml(m)) +
      (meta.length ? `<div class="session-meta">${meta.join(" ")}</div>` : "") +
      `</div>`;
  }

  // ============================================
  // 产物写回：Agent 工具循环的改动 → 差异面板 → 勾选确认 → 落盘
  //
  // 循环按会话模式分策略（引擎 WritePolicy）：
  //   确认模式 — 改动只暂存到 <项目>/.ruyix/stage/，这里读出「暂存 vs 项目当前」
  //               的差异面板，人勾选后才调 agent_stage_apply（写前备份）；
  //   写入/自主 — 循环里已直接写入（覆盖前备份），会话里只报告结果。
  // ============================================

  /** 在指定助手气泡下打开暂存差异确认面板（确认模式循环结束后自动调） */
  function openStagePanel(owner, stageDir) {
    if (!owner || owner.querySelector(".session-apply")) return;
    const panel = document.createElement("div");
    panel.className = "session-apply";
    panel.dataset.stage = stageDir; // 写回走 agent_stage_apply
    panel.innerHTML = `<div class="session-apply-head">${L("正在读取差异…", "Reading changes…")}</div>`;
    owner.appendChild(panel);
    loadStagePreview(panel, stageDir);
  }

  const APPLY_KIND = {
    add: () => L("新增", "add"),
    modify: () => L("修改", "modify"),
    same: () => L("一致", "same"),
  };

  function kindLabel(k) {
    return (APPLY_KIND[k] ?? (() => k))();
  }

  function truncateLines(s, n) {
    const lines = String(s || "").split("\n");
    return lines.length > n
      ? lines.slice(0, n).join("\n") + `\n… ${L("省略", "omitted")} ${lines.length - n} ${L("行", "lines")}`
      : lines.join("\n");
  }

  async function loadStagePreview(panel, stageDir) {
    const invoke = getInvoke();
    if (!invoke || !root()) {
      panel.innerHTML = `<div class="session-apply-head">${L("未打开项目，无处写回", "No project open")}</div>`;
      return;
    }
    // stage_dir 形如 <项目>/.ruyix/stage/agent-<ts>；命令只要末段 id
    const stageId = String(stageDir).split(/[\\/]/).pop();
    try {
      const pv = await invoke("agent_stage_preview", { stageId, projectRoot: root() });
      renderApplyPanel(panel, pv);
    } catch (err) {
      panel.innerHTML = `<div class="session-apply-head session-apply-head--err">${esc(String(err))}</div>`;
    }
  }

  function renderApplyPanel(panel, pv) {
    const counts = { add: 0, modify: 0, same: 0 };
    (pv.changes ?? []).forEach((c) => {
      counts[c.kind] = (counts[c.kind] ?? 0) + 1;
    });
    if (!pv.changes?.length) {
      panel.innerHTML = `<div class="session-apply-head">${L("暂存区没有可写回的文件", "Nothing to apply")}</div>`;
      return;
    }
    const rows = pv.changes.map(rowHtml).join("");
    panel.innerHTML =
      `<div class="session-apply-head">` +
      `${L("改动差异", "Changes")}：${L("新增", "add")} ${counts.add} · ` +
      `${L("修改", "modify")} ${counts.modify} · ${L("一致", "same")} ${counts.same}` +
      (pv.dirty ? `<span class="session-apply-warn">⚠ ${L("项目工作树有未提交改动，建议先提交以便回滚", "Working tree is dirty — consider committing first")}</span>` : "") +
      `</div>` +
      `<label class="session-apply-all">` +
      `<input type="checkbox" data-toggle-all checked> ${L("全选", "select all")}</label>` +
      `<div class="session-apply-list">${rows}</div>` +
      `<div class="session-apply-actions">` +
      `<button class="agent-btn agent-btn--run" data-confirm>✓ ${L("写入项目", "Write to project")}</button>` +
      `<button class="agent-btn agent-btn--cancel" data-close>${L("取消", "cancel")}</button>` +
      `<span class="session-apply-hint">${L("被覆盖的文件会先备份到 .ruyix/backups", "Overwritten files are backed up under .ruyix/backups")}</span>` +
      `</div>`;

    panel.querySelector("[data-confirm]").addEventListener("click", () => doApply(panel, pv.run_id));
    panel.querySelector("[data-close]").addEventListener("click", () => panel.remove());
    panel.querySelector("[data-toggle-all]").addEventListener("change", (e) => {
      panel.querySelectorAll("input[data-path]:not([disabled])").forEach((i) => {
        i.checked = e.target.checked;
      });
    });
  }

  function rowHtml(c) {
    const same = c.kind === "same";
    // 覆盖既有文件时行数变少 = 极可能有内容被抹掉（模型重写整文件时的典型事故）。
    // 这类改动必须显式提示，否则"改了个依赖"会顺手删掉别的依赖。
    const bLines = c.before ? c.before.split("\n").length : 0;
    const aLines = c.after.split("\n").length;
    const loss = c.before && aLines < bLines ? bLines - aLines : 0;
    const detail = same ? "" :
      `<details class="session-apply-detail"><summary>${L("查看内容", "view")}</summary>` +
      (c.before === null || c.before === undefined ? "" :
        `<div class="session-apply-label">${L("修改前", "before")}</div>` +
        `<pre class="session-apply-pre session-apply-pre--before">${esc(truncateLines(c.before, 200))}</pre>`) +
      `<div class="session-apply-label">${c.before ? L("修改后", "after") : L("内容", "content")}</div>` +
      `<pre class="session-apply-pre session-apply-pre--after">${esc(truncateLines(c.after, 200))}</pre>` +
      `</details>`;
    const lossTag = loss
      ? `<span class="session-apply-loss">⚠ ${L("将少", "removes")} ${loss} ${L("行", "lines")}</span>`
      : "";
    return `<div class="session-apply-row${same ? " session-apply-row--same" : ""}">` +
      `<label>` +
      `<input type="checkbox" data-path="${esc(c.path)}"${same ? " disabled" : " checked"}>` +
      `<span class="session-apply-kind session-apply-kind--${esc(c.kind)}">${esc(kindLabel(c.kind))}</span>` +
      `<span class="session-apply-path">${esc(c.path)}</span>${lossTag}` +
      `</label>${detail}</div>`;
  }

  /** 会话里追加一条系统提示并刷新（不落 run_id —— 它是写回过程的旁白） */
  function sysMsg(s, wrap, text) {
    s.messages.push({ role: "system", text, ts: nowHms(), run_id: null, status: null });
    fillMsgs(wrap, s);
  }

  /** 唯一写入口径：暂存产物落盘，恒带备份（由确认面板的 data-confirm 触发） */
  function applyStage(stageId, paths) {
    const invoke = getInvoke();
    return invoke("agent_stage_apply", { stageId, projectRoot: root(), paths, backup: true });
  }

  async function doApply(panel, stageId) {
    const paths = [...panel.querySelectorAll("input[data-path]")]
      .filter((i) => i.checked)
      .map((i) => i.dataset.path);
    if (!paths.length) return status("没有勾选任何文件", "no file selected");
    try {
      const res = await applyStage(stageId, paths);
      const tail = res.backup_dir
        ? `${L("备份", "backup")}：${res.backup_dir}`
        : L("没有被覆盖的文件，无需备份", "nothing overwritten");
      const rows = res.skipped
        .map((s) => `<div class="session-apply-skip">${esc(s.path)} — ${esc(s.reason)}</div>`)
        .join("");
      panel.innerHTML =
        `<div class="session-apply-head session-apply-head--ok">` +
        `✓ ${L("已写入", "written")} ${res.applied.length} ${L("个文件", "files")} · ${esc(tail)}</div>` +
        `<div class="session-apply-list">${rows}</div>`;
      status(`已写入 ${res.applied.length} 个文件`, "ok");
    } catch (err) {
      status("写入失败: " + err, "error");
    }
  }

  // ============================================
  // 大纲区 · 任务计划
  //
  // 模型返回任务计划后，大纲区显示任务列表，步骤状态：
  //   ✅ 已完成   ⌛ 等待执行   ⛏️ 进行中   ❌ 失败   ⏹️ 本次未完成
  // 任务列表在模型调用伪工具 plan 的瞬间经 agent://plan 事件到达；
  // 随后 agent://step 事件按「步骤文件是否全部落地」推进状态（全落地 ✅ / 部分 ⛏️）。
  // run 收尾时引擎再发一轮终态（settle_steps）：没有声明文件的步骤、以及声明了文件
  // 却没产出的步骤，都在这时落定 —— 否则它们会永远停在初始的 ⌛（沙漏=等待执行，
  // run 已经结束还显示等待就是在骗人）。
  // 重启后打开旧会话，由最近一条带 run_id 的消息回读 run 记录补缓存（hydratePlan）。
  // ============================================

  const STEP_EMOJI = {
    done: "✅",       // 已完成
    running: "⛏️",   // 进行中
    error: "❌",      // 失败（独立状态：一眼看出 run 断在哪一步）
    pending: "⌛",    // 等待执行（plan 刚到、还没轮到它）
    skipped: "⏹️",   // 本次未完成（run 结束了还没产出；原因放 title 提示）
  };

  /** 计划（agent://plan 载荷或 RunRecord.plan）→ 会话步骤缓存（全 ⌛ 起步） */
  function setPlanSteps(s, plan) {
    if (!plan?.steps?.length) return;
    s._plan = plan.steps.map((x) => ({
      id: x.id,
      title: x.title || "",
      detail: x.detail ?? "",
      files: x.files ?? [],
      kind: x.kind ?? "code",
      st: "pending",
      note: "",
    }));
    renderOutline(s);
  }

  /** agent://step → 推进对应步骤（index 从 1 起，与计划步骤一一对应） */
  function applyStepEvent(s, p) {
    const st = (s._plan ?? [])[(p.index ?? 0) - 1];
    if (!st) return;
    if (p.status === "running") {
      st.st = "running";
      st.note = "";
    } else if (p.status === "done") {
      st.st = "done";
      st.note = "";
    } else if (p.status === "skipped") {
      // 终态：run 结束了这一步没产出（引擎在收尾事件里带上缺什么）
      st.st = "skipped";
      st.note = p.notes || L("本次未完成", "not done this run");
    } else {
      st.st = "error";
      st.note = L("本步失败", "failed") + (p.error ? `：${p.error}` : "");
    }
    renderOutline(s);
  }

  /** run 结束：RunRecord 是唯一事实 —— 重建计划结构并用 generation 终态覆盖 */
  function syncPlanFromRecord(s, rec) {
    if (rec?.plan?.steps?.length) setPlanSteps(s, rec.plan);
    const final = new Map(
      (rec?.generation?.steps ?? []).map((x) => [x.step_id, x.status]),
    );
    (s._plan ?? []).forEach((st) => {
      const f = final.get(st.id);
      if (f === "done") {
        st.st = "done";
        st.note = "";
      } else if (f === "skipped" || f === "error") {
        st.st = f === "skipped" ? "skipped" : "error";
        st.note =
          f === "skipped"
            ? L("本次未完成", "not done this run")
            : L("本步失败", "failed");
      }
    });
    renderOutline(s);
  }

  /** 无缓存时按最近一条带 run_id 的消息回读计划（重启后打开会话也有任务列表） */
  function hydratePlan(s) {
    if (s._plan || s._planHydrated) return;
    s._planHydrated = true;
    const invoke = getInvoke();
    if (!invoke || !root()) return;
    const last = [...s.messages].reverse().find((m) => m.role === "assistant" && m.run_id);
    if (!last) return;
    invoke("agent_run_load", { runId: last.run_id, projectRoot: root() })
      .then((rec) => syncPlanFromRecord(s, rec))
      .catch(() => {});
  }

  /**
   * 大纲区渲染该会话的任务列表。main.js 在切到会话 tab 时调用；
   * 事件（plan/step）到达时对本会话直接调用。会话 tab 不在台前时只补缓存不写 DOM
   * （切回来由 switchTab 再渲染），避免覆盖文件 tab 正在展示的文件大纲。
   */
  function renderOutline(s) {
    hydratePlan(s);
    const el = $("outline-content");
    if (!el || !state) return;
    const active = (state.tabs ?? []).find((t) => t.id === state.activeTabId);
    if (!active?._isSession || active._session !== s) return;
    const steps = s._plan ?? [];
    if (!steps.length) {
      el.innerHTML = `<div class="outline-placeholder">${L("暂无任务计划", "No task plan yet")}</div>`;
      return;
    }
    const done = steps.filter((x) => x.st === "done").length;
    const failed = steps.filter((x) => x.st === "error").length;
    // 未完成也要计数，否则「✅ 2/3」里的那个 1 没有解释
    const notDone = steps.filter((x) => x.st === "skipped").length;
    el.innerHTML =
      `<div class="outline-plan-head">${L("任务计划", "Task plan")} · ✅ ${done}/${steps.length}` +
      (notDone ? ` · ⏹️ ${notDone}` : "") +
      (failed ? ` · ❌ ${failed}` : "") + `</div>` +
      steps.map((x) => {
        const tip = [
          x.kind ? `[${x.kind}]` : "",
          x.detail,
          ...(x.files ?? []),
          x.note ? `⚠ ${x.note}` : "",
        ].filter(Boolean).join("\n");
        const cls = x.st === "running"
          ? " outline-plan-step--running"
          : x.st === "error"
            ? " outline-plan-step--error"
            : x.st === "skipped" ? " outline-plan-step--skipped" : "";
        return `<div class="outline-item outline-plan-step${cls}"` +
          ` title="${esc(tip)}">` +
          `<span class="outline-plan-emoji">${STEP_EMOJI[x.st] ?? STEP_EMOJI.pending}</span>` +
          `<span class="outline-plan-title">${esc(truncate((x.id ? `${x.id}. ` : "") + x.title, 30))}</span>` +
          `</div>`;
      }).join("");
  }

  // ============================================
  // 发送：消息 → 引擎任务（事件流折叠进助手气泡）
  // ============================================

  /** 工具栏当前模式：confirm（人工确认）/ write（验证过自动写）/ auto（无条件自动写） */
  function currentMode(wrap) {
    return wrap.querySelector("[data-mode]")?.value || "confirm";
  }

  function setBusy(b) {
    busy = b;
    document.querySelectorAll(".session-send").forEach((btn) => (btn.disabled = b));
    document.querySelectorAll(".session-cancel").forEach((btn) => (btn.disabled = !b));
  }

  /**
   * 验证/复核事件 → 进行中的助手气泡实时更新。
   * runningPlaceholder 是正在进行的那条消息对象；事件只改它的数据，DOM 由这里重渲染
   * （与 fillMsgs 同一套渲染函数，避免两处 HTML 长歪）。
   */
  function appendGate(payload, kind) {
    const ph = runningPlaceholder;
    if (!ph || !runSession) return;
    if (kind === "verify") {
      if (!ph.verify) ph.verify = [];
      ph.verify.push(payload);
    } else {
      if (!ph.reflect) ph.reflect = [];
      ph.reflect.push(payload);
    }
    ph.status = gateStatus(ph);
    const wrap = (state?.tabs ?? []).find((t) => t._isSession && t._session === runSession)?._sessionEl;
    const el = wrap?.querySelector("[data-msgs]");
    if (!el) return;
    el.innerHTML = runSession.messages.map(msgHtml).join("");
    el.scrollTop = el.scrollHeight;
  }

  async function sendMessage(s, wrap, text) {
    s.title = s.title || truncate(text, 24);
    // 对话历史（不含本条）：模型解析指代（"它/这个报错"指上一轮）靠它
    const history = s.messages
      .filter((m) => (m.role === "user" || m.role === "assistant") && m.text)
      .slice(-12)
      .map((m) => ({ role: m.role, text: m.text }));
    s.messages.push({ role: "user", text, ts: nowHms(), run_id: null, status: null });
    fillMsgs(wrap, s);

    const invoke = getInvoke();
    if (!invoke || !root()) {
      demoReply(s, wrap);
      return;
    }
    setBusy(true);
    const placeholder = { role: "assistant", text: L("…", "…"), ts: nowHms(), run_id: null, status: null };
    s.messages.push(placeholder);
    runningPlaceholder = placeholder;
    runSession = s;
    fillMsgs(wrap, s);
    let rep = null;
    const mode = currentMode(wrap);
    try {
      // Agent 工具循环（Read/Write/Execute/Connect）：问答会自己 read 项目，
      // 任务会改文件并验证，项目外的事交给 connect（MCP 工具 / 远端 Agent）
      rep = await invoke("agent_reply", { task: text, history, mode, projectRoot: root() });
      placeholder.text = rep.answer || L("（空回复）", "(empty reply)");
      // 验证 / 复核结论挂在这条助手消息上（持久化后重开也能看到"这轮验过没有"）
      placeholder.verify = rep.verifications ?? [];
      placeholder.reflect = rep.reflections ?? [];
      placeholder.status = gateStatus(placeholder);
    } catch (err) {
      placeholder.status = "failed";
      placeholder.text = String(err);
    } finally {
      runningPlaceholder = null;
      runSession = null;
      setBusy(false);
      fillMsgs(wrap, s);
      renderList();
      persist(s);
    }
    // 写回结果在消息流定稿后再处理（面板会被 fillMsgs 的重渲染冲掉）
    if (!rep) return;
    if (rep.stage_dir) {
      // 确认模式：改动在暂存区，会话内直接打开差异面板，人工勾选落盘
      const nodes = wrap.querySelectorAll(".session-msg--agent");
      openStagePanel(nodes[nodes.length - 1], rep.stage_dir);
    } else if (rep.changes?.length) {
      // 写入/自主模式：循环里已直接写入，报告结果
      const tail = rep.backup_dir
        ? `${L("备份", "backup")}：${rep.backup_dir}`
        : L("没有被覆盖的文件，无需备份", "nothing overwritten");
      sysMsg(s, wrap, `✅ ${L("已直接写入", "written")} ${rep.changes.length} ${L("个文件", "files")} · ${tail}`);
      persist(s);
    }
  }

  /** run 结果 → 一句话回复：不再需要 —— 工具循环的 final 答复本身就是回复 */

  async function persist(s) {
    const invoke = getInvoke();
    if (!invoke || !root()) {
      renderList();
      return;
    }
    try {
      const saved = await invoke("agent_session_save", {
        sessionJson: JSON.stringify(s),
        projectRoot: root(),
      });
      Object.assign(s, saved);
      const idx = sessions.findIndex((x) => x.id === s.id);
      if (idx >= 0) {
        sessions[idx] = s;
      } else {
        sessions.unshift(s);
      }
      renderList();
    } catch (err) {
      status("会话保存失败: " + err, "error");
    }
  }

  /** 无后端演示：假 agent 回复（浏览器直开 / ui-smoke） */
  function demoReply(s, wrap) {
    const last = s.messages[s.messages.length - 1];
    const text = last && last.role === "user" ? last.text : "";
    const reply = {
      role: "assistant",
      text: L(`（演示回复）收到：「${truncate(text, 40)}」。连接 Tauri 后端后，这里会真跑工具循环（读/写/执行/连接）+ 机械验证与复核。`,
        `(demo) Got it: "${truncate(text, 40)}". Connect the backend to run the real tool loop (read/write/execute/connect) with mechanical verification and review.`),
      ts: nowHms(),
      run_id: null,
      status: null,
    };
    setTimeout(() => {
      s.messages.push(reply);
      fillMsgs(wrap, s);
      renderList();
    }, 400);
  }

  // ============================================
  // 命令栏接入：agent [任务]
  // ============================================

  async function handleCommand(rest) {
    const arg = String(rest || "").trim();
    const s = await newSession(false);
    if (!s) return;
    if (!arg) return; // 仅打开新会话 tab
    const wrap = (state?.tabs ?? []).find((t) => t._isSession && t._session.id === s.id)
      ?._sessionEl;
    if (wrap) sendMessage(s, wrap, arg);
  }

  // ============================================
  // 初始化（main.js 调用）
  // ============================================

  function attach() {
    $("session-new-btn")?.addEventListener("click", () => newSession());
    // 引擎事件流 → 进行中的助手气泡实时显示阶段进度
    try {
      const listen = window.__TAURI__?.event?.listen;
      if (listen) {
        listen("agent://stage", (ev) => {
          const p = ev.payload ?? {};
          const ph = runningPlaceholder;
          if (ph) ph.text = `${p.stage ?? ""} ${p.status ?? ""} ${p.detail ?? ""}`.trim() || ph.text;
        });
        listen("agent://log", (ev) => {
          const p = ev.payload ?? {};
          if (p.level === "ok" && runningPlaceholder) runningPlaceholder.text = String(p.msg ?? "");
        });
        // 规划完成的瞬间 → 大纲区出现任务列表；随后 step 事件推进三态
        listen("agent://plan", (ev) => {
          if (runSession) setPlanSteps(runSession, ev.payload);
        });
        listen("agent://step", (ev) => {
          if (runSession) applyStepEvent(runSession, ev.payload ?? {});
        });
        // v0.3 质量门禁：验证 / 复核结论实时补进进行中的气泡（不用等 run 结束）
        listen("agent://verify", (ev) => appendGate(ev.payload ?? {}, "verify"));
        listen("agent://reflect", (ev) => appendGate(ev.payload ?? {}, "reflect"));
      }
    } catch {
      // 浏览器模式无 Tauri 事件
    }
    // 环境探针（沿用原 agent 面板的能力）
    const invoke = getInvoke();
    if (invoke) {
      invoke("agent_env_probe", { projectRoot: null })
        .then((env) => renderEnvChips(env))
        .catch(() => {});
    } else {
      renderEnvChips(null);
    }
    refreshList();
  }

  function renderEnvChips(env) {
    const el = $("agent-env-chips");
    if (!el) return;
    if (!env) {
      el.innerHTML = `<span class="agent-chip agent-chip--warn">✗ ${L("后端不可用", "backend unavailable")}</span>`;
      return;
    }
    const chips = [];
    const dockerOk = !!env.docker?.usable;
    chips.push(`<span class="agent-chip agent-chip--${dockerOk ? "ok" : "warn"}">${dockerOk ? "✓" : "✗"} docker ${dockerOk ? L("已隔离", "isolated") : L("未隔离", "host")}</span>`);
    for (const p of [env.python, env.node, env.git]) {
      const short = String(p?.version || "").trim().split(/\s+/).pop() || "";
      chips.push(`<span class="agent-chip agent-chip--${p?.available ? "ok" : "warn"}">${p?.available ? "✓" : "✗"} ${esc(p?.name ?? "?")} ${esc(short)}</span>`);
    }
    chips.push(`<span class="agent-chip agent-chip--${env.llm_key_configured ? "ok" : "warn"}">${env.llm_key_configured ? "✓" : "✗"} key ${env.llm_key_configured ? L("已配置", "set") : L("未配置", "missing")}</span>`);
    el.innerHTML = chips.join("");
  }

  /** main.js 挂钩：为会话 tab 懒构建 DOM（switchTab 时调用） */
  function ensureChatEl(tab) {
    if (!tab._sessionEl) {
      tab._sessionEl = buildChatEl(tab._session);
    }
    return tab._sessionEl;
  }

  /** 项目关闭（setNavigatorMode("projects")）：会话随项目走，收掉所有聊天 tab */
  function projectClosed() {
    if (!state) return;
    for (let i = state.tabs.length - 1; i >= 0; i--) {
      if (state.tabs[i]._isSession) state.tabs.splice(i, 1);
    }
    sessions = [];
    renderList();
  }

  window.SessionUI = {
    attach, handleCommand, ensureChatEl, projectClosed, refreshList,
    newSession, openSession, sendMessage, renderOutline,
  };
})();
