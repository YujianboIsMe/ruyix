#!/usr/bin/env node
/**
 * 模拟终端**真实几何**探针（需要本机装了 Edge/Chromium）
 *
 * 为什么需要它：模拟终端（xterm.js + Rust PTY）的尺寸问题只存在于**布局引擎**里 ——
 * Node 的 DOM 桩量不到 `.xterm-screen` 的像素尺寸，也量不到"容器缩小时它跟不跟"。
 * 真机上出过一次事故（用户报）：终端只占一块 763×368（= 创建时那 100×24 的像素尺寸），
 * 窗口怎么变都不动。根因两条：
 *   ① `new Terminal({rows:24, cols:100})` 之后**没有任何 resize 路径**（前端从不调后端的
 *      `pty_resize`，也没有观察容器尺寸）；
 *   ② CSS 里 `.terminal-container { min-width: fit-content }` 把容器撑到内容（xterm 那一块
 *      固定像素）的宽度 —— 窗口变小时容器不肯缩，于是连"尺寸变化"这件事都观察不到，还多出
 *      一根横向滚动条。
 *
 * 做法：拼一个自包含页面（真 index.html 骨架 + 真 styles.css/xterm.css + 真 xterm.js +
 * 真 main.js，形参照旧走 `window.__TAURI__` 桩），在无头 Edge 里：`showTerminalView()` →
 * `spawnTerminal()` → 量 → **缩小外层** → 再量。全程用**真的 xterm**，量到的就是真机上那块屏。
 *
 * 为什么走 CDP（而不是像 editor-layout.js 那样 `--dump-dom`）：**`--dump-dom` 模式下
 * 只出一次初始帧，之后 rAF 不跑、ResizeObserver 不再派发**（实测：最小页面里挂上观察器后
 * 改宽度能收到 1 次；在真页面里改宽度收到 0 次，还会把等 rAF 的脚本挂死）。而"尺寸变了要重新
 * 适配"恰恰是这条链路的事 —— 只能走 CDP（`Runtime.evaluate` + awaitPromise）。
 * 需要 Node 22+（内置 fetch / WebSocket），不引入任何 npm 依赖。
 *
 * 判据（任何一条不成立即退出码 1）：
 *   1. 打开后终端**铺满容器**（差值与边距/回滚条同量级），而不是停在 100×24；
 *   2. 外层缩小时终端**跟着缩**（cols 变小、屏幕像素变小）—— 用户报的 bug 的反面；
 *   3. 每次 xterm 尺寸变化都同步调了 `pty_resize`，rows/cols 与 xterm 自己的一致；
 *   4. 终端区**没有**横向滚动条（唯一滚动容器是 xterm 自己的 `.xterm-viewport`）。
 *
 * 用法：
 *   node scripts/terminal-layout.js               # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   MSEDGE_PATH=<path> node scripts/terminal-layout.js
 *   node scripts/terminal-layout.js --shot=term.png    # 顺带出一张图，肉眼可核对
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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 终端几何探针未运行（尺寸问题将不被覆盖）");
  process.exit(0);
}

const WIDE = 1100; // 起始宽度（和真机差不多量级）
const NARROW = 720; // 缩小后的宽度

// ---- 拼页面：真实 HTML + 真实 CSS（含 xterm.css）+ 真实 xterm.js + 真实 main.js ----
let html = read("ui/index.html")
  .replace(/<script[\s\S]*?<\/script>/g, "")
  .replace(/<link[^>]*>/g, "");
const css = read("ui/styles.css") + "\n" + read("ui/packages/xterm.css");
const enc = (s) => JSON.stringify(s).replace(/</g, "\\u003c");

// 桩：Tauri（记录 pty_* 调用）+ I18N（不拉语言文件）。必须在 main.js 之前执行。
const stubs = `
window.__PTY_CALLS__ = [];
window.__ERRORS__ = [];
window.onerror = (m) => window.__ERRORS__.push(String((m && m.message) || m));
const invoke = (cmd, args) => {
  window.__PTY_CALLS__.push({ cmd, args: args || {} });
  return Promise.resolve({});
};
window.__TAURI__ = {
  core: { invoke },
  event: { listen: () => Promise.resolve(() => {}) },
};
window.I18N = { init: () => Promise.resolve(), t: (k) => k, getLang: () => "zh-CN", setLang: () => {} };
`;

// 驱动：量宽屏 → 缩小 → 量 → 放大 → 量。结果挂在 window.__PROBE_RESULT__（Promise）上等 CDP 取。
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
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const frame = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())));
  const settle = async (ms) => { await wait(ms); await frame(); await wait(60); await frame(); };
  const snap = (tag) => {
    const view = document.getElementById("terminal-view");
    const cont = document.getElementById("terminal-container");
    const screen = document.querySelector("#terminal-container .xterm-screen");
    const tab = (window.state.tabs || []).find((t) => t._isTerminal);
    const term = tab && tab._term;
    return {
      tag,
      outer: box(document.getElementById("app")),
      container: box(cont), screen: box(screen),
      viewBars: bars(view), containerBars: bars(cont),
      cols: term ? term.cols : null, rows: term ? term.rows : null,
      inner: cont ? { w: cont.clientWidth, h: cont.clientHeight } : null,
    };
  };
  try {
    // initApp 挂在 DOMContentLoaded 上；探针不需要真后端，摘掉它（spawnTerminal 等仍是全局函数）
    document.removeEventListener("DOMContentLoaded", window.initApp);
    window.showTerminalView();
    document.getElementById("app").style.width = "${WIDE}px";

    await window.spawnTerminal("PowerShell", "cmd");
    await settle(400);
    const wide = snap("wide");

    // 自己挂一个观察器（**留引用**，否则会被 GC 掉）：量清"尺寸事件到底来没来"
    let roFired = 0;
    let roSaw = null;
    window.__DIAG_RO__ = new ResizeObserver((entries) => {
      roFired++;
      const e = entries[0];
      roSaw = e ? { w: round(e.contentRect.width), h: round(e.contentRect.height) } : null;
    });
    window.__DIAG_RO__.observe(document.getElementById("terminal-container"));

    // 缩小外层（模拟窗口变小）→ 应当自动重新适配（fitTerminal 有 60ms 防抖）
    document.getElementById("app").style.width = "${NARROW}px";
    await settle(600);
    const narrow = snap("narrow");

    // 诊断：直接走生产那条防抖路径，看它能不能跟上（分得清"没触发"还是"逻辑不对"）
    window.scheduleTerminalFit();
    await settle(250);
    const viaSchedule = snap("viaSchedule");

    // 再放大回去，确认双向都跟
    document.getElementById("app").style.width = "${WIDE}px";
    await settle(600);
    const back = snap("back");

    out.wide = wide;
    out.narrow = narrow;
    out.viaSchedule = viaSchedule;
    out.back = back;
    out.roFired = roFired;
    out.roSaw = roSaw;
    out.ptyCalls = window.__PTY_CALLS__.filter((c) => c.cmd === "pty_resize");
    out.spawnCalls = window.__PTY_CALLS__.filter((c) => c.cmd === "pty_spawn").length;
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
    `<script>eval(${enc(read("ui/packages/xterm.js"))})</script>` +
    `<script>eval(${enc(read("ui/scripts/main.js"))})</script>` +
    `<script>${driver}</script></body>`
);

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruyix-term-layout-"));
const page = path.join(dir, "probe.html");
fs.writeFileSync(page, html, "utf8");

const url = "file:///" + page.replace(/\\/g, "/");

// ---- 起无头浏览器 + CDP ----
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

/** 从 stderr 里读 "DevTools listening on ws://127.0.0.1:PORT/..." */
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

