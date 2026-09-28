#!/usr/bin/env node
/**
 * 会话执行轨迹的**真实布局**探针（几何门禁，需要本机装了 Edge/Chromium）
 *
 * 为什么需要它：ui-smoke 里的 U41 只能证明"代码里写了 nowrap / overflow / text-overflow"，
 * 而用户要的是**看见**一句话 ——「一行显示不全就用省略号」。这是纯几何主张，
 * 在 Node 的微型 DOM 桩里根本量不到（没有布局引擎）。它已经在别处出过事故：
 * 编辑器那对滚动条、光标与字形错位，全是同一类"桩里量不到"的坏。
 *
 * 做法：把**真实的** ui/index.html（剥掉 <script>/<link>）+ ui/styles.css + ui/scripts/session.js
 * 拼成一个自包含页面，在无头 Edge 里打开；用真 Tauri 事件桩把会话跑到"跑着呢"那一帧
 * （`agent_reply` 挂着不兑现），再喂几条真事件，最后逐元素量 offset/client/scroll。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 每条轨迹行**只有一行高**（clientHeight ≈ line-height）—— nowrap 生效，没折行；
 *   2. 长行真的**溢出**了（scrollWidth > clientWidth）—— 有东西可省，否则判据 3 是空话；
 *   3. 溢出被省略号收尾：计算样式里 text-overflow:ellipsis + overflow:hidden + nowrap 三件齐；
 *   4. 轨迹块**没有**横向滚动条，也不撑破消息盒 —— 这是"flex 子项要 min-width:0"
 *      与"块要 align-self:stretch 拿容器宽度"两条不变量；
 *   5. 图标与文字在同一行（tag 不是被挤到自己一行上）。
 *   6. **内容面能拖蓝复制**：会话气泡/轨迹/输入框/导航/编辑器的 computed `user-select` 必须是
 *      `text`，而标题栏/状态栏/命令栏必须仍是 `none`（继承来的 `none` 会把内容盖住 ——
 *      用户报的「agent 聊天界面的内容无法选中复制」就是它；不许用"把全局翻成可选"来糊）。
 *
 * 用法：
 *   node scripts/session-trace-layout.js          # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   node scripts/session-trace-layout.js --shot=<file.png>   # 顺带出一张截图，人眼复核
 *   MSEDGE_PATH=<path> node scripts/session-trace-layout.js
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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 轨迹布局探针未运行（几何问题将不被覆盖）");
  process.exit(0);
}

const WIDE = 860; // 探针给聊天面板的宽度（会话 tab 开在中央编辑区，真机大致这个量级）

// ---- 拼页面：真实 HTML + 真实 CSS + 真实 session.js ----
let html = read("ui/index.html")
  .replace(/<script[\s\S]*?<\/script>/g, "")
  .replace(/<link[^>]*>/g, "");
const css = read("ui/styles.css");
const enc = (s) => JSON.stringify(s).replace(/</g, "\\u003c");
const sessionSrc = read("ui/scripts/session.js");

const driver = `
(async function () {
  const out = { errors: [], notes: [] };
  window.onerror = (m) => out.errors.push(String((m && m.message) || m));
  const round = (n) => Math.round(n);
  const info = (el) => {
    const cs = getComputedStyle(el);
    const bx = parseFloat(cs.borderLeftWidth) + parseFloat(cs.borderRightWidth);
    const by = parseFloat(cs.borderTopWidth) + parseFloat(cs.borderBottomWidth);
    return {
      clientW: el.clientWidth, clientH: el.clientHeight,
      scrollW: el.scrollWidth, scrollH: el.scrollHeight,
      // 真画出来的滚动条才占这几像素（offset 含边框与滚动条，client 都不含）
      vBar: round(el.offsetWidth - bx - el.clientWidth),
      hBar: round(el.offsetHeight - by - el.clientHeight),
    };
  };
  try {
    // 真 Tauri 桩：事件回调收下来、invoke 按命令应答（agent_reply 挂着 = 这一轮还在跑）
    const handlers = new Map();
    const invoke = (cmd) => {
      if (cmd === "agent_reply") return new Promise(() => {});
      if (cmd === "agent_session_new") {
        return Promise.resolve({ id: "probe", title: "", created_at: "t", updated_at: "t", messages: [] });
      }
      if (cmd === "agent_session_list") return Promise.resolve([]);
      if (cmd === "agent_env_probe") return Promise.resolve(null);
      return Promise.resolve({});
    };
    window.__TAURI__ = {
      core: { invoke },
      event: { listen: (n, cb) => { handlers.set(n, cb); return Promise.resolve(() => {}); } },
    };
    window.state = { tabs: [], currentProject: { path: "C:/probe" } };
    window.getTauriInvoke = () => invoke;
    window.setStatus = () => {};
    window.renderTabs = () => {};
    window.switchTab = () => {};
    window.closeTab = () => {};

    new Function(${enc(sessionSrc)})();
    const UI = window.SessionUI;
    UI.attach();
    const s = await UI.newSession(false);
    const wrap = UI.ensureChatEl(window.state.tabs[0]);
    // 挂到**真机的位置**：index.html 里 #editor-area > #session-view > #session-container
    // （会话 tab 就开在中央编辑区），并把沿途容器点亮（它们默认 display:none）。
    //
    // 2026-09-28 改：原来直接 document.body.appendChild(wrap)，理由是".session-* 全是
    // 无祖先限定的选择器" —— 那条对**几何**仍然成立，但对**继承属性**不成立：新加的
    // #editor-area { user-select: text } 是祖先限定的，挂 body 上量到的 user-select
    // 就不是真机那个值（判据 6 当场红：msgs=none）。夹具要跟真 DOM 同构，不能只跟几何同构。
    for (const sel of ["#editor-area", "#session-view"]) {
      const el = document.querySelector(sel);
      if (el) el.style.display = "block";
    }
    const host = document.querySelector("#session-container") || document.body;
    // fixed 只为出图时不被应用外壳（#app 占满 100vh）盖住；宽高是写死的，几何不受影响
    wrap.style.position = "fixed";
    wrap.style.left = "0";
    wrap.style.top = "0";
    wrap.style.zIndex = "9999";
    wrap.style.width = "${WIDE}px";
    wrap.style.height = "620px";
    host.appendChild(wrap);

    UI.sendMessage(s, wrap, "把会话气泡变成看得见的执行轨迹");
    for (let i = 0; i < 30; i++) await Promise.resolve();

    // ---- 附件（v1.3）：发送之后待发条必须清空，图必须出现在**历史里那条消息**上 ----
    // 复刻用户报的现场：先往待发条塞一张（1x1 PNG 的 data URL 当预览，不需要 FileReader），
    // 再发一条带图的消息。判据在下面（本文件是模板字符串，注释里不许有反引号）。
    const SHOT = {
      name: "shot.png",
      mime: "image/png",
      data_base64: "iVBORw0KGgo=",
      _preview:
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==",
    };
    s._pending = [SHOT];
    UI.renderPending(wrap, s);
    const pendingEl = wrap.querySelector("[data-pending]");
    out.attach = {
      beforeCount: pendingEl.querySelectorAll(".session-shot-thumb").length,
      beforeHidden: pendingEl.hidden,
    };
    UI.sendMessage(s, wrap, "这图里是什么？（截图附件）", [SHOT]);
    for (let i = 0; i < 30; i++) await Promise.resolve();
    out.attach.afterCount = pendingEl.querySelectorAll(".session-shot-thumb").length;
    out.attach.afterHidden = pendingEl.hidden;
    out.attach.pendingLeft = (s._pending || []).length;
    out.attach.inHistory = wrap.querySelectorAll("[data-msgs] .session-shot-thumb").length;

    // 喂真事件：阶段 / 计划 / 两条长调用 / 一条失败 / 收尾
    const LONG = "ui/scripts/session.js（905-1145）" + "很长的补充说明".repeat(12);
    const fire = (n, p) => handlers.get(n)({ payload: p });
    fire("agent://stage", { stage: "agent", status: "start", detail: "工具循环（最多 40 轮，写入策略：Confirm）" });
    fire("agent://plan", { steps: [{ id: 1, title: "读现有渲染" }, { id: 2, title: "加轨迹" }] });
    fire("agent://log", { level: "info", msg: "[agent] 第 1 轮 read ✓ " + LONG });
    fire("agent://log", { level: "warn", msg: "[agent] 第 2 轮 write ✗ 锚点在文件里出现 2 次，整批作废" });
    fire("agent://log", { level: "ok", msg: "[agent] 完成，共 2 轮" });

    const msgs = wrap.querySelector("[data-msgs]");
    const trace = msgs.querySelector(".session-trace");
    if (!trace) { out.errors.push("DOM 里没有 .session-trace（事件没渲染成轨迹）"); }
    out.msgs = info(msgs);
    out.trace = trace ? info(trace) : null;
    out.msgBox = trace ? info(trace.parentElement) : null;
    out.rows = [];
    let long = null;
    for (const row of msgs.querySelectorAll(".session-trace-row")) {
      const text = row.querySelector(".session-trace-text");
      const tag = row.querySelector(".session-trace-tag");
      const cs = getComputedStyle(text);
      const rec = {
        line: round(parseFloat(cs.lineHeight)),
        h: text.clientHeight,
        clientW: text.clientWidth,
        scrollW: text.scrollWidth,
        textOverflow: cs.textOverflow,
        overflowX: cs.overflowX,
        whiteSpace: cs.whiteSpace,
        title: text.getAttribute("title") ? text.getAttribute("title").length : 0,
        sameRow: Math.abs(tag.getBoundingClientRect().top - text.getBoundingClientRect().top) <= 2,
        head: (text.textContent || "").slice(0, 24),
      };
      out.rows.push(rec);
      // 最长的那条（溢出得最厉害）拿来做省略号判据
      if (!long || rec.scrollW - rec.clientW > long.scrollW - long.clientW) long = rec;
    }
    out.long = long;
    out.expected = { longChars: LONG.length, wide: ${WIDE} };
    // 能不能选中复制（2026-09-28 补）：html, body 上的 user-select:none 是**继承**的，
    // 所以"这条内容能不能被拖蓝复制"完全由 computed user-select 决定 —— 这正是浏览器
    // 判定的那一个值，在 DOM 桩里量不到。内容面必须 text，chrome 必须仍是 none
    // （后者证明我们开的是"内容面"而不是"把全局翻成可选"）。
    // 注意：本文件是模板字符串，注释里**不许出现反引号**（会把模板提前收掉）。
    const usel = (sel) => {
      const el = document.querySelector(sel);
      return el ? getComputedStyle(el).userSelect : "MISSING";
    };
    out.select = {
      msgs: usel("[data-msgs]"),
      bubble: usel("[data-msgs] .session-bubble"),
      traceText: usel("[data-msgs] .session-trace-text"),
      input: usel(".session-input"),
      navigator: usel("#navigator"),
      editor: usel("#editor-area"),
      titlebar: usel("#titlebar"),
      statusbar: usel("#statusbar"),
      commandBar: usel("#command-bar"),
    };
    // 判据 7（2026-09-28 补）：工具栏一排控件**同高**且不撑破。
    // 量的是真浏览器算出来的 offsetHeight：.session-model-select 当初一条样式都没有，
    // 走的是浏览器默认 select 外观 —— 这种「两套盒模型混在一行」在代码里看不出来，只能量。
    // 厂商对象在探针里是空的（模型下拉会显示「（模型未知）」= 很窄），那把宽度判据就量了个假的。
    // 这里塞两条**真实长度**的模型名，量的才是"真机上这个下拉有多宽"（真机里由 ai_vendor 填）。
    const modelEl = wrap.querySelector("[data-model]");
    if (modelEl) {
      modelEl.innerHTML =
        "<option>DeepSeek-V4.1-Flash</option><option>DeepSeek-V3.2-Pro（deepseek-v4-pro）</option>";
    }
    // 判据 9（2026-09-28 换皮时补）：**对比度**。换调色板最容易犯的错是"看着高级、其实看不清"，
    // 而这件事只有浏览器算得出来（computed color + 相对亮度）。量两组不透明对：
    //   ① 状态栏（换皮时从整条蓝改成深灰，最需要盯）；② 消息区底色 vs 气泡文字。
    // 注：气泡自己的底色是半透明的，这里量的是**它坐在哪块底上**（近似，但方向正确）。
    const lum = (rgb) => {
      const m = String(rgb).match(/[0-9.]+/g) || [0, 0, 0];
      const f = (v) => {
        const c = v / 255;
        return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
      };
      return 0.2126 * f(+m[0]) + 0.7152 * f(+m[1]) + 0.0722 * f(+m[2]);
    };
    const ratio = (el, bgEl) => {
      const a1 = lum(getComputedStyle(el).color) + 0.05;
      const b1 = lum(getComputedStyle(bgEl).backgroundColor) + 0.05;
      return Math.round((Math.max(a1, b1) / Math.min(a1, b1)) * 100) / 100;
    };
    const sb = document.querySelector("#statusbar");
    const sbItem = sb && sb.querySelector(".status-item");
    const msgsEl = wrap.querySelector("[data-msgs]");
    const agentBubble = msgsEl && msgsEl.querySelector(".session-msg--agent .session-bubble");
    const userBubble = msgsEl && msgsEl.querySelector(".session-msg--user .session-bubble");
    // 半透明底色要先**合成**再算：气泡的背景是 accent 的 22%，直接拿 rgba 算出来的比值是假的
    // （第一次就差点被这个骗过去 —— 用户气泡是半透明的，量不到就等于没量）。
    const blend = (over, under) => {
      const a1 = String(over).match(/[0-9.]+/g) || [];
      const b1 = String(under).match(/[0-9.]+/g) || [];
      const al = a1.length > 3 ? +a1[3] : 1;
      const mix = [0, 1, 2].map((i) => Math.round(+a1[i] * al + +b1[i] * (1 - al)));
      return "rgb(" + mix.join(",") + ")";
    };
    const ratioOn = (el, underBg) => {
      const cs = getComputedStyle(el);
      const bg = blend(cs.backgroundColor, underBg);
      const a1 = lum(cs.color) + 0.05;
      const b1 = lum(bg) + 0.05;
      return Math.round((Math.max(a1, b1) / Math.min(a1, b1)) * 100) / 100;
    };
    const under = msgsEl ? getComputedStyle(msgsEl).backgroundColor : "rgb(0,0,0)";
    out.contrast = {
      statusbar: sb && sbItem ? ratio(sbItem, sb) : null,
      statusbarBg: sb ? getComputedStyle(sb).backgroundColor : null,
      agent: agentBubble ? ratioOn(agentBubble, under) : null,
      user: userBubble ? ratioOn(userBubble, under) : null,
      msgsBg: msgsEl ? getComputedStyle(msgsEl).backgroundColor : null,
    };
    const tb = wrap.querySelector(".session-toolbar");
    out.toolbar = tb
      ? {
          kids: Array.from(tb.children)
            .filter((el) => el.offsetHeight)
            .map((el) => ({ cls: el.className, h: el.offsetHeight, w: el.offsetWidth })),
          clientW: tb.clientWidth,
          scrollW: tb.scrollWidth,
        }
      : null;
  } catch (err) {
    out.errors.push("DRIVER: " + ((err && err.stack) || err));
  }
  const pre = document.createElement("pre");
  pre.id = "__PROBE__";
  pre.textContent = JSON.stringify(out).replace(/&/g, "&#38;").replace(/</g, "&#60;");
  document.body.appendChild(pre);
})();
`;

html = html.replace(
  /<\/body>/,
  () => `<style>${css}</style><script>${driver}</script></body>`
);
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-trace-layout-"));
const page = path.join(dir, "probe.html");
fs.writeFileSync(page, html, "utf8");

// --shot=<file>：同一页再跑一次出图（几何判据照旧吃上面那次的 --dump-dom 结果），
// 目的是让人能**看见**"一行一条 + 省略号"，而不是只信探针的结论
const shotArg = process.argv.slice(2).find((a) => a.startsWith("--shot="));
let shotPath = null;
if (shotArg) {
  const want = shotArg.split("=")[1];
  shotPath = path.isAbsolute(want) ? want : path.resolve(process.cwd(), want);
  cp.spawnSync(
    browser,
    [
      "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
      "--user-data-dir=" + path.join(dir, "profile-shot"),
      "--window-size=1080,700", "--force-device-scale-factor=1",
      "--virtual-time-budget=15000", "--hide-scrollbars",
      "--screenshot=" + shotPath,
      "file:///" + page.replace(/\\/g, "/"),
    ],
    { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }
  );
  console.log(fs.existsSync(shotPath)
    ? `  截图: ${shotPath}`
    : `FAIL: 截图没生成（--screenshot 没落地）`);
}

const cpRes = cp.spawnSync(
  browser,
  [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + path.join(dir, "profile"),
    "--window-size=1080,700", "--force-device-scale-factor=1",
    "--virtual-time-budget=15000", "--dump-dom",
    "file:///" + page.replace(/\\/g, "/"),
  ],
  { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 }
);

const dom = cpRes.stdout || "";
const m = dom.match(/<pre id="__PROBE__">([\s\S]*?)<\/pre>/);
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
if (out.errors.length) {
  fs.rmSync(dir, { recursive: true, force: true });
  console.log("session-trace-layout: " + bad + " 项不通过");
  process.exit(1);
}

const rows = out.rows || [];
console.log(`轨迹行 ${rows.length} 条｜消息盒宽 ${out.msgBox && out.msgBox.clientW}px｜` +
  `轨迹块宽 ${out.trace && out.trace.clientW}px｜面板宽 ${out.msgs.clientW}px`);

rows.length >= 5
  ? ok(`事件渲染出 ${rows.length} 条轨迹行`)
  : fail(`轨迹行数应 ≥ 5（阶段/计划/两条调用/失败/收尾），实际 ${rows.length}`);

// 判据 1：一行一条（折行就是"一行"没做到）
const wrapped = rows.filter((r) => r.h > r.line + 2);
wrapped.length === 0
  ? ok(`每条都只有一行高（${rows[0] ? rows[0].h : "-"}px ≈ 行高 ${rows[0] ? rows[0].line : "-"}px）`)
  : fail(`有 ${wrapped.length} 条折成了多行（clientHeight > line-height）：` +
      wrapped.map((r) => `"${r.head}…" ${r.h}px>${r.line}px`).join("；") +
      " —— nowrap 没生效");

// 判据 2 + 3：真的溢出，且被省略号收尾
const long = out.long || {};
long.scrollW > long.clientW
  ? ok(`长行真的溢出了（scrollWidth ${long.scrollW} > clientWidth ${long.clientW}，` +
      `原文 ${out.expected.longChars} 字）`)
  : fail(`长行没有溢出（scrollWidth ${long.scrollW} <= clientWidth ${long.clientW}）：` +
      "要么盒子被内容撑宽了（min-width:0 缺失），要么文本压根没被约束 —— 这条不成立，" +
      "下面那条省略号的判据就是空话");
long.textOverflow === "ellipsis" && long.overflowX === "hidden" && long.whiteSpace === "nowrap"
  ? ok("溢出由省略号收尾（nowrap + overflow:hidden + text-overflow:ellipsis 三件齐）")
  : fail(`溢出没被省略号收尾：text-overflow=${long.textOverflow} overflow-x=${long.overflowX} ` +
      `white-space=${long.whiteSpace}`);
rows.every((r) => r.title > 0)
  ? ok("每条长行都把全文留在 title 里（hover 能看全）")
  : fail("有行的 title 是空的：省略号收尾后全文就再也拿不到了");

// 判据 4：不撑破、不出横向滚动条
(out.trace && out.trace.hBar === 0 && out.msgs.hBar === 0)
  ? ok("轨迹块与消息区都没有横向滚动条")
  : fail(`出现横向滚动条：轨迹块 hBar=${out.trace && out.trace.hBar} 消息区 hBar=${out.msgs.hBar}` +
      " —— 行宽没被约束住（flex 子项的 min-width:0 / 块的 align-self:stretch）");
(out.msgBox && out.msgBox.clientW >= out.msgs.clientW * 0.6)
  ? ok(`轨迹块拿到了容器宽度（消息盒 ${out.msgBox.clientW}px vs 面板 ${out.msgs.clientW}px）`)
  : fail(`轨迹块的宽度靠内容撑（消息盒 ${out.msgBox && out.msgBox.clientW}px，` +
      `面板 ${out.msgs.clientW}px）：短句会被缩成一条窄条，长句又把盒子撑破`);

// 判据 5：图标与文字同行
rows.every((r) => r.sameRow)
  ? ok("图标与文字在同一行")
  : fail("有行的图标被挤到了自己一行上");

// 判据 6：**内容面能被拖蓝复制**，而 chrome 仍旧禁选。
// `html, body { user-select: none }` 是**继承**的 ⇒ 只给编辑器三件开选择时，会话气泡、
// 轨迹、差异面板全选不中（用户报的「agent 聊天界面的内容无法选中复制」）。
// 这里量的是浏览器自己用的那个值（computed `user-select`），不是"代码里写了没写"。
const sel = out.select || {};
const wantText = ["msgs", "bubble", "traceText", "input", "navigator", "editor"];
const wantNone = ["titlebar", "statusbar", "commandBar"];
const notText = wantText.filter((k) => sel[k] !== "text");
const notNone = wantNone.filter((k) => sel[k] !== "none");
notText.length === 0
  ? ok(`内容面可选（${wantText.join(" / ")} 的 user-select 都是 text）`)
  : fail(`还是选不中：${notText.map((k) => k + "=" + sel[k]).join(" / ")} —— ` +
      "会话/编辑器容器上没有 user-select:text（继承下来的 none 把它盖住了）");
notNone.length === 0
  ? ok("chrome 仍旧禁选（标题栏 / 状态栏 / 命令栏 user-select:none）")
  : fail(`chrome 被放开成可选：${notNone.map((k) => k + "=" + sel[k]).join(" / ")} —— ` +
      "那是把全局翻成可选，不是给内容面开选择");

// 判据 7：工具栏**同高**，且不撑破（横向滚动条 / 换行都是"没做完"的样子）。
const tbm = out.toolbar;
const hs = tbm ? [...new Set(tbm.kids.map((k) => k.h))] : [];
tbm && hs.length === 1
  ? ok(`工具栏一排控件同高（${hs[0]}px × ${tbm.kids.length} 个）`)
  : fail(`工具栏高度不齐：${tbm ? tbm.kids.map((k) => k.cls + "=" + k.h + "px").join(" / ") : "没量到 .session-toolbar"}` +
      " —— 下拉与按钮走了两套盒模型（高度/圆角/字号要在同一段里定死）");
tbm && tbm.scrollW <= tbm.clientW + 1
  ? ok(`工具栏没撑破（scrollWidth ${tbm.scrollW} ≤ clientWidth ${tbm.clientW}）`)
  : fail(`工具栏撑破了（scrollWidth ${tbm ? tbm.scrollW : "?"} > clientWidth ${tbm ? tbm.clientW : "?"}）` +
      " —— 这排控件的宽度和超出了容器（下拉要有 max-width，按钮组要靠右）");
// 模型下拉**不许吃掉半行**：select 的固有宽度按最长那条 option 算，模型名带厂商前缀本来就长，
// 不封顶它会一路撑到 600px、把三颗按钮挤到边上（用户报「模型下拉太宽」就是这一条）。
const msel = tbm && tbm.kids.find((k) => String(k.cls).indexOf("session-model-select") >= 0);
const mcap = tbm ? Math.min(260, Math.round(tbm.clientW * 0.45)) : 0;
msel && msel.w <= mcap
  ? ok(`模型下拉贴着内容走（${msel.w}px ≤ 上限 ${mcap}px）`)
  : fail(`模型下拉太宽（${msel ? msel.w : "?"}px > 上限 ${mcap}px，工具栏宽 ${tbm ? tbm.clientW : "?"}px）` +
      " —— 给它 max-width（现在 15rem）并靠右成组放那三颗按钮");
tbm ? ok(`工具栏各控件宽度：${tbm.kids.map((k) => k.cls + "=" + k.w + "px").join(" / ")}`) : null;

// 判据 8：截图附件的三件事（用户报过"图发出去之后输入区还挂着、历史里又找不到"）。
const at = out.attach || {};
at.beforeCount === 1 && at.beforeHidden === false
  ? ok("待发条能显示待发的图（1 张）")
  : fail(`待发条没显示图（${at.beforeCount} 张 / hidden=${at.beforeHidden}）—— 夹具或 renderPending 坏了`);
at.afterHidden === true && at.pendingLeft === 0 && at.afterCount === 0
  ? ok("发送之后待发条清空（输入区不再挂着那张图）")
  : fail(`发送之后待发条没清空（hidden=${at.afterHidden} / 缩略图 ${at.afterCount} 张 / s._pending=${at.pendingLeft}）` +
      " —— 图要跟着消息走，不能留在输入区（用户报的就是这个）");
at.inHistory >= 1
  ? ok(`图进了聊天历史（消息区 ${at.inHistory} 张缩略图）`)
  : fail("图没进聊天历史（消息区 0 张缩略图）—— 发送时要把图挂到那条用户消息上");

// 判据 9：对比度（WCAG 正文 4.5:1）。换皮不许"好看了但看不清"。
const ct = out.contrast || {};
const pairs = [
  ["状态栏文字", ct.statusbar, 4.5],
  ["助手气泡文字", ct.agent, 4.5],
  ["用户气泡文字", ct.user, 4.5],
];
for (const [name, r, min] of pairs) {
  r === null || typeof r === "undefined"
    ? fail(`${name}：量不到（元素或样式变了？）`)
    : r >= min
      ? ok(`${name}对比度 ${r}:1 ≥ ${min}:1`)
      : fail(`${name}对比度只有 ${r}:1（要 ≥ ${min}:1）—— 灰阶挑得太近，深色主题更容易犯这个错`);
}

fs.rmSync(dir, { recursive: true, force: true });
if (bad) {
  console.log("session-trace-layout: " + bad + " 项不通过");
  process.exit(1);
}
console.log("session-trace-layout: 全部通过");
