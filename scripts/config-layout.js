#!/usr/bin/env node
/**
 * 配置文件页**真实布局与交互**探针（几何门禁，需要本机装了 Edge/Chromium）
 *
 * 为什么需要它：ui-smoke 的配置回放跑在 Node 的微型 DOM 桩上 —— 没有布局引擎，
 * 也没有真实事件分发。于是"折叠之后到底还占不占高度""点了索引有没有滚过去"
 * 这类问题在桩里**根本量不到**；而这一次改动里最危险的一条恰好也是这类：
 * 折叠如果图省事去重建 innerHTML，controls（render 那刻抓的节点引用）就失效了，
 * 用户改过的值在 collect() 眼里静默变回初始值 —— **保存会漏提交，界面却完全正常**。
 * 静态门禁守不住这个（它只能守住"别写 innerHTML"），所以必须真跑一遍。
 *
 * 做法：把**真实的** ui/index.html（剥掉 <script>/<link>）+ ui/styles.css +
 * ui/config.js + 真实的 zh-CN.json 拼成一个自包含页面，在无头 Edge 里喂一份
 * 接近真机的配置 schema（25 个键 → 十几个块），然后：
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 默认全折叠：每块的 .config-section-rows 计算高度为 0，且 display:none；
 *   2. 真的变短了：全展开后的 scrollHeight 至少是默认状态的两倍；
 *   3. 索引完整：索引项数 == 正文块数，且块标题是**可读名字**（不是 harness.llm）；
 *   4. 点索引第 N 项 → 该块展开，且它落进了正文可视区（"跳过去了"而不是"没反应"）；
 *   5. **改值 → 折叠 → 值还在**：isDirty 仍为真、节点还在 DOM 里，且保存提交的条目里
 *      确实是改后的值（这条是本次改动的核心风险）；
 *   6. 滚到底 → 高亮项变成最后一块，且高亮**只有一个**。
 *
 * 用法：
 *   node scripts/config-layout.js          # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   MSEDGE_PATH=<path> node scripts/config-layout.js
 */
const fs = require("fs");
const os = require("os");
const path = require("path");
const cp = require("child_process");

const ROOT = path.resolve(__dirname, "..");
const read = (p) => fs.readFileSync(path.join(ROOT, p), "utf8");

function findBrowser() {
  if (process.env.MSEDGE_PATH) return process.env.MSEDGE_PATH;
  const cands = [
    "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
    "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
    "/usr/bin/microsoft-edge",
    "/usr/bin/google-chrome",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  ];
  return cands.find((p) => fs.existsSync(p)) || null;
}

const browser = findBrowser();
if (!browser) {
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 配置页布局探针未运行（折叠几何将不被覆盖）");
  process.exit(0);
}

// ---- 拼页面：真实 HTML + 真实 CSS，config.js 与语言包留给 driver 自己注入 ----
let html = read("ui/index.html")
  .replace(/<script[\s\S]*?<\/script>/g, "")
  .replace(/<link[^>]*>/g, "");
const css = read("ui/styles.css");
const enc = (s) => JSON.stringify(s).replace(/</g, "\\u003c");
const cfgSrc = read("ui/config.js");
// 注意是 **JSON.parse 之后**再嵌：直接把文件文本嵌进去，页面里拿到的是一个字符串，
// 查键永远落空 → 所有文案静默退化成原始键名，探针却"看起来在跑"。
const zh = JSON.parse(read("ui/lang/zh-CN.json"));

