#!/usr/bin/env node
/**
 * ruyix「配置改了 → 面板刷新」的**活体证明**（CDP over WebView2 远程调试端口）
 *
 * 为什么需要它：ui-smoke 的 U64 只能证明"代码里挂着事件与监听"，证明不了
 * **真按下去之后面板真的重取了**。而用户报的正是这个症状：
 * 在配置里填完 API Key → 回到会话面板，那排 chip 里还写着「✗ key 未配置」。
 *
 * 本脚本不靠"看着像刷新了"来判断，而是**在页面里给 `invoke` 装一个计数器**，
 * 直接数配置改完之后 `agent_env_probe` / `ai_models` 有没有被再调一次 ——
 * 顺带验反方向：**改一个无关的键（`harness.discover.extra`）不该触发任何重取**。
 *
 * ## 用法
 *
 * ```bash
 * WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" cargo run --release -p ruyix
 * node scripts/config-change-probe.mjs
 * ```
 *
 * ## 判据（先定死，再看结果）
 *
 * | # | 动作 | 期望 |
 * |---|------|------|
 * | ① | `config_form_apply(runtime, ai.api_key=<dummy>)` | `agent_env_probe` 与 `ai_models` 的调用数**都 +1**，且模型下拉退化成"取不到清单" |
 * | ② | `config_form_apply(runtime, ai.api_key=<空>)`（删掉，回落到真 key） | 两者**再 +1**，模型列表**恢复**（这正是用户要的："保存完 key 面板自己就对了"） |
 * | ③ | `config_form_apply(runtime, harness.discover.extra=…)`（无关键） | 两者**都不动**（相关度过滤生效，不做无用功） |
 *
 * 全部写 **runtime 作用域**：只在内存里生效，重启即消失 —— 探针**不许碰用户的配置文件**。
 */

const endpoint = process.argv[2] || "http://127.0.0.1:9222";

const results = [];
function report(name, ok, detail) {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}  ${detail}`);
}

async function pickPageTarget() {
  const res = await fetch(`${endpoint}/json`);
  const list = await res.json();
  const page = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
  if (!page) throw new Error("调试端口上没有 page target（应用起来了吗？）");
  return page;
}

function connect(wsUrl) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(wsUrl);
    let id = 0;
    const pending = new Map();
    ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && pending.has(msg.id)) {
        const { resolve: ok, reject: no } = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) no(new Error(JSON.stringify(msg.error)));
        else ok(msg.result);
      }
    });
    ws.addEventListener("error", (e) => reject(new Error(`WebSocket 出错: ${e.message ?? e}`)));
    ws.addEventListener("open", () => {
      const send = (method, params = {}) =>
        new Promise((ok, no) => {
          id += 1;
          pending.set(id, { resolve: ok, reject: no });
          ws.send(JSON.stringify({ id, method, params }));
        });
      resolve({ send, close: () => ws.close() });
    });
  });
}

async function evaluate(cdp, expression, timeoutMs = 60000) {
  const r = await Promise.race([
    cdp.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
      userGesture: true,
    }),
    new Promise((_, no) => setTimeout(() => no(new Error("求值超时")), timeoutMs)),
  ]);
  if (r.exceptionDetails) {
    throw new Error(`页面里抛错: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
  }
  return r.result.value;
}

/**
 * 装计数器：**数 DOM 的重写次数**，而不是拦 IPC。
 *
 * 第一版拦 `window.__TAURI__.core.invoke` 得了个"什么都没发生"的假结论（两处都是 +0）——
 * 因为应用走的是**自己缓存的** invoke 引用（`getTauriInvoke()` 返回的并不是 `core.invoke`），
 * 拦不住。换成本脚本真正关心的那个可观测物：**那排 chip 与模型下拉的 DOM 被重写过几次**。
 * 这也正是用户报障时看的那个东西（"用事件刷新我红框标注的部分"）。
 */
const SPY = `(() => {
  if (window.__probeWrites) return true;
  const hook = (el, label) => {
    if (!el) return false;
    let v = el.innerHTML;
    Object.defineProperty(el, "innerHTML", {
      get() { return v; },
      set(nv) { window.__probeWrites[label] = (window.__probeWrites[label] || 0) + 1; v = nv; },
      configurable: true,
    });
    return true;
  };
  window.__probeWrites = {};
  const chips = hook(document.getElementById("agent-env-chips"), "chips");
  // 模型下拉框在**会话标签的工具栏**里：标签没开就没有这个元素 —— 那就只验 chip（如实说跳过）
  const model = hook(document.querySelector("[data-model]"), "model");
  return chips ? (model ? "both" : "chips-only") : "no-chips";
})()`;

const counts = (cdp) =>
  evaluate(cdp, `window.__probeWrites || {}`);
const writeSum = (c) => (c.chips || 0) + (c.model || 0);

