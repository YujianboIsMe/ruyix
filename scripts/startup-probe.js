#!/usr/bin/env node
/**
 * 启动探针 —— **真浏览器 + 真 index.html + 真脚本清单**（需要本机 Edge/Chrome）
 *
 * 为什么需要它（真事故，用户报）：
 *   Uncaught SyntaxError: Identifier 'L' has already been declared (at main.js:1:1)
 *   根因不是某个函数写错，而是**脚本之间**的事：index.html 里 15 个 <script> 全是经典脚本
 *   （没有 type="module"），**共享同一个全局作用域**；command.js 与 main.js 各写了一次顶层
 *   `const L`，于是第二个脚本在**求值前**就抛 SyntaxError —— **那一整份不执行**。
 *   main.js 不执行 ⇒ 界面死掉（window.state 没建、事件没挂），用户看到的就是这句启动报错。
 *
 * 为什么现有门禁全都漏了它（这是本探针存在的理由）：
 *   · ui-smoke 的面板回放把脚本 `eval()` 进 Node —— eval 有自己的一层作用域，撞不上；
 *   · 布局探针（memory-layout.js 等）更彻底：先把 index.html 的 <script> 全删掉，再
 *     `eval(read("ui/scripts/main.js"))` —— 既不加载 command.js，也不让**浏览器**去求值脚本，
 *     等于把病灶藏起来。它们量的是几何，不是"这份文档能不能起来"。
 *   只有"按 index.html 的真实顺序、把真实的那些文件交给真浏览器当经典脚本执行"才看得见。
 *
 * 判据（预注册；任一条不成立即退出码 1）：
 *   A1  清单自洽：被引用的脚本都存在、非空，且没有重复 include；
 *   A2  不漏接线：ui/ 下每个一方 .js（排除 vendored xterm/markdown-it）都在 index.html 里被引用；
 *   A3  逐个加载成功：N/N 触发 onload、0 个 onerror（少一块 = 界面少一块）；
 *   A4  **零 SyntaxError**（用户看到的那条；解析错误必定触发 window error 事件）；
 *   A5  脚本真的执行到了：`state` / `showPane` / `L` / `EDITOR_PANES` 都在；
 *   A6  其余错误只报告不判红（没有真 Tauri 宿主，invoke/fetch 失败正常），但要点名。
 *
 * 用法：
 *   node scripts/startup-probe.js                 # 有 Edge 就跑，没有就 SKIP（退出码 0）
 *   MSEDGE_PATH=<path> node scripts/startup-probe.js
 */
const fs = require("fs");
const path = require("path");
const cp = require("child_process");
const http = require("http");

const ROOT = path.resolve(__dirname, "..");
const UI = path.join(ROOT, "ui");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const VENDORED = ["xterm.js", "markdown-it.min.js"];

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
  console.log("SKIP: 本机没有找到 Edge/Chrome —— 启动探针未运行（脚本清单 / 全局作用域冲突将不被覆盖）");
  process.exit(0);
}

