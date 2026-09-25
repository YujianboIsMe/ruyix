#!/usr/bin/env node
/**
 * ruyix 语音转写 —— **真实 WebView 上的活体证明**（CDP over WebView2 远程调试端口）
 *
 * 为什么需要它：引擎侧的单测与 `examples/voice_probe` 只证明"模型能转写"，
 * ui-smoke 只证明"代码里挂着该挂的东西"。两者都**碰不到 UI 里那条路**
 * —— 而用户报的恰恰是 UI 里的症状：
 *
 *   > 语音识别卡住了不动啊？一直 transcribing locally，不会变化啊。一直是挂起状态。
 *
 * 现场证据（devtools 网络面板）：`voice_status` 一条花了 **6.58 秒**，两条 `voice_transcribe`
 * 永远停在"挂起"。根因是自检把 456MB 权重 `fs::read` 进内存 + 每次重算 sha256，
 * 而那个自检当时是**同步命令 ⇒ 跑在主线程上**：主线程被占 6.5 秒，界面里在飞的 IPC
 * 回复送不回去。所以本脚本专门来验**修完之后这条路真的通**，而且**快**：
 *
 * ## 用法
 *
 * ```bash
 * # 1) 带调试端口 + 指定模型目录启动（模型目录是必要的：否则自检报"未安装"，
 * #    那样测到的是错误路径，不是转写路径）
 * RUYIX_VOICE_MODEL_DIR='D:\Models' \
 *   WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" \
 *   cargo run -p ruyix
 * # 2) 另开一个终端（wav 必须是 **16kHz 单声道 16bit**）
 * node scripts/voice-transcribe-probe.mjs <某个.wav>
 * ```
 *
 * 需要 Node 22+（内置 fetch / WebSocket，零 npm 依赖 —— 与本项目"零 npm"一致）。
 *
 * ## 判据（先定死，再看结果）
 *
 * | # | 试什么 | 期望 |
 * |---|--------|------|
 * | ① | `voice_status` 第一次 | 返回（Ready），耗时记下来当基线 |
 * | ② | `voice_status` 第二次 | **< 300ms**（同一份文件不该重算哈希 —— 这是 6.5 秒的根） |
 * | ③ | `voice_transcribe`（真音频） | **必须返回**，且文本非空、耗时 < 120s |
 * | ④ | 转写期间 | 收到 `voice://stage`（说明"在算"和"卡死"分得开） |
 *
 * 跑挂（连不上调试端口 / 转写没回来）都算失败，脚本以非零码退出 ——
 * "挂起"这个症状的判据只能是"它回来了"，不能是"看着像没事"。
 *
 * ## 必须在 **release** 构建上验（实测教训）
 *
 * debug 构建下 candle 的推理慢十几倍：同一段 3.77 秒音频，release 约 11 秒、
 * debug 会顶到 200 秒以上（第一次跑探针就被 200s 超时拦住，看着像"又挂起了"，其实只是在算）。
 * 所以：默认超时放到 **600 秒**，并且**别用 debug 结果判断"有没有挂起"** ——
 * 那会把人带偏。判断"挂起"要看**第二次 `voice_status` 是不是 3ms 级**（缓存是否生效）
 * 与"转写最终有没有回来"这两条，而不是"等了多久"。
 */

import { readFileSync } from "node:fs";

const endpoint = process.argv[2] && !process.argv[2].endsWith(".wav")
  ? process.argv[2]
  : "http://127.0.0.1:9222";
const wavPath = process.argv.find((a) => a.endsWith(".wav"));
if (!wavPath) {
  console.error("用法: node scripts/voice-transcribe-probe.mjs [调试端口] <16kHz 单声道 16bit .wav>");
  process.exit(2);
}

/** 极简 WAV 读取：只要 16bit PCM 的 data 块（本项目的测试音频就是这个形状）。 */
function wavToPcmBase64(path) {
  const buf = readFileSync(path);
  if (buf.toString("ascii", 0, 4) !== "RIFF" || buf.toString("ascii", 8, 12) !== "WAVE") {
    throw new Error("不是 WAV 文件");
  }
  let off = 12;
  let fmt = null;
  let data = null;
  while (off + 8 <= buf.length) {
    const id = buf.toString("ascii", off, off + 4);
    const size = buf.readUInt32LE(off + 4);
    const body = off + 8;
    if (id === "fmt ") {
      fmt = {
        channels: buf.readUInt16LE(body + 2),
        sampleRate: buf.readUInt32LE(body + 4),
        bits: buf.readUInt16LE(body + 14),
      };
    } else if (id === "data") {
      data = buf.subarray(body, body + size);
    }
    off = body + size + (size % 2);
  }
  if (!fmt || !data) throw new Error("WAV 缺 fmt / data 块");
  if (fmt.bits !== 16 || fmt.channels !== 1 || fmt.sampleRate !== 16000) {
    throw new Error(
      `音频形状不对：需要 16kHz 单声道 16bit，实际 ${fmt.sampleRate}Hz / ${fmt.channels}ch / ${fmt.bits}bit`
    );
  }
  const n = Math.floor(data.length / 2);
  const f32 = Buffer.alloc(n * 4);
  for (let i = 0; i < n; i += 1) {
    f32.writeFloatLE(data.readInt16LE(i * 2) / 32768, i * 4);
  }
  return { base64: f32.toString("base64"), secs: n / 16000 };
}