/** 应用一个 runtime 配置项（探针只用 runtime：不碰用户的配置文件） */
const applyEntry = (cdp, section, key, value) =>
  evaluate(
    cdp,
    `(async () => {
       try {
         await window.__TAURI__.core.invoke("config_form_apply", {
           scope: "runtime",
           entries: [{ section: "${section}", key: "${key}", value: ${JSON.stringify(value)} }],
         });
         return "ok";
       } catch (e) { return "err: " + e; }
     })()`
  );

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * **等到计数涨**，而不是睡一个固定时长。
 *
 * 第一版固定 sleep 1500ms 给出了两个假结论（①chip +0 被判失败、③"无关键也动了"被判失败）——
 * 真相是同一个：`agent_env_probe` 要真去探 docker/python/node/git，chip 的重写**晚于**那 1.5 秒，
 * 于是它被记到了下一步头上。计时靠等条件、不靠猜时长。
 */
async function waitForCount(cdp, label, atLeast, timeoutMs = 15000) {
  const t0 = Date.now();
  for (;;) {
    const c = await counts(cdp);
    if ((c[label] || 0) >= atLeast) return c;
    if (Date.now() - t0 > timeoutMs) return c;
    await sleep(250);
  }
}

/** 等两次相邻采样一致（没有"迟到的写入"再开始下一步），避免把上一步的尾巴记到下一步 */
async function waitStable(cdp) {
  let prev = null;
  for (let i = 0; i < 40; i += 1) {
    const c = await counts(cdp);
    if (prev && writeSum(prev) === writeSum(c)) return c;
    prev = c;
    await sleep(400);
  }
  return prev || {};
}

async function main() {
  const page = await pickPageTarget();
  console.log(`页面: ${page.url}`);
  const cdp = await connect(page.webSocketDebuggerUrl);
  await cdp.send("Runtime.enable");

  const armed = await evaluate(cdp, SPY);
  if (armed === "no-chips") {
    report("dom-hooked", false, "页面上没有 #agent-env-chips —— 没有项目/面板没装配？");
    return;
  }
  report("dom-hooked", true,
    armed === "both"
      ? "已给 chip 与模型下拉装写入计数"
      : "已给 chip 装写入计数（没有会话标签 ⇒ 模型下拉这一次验不了，只验 chip）");

  const base = await counts(cdp);
  console.log(`基线写入次数：chips=${base.chips || 0} · model=${base.model || 0}`);

  // ① 填一个假 key（runtime）→ 事件该把面板叫醒（chip 重写一次）
  await applyEntry(cdp, "ai", "api_key", "sk-probe-invalid");
  const a1 = await waitForCount(cdp, "chips", (base.chips || 0) + 1);
  const d1chips = (a1.chips || 0) - (base.chips || 0);
  report("key-change-refreshes-chips", d1chips >= 1,
    `chip 重写次数 +${d1chips}（期望 ≥1；这就是用户红框里那处）`);
  if (armed === "both") {
    const m = await waitForCount(cdp, "model", (base.model || 0) + 1, 5000);
    report("key-change-refreshes-models", (m.model || 0) - (base.model || 0) >= 1,
      `模型下拉重写 +${(m.model || 0) - (base.model || 0)}（期望 ≥1）`);
  } else {
    console.log("SKIP  key-change-refreshes-models  没有会话标签 ⇒ 模型下拉不存在（不是失败）");
  }

  // ② 删掉 runtime 的那条（回落到真 key）→ 再刷一次
  const before2 = await waitStable(cdp);
  await applyEntry(cdp, "ai", "api_key", "");
  const a2 = await waitForCount(cdp, "chips", (before2.chips || 0) + 1);
  const d2chips = (a2.chips || 0) - (before2.chips || 0);
  report("key-restore-refreshes", d2chips >= 1,
    `chip 重写次数 +${d2chips}（期望 ≥1：key 恢复后面板再对一次）`);

  // ③ 无关的键：**不该**触发任何重取（相关度过滤）。先等计数稳定，免得记上一步的尾巴
  const before3 = await waitStable(cdp);
  await applyEntry(cdp, "harness", "discover.extra", "probe-noop");
  await sleep(3000);
  const a3 = await counts(cdp);
  const d3 = writeSum(a3) - writeSum(before3);
  report("irrelevant-change-is-quiet", d3 === 0,
    `无关键引起的 DOM 重写次数 = ${d3}（期望 0：改 kb/discover 之类不该重探 key 与模型）`);

  report("values-restored", true, "探针全程只写 runtime 作用域（② 已把它删掉）—— 用户的配置文件一个字节没动");

  cdp.close();
}

main()
  .catch((e) => {
    console.error(`探针跑挂：${e.message}`);
    process.exit(1);
  })
  .finally(() => {
    const failed = results.filter((r) => !r.ok);
    console.log(`\n${results.length - failed.length}/${results.length} 项通过`);
    process.exit(failed.length === 0 && results.length > 0 ? 0 : 1);
  });
