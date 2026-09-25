#!/usr/bin/env node
/**
 * 记忆面板**真实几何**探针（需要本机装了 Edge/Chromium）
 *
 * 为什么需要它：记忆面板的四问（当前信念 / 凭什么 / 那时是什么 / 丢过什么）都是表格，
 * 而"表格会不会撑破容器、长值会不会换成横向滚动条、面板是不是只占半宽"这些问题**只存在于
 * 布局引擎里** —— Node 的 DOM 桩量不到像素，也量不到"值很长时它是换行还是把盒子撑宽"。
 * 与 bug 3（面板并排各占一半）同类：`showPane` 保证了"只有一块可见"，
 * 但"可见的那块真的铺满"、"内容有没有越界"必须在真浏览器里量。
 *
 * 做法：拼一个自包含页面（真 index.html 骨架 + 真 styles.css + 真 main.js + 真 memory.js，
 * 后端形参照旧走 `window.__TAURI__` 桩，桩里给**带长值**的记忆数据），在无头 Edge 里：
 * `MemoryUI.open()` → 量 → 点「凭什么」展开修订链 → 量 → 切到服务面板（互斥的反面）→ 量 →
 * 缩窄外层 → 再量。全程用**真的**面板与**真的** CSS。
 *
 * 走 CDP（不是 `--dump-dom`）：dump-dom 只出一次初始帧，之后 rAF 不跑、ResizeObserver 不再
 * 派发（实测），而"缩窄窗口要跟着变"正是这条链路的事。需要 Node 22+（内置 fetch / WebSocket），
 * 不引入任何 npm 依赖。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 面板**铺满**编辑区（宽高与容器同量级）—— 不是并排的半宽（bug 3 那一类）；
 *   2. 面板自己**不出现横向滚动条**：容器内 `scrollW <= clientW + 1`，页面也不横向滚；
 *   3. 唯一滚动容器是 `.mem-body`（纵向滚它，不是整块面板滚）；
 *   4. **超长的键/值不许撑破表格**：值变高（换行）而表格宽不涨；
 *   5. 展开「凭什么」修订链之后，上面第 2/4 条仍然成立；
 *   6. 切换到服务面板后记忆面板**必须隐藏**（互斥的反面：不许两块都在）；
 *   7. 外层缩窄时面板跟着缩，且仍然不出现横向滚动条。
 *
 * 用法：
 *   node scripts/memory-layout.js                # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   MSEDGE_PATH=<path> node scripts/memory-layout.js
 *   node scripts/memory-layout.js --shot=mem.png # 顺带出一张图，肉眼可核对
 */
const fs = require("fs");
const os = require("os");
const path = require("path");
const cp = require("child_process");

const ROOT = path.resolve(__dirname, "..");
const read = (p) => fs.readFileSync(path.join(ROOT, p), "utf8");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 记忆面板几何探针未运行（布局问题将不被覆盖）");
  process.exit(0);
}

const WIDE = 1100;
const NARROW = 720;

// ---- 拼页面：真实 HTML + 真实 CSS + 真实 main.js / memory.js ----
let html = read("ui/index.html")
  .replace(/<script[\s\S]*?<\/script>/g, "")
  .replace(/<link[^>]*>/g, "");
const css = read("ui/styles.css");
const enc = (s) => JSON.stringify(s).replace(/</g, "\\u003c");

// 桩：Tauri（记忆四问给**带长值**的固定数据）+ I18N（不拉语言文件）。必须在 main.js 之前执行。
const LONG_VALUE =
  "记忆库每天压缩一次，原始文件都保留着；这条故意写得很长很长，用来验证表格是换行而不是把容器撑宽。".repeat(3);
