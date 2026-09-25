/**
 * ruyix — 项目记忆面板：顶部菜单「记忆」→ 中央编辑区打开。
 *
 * ## 治的是什么病
 *
 * 记忆落地在引擎里（`crates/harness-engine/src/mem/`），但那是**文件与命令**层面的东西：
 * 用户想知道"它到底记住了什么、凭什么、什么时候变的、丢过什么"时，只能去翻 `mem.db`。
 * 而这三问恰恰是这套设计的全部意义 —— 看不见就等于没有：
 *
 * - **当前信念**：现在认为什么为真（被推翻的**不在**这里 —— 它们没被删，只是离开了当前事实）
 * - **凭什么（why）**：点一条看完整修订链（谁在什么时候、以什么依据、用什么算子改的它）
 * - **那时是什么（as-of）**：按时间点查回旧值 —— 历史一条不丢的**可操作形态**
 * - **丢过什么（收据）**：有界注入块逐出的条目必须留下"丢了什么 / 怎么换回来"
 *
 * ## 四条纪律
 *
 * 1. **面板不自己算**。`mem_now` / `mem_why` / `mem_as_of` / `mem_receipts` 全部由引擎回答，
 *    前端不重实现折叠、不猜哪条被推翻了 —— 面板和模型看的是同一份事实。
 * 2. **"被推翻"要看得见**：列表默认只给当前信念，但每条都点得开修订链，
 *    历史因此在界面上是**可抵达的**而不是"看不见即不存在"。
 * 3. **缺模型要说人话**：向量腿缺席时记忆退化为纯词法检索，这不是错误但必须说出来
 *    （`embed_reason` 直接显示，不吞掉）。
 * 4. **写入是显式的**：`记一条` 走人写入口（origin=human），与探针/引擎决策在账本里同列但来源不同。
 */

