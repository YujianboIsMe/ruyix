/**
 * ruyix — 配置表单（配置菜单 → 全局 / 项目 / 运行）
 *
 * 一个作用域 = 一个表单标签：扫描该作用域及其回退链（runtime → project → global）
 * 上的全部平铺配置项，按 section 分组渲染成表单；已知项给标签/类型/枚举，
 * 扫描到的未知键按值形状猜类型也能编辑。
 *
 * 三个按钮（都走命令系统）：
 *   保存  只把改动过的行写回该作用域（空值 = 删除该键）
 *   应用  保存 + 把这些值刷新进 IDE 运行时内存里的配置对象（优先级最高，重启失效）
 *   取消  丢弃未保存的改动，重新扫描
 *
 * 与其它面板同款防御式写法：无 Tauri（浏览器直开）时显示"后端不可用"。
 */

window.ConfigUI = (() => {
  "use strict";

  const L = (zh, en) => (window.I18N && I18N.getLang() === "en" ? en : zh);
  const SUB_COMMANDS = ["form", "save", "apply", "cancel"];

  // ============================================
  // 已知配置项 schema（"扫描配置项"的已知面）
  // 标签 / 说明走 i18n：config.field.<section>.<key> / config.desc.<section>.<key>
  // 这里只声明 section 顺序、字段顺序与控件类型
  // ============================================
  const SCHEMA = [
    {
      section: "ai",
      fields: [
        { key: "api_url", kind: "text" },
        { key: "api_key", kind: "password" },
        { key: "model", kind: "text" },
        { key: "alias", kind: "text" },
      ],
    },
    {
      section: "ui",
      fields: [
        { key: "lang", kind: "select", options: ["zh-CN", "en"] },
        { key: "emoji", kind: "toggle" },
      ],
    },
    {
      section: "harness",
      fields: [
        { key: "workspace_root", kind: "text" },
        { key: "llm.temperature", kind: "number" },
        { key: "llm.max_tokens", kind: "number" },
        { key: "sandbox.mode", kind: "select", options: ["prefer", "require", "off"] },
        { key: "sandbox.image", kind: "text" },
        { key: "lint.enabled", kind: "toggle" },
        { key: "lint.package_dir", kind: "text" },
        { key: "lint.max_repair_rounds", kind: "number" },
        { key: "kb.enabled", kind: "toggle" },
        { key: "kb.top_k", kind: "number" },
        // v0.4 计划即执行：默认开。关掉就回到"plan 只给用户看进度"的旧行为 ——
        // 但关着的时候大纲进度只能靠推断（拿模型事前列的 files 对账），实测会大面积误判，
        // 所以这个开关要摆在明面上，用户看得见"我关了什么"。
        { key: "step.execute_plan", kind: "toggle" },
        { key: "step.max_steps", kind: "number" },
        { key: "agent.max_elapsed_secs", kind: "number" },
        // v0.5 命令发现：把"本机有什么命令"实测后写进模型上下文。默认开 —— 关掉之后模型
        // 只能自己一轮轮试（实测有 5~6 轮纯耗在探 mvn / java 在不在）。extra 是逃生口：
        // 工具表没覆盖的命令写在这里，逗号分隔，不用改代码。
        { key: "discover.enabled", kind: "toggle" },
        { key: "discover.ttl_secs", kind: "number" },
        { key: "discover.extra", kind: "text" },
        // v0.5 环境准备：缺失工具按需安装，走 connect（宿主挑包管理器，每次留记录）。默认开 ——
        // 这是自成长闭环的最后一环（探测 → 告知 → 请求安装 → 再探测）。关掉后宿主连清单都不摆
        // env 目标，模型侧彻底看不见。
        { key: "env.install_enabled", kind: "toggle" },
        // v0.6 托管进程：永不退出的服务有了生命周期出口。后台起 + 一条命令判就绪 + 句柄收。
        // 这不是新原语，是 execute 的第三个维度。默认开 —— 关掉后 background 一律被拒，
        // 模型会被推回 `start` / `Start-Process` 那套歪招（实测就死在这儿，空转 17 轮）。
        { key: "proc.enabled", kind: "toggle" },
        { key: "proc.max", kind: "number" },
        { key: "proc.ready_timeout_secs", kind: "number" },
        // v0.7 批量调用：一轮发多个互不依赖的调用，引擎把**连续的只读调用并发执行**、
        // 写入/执行按声明顺序串行，结果一次全回给模型。默认开 —— 这是省轮次的主通道
        // （一次读 5 个文件从 5 轮降到 1 轮）。batch_max 是单批上限（超了报上限让它拆批，
        // 不静默截断）；batch_parallel 只关"并发读"本身，用于排查并发相关的问题。
        { key: "agent.batch", kind: "toggle" },
        { key: "agent.batch_max", kind: "number" },
        { key: "agent.batch_parallel", kind: "toggle" },
        // v0.8 提问（ask_user）：需求歧义只能问委托人 —— "做一个远程登录功能" 登哪台机器？
        // 四原语组合都取不到这个答案。默认开：关掉等于让模型回去猜（猜错的代价是整体返工）。
        // timeout_secs = 0 表示无限等；超时一律 fail-closed（引擎拒绝依赖它的动作，绝不假设同意）。
      ],
    },
  ];

  const NUMERIC = /^-?\d+(\.\d+)?$/;

  let tab = null;      // 当前渲染的配置标签
  let controls = [];   // 渲染后的控件引用，下标 = row.idx
  let busy = false;    // 保存 / 应用进行中，防重入

  // ============================================
  // 小工具
  // ============================================

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
    if (typeof setStatus === "function") setStatus(msg, kind);
  }

  function schemaField(section, key) {
    const s = SCHEMA.find((x) => x.section === section);
    return s ? s.fields.find((f) => f.key === key) : null;
  }

  /** 未知键的类型猜测：键名敏感词 → 布尔字面量 → 数字 → 文本 */
  function guessKind(key, value) {
    if (/key|secret|token|password/i.test(key)) return "password";
    if (value === "true" || value === "false") return "toggle";
    if (value !== "" && NUMERIC.test(value)) return "number";
    return "text";
  }

  // ============================================
  // 行模型：schema 已知项 ∪ 扫描到的键
  // ============================================

  function buildRows(dump) {
    const byId = new Map();
    for (const e of dump.entries) byId.set(e.section + "." + e.key, e);

    const rows = [];
    const used = new Set();
    const push = (section, key) => {
      const id = section + "." + key;
      if (used.has(id)) return;
      used.add(id);
      const entry = byId.get(id);
      const known = schemaField(section, key);
      const value = entry ? entry.value : "";
      rows.push({
        idx: 0,
        section,
        key,
        fullKey: entry ? entry.full_key : "ruyix.code." + id,
        initial: value,
        inherited: entry ? entry.inherited : null,
        kind: known ? known.kind : guessKind(key, value),
        options: known && known.options ? known.options : null,
        known: !!known,
      });
    };

    for (const s of SCHEMA) for (const f of s.fields) push(s.section, f.key);
    const rest = dump.entries
      .filter((e) => !used.has(e.section + "." + e.key))
      .sort((a, b) => (a.section + "." + a.key).localeCompare(b.section + "." + b.key));
    for (const e of rest) push(e.section, e.key);

    rows.forEach((r, i) => {
      r.idx = i;
    });
    return rows;
  }

  function groupRows(rows) {
    const groups = [];
    for (const r of rows) {
      let g = groups.find((x) => x.section === r.section);
      if (!g) {
        g = { section: r.section, rows: [] };
        groups.push(g);
      }
      g.rows.push(r);
    }
    return groups;
  }

  /** 期望值：优先取本标签暂存的编辑值（切标签页不丢改动），否则取扫描值 */
  function shownValue(row) {
    const stashed = tab && tab._config && tab._config.stash;
    if (stashed && Object.hasOwn(stashed, row.fullKey)) return stashed[row.fullKey];
    return row.initial;
  }

  // ============================================
  // 渲染
  // ============================================

  function renderControl(row, val) {
    const n = row.idx;
    if (row.kind === "toggle") {
      const on = val === "true";
      return `<label class="config-switch"><input type="checkbox" data-row="${n}"` +
        `${on ? " checked" : ""}><span class="config-switch-track"></span></label>`;
    }
    if (row.kind === "select") {
      const options = (row.options || []).slice();
      if (val && !options.includes(val)) options.push(val); // 保留枚举外的既有值
      const opts = options
        .map((o) => `<option value="${esc(o)}"${o === val ? " selected" : ""}>${esc(o)}</option>`)
        .join("");
      return `<select class="config-input config-input--select" data-row="${n}">` +
        `<option value=""${val ? "" : " selected"}>${L("（未设置）", "(unset)")}</option>` +
        `${opts}</select>`;
    }
    const type = row.kind === "password"
      ? "password"
      : row.kind === "number" && (val === "" || NUMERIC.test(val)) ? "number" : "text";
    const input = `<input class="config-input" type="${type}" data-row="${n}" ` +
      `value="${esc(val)}" spellcheck="false" autocomplete="off">`;
    if (row.kind !== "password") return input;
    return `<span class="config-pw">${input}` +
      `<button type="button" class="config-eye" data-eye="${n}" ` +
      `title="${L("显示 / 隐藏", "show / hide")}">👁</button></span>`;
  }

  function renderRow(row) {
    const val = shownValue(row);
    const labelKey = "config.field." + row.section + "." + row.key;
    const descKey = "config.desc." + row.section + "." + row.key;
    const label = row.known ? I18N.t(labelKey) : row.fullKey;
    const desc = row.known ? I18N.t(descKey) : "";
    const hasDesc = hasKey(descKey);
    const inherited = val
      ? ""
      : renderInherited(row);
    return `<div class="config-field${!val && row.inherited ? " config-field--inherited" : ""}">` +
      `<div class="config-field-label">` +
      `<span class="config-field-name" title="${esc(row.fullKey)}">${esc(label)}</span>` +
      (row.known ? "" : `<span class="config-tag">${L("扫描", "scanned")}</span>`) +
      `</div>` +
      `<div class="config-field-body">${renderControl(row, val)}${inherited}` +
      (hasDesc ? `<div class="config-field-desc">${esc(desc)}</div>` : "") +
      `</div></div>`;
  }

  function renderInherited(row) {
    if (!row.inherited) return "";
    const v = row.kind === "password" ? "••••••" : row.inherited.value;
    return `<div class="config-inherit">` +
      `${I18N.t("config.origin_inherit", { scope: row.inherited.scope, value: v })}</div>`;
  }

  function hasKey(key) {
    return I18N.t(key) !== key;
  }

  function render(tabRef) {
    tab = tabRef;
    controls = [];
    const view = $("config-view");
    const body = $("config-body");
    if (!view || !body || !tab) return;

    const conf = tab._config || {};
    $("config-scope-title").textContent = I18N.t("config.tab_" + tab.configScope);
    $("config-dir").textContent = conf.dir || "";
    $("config-hint").textContent = I18N.t("config.hint");

    const groups = groupRows(conf.rows || []);
    body.innerHTML = groups.map((g) => {
      const title = hasKey("config.section." + g.section)
        ? I18N.t("config.section." + g.section)
        : g.section;
      return `<section class="config-section">` +
        `<div class="config-section-head">` +
        `<span class="config-section-name">${esc(title)}</span>` +
        `<span class="config-section-file">${esc(g.section)}.toml</span></div>` +
        `<div class="config-section-rows">${g.rows.map(renderRow).join("")}</div>` +
        `</section>`;
    }).join("");

    controls = Array.from(body.querySelectorAll("[data-row]"));
    // 标记渲染归属：stash / isDirty 只对"这份 DOM 的主人"生效
    view.dataset.configTab = tab.id;
    updateButtons();
  }

  // ============================================
  // 控件读写
  // ============================================

  function readValue(row) {
    const el = controls[row.idx];
    if (!el) return row.initial;
    if (row.kind === "toggle") {
      if (el.checked) return "true";
      // 从未设置过 → 取消勾选即回到"未设置"，不留 false 噪声
      return row.initial === "" ? "" : "false";
    }
    return el.value;
  }

  /** 收集表单值；changesOnly = 只提交改动过的行（保存用）。基准是已提交值（row.initial） */
  function collect(changesOnly) {
    if (!tab || !tab._config || !ownerIs(tab)) return [];
    const out = [];
    for (const row of tab._config.rows) {
      const value = readValue(row);
      if (changesOnly && value === row.initial) continue;
      out.push({ section: row.section, key: row.key, value });
    }
    return out;
  }

  /** 把 DOM 上的编辑值暂存到标签里（切标签页不丢） */
  function stash() {
    if (!tab || !tab._isConfig || tab.id !== window.state?.activeTabId) return;
    if (!tab._config || !controls.length || !ownerIs(tab)) return;
    const map = {};
    for (const row of tab._config.rows) map[row.fullKey] = readValue(row);
    tab._config.stash = map;
    tab._modified = isDirty();
  }

  /** 当前 DOM 是否属于这个标签（防止把 A 表单的值暂存到 B 标签上） */
  function ownerIs(t) {
    const view = $("config-view");
    return !!view && view.dataset.configTab === t.id;
  }

  function isDirty() {
    if (!tab || !tab._config || !ownerIs(tab)) return false;
    return tab._config.rows.some((r) => readValue(r) !== r.initial);
  }

  function updateButtons() {
    const dirty = isDirty();
    const dot = $("config-dirty");
    if (dot) dot.style.display = dirty ? "" : "none";
    const save = $("config-btn-save");
    if (save) save.disabled = !dirty || busy;
    if (tab) tab._modified = dirty;
  }

  // ============================================
  // 后端交互
  // ============================================

  async function load(scope) {
    const invoke = getInvoke();
    if (!invoke) {
      status(I18N.t("status.tauri_unavail"), "error");
      return null;
    }
    try {
      return await invoke("config_form_load", { scope, projectRoot: root() });
    } catch (err) {
      status(String(err), "error");
      return null;
    }
  }

  /** 打开（或复用）配置标签，渲染表单 */
  async function open(scope) {
    if (scope === "project" && !root()) {
      status(I18N.t("config.need_project"), "error");
      return;
    }
    tab = window.state.tabs.find((t) => t._isConfig && t.configScope === scope) || null;
    const dump = await load(scope);
    if (!dump) return;

    // 已开着的标签保留未保存的编辑值（从菜单重开不该吞掉它）
    const stash = tab && tab._config ? tab._config.stash : {};
    if (!tab) {
      tab = {
        id: "config-" + scope,
        name: I18N.t("config.tab_" + scope),
        path: "",
        content: "",
        _isConfig: true,
        configScope: scope,
      };
      window.state.tabs.push(tab);
    }
    tab._config = { dir: dump.dir, rows: buildRows(dump), stash };
    tab._modified = false;

    if (typeof renderTabs === "function") renderTabs();
    if (typeof switchTab === "function") switchTab(tab.id);
    else render(tab);
    status(I18N.t("config.loaded", { dir: dump.dir }));
  }

  /** 保存：只写改动过的行（空值 = 删除该键） */
  async function save() {
    return submit("save");
  }

  /** 应用：保存 + 刷新 IDE 运行时内存里的配置对象 */
  async function apply() {
    return submit("apply");
  }

  async function submit(kind) {
    if (!tab || busy) return;
    const invoke = getInvoke();
    if (!invoke) {
      status(I18N.t("status.tauri_unavail"), "error");
      return;
    }
    // 保存提交增量，应用提交整表（要把值刷进运行时对象）
    const entries = collect(kind === "save");
    if (kind === "save" && entries.length === 0) {
      status(I18N.t("config.no_change"));
      return;
    }

    busy = true;
    updateButtons();
    try {
      const report = await invoke(
        kind === "apply" ? "config_form_apply" : "config_form_save",
        { scope: tab.configScope, entries, projectRoot: root() },
      );
      // 改的是界面语言 / Emoji → 立刻生效（应用与保存都算数：值已落盘）
      await maybeReloadUiConfig(entries);
      await refreshTab();
      status(reportLine(kind, report));
    } catch (err) {
      status(String(err), "error");
    } finally {
      busy = false;
      updateButtons();
    }
  }

  /** 取消：丢弃未保存的改动，重新扫描 */
  async function cancel() {
    if (!tab) return;
    if (isDirty() && typeof showConfirm === "function") {
      const ok = await showConfirm(
        I18N.t("config.cancel_title"),
        I18N.t("config.cancel_msg"),
      );
      if (!ok) return;
    }
    await refreshTab();
    status(I18N.t("config.cancelled"));
  }

  /** 重新扫描并重绘（改完 / 取消后） */
  async function refreshTab() {
    if (!tab) return;
    const dump = await load(tab.configScope);
    if (!dump) return;
    tab._config = { dir: dump.dir, rows: buildRows(dump), stash: {} };
    tab._modified = false;
    render(tab);
  }

  function reportLine(kind, report) {
    const saved = report?.saved ?? 0;
    const removed = report?.removed ?? 0;
    const applied = report?.applied ?? 0;
    if (kind === "apply") return I18N.t("config.applied", { saved, removed, applied });
    return I18N.t("config.saved", { saved, removed });
  }

  /** ui.lang / ui.emoji 改动后热重载界面语言（含 Emoji 叠加） */
  async function maybeReloadUiConfig(entries) {
    const touched = entries.some((e) => e.section === "ui" && (e.key === "lang" || e.key === "emoji"));
    if (!touched || !window.I18N) return;
    try {
      await I18N.init();
      if (typeof refreshI18nUI === "function") refreshI18nUI();
      if (typeof updateLangMenu === "function") updateLangMenu();
    } catch {
      // 语言重载失败不影响配置保存结果
    }
  }

  // ============================================
  // 命令入口（命令系统是前端与后端的唯一桥）
  // ============================================

  async function handleCommand(rest) {
    const tokens = String(rest || "").trim().split(/\s+/);
    const sub = (tokens[0] || "form").toLowerCase();
    const scope = tokens[1];
    if (!SUB_COMMANDS.includes(sub)) {
      status(I18N.t("config.cmd_usage"), "error");
      return;
    }
    if (sub === "form") {
      await open(scope || "global");
      return;
    }
    // save / apply / cancel 作用在当前打开的配置标签上
    if (!tab || (scope && scope !== tab.configScope)) {
      await open(scope || "global");
      return;
    }
    if (sub === "save") await save();
    else if (sub === "apply") await apply();
    else await cancel();
  }

  // ============================================
  // 事件绑定
  // ============================================

  function attach() {
    const view = $("config-view");
    if (!view) return;

    $("config-btn-save")?.addEventListener("click", () => {
      handleCommand("save " + (tab?.configScope ?? ""));
    });
    $("config-btn-apply")?.addEventListener("click", () => {
      handleCommand("apply " + (tab?.configScope ?? ""));
    });
    $("config-btn-cancel")?.addEventListener("click", () => {
      handleCommand("cancel " + (tab?.configScope ?? ""));
    });

    // 输入 / 勾选 / 下拉：刷新脏标记
    view.addEventListener("input", updateButtons);
    view.addEventListener("change", updateButtons);

    // 密码显示 / 隐藏
    view.addEventListener("click", (e) => {
      const btn = e.target.closest("[data-eye]");
      if (!btn) return;
      const input = controls[Number(btn.dataset.eye)];
      if (!input) return;
      input.type = input.type === "password" ? "text" : "password";
    });
  }

  return { attach, handleCommand, render, stash, open, save, apply, cancel, isDirty };
})();