const stubs = `
window.__MEM_CALLS__ = [];
window.__ERRORS__ = [];
window.onerror = (m) => window.__ERRORS__.push(String((m && m.message) || m));
const __hits = () => ([
  { key: "build.command", value: "cargo tauri build --no-bundle", status: "active",
    valid_from: 1758700000, valid_to: null, prov: 3, score: 0 },
  { key: "tool.mvn", value: "D:\\\\Tools\\\\Maven\\\\bin\\\\mvn.cmd", status: "active",
    valid_from: 1758700000, valid_to: null, prov: 2, score: 0 },
  { key: "memory.backup.policy.very.long.key.name.for.layout.probe", value: ${enc(LONG_VALUE)},
    status: "contested", valid_from: 1758700000, valid_to: null, prov: 5, score: 0 },
]);
const __answers = {
  mem_status: () => ({ scope: "probe", db: "D:/probe/global/memory/mem.db", events: 42,
    beliefs_active: 3, beliefs_all: 7, receipts: 1, vectors: 3,
    embed_available: true, embed_reason: null }),
  mem_beliefs: () => ({ scope: "probe", hits: __hits() }),
  mem_why: () => ({ scope: "probe", chain: [
    { seq: 7, ts: 1758700000, kind: "obs", op: null, value: "cargo tauri build --no-bundle",
      origin: "probe", reason: null, prov: [], id: "e7" },
    { seq: 9, ts: 1758700100, kind: "rev", op: "supersede",
      value: "cargo tauri build --no-bundle", origin: "engine",
      reason: "旧值当时确实成立，只是后来换了工具链", prov: ["e7"], id: "e9" },
  ] }),
  mem_as_of: () => ({ scope: "probe", at: 1758700000, hits: __hits() }),
  mem_receipts: () => ({ scope: "probe", receipts: [
    { id: 3, ts: 1758700200, kind: "transcript_compaction", covered: 1,
      dropped: ["[user] 帮我把构建命令改成…", "[assistant] 已经改完了，验证方式是…"],
      rehydrate: "打开会话 sess-42 的第 1..8 条（原话一字未动）", note: "历史超出预算，压实了 8 条" },
  ] }),
  mem_rebuild: () => ({ replayed_events: 42 }),
  mem_record: () => ({ scope: "probe", id: "e99" }),
};
const invoke = (cmd, args) => {
  window.__MEM_CALLS__.push({ cmd, args: args || {} });
  const f = __answers[cmd];
  return Promise.resolve(f ? f() : {});
};
window.__TAURI__ = {
  core: { invoke },
  event: { listen: () => Promise.resolve(() => {}) },
};
window.I18N = { init: () => Promise.resolve(), t: (k) => k, getLang: () => "zh-CN", setLang: () => {} };
`;

// 驱动：开面板 → 量 → 展开修订链 → 量 → 切走（互斥）→ 量 → 缩窄 → 量
const driver = `
window.__PROBE_RESULT__ = (async function () {
  const out = { errors: [], notes: [] };
  const round = (n) => Math.round(n);
  const box = (el) => {
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { w: round(r.width), h: round(r.height) };
  };
  const bars = (el) => {
    if (!el) return null;
    return { clientW: el.clientWidth, clientH: el.clientHeight,
             scrollW: el.scrollWidth, scrollH: el.scrollHeight };
  };
  const vis = (id) => {
    const el = document.getElementById(id);
    return !!el && getComputedStyle(el).display !== "none";
  };
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const frame = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())));
  const settle = async (ms) => { await wait(ms); await frame(); await wait(60); await frame(); };

  const panel = () => document.getElementById("memory-view");
  const body = () => document.querySelector("#memory-view .mem-body");
  const table = () => document.querySelector("#mem-now-body");
  const snap = (tag) => {
    const p = panel();
    const host = p ? p.parentElement : null;
    const row = document.querySelector("#mem-now-body tr:last-child td:nth-child(2)");
    return {
      tag,
      panel: box(p),
      host: box(host),
      panelBars: bars(p),
      bodyBars: bars(body()),
      tableBars: bars(table()),
      docScrollW: document.documentElement.scrollWidth,
      winW: window.innerWidth,
      lastValueBox: box(row),
      cells: Array.from(document.querySelectorAll("#mem-now-body td")).map((td) => ({
        w: td.clientWidth,
        sw: td.scrollWidth,
        cls: (td.className || "").toString().slice(0, 18),
      })),
      vis: { memory: vis("memory-view"), service: vis("service-view"),
             config: vis("config-view"), editor: vis("editor-view") },
    };
  };

  try {
    // initApp 挂在 DOMContentLoaded 上；探针不需要真后端，摘掉它（面板函数本身仍是全局）
    document.removeEventListener("DOMContentLoaded", window.initApp);
    window.state.currentProject = { name: "probe", path: "D:/probe" };
    window.state.tabs = [];
    window.state.activeTabId = null;
    document.getElementById("app").style.width = "${WIDE}px";

    window.MemoryUI.open();
    await settle(400);
    const wide = snap("wide");

    // 展开「凭什么」：修订链是第二张表，它不许把面板撑出横向滚动条
    const link = document.querySelector("#mem-now-body button.mem-link");
    if (link) link.click();
    await settle(300);
    const why = snap("why");

    // 互斥的反面：切到服务面板 → 记忆面板必须隐藏
    window.showServiceView();
    await settle(200);
    const switched = snap("switched");

    // 切回来（复用一个已存在的标签页）
    window.MemoryUI.open();
    await settle(300);

    // 缩窄外层：面板跟着缩，且仍然不许出现横向滚动条
    document.getElementById("app").style.width = "${NARROW}px";
    await settle(500);
    const narrow = snap("narrow");

    // 诊断：内容区里**谁**越了界（只报"有溢出"等于让人自己去猜，那是探针的失职）
    const b = body();
    const bb = b ? b.getBoundingClientRect() : null;
    out.overflowers = bb
      ? Array.from(b.querySelectorAll("*"))
          .filter((el) => el.getBoundingClientRect().right > bb.right + 1)
          .slice(0, 6)
          .map((el) => ({
            tag: el.tagName.toLowerCase(),
            cls: (el.className || "").toString().slice(0, 40),
            right: round(el.getBoundingClientRect().right),
            w: round(el.getBoundingClientRect().width),
          }))
      : [];

    out.wide = wide;
    out.why = why;
    out.switched = switched;
    out.narrow = narrow;
    out.calls = window.__MEM_CALLS__.map((c) => c.cmd);
    out.errors = window.__ERRORS__;
  } catch (err) {
    out.errors.push("DRIVER: " + ((err && err.stack) || err));
  }
  return out;
})();
`;

