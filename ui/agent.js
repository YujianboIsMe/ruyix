/**
 * Darkhorse Code — Agent 控制台（融合计划 Z4/Z5）
 *
 * 渲染 agent://* 事件流（payload 与 harness:// 逐字段同形，见《融合计划》附录 B）。
 * 引擎落位（P1）前内置演示回放：用与真实契约同形的事件数据驱动全部渲染器，
 * 前端在无后端时即可自测与预览；Tauri invoke 可用时自动走真实 agent_run。
 */

(function () {
  "use strict";

  // 动态文案：浏览器直开（无 lang 文件）时 I18N.t 只会回显 key，故用 L() 兜底
  const L = (zh, en) => (window.I18N && I18N.getLang() === "en" ? en : zh);

  const STAGES = [
    { id: "plan", label: () => L("规划", "Plan") },
    { id: "generate", label: () => L("生成", "Generate") },
    { id: "lint", label: () => L("规约", "Lint") },
    { id: "verify", label: () => L("验证", "Verify") },
    { id: "repair", label: () => L("修复", "Repair") },
  ];

  const STEP_ICON = {
    running: "●", done: "✓", error: "✗", pending: "○",
  };

  let host = null;           // main.js 注入的视图切换钩子
  let elapsedTimer = null;   // 运行秒表
  let demoTimers = [];       // 演示回放的挂起定时器
  let seq = 0;               // run 序号

  const vm = newRunVm();
  const history = [];        // 演示历史（内存态）

  function newRunVm() {
    return {
      runId: null,
      task: "",
      mode: "full",
      status: "idle",        // idle | running | done | error | canceled
      startedAt: 0,
      elapsed: 0,
      demo: false,
      stages: {},            // { plan: {status, detail}, ... }
      steps: [],             // agent://step 的累积（按 index 更新）
      logs: [],              // {time, level, msg}
      lint: null,            // agent://lint payload
      repairs: [],           // agent://repair payload（按 round 更新）
      kb: null,              // agent://kb payload
    };
  }

  // ============================================
  // 工具
  // ============================================

  function $(id) {
    return document.getElementById(id);
  }

  function esc(s) {
    return String(s ?? "").replace(/[&<>"']/g, (c) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
    }[c]));
  }

  function nowHms() {
    return new Date().toTimeString().slice(0, 8);
  }

  function newRunId() {
    const d = new Date();
    const p = (n) => String(n).padStart(2, "0");
    return `run_${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}` +
      `_${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}_${String(++seq).padStart(3, "0")}`;
  }

  // ============================================
  // 事件入口（与附录 B 契约同形；真实模式由 Tauri listen 接入）
  // ============================================

  function onLog(p) {
    vm.logs.push({ time: nowHms(), level: p.level || "info", msg: p.msg || "" });
    if (vm.logs.length > 400) vm.logs.shift();
    renderLog();
  }

  function onStage(p) {
    vm.stages[p.stage] = { status: p.status, detail: p.detail || "" };
    renderPipeline();
    renderReports();
  }

  function onStep(p) {
    const i = vm.steps.findIndex((s) => s.index === p.index);
    if (i >= 0) vm.steps[i] = p;
    else vm.steps.push(p);
    vm.steps.sort((a, b) => (a.index || 0) - (b.index || 0));
    renderSteps();
  }

  function onLint(p) {
    vm.lint = p;
    renderReports();
  }

  function onRepair(p) {
    const i = vm.repairs.findIndex((r) => r.round === p.round);
    if (i >= 0) vm.repairs[i] = p;
    else vm.repairs.push(p);
    renderReports();
  }

  function onKb(p) {
    vm.kb = p;
    renderReports();
  }

  // ============================================
  // 渲染
  // ============================================

  function renderShell() {
    const idle = $("agent-console-idle");
    const run = $("agent-console-run");
    if (!idle || !run) return;
    if (vm.status === "idle") {
      idle.style.display = "";
      run.style.display = "none";
    } else {
      idle.style.display = "none";
      run.style.display = "";
    }
    renderHeader();
    renderPipeline();
    renderSteps();
    renderReports();
    renderLog();
  }

  function renderHeader() {
    const meta = $("agent-run-meta");
    const pill = $("agent-status-pill");
    if (!meta || !pill) return;

    const modeText = vm.mode === "plan" ? L("仅规划", "plan-only") : L("全流程", "full");
    const parts = [];
    if (vm.runId) parts.push(vm.runId);
    if (vm.task) parts.push(esc(truncate(vm.task, 40)));
    parts.push(modeText);
    parts.push(`${vm.elapsed}s`);
    meta.textContent = parts.join(" · ");

    const P = {
      running: { cls: "agent-pill--run", text: L("运行中", "Running") },
      done: { cls: "agent-pill--ok", text: L("完成", "Done") },
      error: { cls: "agent-pill--err", text: L("失败", "Failed") },
      canceled: { cls: "agent-pill--off", text: L("已取消", "Canceled") },
      idle: { cls: "", text: "" },
    }[vm.status] || { cls: "", text: "" };
    pill.className = "agent-pill " + P.cls;
    pill.textContent = (vm.demo ? L("演示 · ", "demo · ") : "") + P.text;

    const running = vm.status === "running";
    setBtnEnabled($("agent-cancel-btn"), running);
    setBtnEnabled($("agent-console-cancel"), running);
  }

  function setBtnEnabled(btn, enabled) {
    if (btn) btn.disabled = !enabled;
  }

  function truncate(s, n) {
    return s.length > n ? s.slice(0, n - 1) + "…" : s;
  }

  function renderPipeline() {
    const el = $("agent-pipeline");
    if (!el) return;
    el.innerHTML = STAGES.map((st, i) => {
      const s = vm.stages[st.id] || { status: "pending", detail: "" };
      const cls = {
        start: "running", done: "done", error: "error", skip: "skip",
      }[s.status] || "pending";
      const icon = {
        running: "●", done: "✓", error: "✗", skip: "−", pending: "○",
      }[cls];
      const arrow = i < STAGES.length - 1
        ? '<span class="agent-stage-arrow">─</span>' : "";
      const detail = s.detail ? `<span class="agent-stage-detail">${esc(s.detail)}</span>` : "";
      return `${arrow}<span class="agent-stage agent-stage--${cls}">` +
        `<span class="agent-stage-icon">${icon}</span>${esc(st.label())}` +
        `${detail}</span>`;
    }).join("");
  }

  function renderSteps() {
    const el = $("agent-steps");
    if (!el) return;
    if (!vm.steps.length) {
      el.innerHTML = `<div class="agent-empty">${L("等待事件流…", "Waiting for events…")}</div>`;
      return;
    }
    el.innerHTML = vm.steps.map((s) => {
      const icon = STEP_ICON[s.status] || "○";
      const notes = s.notes ? `<div class="agent-step-notes">${esc(s.notes)}</div>` : "";
      const files = Array.isArray(s.files) && s.files.length
        ? `<div class="agent-step-files">${s.files.map((f) =>
            `<code>${esc(f)}</code>`).join("")}</div>` : "";
      const error = s.error ? `<div class="agent-step-error">${esc(s.error)}</div>` : "";
      return `<div class="agent-step agent-step--${s.status || "pending"}">` +
        `<div class="agent-step-head"><span class="agent-step-idx">#${s.index}</span>` +
        `<span class="agent-step-title">${esc(s.title || "")}</span>` +
        `<span class="agent-step-icon">${icon}</span></div>` +
        `${notes}${files}${error}</div>`;
    }).join("");
  }

  function renderReports() {
    const el = $("agent-reports");
    if (!el) return;
    const cards = [];

    // 验证卡（附录 B 无独立 verify payload，状态经 stage 事件 + detail 传递）
    const v = vm.stages.verify;
    if (v) {
      const cls = { start: "run", done: "ok", error: "err", skip: "off" }[v.status] || "off";
      const label = {
        start: L("进行中", "running"), done: L("通过", "passed"),
        error: L("未通过", "failed"), skip: L("跳过", "skipped"),
      }[v.status] || v.status;
      cards.push('<div class="agent-card agent-card--verify">' +
        `<div class="agent-card-head">${L("验证", "Verify")}` +
        `<span class="agent-badge agent-badge--${cls}">${label}</span></div>` +
        `<div class="agent-card-body">${esc(v.detail || "—")}</div></div>`);
    }

    // 规约卡
    if (vm.lint) {
      const l = vm.lint;
      const okBadge = l.ran
        ? (l.ok ? `<span class="agent-badge agent-badge--ok">${L("通过", "clean")}</span>`
                : `<span class="agent-badge agent-badge--err">${l.errors}E / ${l.warnings}W</span>`)
        : `<span class="agent-badge agent-badge--off">${L("未运行", "not run")}</span>`;
      const diags = Array.isArray(l.diagnostics) ? l.diagnostics : [];
      const rows = diags.slice(0, 6).map((d) =>
        `<div class="agent-diag"><code>${esc(d.file || "")}${d.line ? ":" + d.line : ""}</code>` +
        `${d.rule ? ` <span class="agent-diag-rule">${esc(d.rule)}</span>` : ""}` +
        ` <span>${esc(d.msg || "")}</span></div>`).join("");
      cards.push('<div class="agent-card agent-card--lint">' +
        `<div class="agent-card-head">${L("规约", "Lint")}${okBadge}</div>` +
        `<div class="agent-card-body">${esc(l.summary || l.cmd || "")}</div>${rows}</div>`);
    }

    // 修复轮次卡
    for (const r of vm.repairs) {
      const running = r.status === "running";
      const badge = running
        ? `<span class="agent-badge agent-badge--run">${L("进行中", "running")}</span>`
        : (r.applied
            ? `<span class="agent-badge agent-badge--ok">${L("已修复", "fixed")}</span>`
            : `<span class="agent-badge agent-badge--off">${L("未应用", "not applied")}</span>`);
      cards.push('<div class="agent-card agent-card--repair">' +
        `<div class="agent-card-head">${L("修复", "Repair")} · ${L("第", "round")} ${r.round}${badge}</div>` +
        `<div class="agent-card-body"><code>${esc(r.before || "—")}</code> → ` +
        `<code>${esc(r.after || "…")}</code></div>` +
        `${r.detail ? `<div class="agent-card-body">${esc(r.detail)}</div>` : ""}` +
        `${r.notes ? `<div class="agent-card-body agent-card-note">${esc(r.notes)}</div>` : ""}</div>`);
    }

    // 知识库卡（kb.enabled 时才有事件）
    if (vm.kb) {
      const k = vm.kb;
      const pct = k.total ? Math.round((k.done / k.total) * 100) : 0;
      cards.push('<div class="agent-card agent-card--kb">' +
        `<div class="agent-card-head">${L("知识库", "KB")} · ${esc(k.phase || "")}</div>` +
        `<div class="agent-card-body">${k.done}/${k.total} · ${pct}% · ${esc(k.msg || "")}</div>` +
        `<div class="agent-progress"><div class="agent-progress-bar" style="width:${pct}%"></div></div></div>`);
    }

    el.innerHTML = cards.length
      ? cards.join("")
      : `<div class="agent-empty">${L("暂无报告", "No reports yet")}</div>`;
  }

  function renderLog() {
    const el = $("agent-log");
    if (!el) return;
    if (!vm.logs.length) {
      el.innerHTML = `<div class="agent-empty">${L("等待日志…", "Waiting for logs…")}</div>`;
      return;
    }
    el.innerHTML = vm.logs.map((l) =>
      `<div class="agent-log-line agent-log-line--${esc(l.level)}">` +
      `[${l.time}] ${esc(l.level.toUpperCase())} ${esc(l.msg)}</div>`).join("");
    el.scrollTop = el.scrollHeight;
  }

  function renderEnvChips() {
    const el = $("agent-env-chips");
    if (!el) return;
    // P2 接入 agent_env_probe 后替换为真实探针
    const env = [
      { name: "docker", ok: false, note: L("未隔离 · 宿主执行", "unisolated · host") },
      { name: "python", ok: true, note: "3.12.4" },
      { name: "git", ok: true, note: "2.45.2" },
      { name: "node", ok: true, note: "22.9.0" },
    ];
    el.innerHTML = env.map((e) =>
      `<span class="agent-chip agent-chip--${e.ok ? "ok" : "warn"}">` +
      `${e.ok ? "✓" : "✗"} ${esc(e.name)} ${esc(e.note)}</span>`).join("");
  }

  function renderHistory() {
    const el = $("agent-history-list");
    if (!el) return;
    if (!history.length) {
      el.innerHTML = `<div class="agent-history-empty">${L("暂无运行记录", "No runs yet")}</div>`;
      return;
    }
    el.innerHTML = history.slice().reverse().map((h, i) =>
      `<div class="agent-history-item" data-hist="${history.length - 1 - i}">` +
      `<span class="agent-dot agent-dot--${h.status}"></span>` +
      `<span class="agent-history-task">${esc(truncate(h.task, 26))}</span>` +
      `<span class="agent-history-time">${esc(h.elapsed)}s</span></div>`).join("");
    el.querySelectorAll("[data-hist]").forEach((item) => {
      item.addEventListener("click", () => restoreRun(history[Number(item.dataset.hist)]));
    });
  }

  // ============================================
  // 运行控制
  // ============================================

  function currentMode() {
    const radio = document.querySelector('input[name="agent-mode"]:checked');
    return radio ? radio.value : "full";
  }

  function startTask(task, mode) {
    if (vm.status === "running") return;
    Object.assign(vm, newRunVm(), { task, mode, status: "running", startedAt: Date.now() });
    startClock();
    renderShell();
    showConsole();

    const invoke = window.__TAURI__?.core?.invoke;
    if (invoke) {
      invoke("agent_run", { task, projectRoot: null })
        .then(() => finish("done"))
        .catch(() => {
          onLog({ level: "warn", msg: L("引擎未接入（P1 落位后可用），以下为演示回放",
            "Engine not wired yet (lands in P1); replaying demo below") });
          startDemo(task, mode, true);
        });
      return;
    }
    startDemo(task, mode, true);
  }

  function startDemo(task, mode, keepVm) {
    if (vm.status === "running" && !keepVm) return;
    if (!keepVm) {
      Object.assign(vm, newRunVm(), {
        task: task || L("演示任务：实现 truncate_words 并补测试",
          "Demo task: implement truncate_words with tests"),
        mode: mode || "full",
      });
    } else {
      vm.runId = vm.runId || newRunId();
    }
    vm.demo = true;
    vm.status = "running";
    vm.runId = vm.runId || newRunId();
    vm.startedAt = Date.now();
    startClock();
    renderShell();
    showConsole();

    const script = buildDemoScript(vm.mode);
    let acc = 0;
    for (const item of script) {
      acc += item.dt;
      demoTimers.push(setTimeout(() => dispatch(item.ev), acc));
    }
    demoTimers.push(setTimeout(() => finish("done"), acc + 300));
  }

  function cancelRun() {
    if (vm.status !== "running") return;
    stopDemo();
    finish("canceled");
    onLog({ level: "warn", msg: L("已取消（取消位贯通 <1s）", "canceled (cancel flag <1s)") });
  }

  function finish(status) {
    stopClock();
    stopDemo();
    vm.status = status;
    if (status === "done" && vm.stages.verify?.status !== "done" && vm.mode !== "plan") {
      // 引擎侧异常提前结束时按失败呈现
      vm.status = "error";
    }
    renderHeader();
    if (vm.runId) {
      history.push({
        id: vm.runId,
        task: vm.task,
        status: vm.status,
        elapsed: vm.elapsed,
        vmJson: JSON.stringify({ ...vm, logs: vm.logs.slice(-50) }),
      });
      if (history.length > 20) history.shift();
      renderHistory();
    }
    if (typeof setStatus === "function") {
      setStatus(L("Agent 运行结束", "Agent run finished"));
    }
  }

  function restoreRun(entry) {
    if (vm.status === "running") return;
    try {
      Object.assign(vm, JSON.parse(entry.vmJson));
    } catch {
      return;
    }
    renderShell();
    showConsole();
  }

  function startClock() {
    stopClock();
    elapsedTimer = setInterval(() => {
      vm.elapsed = Math.round((Date.now() - vm.startedAt) / 1000);
      const meta = $("agent-run-meta");
      if (meta) meta.textContent = meta.textContent.replace(/\d+s$/, vm.elapsed + "s");
    }, 1000);
  }

  function stopClock() {
    if (elapsedTimer) clearInterval(elapsedTimer);
    elapsedTimer = null;
  }

  function stopDemo() {
    demoTimers.forEach(clearTimeout);
    demoTimers = [];
  }

  function dispatch(ev) {
    const fn = {
      log: onLog, stage: onStage, step: onStep, lint: onLint,
      repair: onRepair, kb: onKb,
    }[ev.type];
    if (fn) fn(ev.payload);
  }

  // ============================================
  // 演示事件脚本（与真实契约同形；kb 默认关闭故无 kb 事件）
  // ============================================

  function buildDemoScript(mode) {
    const s = [
      { dt: 0, ev: { type: "log", payload: { level: "info",
        msg: "run 开始 · sandbox=prefer（docker 不可用 → 宿主执行 · 未隔离）· kb=off" } } },
      { dt: 300, ev: { type: "stage", payload: { stage: "plan", status: "start" } } },
      { dt: 300, ev: { type: "step", payload: { index: 1, total: 3,
        title: "解析任务与上下文", status: "running" } } },
      { dt: 700, ev: { type: "step", payload: { index: 1, total: 3,
        title: "解析任务与上下文", status: "done",
        notes: "定位 src/utils/string.rs · lib.rs 已导出模块" } } },
      { dt: 300, ev: { type: "step", payload: { index: 2, total: 3,
        title: "制定实现方案", status: "running" } } },
      { dt: 700, ev: { type: "step", payload: { index: 2, total: 3,
        title: "制定实现方案", status: "done",
        notes: "pub fn truncate_words(s: &str, max_words: usize) -> String" } } },
      { dt: 300, ev: { type: "step", payload: { index: 3, total: 3,
        title: "规划测试用例", status: "running" } } },
      { dt: 700, ev: { type: "step", payload: { index: 3, total: 3,
        title: "规划测试用例", status: "done",
        notes: "4 例：空串 / 短于上限 / 恰好等于 / 多字节中文" } } },
      { dt: 300, ev: { type: "stage", payload: { stage: "plan", status: "done",
        detail: "3 步 · 计划就绪" } } },
    ];
    if (mode === "plan") {
      s.push({ dt: 300, ev: { type: "log", payload: { level: "ok",
        msg: "plan-only 完成 · 计划可编辑后走 agent_generate" } } });
      return s;
    }
    s.push(
      { dt: 300, ev: { type: "stage", payload: { stage: "generate", status: "start" } } },
      { dt: 200, ev: { type: "step", payload: { index: 1, total: 2,
        title: "生成 src/utils/string.rs", status: "running" } } },
      { dt: 900, ev: { type: "step", payload: { index: 1, total: 2,
        title: "生成 src/utils/string.rs", status: "done",
        notes: "+64 行", files: ["src/utils/string.rs"] } } },
      { dt: 300, ev: { type: "step", payload: { index: 2, total: 2,
        title: "生成 tests/string_test.rs", status: "running" } } },
      { dt: 900, ev: { type: "step", payload: { index: 2, total: 2,
        title: "生成 tests/string_test.rs", status: "done",
        notes: "+38 行 · 4 用例", files: ["tests/string_test.rs"] } } },
      { dt: 300, ev: { type: "stage", payload: { stage: "generate", status: "done",
        detail: "2 文件 · 102 行" } } },
      { dt: 300, ev: { type: "stage", payload: { stage: "lint", status: "start" } } },
      { dt: 600, ev: { type: "lint", payload: { ran: true, ok: false, errors: 2,
        warnings: 1, summary: "ruff: 2 errors · 1 warning", cmd: "ruff check .",
        diagnostics: [
          { file: "src/utils/string.rs", line: 42, rule: "E501", msg: "line too long (91 > 88)" },
          { file: "src/utils/string.rs", line: 57, rule: "E721", msg: "use `is` or `is not`" },
          { file: "tests/string_test.rs", line: 12, rule: "W291", msg: "trailing whitespace" },
        ] } } },
      { dt: 200, ev: { type: "stage", payload: { stage: "lint", status: "done",
        detail: "2E · 1W" } } },
      { dt: 300, ev: { type: "stage", payload: { stage: "verify", status: "start" } } },
      { dt: 200, ev: { type: "log", payload: { level: "info", msg: "cargo test --color never" } } },
      { dt: 1000, ev: { type: "stage", payload: { stage: "verify", status: "error",
        detail: "tests 3/4 · string_test::test_multibyte 断言失败" } } },
      { dt: 200, ev: { type: "step", payload: { index: 3, total: 3,
        title: "运行测试", status: "error",
        error: "string_test::test_multibyte: 断言失败（期望 \"你好世…\"，实得 \"你好世\"）" } } },
      { dt: 300, ev: { type: "stage", payload: { stage: "repair", status: "start" } } },
      { dt: 200, ev: { type: "repair", payload: { round: 1, before: "tests 3/4",
        after: "—", status: "running", applied: false } } },
      { dt: 900, ev: { type: "repair", payload: { round: 1, before: "tests 3/4",
        after: "tests 4/4", status: "done", applied: true,
        detail: "按 char_indices 对齐 UTF-8 词边界后截断",
        notes: "改动 src/utils/string.rs:31-36" } } },
      { dt: 300, ev: { type: "stage", payload: { stage: "verify", status: "start" } } },
      { dt: 200, ev: { type: "log", payload: { level: "info",
        msg: "cargo test --color never（复验）" } } },
      { dt: 1000, ev: { type: "stage", payload: { stage: "verify", status: "done",
        detail: "tests 4/4 · build ✓" } } },
      { dt: 200, ev: { type: "step", payload: { index: 3, total: 3,
        title: "运行测试", status: "done", notes: "4/4 通过" } } },
      { dt: 300, ev: { type: "log", payload: { level: "ok",
        msg: "run 完成 · 1 轮修复 · 用量 11.7k tokens" } } },
    );
    return s;
  }

  // ============================================
  // 页面切换（镜像 showHelpPage 的显示逻辑）
  // ============================================

  function showConsole() {
    const page = $("agent-page");
    if (!page) return;
    for (const id of ["welcome-content", "help-page", "editor-body"]) {
      const el = $(id);
      if (el) el.style.display = "none";
    }
    page.style.display = "";
  }

  function hideConsole() {
    const page = $("agent-page");
    if (page) page.style.display = "none";
    if (host) {
      if (host.isProjectOpen()) host.showProjectWorkspace();
      else host.showWelcomePage();
    } else {
      const body = $("editor-body");
      if (body) body.style.display = "";
    }
  }

  // ============================================
  // 命令栏接入（Z5：agent <任务>）
  // ============================================

  function handleCommand(rest) {
    const arg = String(rest || "").trim();
    if (!arg) {
      showConsole();
      return;
    }
    const input = $("agent-task-input");
    if (input) input.value = arg;
    startTask(arg, currentMode());
  }

  // ============================================
  // 初始化（main.js 注入 AgentHost 后调用）
  // ============================================

  function attach(host_) {
    host = host_;

    $("agent-run-btn")?.addEventListener("click", () => {
      const input = $("agent-task-input");
      const task = (input?.value || "").trim();
      if (!task) {
        input?.focus();
        if (typeof setStatus === "function") {
          setStatus(L("请先描述任务", "Describe a task first"), "error");
        }
        return;
      }
      startTask(task, currentMode());
    });

    $("agent-cancel-btn")?.addEventListener("click", cancelRun);
    $("agent-console-cancel")?.addEventListener("click", cancelRun);
    $("agent-demo-btn")?.addEventListener("click", () => startDemo("", currentMode(), false));
    $("agent-hero-demo")?.addEventListener("click", () => startDemo("", currentMode(), false));
    $("agent-console-close")?.addEventListener("click", hideConsole);

    $("agent-task-input")?.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) $("agent-run-btn")?.click();
    });

    // nav 标签点击：面板由 setupNavigatorTabs 切换，这里联动控制台视图
    document.querySelector('.nav-tab[data-tab="agent"]')
      ?.addEventListener("click", () => {
        if (vm.status !== "running") showConsole();
      });

    renderEnvChips();
    renderHistory();
    renderShell();

    // 真实事件流（引擎落位后生效；Tauri 事件 API 不可用时静默跳过）
    try {
      const listen = window.__TAURI__?.event?.listen;
      if (listen) {
        const MAP = [
          ["agent://log", onLog], ["agent://stage", onStage], ["agent://step", onStep],
          ["agent://lint", onLint], ["agent://repair", onRepair], ["agent://kb", onKb],
        ];
        for (const [name, fn] of MAP) {
          listen(name, (ev) => fn(ev.payload)).catch(() => {});
        }
      }
    } catch {
      // 浏览器模式无 Tauri 事件
    }
  }

  window.AgentUI = { attach, handleCommand };
})();