window.MemoryUI = (() => {
  "use strict";

  const T = (k, p) => (window.I18N && typeof I18N.t === "function" ? I18N.t(k, p) : k);

  let tab = null;
  let lastScope = "";

  function invoke() {
    return typeof getTauriInvoke === "function" ? getTauriInvoke() : null;
  }

  /** 当前项目根（记忆作用域靠它区分；没打开项目 = 机器级） */
  function projectRoot() {
    const p = window.state && window.state.currentProject;
    return p && p.path ? p.path : null;
  }

  function el(id) {
    return document.getElementById(id);
  }

  function esc(s) {
    return String(s == null ? "" : s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  /** 秒级时间戳 → 本地 `2026-09-25 21:30:05`（面板上只有时刻能回答"什么时候变的"） */
  function fmtTs(sec) {
    const n = Number(sec);
    if (!n) return "—";
    const d = new Date(n * 1000);
    const p2 = (x) => String(x).padStart(2, "0");
    return (
      `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())} ` +
      `${p2(d.getHours())}:${p2(d.getMinutes())}:${p2(d.getSeconds())}`
    );
  }

  /** 骨架（只建一次；之后所有更新都是填内容，不重建整块） */
  function build() {
    const root = el("memory-view");
    if (!root || root.dataset.built === "1") return root;
    root.innerHTML = `
      <div class="mem-head">
        <div class="mem-head-row">
          <span class="mem-title" data-i18n="mem.title"></span>
          <span class="mem-scope" id="mem-scope"></span>
          <span class="mem-embed" id="mem-embed"></span>
          <button class="mem-btn" id="mem-fetch" data-i18n="mem.fetchModel"></button>
          <span class="mem-spacer"></span>
          <button class="mem-btn" id="mem-add" data-i18n="mem.add"></button>
          <button class="mem-btn" id="mem-rebuild" data-i18n="mem.rebuild"></button>
        </div>
        <div class="mem-head-row">
          <input class="mem-input" id="mem-q" type="text" />
          <button class="mem-btn" id="mem-search" data-i18n="mem.search"></button>
          <span class="mem-hint" data-i18n="mem.queryHint"></span>
          <span class="mem-status" id="mem-status"></span>
        </div>
      </div>
      <div class="mem-body">
        <div class="mem-section">
          <div class="mem-section-title" data-i18n="mem.sectionNow"></div>
          <table class="mem-table mem-table--now"><thead><tr>
            <th data-i18n="mem.colKey"></th><th data-i18n="mem.colValue"></th>
            <th data-i18n="mem.colStatus"></th><th data-i18n="mem.colProv"></th>
            <th data-i18n="mem.colActions"></th>
          </tr></thead><tbody id="mem-now-body"></tbody></table>
          <div class="mem-empty" id="mem-now-empty"></div>
        </div>
        <div class="mem-section" id="mem-why-section" style="display:none">
          <div class="mem-section-title" id="mem-why-title"></div>
          <table class="mem-table mem-table--why"><thead><tr>
            <th data-i18n="mem.colSeq"></th><th data-i18n="mem.colWhen"></th>
            <th data-i18n="mem.colKind"></th><th data-i18n="mem.colOp"></th>
            <th data-i18n="mem.colValue"></th><th data-i18n="mem.colOrigin"></th>
            <th data-i18n="mem.colReason"></th>
          </tr></thead><tbody id="mem-why-body"></tbody></table>
        </div>
        <div class="mem-section">
          <div class="mem-section-title" data-i18n="mem.sectionAsOf"></div>
          <div class="mem-head-row">
            <input class="mem-input" id="mem-at" type="datetime-local" />
            <button class="mem-btn" id="mem-asof-go" data-i18n="mem.asOfGo"></button>
            <span class="mem-hint" data-i18n="mem.asOfHint"></span>
          </div>
          <table class="mem-table mem-table--asof"><thead><tr>
            <th data-i18n="mem.colKey"></th><th data-i18n="mem.colValue"></th>
            <th data-i18n="mem.colStatus"></th><th data-i18n="mem.colFrom"></th>
            <th data-i18n="mem.colTo"></th>
          </tr></thead><tbody id="mem-asof-body"></tbody></table>
        </div>
        <div class="mem-section">
          <div class="mem-section-title" data-i18n="mem.sectionReceipts"></div>
          <table class="mem-table mem-table--receipts"><thead><tr>
            <th data-i18n="mem.colWhen"></th><th data-i18n="mem.colKind"></th>
            <th data-i18n="mem.colDropped"></th><th data-i18n="mem.colRehydrate"></th>
          </tr></thead><tbody id="mem-receipts-body"></tbody></table>
          <div class="mem-empty" id="mem-receipts-empty"></div>
        </div>
      </div>`;
    root.dataset.built = "1";
    applyI18n(root);
    el("mem-q")?.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void refresh();
    });
    el("mem-search")?.addEventListener("click", () => void refresh());
    el("mem-asof-go")?.addEventListener("click", () => void refreshAsOf());
    el("mem-add")?.addEventListener("click", () => void addOne());
    el("mem-rebuild")?.addEventListener("click", () => void rebuild());
    el("mem-fetch")?.addEventListener("click", () => void fetchModel());
    // 模型下载进度（`mem://model`）：96MB 的下载必须看得见，否则用户以为卡死了
    const listen = window.__TAURI__?.event?.listen;
    if (listen) {
      listen("mem://model", (ev) => {
        const p = ev.payload ?? {};
        const line = el("mem-embed");
        if (!line) return;
        if (p.phase === "start") line.textContent = T("mem.fetchStart");
        else if (p.phase === "progress") {
          line.textContent = T("mem.fetchProgress", {
            file: p.file,
            done: (Number(p.done) / 1e6).toFixed(1),
            total: (Number(p.total) / 1e6).toFixed(1),
          });
        } else if (p.phase === "done") {
          line.textContent = T("mem.fetchDone", { n: p.fetched });
          void refresh();
        } else if (p.phase === "error") {
          line.textContent = T("mem.fetchFailed", { line: p.line || "" });
        }
        if (typeof setStatus === "function") setStatus(line.textContent, p.phase === "error" ? "error" : "info");
      }).catch(() => {});
    }
    el("mem-now-body")?.addEventListener("click", (e) => {
      const tr = e.target.closest("tr[data-key]");
      if (tr) void showWhy(tr.dataset.key);
    });
    // 顶栏「记忆」菜单项：面板自己绑自己（免得 main.js 再长一段菜单接线）
    document.getElementById("menu-memory")?.addEventListener("click", () => open());
    return root;
  }

  /** 把 data-i18n 的元素文案补齐（面板文案一律走语言文件，不留硬编码） */
  function applyI18n(root) {
    root.querySelectorAll("[data-i18n]").forEach((n) => {
      n.textContent = T(n.dataset.i18n);
    });
    const q = el("mem-q");
    if (q) q.placeholder = T("mem.queryPlaceholder");
  }

  function setStatus(text, ok) {
    const s = el("mem-status");
    if (s) {
      s.textContent = text;
      s.dataset.ok = ok ? "1" : "0";
    }
  }

  function errText(e) {
    return String((e && e.message) || e || "");
  }

  /** 面板整体刷新：先状态（含向量腿是否可用），再当前信念与收据 */
  async function refresh() {
    const api = invoke();
    if (!api) return;
    build();
    try {
      const st = await api("mem_status", { projectRoot: projectRoot() });
      lastScope = st.scope || "";
      el("mem-scope").textContent = st.scope || "";
      const emb = st.embed_available
        ? T("mem.embedOn")
        : T("mem.embedOff", { reason: st.embed_reason || "?" });
      el("mem-embed").textContent = emb;
      el("mem-embed").dataset.ok = st.embed_available ? "1" : "0";
      // 缺模型才露出〔取模型〕：一键把语义检索补齐（单 exe 也是这样装起来的）
      const fetchBtn = el("mem-fetch");
      if (fetchBtn) fetchBtn.style.display = st.embed_available ? "none" : "";
      setStatus(
        T("mem.statLine", {
          events: st.events,
          active: st.beliefs_active,
          all: st.beliefs_all,
          receipts: st.receipts,
          vectors: st.vectors,
        }),
        true,
      );
      const q = el("mem-q")?.value || "";
      const b = await api("mem_beliefs", {
        query: q,
        limit: 100,
        projectRoot: projectRoot(),
      });
      renderNow(b.hits || []);
      const r = await api("mem_receipts", { limit: 50, projectRoot: projectRoot() });
      renderReceipts(r.receipts || []);
    } catch (e) {
      // 记忆没装上属于**能力缺席**而不是崩溃：要说出来，且不要清空成"暂无数据"（那是在撒谎）
      setStatus(errText(e), false);
    }
  }

  function renderNow(hits) {
    const body = el("mem-now-body");
    const empty = el("mem-now-empty");
    body.innerHTML = hits
      .map(
        (h) => `
      <tr data-key="${esc(h.key)}">
        <td class="mem-mono">${esc(h.key)}</td>
        <td>${esc(h.value)}</td>
        <td><span class="mem-tag" data-s="${esc(h.status)}">${esc(h.status)}</span></td>
        <td class="mem-num">${esc(String(h.prov))}</td>
        <td><button class="mem-link" data-i18n="mem.why">${esc(T("mem.why"))}</button></td>
      </tr>`,
      )
      .join("");
    empty.textContent = hits.length ? "" : T("mem.emptyNow");
  }

  function renderReceipts(rs) {
    const body = el("mem-receipts-body");
    body.innerHTML = rs
      .map(
        (r) => `
      <tr>
        <td class="mem-mono">${esc(fmtTs(r.ts))}</td>
        <td>${esc(r.kind)}</td>
        <td class="mem-mono">${esc((r.dropped || []).join(", "))}</td>
        <td class="mem-mono">${esc(r.rehydrate || "")}</td>
      </tr>`,
      )
      .join("");
    el("mem-receipts-empty").textContent = rs.length ? "" : T("mem.emptyReceipts");
  }

  /** why：完整修订链 —— 谁在什么时候、以什么依据、用什么算子改的 */
  async function showWhy(key) {
    const api = invoke();
    if (!api) return;
    try {
      const r = await api("mem_why", { key, projectRoot: projectRoot() });
      const chain = r.chain || [];
      el("mem-why-section").style.display = "";
      el("mem-why-title").textContent = T("mem.whyTitle", { key });
      el("mem-why-body").innerHTML = chain
        .map(
          (e) => `
        <tr>
          <td class="mem-num">${esc(String(e.seq))}</td>
          <td class="mem-mono">${esc(fmtTs(e.ts))}</td>
          <td>${esc(e.kind || "")}</td>
          <td>${esc(e.op || "")}</td>
          <td>${esc(e.value == null ? "" : e.value)}</td>
          <td>${esc(e.origin || "")}</td>
          <td>${esc(e.reason || "")}</td>
        </tr>`,
        )
        .join("");
    } catch (e) {
      setStatus(errText(e), false);
    }
  }

  /** as-of：按时间点查回当时成立的信念（含后来被推翻的） */
  async function refreshAsOf() {
    const api = invoke();
    if (!api) return;
    const raw = el("mem-at")?.value;
    if (!raw) {
      setStatus(T("mem.pickTime"), false);
      return;
    }
    const at = Math.floor(new Date(raw).getTime() / 1000);
    try {
      const r = await api("mem_as_of", { at, projectRoot: projectRoot() });
      el("mem-asof-body").innerHTML = (r.hits || [])
        .map(
          (h) => `
        <tr>
          <td class="mem-mono">${esc(h.key)}</td>
          <td>${esc(h.value)}</td>
          <td><span class="mem-tag" data-s="${esc(h.status)}">${esc(h.status)}</span></td>
          <td class="mem-mono">${esc(fmtTs(h.valid_from))}</td>
          <td class="mem-mono">${esc(h.valid_to ? fmtTs(h.valid_to) : "—")}</td>
        </tr>`,
        )
        .join("");
      setStatus(T("mem.asOfDone", { n: (r.hits || []).length }), true);
    } catch (e) {
      setStatus(errText(e), false);
    }
  }

  /** 人写一条（origin=human）：键 + 值，落成一条观察事件 */
  async function addOne() {
    const api = invoke();
    if (!api) return;
    const key = await window.showPrompt?.(T("mem.addKey"), "");
    if (!key) return;
    const value = await window.showPrompt?.(T("mem.addValue"), "");
    if (value == null) return;
    try {
      await api("mem_record", { key, value, projectRoot: projectRoot() });
      await refresh();
    } catch (e) {
      setStatus(errText(e), false);
    }
  }

  /** 一键取模型：96MB 下载走后台，进度由 `mem://model` 事件推回来（命令立即返回） */
  async function fetchModel() {
    const api = invoke();
    if (!api) return;
    try {
      const r = await api("mem_model_fetch");
      if (el("mem-embed")) el("mem-embed").textContent = r.line || T("mem.fetchStart");
    } catch (e) {
      setStatus(errText(e), false);
    }
  }

  /** 重放账本重建派生层（派生的东西删了也不丢东西 —— EC 的可操作形态） */
  async function rebuild() {
    const api = invoke();
    if (!api) return;
    try {
      const r = await api("mem_rebuild", { projectRoot: projectRoot() });
      setStatus(T("mem.rebuilt", { n: r.replayed_events }), true);
      await refresh();
    } catch (e) {
      setStatus(errText(e), false);
    }
  }

  return {
    /** 打开（或复用一个已存在的）记忆面板标签页 —— 与服务面板同一套惯例 */
    open() {
      const s = window.state;
      if (!s) return;
      const existing = s.tabs.find((t) => t._isMemory);
      if (existing) {
        switchTab(existing.id);
        return;
      }
      const t = {
        id: "mem-" + Date.now().toString(),
        name: T("mem.title"),
        path: "",
        content: "",
        _isMemory: true,
      };
      s.tabs.push(t);
      renderTabs();
      switchTab(t.id);
    },

    /** 进入这个标签页时挂载/刷新 */
    render(t) {
      tab = t;
      build();
      void refresh();
    },

    /** 标签页关掉：停掉一切后台动作（本面板没有轮询，只需清掉展开的修订链） */
    close() {
      const s = el("mem-why-section");
      if (s) s.style.display = "none";
      tab = null;
    },

    refresh,
  };
})();