html = html.replace(
  /<\/body>/,
  () =>
    `<style>${css}</style>` +
    `<script>${stubs}</script>` +
    `<script>eval(${enc(read("ui/main.js"))})</script>` +
    `<script>eval(${enc(read("ui/memory.js"))})</script>` +
    `<script>${driver}</script></body>`
);

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-mem-layout-"));
const page = path.join(dir, "probe.html");
fs.writeFileSync(page, html, "utf8");

const url = "file:///" + page.replace(/\\/g, "/");

const child = cp.spawn(
  browser,
  [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + path.join(dir, "profile"),
    `--window-size=${WIDE + 40},760`, "--force-device-scale-factor=1",
    "--remote-debugging-port=0", url,
  ],
  { stdio: ["ignore", "pipe", "pipe"] }
);

function waitForEndpoint(timeoutMs = 20000) {
  return new Promise((resolve, reject) => {
    let buf = "";
    const t = setTimeout(() => reject(new Error("等不到 DevTools 端点")), timeoutMs);
    child.stderr.on("data", (d) => {
      buf += d.toString();
      const m = buf.match(/ws:\/\/127\.0\.0\.1:(\d+)\//);
      if (m) {
        clearTimeout(t);
        resolve(`http://127.0.0.1:${m[1]}`);
      }
    });
    child.on("exit", (code) => {
      clearTimeout(t);
      reject(new Error("浏览器提前退出，码 " + code));
    });
  });
}

function cdp(ws) {
  let id = 0;
  const pending = new Map();
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
    }
  });
  return (method, params = {}) =>
    new Promise((resolve, reject) => {
      const myId = ++id;
      pending.set(myId, { resolve, reject });
      ws.send(JSON.stringify({ id: myId, method, params }));
    });
}

async function openCdp(endpoint) {
  for (let i = 0; i < 60; i++) {
    try {
      const list = await (await fetch(`${endpoint}/json/list`)).json();
      const target = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
      if (target) {
        const ws = new WebSocket(target.webSocketDebuggerUrl);
        await new Promise((res, rej) => {
          ws.addEventListener("open", res, { once: true });
          ws.addEventListener("error", () => rej(new Error("WebSocket 打不开")), { once: true });
        });
        return ws;
      }
    } catch {
      /* 端口还没起来，继续等 */
    }
    await sleep(200);
  }
  throw new Error("CDP 目标没出现");
}

async function cleanup() {
  try {
    child.kill();
  } catch {}
  for (let i = 0; i < 20; i++) {
    if (child.exitCode !== null || child.signalCode) break;
    await sleep(100);
  }
  for (let i = 0; i < 5; i++) {
    try {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
      return;
    } catch {
      await sleep(300);
    }
  }
}