const driver = `
(async function () {
  // 导成预览页给人看时：别把探针的 JSON 结果糊在页面底下，也别让"打开文件以开始编辑"
  // 那行空态文字留在配置页上方（真机上 showConfigView() 会把它藏掉，这里没有 main.js）
  const PREVIEW = ${process.env.CONFIG_PREVIEW ? "true" : "false"};
  const out = { errors: [], notes: [], steps: {} };
  window.onerror = (m) => out.errors.push(String((m && m.message) || m));
  const box = (el) => {
    const r = el.getBoundingClientRect();
    return { top: Math.round(r.top), h: Math.round(r.height) };
  };
  try {
    const zh = ${enc(zh)};
    window.I18N = {
      getLang: () => "zh-CN",
      t(key, params) {
        let s = Object.prototype.hasOwnProperty.call(zh, key) ? zh[key] : key;
        for (const [k, v] of Object.entries(params || {})) {
          s = s.replaceAll("{" + k + "}", String(v));
        }
        return s;
      },
    };

    // 贴近真机的一份 schema：25 个键 → 十几个块（少了就测不出"太长"这件事）
    const schema = [];
    const add = (p, kind, dft) => schema.push(
      { path: p, kind: kind, default: dft, ui: true, options: [] });
    add("workspace_root", "text", "C:/x/runs");
    add("max_context_chars", "int", "60000");
    add("llm.temperature", "float", "0.2");
    add("llm.max_tokens", "int", "8192");
    add("verify.enabled", "bool", "true");
    add("verify.max_rounds", "int", "3");
    add("gate.enabled", "bool", "true");
    add("reflect.enabled", "bool", "true");
    add("agent.max_elapsed_secs", "int", "1800");
    add("agent.history_trim", "bool", "true");
    add("agent.history_keep_rounds", "int", "6");
    add("step.execute_plan", "bool", "true");
    add("step.max_steps", "int", "96");
    add("lint.enabled", "bool", "true");
    add("lint.max_repair_rounds", "int", "3");
    add("sandbox.mode", "text", "require");
    add("sandbox.image", "text", "rust:1");
    add("kb.enabled", "bool", "false");
    add("kb.top_k", "int", "4");
    add("discover.extra", "list", "");
    add("env.enabled", "bool", "true");
    add("proc.enabled", "bool", "true");
    add("proc.max", "int", "4");
    add("ask.enabled", "bool", "true");
    add("ask.timeout_secs", "int", "300");

    const dump = {
      scope: "global",
      dir: "C:\\\\Users\\\\probe\\\\.ruyix\\\\code",
      entries: [
        { section: "ai", key: "model", full_key: "ruyix.code.ai.model",
          value: "deepseek-chat", inherited: null },
      ],
    };
    const seen = { save: null };
    window.__probe = { seen: seen };
    window.getTauriInvoke = () => async (cmd, args) => {
      if (cmd === "config_form_load") return dump;
      if (cmd === "config_schema") return schema;
      if (cmd === "ai_list_models") return [];
      if (cmd === "config_form_save") { seen.save = args; return { saved: args.entries.length, removed: 0, applied: 0 }; }
      if (cmd === "config_form_apply") { return { saved: args.entries.length, removed: 0, applied: args.entries.length }; }
      return {};
    };
    const appState = { tabs: [], activeTabId: null, currentProject: { path: "C:/proj" } };
    window.state = appState;
    window.setStatus = () => {};
    window.renderTabs = () => {};
    window.showConfirm = async () => true;
    window.switchTab = (id) => {
      appState.activeTabId = id;
      const t = appState.tabs.find((x) => x.id === id);
      if (t && t._isConfig) window.ConfigUI.render(t);
    };

    new Function(${enc(cfgSrc)})();
    const ConfigUI = window.ConfigUI;
    out.hasApi = ["render", "renderOutline", "foldAll", "revealBlock", "toggleSection"]
      .every((f) => typeof ConfigUI[f] === "function");

    // 祖先里若有 display:none（无项目时的初始态）就地打开；配置视图自身是 inline none
    for (let el = document.getElementById("config-view"); el && el !== document.body;
         el = el.parentElement) {
      if (getComputedStyle(el).display === "none") {
        el.style.display = el.classList.contains("editor-body") ? "flex" : "block";
        out.notes.push("强制显示祖先 " + (el.id || el.className));
      }
    }
    document.getElementById("config-view").style.display = "";
    const empty = document.getElementById("editor-empty");
    if (empty) empty.style.display = "none"; // showConfigView() 在真机上干的事
    ConfigUI.attach();
    await ConfigUI.open("global");

    const body = document.getElementById("config-body");
    const oc = document.getElementById("outline-content");
    const secs = Array.from(body.querySelectorAll(".config-section"));
    const rowsH = (s) => Math.round(s.querySelector(".config-section-rows").getBoundingClientRect().height);
    const tocSel = "#outline-content .outline-config-block";
    const toc = () => Array.from(document.querySelectorAll(tocSel));

    out.blockCount = secs.length;
    out.tocCount = toc().length;
    out.defH = body.scrollHeight;
    out.defRowsH = secs.map(rowsH);
    out.defDisplays = secs.map((s) =>
      getComputedStyle(s.querySelector(".config-section-rows")).display);
    out.names = secs.slice(0, 3).map((s) =>
      s.querySelector(".config-section-name").textContent.trim());
    out.tocNames = toc().slice(0, 3).map((n) => n.textContent.replace(/\\s+/g, " ").trim());
    out.bodyVisibleH = body.clientHeight;
    // 诊断量：字段行高度受"说明文字换几行"影响，而换行又受可用宽度影响 ——
    // 数字对不上时先看这几个，别对着总数猜。
    const dcs = body.querySelectorAll(".config-field-desc");
    out.diag = {
      bodyW: body.clientWidth,
      fields: body.querySelectorAll(".config-field").length,
      descs: dcs.length,
      firstFieldH: Math.round(body.querySelector(".config-field").getBoundingClientRect().height),
      firstDescH: dcs.length ? Math.round(dcs[0].getBoundingClientRect().height) : -1,
      preview: PREVIEW,
    };

    // 全展开 → 量"到底有多长"（判据 2 的分母）
    ConfigUI.foldAll();
    out.allH = body.scrollHeight;
    out.allRowsH = secs.slice(0, 3).map(rowsH);

    // 判据 4：点索引第 3 项，应该展开它并滚进可视区
    const target = toc()[2];
    out.clickTarget = target ? target.dataset.configSection : null;
    target.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    const tSec = secs.find((s) => s.dataset.section === out.clickTarget);
    out.jump = {
      rowsH: tSec ? rowsH(tSec) : -1,
      secTop: tSec ? box(tSec).top : null,
      bodyTop: box(body).top,
      bodyBottom: box(body).top + body.clientHeight,
    };

    // 判据 5：改一个值 → 折叠它 → 值必须还在，且保存要真的提交它
    const firstInput = body.querySelector("input.config-input");
    firstInput.value = "PROBE-VALUE";
    firstInput.dispatchEvent(new Event("input", { bubbles: true }));
    const dirtyBefore = ConfigUI.isDirty();
    const nodeBefore = firstInput;
    // 折叠它所在的块（点标题条）
    const hostSec = firstInput.closest(".config-section");
    hostSec.querySelector(".config-section-head")
      .dispatchEvent(new MouseEvent("click", { bubbles: true }));
    const stillThere = body.querySelector('input.config-input[data-row="' +
      firstInput.dataset.row + '"]');
    const dirtyAfter = ConfigUI.isDirty();
    await ConfigUI.save();
    const savedEntries = (window.__probe.seen.save || { entries: [] }).entries;
    out.collapse = {
      dirtyBefore: dirtyBefore,
      dirtyAfterFold: dirtyAfter,
      sameNode: stillThere === nodeBefore,
      valueKept: stillThere ? stillThere.value : null,
      rowsHAfterFold: rowsH(hostSec),
      savedProbeValue: savedEntries.some((e) => e.value === "PROBE-VALUE"),
      savedCount: savedEntries.length,
    };

    // 判据 6：滚到底 → 高亮最后一块，且只有一个
    ConfigUI.foldAll(); // 保存后重扫过，先全展开（内容够长才有得滚）
    body.scrollTop = body.scrollHeight;
    body.dispatchEvent(new Event("scroll"));
    const act = toc().filter((n) => n.classList.contains("outline-item--active"));
    out.spy = {
      activeCount: act.length,
      activeTitle: act.length ? act[0].dataset.configSection : null,
      lastTitle: secs.length ? secs[secs.length - 1].dataset.section : null,
      scrollTop: Math.round(body.scrollTop),
    };
  } catch (err) {
    out.errors.push("DRIVER: " + ((err && err.stack) || err));
  }
  // 收尾：把页面恢复成"刚打开"的样子（全折叠 + 滚回顶部）。探针的输出被导成预览页给人看时，
  // 看到的该是真实默认态，而不是被测试翻腾过的一堆展开块。
  try {
    window.ConfigUI.foldAll();
    document.getElementById("config-body").scrollTop = 0;
  } catch (e2) {
    /* 上面已经报错了就别再添一条 */
  }
  const pre = document.createElement("pre");
  pre.id = "__PROBE__";
  if (PREVIEW) pre.style.display = "none"; // 导出预览时别把结果糊在页面底下
  pre.textContent = JSON.stringify(out).replace(/&/g, "&#38;").replace(/</g, "&#60;");
  document.body.appendChild(pre);
})();
`;