/** 极简 CDP 客户端：send(method, params) → Promise<result> */
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

/** 收工：先杀浏览器并**等它真的退出**，否则 profile 目录还被占着，rm 会 EPERM */
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
  // 删不掉就算了：临时目录，别让收尾把自己搞挂
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

    // 等页面加载完（file:// 立刻完），再把驱动那个 Promise 取回来
    for (let i = 0; i < 100; i++) {
      const r = await send("Runtime.evaluate", {
        expression: "document.readyState === 'complete'",
        returnByValue: true,
      });
      if (r.result && r.result.value === true) break;
      await sleep(100);
    }
    // 驱动脚本在**解析时**就把 Promise 挂在 `window.__PROBE_RESULT__` 上了，所以正常情况
    // 这一句立刻通过。留着这段"先看有没有"是因为反过来会很难查：取不到结果时下面的
    // `res.result.value` 只是 undefined，最后只印一句"1 项不通过" ——
    // **红一次却没有证据**（实测在负载重的机器上偶发过一次，ui-smoke 那边的简报里只有摘要）。
    sawGlobal = false;
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
    // 取结果：拿不到就**重试**（上面的竞态是偶发的，重试一次比让人看一句"1 项不通过"划算）。
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
    // 这一支以前只印一句"1 项不通过"：红的**没有证据**，看不出是几何不通过还是探针没取到结果。
    // 两者必须分得开 —— 前者是真缺陷，后者是探针/浏览器层的竞态，处置完全不同。
    console.log(
      "FAIL: 页面没交回结果（window.__PROBE_RESULT__ 取到 " + (sawGlobal ? "有定义但没解析出值" : "undefined") +
        "）—— 这是**探针自身**没取到结果（CDP/竞态），不是任何几何判据不通过；判据这一轮**没被评估过**"
    );
    console.log("terminal-layout: 1 项不通过");
    process.exit(1);
  }

  const wide = out.wide || {};
  const narrow = out.narrow || {};
  const back = out.back || {};
  console.log(
    `宽屏：外层 ${wide.outer && wide.outer.w}px｜容器 ${wide.inner && wide.inner.w}×${wide.inner && wide.inner.h}px｜` +
      `屏幕 ${wide.screen && wide.screen.w}×${wide.screen && wide.screen.h}px｜cols×rows ${wide.cols}×${wide.rows}`
  );
  console.log(
    `窄屏：外层 ${narrow.outer && narrow.outer.w}px｜容器 ${narrow.inner && narrow.inner.w}×${narrow.inner && narrow.inner.h}px｜` +
      `屏幕 ${narrow.screen && narrow.screen.w}×${narrow.screen && narrow.screen.h}px｜cols×rows ${narrow.cols}×${narrow.rows}`
  );

  if (!wide.screen || !wide.inner) {
    fail("没量到 .xterm-screen —— 终端没被打开（spawnTerminal 走通了吗？）");
  } else {
    // 判据 1：铺满容器（不是停在创建时的 100×24）。
    // 容差给宽：`.xterm` 自己的 padding + 容器 padding + 回滚条宽度都不是格子 ——
    // 判据是"差值在几圈边距之内"，不是逐像素相等。
    const dw = Math.abs(wide.screen.w - wide.inner.w);
    const dh = Math.abs(wide.screen.h - wide.inner.h);
    dw <= 40 && dh <= 40
      ? ok(`打开后就铺满容器（屏幕与容器差 ${dw}×${dh}px —— 边距/回滚条的量级内）`)
      : fail(
          `终端没有铺满容器：屏幕 ${wide.screen.w}×${wide.screen.h} vs 容器 ${wide.inner.w}×${wide.inner.h}` +
            `（差 ${dw}×${dh}px）—— 还停在创建时的固定尺寸？`
        );

    // 判据 2：跟着窗口缩（用户报的 bug 的反面）
    if (!narrow.screen) {
      fail("窄屏没量到 .xterm-screen");
    } else if (narrow.cols < wide.cols && narrow.screen.w < wide.screen.w) {
      ok(
        `外层缩小时终端跟着缩：cols ${wide.cols} → ${narrow.cols}，` +
          `屏幕宽 ${wide.screen.w} → ${narrow.screen.w}px（容器 ${wide.inner.w} → ${narrow.inner.w}px）`
      );
    } else {
      // 失败时把病因分开说：尺寸事件没来 / 来了但没重新适配
      const roNote =
        out.roFired > 0
          ? `观察器收到 ${out.roFired} 次尺寸事件`
          : `观察器一次都没收到尺寸事件（容器确实变了：${wide.inner.w} → ${narrow.inner.w}px）`;
      const diag =
        out.roFired === 0
          ? `${roNote} ⇒ 观察器没挂上`
          : out.viaSchedule && out.viaSchedule.cols < wide.cols
            ? `${roNote}；直接调生产那条 scheduleTerminalFit() 能跟上（cols → ${out.viaSchedule.cols}）` +
              ` ⇒ **观察器到 fitTerminal 那条线没接上**`
            : `${roNote}；scheduleTerminalFit() 也没跟上 ⇒ 防抖回调里没找到激活的终端标签，或 fitTerminal 没算对`;
      fail(
        `外层缩小时终端没跟：cols ${wide.cols} → ${narrow.cols}，屏幕宽 ${wide.screen.w} → ` +
          `${narrow.screen.w}px（用户报的就是这个）。${diag}`
      );
    }
    if (narrow.screen && Math.abs(narrow.screen.w - narrow.inner.w) <= 40) {
      ok("窄屏下也铺满容器");
    } else if (narrow.screen && narrow.cols >= wide.cols) {
      // 上一条已把病因说清，这里不重复计分
    } else if (narrow.screen) {
      fail(`窄屏没铺满：屏幕 ${narrow.screen.w} vs 容器 ${narrow.inner.w}px`);
    }
    if (back.cols && back.cols > narrow.cols) {
      ok(`放大回去也跟随：cols ${narrow.cols} → ${back.cols}`);
    } else {
      fail(`放大回去没跟随：cols ${narrow.cols} → ${back.cols}`);
    }
  }

  // 判据 3：xterm 与 PTY 同步（否则 shell 按旧宽度折行）
  const calls = out.ptyCalls || [];
  if (calls.length === 0) {
    fail("从来没有调用 pty_resize —— PTY 的 winsize 会一直停在旧值（shell 折行会错位）");
  } else {
    // 两个方向都要有：缩小时同步过（29×36），放大回来也同步过（78×36）
    const hitNarrow = calls.some((c) => c.args.cols === narrow.cols && c.args.rows === narrow.rows);
    const hitWide = calls.some((c) => c.args.cols === wide.cols && c.args.rows === wide.rows);
    hitNarrow
      ? ok(`缩小时 PTY 同步改了：pty_resize ${narrow.cols}×${narrow.rows}`)
      : fail(
          `缩小时没同步 PTY：${narrow.cols}×${narrow.rows} 没有出现在 pty_resize 调用里（` +
            calls.map((c) => `${c.args.cols}×${c.args.rows}`).join(" ") +
            `）—— shell 会按旧宽度折行`
        );
    hitWide
      ? ok(`放大回来也同步了：pty_resize ${wide.cols}×${wide.rows}（与 xterm 一致）`)
      : fail(`放大回来没同步 PTY：${wide.cols}×${wide.rows} 不在调用里`);
  }

  // 判据 4：终端区只能有 xterm 自己的滚动容器
  const hBars = [wide.viewBars, narrow.viewBars, wide.containerBars, narrow.containerBars]
    .filter(Boolean)
    .map((b) => b.hBar)
    .filter((n) => n > 0);
  hBars.length === 0
    ? ok("终端区没有横向滚动条（唯一滚动容器是 xterm 自己的 .xterm-viewport）")
    : fail(`终端区出现横向滚动条：${hBars.join("/")}px —— min-width:0 没生效或被内容撑开了`);

  if (bad) {
    console.log("terminal-layout: " + bad + " 项不通过");
    process.exit(1);
  }
  console.log("terminal-layout: 全部通过");
})();
