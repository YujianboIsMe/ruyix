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
  // 截图附件（v1.3 多模态）：粘贴 / 选图 / 拖拽 → 缩图 → 随消息发出去
  //
  // 分工：**浏览器缩图**（canvas 是这台机器上现成的图像编解码器，宿主与引擎都不为此引依赖）；
  // 宿主只管闸与落盘（`agent::attachments`）；引擎只管按协议把字节摆对位置。
  // 图**不进会话文件**：会话里存的是宿主回执的 `shots`（路径 + 元数据），字节在
  // `<便携根>/projects/<项目 key>/shots/`，重开会话按路径回读（`hydrateShots`）。
  // ============================================

  /** 长边上限（与宿主侧口径一致）；小图**原样发** —— 不重编码就没有画质损失 */
  const SHOT_MAX_EDGE = 1600;
  /** 超过这个字节数也重编码（原图能不动就不动，但 2 MB 以上的往返会明显拖慢） */
  const SHOT_SOFT_BYTES = 2 * 1024 * 1024;
  /** 一条消息最多几张 —— 与宿主 `attachments::MAX_PER_MESSAGE` 同值（前端先拦，省一次往返） */
  const MAX_SHOTS = 6;

  /** 路径 → data URL：重开会话时按路径回读，一张只读一次 */
  const shotCache = new Map();

  function dataUrlBytes(url) {
    const i = String(url || "").indexOf(",");
    const b64 = i < 0 ? "" : String(url).slice(i + 1);
    return Math.floor((b64.length * 3) / 4);
  }

  function fileToDataUrl(file) {
    return new Promise((res, rej) => {
      const r = new FileReader();
      r.onload = () => res(String(r.result || ""));
      r.onerror = () => rej(new Error(L("这个文件读不出来", "this file cannot be read")));
      r.readAsDataURL(file);
    });
  }

  function loadImageEl(url) {
    return new Promise((res, rej) => {
      const im = new Image();
      im.onload = () => res(im);
      im.onerror = () => rej(new Error(L("这不是一张能解的图", "not a decodable image")));
      im.src = url;
    });
  }

  /**
   * 一张用户给的图 → `{name, mime, data_base64, _preview}`（发给宿主的那份 + 本地预览）。
   *
   * `mime` **从编码回执的头里取**，不从 `file.type` 取：浏览器拿不准原类型时会退回 png，
   * 而"字节实际是什么"才是事实（宿主那边按魔数再嗅一次 —— 两边都不信声明）。
   */
  async function toAttachment(file) {
    const url0 = await fileToDataUrl(file);
    if (!/^data:image\//.test(url0)) {
      throw new Error(L(`不是图片：${file.type || file.name}`, `not an image: ${file.type || file.name}`));
    }
    const im = await loadImageEl(url0);
    const edge = Math.max(im.naturalWidth || 0, im.naturalHeight || 0);
    let url = url0;
    if (edge > SHOT_MAX_EDGE || dataUrlBytes(url0) > SHOT_SOFT_BYTES) {
      const scale = Math.min(1, SHOT_MAX_EDGE / (edge || 1));
      const w = Math.max(1, Math.round((im.naturalWidth || 1) * scale));
      const h = Math.max(1, Math.round((im.naturalHeight || 1) * scale));
      const cv = document.createElement("canvas");
      cv.width = w;
      cv.height = h;
      const cx = cv.getContext("2d");
      if (!cx) throw new Error(L("画不出缩放用的画布", "no canvas context available"));
      cx.drawImage(im, 0, 0, w, h);
      // 编码格式跟原图走（截图多是 PNG，照片多是 JPEG）；`toDataURL` 的回执头才是真实类型
      const want =
        file.type === "image/jpeg" ? "image/jpeg"
          : file.type === "image/webp" ? "image/webp"
            : "image/png";
      url = cv.toDataURL(want, 0.92);
    }
    const mime = (url.match(/^data:([^;,]+)/) || [])[1] || file.type || "image/png";
    return {
      name: file.name || "shot.png",
      mime,
      data_base64: url.slice(url.indexOf(",") + 1),
      _preview: url,
    };
  }

  /** 取走待发列表并清空（发送路径只有这一条，免得两处各自清自己的） */
  function takePending(s) {
    const list = pendingOf(s);
    s._pending = [];
    return list;
  }

  /** 这条会话待发的图（`s._pending`；**不进会话文件** —— 发出去之后宿主的回执才是事实） */
  function pendingOf(s) {
    if (!Array.isArray(s._pending)) s._pending = [];
    return s._pending;
  }

  /** 待发条的 HTML：张数 + 体积 + 每张一个 ✕ */
  function pendingHtml(s) {
    const list = pendingOf(s);
    if (!list.length) return "";
    const mb = list.reduce((n, a) => n + dataUrlBytes(a._preview), 0) / 1024 / 1024;
    const head = L(
      `待发 ${list.length} 张 · ${mb.toFixed(1)} MB`,
      `${list.length} attached · ${mb.toFixed(1)} MB`
    );
    const items = list
      .map((a, i) =>
        `<span class="session-shot">` +
        `<img class="session-shot-thumb" src="${esc(a._preview)}" alt="${esc(a.name)}" title="${esc(a.name)}">` +
        `<button class="session-shot-x" data-shot-drop="${i}" title="${L("移除", "remove")}">✕</button>` +
        `</span>`
      )
      .join("");
    return `<div class="session-pending-head">${esc(head)}</div>` +
      `<div class="session-pending-list">${items}</div>`;
  }

  /** 重画待发条（只在附件变化时调） */
  function renderPending(wrap, s) {
    const el = wrap.querySelector("[data-pending]");
    if (!el) return;
    const html = pendingHtml(s);
    el.innerHTML = html;
    el.hidden = !html;
  }

  /**
   * 当前模型能不能读图（宿主 `ai_vendor` 的 caps —— 与那颗 🌏 用的是同一份能力事实）。
   * 拿不到能力就按"不能"处理并说清：宁可发不出去（有原因），也不要发出去之后被引擎拒。
   */
  function visionGate(wrap) {
    const caps = wrap._vendor && wrap._vendor.caps;
    if (!caps) {
      return { ok: false, why: L("还不知道当前模型能不能读图（没取到模型能力）", "the model's vision capability is unknown") };
    }
    if (!caps.multimodal) {
      const m = caps.model || L("当前模型", "the current model");
      return {
        ok: false,
        why: L(
          `当前模型 ${m} 读不了图 —— 换成能读图的模型（如 deepseek-flash），或者用文字描述`,
          `model ${m} cannot read images — switch to a vision model (e.g. deepseek-flash) or describe it in words`
        ),
      };
    }
    return { ok: true, why: "" };
  }

  /**
   * 一条消息要显示的截图：**宿主回执优先，回执还没到就用内存里的预览**。
   *
   * 为什么必须有「回执还没到」这一支：`attachments`（路径 + 元数据）是 run **跑完**才回填的，
   * 而图在按下发送那一刻就已经属于这条消息了 —— 跑着的时候、以及这一轮失败（没有回执）时，
   * 气泡里都得看得见。少了它，用户看到的就是「图明明发出去了，历史里却找不到」。
   */
  function shotItems(m) {
    const stored = m.attachments ?? [];
    if (stored.length) {
      return stored.map((a, i) => ({
        src: (m._preview && m._preview[i]) || shotCache.get(a.path) || "",
        path: a.path,
        name: a.name || L("截图", "screenshot"),
        bytes: a.bytes || 0,
      }));
    }
    return (m._preview ?? []).map((url, i) => ({
      src: url,
      path: "",
      name: L(`截图 ${i + 1}`, `screenshot ${i + 1}`),
      bytes: 0,
    }));
  }

  /** 一条消息随行的截图（缩略图）。没有 src 的先留占位，交给 `hydrateShots` 读盘补上。 */
  function shotStripHtml(m) {
    const list = shotItems(m);
    if (!list.length) return "";
    const items = list
      .map((a) => {
        const kb = a.bytes ? ` · ${Math.round(a.bytes / 1024)} KB` : "";
        const src = a.src ? `src="${esc(a.src)}"` : `data-shot-src="${esc(a.path)}"`;
        return `<img class="session-shot-thumb session-shot-thumb--msg" ${src}` +
          ` data-shot="${esc(a.path)}" alt="${esc(a.name)}" title="${esc(a.name)}${kb}">`;
      })
      .join("");
    return `<div class="session-shots">${items}</div>`;
  }

  /** 把还没有 src 的缩略图按路径补上（读一次、缓存住）—— 重开会话的图就靠它 */
  async function hydrateShots(scope) {
    const invoke = getInvoke();
    const imgs = Array.from(scope.querySelectorAll("img[data-shot-src]"));
    if (!imgs.length || !invoke) return;
    for (const img of imgs) {
      const path = img.dataset.shotSrc;
      try {
        let url = shotCache.get(path);
        if (!url) {
          const f = await invoke("read_file_base64", { path });
          url = "data:" + (f.mime || "image/png") + ";base64," + f.base64;
          shotCache.set(path, url);
        }
        img.src = url;
        delete img.dataset.shotSrc;
      } catch (e) {
        // 图被删了就照实说（不静默留一个破图框），并说清是哪一张
        img.classList.add("session-shot-thumb--missing");
        img.alt = L("图不在了", "image missing");
        img.title = String(e);
        delete img.dataset.shotSrc;
      }
    }
  }

  /**
   * 点缩略图看大图。**自己家的浮层**，不是新窗口/新标签：整个 IDE 活在一个文档里
   * （标签栏、文件树、会话都只是 DOM 状态），导航出去就回不来 —— 见 nav 闸门那条 P0。
   */
  function showShotLarge(src, title) {
    if (!src) return;
    const box = document.createElement("div");
    box.className = "shot-lightbox";
    box.innerHTML = `<img src="${esc(src)}" alt="${esc(title || "")}">` +
      `<div class="shot-lightbox-hint">${esc(title || "")} ${L("（点击任意处关闭）", "(click anywhere to close)")}</div>`;
    const close = () => {
      box.remove();
      document.removeEventListener("keydown", onKey);
    };
    const onKey = (e) => {
      if (e.key === "Escape") close();
    };
    box.addEventListener("click", close);
    document.addEventListener("keydown", onKey);
    document.body.appendChild(box);
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
      `<div class="session-pending" data-pending hidden></div>` +
      `<div class="session-input-row">` +
      `<textarea class="session-input" rows="2" placeholder="${L("交代工作，Ctrl+Enter 发送", "Describe the work — Ctrl+Enter to send")}"></textarea>` +
      `<div class="session-toolbar">` +
      `<select class="session-mode-select" data-mode>` +
      `<option value="confirm" selected title="${L("运行后自动打开差异确认框，人工勾选写入", "Open a diff panel after each run; you confirm what to write")}">🤝 ${L("确认模式", "Confirm")}</option>` +
      `<option value="write" title="${L("Agent 改动直接写入项目（覆盖前备份到 .ruyix/backups）", "Agent writes land directly (backed up to .ruyix/backups first)")}">✍️ ${L("写入模式", "Write")}</option>` +
      `<option value="auto" title="${L("同写入模式：改动直接落盘并自行动验证", "Same as write mode: changes land directly and the agent verifies itself")}">🚀 ${L("自主模式", "Autonomous")}</option>` +
      `</select>` +
      `<select class="session-model-select" data-model></select>` +
      `<button class="agent-btn session-attach" data-attach title="${L("附件：截图（选图 / 粘贴 / 直接拖进来）", "Attach screenshots (pick / paste / drop)")}">📎</button>` +
      `<input type="file" class="session-file" data-file accept="image/*" multiple hidden>` +
      `<button class="agent-btn session-web" data-web title="${L("服务端联网检索（按模型能力决定可用与否）", "Server-side web search (availability depends on the model)")}">🌏</button>` +
      `<button class="agent-btn agent-btn--run session-run" data-run title="${L("发送（Ctrl+Enter）", "Send (Ctrl+Enter)")}">▶</button>` +
      `</div></div>`;
    fillMsgs(wrap, s);

    const input = wrap.querySelector(".session-input");
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        // Ctrl+Enter = 发送（**不是**那颗按钮的 click）：运行中那颗按钮是"停止"，
        // 手快连按 Ctrl+Enter 不该把正在跑的任务停掉。
        const text = input.value.trim();
        const shots = pendingOf(s);
        // 只有图没有字也算一条消息（引擎要求非空任务，`sendMessage` 会给一句中性说明）
        if ((!text && !shots.length) || busy) return;
        input.value = "";
        sendMessage(s, wrap, text, takePending(s));
      }
    });
    // ---- 截图入口（v1.3）：📎 选图 / 粘贴 / 拖拽 ----
    // 三个入口共用一个落点（`addFiles`）：一份代码、一份闸、一份报错文案。
    const attachBtn = wrap.querySelector("[data-attach]");
    const fileInput = wrap.querySelector("[data-file]");
    /** 能力未知/不支持时禁用 📎 并说明原因（不给一个按下去没反应的按钮） */
    function paintAttach() {
      const g = visionGate(wrap);
      attachBtn.disabled = !g.ok;
      attachBtn.title = g.ok
        ? L(`附件：截图（选图 / 粘贴 / 拖进来）—— 最多 ${MAX_SHOTS} 张`, `Attach screenshots (pick / paste / drop) — up to ${MAX_SHOTS}`)
        : g.why;
    }
    async function addFiles(files) {
      const list = Array.from(files || []);
      if (!list.length) return;
      const g = visionGate(wrap);
      if (!g.ok) return void status(g.why, "error");
      const cur = pendingOf(s);
      if (cur.length + list.length > MAX_SHOTS) {
        return void status(L(`一条消息最多 ${MAX_SHOTS} 张图`, `at most ${MAX_SHOTS} images per message`), "error");
      }
      for (const f of list) {
        try {
          cur.push(await toAttachment(f));
        } catch (e) {
          status(String((e && e.message) || e), "error");
        }
      }
      renderPending(wrap, s);
      input.focus();
    }
    attachBtn.addEventListener("click", () => fileInput.click());
    fileInput.addEventListener("change", () => {
      addFiles(fileInput.files);
      fileInput.value = ""; // 同一个文件连选两次也要有反应
    });
    // 粘贴：Win+Shift+S / 微信 / 截图工具的图都落在这里；纯文字粘贴照旧不管
    input.addEventListener("paste", (e) => {
      const files = Array.from((e.clipboardData && e.clipboardData.files) || []).filter((f) =>
        String(f.type).startsWith("image/")
      );
      if (!files.length) return;
      e.preventDefault();
      addFiles(files);
    });
    // 拖拽：接整块会话区（拖到输入框外面也算"发给这个会话"）
    wrap.addEventListener("dragover", (e) => {
      if (!Array.from((e.dataTransfer && e.dataTransfer.types) || []).includes("Files")) return;
      e.preventDefault();
      wrap.classList.add("session-chat--drop");
    });
    wrap.addEventListener("dragleave", () => wrap.classList.remove("session-chat--drop"));
    wrap.addEventListener("drop", (e) => {
      const files = Array.from((e.dataTransfer && e.dataTransfer.files) || []).filter((f) =>
        String(f.type).startsWith("image/")
      );
      if (!files.length) return;
      e.preventDefault();
      wrap.classList.remove("session-chat--drop");
      addFiles(files);
    });
    renderPending(wrap, s);
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
    // 模型厂商对象（宿主 `ai_vendor` 的回执）：厂商身份 + 模型清单 + 当前模型能力。
    // **只由 reloadVendor 写** —— 它是这份对象的唯一来源（见下面 reloadVendor 的注释）。
    wrap._vendor = null;
    const paintWeb = () => {
      const caps = wrap._vendor && wrap._vendor.caps;
      if (!caps) {
        webBtn.disabled = true;
        webBtn.title = L("联网能力未知（未取到模型能力）", "Web capability unknown");
        return;
      }
      if (!caps.web_search) {
        // 联网是"（协议 × 模型）"的能力：同一个模型换条协议可能就能搜（实测 flash 就是）。
        // 所以文案要把**协议**写出来，否则用户看到的是一个无法解释的灰按钮。
        const proto = caps.api_format || "openai";
        webBtn.disabled = true;
        webBtn.title = L(
          `当前模型 ${caps.model} 在 ${proto} 协议下不支持服务端联网检索（换模型或换协议）`,
          `model ${caps.model} has no server-side web search on the ${proto} protocol`
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
      // 开关状态是本地的（`s._webSearch`）—— 只重画这颗按钮，**不重取厂商对象**：
      // 联网开关与"这是哪家厂商"无关，重取只会把用户刚选的模型按配置里的默认画回去。
      paintWeb();
    });
    // ---- 模型下拉框（需求：探测厂商模型列表，在会话界面就能换模型）----
    //
    // 数据源是**厂商的** `GET /models`（宿主 `ai_vendor` 里那份厂商对象，与配置表单同源）——
    // 前端不维护模型名单，厂商加一个模型这里就多一项（实测 DeepSeek 今天只有
    // `deepseek-flash` / `deepseek-v4-pro` 两个，而能力表里还留着两个已退役的旧名）。
    // 每一项的 title 写清能力（联网 / 读图 / 录音 / 上下文），用户不必去配置面板猜。
    const modelSel = wrap.querySelector("[data-model]");
    const paintModel = () => {
      const cur = (wrap._vendor && wrap._vendor.model) || "";
      const list = wrap._vendor && wrap._vendor.models;
      if (!list || !list.length) {
        // 拿不到厂商列表**不许编**：只留当前模型一项并说清为什么换不了
        modelSel.innerHTML = `<option>${esc(cur || L("（模型未知）", "(model unknown)"))}</option>`;
        modelSel.disabled = true;
        // 宿主把"为什么拿不到"放在 models_error 里 —— 原样显示，别让用户猜
        const why = wrap._vendor && wrap._vendor.models_error;
        const base = L(
          "拿不到厂商模型列表（没配 Key / 网络不通 / 该端点没有 /models）—— 默认模型在配置面板里改",
          "Vendor model list unavailable (no key / offline / no /models endpoint) — set the default in the config panel"
        );
        modelSel.title = why ? L(`${base}：${why}`, `${base}: ${why}`) : base;
        return;
      }
      const capText = (m) =>
        [m.web_search ? L("联网", "web") : null, m.multimodal ? L("读图", "vision") : null,
         m.audio ? L("录音", "audio") : null].filter(Boolean).join(" · ") || L("纯文本", "text only");
      const label = (m) => {
        const ctx = m.context_window ? ` · ${Math.round(m.context_window / 1024)}K` : "";
        return `${m.name || m.id}（${m.id}）— ${capText(m)}${ctx}`;
      };
      let opts = list
        .map((m) => `<option value="${esc(m.id)}" title="${esc(label(m))}">${esc(m.name || m.id)}</option>`)
        .join("");
      // 当前模型不在厂商列表里（配置里留着一个退役旧名）也要看得见：补一项并标明，
      // 否则 select 会静默跳到列表第一项，用户以为模型被换了。
      if (cur && !list.some((m) => m.id === cur)) {
        opts = `<option value="${esc(cur)}" title="${esc(cur + L("（不在厂商列表里）", " (not in the vendor list)"))}">` +
          `${esc(cur)}${L("（不在厂商列表）", " (not listed)")}</option>` + opts;
      }
      modelSel.innerHTML = opts;
      modelSel.disabled = false;
      modelSel.value = cur || list[0].id;
      modelSel.title = L(
        "本次会话使用的模型（默认模型在配置面板里改；此项只影响本次运行）",
        "Model for this session (change the default in the config panel)"
      );
    };

    // 厂商对象的**一次完整重取**：身份 + 模型清单 + 当前模型 + 能力，一发回来（宿主 `ai_vendor`）。
    //
    // 触发源只有三个：面板首次打开、用户换模型、宿主广播"厂商变了"（`model://vendor-changed`，
    // 端点 / 密钥 / 协议之一变化）。**不再跟着任意配置变化重取** —— 那正是这轮 bug 的根：
    // 拧一下 🌏（写 harness.llm.web_search）也会触发重取，拿配置里的默认模型把用户刚选的画回去
    // （用户实测：toggle 联网就跳回 pro）。
    //
    // 起个名字而不是匿名 IIFE 是上一轮的教训：这里曾写着 `await refreshCaps()`，而 `refreshCaps`
    // 在 ui/ 里**从未定义** ⇒ ReferenceError ⇒ 静默 reject ⇒ 列表取到了却没人重画（下拉永远
    // 停在"（模型未知）"）。现在：拿完就重画、失败可见、首次拿不到只重试一次。
    let retried = false;
    const reloadVendor = async () => {
      const invoke = getInvoke();
      if (!invoke) return;
      try {
        wrap._vendor = await invoke("ai_vendor", { projectRoot: root() });
      } catch (e) {
        wrap._vendor = null;
        status(L(`取模型厂商失败：${e}`, `failed to load the model vendor: ${e}`), "error");
      }
      paintModel();
      paintWeb();
      paintAttach();
      // 首次就没取到清单 ⇒ 1.5 秒后重试**一次**：应用刚起来时后端可能尚未就绪，
      // 一次失败不该等于永久「模型未知」。
      if ((!wrap._vendor || !(wrap._vendor.models || []).length) && !retried) {
        retried = true;
        setTimeout(() => { reloadVendor(); }, 1500);
      }
    };
    reloadVendor();
    // 登记给 `model://vendor-changed` 用（见 attach 里的监听）：厂商真的换了才重取。
    vendorDependent.push({ el: wrap, fn: reloadVendor });
    modelSel.addEventListener("change", async () => {
      const id = modelSel.value;
      const invoke = getInvoke();
      if (!invoke || !id) return;
      try {
        // runtime 作用域：只影响本次运行，不落盘改掉用户的默认模型
        await invoke("config_form_apply", {
          scope: "runtime",
          projectRoot: root(),
          // 模型名**单一来源是 `ai.*`**（宿主 config_bridge 读 ai.model 再映射给引擎的
          // llm.model）。键名只能是 `model`：宿主的完整键 = `ruyix.code.<section>.<key>`，
          // 把键名写成 `ai.model`（section 已经是 ai 了）会拼出 `ruyix.code.ai.ai.model`
          // 这个**没人读**的死键 ——
          // 于是"切到 flash"既不进引擎（引擎仍用 pro），也会被下一次重取按配置里的默认弹回去。
          entries: [{ section: "ai", key: "model", value: id }],
        });
      } catch (e) {
        status(String(e), "error");
      }
      // 换模型要**重新问一次能力**（联网/录音的可用性跟着模型走）—— 重取整个厂商对象最省心，
      // 且 `ai_vendor` 里的"当前模型"就是从配置解析出来的那个，刚写进去的这次选择会生效。
      await reloadVendor();
    });
    paintModel();
    paintWeb();
    paintAttach();

    /** 运行中按同一个按钮 = 停止：取消位 + 一句"已取消"留痕（原 data-cancel 的行为一字未改） */
    async function stopRun() {
      const invoke = getInvoke();
      if (invoke) invoke("agent_cancel").catch(() => {});
      setBusy(false);
      s.messages.push({
        role: "system", text: L("已取消", "canceled"), ts: nowHms(),
        run_id: null, status: "canceled",
      });
      fillMsgs(wrap, s);
      await persist(s);
    }
    wrap.querySelector("[data-run]").addEventListener("click", () => {
      // 同一个按钮两件事：运行中 = 停止；空闲 = 发送。判据只有这一个 `busy`。
      if (busy) return void stopRun();
      const text = input.value.trim();
      const shots = pendingOf(s);
      if (!text && !shots.length) return;
      input.value = "";
      sendMessage(s, wrap, text, takePending(s));
    });
    // 提问卡（v0.8 ask_user）：卡片按钮/输入框由 askHtml 在每次重渲染时重建，
    // 所以走**事件委托**（绑在容器上），而不是逐个按钮 addEventListener。
    wrap.addEventListener("click", (e) => {
      // 缩略图：点开看大图（自家浮层，不开新窗口）
      const thumb = e.target.closest("img.session-shot-thumb");
      if (thumb && thumb.src) return void showShotLarge(thumb.src, thumb.alt);
      // 待发条上的 ✕：把这张从这次发送里去掉
      const drop = e.target.closest("[data-shot-drop]");
      if (drop) {
        pendingOf(s).splice(Number(drop.dataset.shotDrop), 1);
        renderPending(wrap, s);
        return;
      }
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
    // 截图缩略图：有 src 的当场画出来，没有的（重开会话）按路径读盘补上 —— 异步，不挡渲染
    hydrateShots(el).catch(() => {});
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
   * 进展记忆小节（v1.1 findings 账本，§8-5）。
   *
   * 模型在跑的过程中用 `record_findings` 记下的**结论 + 证据**（"它凭什么这么改"）。此前这份
   * 账本只活在 run 里、跑完即焚（交付记录里那条"拍了「是」却一直没落"）—— 用户既看不到，
   * 重开会话更无从复核，所以补这一节。
   *
   * **被取代的条目照旧显示**（划掉记号 + 指向取代它的那条）：findings 的语义是"不改历史"，
   * 界面不该比账本更干净 —— 那会让人以为模型从没改过主意。样式复用验证/复核那套类名，
   * 不新增 CSS（视觉沿用既有的一节）。
   */
  function findingsHtml(m) {
    const fs = m.findings ?? [];
    if (!fs.length) return "";
    const rows = fs.map((f) => {
      const gone = !!f.superseded_by;
      return (
        `<div class="session-gate-item">` +
        `<span class="session-gate-tag">${gone ? "↩" : "•"} ${esc(f.id ?? "")}</span> ` +
        `${esc(f.claim ?? "")}` +
        (f.evidence ? `<span class="session-gate-ev">${esc(f.evidence)}</span>` : "") +
        (gone ? ` <span class="session-gate-ev">→ ${esc(f.superseded_by)}</span>` : "") +
        (f.note ? `<div class="session-gate-item">${esc(f.note)}</div>` : "") +
        `</div>`);
    });
    return (
      `<div class="session-gate">` +
      `<div class="session-gate-row">` +
      `<span class="session-gate-tag">📌 ${L("结论", "Findings")}</span>` +
      `<span class="session-gate-text">${L("模型记下的依据（被取代的划线保留）", "what it established (superseded kept, struck through)")}</span>` +
      `</div>` +
      rows.join("") +
      `</div>`);
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

  // Agent 输出是 markdown：markdown-it 渲染（vendor 自 ui/packages/markdown-it.min.js，
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
    // 用户消息的截图贴在气泡**上面**（先看现场再看话）
    const shots = mine ? shotStripHtml(m) : "";
    const bubble = mine
      ? `<div class="session-bubble">${esc(m.text)}</div>`
      : `<div class="session-bubble session-bubble--md">${mdHtml(m.text)}</div>`;
    // 有轨迹的消息放宽一点宽度：轨迹是日志，路径/命令比对话正文长，78% 会一路省略号
    const wide = !mine && (live || (m.trace ?? []).length) ? " session-msg--trace" : "";
    // 顺序（用户报过 bug 的地方，别改回去）：**过程在上、结论在下** ——
    //   ① 轨迹（思考 · 执行，工具循环的实时输出）
    //   ② 提问卡（跑到一半问你的那条，紧跟过程）
    //   ③ 最终回复（这次交付的正文）
    //   ④ 验证 / 复核（针对上面那条回复的结论，贴着它）
    //   ⑤ meta（run id / 状态）
    // 原先 bubble 在最前面 ⇒ 任务跑完后轨迹追加到气泡**下面**，读起来像"先给结论再做事"。
    return `<div class="session-msg ${mine ? "session-msg--user" : "session-msg--agent"}${wide}">` +
      (mine ? shots + bubble : traceHtml(m, live) + askHtml(m) + findingsHtml(m) + gateHtml(m) + bubble) +
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

  /**
   * 发送/停止是**同一个按钮**（用户要求）：空闲时 ▶ = 发送，运行中 ■ = 停止。
   *
   * 为什么不摆两个按钮互相禁用（旧样子）：一个永远灰着的按钮既占位又要解释自己为什么灰，
   * 而"运行中"本来就是一个状态而不是一个功能 —— 状态该改外观，不该换按钮。
   * 按钮**任何时刻都可点**（灰按钮 = 按下去没反应的按钮，本项目不许有）：
   * 点下去干什么由 `busy` 决定，键盘 Ctrl+Enter 只走发送（防手快连发）。
   */
  function setBusy(b) {
    busy = b;
    document.querySelectorAll(".session-run").forEach((btn) => {
      btn.classList.toggle("session-run--busy", b);
      btn.textContent = b ? "■" : "▶";
      btn.title = b
        ? L("停止当前任务", "Stop the running task")
        : L("发送（Ctrl+Enter）", "Send (Ctrl+Enter)");
    });
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

  async function sendMessage(s, wrap, text, shots) {
    const withShots = shots ?? [];
    // 只有图没有字：引擎要求非空任务（空消息会被当面拒），给一句中性的说明当任务
    const task = text || (withShots.length ? L("看一下我发的截图", "look at this screenshot") : "");
    s.title = s.title || truncate(task, 24);
    // 对话历史（不含本条）：模型解析指代（"它/这个报错"指上一轮）靠它
    const history = s.messages
      .filter((m) => (m.role === "user" || m.role === "assistant") && m.text)
      .slice(-12)
      // `run_id` 是**跨 run 检索的钥匙**（v1.5 会话层）：宿主把它交给引擎，
      // 引擎才知道"前面几个 run 的留痕是哪几份"。少了它，甸服永远只搜得到本 run。
      .map((m) => ({ role: m.role, text: m.text, run_id: m.run_id ?? null }));
    // `attachments` 先空着，等宿主回执（落盘后的路径）再回填；
    // `_preview` 是内存里的 data URL —— `persist` 会把 `_` 开头的字段剥掉，不进会话文件
    const userMsg = {
      role: "user",
      text: task,
      ts: nowHms(),
      run_id: null,
      status: null,
      attachments: [],
      _preview: withShots.map((p) => p._preview),
    };
    s.messages.push(userMsg);
    // 待发条**随发送清空**：图从「输入区」移到「历史里那条消息」上。
    // 用户报的就是这里 —— 发完之后输入区还挂着那张图（看着像没发出去），历史里又找不到它。
    s._pending = [];
    renderPending(wrap, s);
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
      rep = await invoke("agent_reply", { task: task, history, mode, projectRoot: root(), sessionId: (s && s.id) || null, attachments: withShots.map((p) => ({ name: p.name, mime: p.mime, data_base64: p.data_base64 })) });
      placeholder.text = rep.answer || L("（空回复）", "(empty reply)");
      // 宿主回执：图落在 <便携根>/projects/<键>/shots/，路径进会话、预览进内存缓存
      userMsg.attachments = rep.shots ?? [];
      withShots.forEach((p, i) => {
        const shot = userMsg.attachments[i];
        if (shot) shotCache.set(shot.path, p._preview);
      });
      // 验证 / 复核结论挂在这条助手消息上（持久化后重开也能看到"这轮验过没有"）
      placeholder.verify = rep.verifications ?? [];
      placeholder.reflect = rep.reflections ?? [];
      // 结论账本（v1.1 §8-5）：跑完跟消息走 —— 字段没在 SessionMsg 里声明的话会被
      // serde 静默抹掉（磁盘与内存一起丢），宿主侧已同步声明
      placeholder.findings = rep.findings ?? [];
      // 本 run 的留痕 id：存进消息（`SessionMsg.run_id` 已声明），下一个 run 的 history 才带得上
      if (rep.run_id) placeholder.run_id = rep.run_id;
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
      // `_` 开头的消息字段是**临时**的（`_preview` = 截图的 data URL，一张几 MB）——
      // 原样 stringify 会把几 MB 的 base64 写进会话文件。落盘只带真字段。
      const onDisk = {
        ...s,
        messages: s.messages.map((m) => {
          const keep = {};
          for (const [k, v] of Object.entries(m)) if (!k.startsWith("_")) keep[k] = v;
          return keep;
        }),
      };
      const saved = await invoke("agent_session_save", {
        sessionJson: JSON.stringify(onDisk),
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

  /** 清空**当前**会话的结论视图（`agent findings clear`）。 */
  async function clearFindings() {
    const tab = (state?.tabs ?? []).find((x) => x._isSession && x.id === state.activeTabId);
    const s = tab?._session;
    if (!s) {
      status(L("没有打开的会话 —— 先开一个会话再清", "No session open — open one first"), "error");
      return;
    }
    let n = 0;
    for (const m of s.messages) {
      if (Array.isArray(m.findings)) n += m.findings.length;
      m.findings = [];
    }
    if (tab._sessionEl) fillMsgs(tab._sessionEl, s);
    await persist(s);
    status(
      L(`已清空本会话的结论视图（${n} 条）—— 落盘那份没动`,
        `Cleared this session's findings view (${n}) — on-disk copies untouched`),
    );
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
    // `agent findings clear`：清空**本会话的结论视图**（v1.1 §8-3 当年拍了「给」，一直没实现）。
    // 只清内存与存档里那份 active 视图 —— **不动落盘文件**（与 findings「不物理删除」一致），
    // 所以这条命令**不许**调任何后端删除/清理命令，唯一的后端调用是会话保存。
    if (/^findings\s+clear$/i.test(arg)) {
      await clearFindings();
      return;
    }
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
        // 配置改了（`config://changed`）：只重探**环境状态**（那排 chip 写着 key 配没配）。
        //
        // 触发场景就是用户报的那一条：在配置里填完 API Key → 会话面板那排 chip 还写着
        // 「✗ key 未配置」—— 面板只在打开时探过一次，之后没人告诉它 key 变了。
        // **模型/厂商不在这里刷新**：那是下面 `model://vendor-changed` 的事。
        listen("config://changed", (ev) => {
          const keys = (ev.payload ?? {}).keys || [];
          if (!touchesEnvState(keys)) return; // 改别的键不值得重探
          probeEnv();
        });
        // 厂商变了（`model://vendor-changed`）：端点 / 密钥 / 协议之一变化，宿主在 runtime 里
        // 刷新了厂商对象之后才广播。**只有这一件事能让会话面板重取模型清单与能力** ——
        // 于是"拧一下 🌏（写 harness.llm.web_search）就把选中的模型弹回 pro"这条回环从根上没了。
        listen("model://vendor-changed", () => {
          for (const it of aliveVendorDependent()) {
            Promise.resolve()
              .then(() => it.fn())
              .catch(() => {});
          }
        });
      }
    } catch {
      // 浏览器模式无 Tauri 事件
    }
    // 环境探针（沿用原 agent 面板的能力）—— 起个名字，好让 `config://changed` 也能叫它
    probeEnv();
    syncForProject(true);
  }

  /**
   * 厂商变了之后要**重取**的东西（会话面板里"取决于当前厂商"的状态：模型清单 / 能力）。
   *
   * 为什么需要这张表：宿主只知道"厂商变了"，不知道谁在用。每个会话标签登记自己的
   * `reloadVendor`，标签关掉后由 `aliveVendorDependent` 顺手剔除。
   */
  const vendorDependent = [];

  /** 环境探针（那排 chip）：key 配没配就写在里面 */
  function probeEnv() {
    const invoke = getInvoke();
    if (!invoke) return renderEnvChips(null);
    invoke("agent_env_probe", { projectRoot: null })
      .then((env) => renderEnvChips(env))
      .catch(() => {});
  }

  /**
   * `config://changed` 的载荷里，哪些键会改变**环境状态**（那排 chip）？
   *
   * 只认 `ai.*` / `ai_fallback.*`（端点与密钥）—— 它们才决定"key 配没配"。
   * `harness.llm.*` 不再算：模型清单与能力走 `model://vendor-changed`，与这里无关。
   * 只认相关的：改个 `ui.lang`、拧一下 🌏 都不值得把那排 chip 全重探一遍。
   */
  function touchesEnvState(keys) {
    return (keys || []).some((k) => k && (k.section === "ai" || k.section === "ai_fallback"));
  }

  /** 还活着的刷新登记项（标签页关掉后 DOM 就不在文档里了，顺手剔除，免得越攒越多） */
  function aliveVendorDependent() {
    const alive = (el) =>
      typeof document?.contains !== "function" || !el || document.contains(el);
    for (let i = vendorDependent.length - 1; i >= 0; i -= 1) {
      if (!alive(vendorDependent[i].el)) vendorDependent.splice(i, 1);
    }
    return vendorDependent;
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
  // 探针用：复刻「发送前待发条里有图」的现场（见 scripts/session-trace-layout.js 判据 8）
  renderPending,
    attach, handleCommand, ensureChatEl, projectClosed, refreshList, syncForProject,
    newSession, openSession, sendMessage, renderOutline,
  };
})();