const fails = [];
const say = (ok, text) => {
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${text}`);
  if (!ok) fails.push(text);
};

// ── 真清单：就地读 index.html，顺序即浏览器的求值顺序 ──────────────────────────
const html0 = fs.readFileSync(path.join(UI, "index.html"), "utf8");
const srcs = [...html0.matchAll(/<script[^>]*\bsrc="([^"]+)"[^>]*>/g)].map((m) => m[1]);

const missing = srcs.filter(
  (s) => !fs.existsSync(path.join(UI, s)) || fs.statSync(path.join(UI, s)).size === 0
);
const dup = srcs.filter((s, i) => srcs.indexOf(s) !== i);
say(
  missing.length === 0 && srcs.length > 0,
  `A1  被引用的脚本存在且非空（${srcs.length} 个）` + (missing.length ? ` —— 缺/空: ${missing.join(", ")}` : "")
);
say(dup.length === 0, `A1b 没有重复 include` + (dup.length ? ` —— 重复: ${dup.join(", ")}` : ""));

// 一方脚本住在 `ui/scripts/`（vendored 在 `ui/packages/`）——清单从目录来，
// 这样"新增一个一方脚本却忘了挂进 index.html"仍然会被 A2 抓住。
const firstParty = fs
  .readdirSync(path.join(UI, "scripts"))
  .filter((f) => f.endsWith(".js") && !VENDORED.includes(f))
  .sort();
const unwired = firstParty.filter((f) => !srcs.includes("scripts/" + f));
say(
  unwired.length === 0,
  `A2  ui/scripts/ 下每个一方脚本都被 index.html 引用（${firstParty.length} 个）` +
    (unwired.length ? ` —— 漏挂: ${unwired.join(", ")}` : "")
);

// ── 拼页面：真 HTML；桩必须在**第一个脚本之前**；每个脚本挂 load/error 计数 ────
const stub = `<script>
window.__STARTUP__ = { loaded: [], failed: [], errors: [] };
window.addEventListener("error", (e) => {
  const err = e && e.error;
  window.__STARTUP__.errors.push(
    (err && err.name ? err.name + ": " : "") +
      ((e && (e.message || (err && err.message))) || String(e))
  );
});
window.addEventListener("unhandledrejection", (e) => {
  window.__STARTUP__.errors.push("unhandledrejection: " + ((e.reason && e.reason.message) || String(e.reason)));
});
// 只补到"脚本自己能跑完"为止：没有真宿主，invoke/fetch 的失败属于 A6（只报告）
window.__TAURI__ = {
  core: { invoke: () => Promise.resolve({}) },
  event: { listen: () => Promise.resolve(() => {}), emit: () => Promise.resolve() },
  window: {},
};
</script>`;

let html = html0.replace(
  /<script([^>]*)\bsrc="([^"]+)"([^>]*)>/g,
  (m, a, s, b) =>
    `<script${a} src="${s}"${b} onload="window.__STARTUP__.loaded.push('${s}')" onerror="window.__STARTUP__.failed.push('${s}')">`
);
html = html.replace(/<script/, `${stub}\n<script`);

async function main() {
// ── 用 http 供起来（不用 file://：相对路径的 lang/*.json 在 file:// 下会被 CORS 挡） ──
const server = http.createServer((req, res) => {
  const url = decodeURIComponent((req.url || "/").split("?")[0]);
  if (url === "/" || url === "/index.html" || url === "/probe.html") {
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    res.end(html);
    return;
  }
  const fp = path.join(UI, url.replace(/^\/+/, ""));
  if (!fp.startsWith(UI) || !fs.existsSync(fp) || fs.statSync(fp).isDirectory()) {
    res.writeHead(404);
    res.end("404");
    return;
  }
  const type = url.endsWith(".js")
    ? "text/javascript"
    : url.endsWith(".css")
      ? "text/css"
      : url.endsWith(".json")
        ? "application/json"
        : "application/octet-stream";
  res.writeHead(200, { "content-type": `${type}; charset=utf-8` });
  res.end(fs.readFileSync(fp));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const port = server.address().port;

const dir = fs.mkdtempSync(path.join(require("os").tmpdir(), "ruyix-startup-"));
const child = cp.spawn(
  browser,
  [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + path.join(dir, "profile"),
    "--window-size=1280,800", "--remote-debugging-port=0",
    `http://127.0.0.1:${port}/`,
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
  try {
    server.close();
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

let status = 0;
try {
  const ws = await openCdp(await waitForEndpoint());
  const send = cdp(ws);
  // Runtime 层也收一份（带 url:line，和用户看到的 "at main.js:1:1" 是同一处）
  const exceptions = [];
  ws.addEventListener("message", (ev) => {
    let msg;
    try {
      msg = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (msg.method === "Runtime.exceptionThrown") {
      const d = msg.params.exceptionDetails || {};
      exceptions.push(
        `${d.text || "exception"}` +
          (d.exception && d.exception.description ? ` ${String(d.exception.description).split("\n")[0]}` : "") +
          (d.url ? ` @ ${String(d.url).split("/").pop()}:${d.lineNumber + 1}` : "")
      );
    }
  });
  await send("Runtime.enable");

  // 等"每个脚本都给个说法"（onload 或 onerror）
  const deadline = Date.now() + 20000;
  let settledCount = -1;
  while (Date.now() < deadline) {
    const r = await send("Runtime.evaluate", {
      expression: "window.__STARTUP__ ? window.__STARTUP__.loaded.length + window.__STARTUP__.failed.length : -1",
      returnByValue: true,
    });
    settledCount = r.result.value;
    if (settledCount >= srcs.length) break;
    await sleep(200);
  }
  await sleep(400); // 让 DOMContentLoaded 后的启动代码把错误也吐完

  const snap = await send("Runtime.evaluate", {
    expression: `JSON.stringify({
      loaded: window.__STARTUP__.loaded, failed: window.__STARTUP__.failed,
      errors: window.__STARTUP__.errors,
      hasState: typeof state === "object", hasShowPane: typeof showPane === "function",
      hasL: typeof L === "function", hasPanes: typeof EDITOR_PANES === "object",
      readyState: document.readyState
    })`,
    returnByValue: true,
  });
  const s = JSON.parse(snap.result.value);
  const syntax = s.errors.filter((e) => /^SyntaxError/.test(e));

  say(
    s.loaded.length === srcs.length && s.failed.length === 0,
    `A3  逐个加载成功（${s.loaded.length}/${srcs.length}）` +
      (s.failed.length ? ` —— 加载失败: ${s.failed.join(", ")}` : "") +
      (settledCount < srcs.length ? ` —— 只等到 ${settledCount}/${srcs.length} 个给说法` : "")
  );
  say(
    syntax.length === 0,
    "A4  零 SyntaxError" + (syntax.length ? ` —— ${syntax.join(" ⏐ ")}` : "")
  );
  say(
    s.hasState && s.hasShowPane && s.hasL && s.hasPanes,
    `A5  脚本真的执行到了（state=${s.hasState} showPane=${s.hasShowPane} L=${s.hasL} EDITOR_PANES=${s.hasPanes}）`
  );
  const other = s.errors.filter((e) => !/^SyntaxError/.test(e));
  console.log(
    `  ·   A6 另外 ${other.length} 条非语法错误（没有真 Tauri 宿主，属正常；仅记录）` +
      (other.length ? `: ${other.slice(0, 3).join(" ⏐ ")}` : "")
  );
  if (exceptions.length) {
    console.log(`  ·   CDP 另见 ${exceptions.length} 条异常: ${exceptions.slice(0, 3).join(" ⏐ ")}`);
  }
  console.log(
    `\nstartup-probe: ${fails.length ? `${fails.length} 条判据不成立` : `全部判据成立（${srcs.length} 个脚本）`}`
  );
  status = fails.length ? 1 : 0;
  ws.close();
} catch (err) {
  console.log(`  FAIL  探针自身出错: ${(err && err.stack) || err}`);
  status = 1;
} finally {
  await cleanup();
}
  process.exitCode = status;
}

main();
