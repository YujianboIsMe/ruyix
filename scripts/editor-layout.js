#!/usr/bin/env node
/**
 * 编辑器**真实布局**探针（几何门禁，需要本机装了 Edge/Chromium）
 *
 * 为什么需要它：ui-smoke 的其余场景跑在 Node 的微型 DOM 桩上 —— 没有布局引擎，
 * 所以"谁长出了滚动条""textarea 会不会为了露出光标自己内部滚动""容器够不够宽"
 * 这类问题在桩里**根本量不到**。它们已经在真机上出过一次事故：
 * 编辑器里同时出现两对滚动条（`.editor-view` 一对 + `<textarea>` 自己一对），
 * 而且光标与高亮字形错位 —— 原因是 P2 把 backdrop 改成绝对定位后，容器失去
 * "被内容撑开"的能力，只剩 `flex:1` = 视口宽度，textarea 的正文装不下自己那一格。
 *
 * 做法：把**真实的** ui/index.html（剥掉 <script>/<link>）+ ui/styles.css +
 * ui/main.js 拼成一个自包含页面，在无头 Edge 里打开、喂一份 5000 行的合成源码
 * （第 11 行故意很长、且**不在可视窗口内**），然后逐元素量 offset/client/scroll。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 编辑器子树里**只有一个**元素画出滚动条，且它是 `.editor-view`（textarea 必须为 0）；
 *   2. textarea 的正文装得下自己的格子（scrollWidth <= clientWidth）——
 *      装不下它就会自己滚，于是"露出光标"时字形与光标错位；
 *   3. 光标放到最长行末尾：textarea.scrollLeft 必须保持 0，改由外层滚（两层才同步）；
 *   4. 容器宽度必须**大于可视宽度若干倍**（说明宽度真由"最宽行"给出，而不是视口宽）。
 *
 * 用法：
 *   node scripts/editor-layout.js          # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   MSEDGE_PATH=<path> node scripts/editor-layout.js
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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 布局探针未运行（几何问题将不被覆盖）");
  process.exit(0);
}

// ---- 拼页面：真实 HTML + 真实 CSS + 真实 main.js ----
let html = read("ui/index.html")
  .replace(/<script[\s\S]*?<\/script>/g, "")
  .replace(/<link[^>]*>/g, "");
const css = read("ui/styles.css");
const enc = (s) => JSON.stringify(s).replace(/</g, "\\u003c");
// main.js 的引导块剥掉：只借函数声明，不要它去初始化整个应用（没有 Tauri 后端）
const mainSrc = read("ui/main.js").replace(
  /if \(document\.readyState === "loading"\) \{[\s\S]*?initApp\(\);[\s\S]*?\n\}/,
  "/* 引导块被探针剥掉 */"
);