async function pickPageTarget() {
  const res = await fetch(`${endpoint}/json`);
  const list = await res.json();
  const page = list.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
  if (!page) throw new Error("调试端口上没有 page target（应用起来了吗？）");
  return page;
}

/** 一条极简 CDP 客户端：只用到 Runtime.enable / Runtime.evaluate。 */
function connect(wsUrl) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(wsUrl);
    let id = 0;
    const pending = new Map();
    const events = [];
    ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && pending.has(msg.id)) {
        const { resolve: ok, reject: no } = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) no(new Error(JSON.stringify(msg.error)));
        else ok(msg.result);
      } else if (msg.method) {
        events.push(msg);
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
      resolve({ send, events, close: () => ws.close() });
    });
  });
}

/** 在页面里求值，返回 by-value 结果。 */
async function evaluate(cdp, expression, timeoutMs = Number(process.env.PROBE_TIMEOUT_MS || 600000)) {
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

const results = [];
function report(name, ok, detail) {
  results.push({ name, ok, detail });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}  ${detail}`);
}

async function main() {
  const { base64, secs } = wavToPcmBase64(wavPath);
  console.log(`音频: ${wavPath}（${secs.toFixed(2)} 秒）→ base64 ${Math.round(base64.length / 1024)} KB`);
  const page = await pickPageTarget();
  console.log(`页面: ${page.url}`);
  const cdp = await connect(page.webSocketDebuggerUrl);
  await cdp.send("Runtime.enable");

  const hasInvoke = await evaluate(cdp, "!!(window.__TAURI__ && window.__TAURI__.core)");
  if (!hasInvoke) {
    report("tauri-invoke-available", false, "页面里没有 window.__TAURI__.core —— 探针连的不是 ruyix 窗口");
    return;
  }

  // ① + ②：自检两次 —— 第一次允许慢（首次全量校验），第二次必须飞快（缓存生效）
  const first = await evaluate(
    cdp,
    `(async () => { const t = performance.now();
       const st = await window.__TAURI__.core.invoke("voice_status");
       return { ms: Math.round(performance.now() - t), ready: !!st.ready, line: st.line }; })()`
  );
  report("voice-status-first", true, `${first.ms} ms · ready=${first.ready} · ${first.line}`);
  const second = await evaluate(
    cdp,
    `(async () => { const t = performance.now();
       const st = await window.__TAURI__.core.invoke("voice_status");
       return { ms: Math.round(performance.now() - t), ready: !!st.ready }; })()`
  );
  report(
    "voice-status-second-is-cached",
    second.ms < 300,
    `${second.ms} ms（判据 <300ms；这一步就是"每次自检重算 6.5 秒"的反证）`
  );

  // ③ + ④：真转写 —— 判据是"它回来了"，不是"看着像没事"
  const expr = `(async () => {
    const stages = [];
    const un = await window.__TAURI__.event.listen("voice://stage", (e) => {
      stages.push((e.payload && e.payload.line) || e.payload && e.payload.phase);
    }).catch(() => null);
    const t = performance.now();
    try {
      const out = await window.__TAURI__.core.invoke("voice_transcribe", { data: "${base64}", language: null });
      return { ms: Math.round(performance.now() - t), text: out.text, lang: out.language,
               tokens: out.tokens, stages: out.stages, heard: stages };
    } catch (e) {
      return { ms: Math.round(performance.now() - t), error: String(e), heard: stages };
    } finally {
      if (un) { try { un(); } catch {} }
    }
  })()`;
  const tx = await evaluate(cdp, expr);
  if (tx.error) {
    report("voice-transcribe-returns", false, `转写报错（不是挂起，但也没成）：${tx.error}`);
  } else {
    report(
      "voice-transcribe-returns",
      !!tx.text && tx.ms < 120000,
      `${tx.ms} ms · lang=${tx.lang} · ${tx.tokens} token · 文本="${tx.text}"` +
        `（debug 构建慢十几倍，别拿这个数当性能；release 上同一段约 11 秒）`
    );
    report(
      "voice-stage-visible",
      Array.isArray(tx.heard) && tx.heard.length > 0,
      `转写期间收到的阶段：${JSON.stringify(tx.heard)}`
    );
  }
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