(async () => {
  let bad = 0;
  const fail = (msg) => {
    console.log("FAIL: " + msg);
    bad++;
  };
  const ok = (msg) => console.log("  ok  " + msg);

  let out;
  let sawGlobal = false;
  try {
    const endpoint = await waitForEndpoint();
    const ws = await openCdp(endpoint);
    const send = cdp(ws);
    for (let i = 0; i < 100; i++) {
      const r = await send("Runtime.evaluate", {
        expression: "document.readyState === 'complete'",
        returnByValue: true,
      });
      if (r.result && r.result.value === true) break;
      await sleep(100);
    }
    for (let i = 0; i < 50; i++) {
      const g = await send("Runtime.evaluate", {
        expression: "typeof window.__PROBE_RESULT__",
        returnByValue: true,
      });
      if (g.result && g.result.value === "object") {
        sawGlobal = true;
        break;
      }
      await sleep(100);
    }
    const shotArg = process.argv.slice(2).find((a) => a.startsWith("--shot="));
    if (shotArg) {
      const want = shotArg.split("=")[1];
      const shotPath = path.isAbsolute(want) ? want : path.resolve(process.cwd(), want);
      const img = await send("Page.captureScreenshot", { format: "png" });
      fs.writeFileSync(shotPath, Buffer.from(img.data, "base64"));
      console.log(`  截图: ${shotPath}`);
    }
    for (let i = 0; i < 3; i++) {
      const res = await send("Runtime.evaluate", {
        expression: "window.__PROBE_RESULT__",
        awaitPromise: true,
        returnByValue: true,
      });
      if (res.exceptionDetails) {
        throw new Error("页面里抛异常：" + JSON.stringify(res.exceptionDetails).slice(0, 400));
      }
      out = res.result.value;
      if (out) break;
      await sleep(500);
    }
    ws.close();
  } catch (err) {
    console.log("FAIL: 探针没能跑起来：" + ((err && err.message) || err));
    await cleanup();
    process.exit(1);
  }
  await cleanup();

  for (const e of (out && out.errors) || []) fail("页面报错: " + e);
  if (!out) {
    console.log(
      "FAIL: 页面没交回结果（window.__PROBE_RESULT__ 取到 " +
        (sawGlobal ? "有定义但没解析出值" : "undefined") +
        "）—— 这是**探针自身**没取到结果（CDP/竞态），不是任何几何判据不通过；判据这一轮**没被评估过**"
    );
    console.log("memory-layout: 1 项不通过");
    process.exit(1);
  }

  const wide = out.wide || {};
  const why = out.why || {};
  const switched = out.switched || {};
  const narrow = out.narrow || {};
  console.log(
    `宽屏：面板 ${wide.panel && wide.panel.w}×${wide.panel && wide.panel.h}px｜` +
      `容器 ${wide.host && wide.host.w}×${wide.host && wide.host.h}px｜` +
      `窄屏：面板 ${narrow.panel && narrow.panel.w}px｜表格 ${wide.tableBars && wide.tableBars.scrollW}/${wide.tableBars && wide.tableBars.clientW}`
  );

  const near = (a, b, tol) => a != null && b != null && Math.abs(a - b) <= tol;

  // 1. 铺满：宽高都与容器同量级（半宽并排会差一倍）
  if (wide.panel && wide.host) {
    if (near(wide.panel.w, wide.host.w, 3) && near(wide.panel.h, wide.host.h, 3)) {
      ok(`面板铺满编辑区（${wide.panel.w}×${wide.panel.h} vs ${wide.host.w}×${wide.host.h}）`);
    } else {
      fail(
        `面板没有铺满编辑区：面板 ${wide.panel.w}×${wide.panel.h} vs 容器 ${wide.host.w}×${wide.host.h}` +
          `（并排半宽是 bug 3 那一类：只有一块可见还不够，可见的那块要占满）`
      );
    }
  } else {
    fail("量不到面板或容器 —— 面板可能没建出来（memory.js 没跑 / showPane 没点亮）");
  }

  // 2/3. 没有横向滚动条 + 唯一纵向滚动容器是 .mem-body
  const noH = (snapObj, tag) => {
    const p = snapObj.panelBars;
    const b = snapObj.bodyBars;
    if (!p || !b) return fail(`${tag}: 量不到面板/内容区的框`);
    if (p.scrollW <= p.clientW + 1) ok(`${tag}: 面板无横向滚动条（${p.scrollW} <= ${p.clientW}）`);
    else fail(`${tag}: 面板出现横向滚动条（scrollW ${p.scrollW} > clientW ${p.clientW}）`);
    if (snapObj.docScrollW <= snapObj.winW + 1) ok(`${tag}: 页面不横向滚（${snapObj.docScrollW} <= ${snapObj.winW}）`);
    else fail(`${tag}: 页面横向溢出（${snapObj.docScrollW} > ${snapObj.winW}）`);
    if (p.scrollH <= p.clientH + 1) ok(`${tag}: 唯一纵向滚动容器是内容区（面板自身不滚）`);
    else fail(`${tag}: 面板自身也在纵向滚（scrollH ${p.scrollH} > clientH ${p.clientH}）`);
    // 内容区不许有东西**真的出界**。
    //
    // 注意：这里**不**用 `scrollW <= clientW` —— Chrome 在 `overflow:auto` 的盒子上会把
    // 右内边距算进 scrollWidth，窄屏下恒定多出 4px（实测：表格 202/202、每个单元格
    // scrollW==clientW，内容区却报 234>230）。拿它当判据就是拿浏览器的会计口径当缺陷。
    // 真正要看的是"有没有元素/文本伸出内容盒"：元素级看右边缘，文本级看单元格自身。
    const outEls = (snapObj.overflowers || []).length;
    const outCells = (snapObj.cells || []).filter((c) => c.sw > c.w + 1).length;
    if (outEls === 0 && outCells === 0) {
      ok(`${tag}: 内容区没有东西出界（出界元素 0 · 出界单元格 0；scrollW/clientW ${b.scrollW}/${b.clientW} 只是 padding 会计）`);
    } else {
      fail(
        `${tag}: 内容区有 ${outEls} 个元素出界、${outCells} 个单元格文本溢出` +
          `｜元素 ${JSON.stringify((snapObj.overflowers || []).slice(0, 4))}` +
          `｜单元格 ${JSON.stringify((snapObj.cells || []).filter((c) => c.sw > c.w + 1).slice(0, 3))}`
      );
    }
  };
  noH(wide, "打开");
  noH(why, "展开修订链");

  // 4. 超长值不许撑破表格：表格宽不超容器，但那一格**变高**了（换行）
  const tb = wide.tableBars;
  if (tb && wide.panelBars) {
    if (tb.scrollW <= wide.panelBars.clientW + 1) {
      ok(`超长值没撑破表格（${tb.scrollW} <= 容器 ${wide.panelBars.clientW}）`);
    } else {
      fail(`超长值把表格撑破了：${tb.scrollW} > 容器 ${wide.panelBars.clientW}`);
    }
  }
  if (wide.lastValueBox && wide.lastValueBox.h > 24) {
    ok(`超长值走的是换行（那一格高 ${wide.lastValueBox.h}px）`);
  } else {
    fail(
      `超长值那一格只有 ${wide.lastValueBox && wide.lastValueBox.h}px 高 —— 要么没渲染，` +
        `要么被压成一行溢出（该换行）`
    );
  }

  // 5. 互斥的反面：切到服务面板后记忆面板必须隐藏
  if (switched.vis && switched.vis.memory === false && switched.vis.service === true) {
    ok("切到服务面板后记忆面板隐藏（互斥成立）");
  } else {
    fail(
      `互斥不成立：记忆 ${switched.vis && switched.vis.memory} / 服务 ${switched.vis && switched.vis.service}` +
        `（两块同时可见 = bug 3 复发）`
    );
  }

  // 6. 缩窄跟着缩
  if (narrow.panel && wide.panel && narrow.panel.w < wide.panel.w - 100) {
    ok(`缩窄时面板跟着缩（${wide.panel.w} → ${narrow.panel.w}px）`);
  } else {
    fail(`缩窄时面板没跟着缩：${wide.panel && wide.panel.w} → ${narrow.panel && narrow.panel.w}px`);
  }
  noH(narrow, "窄屏");

  // 7. 命令真的被调过（面板不是空壳）
  const need = ["mem_status", "mem_beliefs", "mem_receipts"];
  const miss = need.filter((c) => !(out.calls || []).includes(c));
  if (miss.length === 0) ok("面板拉到了 mem_status / mem_beliefs / mem_receipts");
  else fail(`面板没调用这些命令：${miss.join(", ")}`);

  console.log(bad === 0 ? "memory-layout: 全部达预期" : `memory-layout: ${bad} 项不通过`);
  process.exit(bad === 0 ? 0 : 1);
})();
