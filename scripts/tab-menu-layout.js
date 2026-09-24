#!/usr/bin/env node
/**
 * 标签页右键菜单**真 DOM**探针（需要本机装了 Edge/Chromium）
 *
 * 用户报："编辑区标签的右键菜单里没有【关闭左侧】子菜单。"
 *
 * 代码里那一项是**有的**（`ui/index.html` 的 `data-tab-action="left"`），问题在**可见性规则**：
 * 原实现"计划为空就把整项隐藏"——右键的标签左边没东西时，【关闭左侧】整条**消失**，
 * 用户看到的就是"菜单里没有这一项"。
 *
 * 契约（本探针钉的就是它）：**关闭类菜单项恒在**，干不了的那几项**置灰禁用**而不是消失 ——
 * 菜单形状稳定、位置记得住；点禁用项什么都不发生。
 *
 * 为什么不能用 Node 桩：这里要验的正是"**真 index.html 的 markup + 真 styles.css** 下，
 * 每一项的 computed display 与 class 到底怎样" —— 桩里没有 CSS，量不到"是不是被哪条规则藏了"。
 *
 * 做法：真 index.html（剥 script/link）+ 真 styles.css 拼成自包含页面，往 `#tab-bar` 注入
 * 三个假标签，把 main.js 里 `closePlan` + `setupTabContextMenu` 的**真源码**切片用
 * `new Function` 挂到**真 document** 上，然后派发真的 contextmenu 事件。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 右键中间标签：七项**全在**且都不是禁用态；
 *   2. 右键**最左**标签：七项**仍然全在**（不许消失），其中【关闭左侧】是**禁用**态；
 *   3. 右键不改激活标签；
 *   4. 点禁用项：不关任何标签；
 *   5. 点【关闭左侧】：只关它左边的那些；
 *   6. 静态：`ui/styles.css` 里没有把 `.context-menu-item` / `--path-only` 藏掉的规则
 *      （否则 JS 再对，菜单里也看不见）。
 *
 * 用法：
 *   node scripts/tab-menu-layout.js            # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   MSEDGE_PATH=<path> node scripts/tab-menu-layout.js
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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 标签右键菜单探针未运行（菜单形状将不被覆盖）");
  process.exit(0);
}

const mainSrc = read("ui/main.js");
const start = mainSrc.indexOf("function closePlan(ids, targetId, mode) {");
const end = mainSrc.indexOf("\nfunction setupContextMenu(", start);
if (start < 0 || end <= start) {
  console.log("FAIL: main.js 里定位不到 closePlan…setupTabContextMenu 这段源码（切片锚点失效）");
  process.exit(1);
}
const funcSlice = mainSrc.slice(start, end);

let html = read("ui/index.html")
  .replace(/<script[\s\S]*?<\/script>/g, "")
  .replace(/<link[^>]*>/g, "");
const css = read("ui/styles.css");

const driver = `
(function () {
  const out = { errors: [] };
  const slice = ${JSON.stringify(funcSlice)};
  try {
    const bar = document.getElementById("tab-bar");
    const menu = document.getElementById("tab-context-menu");
    // 三个假标签（形状照抄 renderTabs：.tab-item[data-tab-id] + .tab-name）
    bar.innerHTML = "";
    ["t1", "t2", "t3"].forEach((id) => {
      const el = document.createElement("div");
      el.className = "tab-item";
      el.dataset.tabId = id;
      const nm = document.createElement("span");
      nm.className = "tab-name";
      nm.textContent = id;
      el.appendChild(nm);
      bar.appendChild(el);
    });
    const state = {
      tabs: [
        { id: "t1", name: "a.rs", path: "D:\\\\p\\\\a.rs" },
        { id: "t2", name: "b.rs", path: "D:\\\\p\\\\b.rs" },
        { id: "t3", name: "c.rs", path: "D:\\\\p\\\\c.rs" },
      ],
      activeTabId: "t1",
    };
    const closed = [];
    const mod = new Function(
      "document",
      "state",
      "closeTab",
      "hideContextMenu",
      "handleContextCopyPath",
      slice + "\\nreturn { setupTabContextMenu };"
    )(
      document,
      state,
      (id) => closed.push(id),
      (m) => {
        m.style.display = "none";
      },
      async () => {}
    );
    mod.setupTabContextMenu();

    const rows = () =>
      [...menu.querySelectorAll("[data-tab-action]")].map((r) => ({
        action: r.dataset.tabAction,
        shown: getComputedStyle(r).display !== "none",
        disabled: r.classList.contains("context-menu-item--disabled"),
      }));
    const rightClick = (id) => {
      const el = [...bar.querySelectorAll(".tab-item")].find((n) => n.dataset.tabId === id);
      el.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, clientX: 120, clientY: 40 }));
    };

    rightClick("t2");
    out.middle = rows();
    out.activeAfterRightClick = state.activeTabId;

    rightClick("t1"); // 最左标签
    out.first = rows();

    // 点一个禁用项：什么都不该发生
    const dis = [...menu.querySelectorAll("[data-tab-action]")].find((r) =>
      r.classList.contains("context-menu-item--disabled")
    );
    out.disabledAction = dis ? dis.dataset.tabAction : null;
    if (dis) dis.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    out.closedAfterDisabledClick = closed.slice();

    // 中间标签点【关闭左侧】：只关它左边的
    rightClick("t2");
    const leftItem = menu.querySelector('[data-tab-action="left"]');
    if (leftItem) leftItem.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    out.closedAfterLeft = closed.slice();
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
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-tab-menu-"));
const page = path.join(dir, "probe.html");
fs.writeFileSync(page, html, "utf8");

const cpRes = cp.spawnSync(
  browser,
  [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + path.join(dir, "profile"),
    "--window-size=1400,900", "--force-device-scale-factor=1",
    "--virtual-time-budget=10000", "--dump-dom",
    "file:///" + page.replace(/\\/g, "/"),
  ],
  { encoding: "utf8", maxBuffer: 128 * 1024 * 1024 }
);

const dom = cpRes.stdout || "";
const m = dom.match(/<pre id="__PROBE__">([\s\S]*?)<\/pre>/);
fs.rmSync(dir, { recursive: true, force: true });
if (!m) {
  console.log("FAIL: 探针页面没有产出结果（脚本没跑到底）");
  console.log("  stderr: " + (cpRes.stderr || "").split("\n").filter(Boolean).slice(0, 2).join(" ⏐ "));
  process.exit(1);
}
const out = JSON.parse(m[1].replace(/&#60;/g, "<").replace(/&#38;/g, "&"));

const ALL = ["close", "others", "right", "left", "all", "copy-path", "copy-full-path"];
let bad = 0;
const fail = (msg) => {
  console.log("FAIL: " + msg);
  bad++;
};
const ok = (msg) => console.log("  ok  " + msg);
const fmt = (rows) =>
  (rows || [])
    .map((r) => `${r.action}${r.shown ? "" : "(藏)"}${r.disabled ? "(灰)" : ""}`)
    .join(" ");

for (const e of out.errors || []) fail("页面报错: " + e);

console.log("中间标签： " + fmt(out.middle));
console.log("最左标签： " + fmt(out.first));

// 判据 1：中间标签七项全在、都不禁用
const mid = out.middle || [];
ALL.every((a) => mid.some((r) => r.action === a && r.shown && !r.disabled))
  ? ok("右键中间标签：七项全在，且都不是禁用态")
  : fail(`右键中间标签的菜单不对：${fmt(mid)}`);

// 判据 2：最左标签七项**仍然全在**（用户报的正是这一条），【关闭左侧】为禁用
const first = out.first || [];
const allShown = ALL.every((a) => first.some((r) => r.action === a && r.shown));
allShown
  ? ok("右键最左标签：七项仍然全在（不消失）")
  : fail(`有菜单项在最左标签上**消失了**：${fmt(first)} —— 用户报的【关闭左侧】缺失就是这个成因`);
const leftRow = first.find((r) => r.action === "left");
leftRow && leftRow.disabled
  ? ok("最左标签上【关闭左侧】是**禁用**态（没东西可关，但不许消失）")
  : fail(`最左标签上【关闭左侧】应当禁用而不是可用：${JSON.stringify(leftRow)}`);

// 判据 3：右键不改激活标签
out.activeAfterRightClick === "t1"
  ? ok(`右键不改激活标签（仍是 ${out.activeAfterRightClick}）`)
  : fail(`右键改了激活标签：${out.activeAfterRightClick}`);

// 判据 4：点禁用项什么都不做
(out.closedAfterDisabledClick || []).length === 0
  ? ok(`点禁用项（${out.disabledAction}）什么都不发生`)
  : fail(`点禁用项竟然关了标签：${JSON.stringify(out.closedAfterDisabledClick)}`);

// 判据 5：点【关闭左侧】只关左边的
JSON.stringify(out.closedAfterLeft || []) === JSON.stringify(["t1"])
  ? ok("点【关闭左侧】只关它左边的那些（t1）")
  : fail(`【关闭左侧】关错了：${JSON.stringify(out.closedAfterLeft)}（期望 ["t1"]）`);

// 判据 6：静态 —— 没有 CSS 规则把菜单项藏掉
const hiddenByCss = [...css.matchAll(/\.context-menu[^{]*\{[^}]*display\s*:\s*none[^}]*\}/g)].map((x) => x[0]);
hiddenByCss.length === 0
  ? ok("ui/styles.css 里没有把 .context-menu* 藏掉的规则")
  : fail(`styles.css 里有规则会藏菜单项：${hiddenByCss.join(" ⏐ ")}`);

if (bad) {
  console.log("tab-menu-layout: " + bad + " 项不通过");
  process.exit(1);
}
console.log("tab-menu-layout: 全部通过");
