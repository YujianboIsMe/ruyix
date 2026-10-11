#!/usr/bin/env node
/**
 * 会话持久化 —— **真实 WebView 上的活体证明**（CDP over WebView2 远程调试端口）
 *
 * 为什么需要它：单测只证明 `sessions.rs` 的读写在字节层面正确，ui-smoke 只证明代码里**挂着**
 * 同步与落盘的调用。两者都碰不到用户报的那句原话 ——「重新打开 IDE，会话历史全丢」。
 * 而那一环恰恰只在真机上才暴露：**文件一直在写，是列表从来没被加载**
 * （`refreshList()` 原先只在 `attach()` 末尾调一次，而那一刻项目还没打开 → 早退 → 面板永远
 * 写着"暂无会话"）。所以本脚本直接对运行中的 WebView2 发 CDP：读面板 DOM、读后端列表、
 * 真写一条会话、再重启一次看它还在不在。
 *
 * ## 用法（两阶段，中间手动重启一次应用）
 *
 * ```bash
 * # 0) 清掉在跑的实例，带调试端口启动
 * WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" cargo run
 * # 1) 第一阶段：核对"列表加载 / 面板渲染 / 自动接上最近一条 / 写盘往返 / 原子性"
 * node scripts/session-persist-probe.mjs --phase=before
 * # 2) 杀掉应用，再按上面同样方式启动
 * # 3) 第二阶段：核对"重启后那一条还在，并且面板画得出来"
 * node scripts/session-persist-probe.mjs --phase=after --expect-id=<第一阶段打印的 id>
 * ```
 *
 * ## 判据（先定死，再看结果）
 *
 * | # | 检查 | 期望 |
 * |---|------|------|
 * | ① | 后端 `agent_session_list` 条数 vs 盘上 `*.json` 条数（node 直接数盘） | 相等 |
 * | ② | 面板 `#session-list .agent-history-item` 条数 | 等于列表条数（面板真的画出来了） |
 * | ③ | 有没有"自动接上最近一条"的会话 tab，且它的消息数 > 0 | 有（重启后接着上次继续） |
 * | ④ | 写盘往返：`agent_session_save` 一条含消息的会话 → 文件出现在 `<root>/.ruyix/code/agent/sessions/` | 出现，且内容一致 |
 * | ⑤ | 写完之后目录里有没有 `*.json.tmp` 残留 | 没有（原子写：tmp 必须被 rename 掉） |
 * | ⑥ | 同一个会话连删两次 | 两次都不报错（删除幂等） |
 * | ⑦ | **after 阶段**：第一阶段那条会话仍在列表与面板里 | 在（这就是"活过重启"） |
 *
 * 连不上调试端口 = 未通过（脚本非零码退出），不算"跳过"。
 *
 * 需要 Node 22+（内置 fetch / WebSocket；WebView2 建议同时加 `--remote-allow-origins=*`）。
 */

import fs from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const endpoint = args.find((a) => a.startsWith("http")) || "http://127.0.0.1:9222";
const phase = (args.find((a) => a.startsWith("--phase=")) || "--phase=before").split("=")[1];
let expectId = (args.find((a) => a.startsWith("--expect-id=")) || "").split("=")[1] || "";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function findPage() {
  for (let i = 0; i < 40; i++) {
    try {
      const list = await (await fetch(`${endpoint}/json/list`)).json();
      const pages = list.filter((t) => t.type === "page");
      if (pages.length) {
        return pages.find((t) => String(t.url).includes("tauri.localhost")) || pages[0];
      }
    } catch {
      // 还没起来
    }
    await sleep(500);
  }
  throw new Error(
    `连不上 ${endpoint}/json/list —— 应用要以 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222" 启动`
  );
}

/** 等应用把项目打开 + 会话列表加载完（启动恢复是异步的，给它时间） */
async function waitReady(evalJs, tries = 60) {
  for (let i = 0; i < tries; i++) {
    const ok = await evalJs(
      "!!(window.state && window.state.currentProject && document.querySelector('#session-list'))"
    );
    if (ok === true) {
      // 再给 syncForProject 一点时间（它是 async 的）
      for (let j = 0; j < 20; j++) {
        const n = await evalJs(
          "document.querySelectorAll('#session-list .agent-history-item').length"
        );
        if (typeof n === "number" && n > 0) return true;
        await sleep(400);
      }
      return true;
    }
    await sleep(500);
  }
  return false;
}

