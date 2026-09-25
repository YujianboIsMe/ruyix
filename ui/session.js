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
  let runWrap = null;       // 它的聊天 DOM（提问卡要就地重渲染）

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
      status(L("会话列表刷新失败: ", "Session list refresh failed: ") + err, "error");
    }
  }

  /**
   * 项目打开 / 切换后同步会话列表 —— **这是"重启 IDE 会话历史全丢"的根因所在**。
   *
   * `refreshList()` 原先全仓只被调一次（`attach()` 末尾），而那一刻项目往往还没打开 →
   * `root()` 为空 → 直接早退，之后再没人叫它。于是文件明明躺在
   * `<root>/.ruyix/code/agent/sessions/`（实测 cloud-shop 里 27 个），面板却永远写着"暂无会话"。
   * 所以"项目打开"这个收口点必须显式同步一次。
   *
   * `autoOpenNewest`：把最近一条**有消息**的会话直接开成 tab（接着上次继续）。空会话
   * （新建但一句没用过）不占位 —— 免得每次打开项目都弹一个空聊天。
   */
  async function syncForProject(autoOpenNewest) {
    if (!root()) {
      sessions = [];
      renderList();
      return;
    }
    await refreshList();
    if (!autoOpenNewest) return;
    const newest = sessions.find((s) => (s.messages?.length ?? 0) > 0);
    if (newest) openSessionById(newest.id);
  }

  async function newSession(autofocus) {
    const invoke = getInvoke();
    let s;
    if (invoke && root()) {
      try {
        s = await invoke("agent_session_new", { projectRoot: root() });
        sessions.unshift(s);
      } catch (err) {
        return status(L("新建会话失败: ", "Failed to create session: ") + err, "error");
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
        return status(L("删除失败: ", "Delete failed: ") + err, "error");
      }
    } else {
      sessions = sessions.filter((s) => s.id !== id);
    }
    renderList();
    status(L("会话已删除", "Session deleted"));
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
        .catch((err) => status(L("打开会话失败: ", "Failed to open session: ") + err, "error"));
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
      `<button class="agent-btn session-web" data-web title="${L("服务端联网检索（按模型能力决定可用与否）", "Server-side web search (availability depends on the model)")}">🌏</button>` +
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
    // 🌏 服务端联网检索：开关写在 **runtime 作用域**（本次会话生效、不落盘）。
    //
    // 能不能用取决于**模型能力**（矩阵在引擎 `llm::model_caps` 里，宿主不抄一份）——
    // 实测只有 v4-pro 在 /responses 上真检索，flash 一次都不检索。所以模型不支持时
    // 直接禁用并说清原因：**不给一个按下去没反应的按钮**。
    const webBtn = wrap.querySelector("[data-web]");
    wrap._webCaps = null;
    const paintWeb = () => {
      const caps = wrap._webCaps;
      if (!caps) {
        webBtn.disabled = true;
        webBtn.title = L("联网能力未知（未取到模型能力）", "Web capability unknown");
        return;
      }
      if (!caps.web_search) {
        webBtn.disabled = true;
        webBtn.title = L(
          L(`当前模型 ${caps.model} 不支持服务端联网检索`, `model ${caps.model} does not support server-side web search`),
          `Model ${caps.model} has no server-side web search`
        );
        return;
      }
      const on = s._webSearch === undefined ? true : s._webSearch;
      webBtn.disabled = false;
      webBtn.classList.toggle("session-web--on", !!on);
      webBtn.title = on
        ? L("服务端联网检索：已开启（点此关闭）", "Web search: on (click to disable)")
        : L("服务端联网检索：已关闭（点此开启）", "Web search: off (click to enable)");
    };
    webBtn.addEventListener("click", async () => {
      const on = !(s._webSearch === undefined ? true : s._webSearch);
      s._webSearch = on;
      const invoke = getInvoke();
      if (invoke) {
        try {
          await invoke("config_form_apply", {
            scope: "runtime",
            projectRoot: root(),
            entries: [
              { section: "harness", key: "llm.web_search", value: on ? "auto" : "off" },
            ],
          });
        } catch (e) {
          status(String(e), "error");
        }
      }
      paintWeb();
    });
    (async () => {
      const invoke = getInvoke();
      if (!invoke) return;
      try {
        wrap._webCaps = await invoke("ai_model_caps", { model: null, projectRoot: root() });
      } catch {
        wrap._webCaps = null;
      }
      paintWeb();
    })();
    paintWeb();

    wrap.querySelector("[data-send]").addEventListener("click", () => {
      const text = input.value.trim();
      if (!text) return;
      if (busy) return status(L("已有任务在跑，请先取消或等待", "A task is already running — cancel it or wait"), "error");
      input.value = "";
      sendMessage(s, wrap, text);
    });
    wrap.querySelector("[data-cancel]").addEventListener("click", async () => {
      const invoke = getInvoke();
      if (invoke) invoke("agent_cancel").catch(() => {});
      setBusy(false);
      s.messages.push({
        role: "system", text: L("已取消", "canceled"), ts: nowHms(),
        run_id: null, status: "canceled",
      });
      fillMsgs(wrap, s);
      await persist(s);
    });
    // 提问卡（v0.8 ask_user）：卡片按钮/输入框由 askHtml 在每次重渲染时重建，
    // 所以走**事件委托**（绑在容器上），而不是逐个按钮 addEventListener。
    wrap.addEventListener("click", (e) => {
      const opt = e.target.closest("[data-ask-opt]");
      if (opt) {
        answerAsk(opt.dataset.askId, opt.dataset.askText ?? "", Number(opt.dataset.askOpt));
        return;
      }
      const send = e.target.closest("[data-ask-send]");
      if (send) {
        const inp = wrap.querySelector(`[data-ask-input="${send.dataset.askId}"]`);
        answerAsk(send.dataset.askId, (inp?.value ?? "").trim(), null);
      }
    });
    wrap.addEventListener("keydown", (e) => {
      if (e.key !== "Enter") return;
      const inp = e.target.closest("[data-ask-input]");
      if (!inp) return;
      e.preventDefault();
      answerAsk(inp.dataset.askInput, inp.value.trim(), null);
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

  /** 没答到的几种原因（照实显示，不粉饰） */
  function askStateText(state) {
    if (state === "timeout") return L("等超时了", "timed out");
    if (state === "canceled") return L("你取消了这次运行", "run canceled");
    if (state === "no_asker") return L("当前环境没有提问通道", "no asking channel here");
    if (state === "expired") return L("回答晚了一步，提问已失效", "answered too late");
    if (state === "failed") return L("提问失败", "asking failed");
    return state || L("未知", "unknown");
  }

  /**
   * 提问小节（v0.8 `ask_user`）：需求歧义时模型问用户的那一问。
   *
   * 展示顺序严格是「问什么 → **为什么问** → 怎么答」：`why`（这个答案会决定什么）不是装饰 ——
   * 用户凭它判断这一问值不值得答，也是"模型不许把危险动作包装成一个无害问题"的落点。
   * 已答过的提问在重开会话后照样看得见（跟着消息存档），所以这里既渲染"待答"也渲染"已答"。
   */
  function askHtml(m) {
    const asks = m.ask ?? [];
    if (!asks.length) return "";
    const rows = [];
    for (const a of asks) {
      const head = `<div class="session-ask-q">❓ ${esc(a.question ?? "")}</div>`;
      const why = a.why
        ? `<div class="session-ask-why">${L("为什么问", "Why")}：${esc(a.why)}</div>`
        : "";
      let body;
      if (a.state === "asking") {
        const opts = (a.options ?? [])
          .map((o, i) =>
            `<button class="agent-btn session-ask-opt" data-ask-id="${esc(a.id)}" data-ask-opt="${i}" data-ask-text="${esc(o)}">${esc(o)}</button>`)
          .join("");
        const note = a.timeout_secs
          ? `<div class="session-ask-note">${L(`超过 ${a.timeout_secs} 秒不回答，这一问就作废 —— 引擎不会替你猜`,
            `Unanswered for ${a.timeout_secs}s and the question expires — the engine will not guess`)}</div>`
          : "";
        body =
          (opts ? `<div class="session-ask-opts">${opts}</div>` : "") +
          `<div class="session-ask-free">` +
          `<input class="session-ask-input" data-ask-input="${esc(a.id)}" placeholder="${L("或者直接写下你的答案…", "Or type your answer…")}">` +
          `<button class="agent-btn agent-btn--run" data-ask-id="${esc(a.id)}" data-ask-send>${L("回答", "Answer")}</button>` +
          `</div>` + note;
      } else if (a.state === "answered") {
        body = `<div class="session-ask-answer">` +
          `<span class="session-ask-ok">${L("你的回答", "Your answer")}：${esc(a.answer ?? "")}</span></div>`;
      } else {
        body = `<div class="session-ask-answer">` +
          `<span class="session-ask-miss">${L("没有回答", "No answer")}（${esc(askStateText(a.state))}）</span></div>`;
      }
      rows.push(`<div class="session-ask">${head}${why}${body}</div>`);
    }
    return `<div class="session-gate session-ask-box">${rows.join("")}</div>`;
  }

  /** `agent://ask`：把问题挂到正在跑的那条助手消息上（重渲染后由 askHtml 画出来） */
  function showAskCard(p) {
    const s = runSession;
    const m = runningPlaceholder;
    if (!s || !m || !p.id) return;                    // 没有在跑的 run：忽略（残留事件）
    const list = m.ask ?? [];
    if (list.some((x) => x.id === p.id)) return;      // 幂等：事件重放不重复插
    m.ask = [...list, { ...p, state: "asking" }];
    if (runWrap) fillMsgs(runWrap, s);
    status(L("Agent 在等你回答一个问题", "The agent is waiting for your answer"));
  }

  /**
   * 回答一个提问。失效（超时 / 已取消 / 已答过）时**照实说**，不假装成功 ——
   * 答案是语义与权限的输入，投错比投丢更糟（后端返回 false 就是判据）。
   */
  async function answerAsk(askId, text, optionIndex) {
    const invoke = getInvoke();
    if (!invoke) return;
    let ok = false;
    try {
      ok = await invoke("agent_ask_answer", { askId, text, optionIndex });
    } catch (err) {
      ok = false;
    }
    for (const a of (runningPlaceholder?.ask ?? [])) {
      if (a.id === askId) {
        a.state = ok ? "answered" : "expired";
        a.answer = text;
      }
    }
    if (!ok) status(L("这个提问已经失效（超时 / 已取消）", "This question has expired"), "error");
    if (runWrap && runSession) fillMsgs(runWrap, runSession);
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

  // ============================================
  // 执行轨迹（v0.0.5）：agent 在跑什么，一行一条
  //
  // 为什么要有它：引擎一直在发 `agent://log`（每次工具调用一条 `第 N 轮 <tool> ✓ <摘要>`）
  // 与 `agent://stage`（阶段通告），原先的监听**只改 `ph.text` 且不重渲染** ——
  // 运行中的气泡于是永远停在初始的 "…"，用户完全看不到 agent 在干什么。
  // 现在统一进这份轨迹：一次思考 / 一次调用 / 一条告警各占一行，一行显示不全用省略号收尾
  // （`title` 里给全文），并跟着助手消息落盘（`SessionMsg.trace`，否则重开就丢）。
  // ============================================

  /** 轨迹行的 kind → 图标。未知 kind 一律按 think 画（不假装认得）。 */
  const TRACE_ICON = { think: "💭", do: "⚙", fail: "⚠", done: "✅" };
  /** 气泡里最多画几行（更早的折起来 —— 一轮 run 有几十条时，气泡不能被日志撑爆） */
  const TRACE_SHOW = 24;
  /** 最多存几行（落盘体积闸；超出丢最老的） */
  const TRACE_KEEP = 200;

  /** 一条轨迹 → 一行（换行会破坏"一行一条"，所以在这里先压平）。
   *  `kind` 只认清单里的四个（`hasOwnProperty`，**不走原型链**）：它会被持久化、
   *  从盘上的会话文件读回来，所以是**输入不是常量** —— 实测 `TRACE_ICON[t.kind]` 这种
   *  真值判断会被 `kind: "constructor"` 命中，图标格里漏出 `function Object() { [native code] }`，
   *  class 也拼成 `session-trace-row--constructor`。 */
  function traceRowHtml(t) {
    const kind = Object.prototype.hasOwnProperty.call(TRACE_ICON, t.kind) ? t.kind : "think";
    const text = String(t.text ?? "").replace(/\s+/g, " ").trim();
    return `<div class="session-trace-row session-trace-row--${kind}">` +
      `<span class="session-trace-tag">${TRACE_ICON[kind]}</span>` +
      `<span class="session-trace-text" title="${esc(text)}">${esc(text)}</span></div>`;
  }

  /**
   * 轨迹小节。`live` = 这条消息正在跑（此时即便还没有一条事件也占位，
   * 否则首个事件到达时连落点都没有）。
   */
  function traceHtml(m, live) {
    const rows = m.trace ?? [];
    if (!rows.length && !live) return "";
    const shown = rows.slice(-TRACE_SHOW);
    const folded = rows.length - shown.length;
    // "未显示"而不是"已收起"：超出 TRACE_KEEP 的是**真丢了**，说成"收起"就成了谎
    const more = folded > 0
      ? `<div class="session-trace-more">${L(`… 更早 ${folded} 条未显示`, `… ${folded} earlier lines not shown`)}</div>`
      : "";
    const body = shown.length
      ? shown.map(traceRowHtml).join("")
      : `<div class="session-trace-row session-trace-row--think">` +
        `<span class="session-trace-tag">${TRACE_ICON.think}</span>` +
        `<span class="session-trace-text">${L("正在准备…", "Preparing…")}</span></div>`;
    return `<div class="session-trace" data-trace-live="${live ? "1" : "0"}">` +
      `<div class="session-trace-head">${L("思考 · 执行", "Thinking · Steps")}</div>` +
      more + body + `</div>`;
  }

  /**
   * 引擎日志 → 轨迹的一行。分类只按**日志自己说的事**，不猜：
   *   收尾（"完成，共 N 轮"）→ done；warn/error 或 ✗ → fail；✓ → do（一次调用）；
   *   其余（阶段判定 / 检索 / 折叠 / 提问）→ think。
   */
  function traceFromLog(level, msg) {
    const text = String(msg ?? "").replace(/^\[agent\]\s*/, "").replace(/\s+/g, " ").trim();
    if (!text) return null;
    if (/完成，共 \d+ 轮/.test(text)) return { kind: "done", text };
    if (level === "warn" || level === "error" || text.includes("✗")) return { kind: "fail", text };
    if (text.includes("✓")) return { kind: "do", text };
    return { kind: "think", text };
  }

  /** 进行中 run 的消息容器（runWrap 优先，tab 查回来兜底 —— 事件可能晚于切 tab） */
  function runMsgsEl() {
    const wrap = runWrap ?? (state?.tabs ?? [])
      .find((t) => t._isSession && t._session === runSession)?._sessionEl;
    return wrap?.querySelector("[data-msgs]") ?? null;
  }

  /**
   * 进行中气泡重渲染。事件只改消息对象上的数据，DOM 一律走这条（与 fillMsgs
   * 同一套渲染函数，避免两处 HTML 长歪），并滚到底 —— 新的一行要在视野里。
   */
  function rerenderRun() {
    const el = runMsgsEl();
    if (!runSession || !el) return;
    el.innerHTML = runSession.messages.map(msgHtml).join("");
    el.scrollTop = el.scrollHeight;
  }

  /** 事件 → 轨迹追加一行（一行一条，超出 TRACE_KEEP 丢最老的） */
  function pushTrace(kind, text) {
    const ph = runningPlaceholder;
    const line = String(text ?? "").replace(/\s+/g, " ").trim();
    if (!ph || !line) return;
    const list = ph.trace ?? (ph.trace = []);
    list.push({ kind, text: line });
    if (list.length > TRACE_KEEP) list.splice(0, list.length - TRACE_KEEP);
    rerenderRun();
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
    const live = !mine && m === runningPlaceholder;
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
    // 有轨迹的消息放宽一点宽度：轨迹是日志，路径/命令比对话正文长，78% 会一路省略号
    const wide = !mine && (live || (m.trace ?? []).length) ? " session-msg--trace" : "";
    return `<div class="session-msg ${mine ? "session-msg--user" : "session-msg--agent"}${wide}">` +
      bubble +
      (mine ? "" : gateHtml(m) + askHtml(m) + traceHtml(m, live)) +
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
    if (!paths.length) return status(L("没有勾选任何文件", "no file selected"), "no file selected");
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
      status(L(`已写入 ${res.applied.length} 个文件`, `${res.applied.length} file(s) written`), "ok");
    } catch (err) {
      status(L("写入失败: ", "Write failed: ") + err, "error");
    }
  }

  // ============================================
  // 大纲区 · 任务计划
  //
  // 模型返回任务计划后，大纲区显示任务列表，步骤状态：
  //   ✅ 已完成   ⌛ 等待执行   ⛏️ 进行中   ⚠️ 未对齐   ❌ 失败   ⏹️ 本次未完成
  // 任务列表在模型调用伪工具 plan 的瞬间经 agent://plan 事件到达。
  // execute_plan 开启时（默认）状态由**引擎**报：派发前 ⛏️、跑完 ✅、失败 ❌。
  // 关着的时候只能靠推断（"步骤声明的文件是否落地"：全落地 ✅ / 部分 ⛏️）。
  // run 收尾时引擎再发一轮终态（settle_steps），给每个步骤一个终态 —— 否则没轮到派发的
  // 步骤会永远停在初始的 ⌛（沙漏=等待执行，run 已经结束还显示等待就是在骗人）。
  // 收尾的推断口径要**宽**：声明了却本来就存在、无需重写的文件不算缺；有产出但对不上
  // 声明的落 ⚠️「未对齐」而不是 ⏹️"跳过"（跳过=一件没做，比事实重，和沙漏是同一种骗人）。
  // run 结束把当前计划（含终态）挂到助手消息上随会话落盘：会话跑的是工具循环，
  // 它不落 RunRecord、run_id 是空的，重启后没有别的可回读 —— 不存快照，
  // 重开旧会话时大纲区的任务列表会整个消失（不是沙漏，是压根没有）。
  // 老会话没有快照字段，仍按最近一条带 run_id 的消息回读 run 记录（hydratePlan）。
  // ============================================

  const STEP_EMOJI = {
    done: "✅",       // 已完成
    running: "⛏️",   // 进行中
    error: "❌",      // 失败（独立状态：一眼看出 run 断在哪一步）
    partial: "⚠️",   // 未对齐：做了，产出却和它自己开列的清单不一致（改名/少写/只写一部分）
    pending: "⌛",    // 等待执行（plan 刚到、还没轮到它）
    skipped: "⏹️",   // 本次未完成（run 结束了还没产出；原因放 title 提示）
  };

  /** 大纲头部的计数：每种终态各几个（pending 不数，它就是"还没轮到"） */
  function outlineCounts(steps) {
    const n = { done: 0, partial: 0, error: 0, skipped: 0 };
    for (const x of steps) if (n[x.st] !== undefined) n[x.st] += 1;
    return n;
  }

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

  /**
   * s._plan（UI 形状）→ 落盘快照：步骤定义与各步终态分开存，
   * 与 `RunRecord.plan` + `RunRecord.generation.steps` 是同一种分工。
   */
  function planSnapshot(steps) {
    if (!steps?.length) return null;
    return {
      steps: steps.map((x) => ({
        id: x.id ?? 0,
        title: x.title ?? "",
        detail: x.detail ?? "",
        files: x.files ?? [],
        kind: x.kind ?? "code",
      })),
      states: steps.map((x) => ({
        id: x.id ?? 0,
        st: x.st ?? "pending",
        note: x.note ?? "",
      })),
    };
  }

  /** 落盘快照 → s._plan（终态按 id 合并回来；快照缺 states 时退化成全 ⌛） */
  function planFromSnapshot(snap) {
    const steps = snap?.steps ?? [];
    if (!steps.length) return null;
    const states = snap.states ?? [];
    const byId = new Map(states.map((x) => [x.id, x]));
    return steps.map((x, i) => {
      const s2 = byId.get(x.id) ?? states[i];
      return {
        id: x.id,
        title: x.title || "",
        detail: x.detail ?? "",
        files: x.files ?? [],
        kind: x.kind ?? "code",
        st: s2?.st || "pending",
        note: s2?.note ?? "",
      };
    });
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
      // 终态：run 结束了这一步一件没做（引擎在收尾事件里带上缺什么）
      st.st = "skipped";
      st.note = p.notes || L("本次未完成", "not done this run");
    } else if (p.status === "partial") {
      // 终态：做了事，产出却和它自己开列的清单不一致。**不能落到下面的 error** ——
      // 那是"失败"，比事实重；引擎在 notes 里带了「已写哪些 / 还缺哪些」。
      st.st = "partial";
      st.note = p.notes || L("产出与声明不一致", "produced ≠ declared");
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

  /** 无缓存时恢复大纲：先读助手消息里的计划快照，再回退到带 run_id 的 run 记录 */
  function hydratePlan(s) {
    if (s._plan || s._planHydrated) return;
    s._planHydrated = true;
    // 工具循环（会话里的 run）不落 RunRecord，run_id 是空的 —— 计划就存在消息上
    const snap = [...s.messages]
      .reverse()
      .find((m) => m.role === "assistant" && m.plan?.steps?.length)?.plan;
    if (snap) {
      s._plan = planFromSnapshot(snap);
      renderOutline(s);
      return;
    }
    // 老会话（快照字段出现之前落的盘）：仍按最近一条带 run_id 的消息回读 run 记录
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
    // ⚠️/⏹️ 都要计数：不给数字，用户没法解释「✅ 2/8」剩下的 6 是什么。
    // 两者必须分开 —— ⚠️ 是"做了但产出与声明不一致"，⏹️ 才是"这步一件没做"；
    // 把前者也说成"跳过"是拿比事实更重的词描述，和沙漏是同一种骗人。
    const c = outlineCounts(steps);
    el.innerHTML =
      `<div class="outline-plan-head">${L("任务计划", "Task plan")} · ✅ ${done}/${steps.length}` +
      (c.partial ? ` · ⚠️ ${c.partial}` : "") +
      (c.skipped ? ` · ⏹️ ${c.skipped}` : "") +
      (failed ? ` · ❌ ${failed}` : "") + `</div>` +
      steps.map((x) => {
        const tip = [
          x.kind ? `[${x.kind}]` : "",
          x.detail,
          ...(x.files ?? []),
          x.note ? `⚠ ${x.note}` : "",
        ].filter(Boolean).join("\n");
        const cls = x.st === "running" || x.st === "partial" || x.st === "error" ||
          x.st === "skipped"
          ? ` outline-plan-step--${x.st}`
          : "";
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
   * runningPlaceholder 是正在进行的那条消息对象；事件只改它的数据、**不碰 DOM**，
   * 重渲染统一走 rerenderRun（与 fillMsgs 同一套渲染函数，避免两处 HTML 长歪）。
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
    rerenderRun();
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
    // 先落盘用户这句话：run 跑一半崩了 / 被强杀，也不至于整段对话消失。
    // **必须 await**：同一会话两次落盘共用同一个 `.json.tmp`，并发时会有一次 rename 失败。
    await persist(s);

    const invoke = getInvoke();
    if (!invoke || !root()) {
      demoReply(s, wrap);
      return;
    }
    setBusy(true);
    // 占位气泡的正文在 run 结束前保持这句话不动 —— 实时进展由下面的轨迹小节承载
    // （原先把日志一行行写进 text 又不重渲染，于是永远停在 "…"）
    const placeholder = {
      role: "assistant", text: L("正在执行…", "Working…"),
      ts: nowHms(), run_id: null, status: null,
    };
    s.messages.push(placeholder);
    runningPlaceholder = placeholder;
    runSession = s;
    runWrap = wrap;
    fillMsgs(wrap, s);
    let rep = null;
    const mode = currentMode(wrap);
    try {
      // Agent 工具循环（Read/Write/Execute/Connect）：问答会自己 read 项目，
      // 任务会改文件并验证，项目外的事交给 connect（MCP 工具 / 远端 Agent）
      // sessionId 是转录压实收据的**坐标**（"打开会话 X 的第 a..b 条"）：不带就只能退回
      // "任务前缀"，换回时人要自己找。注意这行**保持单行** —— U16 认的就是这个形状。
      rep = await invoke("agent_reply", { task: text, history, mode, projectRoot: root(), sessionId: (s && s.id) || null });
      placeholder.text = rep.answer || L("（空回复）", "(empty reply)");
      // 验证 / 复核结论挂在这条助手消息上（持久化后重开也能看到"这轮验过没有"）
      placeholder.verify = rep.verifications ?? [];
      placeholder.reflect = rep.reflections ?? [];
      // 提问留痕（v0.8）：跟着消息存档，重开会话仍看得见问过什么、怎么答的
      placeholder.ask = rep.asks ?? [];
      placeholder.status = gateStatus(placeholder);
    } catch (err) {
      placeholder.status = "failed";
      placeholder.text = String(err);
    } finally {
      runningPlaceholder = null;
      runSession = null;
      runWrap = null;
      setBusy(false);
      // 计划（含终态）跟着这条消息落盘：工具循环不落 RunRecord，run_id 是空的，
      // 这是重启后恢复大纲区任务列表的唯一来源
      const snap = planSnapshot(s._plan);
      if (snap) placeholder.plan = snap;
      fillMsgs(wrap, s);
      renderList();
      await persist(s);
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
      await persist(s);
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
      // 后端回显的是**请求发出那一刻**的快照 —— 只同步元数据，绝不整体 assign。
      //
      // `Object.assign(s, saved)` 会把 `messages` 一起盖回来：请求发出之后才 push 的消息
      // （运行中的助手气泡就是）不在后端那份快照里，于是被从 `s.messages` 里**抹掉**。
      // 真凶现场：2026-09-21 20:13 那轮「飞机大战」（sess_2026-09-21T1733068908168000800.json）
      // 盘上只剩 [用户任务, 写回回执] 两条，助手回复连同它的计划快照一起消失 ——
      // 于是重启后大纲区的 ❌ 与失败原因再也回看不了（`_plan` 活在内存里，当场看着还是好的）。
      // 会话内容的事实源永远是内存里这份 `s`；后端是持久化通道，不是权威。
      if (saved?.id) s.id = saved.id;
      if (saved?.title != null) s.title = saved.title;
      if (saved?.created_at) s.created_at = saved.created_at;
      if (saved?.updated_at) s.updated_at = saved.updated_at;
      const idx = sessions.findIndex((x) => x.id === s.id);
      if (idx >= 0) {
        sessions[idx] = s;
      } else {
        sessions.unshift(s);
      }
      renderList();
    } catch (err) {
      status(L("会话保存失败: ", "Failed to save session: ") + err, "error");
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
    // 引擎事件流 → 进行中的助手气泡的**执行轨迹**（一行一条；思考与调用交错但保持时序，
    // 拆成两段会把"什么时候想到、什么时候做"的真实顺序打乱）
    try {
      const listen = window.__TAURI__?.event?.listen;
      if (listen) {
        listen("agent://stage", (ev) => {
          const p = ev.payload ?? {};
          const detail = String(p.detail ?? "").trim();
          pushTrace("think", detail || [p.stage, p.status].filter(Boolean).join(" "));
        });
        listen("agent://log", (ev) => {
          const p = ev.payload ?? {};
          const line = traceFromLog(p.level, p.msg);
          if (line) pushTrace(line.kind, line.text);
        });
        // 规划完成的瞬间 → 大纲区出现任务列表；随后 step 事件推进三态
        listen("agent://plan", (ev) => {
          if (runSession) setPlanSteps(runSession, ev.payload);
          const steps = ev.payload?.steps ?? [];
          if (steps.length) {
            const names = steps.map((x) => x.title || "").join(" → ");
            pushTrace("think", L(`列了 ${steps.length} 步计划：${names}`,
              `planned ${steps.length} step(s): ${names}`));
          }
        });
        listen("agent://step", (ev) => {
          if (runSession) applyStepEvent(runSession, ev.payload ?? {});
        });
        // v0.3 质量门禁：验证 / 复核结论实时补进进行中的气泡（不用等 run 结束）
        listen("agent://verify", (ev) => appendGate(ev.payload ?? {}, "verify"));
        listen("agent://reflect", (ev) => appendGate(ev.payload ?? {}, "reflect"));
        // 提问（v0.8）：模型问需求歧义 → 会话里弹问题卡，等你答完它继续做
        listen("agent://ask", (ev) => showAskCard(ev.payload ?? {}));
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
    syncForProject(true);
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
    attach, handleCommand, ensureChatEl, projectClosed, refreshList, syncForProject,
    newSession, openSession, sendMessage, renderOutline,
  };
})();
