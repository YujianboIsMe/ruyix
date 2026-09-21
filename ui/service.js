/**
 * ruyix — 服务（托管进程）面板：顶部菜单「服务」→ 中央编辑区打开。
 *
 * ## 治的是什么病
 *
 * agent 用 background 起的常驻服务（mvn spring-boot:run / java -jar / npm run dev）是
 * **宿主** spawn 的，却不在宿主的进程树里（Windows 上 cmd → mvn.cmd → java 三代）。
 * 它们只活在引擎的进程表里，UI 上等于不存在：起得来、看不见、也停不掉 ——
 * 用户只能开任务管理器按 pid 找。这个面板把那份表摆到界面上，并允许按 pid 收掉。
 *
 * ## 三条纪律
 *
 * 1. **读引擎同一份表**。后端 `proc_list` 直接返回 `harness_engine::proc::listing()`，
 *    前端不自己扫端口、不自己猜进程 —— 面板和模型看的是同一份事实，不各说各话。
 * 2. **一手证据是 pid**。表里本来就按 pid 存（见 proc.rs 的 `table()`），
 *    所以"停止"直接按 pid 发，不需要先翻译成引擎自造的 handle。
 * 3. **时间给全**：年月日时分秒 + 括号里的"已启动多久"。只有时刻能算出"几点起的"，
 *    只有时长能回答"跑了多久了" —— 少一个，用户就得自己拿当前时间去减。
 *
 * 表格是**学术三线表**：只有顶线、表头下线、底线三条横线，没有竖线、没有内部行线。
 */