const failures = [];
const expect = (label, ok, detail) => {
  if (!ok) failures.push(`${label}${detail ? ` — ${detail}` : ""}`);
  console.log(`${ok ? "[OK]  " : "[FAIL]"} ${label}${detail ? `\n        ${detail}` : ""}`);
};

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
    if (r.result && r.result.exceptionDetails) {
      return `<异常: ${r.result.exceptionDetails.text} ${JSON.stringify(
        r.result.exceptionDetails.exception?.description ?? ""
      )}>`;
    }
    return r.result && r.result.result ? r.result.result.value : undefined;
  };

  console.log(`[i] 页面: ${page.url}`);
  console.log(`[i] 阶段: ${phase}\n`);

  const ready = await waitReady(evalJs);
  const root = await evalJs("(window.state && window.state.currentProject && window.state.currentProject.path) || ''");
  console.log(`[i] 项目根: ${root}`);
  if (!ready || !root) {
    expect("应用就绪（项目已打开）", false, `ready=${ready} root=${JSON.stringify(root)}`);
    ws.close();
    process.exit(1);
  }

  // ---- ① 后端列表 vs 盘上文件数 ----
  const listed = await evalJs(
    `window.__TAURI__.core.invoke('agent_session_list', { projectRoot: ${JSON.stringify(root)} })`
  );
  const listedIds = Array.isArray(listed) ? listed.map((s) => s.id) : [];
  const dir = path.join(root, ".ruyix", "code", "agent", "sessions");
  let files = [];
  try {
    files = fs
      .readdirSync(dir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => f.replace(/\.json$/, ""));
  } catch (e) {
    expect("会话目录可读", false, `${dir}: ${e.message}`);
  }
  console.log(`[i] 后端列表 ${listedIds.length} 条 / 盘上 ${files.length} 个文件：${dir}`);
  expect(
    "① 后端列表与盘上文件数一致",
    listedIds.length === files.length,
    `列表 ${listedIds.length} vs 文件 ${files.length}`
  );

  // ---- ② 面板真的画出来了（用户报的就是这一环） ----
  // 面板刷新是异步的（syncForProject / deleteSession 都不阻塞）→ 给它最多 3 秒收敛，
  // 收敛不了就按实测数字判失败（不粉饰，也不拿时序噪声当通过）。
  let items = await evalJs("document.querySelectorAll('#session-list .agent-history-item').length");
  for (let i = 0; i < 12 && items !== listedIds.length; i++) {
    await sleep(250);
    items = await evalJs("document.querySelectorAll('#session-list .agent-history-item').length");
  }
  const emptyText = await evalJs("(document.querySelector('#session-list') || {}).textContent || ''");
  console.log(`[i] 面板条目: ${items}｜首屏文案: ${String(emptyText).slice(0, 40)}`);
  expect(
    "② 会话面板渲染出条目（不是「暂无会话」）",
    items === listedIds.length && items > 0,
    `面板 ${items} vs 列表 ${listedIds.length}｜文案=${String(emptyText).slice(0, 30)}`
  );

  // ---- ③ 自动接上最近一条（有消息的） ----
  const tabInfo = await evalJs(
    `(() => {
       const tabs = (window.state.tabs || []).filter((t) => t._isSession && t._session);
       const t = tabs[tabs.length - 1];
       return t ? { id: t._session.id, msgs: (t._session.messages || []).length,
                    bubbles: (t._sessionEl ? t._sessionEl.querySelectorAll('.session-msg').length : -1) } : null;
     })()`
  );
  console.log(`[i] 会话 tab: ${JSON.stringify(tabInfo)}`);
  expect(
    "③ 打开项目后自动接上最近一条有消息的会话",
    tabInfo && tabInfo.msgs > 0 && tabInfo.bubbles > 0,
    JSON.stringify(tabInfo)
  );

  if (phase === "before") {
    // ---- ④ 写盘往返（不需要 LLM） ----
    const probe = {
      id: "sess_probe_" + new Date().toISOString().replace(/[^0-9]/g, ""),
      title: "持久化探针（可删）",
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
      messages: [
        { role: "user", text: "探针：这条消息要活过重启", ts: "00:00:01", run_id: null, status: null },
        { role: "assistant", text: "探针：收到", ts: "00:00:02", run_id: null, status: null },
      ],
    };
    const saved = await evalJs(
      `window.__TAURI__.core.invoke('agent_session_save', { sessionJson: ${JSON.stringify(
        JSON.stringify(probe)
      )}, projectRoot: ${JSON.stringify(root)} })`
    );
    const file = path.join(dir, `${probe.id}.json`);
    const exists = fs.existsSync(file);
    console.log(`[i] 探针写入: ${file} → ${exists ? "在" : "不在"}`);
    expect("④ 会话真的落到 <root>/.ruyix/.../sessions/", exists, `save 返回=${JSON.stringify(saved)?.slice(0, 120)}`);
    if (exists) {
      const back = JSON.parse(fs.readFileSync(file, "utf8"));
      expect(
        "④b 落盘内容与传入一致（标题 + 消息条数）",
        back.title === probe.title && (back.messages || []).length === 2,
        `title=${back.title} msgs=${(back.messages || []).length}`
      );
    }
    const leftovers = fs.readdirSync(dir).filter((f) => f.endsWith(".tmp"));
    expect("⑤ 原子写：目录里没有 *.json.tmp 残留", leftovers.length === 0, leftovers.join(", "));

    const listed2 = await evalJs(
      `window.__TAURI__.core.invoke('agent_session_list', { projectRoot: ${JSON.stringify(root)} })`
    );
    const ids2 = Array.isArray(listed2) ? listed2.map((s) => s.id) : [];
    expect("④c 新会话出现在列表里且排在最前", ids2[0] === probe.id, `首条=${ids2[0]}`);

    console.log(`\nPROBE_ID=${probe.id}`);
    console.log("[i] 现在杀掉应用、按同样方式重启，再跑 --phase=after --expect-id=<上面的 id>");
  } else {
    // ---- ⑦ 重启后那条还在 ----
    const inList = listedIds.includes(expectId);
    const inPanel = await evalJs(
      `document.querySelectorAll('#session-list .agent-history-item[data-open="${expectId}"]').length > 0`
    );
    console.log(`[i] 重启后查找 ${expectId}：列表=${inList} 面板=${inPanel}`);
    expect("⑦ 上一阶段写入的会话活过了重启（在列表里）", inList, `expect-id=${expectId}`);
    expect("⑦b 而且面板画得出来", inPanel === true, `面板命中=${inPanel}`);

    // 点开它，确认消息真的是从盘上读回来的
    await evalJs(`(() => {
      const el = document.querySelector('#session-list .agent-history-item[data-open="${expectId}"]');
      if (el) el.click();
      return !!el;
    })()`);
    await sleep(1200);
    const opened = await evalJs(
      `(() => {
         const t = (window.state.tabs || []).find((x) => x._isSession && x._session && x._session.id === ${JSON.stringify(
           expectId
         )});
         return t ? { msgs: (t._session.messages || []).length,
                      first: (t._session.messages || [])[0]?.text || '',
                      bubbles: (t._sessionEl ? t._sessionEl.querySelectorAll('.session-msg').length : -1) } : null;
       })()`
    );
    console.log(`[i] 点开后: ${JSON.stringify(opened)}`);
    expect(
      "⑦c 点开后消息从盘上读回来了（内容一致）",
      opened && opened.msgs === 2 && String(opened.first).includes("活过重启") && opened.bubbles === 2,
      JSON.stringify(opened)
    );

    // ---- ⑥ 删除幂等 + 清掉探针 ----
    const del1 = await evalJs(
      `window.__TAURI__.core.invoke('agent_session_delete', { id: ${JSON.stringify(expectId)}, projectRoot: ${JSON.stringify(root)} }).then(() => 'ok').catch((e) => 'ERR:' + e)`
    );
    const del2 = await evalJs(
      `window.__TAURI__.core.invoke('agent_session_delete', { id: ${JSON.stringify(expectId)}, projectRoot: ${JSON.stringify(root)} }).then(() => 'ok').catch((e) => 'ERR:' + e)`
    );
    console.log(`[i] 连删两次: ${del1} / ${del2}`);
    expect("⑥ 删除幂等（第二次也不报错）", del1 === "ok" && del2 === "ok", `${del1} / ${del2}`);
    expect("⑥b 探针文件已清掉（不留垃圾）", !fs.existsSync(path.join(dir, `${expectId}.json`)));
  }

  console.log(`\n=== 汇总（${phase}）===`);
  if (failures.length === 0) {
    console.log("会话持久化探针通过：全部检查通过");
  } else {
    console.error(`失败 ${failures.length} 项：\n - ${failures.join("\n - ")}`);
    process.exitCode = 1;
  }
  ws.close();
}

main().catch((e) => {
  console.error(`探针跑挂：${e.message}`);
  process.exit(1);
});
