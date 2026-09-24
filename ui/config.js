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
 * 页面组织：十几个块、几十行，一屏装不下。所以正文只给"块标题条"（点击折叠 / 展开，
 * 默认全折），右侧大纲区渲染同一份**块索引**（标题 + 字段数）：点一下展开并滚过去，
 * 滚正文时高亮当前块。折叠一律走 CSS class —— 重建 DOM 会让 collect() 静默漏掉用户
 * 已改的值（见 applyFold 的注释）。
 *
 * 与其它面板同款防御式写法：无 Tauri（浏览器直开）时显示"后端不可用"。
 */

window.ConfigUI = (() => {
  "use strict";

  const L = (zh, en) => (window.I18N && I18N.getLang() === "en" ? en : zh);
  const SUB_COMMANDS = ["form", "save", "apply", "cancel"];

  // ============================================
  // 已知配置项 schema
  //   ai / ui 两段是**宿主的键空间**（不在引擎 schema 里），键名在这里声明。
  //   harness 段的字段**不在这里写** —— 运行时由后端 `config_schema` 给出，
  //   键名 / 类型 / 默认值 / 枚举取值全部来自引擎自己的声明（见 loadHarnessFields）。
  //   加一个引擎键不用改这个文件，这正是"键表单源化"要买到的东西。
  //
  // 标签 / 说明走 i18n：config.field.<section>.<key> / config.desc.<section>.<key>
  // 这里只声明 section 顺序、字段顺序与控件类型
  // ============================================
  const SCHEMA = [
    {
      section: "ai",
      fields: [
        { key: "api_url", kind: "text" },
        { key: "api_key", kind: "password" },
        // 模型：优先渲染成下拉框，选项来自厂商 `/models`（见 loadModelChoices）。
        // 取不到就退回文本框 —— 让用户手填，也不要给一份可能跑不通的清单。
        { key: "model", kind: "text", dynamic: "models" },
        { key: "alias", kind: "text" },
        // 协议格式：二选一（OpenAI 兼容 / Anthropic Messages）。引擎按它建请求与解析响应。
        { key: "api_format", kind: "select", options: ["openai", "anthropic"] },
        // 工具协议（v0.0.6）：声明 tools 让模型把动作发进 tool_calls —— 这是治"模型自带标记
        // 漏进正文、白烧一轮"的那一步。关掉 = 一行回滚到老协议（动作写在 content 的 JSON 里）。
        { key: "tool_protocol", kind: "select", options: ["true", "false"] },
      ],
    },
    {
      // 备用 LLM（故障切换）：主用不可用时（网络/5xx/429/超时）引擎自动切到它。
      // 字段与 `ai` 段基本同构，两处**有意不同**：
      //   ① 没有 `alias` —— 别名是宿主展示用的字段，而备用 LLM 在界面上没有展示位，
      //      加上去就是个"配了没人读"的死字段（U39 门禁专门守这条）；
      //   ② 模型不做 /models 下拉 —— 那份清单来自主用端点的 `/models`，对备用端点未必成立。
      // `api_format` 允许与主用不同（异构切换：主用 OpenAI、备用 Anthropic）。
      section: "ai_fallback",
      fields: [
        { key: "api_url", kind: "text" },
        { key: "api_key", kind: "password" },
        { key: "model", kind: "text" },
        { key: "api_format", kind: "select", options: ["openai", "anthropic"] },
      ],
    },
    {
      section: "ui",
      fields: [
        { key: "lang", kind: "select", options: ["zh-CN", "en"] },
      ],
    },
  ];

  /** 引擎声明的 harness 字段（运行时拉一次；拉不到就只剩 ai/ui，表单仍可用） */
  let harnessFields = null;

  /** 厂商模型列表（运行时拉一次；拉不到为 null，模型那行退回文本框） */
  let modelChoices = null;

  /** 引擎的 kind → 控件类型 */
  function kindOf(kind) {
    if (kind === "bool") return "toggle";
    if (kind === "int" || kind === "float") return "number";
    return "text";
  }

  /**
   * 从后端取引擎配置 schema。
   *
   * 只取 `ui: true` 的键 —— 哪些键该给用户看是**引擎自己**的判断
   * （`config::FORM_HIDDEN`），前端不维护这份名单，也就不会与引擎脱节。
   */
  async function loadHarnessFields() {
    if (harnessFields) return harnessFields;
    const invoke = getInvoke();
    if (!invoke) return null;
    try {
      const specs = await invoke("config_schema");
      harnessFields = (specs || [])
        .filter((s) => s.ui)
        .map((s) => {
          const path = String(s.path);
          return {
            key: path,
            kind: kindOf(s.kind),
            options: s.options && s.options.length ? s.options : null,
            // 分组用 path 第一段（llm / sandbox / kb …），渲染成子段标题。
            // 无点的顶层键（workspace_root / max_context_chars）不成组 —— 各占一块
            // 只会多出一堆"一块一行"的碎片，归到 general。
            // **注意**：section 仍是 `harness`（落盘键由 section+key 拼出，不能动），
            // 这个 group 只影响显示。
            group: path.includes(".") ? path.split(".")[0] : "general",
            hint: s.default,
          };
        });
      return harnessFields;
    } catch (err) {
      // 取不到 schema 不该让表单不可用：未知键仍会以"扫描"形态出现，照样能编辑
      status(String(err), "error");
      return null;
    }
  }

  /**
   * 拉厂商模型列表（`ai_list_models` → GET /models）。
   *
   * 失败是**常态**（没配 Key / 断网），所以这里不报错、也不缓存失败结果：
   * 返回 null 让模型那行退回文本框，用户手填照样能用。
   */
  async function loadModelChoices() {
    if (modelChoices) return modelChoices;
    const invoke = getInvoke();
    if (!invoke) return null;
    try {
      const ids = await invoke("ai_list_models", { projectRoot: root() });
      if (Array.isArray(ids) && ids.length) modelChoices = ids.slice();
    } catch {
      modelChoices = null;
    }
    return modelChoices;
  }

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
    // known = schema 里的声明（宿主手写段，或引擎 schema 给的字段）；group 只影响分组显示
    const push = (section, key, known, group) => {
      const id = section + "." + key;
      if (used.has(id)) return;
      used.add(id);
      const entry = byId.get(id);
      const value = entry ? entry.value : "";
      rows.push({
        idx: 0,
        section,
        group: group || null,
        key,
        fullKey: entry ? entry.full_key : "ruyix.code." + id,
        initial: value,
        inherited: entry ? entry.inherited : null,
        kind: known ? known.kind : guessKind(key, value),
        options: known && known.options ? known.options : null,
        known: !!known,
      });
    };

    for (const s of SCHEMA) {
      for (const f of s.fields) {
        // 动态选项：只有真拿到了厂商列表才升级成下拉框（`renderControl` 会顺带
        // 把枚举外的既有值补进选项，不会把用户已配的模型吞掉）
        const known =
          f.dynamic === "models" && modelChoices && modelChoices.length
            ? { ...f, kind: "select", options: modelChoices }
            : f;
        push(s.section, f.key, known, null);
      }
    }
    // 引擎声明的 harness 字段：section 恒为 `harness`（落盘键 = section + key），
    // group 取 path 第一段，只影响渲染出来的子段标题。
    for (const f of harnessFields || []) push("harness", f.key, f, f.group);
    const rest = dump.entries
      .filter((e) => !used.has(e.section + "." + e.key))
      .sort((a, b) => (a.section + "." + a.key).localeCompare(b.section + "." + b.key));
    for (const e of rest) push(e.section, e.key, null, null);

    rows.forEach((r, i) => {
      r.idx = i;
    });
    return rows;
  }

  /** 按"段 + 子组"聚类：harness 会拆成 harness.llm / harness.sandbox / … 便于阅读 */
  function groupRows(rows) {
    const groups = [];
    for (const r of rows) {
      const title = r.group ? r.section + "." + r.group : r.section;
      let g = groups.find((x) => x.title === title);
      if (!g) {
        g = { section: r.section, title, rows: [] };
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
  // 折叠 + 大纲区索引
  //
  // 配置页有十几个块、几十行，一屏装不下 —— 正文只给"块标题条"，块里内容按需展开；
  // 右侧大纲区就是这个页面的索引：每块一行（标题 + 字段数），点一下展开并滚过去，
  // 滚正文时高亮当前块。折叠态挂在标签上，切走再回来不丢。
  // ============================================

  /** 块标题条的状态：标题 → true 表示**展开**。空对象 = 全部折叠（首屏即索引） */
  function isOpen(t, title) {
    return !!(t._configOpen && t._configOpen[title]);
  }

  function blockTitle(title) {
    return hasKey("config.section." + title) ? I18N.t("config.section." + title) : title;
  }

  /** 属性选择器里的取值转义（块标题可能是扫描来的任意 section 名） */
  function attrEsc(s) {
    return String(s ?? "").replace(/["\\]/g, "\\$&");
  }

  /**
   * 只切换 class，**绝不重建 innerHTML**。
   *
   * controls 是 render 那一刻 `querySelectorAll("[data-row]")` 抓下来的节点引用，
   * 下标 = row.idx，collect() / isDirty() 全靠它读用户输入。一旦为了"折叠"重建 DOM，
   * 用户改过的值在 collect() 眼里静默变回 row.initial —— 保存时漏提交，且界面看着正常。
   * 所以折叠走 CSS（display:none），节点一个都不动。
   */
  function applyFold(t) {
    if (!t || !t._isConfig || !ownerIs(t)) return;
    const body = $("config-body");
    if (!body) return;
    body.querySelectorAll(".config-section").forEach((sec) => {
      sec.classList.toggle("config-section--folded", !isOpen(t, sec.dataset.section || ""));
    });
  }

  /**
   * 正文字顶端的那个块 —— 滚动高亮用它，不猜、只量。
   *
   * 滚到底要单独判：内容到头时最后一块的顶**永远压不到视口顶部**，照"顶 ≤ 视口顶"去找
   * 会停在倒数第三四块上（用户明明在看最后一块，高亮却指着已经滚出去的那块）。所以
   * 触底就直接给最后一块 —— 这也正是"我在哪"想要的答案。
   */
  function sectionAtTop() {
    const body = $("config-body");
    const nodes = body ? body.querySelectorAll(".config-section") : [];
    if (!nodes.length) return null;
    if (body.scrollTop + body.clientHeight >= body.scrollHeight - 2) {
      return nodes[nodes.length - 1].dataset.section || null;
    }
    const limit = body.getBoundingClientRect().top + 8;
    let cur = nodes[0];
    for (const n of nodes) {
      if (n.getBoundingClientRect().top <= limit) cur = n;
      else break;
    }
    return cur.dataset.section || null;
  }

  /** 高亮当前块（只改 class，不重建 —— 重建会把用户刚点的项换掉、焦点也丢） */
  function syncOutlineActive(t) {
    const el = $("outline-content");
    if (!el || el.dataset.configTab !== t.id) return;
    const title = sectionAtTop();
    el.querySelectorAll(".outline-config-block").forEach((n) => {
      n.classList.toggle("outline-item--active", n.dataset.configSection === title);
    });
  }

  /**
   * 大纲区渲染本配置页的块索引。
   *
   * 双守卫（ownerIs + activeTabId）：大纲区只有一块 DOM，文件大纲与会话任务计划都写它。
   * 后台标签的渲染不许覆盖前台的 —— 少了这道门，切回文件时大纲已经被别的标签悄悄换掉了
   * （同 session.js::renderOutline 的教训）。
   */
  function renderOutline(t) {
    const el = $("outline-content");
    if (!el || !t || !t._isConfig || !t._config) return;
    if (window.state?.activeTabId !== t.id || !ownerIs(t)) return;
    const groups = groupRows(t._config.rows || []);
    if (!groups.length) {
      el.innerHTML = `<div class="outline-placeholder">${I18N.t("config.outline_empty")}</div>`;
      delete el.dataset.configTab;
      return;
    }
    const allOpen = groups.every((g) => isOpen(t, g.title));
    el.dataset.configTab = t.id;
    el.innerHTML =
      `<div class="outline-plan-head outline-config-head">` +
      `<span>${esc(I18N.t("config.outline_head", { n: groups.length }))}</span>` +
      `<button type="button" class="config-outline-all" data-config-fold-all>` +
      `${esc(I18N.t(allOpen ? "config.outline_collapse_all" : "config.outline_expand_all"))}` +
      `</button></div>` +
      groups
        .map((g) => {
          const name = blockTitle(g.title);
          return (
            `<div class="outline-item outline-config-block"` +
            ` data-config-section="${esc(g.title)}" title="${esc(name)}">` +
            `<span class="outline-config-name">${esc(name)}</span>` +
            `<span class="outline-config-count">${g.rows.length}</span></div>`
          );
        })
        .join("");
    syncOutlineActive(t);
  }

  /** 点块标题条：翻转这一块（只改 class，见 applyFold） */
  function toggleSection(sec) {
    if (!tab || !sec) return;
    const title = sec.dataset.section || "";
    tab._configOpen = tab._configOpen || {};
    tab._configOpen[title] = !tab._configOpen[title];
    applyFold(tab);
    renderOutline(tab); // 索引头部的"展开全部 / 全部折叠"文案跟着翻
  }

  /** 索引头部按钮：全开 ↔ 全收 */
  function foldAll() {
    if (!tab || !tab._config) return;
    const groups = groupRows(tab._config.rows || []);
    const allOpen = groups.length > 0 && groups.every((g) => isOpen(tab, g.title));
    tab._configOpen = {};
    if (!allOpen) for (const g of groups) tab._configOpen[g.title] = true;
    applyFold(tab);
    renderOutline(tab);
  }

  /** 点索引里的某一块：展开它，再滚到正文对应位置 */
  function revealBlock(title) {
    if (!tab || !tab._config) return;
    tab._configOpen = tab._configOpen || {};
    tab._configOpen[title] = true;
    // 顺序要紧：**先展开再量位置** —— 折叠着的块高度是 0，那一刻量出来的 rect 是错的
    applyFold(tab);
    const body = $("config-body");
    const sec = body?.querySelector(`.config-section[data-section="${attrEsc(title)}"]`);
    sec?.scrollIntoView({ block: "start" });
    renderOutline(tab);
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

    // 归属标记要**在建 DOM 之前**打：applyFold / renderOutline 都用 ownerIs 判定，
    // 放到函数尾巴会让这一次渲染被自己挡掉（折叠态要等下一次渲染才生效）。
    view.dataset.configTab = tab.id;

    const groups = groupRows(conf.rows || []);
    body.innerHTML = groups.map((g) => {
      const title = blockTitle(g.title);
      // 块标题条 = 折叠开关（点击交由 attach 的委托处理），caret 只在折叠时转个向
      return `<section class="config-section" data-section="${esc(g.title)}">` +
        `<div class="config-section-head" title="${esc(I18N.t("config.fold_tip"))}">` +
        `<span class="config-section-caret">▾</span>` +
        `<span class="config-section-name">${esc(title)}</span>` +
        `<span class="config-section-file">${esc(g.section)}.toml</span></div>` +
        `<div class="config-section-rows">${g.rows.map(renderRow).join("")}</div>` +
        `</section>`;
    }).join("");

    controls = Array.from(body.querySelectorAll("[data-row]"));
    applyFold(tab);
    renderOutline(tab);
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
    // 表单数据与引擎 schema 并行取：schema 拿不到只影响 harness 段（ai/ui 照常）
    const [dump] = await Promise.all([
      load(scope),
      loadHarnessFields(),
      loadModelChoices(),
    ]);
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
      // 改的是界面语言 → 立刻生效（应用与保存都算数：值已落盘）
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

  /** ui.lang 改动后热重载界面语言 */
  async function maybeReloadUiConfig(entries) {
    const touched = entries.some((e) => e.section === "ui" && e.key === "lang");
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

    // 块标题条 = 折叠开关；眼睛 = 密码显隐
    view.addEventListener("click", (e) => {
      const head = e.target.closest(".config-section-head");
      if (head) {
        toggleSection(head.closest(".config-section"));
        return;
      }
      const btn = e.target.closest("[data-eye]");
      if (!btn) return;
      const input = controls[Number(btn.dataset.eye)];
      if (!input) return;
      input.type = input.type === "password" ? "text" : "password";
    });

    // 滚正文 → 索引里的高亮跟着走（passive：这是纯观察，不该拖慢滚动）
    $("config-body")?.addEventListener(
      "scroll",
      () => {
        if (tab && tab._isConfig) syncOutlineActive(tab);
      },
      { passive: true },
    );

    // 索引区：点某一块 → 展开 + 滚过去；点头部按钮 → 全开 / 全收。
    // 用委托（不逐项挂监听）—— renderOutline 每次滚动高亮都可能重写这棵子树。
    $("outline-content")?.addEventListener("click", (e) => {
      if (!tab || !tab._isConfig) return;
      if (e.target.closest("[data-config-fold-all]")) {
        foldAll();
        return;
      }
      const item = e.target.closest("[data-config-section]");
      if (item) revealBlock(item.dataset.configSection || "");
    });
  }

  return {
    attach, handleCommand, render, renderOutline, stash, open, save, apply, cancel, isDirty,
    // 测试/门禁用的内部钩子：折叠态是"改值之后仍然收得到"的前提，值得被断言
    toggleSection, foldAll, revealBlock,
  };
})();
