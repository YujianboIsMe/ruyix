#!/usr/bin/env node
/**
 * ruyix 外链闸门 —— **真实 WebView 上的活体证明**（CDP over WebView2 远程调试端口）
 *
 * 为什么需要它：单元测试只证明 `nav_verdict` 这个**判定**对，ui-smoke 只证明代码里**挂着**闸门。
 * 两者都碰不到"WebView 真的没被导航走"——而那正是那个 P0 的症状（点一下链接，整个 IDE 变成那张网页）。
 * 所以本脚本直接对运行中的 WebView2 发 CDP 指令，亲手试每一条"能把 IDE 顶掉"的路子，再读回结果。
 *
 * ## 用法
 *
 * ```bash
 * # 1) 带调试端口启动（Windows PowerShell：$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="…"）
 * WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" cargo run
 * # 2) 另开一个终端
 * node scripts/nav-guard-probe.mjs          # 默认连 http://127.0.0.1:9222
 * ```
 *
 * 需要 Node 22+（用了内置 fetch / WebSocket，不引入任何 npm 依赖 —— 与本项目"零 npm"一致）。
 *
 * ## 判据（先定死，再看结果）
 *
 * 六条路子跑完，`location.href` 必须**始终**是基线那条自家文档，页面 target 始终只有一个，
 * 且 `#app` / `#command-input` 还在（也就是"IDE 没被顶掉"）：
 *
 * | # | 路子 | 期望 |
 * |---|------|------|
 * | ① | 基线 | 自家文档 |
 * | ② | `location.href = <agent 的服务地址>`（**绕开**前端拦截，只有后端闸门能拦） | 不导航，交给系统浏览器 |
 * | ③ | agent 输出经 markdown-it(linkify) 渲染出的 `<a href>`，真的点下去 | 不导航，交给系统浏览器 |
 * | ④ | 同源非文档 `http://tauri.localhost/main.css`（白页，没有可交给浏览器的出口） | 不导航 |
 * | ⑤ | `window.open(<服务地址>)`（on_new_window 分支） | 不导航、不多开窗口，交给系统浏览器 |
 * | ⑥ | `javascript:` 链接 | 拦下 + 会被说明，且**没有执行** |
 *
 * 只有在④⑤⑥这种"本该发生的事没发生"不好判断时，才靠状态栏文案与 `document.title` 兜底核对。
 * 跑挂（连不上调试端口）不算通过 —— 脚本会以非零码退出。
 */

const endpoint = process.argv[2] || "http://127.0.0.1:9222";
/** agent 起了后台服务后最可能回给用户的那种地址 —— 也就是用户踩坑时点的那一条 */
const SERVICE_URL = "http://localhost:8080/actuator/health";
/** 自家站点上不是文档的路径：导航过去只是一张 404 白页 */
const REFUSE_URL = "http://tauri.localhost/main.css";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function pageTargets() {
  try {
    const list = await (await fetch(`${endpoint}/json/list`)).json();
    return list.filter((t) => t.type === "page");
  } catch {
    return null;
  }
}

async function findPage() {
  for (let i = 0; i < 40; i++) {
    const list = await pageTargets();
    if (list && list.length) {
      return list.find((t) => String(t.url).includes("tauri.localhost")) || list[0];
    }
    await sleep(500);
  }
  throw new Error(
    `连不上 ${endpoint}/json/list —— 应用要以 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" 启动`
  );
}

