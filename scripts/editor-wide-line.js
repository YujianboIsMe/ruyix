#!/usr/bin/env node
/**
 * 编辑器**宽行只读视图**探针（真浏览器结构/几何门禁，v0.13；需要本机装了 Edge/Chromium）
 *
 * 为什么需要它：`ui/xterm.js`（283,404 字节、**2 行**、最长行 283,184 字符 ≈ 2.2e6 px）
 * 一打开就把编辑器打崩。纵向虚拟化救不了 —— 那里只有一行，"可见行"就是那一行本身。
 * 修法是宽行只读视图：超长行切成显示段、textarea 退出布局、顶部加一条不编号的虚拟横幅。
 * 这几件事**全都只存在于布局引擎里**（Node 的 DOM 桩量不到），所以门禁必须是真浏览器。
 *
 * 做法：把**真实的** ui/index.html（剥 <script>/<link>）+ ui/styles.css + ui/main.js
 * 拼成一个自包含页面（main.js 的引导块剥掉，只借函数），在无头 Edge 里跑四个臂：
 *
 *   臂 1  2000 列   → **不该**触发（阈值是「> 2000」）
 *   臂 2  2001 列   → 触发
 *   臂 3  283,184 列（xterm.js 同量级）→ 只读视图的全部判据
 *   臂 4  **反向验证**：把阈值调成 1e9（等价于关掉这条闸门）后喂 20,000 列 →
 *         必须观测到旧病征（内容宽 ≫ 视口）。这一臂是给闸门自己做的对照：
 *         若关掉闸门也没事，说明这门是摆设。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 触发精度：2000 列不触发、2001 列触发（逐列，不看"长行存在与否"）；
 *   2. 283,184 列进入只读视图：横幅行文字对得上、`logical[0] === 0`、它在代码区第一行且带警示类；
 *   3. **虚拟行不计入行号**：gutter 前三格是 `["", "1", ""]` —— 横幅空号、内容首行 1、续段空号；
 *   4. 宽度封顶：容器宽 ≤ 视口宽，且 `.editor-view` **没有横向滚动条**；
 *   5. textarea 退出布局（`display:none`）+ 全文高度由流内 spacer 给出 + 真的能滚到底；
 *   6. 背板节点数有界（不再随行长增长）；
 *   7. 显示行按段拼回去**逐段等于原文**（不是"看起来像"）；
 *   8. 反向验证（臂 4）：关掉闸门后旧病征必须复现。
 *
 * 不测的：**墙钟耗时**。本脚本用 `--dump-dom` + `--virtual-time-budget`（无头下 rAF 不派发，
 * 探针只能同步驱动），虚拟时间里量时间会失真 —— 性能请用真机（打开那个文件）试，
 * 判据是"能打开、能看、能滚"，不是某个 ms 数。
 *
 * 用法：
 *   node scripts/editor-wide-line.js          # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   MSEDGE_PATH=<path> node scripts/editor-wide-line.js
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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 宽行只读视图探针未运行（该路将不被覆盖）");
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
// 臂 4 用：把阈值调到天上 = 等价于"没有这条闸门"，用来给闸门自己做对照
const THRESHOLD_DECL = "const EDITOR_WIDE_MAX_COLS = 2000;";
if (!mainSrc.includes(THRESHOLD_DECL)) {
  console.log("FAIL: 在 main.js 里定位不到阈值声明（探针锚点失效）：" + THRESHOLD_DECL);
  process.exit(1);
}
const noGateSrc = mainSrc.replace(THRESHOLD_DECL, "const EDITOR_WIDE_MAX_COLS = 1000000000;");

const BANNER = "行太宽，触发只读限制";

const driver = `
(async function () {
  const out = { errors: [], notes: [] };
  window.onerror = (m) => out.errors.push(String((m && m.message) || m));
  const NL = String.fromCharCode(10);
  const round = (n) => Math.round(n);
  const info = (el) => {
    const cs = getComputedStyle(el);
    const bx = parseFloat(cs.borderLeftWidth) + parseFloat(cs.borderRightWidth);
    const by = parseFloat(cs.borderTopWidth) + parseFloat(cs.borderBottomWidth);
    return {
      display: cs.display,
      clientW: el.clientWidth, clientH: el.clientHeight,
      scrollW: el.scrollWidth, scrollH: el.scrollHeight,
      offsetH: el.offsetHeight,
      // 真画出来的滚动条才占这几像素（offset 含边框与滚动条，client 都不含）
      vBar: round(el.offsetWidth - bx - el.clientWidth),
      hBar: round(el.offsetHeight - by - el.clientHeight),
    };
  };
  const gutterTexts = () =>
    Array.prototype.slice.call(document.querySelectorAll("#editor-gutter .gutter-line"))
      .map((e) => e.textContent);
  const codeTexts = () =>
    Array.prototype.slice.call(document.querySelectorAll("#editor-code-backdrop .code-line"))
      .map((e) => e.textContent);

  try {
    // 祖先里若有 display:none（无项目时的初始态）就地打开 —— 真机打开项目后就是这个样子
    for (let el = document.getElementById("editor-view"); el && el !== document.body; el = el.parentElement) {
      if (getComputedStyle(el).display === "none") {
        el.style.display = el.classList.contains("editor-body") ? "flex" : "block";
        out.notes.push("强制显示祖先 " + (el.id || el.className));
      }
    }
    // 同一份 main.js 装两遍：真实阈值一遍、阈值调到天上（= 无闸门）一遍
    const api = (src) =>
      new Function(
        src + "\\nreturn { showEditor: showEditor, setupEditorVirtualScroll: setupEditorVirtualScroll," +
        " renderPlainCode: renderPlainCode, isWideText: isWideText," +
        " editorSegCols: editorSegCols, recutWideModel: recutWideModel," +
        " model: function () { return editorModel; } };"
      )();
    const ed = api(${enc(mainSrc)});
    const noGate = api(${enc(noGateSrc)});
    ed.showEditor();
    ed.setupEditorVirtualScroll();
    noGate.showEditor();

    const v = document.getElementById("editor-view");
    const c = document.getElementById("editor-code-container");
    const b = document.getElementById("editor-code-backdrop");
    const ta = document.getElementById("editor-textarea");
    const sp = document.getElementById("editor-flow-spacer");
    const nodes = () => b.querySelectorAll("*").length;
    const render = (ref, text) =>
      ref.renderPlainCode({ id: "probe", name: "probe.js", content: text });

    // ---------------- 臂 1 / 2：触发精度（逐列）----------------
    render(ed, ["a".repeat(2000)].join(NL));
    const a2000 = { wide: !!ed.model().wide, rows: ed.model().total, nodes: nodes() };
    render(ed, ["a".repeat(2001)].join(NL));
    const a2001 = { wide: !!ed.model().wide, rows: ed.model().total, banner: (codeTexts()[0] || "").slice(0, 40) };

    // ---------------- 臂 3：xterm.js 同量级（283,184 列）----------------
    const HUGE = "a".repeat(283184);
    const hugeLines = [HUGE, ""];
    render(ed, hugeLines.join(NL));
    const m3 = ed.model();
    const g3 = gutterTexts();
    const c3 = codeTexts();
    const firstRow = b.querySelector(".code-line");
    const huge = {
      wide: !!m3.wide,
      rows: m3.total,
      lineOfHead: m3.lineOf.slice(0, 4),
      lineOfFirst: m3.lineOf[0],
      bannerText: m3.texts[0],
      bannerDom: c3.length ? c3[0] : "",
      warnRowFirst: !!firstRow && firstRow.className.indexOf("editor-warn-row") >= 0,
      gutterHead: g3.slice(0, 3),
      gutterCount: g3.length,
      segLen: (m3.texts[1] || "").length,
      nodeCount: nodes(),
      view: info(v), container: info(c), textarea: info(ta), spacer: info(sp),
      rebuild: (function () {
        // 按 lineOf 把显示段拼回逻辑行 —— 这是"分块没丢字、没错序"的**唯一**判据，
        // 不能只看"界面上像那么回事"
        const want = hugeLines;
        const got = [];
        for (let i = 0; i < m3.texts.length; i++) {
          const no = m3.lineOf[i];
          if (no < 0) continue; // 顶部虚拟横幅不是内容
          got[no] = (got[no] || "") + m3.texts[i];
        }
        if (got.length !== want.length) return "行数 " + got.length + " != " + want.length;
        for (let i = 0; i < want.length; i++) {
          if (got[i] !== want[i]) {
            return "第 " + (i + 1) + " 行不等（拼回 " + (got[i] || "").length + " 字符 vs 原 " + want[i].length + "）";
          }
        }
        return "ok";
      })(),
    };
    // 滚到底：spacer 给的高度够不够（不够就滚不到最后一行）
    v.scrollTop = 1e9;
    huge.scrollTopAfter = v.scrollTop;
    huge.lastGutter = gutterTexts().slice(-1)[0];

    // ---------------- 臂 3.5：视口变窄 → 段宽必须重切 ----------------
    // （ResizeObserver 在无头 --dump-dom 下不派发，所以这里直接调生产那条重切路径：
    //   段宽一旦是旧视口算出来的，横向滚动条会回来，且行数/spacer 高度全错、滚不到最后几行）
    const narrowBefore = { rows: huge.rows, seg: huge.segLen };
    document.getElementById("app").style.width = "620px";
    const vwNarrow = v.clientWidth; // 读一次布局，逼出真实宽度
    ed.recutWideModel(ed.editorSegCols());
    const m35 = ed.model();
    const narrowAfter = {
      rows: m35.total,
      seg: (m35.texts[1] || "").length,
      view: info(v), container: info(c), spacer: info(sp),
      rebuild: (function () {
        const got = [];
        for (let i = 0; i < m35.texts.length; i++) {
          const no = m35.lineOf[i];
          if (no < 0) continue;
          got[no] = (got[no] || "") + m35.texts[i];
        }
        return got.length === hugeLines.length && got[0] === hugeLines[0] && got[1] === hugeLines[1]
          ? "ok"
          : "重切后拼不回原文";
      })(),
    };

    // ---------------- 臂 4：反向验证（关掉闸门 → 旧病征必须复现）----------------
    render(noGate, ["a".repeat(20000)].join(NL));
    const m4 = noGate.model();
    const firstRow4 = b.querySelector(".code-line");
    const noGateOut = {
      wide: !!m4.wide,
      rows: m4.total,
      view: info(v), container: info(c), textarea: info(ta),
      codeLineScrollW: firstRow4 ? firstRow4.scrollWidth : -1,
    };

    out.arm = { a2000: a2000, a2001: a2001, huge: huge, narrow: { before: narrowBefore, after: narrowAfter, vw: vwNarrow }, noGate: noGateOut };
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
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-editor-wide-"));
const page = path.join(dir, "probe.html");
fs.writeFileSync(page, html, "utf8");

const cpRes = cp.spawnSync(
  browser,
  [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + path.join(dir, "profile"),
    "--window-size=1080,660", "--force-device-scale-factor=1",
    "--virtual-time-budget=20000", "--dump-dom",
    "file:///" + page.replace(/\\/g, "/"),
  ],
  // 超时也算"不通过"：这一路的病征就是**卡死/崩**，等不回来本身就是结论
  { encoding: "utf8", maxBuffer: 256 * 1024 * 1024, timeout: 240000 }
);

const dom = cpRes.stdout || "";
if (cpRes.error && cpRes.error.code === "ETIMEDOUT") {
  console.log("FAIL: 无头浏览器 4 分钟没有返回 —— 页面卡死（宽行那一路没挡住）");
  fs.rmSync(dir, { recursive: true, force: true });
  process.exit(1);
}
const m = dom.match(/<pre id="__PROBE__">([\s\S]*?)<\/pre>/);
if (!m) {
  console.log("FAIL: 探针页面没有产出结果（脚本没跑到底，或渲染进程死了）");
  const errs = (cpRes.stderr || "").split("\n").filter(Boolean).slice(0, 3);
  if (errs.length) console.log("  stderr: " + errs.join(" ⏐ "));
  fs.rmSync(dir, { recursive: true, force: true });
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

const arm = out.arm || {};
const a2000 = arm.a2000 || {};
const a2001 = arm.a2001 || {};
const huge = arm.huge || {};
const noGate = arm.noGate || {};
const hv = huge.view || {};
const hc = huge.container || {};
const ht = huge.textarea || {};
const hs = huge.spacer || {};

// ---- 1. 触发精度 ----
a2000.wide === false
  ? ok("2000 列**不**触发（阈值是「> 2000」，不是「>=」）")
  : fail("2000 列就触发了：阈值判断用了 >= ？实际 wide=" + JSON.stringify(a2000.wide));
a2001.wide === true
  ? ok("2001 列触发（rows " + a2001.rows + "）")
  : fail("2001 列没有触发（wide=" + JSON.stringify(a2001.wide) + "）：长行闸门没接上");

// ---- 2. 只读视图 + 顶部虚拟横幅 ----
console.log(
  "  数据 283,184 列 → 显示行 " + huge.rows + "（第一段 " + huge.segLen + " 字符）、背板节点 " +
    huge.nodeCount + "、容器 " + hc.clientW + "px / 视口 " + hv.clientW + "px"
);
huge.wide === true ? ok("进入只读宽行视图") : fail("283,184 列没有进入只读视图（wide=" + JSON.stringify(huge.wide) + "）");
huge.lineOfFirst === -1 && huge.bannerText === BANNER
  ? ok("顶部虚拟行是横幅且不编号（lineOf[0] === -1）")
  : fail(
      "顶部虚拟行不对：lineOf[0]=" + JSON.stringify(huge.lineOfFirst) +
        "，文字=" + JSON.stringify(huge.bannerText) + "（应为 -1 / " + JSON.stringify(BANNER) + "）"
    );
huge.bannerDom === BANNER && huge.warnRowFirst
  ? ok("横幅是代码区第一行且带警示类 editor-warn-row")
  : fail(
      "横幅没落在代码区第一行/没带警示类：第一行文字 " + JSON.stringify(huge.bannerDom) +
        "、警示类 " + JSON.stringify(huge.warnRowFirst)
    );

// ---- 3. 虚拟行不计入行号 ----
JSON.stringify(huge.gutterHead) === JSON.stringify(["", "1", ""])
  ? ok("gutter 前三格是 [空, 1, 空] —— 横幅不进行号，续段也不重复编号")
  : fail(
      "gutter 行号不对：前三格 " + JSON.stringify(huge.gutterHead) +
        "（应为 [\"\", \"1\", \"\"]：横幅空号、内容首行 1、续段空号）"
    );

// ---- 4. 宽度封顶 + 没有横向滚动条 ----
hc.clientW <= hv.clientW + 1
  ? ok("容器宽被压在视口宽以内（" + hc.clientW + " <= " + hv.clientW + "）")
  : fail("容器比视口宽 " + (hc.clientW - hv.clientW) + "px：宽行的宽度还在撑容器");
hv.scrollW <= hv.clientW + 1
  ? ok("编辑器没有横向滚动条（scrollWidth " + hv.scrollW + " <= clientWidth " + hv.clientW + "）")
  : fail(
      "编辑器出现了横向滚动条（scrollWidth " + hv.scrollW + " > clientWidth " + hv.clientW +
        "）：宽行内容仍在撑滚动区"
    );

// ---- 5. textarea 退出布局 + spacer 撑高 + 能滚到底 ----
ht.display === "none"
  ? ok("textarea 整个退出布局（display:none）—— 那条 28 万字符的单行不再被排版")
  : fail("textarea 还在布局里（display=" + JSON.stringify(ht.display) + "）：全文单行照样会被浏览器排版");
hs.offsetH === huge.rows * 20 + 16
  ? ok("全文高度由流内 spacer 给出（" + hs.offsetH + "px = 显示行 " + huge.rows + " × 20 + 16）")
  : fail("spacer 高度不对：" + hs.offsetH + "px，应为显示行 " + huge.rows + " × 20 + 16 = " + (huge.rows * 20 + 16));
huge.scrollTopAfter > 0
  ? ok("能滚到底（scrollTop " + huge.scrollTopAfter + " / 高度 " + hv.scrollH + "）")
  : fail("滚不动：scrollTop 停在 " + huge.scrollTopAfter + "，scrollHeight " + hv.scrollH);

// ---- 6. 节点数有界 ----
huge.nodeCount > 0 && huge.nodeCount <= 600
  ? ok("背板节点数有界（" + huge.nodeCount + " 个，窗口只有百来行）")
  : fail(
      "背板节点数 " + huge.nodeCount + " 个：窗口外/超长行仍在整条建 DOM（28 万列那条会到十万级）"
    );

// ---- 7. 内容逐段可还原 ----
huge.rebuild === "ok"
  ? ok("显示段按行拼回 == 原文（逐行字符串相等，不是「看起来像」）")
  : fail("显示段拼不回原文：" + JSON.stringify(huge.rebuild));

// ---- 9. 视口变窄 → 段宽重切 ----
const nb = (arm.narrow || {}).before || {};
const na = (arm.narrow || {}).after || {};
const nv = na.view || {};
nb.seg > 0 && na.seg > 0 && na.seg < nb.seg && na.rows > nb.rows
  ? ok("视口变窄后段宽跟着重切（段 " + nb.seg + " → " + na.seg + " 列，显示行 " + nb.rows + " → " + na.rows + "）")
  : fail(
      "视口变窄后没有重切段（旧段 " + JSON.stringify(nb.seg) + " 列 → " + JSON.stringify(na.seg) +
        " 列、显示行 " + JSON.stringify(nb.rows) + " → " + JSON.stringify(na.rows) +
        "）：段宽是旧视口算的，横向滚动条会回来、行数也是错的"
    );
na.rebuild === "ok"
  ? ok("重切之后显示段仍能拼回原文")
  : fail("重切之后拼不回原文：" + JSON.stringify(na.rebuild));
na.spacer && na.spacer.offsetH === na.rows * 20 + 16
  ? ok("重切后 spacer 高度跟着更新（" + na.spacer.offsetH + "px）")
  : fail("重切后 spacer 高度没更新：" + JSON.stringify(na.spacer && na.spacer.offsetH) + "，应为 " + (na.rows * 20 + 16));
nv.clientW > 0 && nv.scrollW <= nv.clientW + 1
  ? ok("重切后依然没有横向滚动条（scrollWidth " + nv.scrollW + " <= clientWidth " + nv.clientW + "）")
  : fail("重切后出现了横向滚动条（scrollWidth " + nv.scrollW + " > clientWidth " + nv.clientW + "）");

// ---- 8. 反向验证：关掉闸门后旧病征必须复现 ----
const ngView = noGate.view || {};
const ngTa = noGate.textarea || {};
const ngBroken =
  noGate.wide === false &&
  ngTa.scrollW > 5 * ngTa.clientW &&
  ngView.scrollW > 5 * ngView.clientW;
ngBroken
  ? ok(
      "反向验证成立：把阈值调大之后，同样的路径立刻退回旧病征（视口 " + ngView.clientW +
        "px / 滚动区 " + ngView.scrollW + "px，textarea 内容宽 " + ngTa.scrollW +
        "px）—— 说明这条闸门在挡真东西"
    )
  : fail(
      "反向验证失败：关掉闸门后没看到旧病征（wide=" + JSON.stringify(noGate.wide) +
        "、视口滚动区 " + ngView.scrollW + "、textarea 内容宽 " + ngTa.scrollW +
        "）—— 要么闸门是摆设，要么探针量错了地方"
    );

fs.rmSync(dir, { recursive: true, force: true });
if (bad) {
  console.log("editor-wide-line: " + bad + " 项不通过");
  process.exit(1);
}
console.log("editor-wide-line: 全部通过");
