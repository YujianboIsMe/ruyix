#!/usr/bin/env node
/**
 * 导航栏**真实几何**探针（需要本机装了 Edge/Chromium）
 *
 * 需求（用户报）：现状导航栏没有水平滚动条；期望"**过宽时**加上水平滚动条"。
 * 为什么需要真浏览器：这件事全在布局引擎里 —— 长文件名到底是被省略号吃掉了、还是把行撑宽了、
 * 侧边栏到底出没出横向滚动条，Node 的 DOM 桩里没有布局引擎，量不到。
 *
 * 做法：把**真实的** ui/index.html（剥掉 <script>/<link>）+ ui/styles.css 拼成自包含页面，
 * 用真 markup 往 `#file-tree` / `#project-list` 塞行（形状照抄 main.js::renderTreeEntry：
 * `.tree-node[style=padding-left] > .tree-icon + .tree-name + button.tree-refresh`），
 * 然后在无头 Edge 里量两种场景：
 *
 *   场景 A（短内容）—— 期望：**不出现**横向滚动条（"过宽时"才加，不是常驻）；
 *   场景 B（长内容）—— 期望：横向滚动条出现，且长名不再被省略号吃掉（能滚过去读到全名）。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. A：`.nav-content` 横向滚动条为 0；
 *   2. B：`.nav-content` 出现横向滚动条（hBar > 0）且真的可滚（scrollWidth > clientWidth）；
 *   3. B：长文件名**没有被压缩**（`.tree-name` 的 scrollWidth ≈ clientWidth）——
 *      这是"能看到全名"的前提，省略号一旦生效，滚动条再宽也没意义；
 *   4. B：纵向滚动仍然正常（长列表能上下滚），两个方向不互相破坏；
 *   5. B：**普通短行**里的刷新按钮仍在可视区内（不被"行被撑到最宽"这种修法带出屏幕外）。
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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 导航栏几何探针未运行（横向滚动条将不被覆盖）");
  process.exit(0);
}

let html = read("ui/index.html")
  .replace(/<script[\s\S]*?<\/script>/g, "")
  .replace(/<link[^>]*>/g, "");
const css = read("ui/styles.css");

const LONG_NAME =
  "ApplicationPropertiesOfTheCloudShopAuthorityServiceIntegrationEnvironment.java";
const LONG_PATH = "D:\\Projects\\Java\\cloud-shop\\cloud-shop-authority\\src\\main\\resources";

const driver = `
(function () {
  const out = { errors: [], notes: [] };
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
  const fileRow = (name, depth, isDir) => {
    const node = document.createElement("div");
    node.className = "tree-node";
    node.style.paddingLeft = depth * 16 + "px";
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
    // 只留导航面板 active（真机上点哪个标签就是哪个）
    document.querySelectorAll(".nav-panel").forEach((p) => p.classList.remove("active"));

    // 先量**终端面板**（同一类"名称型列表" + 行右缘那个 🪟 图标）：改个长名再量
    document.getElementById("nav-panel-terminal").classList.add("active");
    const tlist = document.getElementById("terminal-list");
    const firstTname = tlist && tlist.querySelector(".terminal-name");
    if (firstTname) firstTname.textContent = "PowerShell (Windows Terminal) — 超长名字" + "X".repeat(40);
    const tContent = document.querySelector(".nav-content");
    out.termBars = bars(tContent);
    const tIcon = tlist && tlist.querySelector(".terminal-new-window");
    out.termIcon = tIcon ? { right: round(tIcon.getBoundingClientRect().right) } : null;

    // 再换回文件面板做 A/B 两个场景
    document.querySelectorAll(".nav-panel").forEach((p) => p.classList.remove("active"));
    document.getElementById("nav-panel-files").classList.add("active");
    const tree = document.getElementById("file-tree");
    const content = document.querySelector(".nav-content");
    const nav = document.getElementById("navigator");

    // ---- 场景 A：短内容 ----
    tree.innerHTML = "";
    ["pom.xml", "src", "README.md"].forEach((n) => tree.appendChild(fileRow(n, 0, n === "src")));
    out.shortBars = bars(content);

    // ---- 场景 B：长内容（一个名字极长的**目录**：它带刷新按钮，正好验"按钮遮不遮字"）----
    tree.innerHTML = "";
    tree.appendChild(fileRow("cloud-shop-authority", 0, true));
    tree.appendChild(fileRow("src", 1, true));
    tree.appendChild(fileRow("main", 2, true));
    for (let i = 0; i < 40; i++) {
      tree.appendChild(
        i === 7
          ? fileRow("integration-environment-application-properties-for-cloud-shop-authority", 3, true)
          : fileRow("File" + i + ".java", 3, false)
      );
    }
    tree.appendChild(fileRow("${LONG_NAME}", 3, false));
    // 项目列表面板也塞一条长路径（同一类"过宽"来源）
    const plist = document.getElementById("project-list");
    plist.innerHTML = "";
    const card = document.createElement("div");
    card.className = "project-list-item";
    const picon = document.createElement("span");
    picon.className = "project-icon";
    picon.textContent = "☕";
    const pinfo = document.createElement("div");
    pinfo.className = "project-info";
    const pname = document.createElement("div");
    pname.className = "project-name";
    pname.textContent = "cloud-shop-admin-web-frontend";
    const ppath = document.createElement("div");
    ppath.className = "project-path";
    ppath.textContent = "${LONG_PATH}";
    pinfo.appendChild(pname);
    pinfo.appendChild(ppath);
    card.appendChild(picon);
    card.appendChild(pinfo);
    plist.appendChild(card);

    // 量：列表面板（此时 active）
    out.longBars = bars(content);
    out.navBars = bars(nav);
    out.navRect = (() => {
      const r = nav.getBoundingClientRect();
      return { left: round(r.left), right: round(r.right), w: round(r.width) };
    })();

    // 长名有没有被压缩（省略号吃掉）
    const rectOf = (el) => {
      const r = el.getBoundingClientRect();
      return { left: round(r.left), right: round(r.right) };
    };
    const names = [...document.querySelectorAll(".tree-name")];
    const longEl = names.find((e) => e.textContent === "${LONG_NAME}");
    out.longName = longEl
      ? { clientW: longEl.clientWidth, scrollW: longEl.scrollWidth, ...rectOf(longEl) }
      : null;
    out.ellipsisRule = longEl ? getComputedStyle(longEl).textOverflow : null;

    // **按钮遮不遮字**（本单的原始瑕疵）：拿一个名字很长的**目录**行来量
    const longDirRow = [...document.querySelectorAll(".tree-node")].find((n) =>
      n.textContent.includes("integration-environment-application-properties")
    );
    if (longDirRow) {
      const nm = longDirRow.querySelector(".tree-name");
      const bt = longDirRow.querySelector("button.tree-refresh");
      out.longDir = { name: rectOf(nm), btn: rectOf(bt), nameClientW: nm.clientWidth, nameScrollW: nm.scrollWidth };
    }
    // 短目录行（不滚就该看见按钮）
    const shortDirRow = [...document.querySelectorAll(".tree-node")].find(
      (n) => n.querySelector("button.tree-refresh") && (n.querySelector(".tree-name") || {}).textContent === "src"
    );
    if (shortDirRow) {
      out.shortDir = {
        name: rectOf(shortDirRow.querySelector(".tree-name")),
        btn: rectOf(shortDirRow.querySelector("button.tree-refresh")),
      };
    }

    // 真能横向滚过去：滚到底，看长名与长行的按钮是否都进入可视区
    content.scrollLeft = content.scrollWidth;
    out.scrolled = { scrollLeft: round(content.scrollLeft), maxScroll: content.scrollWidth - content.clientWidth };
    out.longNameAfterScroll = longEl ? rectOf(longEl) : null;
    out.longDirBtnAfterScroll = longDirRow ? rectOf(longDirRow.querySelector("button.tree-refresh")) : null;
    content.scrollLeft = 0;

    // 纵向仍然能滚
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
console.log(
  `短内容：内容区 ${s.clientW}px（滚动宽 ${s.scrollW}）｜横条 ${s.hBar}px｜纵条 ${s.vBar}px`
);
console.log(
  `长内容：内容区 ${l.clientW}px（滚动宽 ${l.scrollW}）｜横条 ${l.hBar}px｜纵条 ${l.vBar}px`
);
if (out.longName) {
  console.log(
    `长文件名：宽 ${out.longName.clientW}px（内容 ${out.longName.scrollW}px）｜` +
      `scrollLeft 能到 ${out.scrolled && out.scrolled.scrollLeft}px（上限 ${out.scrolled && out.scrolled.maxScroll}）`
  );
}

// 判据 1：短内容不该有横向滚动条
s.hBar === 0
  ? ok("短内容时不出现横向滚动条（「过宽时才加」）")
  : fail(`短内容就出现了横向滚动条 ${s.hBar}px —— 那就不叫"过宽时"了`);

// 判据 2：长内容要出横向滚动条，且真的可滚
l.hBar > 0
  ? ok(`长内容时出现横向滚动条（${l.hBar}px，内容 ${l.scrollW}px > 视口 ${l.clientW}px）`)
  : fail(
      `长内容时仍然没有横向滚动条：内容区 ${l.clientW}px、内容宽 ${l.scrollW}px —— ` +
        `要么内容被压缩到视口内（等于看不见全名），要么根本没让它撑宽`
    );
l.scrollW > l.clientW
  ? ok(`横向真能滚：可滚 ${l.scrollW - l.clientW}px`)
  : fail("横向没有可滚的余量");

// 判据 3：长名没有被省略号吃掉（能被完整读到）
if (!out.longName) {
  fail("没找到那条长文件名的行（探针注入失败）");
} else {
  out.longName.scrollW <= out.longName.clientW + 1
    ? ok(`长文件名没被压缩（${out.longName.clientW}px 装下 ${out.longName.scrollW}px 的内容）`)
    : fail(
        `长文件名被截断了：装不下（内容 ${out.longName.scrollW}px vs 格子 ${out.longName.clientW}px）` +
          `，text-overflow=${out.ellipsisRule} —— 省略号一旦生效，横向滚动条再宽也读不全`
      );
  const maxScroll = (out.scrolled && out.scrolled.maxScroll) || 0;
  const after = out.longNameAfterScroll || {};
  maxScroll > 0 && after.right <= (out.navRect && out.navRect.right) + 2
    ? ok("滚到底时长文件名整条进入可视区")
    : fail(
        `滚到底后长名仍在视口外（右边缘 ${after.right} vs 导航右界 ${out.navRect && out.navRect.right}，` +
          `可滚 ${maxScroll}px）`
      );
}

// 判据 4：纵向还正常
out.vertical && out.vertical.scrollH > out.vertical.clientH && l.vBar > 0
  ? ok(`纵向仍可滚（内容 ${out.vertical.scrollH}px > 视口 ${out.vertical.clientH}px）`)
  : fail(`长列表纵向滚不动了：内容 ${out.vertical && out.vertical.scrollH}px、视口 ${out.vertical && out.vertical.clientH}px、纵条 ${l.vBar}px`);

// 判据 5：**刷新按钮不许盖在路径文字上**（用户报的瑕疵：长路径触发横滚时刷新符号压在路径上）
const noOverlap = (row, tag) => {
  if (!row || !row.name || !row.btn) {
    fail(`没量到${tag}的名字/按钮（探针注入失败）`);
    return;
  }
  row.btn.left >= row.name.right - 1
    ? ok(`${tag}：刷新按钮在名字右侧（名字右 ${row.name.right} ≤ 按钮左 ${row.btn.left}），没有遮挡`)
    : fail(
        `${tag}：刷新按钮**盖在路径文字上**（按钮左 ${row.btn.left} < 名字右 ${row.name.right}）` +
          ` —— 别用 position:sticky 把按钮钉在可视区右缘`
      );
};
noOverlap(out.longDir, "长目录行");
noOverlap(out.shortDir, "短目录行");
out.longDir && out.longDir.nameScrollW <= out.longDir.nameClientW + 1
  ? ok(`长目录名的文字也没被压缩（${out.longDir.nameClientW}px 装下 ${out.longDir.nameScrollW}px）`)
  : fail(
      `长目录名被压缩了（${out.longDir && out.longDir.nameClientW} vs ${out.longDir && out.longDir.nameScrollW}）`
    );

// 判据 6：短行的按钮**不滚就能点**；长行的按钮**滚到最右能点**
out.shortDir && out.navRect && out.shortDir.btn.right <= out.navRect.right + 2
  ? ok(`短目录行的刷新按钮不滚就在可视区（右 ${out.shortDir.btn.right} ≤ 导航右界 ${out.navRect.right}）`)
  : fail(
      `短目录行的刷新按钮跑到可视区外（右 ${out.shortDir && out.shortDir.btn.right} > ` +
        `导航右界 ${out.navRect && out.navRect.right}）`
    );
out.longDirBtnAfterScroll &&
out.navRect &&
out.longDirBtnAfterScroll.right <= out.navRect.right + 2 &&
out.longDirBtnAfterScroll.left >= out.navRect.left - 2
  ? ok(
      `长目录行滚到最右时刷新按钮进入可视区（${out.longDirBtnAfterScroll.left}..${out.longDirBtnAfterScroll.right}）`
    )
  : fail(
      `长目录行滚到最右按钮仍不在可视区（${JSON.stringify(out.longDirBtnAfterScroll)} vs 导航 ` +
        `${JSON.stringify(out.navRect)}）`
    );

if (bad) {
  console.log("nav-layout: " + bad + " 项不通过");
  process.exit(1);
}
console.log("nav-layout: 全部通过");