async function main() {
  const page = await findPage();
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  const pending = new Map();
  let nextId = 1;
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = () => rej(new Error("WebSocket 打不开（WebView2 可能需要 --remote-allow-origins=*）"));
  });
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  };
  const evalJs = async (expression) => {
    const id = nextId++;
    const r = await new Promise((res) => {
      pending.set(id, res);
      ws.send(
        JSON.stringify({
          id,
          method: "Runtime.evaluate",
          params: { expression, returnByValue: true, awaitPromise: true },
        })
      );
    });
    if (r.result && r.result.exceptionDetails) return `<异常: ${r.result.exceptionDetails.text}>`;
    return r.result && r.result.result ? r.result.result.value : undefined;
  };

  const failures = [];
  const expect = (label, ok, detail) => {
    if (!ok) failures.push(`${label}: ${detail}`);
    console.log(`${ok ? "[OK]  " : "[FAIL]"} ${label}${detail ? `\n        ${detail}` : ""}`);
  };
  const statusText = () => evalJs("(document.querySelector('#statusbar .status-item') || {}).textContent || ''");

  const baseUrl = await evalJs("location.href");
  console.log(`[i] 页面: ${page.url}\n[i] 基线 URL = ${baseUrl}\n`);

  /** 每次探完都核对三条不变量：URL 没变、IDE 的 DOM 还在、没有多出 page target */
  const invariant = async (label, extraOk, extraDetail) => {
    const url = await evalJs("location.href");
    const appDom = await evalJs(
      "!!document.getElementById('app') && !!document.getElementById('command-input')"
    );
    const targets = (await pageTargets()) || [];
    expect(
      label,
      url === baseUrl && appDom === true && targets.length === 1 && extraOk !== false,
      `url=${url} app-DOM=${appDom} page-targets=${targets.length}${extraDetail ? ` | ${extraDetail}` : ""}`
    );
  };

  await invariant("① 基线", true);

  await evalJs(`location.href = ${JSON.stringify(SERVICE_URL)}; 'issued'`);
  await sleep(2500);
  await invariant("② 原生导航到服务地址（绕开前端拦截 —— 只有后端 on_navigation 能拦）");

  // ③ 用户原场景：agent 输出里的裸 URL 经应用自己的渲染器变成真 <a href>，再真的点下去
  const rendered = await evalJs(
    `(() => { const md = window.markdownit({ html: false, breaks: true, linkify: true });` +
      ` return md.render('服务已起，访问 ${SERVICE_URL} 看状态'); })()`
  );
  expect(
    "③a agent 输出里的裸 URL 确实渲染成 <a href>（病灶本体）",
    typeof rendered === "string" && rendered.includes(`<a href="${SERVICE_URL}"`),
    `渲染: ${String(rendered).slice(0, 140)}`
  );
  const clickedHref = await evalJs(
    `(() => { const d = document.createElement('div'); d.className = 'session-bubble session-bubble--md';` +
      ` d.id = 'nav-probe-bubble'; d.innerHTML = ${JSON.stringify(rendered)}; document.body.appendChild(d);` +
      ` const a = d.querySelector('a'); if (!a) return null; a.click(); return a.getAttribute('href'); })()`
  );
  await sleep(2500);
  const clickStatus = await statusText();
  await invariant(
    "③b 点下去之后 IDE 一动不动",
    typeof clickStatus === "string" && clickStatus.toLowerCase().includes("browser"),
    `href=${clickedHref} 状态栏=${JSON.stringify(clickStatus)}`
  );

  await evalJs(`location.href = ${JSON.stringify(REFUSE_URL)}; 'issued'`);
  await sleep(2000);
  await invariant("④ 同源非文档（Refuse 分支：连浏览器都没得开）");

  await evalJs(`window.open(${JSON.stringify(SERVICE_URL)}); 'issued'`);
  await sleep(2500);
  await invariant("⑤ window.open（on_new_window 分支，不许开成新窗口）");

  await evalJs(
    `(() => { const a = document.createElement('a'); a.href = 'javascript:document.title="PWNED";';` +
      ` a.id = 'nav-probe-js'; a.textContent = 'x'; document.body.appendChild(a); a.click(); })()`
  );
  await sleep(1200);
  const jsStatus = await statusText();
  const title = await evalJs("document.title");
  await invariant(
    "⑥ javascript: 链接被拦下并说明，且没有执行",
    typeof jsStatus === "string" && jsStatus.includes("javascript") && title !== "PWNED",
    `状态栏=${JSON.stringify(jsStatus)} document.title=${JSON.stringify(title)}`
  );

  // 现场收拾干净：探针塞进去的 DOM 全部删掉
  await evalJs(
    "['nav-probe-bubble','nav-probe-js'].forEach((id) => document.getElementById(id)?.remove()); 'cleaned'"
  );
  ws.close();

  if (failures.length) {
    console.log(`\n结论：闸门**没有**全拦住 —— ${failures.join(" | ")}`);
    process.exit(1);
  }
  console.log("\n结论：六条路子全部拦住，页面自始至终停在自家文档上，外链交给系统浏览器，IDE 一动不动。");
}

main().catch((e) => {
  console.error("nav-guard-probe 失败:", e.message);
  process.exit(2);
});