window.ServiceUI = (() => {
  "use strict";

  const T = (k) => (window.I18N && typeof I18N.t === "function" ? I18N.t(k) : k);

  /// 刷新间隔（毫秒）。时长要"在走"，1 秒是肉眼能察觉又不吵的下限。
  const TICK_MS = 1000;

  let timer = null;
  /// 上一次渲染的行（按 pid 排序的 pid 列表）—— 用来判断"只更新时间"还是"整表重建"
  let lastPids = [];

  function invoke() {
    return typeof getTauriInvoke === "function" ? getTauriInvoke() : null;
  }

  /** 已结束的进程（`exited(码)` / `stopped`）不进面板：它没有"管理"可言 */
  function isDead(state) {
    return String(state || "").startsWith("exited") || String(state || "") === "stopped";
  }

  /** 补零到两位 */
  function pad(n) {
    return String(n).padStart(2, "0");
  }

  /**
   * 启动时刻 → `2026-09-20 21:30:05`。用本地时区 —— 用户看的是"我这台机器上的几点"。
   */
  function fmtStarted(ms) {
    const d = new Date(Number(ms) || 0);
    return (
      `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
      `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
    );
  }

  /**
   * 已启动多久 → `2h23m36s`。时分秒按 60 / 60 进位（秒位 < 60），
   * 零值也照实写 `0h0m5s`，不省略 —— 一眼能认出这是时长而不是时刻。
   */
  function fmtElapsed(ms) {
    const total = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    return `${h}h${m}m${s}s`;
  }

  function esc(s) {
    return String(s ?? "")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function rowHtml(p) {
    // 停止按钮放在命令行单元格里（右侧浮动）：表格的**字段**仍然只有三个
    // （PID / 启动时间 / 完整命令行），操作不是字段，不该占一列。
    return (
      `<tr data-pid="${p.pid}">` +
      `<td class="service-col-pid">${p.pid}</td>` +
      `<td class="service-col-started">` +
      `${esc(fmtStarted(p.started_at_ms))} ` +
      `<span class="service-elapsed" data-started="${p.started_at_ms}">` +
      `(${esc(fmtElapsed(p.elapsed_ms))})</span>` +
      `</td>` +
      `<td class="service-col-cmd">` +
      `<span class="service-cmd" title="${esc(p.cmd)}">${esc(p.cmd)}</span>` +
      `<button class="service-stop" data-stop="${p.pid}" data-i18n="service.btn_stop">停止</button>` +
      `</td>` +
      `</tr>`
    );
  }

  function tableHtml(items) {
    return (
      `<table class="service-table">` +
      `<thead><tr>` +
      `<th class="service-col-pid" data-i18n="service.th_pid">PID</th>` +
      `<th class="service-col-started" data-i18n="service.th_started">启动时间</th>` +
      `<th class="service-col-cmd" data-i18n="service.th_cmd">完整命令行</th>` +
      `</tr></thead>` +
      `<tbody>${items.map(rowHtml).join("")}</tbody>` +
      `</table>`
    );
  }

  function setCount(n) {
    const el = document.getElementById("service-count");
    if (el) el.textContent = n > 0 ? `（${n}）` : "";
  }

  /**
   * 只更新"已启动多久"那一列。整表每秒重建会打断用户选中的文本、也会让按钮抖，
   * 所以行集合没变时只改这几个 span 的文本。
   */
  function tickElapsed(items) {
    for (const p of items) {
      const span = document.querySelector(`.service-elapsed[data-started="${p.started_at_ms}"]`);
      if (span) span.textContent = `(${fmtElapsed(p.elapsed_ms)})`;
    }
  }

  async function fetchLive() {
    const inv = invoke();
    if (!inv) return null;
    const all = (await inv("proc_list")) || [];
    return all
      .filter((p) => !isDead(p.state))
      .slice()
      .sort((a, b) => a.started_at_ms - b.started_at_ms);
  }

  async function pull() {
    const body = document.getElementById("service-body");
    if (!body) return;

    let items = null;
    try {
      items = await fetchLive();
    } catch {
      body.innerHTML = `<div class="service-empty">${esc(T("service.load_fail"))}</div>`;
      setCount(0);
      return;
    }
    if (items === null) {
      body.innerHTML = `<div class="service-empty">${esc(T("service.no_tauri"))}</div>`;
      setCount(0);
      return;
    }
    if (items.length === 0) {
      body.innerHTML = `<div class="service-empty">${esc(T("service.empty"))}</div>`;
      setCount(0);
      lastPids = [];
      return;
    }

    const pids = items.map((p) => p.pid);
    const same = pids.length === lastPids.length && pids.every((v, i) => v === lastPids[i]);
    if (same) {
      tickElapsed(items);
      return;
    }

    body.innerHTML = tableHtml(items);
    lastPids = pids;
    setCount(items.length);
    bindStops(body);
    applyI18n(body);
  }

  function bindStops(body) {
    body.querySelectorAll("[data-stop]").forEach((btn) => {
      btn.addEventListener("click", () => stop(Number(btn.dataset.stop)));
    });
  }

  /** 面板内的 data-i18n 文案（表头、按钮）—— 切语言后要跟着走 */
  function applyI18n(root) {
    root.querySelectorAll("[data-i18n]").forEach((el) => {
      el.textContent = T(el.dataset.i18n);
    });
  }

  async function stop(pid) {
    const inv = invoke();
    if (!inv) return;
    const btn = document.querySelector(`.service-stop[data-stop="${pid}"]`);
    if (btn) {
      btn.disabled = true;
      btn.textContent = T("service.stopping");
    }
    try {
      await inv("proc_stop", { pid });
    } catch {
      if (typeof setStatus === "function") setStatus(T("service.stop_fail"), "error");
      if (btn) {
        btn.disabled = false;
        btn.textContent = T("service.btn_stop");
      }
      return;
    }
    lastPids = []; // 强制整表重建：行少了
    await pull();
  }

  /** 进入面板：起定时器并拉一次（时长要自己走） */
  function render() {
    startTimer();
    pull();
  }

  function startTimer() {
    if (timer !== null) return;
    timer = setInterval(() => {
      pull();
    }, TICK_MS);
  }

  /** 离开面板 / 关闭标签页：定时器必须停，否则后台一直在问后端 */
  function close() {
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
    lastPids = [];
  }

  /** 打开（或激活已有的）服务标签页 */
  function open() {
    const s = window.state;
    if (!s) return;
    const existing = s.tabs.find((t) => t._isService);
    if (existing) {
      switchTab(existing.id);
      return;
    }
    const tab = {
      id: "service-" + Date.now().toString(),
      name: T("service.title"),
      path: "",
      content: "",
      _isService: true,
    };
    s.tabs.push(tab);
    renderTabs();
    switchTab(tab.id);
  }

  function attach() {
    document.getElementById("service-btn-refresh")?.addEventListener("click", () => {
      lastPids = [];
      pull();
    });
  }

  return { attach, open, render, pull, stop, close };
})();
