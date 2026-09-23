#!/usr/bin/env node
/**
 * 会话执行轨迹的**真实布局**探针（几何门禁，需要本机装了 Edge/Chromium）
 *
 * 为什么需要它：ui-smoke 里的 U41 只能证明"代码里写了 nowrap / overflow / text-overflow"，
 * 而用户要的是**看见**一句话 ——「一行显示不全就用省略号」。这是纯几何主张，
 * 在 Node 的微型 DOM 桩里根本量不到（没有布局引擎）。它已经在别处出过事故：
 * 编辑器那对滚动条、光标与字形错位，全是同一类"桩里量不到"的坏。
 *
 * 做法：把**真实的** ui/index.html（剥掉 <script>/<link>）+ ui/styles.css + ui/session.js
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
const sessionSrc = read("ui/session.js");

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
    // 直接挂到 body 并给死宽高：样式表里 .session-* 全是**无祖先限定**的选择器
    // （逐条 grep 过），所以这里量到的就是真机上那套几何。fixed 只是为了出图时
    // 它不会落在应用外壳（#app 占满 100vh）下面看不到 —— 宽高固定，几何不受影响
    wrap.style.position = "fixed";
    wrap.style.left = "0";
    wrap.style.top = "0";
    wrap.style.zIndex = "9999";
    wrap.style.width = "${WIDE}px";
    wrap.style.height = "620px";
    document.body.appendChild(wrap);

    UI.sendMessage(s, wrap, "把会话气泡变成看得见的执行轨迹");
    for (let i = 0; i < 30; i++) await Promise.resolve();

    // 喂真事件：阶段 / 计划 / 两条长调用 / 一条失败 / 收尾
    const LONG = "ui/session.js（905-1145）" + "很长的补充说明".repeat(12);
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

fs.rmSync(dir, { recursive: true, force: true });
if (bad) {
  console.log("session-trace-layout: " + bad + " 项不通过");
  process.exit(1);
}
console.log("session-trace-layout: 全部通过");
