#!/usr/bin/env node
/**
 * 导航栏**真实几何**探针（需要本机装了 Edge/Chromium）
 *
 * 方案（用户拍板，v0.11）：**导航区不做横向滚动** —— 长名走省略号 `text-overflow: ellipsis`，
 * 全名 / 完整路径由行上的 `title` 悬停给出。理由：零抖动、行内按钮位置永远整齐。
 *
 * 之前两版横滚都被否，别再来第三版：
 *   ① `min-width: max-content` 把内容撑宽 → 悬停伪元素/滚动条一出现，树就抖；
 *   ② `position: sticky` 把行内按钮钉在可视区右缘 → 滚动的文字从按钮底下过去，按钮压在长路径上。
 *
 * 为什么需要真浏览器：全在布局引擎里 —— 省略号到底有没有生效、行内按钮位置齐不齐、
 * 有没有冒出横向滚动条，Node 的 DOM 桩里没有布局引擎，量不到。
 *
 * 做法：把**真实的** ui/index.html（剥掉 <script>/<link>）+ ui/styles.css 拼成自包含页面，
 * 用真 markup 往 `#file-tree` 塞行（形状照抄 main.js::renderTreeEntry：
 * `.tree-node[style=padding-left][title] > .tree-icon + .tree-name + button.tree-refresh`），
 * 在无头 Edge 里量两个场景。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 短内容：不出现横向滚动条；
 *   2. 长内容：**同样不出现**横向滚动条，且内容宽度不超过视口（真·没有横滚）；
 *   3. 长名确实走了省略号（`text-overflow: ellipsis`，且内容被收进格子）；
 *   4. 行上有 `title` 且等于**完整路径** —— 长名行和普通行都要有（悬停才读得到全名）；
 *   5. 所有目录行的刷新按钮**右边缘对齐**且都在可视区内（位置永远整齐 —— 这是选这个方案的收益）；
 *   6. 长目录行：按钮在名字右侧（不遮字）；
 *   7. 纵向滚动仍正常。
 *
 * 用法：
 *   node scripts/nav-layout.js            # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   MSEDGE_PATH=<path> node scripts/nav-layout.js
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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 导航栏几何探针未运行（长名省略号/title 将不被覆盖）");
  process.exit(0);
}

let html = read("ui/index.html")
  .replace(/<script[\s\S]*?<\/script>/g, "")
  .replace(/<link[^>]*>/g, "");
const css = read("ui/styles.css");

const LONG_NAME =
  "ApplicationPropertiesOfTheCloudShopAuthorityServiceIntegrationEnvironment.java";
const LONG_DIR = "integration-environment-application-properties-for-cloud-shop-authority";
const BASE = "D:\\\\Projects\\\\Java\\\\cloud-shop";
const LONG_PATH = BASE + "\\\\cloud-shop-authority\\\\src\\\\main\\\\java\\\\" + LONG_NAME;
// 驱动里注入 `${LONG_PATH}` 之后，页面看到的是 `\\` 字面量 → 解析出来是**单**反斜杠的 Windows 路径。
// 节点侧要跟它比，就得用同一份"单反斜杠"文本 —— 直接比 LONG_PATH 会永远差一倍反斜杠。
const EXPECT_TITLE = LONG_PATH.replace(/\\\\/g, "\\");

const driver = `
(function () {
  const out = { errors: [] };
  const round = (n) => Math.round(n);
  const bars = (el) => {
    if (!el) return null;
    const cs = getComputedStyle(el);
    const bx = parseFloat(cs.borderLeftWidth) + parseFloat(cs.borderRightWidth);
    const by = parseFloat(cs.borderTopWidth) + parseFloat(cs.borderBottomWidth);
    return {
      clientW: el.clientWidth, clientH: el.clientHeight,
      scrollW: el.scrollWidth, scrollH: el.scrollHeight,
      vBar: round(el.offsetWidth - bx - el.clientWidth),
      hBar: round(el.offsetHeight - by - el.clientHeight),
    };
  };
  const rectOf = (el) => {
    const r = el.getBoundingClientRect();
    return { left: round(r.left), right: round(r.right) };
  };
  // 形状照抄 main.js::renderTreeEntry（含 title）
  const fileRow = (name, depth, isDir, fullPath) => {
    const node = document.createElement("div");
    node.className = "tree-node";
    node.style.paddingLeft = depth * 16 + "px";
    node.title = fullPath || name;
    const icon = document.createElement("span");
    icon.className = "tree-icon " + (isDir ? "tree-icon--folder" : "tree-icon--file");
    icon.textContent = isDir ? "➡️" : "Ⓙ";
    const nm = document.createElement("span");
    nm.className = "tree-name";
    nm.textContent = name;
    node.appendChild(icon);
    node.appendChild(nm);
    if (isDir) {
      const btn = document.createElement("button");
      btn.className = "tree-refresh";
      btn.textContent = "🔄";
      node.appendChild(btn);
    }
    return node;
  };
  try {
    document.querySelectorAll(".nav-panel").forEach((p) => p.classList.remove("active"));
    document.getElementById("nav-panel-files").classList.add("active");
    const tree = document.getElementById("file-tree");
    const content = document.querySelector(".nav-content");
    const nav = document.getElementById("navigator");

    // ---- 场景 A：短内容 ----
    tree.innerHTML = "";
    ["pom.xml", "src", "README.md"].forEach((n) =>
      tree.appendChild(fileRow(n, 0, n === "src", "${BASE}" + "\\\\" + n))
    );
    out.shortBars = bars(content);

    // ---- 场景 B：长内容（长名文件 + 长名目录 + 深层缩进 + 长列表）----
    tree.innerHTML = "";
    tree.appendChild(fileRow("cloud-shop-authority", 0, true, "${BASE}" + "\\\\cloud-shop-authority"));
    tree.appendChild(fileRow("src", 1, true, "${BASE}" + "\\\\cloud-shop-authority\\\\src"));
    tree.appendChild(fileRow("main", 2, true, "${BASE}" + "\\\\cloud-shop-authority\\\\src\\\\main"));
    tree.appendChild(fileRow("${LONG_DIR}", 3, true, "${BASE}" + "\\\\cloud-shop-authority\\\\src\\\\main\\\\${LONG_DIR}"));
    for (let i = 0; i < 40; i++) {
      tree.appendChild(
        i === 7
          ? fileRow("${LONG_NAME}", 3, false, "${LONG_PATH}")
          : fileRow("File" + i + ".java", 3, false, "${BASE}" + "\\\\File" + i + ".java")
      );
    }

    out.longBars = bars(content);
    out.navRect = (() => {
      const r = nav.getBoundingClientRect();
      return { left: round(r.left), right: round(r.right), w: round(r.width) };
    })();

    // 判据 3/4：长名走的什么规则、格子装不装得下、行上有没有 title（= 完整路径）
    const names = [...document.querySelectorAll(".tree-name")];
    const longEl = names.find((e) => e.textContent === "${LONG_NAME}");
    const longRow = longEl ? longEl.closest(".tree-node") : null;
    out.longName = longEl
      ? {
          clientW: longEl.clientWidth,
          scrollW: longEl.scrollWidth,
          overflowRule: getComputedStyle(longEl).textOverflow,
          rowTitle: longRow ? longRow.title : null,
          ...rectOf(longEl),
        }
      : null;

    // 判据 5/6：所有目录行的刷新按钮右边缘（要齐）+ 长目录行按钮与名字不重叠
    const dirRows = [...document.querySelectorAll(".tree-node")].filter((n) =>
      n.querySelector("button.tree-refresh")
    );
    out.btnRights = dirRows.map((n) => round(n.querySelector("button.tree-refresh").getBoundingClientRect().right));
    out.rowsWithTitle = [...document.querySelectorAll(".tree-node")].filter((n) => !n.title).length;
    const longDirRow = [...document.querySelectorAll(".tree-node")].find(
      (n) => n.querySelector("button.tree-refresh") && (n.querySelector(".tree-name") || {}).textContent === "${LONG_DIR}"
    );
    if (longDirRow) {
      out.longDir = {
        name: rectOf(longDirRow.querySelector(".tree-name")),
        btn: rectOf(longDirRow.querySelector("button.tree-refresh")),
        rowTitle: longDirRow.title,
      };
    }

    // 判据 7：纵向仍能滚
    out.vertical = { scrollH: content.scrollHeight, clientH: content.clientHeight };
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
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-nav-layout-"));
const page = path.join(dir, "probe.html");
fs.writeFileSync(page, html, "utf8");

const cpRes = cp.spawnSync(
  browser,
  [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + path.join(dir, "profile"),
    "--window-size=1200,800", "--force-device-scale-factor=1",
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

let bad = 0;
const fail = (msg) => {
  console.log("FAIL: " + msg);
  bad++;
};
const ok = (msg) => console.log("  ok  " + msg);

for (const e of out.errors || []) fail("页面报错: " + e);

const s = out.shortBars || {};
const l = out.longBars || {};
console.log(`短内容：内容区 ${s.clientW}px（滚动宽 ${s.scrollW}）｜横条 ${s.hBar}px｜纵条 ${s.vBar}px`);
console.log(`长内容：内容区 ${l.clientW}px（滚动宽 ${l.scrollW}）｜横条 ${l.hBar}px｜纵条 ${l.vBar}px`);
if (out.longName) {
  console.log(
    `长文件名：格子 ${out.longName.clientW}px（内容 ${out.longName.scrollW}px）｜` +
      `text-overflow=${out.longName.overflowRule}｜行 title=${out.longName.rowTitle ? "有" : "无"}`
  );
}

// 判据 1：短内容不该有横向滚动条
s.hBar === 0 && s.scrollW <= s.clientW + 1
  ? ok(`短内容时没有横向滚动条（视口 ${s.clientW}px 装下 ${s.scrollW}px）`)
  : fail(`短内容就冒出了横向滚动条 / 内容超宽：横条 ${s.hBar}px、${s.scrollW}px vs ${s.clientW}px`);

// 判据 2：长内容也**不该**有横向滚动条（本方案就是不做横滚）
l.hBar === 0
  ? ok(`长内容时也没有横向滚动条（横条 ${l.hBar}px）—— 导航区不做横滚`)
  : fail(`长内容冒出了横向滚动条 ${l.hBar}px —— 本方案明确不做横滚，别让内容撑宽`);
l.scrollW <= l.clientW + 1
  ? ok(`内容没有超出视口（${l.scrollW}px ≤ ${l.clientW}px）：长名是被省略号收掉的，不是靠滚动读的`)
  : fail(`内容比视口宽 ${l.scrollW - l.clientW}px，却又没有横滚 —— 那部分内容谁也够不着`);

// 判据 3：长名确实走省略号
if (!out.longName) {
  fail("没找到那条长文件名的行（探针注入失败）");
} else {
  out.longName.overflowRule === "ellipsis" && out.longName.scrollW > out.longName.clientW
    ? ok(
        `长名走省略号（text-overflow=ellipsis，${out.longName.scrollW}px 的内容收进 ` +
          `${out.longName.clientW}px 的格子）`
      )
    : fail(
        `长名没按省略号收：text-overflow=${out.longName.overflowRule}、` +
          `${out.longName.scrollW}px vs 格子 ${out.longName.clientW}px`
      );
}

// 判据 4：行上有 title（= 完整路径）—— 这是"读全名"的唯一出口
out.rowsWithTitle === 0
  ? ok("每一行都带 title（悬停能读到完整路径）")
  : fail(`${out.rowsWithTitle} 行没有 title —— 长名被省略号收掉后就再也读不到全名了`);
out.longName && out.longName.rowTitle === EXPECT_TITLE
  ? ok(`长名行的 title 就是完整路径（${out.longName.rowTitle}）`)
  : fail(`长名行的 title 不对：得到 ${out.longName && out.longName.rowTitle}，期望 ${EXPECT_TITLE}`);

// 判据 5：目录行的刷新按钮右边缘对齐 + 都在可视区内
const rights = out.btnRights || [];
if (rights.length < 3) {
  fail(`量到的目录行太少（${rights.length}）—— 探针注入失败`);
} else {
  const spread = Math.max(...rights) - Math.min(...rights);
  spread <= 1
    ? ok(`所有目录行的刷新按钮右边缘对齐（${rights.length} 行，散布 ${spread}px）—— 位置永远整齐`)
    : fail(`刷新按钮没对齐：${rights.join(", ")}（散布 ${spread}px）`);
  out.navRect && Math.max(...rights) <= out.navRect.right + 2
    ? ok(`按钮都在可视区内（最右 ${Math.max(...rights)} ≤ 导航右界 ${out.navRect.right}）`)
    : fail(`有按钮被挤到可视区外：最右 ${Math.max(...rights)} > 导航右界 ${out.navRect && out.navRect.right}`);
}

// 判据 6：长目录行按钮在名字右侧（不遮字 —— 用户最初的报障）
out.longDir && out.longDir.name && out.longDir.btn
  ? out.longDir.btn.left >= out.longDir.name.right - 1
    ? ok(`长目录行：刷新按钮在名字右侧（名字右 ${out.longDir.name.right} ≤ 按钮左 ${out.longDir.btn.left}），不遮字`)
    : fail(`长目录行：按钮压在名字上（按钮左 ${out.longDir.btn.left} < 名字右 ${out.longDir.name.right}）`)
  : fail("没量到那条长目录行（探针注入失败）");

// 判据 7：纵向滚动仍正常
out.vertical && out.vertical.scrollH > out.vertical.clientH && l.vBar > 0
  ? ok(`纵向仍可滚（内容 ${out.vertical.scrollH}px > 视口 ${out.vertical.clientH}px）`)
  : fail(
      `长列表纵向滚不动了：内容 ${out.vertical && out.vertical.scrollH}px、` +
        `视口 ${out.vertical && out.vertical.clientH}px、纵条 ${l.vBar}px`
    );

if (bad) {
  console.log("nav-layout: " + bad + " 项不通过");
  process.exit(1);
}
console.log("nav-layout: 全部通过");
