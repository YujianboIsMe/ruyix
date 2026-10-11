#!/usr/bin/env node
/**
 * 右键菜单**分隔线**的真实几何探针（需要本机装了 Edge/Chromium，没有就 SKIP）
 *
 * 为什么需要它：用户报过两次同一件事 ——「bars are too bold」+「there must be only 1 bar」。
 * 成因是**两件事叠在一起**，而两件都只在真浏览器里才看得见：
 *   ① 分隔线的 div 同时挂着 `context-menu-item` 与 `context-menu-sep` ⇒ 吃到菜单项的
 *      `padding: 6px 16px`，而背景铺满**内边距盒** ⇒ 1px 的线变成 13px 的粗灰条；
 *   ② 分隔线在 HTML 里是**静态写死**的，条目却按上下文隐藏（文件 vs 文件夹、项目根 vs 子目录）
 *      ⇒ 中间两个条目一藏，两条分隔线就贴到一起 ⇒ 菜单里出现两根并列的灰条。
 * 光看代码都"看着是对的"（CSS 里明明写着 height:1px），只能量。
 *
 * 做法：把**真实的** ui/index.html（剥掉 <script>）+ ui/styles.css + ui/scripts/contextmenu.js
 * 拼成一个自包含页面，在无头 Edge 里打开，直接调宿主导出的 `ContextMenuUI.showContextMenu`
 * （文件夹一次、文件一次），再逐个量菜单子元素的 offsetHeight / padding / 背景。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 每一条分隔线都是**发丝线**：computed `height:1px` + 上下 `padding:0`（两张菜单都查 ——
 *      标签菜单那张不显示，所以查 computed 而不是 offsetHeight）；
 *   2. 每张菜单**只有一条**可见分隔线；
 *   3. 那条线不在首尾，且两侧都是**可见的菜单项**（不是另一条线、不是空白）。
 *
 * 用法：
 *   node scripts/context-menu-layout.js
 *   node scripts/context-menu-layout.js --shot=<file.png>   # 顺带出图，人眼复核
 *   MSEDGE_PATH=<path> node scripts/context-menu-layout.js
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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 右键菜单分隔线探针未运行");
  process.exit(0);
}

const enc = (s) => JSON.stringify(s);

let html = read("ui/index.html").replace(/<script[\s\S]*?<\/script>/g, "");
const css = read("ui/styles.css");
const menuSrc = read("ui/scripts/contextmenu.js");

const driver = `
(async () => {
  const out = { errors: [] };
  try {
    // 桩：菜单里的"运行状态查询"会调宿主，给一个"不认识这个文件"的回答（走默认分支）
    window.getTauriInvoke = () => async () => ({ known: false });
    window.setStatus = () => {};
    window.state = { currentProject: { path: "D:/proj", name: "proj", lang: "unknown" }, tabs: [], activeTabId: null };
    new Function(${enc(menuSrc)})();
    const UI = window.ContextMenuUI;
    const menu = document.getElementById("context-menu");
    menu.style.display = "block";   // 菜单自身的显示由 showContextMenu 管；这里兜底，免得量到一堆 0
    const info = (el) => {
      const cs = getComputedStyle(el);
      return {
        cls: String(el.className),
        sep: el.classList.contains("context-menu-sep"),
        shown: cs.display !== "none",
        h: el.offsetHeight,
        cssH: cs.height,
        pt: cs.paddingTop,
        pb: cs.paddingBottom,
        bg: cs.backgroundColor,
        text: (el.textContent || "").trim().slice(0, 14),
      };
    };
    const dump = (m) => Array.from(m.children).map(info);
    const visible = (rows) => rows.filter((r) => r.shown);

    await UI.showContextMenu(menu, 10, 10, true);   // 文件夹：隐藏"运行/试跑"
    out.rowsDir = dump(menu);
    await UI.showContextMenu(menu, 10, 10, false);  // 文件：隐藏"创建文件/创建文件夹"
    out.rowsFile = dump(menu);
    // 标签栏那张菜单不进 show 流程（它是静态的），量 computed 就够
    out.rowsTab = dump(document.getElementById("tab-context-menu"));

    out.sepCount = (rows) => visible(rows).filter((r) => r.sep).length;
    out.dir = { seps: out.sepCount(out.rowsDir), vis: visible(out.rowsDir) };
    out.file = { seps: out.sepCount(out.rowsFile), vis: visible(out.rowsFile) };
    out.tab = { vis: visible(out.rowsTab) };
  } catch (err) {
    out.errors.push("DRIVER: " + ((err && err.stack) || err));
  }
  const pre = document.createElement("pre");
  pre.id = "__PROBE__";
  pre.textContent = JSON.stringify(out).replace(/&/g, "&#38;").replace(/</g, "&#60;");
  document.body.appendChild(pre);
})();
`;

html = html.replace(/<\/body>/, () => `<style>${css}</style><script>${driver}</script></body>`);
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-ctxmenu-layout-"));
const page = path.join(dir, "probe.html");
fs.writeFileSync(page, html, "utf8");

const shotArg = process.argv.slice(2).find((a) => a.startsWith("--shot="));
if (shotArg) {
  const want = shotArg.split("=")[1];
  const shotPath = path.isAbsolute(want) ? want : path.resolve(process.cwd(), want);
  cp.spawnSync(
    browser,
    [
      "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
      "--user-data-dir=" + path.join(dir, "profile-shot"),
      "--window-size=900,620", "--force-device-scale-factor=1",
      "--virtual-time-budget=12000",
      "--screenshot=" + shotPath,
      "file:///" + page.replace(/\\/g, "/"),
    ],
    { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }
  );
  console.log(fs.existsSync(shotPath) ? `  截图: ${shotPath}` : "FAIL: 截图没生成");
}

const cpRes = cp.spawnSync(
  browser,
  [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + path.join(dir, "profile"),
    "--window-size=900,620", "--force-device-scale-factor=1",
    "--virtual-time-budget=12000", "--dump-dom",
    "file:///" + page.replace(/\\/g, "/"),
  ],
  { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 }
);

const dom = cpRes.stdout || "";
const m = dom.match(/<pre id="__PROBE__">([\s\S]*?)<\/pre>/);
if (!m) {
  console.log("FAIL: 探针页面没有产出结果（脚本没跑到底）");
  console.log("  stderr: " + (cpRes.stderr || "").split("\n").filter(Boolean).slice(0, 3).join(" ⏐ "));
  process.exit(1);
}
const out = JSON.parse(m[1].replace(/&#60;/g, "<").replace(/&#38;/g, "&"));

let bad = 0;
const fail = (msg) => {
  console.log("FAIL: " + msg);
  bad++;
};
const ok = (msg) => console.log("  ok  " + msg);

for (const e of out.errors || []) fail(e);

// 判据 1：每条分隔线都是发丝线（两张菜单的所有 sep 都查 —— 标签菜单不显示，查 computed）
const allSeps = []
  .concat(out.rowsDir || [], out.rowsFile || [], out.rowsTab || [])
  .filter((r) => r.sep);
const fat = allSeps.filter((r) => r.cssH !== "1px" || r.pt !== "0px" || r.pb !== "0px");
allSeps.length
  ? fat.length === 0
    ? ok(`分隔线都是发丝线（${allSeps.length} 条：height 1px + padding 0，背景 ${allSeps[0].bg}）`)
    : fail(
        `分隔线被撑粗了：${fat.map((r) => `${r.cls}[cssH=${r.cssH} pt=${r.pt} pb=${r.pb}]`).join(" / ")}` +
          " —— 多半是它又挂上了 context-menu-item（那 6px 上下内边距会把 1px 的线变成 13px 的灰条）"
      )
  : fail("一张菜单里都没量到 .context-menu-sep（选择器或类名变了？）");

// 判据 2 + 3：每张**动态**菜单只有一条可见分隔线，且它夹在两个可见菜单项之间
for (const [name, rows, seps] of [
  ["文件夹菜单", out.rowsDir || [], out.dir ? out.dir.seps : -1],
  ["文件菜单", out.rowsFile || [], out.file ? out.file.seps : -1],
]) {
  const vis = (rows || []).filter((r) => r.shown);
  const sepIdx = vis.map((r, i) => (r.sep ? i : -1)).filter((i) => i >= 0);
  const only = sepIdx.length === 1;
  const inner = only && sepIdx[0] > 0 && sepIdx[0] < vis.length - 1;
  const neighbours =
    inner && !vis[sepIdx[0] - 1].sep && !vis[sepIdx[0] + 1].sep;
  only && neighbours
    ? ok(`${name}：只有一条分隔线，且夹在两个菜单项之间（可见 ${vis.length} 项）`)
    : fail(
        `${name}：分隔线不对（可见分隔线 ${seps} 条，位置 ${sepIdx.join(",") || "无"}，可见项 ${vis.length} 条）` +
          " —— 条目按上下文隐藏后，多余的线要折叠掉（collapseSeparators）"
      );
}

fs.rmSync(dir, { recursive: true, force: true });
if (bad) {
  console.log("context-menu-layout: " + bad + " 项不通过");
  process.exit(1);
}
console.log("context-menu-layout: 全部通过");