html = html.replace(/<\/body>/, () => `<style>${css}</style><script>${driver}</script></body>`);
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-config-layout-"));
const page = path.join(dir, "probe.html");
fs.writeFileSync(page, html, "utf8");

const cpRes = cp.spawnSync(
  browser,
  [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + path.join(dir, "profile"),
    "--window-size=1080,660", "--force-device-scale-factor=1",
    "--virtual-time-budget=15000", "--dump-dom",
    "file:///" + page.replace(/\\/g, "/"),
  ],
  { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 }
);

const dom = cpRes.stdout || "";
// `[^>]*` 不能省：导出预览时会给这个 <pre> 加 style="display:none"，
// 写成 `<pre id="__PROBE__">` 就再也匹配不上（表现为"脚本没跑到底"，白查一轮）
const m = dom.match(/<pre id="__PROBE__"[^>]*>([\s\S]*?)<\/pre>/);
if (!m) {
  console.log("FAIL: 探针页面没有产出结果（脚本没跑到底）");
  const errs = (cpRes.stderr || "").split("\n").filter(Boolean).slice(0, 3);
  console.log("  stderr: " + errs.join(" ⏐ "));
  process.exit(1);
}
const out = JSON.parse(m[1].replace(/&#60;/g, "<").replace(/&#38;/g, "&"));

let bad = 0;
const fail = (msg) => {
  console.log("FAIL: " + msg);
  bad++;
};
const ok = (msg) => console.log("  ok  " + msg);

for (const e of out.errors) fail("页面报错: " + e);
if (out.notes.length) console.log("  note " + out.notes.join("; "));
if (!out.hasApi) fail("config.js 没有暴露折叠 / 索引所需的接口");

console.log("块数 " + out.blockCount + "（索引 " + out.tocCount + " 项）· 正文可视高 " +
  out.bodyVisibleH + "px");
console.log("诊断 " + JSON.stringify(out.diag));
console.log("默认 scrollHeight " + out.defH + "px → 全展开 " + out.allH + "px");
console.log("块名: " + (out.names || []).join(" / "));

const zeros = (out.defRowsH || []).every((h) => h === 0);
const noneAll = (out.defDisplays || []).every((d) => d === "none");
zeros && noneAll
  ? ok("默认全折叠：每块的 rows 高度为 0 且 display:none（节点还在 DOM 里，collect 读得到）")
  : fail("默认不是全折叠：rows 高度 " + JSON.stringify(out.defRowsH) +
      " / display " + JSON.stringify(out.defDisplays));

out.allH > out.defH * 2
  ? ok("折叠真的把页面变短了（" + out.defH + "px vs 全展开 " + out.allH + "px）")
  : fail("折叠后没明显变短（" + out.defH + "px vs " + out.allH +
      "px）：默认展开的话这一页还是会要滚三屏");

out.tocCount > 1 && out.tocCount === out.blockCount
  ? ok("索引完整：索引项数 == 正文块数（" + out.tocCount + "）")
  : fail("索引与正文对不上：索引 " + out.tocCount + " 项 vs 正文 " + out.blockCount + " 块");

const rawKeyNames = [...(out.names || []), ...(out.tocNames || [])].filter(
  (n) => /^(harness|llm|sandbox|kb|step|agent|proc|ask)\./.test(n));
rawKeyNames.length === 0
  ? ok("块标题是可读名字（没有退回 harness.llm 这类原始键名）")
  : fail("块标题退回了原始键名: " + rawKeyNames.join(", "));

const j = out.jump || {};
j.rowsH > 0
  ? ok("点索引后该块展开了（rows 高 " + j.rowsH + "px）")
  : fail("点了索引但块没展开：rows 高 " + j.rowsH);
j.secTop !== null && j.secTop >= j.bodyTop - 2 && j.secTop < j.bodyBottom
  ? ok("该块落进了正文可视区（top " + j.secTop + " ∈ [" + j.bodyTop + ", " + j.bodyBottom + ")）")
  : fail("点了索引但块没滚进可视区（top " + j.secTop + " 不在 [" + j.bodyTop +
      ", " + j.bodyBottom + ")）：跳过去才叫索引");

const c = out.collapse || {};
c.dirtyBefore === true
  ? ok("改动后标脏")
  : fail("改了值却没标脏（控件没接上 input 委托？）");
c.sameNode === true && c.valueKept === "PROBE-VALUE"
  ? ok("折叠没有重建 DOM：控件还是同一个节点、值还在")
  : fail("折叠把 DOM 重建了（同节点 " + c.sameNode + " / 值 " + JSON.stringify(c.valueKept) +
      "）：controls 引用失效，用户改过的值会被静默漏提交");
c.dirtyAfterFold === true
  ? ok("折叠后仍然是脏的（isDirty 没被折叠清掉）")
  : fail("折叠之后 isDirty 变假了：值收集已经丢了");
c.savedProbeValue === true
  ? ok("保存提交的条目里确实带着改后的值（" + c.savedCount + " 行）")
  : fail("保存没有提交改后的值（" + c.savedCount + " 行）：这就是「折叠导致漏提交」的真实形态");

const s = out.spy || {};
s.activeCount === 1
  ? ok("高亮只有一个（不会两处同时亮）")
  : fail("高亮项数 " + s.activeCount + "（应恰好 1）");
s.activeTitle && s.activeTitle === s.lastTitle
  ? ok("滚到底时高亮的就是最后一块（" + s.activeTitle + "）")
  : fail("滚到底时高亮的是 " + s.activeTitle + "，期望 " + s.lastTitle);

// 想把真效果导出来给人看：CONFIG_PREVIEW=<路径> 会留下拼好的自包含页面
// （真实 index.html + styles.css + config.js + zh-CN.json，只有配置数据是假的）。
// 单文件、无外链，双击就能在浏览器里点着看。
if (process.env.CONFIG_PREVIEW) {
  const dest = path.resolve(process.env.CONFIG_PREVIEW);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.copyFileSync(page, dest);
  console.log("预览页已写出: " + dest);
}
fs.rmSync(dir, { recursive: true, force: true });
if (bad) {
  console.log("config-layout: " + bad + " 项不通过");
  process.exit(1);
}
console.log("config-layout: 全部通过");