const driver = `
(async function () {
  const out = { errors: [], notes: [] };
  window.onerror = (m) => out.errors.push(String((m && m.message) || m));
  const rect = (el) => {
    const r = el.getBoundingClientRect();
    return [Math.round(r.x), Math.round(r.y), Math.round(r.width), Math.round(r.height)];
  };
  const info = (el) => {
    const cs = getComputedStyle(el);
    const bx = parseFloat(cs.borderLeftWidth) + parseFloat(cs.borderRightWidth);
    const by = parseFloat(cs.borderTopWidth) + parseFloat(cs.borderBottomWidth);
    // 真画出来的滚动条才占这几像素（offset 含边框与滚动条，client 都不含）
    return {
      vBar: Math.round(el.offsetWidth - bx - el.clientWidth),
      hBar: Math.round(el.offsetHeight - by - el.clientHeight),
      clientW: el.clientWidth, clientH: el.clientHeight,
      scrollW: el.scrollWidth, scrollH: el.scrollHeight,
      rect: rect(el),
    };
  };
  try {
    const ed = new Function(${enc(mainSrc)} + "\\nreturn {" +
      " renderPlainCode: renderPlainCode, showEditor: showEditor," +
      " setupEditorVirtualScroll: setupEditorVirtualScroll," +
      " paint: paintEditorWindow };")();

    // 祖先里若有 display:none（无项目时的初始态）就地打开 —— 真机打开项目后就是这个样子
    const startEl = document.getElementById("editor-view");
    for (let el = startEl; el && el !== document.body; el = el.parentElement) {
      if (getComputedStyle(el).display === "none") {
        el.style.display = el.classList.contains("editor-body") ? "flex" : "block";
        out.notes.push("强制显示祖先 " + (el.id || el.className));
      }
    }
    ed.showEditor();
    ed.setupEditorVirtualScroll();

    // 合成源码：5000 行，第 11 行特别长（并且**远离**可视窗口 —— 这正是不变量会被破坏的场景）
    const N = 5000, LONG_AT = 10;
    const lines = [];
    for (let i = 0; i < N; i++) {
      lines.push(
        i === LONG_AT
          ? "// 长行 " + "长".repeat(60) + " " + "a".repeat(220)
          : "let v" + i + " = " + i + "; // 第 " + (i + 1) + " 行"
      );
    }
    const content = lines.join("\\n");
    ed.renderPlainCode({ id: "probe", name: "probe.rs", content: content });

    const v = document.getElementById("editor-view");
    const c = document.getElementById("editor-code-container");
    const b = document.getElementById("editor-code-backdrop");
    const ta = document.getElementById("editor-textarea");

    // 滚到长行看不到的地方：此时容器宽度只能由「窗口里保留的那条最宽行占位」给出
    v.scrollTop = Math.floor(N * 0.6) * 20;
    ed.paint();

    out.view = info(v);
    out.container = info(c);
    out.textarea = info(ta);
    out.keeperPresent = !!b.querySelector(".editor-virt-keeper");
    out.baViewport = out.textarea.clientW; // 只作报告用

    // 光标放到最长行末尾，看它是由外层滚还是 textarea 自己内部滚
    ta.focus();
    let off = 0;
    for (let i = 0; i < LONG_AT; i++) off += lines[i].length + 1;
    off += lines[LONG_AT].length;
    ta.setSelectionRange(off, off);
    ta.blur();
    ta.focus();
    ta.setSelectionRange(off, off);
    out.caret = {
      longLineChars: lines[LONG_AT].length,
      textareaScrollLeft: ta.scrollLeft,
      textareaScrollTop: ta.scrollTop,
      viewScrollLeft: v.scrollLeft,
      viewScrollTop: v.scrollTop,
    };
    // 出一份"谁画了滚动条"的清单（判据 1 直接吃它）
    out.bars = [];
    for (const [name, el] of [["#editor-view", v], ["#editor-code-container", c],
                              ["#editor-code-backdrop", b], ["#editor-textarea", ta]]) {
      const i = info(el);
      if (i.vBar > 0 || i.hBar > 0) out.bars.push({ name: name, vBar: i.vBar, hBar: i.hBar });
    }
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
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-editor-layout-"));
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
const m = dom.match(/<pre id="__PROBE__">([\s\S]*?)<\/pre>/);
if (!m) {
  console.log("FAIL: 探针页面没有产出结果（脚本没跑到底）");
  const errs = (cpRes.stderr || "").split("\n").filter(Boolean).slice(0, 3);
  console.log("  stderr: " + errs.join(" ⏐ "));
  process.exit(1);
}
const out = JSON.parse(
  m[1].replace(/&#60;/g, "<").replace(/&#38;/g, "&")
);

let bad = 0;
const fail = (msg) => {
  console.log("FAIL: " + msg);
  bad++;
};
const ok = (msg) => console.log("  ok  " + msg);

for (const e of out.errors) fail("页面报错: " + e);
if (out.notes.length) console.log("  note " + out.notes.join("; "));

const bars = out.bars || [];
const onlyView = bars.length === 1 && bars[0].name === "#editor-view";
const barText = (x) => x.name + "(竖" + x.vBar + "/横" + x.hBar + ")";
console.log("滚动条: " + (bars.length ? bars.map(barText).join(" + ") : "（无）"));
onlyView
  ? ok("编辑器子树里只有 .editor-view 画滚动条（textarea 没长第二对）")
  : fail(
      "编辑器子树里画滚动条的元素应**只有** #editor-view，实际: " +
        (bars.map((x) => x.name).join(" + ") || "无") +
        " —— textarea 自己那对是「正文比格子宽」的直接证据"
    );

const ta = out.textarea, c = out.container, v = out.view;
ta.scrollW <= ta.clientW + 1
  ? ok("textarea 的正文装得下格子（scrollWidth " + ta.scrollW + " <= clientWidth " + ta.clientW + "）")
  : fail(
      "textarea 的正文装不下格子（scrollWidth " + ta.scrollW + " > clientWidth " + ta.clientW +
        "）：它会长出自己的滚动条，并在「露出光标」时内部滚动，光标与字形错位"
    );

c.clientW > v.clientW * 2
  ? ok("容器宽度由内容给出（容器 " + c.clientW + "px vs 视口 " + v.clientW + "px）")
  : fail(
      "容器宽度没有跟着「最宽行」走（容器 " + c.clientW + "px 只比视口 " + v.clientW +
        "px 宽一点）：最宽行不在窗口内时，宽度占位（.editor-virt-keeper）没起作用"
    );

out.keeperPresent
  ? ok("窗口里保留了最宽行的宽度占位")
  : fail("没有宽度占位元素：滚到宽行不在窗口的位置时，水平滚动条会缩掉");

const caret = out.caret || {};
caret.textareaScrollLeft === 0
  ? ok("光标在最长行末尾时 textarea 没有内部滚动（scrollLeft 0）")
  : fail(
      "光标在最长行末尾时 textarea 自己滚了（scrollLeft " + caret.textareaScrollLeft +
        "）：两层会错位（应该是外层滚、textarea 恒 0）"
    );
caret.viewScrollLeft > 0
  ? ok("水平滚动由外层 .editor-view 承担（scrollLeft " + caret.viewScrollLeft + "）")
  : fail("光标在最长行末尾时外层没有跟着滚（scrollLeft " + caret.viewScrollLeft + "）：光标会看不见");

fs.rmSync(dir, { recursive: true, force: true });
if (bad) {
  console.log("editor-layout: " + bad + " 项不通过");
  process.exit(1);
}
console.log("editor-layout: 全部通过");
