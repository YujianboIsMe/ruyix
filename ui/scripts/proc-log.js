/**
 * ruyix — 托管进程的「输出」标签页：把 `.ruyix/proc/pN.log` 实时灌进一个只读终端。
 *
 * ## 治的是什么病
 *
 * agent 起的常驻服务（`mvn spring-boot:run` / `java -jar` / `npm run dev`）的输出早就落盘了
 * （`proc::start` 一 spawn 就把 stdout + stderr 重定向进日志文件），`ProcInfo.log` 连绝对路径
 * 都带回来了 —— 面板手里**早就有**日志路径，缺的只是"读它"的入口。于是"服务起没起来"
 * 这种问题只能靠去文件管理器里翻 `.ruyix/proc/` 目录。
 *
 * ## 四条纪律
 *
 * 1. **增量，不重读**。日志跑一天能到几百兆，每 500ms 重读全量是白烧 CPU，而且整块重绘会
 *    打断选中的文本、把滚动位置打回顶部。所以只带 `next_offset` 要新内容。
 * 2. **切分点只能落在换行上**。日志按字节存、解码走"严格 UTF-8，失败整体回退活动代码页"，
 *    所以切在半个中文字符中间不是局部花屏，而是**整段乱码**。这条是后端的铁律
 *    （见 `proc::read_log_chunk`），前端负责"给回来的 offset 原样带回去"，不自己加减字节。
 * 3. **`convertEol` 必须有**。日志行尾是 `\n`（LF），而 xterm 里 `\n` 只下移不回列首 ——
 *    不转换的话整屏输出会斜成阶梯。这个坑不踩一次想不到。
 * 4. **生命周期跟着标签页**：切走就停轮询，关掉就 dispose。不然关了一堆标签页之后，
 *    后台还挂着几个定时器在问后端。
 *
 * ## 已知边界（不是 bug，是物理）
 *
 * 子进程发现自己写的是文件而不是终端，会走**全缓冲**：日志会成片出现（安静几十秒，然后
 * 突然一大块），而不是逐行刷新。Windows 没有 `stdbuf`，从外面强制不了行缓冲。所以这个面板
 * 能保证的是"**文件里已经有的，半秒内显示**"；文件里还没有的，谁也变不出来。
 */

window.ProcLogUI = (() => {
  "use strict";

  const T = (k, vars) => (window.I18N && typeof I18N.t === "function" ? I18N.t(k, vars) : k);

  /** 轮询间隔。日志是文件、内容不会丢，人对"服务输出"的延迟根本不敏感。 */
  const POLL_MS = 500;
  /** 一次 tick 内最多连续读几轮：后端说 `more` 就接着读，但别无限读下去 */
  const MAX_ROUNDS = 8;
  const FONT = '"Cascadia Code", "Fira Code", "JetBrains Mono", "Consolas", monospace';
  const FONT_SIZE = 13;
  /** 与 PTY 终端同一套配色 —— 两处终端长得不一样会显得像两个软件 */
  const THEME = {
    background: "#1e1e1e",
    foreground: "#d4d4d4",
    cursor: "#ffffff",
    selectionBackground: "#264f78",
  };

  /** 当前在轮询的定时器。同一时刻只轮询一个标签页（只显示一个，没必要的并行） */
  let timer = null;

  function invoke() {
    return typeof getTauriInvoke === "function" ? getTauriInvoke() : null;
  }

  function status(msg, kind) {
    if (typeof setStatus === "function") setStatus(msg, kind);
  }

  /** 已结束的进程：不会再产出新内容，轮询该停 */
  function isDead(state) {
    const s = String(state || "");
    return s.startsWith("exited") || s === "stopped";
  }

  function esc(s) {
    return String(s ?? "")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function paneRoot() {
    return document.getElementById("proc-log-panes");
  }

  function paneEl(tab) {
    const root = paneRoot();
    return root ? root.querySelector(`[data-pane="${tab.id}"]`) : null;
  }

  function paneHtml(tab) {
    return (
      `<div class="proc-log-pane" data-pane="${tab.id}">` +
      `<div class="proc-log-bar">` +
      `<span class="proc-log-pid">PID ${tab._procPid}</span>` +
      `<span class="proc-log-state" data-state>${esc(tab._procState)}</span>` +
      `<span class="proc-log-cmd" title="${esc(tab._procCmd)}">${esc(tab._procCmd)}</span>` +
      `<button class="proc-log-btn" data-log-follow style="display:none" ` +
      `data-i18n="proc_log.follow">回到最新</button>` +
      `<button class="proc-log-btn" data-log-clear ` +
      `data-i18n="proc_log.clear">清屏</button>` +
      `<button class="proc-log-btn" data-log-copy ` +
      `data-i18n="proc_log.copy">复制全部</button>` +
      `</div>` +
      `<div class="proc-log-term"></div>` +
      `<pre class="proc-log-plain" style="display:none"></pre>` +
      `</div>`
    );
  }

  /**
   * 造（或取回）这个标签页的面板。**每个标签页一个独立 pane**，
   * 不复用 PTY 终端那个容器 —— 两处共用一个容器会互相 `innerHTML = ""` 打架，
   * 而且切来切去内容就丢了（用户要的是"切回来还在"）。
   */
  function ensurePane(tab) {
    const root = paneRoot();
    if (!root) return null;
    let pane = paneEl(tab);
    if (pane) return pane;
    root.insertAdjacentHTML("beforeend", paneHtml(tab));
    pane = paneEl(tab);
    if (pane) bindPane(tab, pane);
    return pane;
  }

  function bindPane(tab, pane) {
    pane.querySelector("[data-log-clear]")?.addEventListener("click", () => {
      tab._term?.clear();
      const pre = pane.querySelector(".proc-log-plain");
      if (pre) pre.textContent = "";
    });
    pane.querySelector("[data-log-copy]")?.addEventListener("click", () => copyAll(tab));
    pane.querySelector("[data-log-follow]")?.addEventListener("click", () => {
      setFollow(tab, true);
      tab._term?.scrollToBottom();
    });
  }

  /** 只显示当前标签页的面板（其它 pane 留着，内容和滚动位置都不动） */
  function showPane(tab) {
    const root = paneRoot();
    if (!root) return;
    root.querySelectorAll(".proc-log-pane").forEach((p) => {
      p.style.display = p.dataset.pane === tab.id ? "" : "none";
    });
  }

  /**
   * 跟随 / 暂停跟随。用户往上滚是要看历史，这时新输出把他正在看的地方顶走就是耍流氓；
   * 手动滚回底部再自动恢复。
   */
  function setFollow(tab, on) {
    if (tab._follow === on) return;
    tab._follow = on;
    const btn = paneEl(tab)?.querySelector("[data-log-follow]");
    if (btn) btn.style.display = on ? "none" : "";
  }

  function termCtor() {
    if (typeof window !== "undefined" && window.Terminal) return window.Terminal;
    return typeof Terminal !== "undefined" ? Terminal : null;
  }

  function mountTerm(tab, pane) {
    if (tab._term) return;
    const host = pane?.querySelector(".proc-log-term");
    if (!host) return;
    const Ctor = termCtor();
    const pre = pane.querySelector(".proc-log-plain");
    if (!Ctor) {
      // 浏览器直开（没有 xterm）时降级成纯文本 —— 总比一片空白强
      if (pre) pre.style.display = "";
      return;
    }
    const term = new Ctor({
      convertEol: true, // ★ LF → CRLF，否则输出斜成阶梯
      disableStdin: true, // 只读：别让人以为能往里敲
      cursorBlink: false,
      scrollback: 10000,
      fontFamily: FONT,
      fontSize: FONT_SIZE,
      theme: THEME,
    });
    tab._term = term;
    term.open(host);
    if (pre) pre.style.display = "none";
    term.onScroll?.(() => {
      const buf = term.buffer?.active;
      if (!buf) return;
      setFollow(tab, buf.viewportY >= buf.baseY);
    });
  }

  /**
   * 列宽手算。这个 bundle 里**没有 FitAddon**，所以量一个同字体的字符宽度自己除 ——
   * 十行代码，不值得为它引一个 addon。
   */
  function cellSize(host) {
    const fallback = { w: FONT_SIZE * 0.6, h: FONT_SIZE * 1.2 };
    if (!host || typeof document.createElement !== "function") return fallback;
    try {
      const probe = document.createElement("span");
      probe.textContent = "W";
      probe.style.cssText =
        `position:absolute;visibility:hidden;white-space:pre;font-family:${FONT};` +
        `font-size:${FONT_SIZE}px;line-height:normal`;
      host.appendChild(probe);
      const r = probe.getBoundingClientRect();
      host.removeChild(probe);
      return { w: r.width || fallback.w, h: r.height || fallback.h };
    } catch {
      return fallback;
    }
  }

  function fit(tab) {
    const term = tab._term;
    const host = paneEl(tab)?.querySelector(".proc-log-term");
    if (!term || !host) return;
    const w = host.clientWidth;
    const h = host.clientHeight;
    if (!w || !h) return;
    const cell = cellSize(host);
    const cols = Math.max(20, Math.floor(w / cell.w));
    const rows = Math.max(4, Math.floor(h / cell.h));
    if (cols === term.cols && rows === term.rows) return;
    try {
      term.resize(cols, rows);
    } catch {
      /* 尺寸异常时忽略：下一次 resize 还有机会 */
    }
  }

  function pushText(tab, text) {
    if (!text) return;
    if (tab._term) {
      tab._term.write(text);
      if (tab._follow) tab._term.scrollToBottom();
      return;
    }
    const pre = paneEl(tab)?.querySelector(".proc-log-plain");
    if (pre) pre.textContent += text;
  }

  /** 往终端里插一行"引擎自己的话"（跳过提示 / 结束提示），用暗色区分开 */
  function note(tab, msg) {
    pushText(tab, `\u001b[2m${msg}\u001b[0m\n`);
  }

  function setState(tab, state) {
    if (!state) return;
    tab._procState = state;
    const el = paneEl(tab)?.querySelector("[data-state]");
    if (!el) return;
    el.textContent = state;
    el.classList?.toggle("proc-log-state--dead", isDead(state));
  }

  async function copyAll(tab) {
    let text = "";
    const buf = tab._term?.buffer?.active;
    if (buf && typeof buf.getLine === "function") {
      const lines = [];
      for (let i = 0; i < buf.length; i += 1) {
        const line = buf.getLine(i);
        lines.push(line ? line.translateToString(true) : "");
      }
      text = lines.join("\n").replace(/\s+$/, "");
    } else {
      text = paneEl(tab)?.querySelector(".proc-log-plain")?.textContent || "";
    }
    const nav = typeof navigator !== "undefined" ? navigator : null;
    try {
      if (!nav?.clipboard) throw new Error("no clipboard");
      await nav.clipboard.writeText(text);
      status(T("proc_log.copied"));
    } catch {
      status(T("proc_log.copy_fail"), "error");
    }
  }

  /** 一轮增量读。后端说 `more`（一次没读完）就接着读，直到干净为止。 */
  async function poll(tab) {
    if (tab._busy || tab._ended) return;
    const inv = invoke();
    if (!inv) {
      note(tab, T("proc_log.no_tauri"));
      tab._ended = true;
      stopTimer();
      return;
    }
    tab._busy = true;
    try {
      for (let round = 0; round < MAX_ROUNDS; round += 1) {
        let chunk;
        try {
          chunk = await inv("proc_log_read", { pid: tab._procPid, offset: tab._offset });
        } catch (err) {
          // 读不到 = 条目已从进程表移除（被停止 / 宿主收尾）。这不是故障，是终态。
          stopTimer();
          tab._ended = true;
          setState(tab, "stopped");
          note(tab, T("proc_log.closed"));
          return;
        }
        if (!chunk) return;
        // 用户已经切走了：别往一个正在后台重算的 pane 里写字，也别推进 offset
        if (window.state && window.state.activeTabId !== tab.id) return;

        tab._offset = chunk.next_offset;
        if (chunk.truncated_head && !tab._notedHead) {
          tab._notedHead = true;
          note(tab, T("proc_log.truncated", { n: chunk.start }));
        }
        pushText(tab, chunk.text);
        setState(tab, chunk.state);

        // 一次没读完 → 先读干净。**这个顺序不能反**：进程退出那一刻日志往往还有一大截没读
        // （服务自己缓冲，退出前一次性冲出来），先判断"死了"就直接收尾 = 把死因那几行丢掉，
        // 而那正是用户点开输出要找的东西。
        if (chunk.more) continue;

        if (isDead(chunk.state)) {
          stopTimer();
          tab._ended = true;
          note(tab, T("proc_log.ended"));
        }
        return;
      }
    } finally {
      tab._busy = false;
    }
  }

  function stopTimer() {
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  }

  function startPolling(tab) {
    stopTimer();
    if (tab._ended) return; // 已经结束的进程不必再问
    poll(tab);
    timer = setInterval(() => poll(tab), POLL_MS);
  }

  /** 进入输出标签页：挂载终端 → 量尺寸 → 开始增量跟随 */
  function render(tab) {
    if (!tab) return;
    const pane = ensurePane(tab);
    showPane(tab);
    mountTerm(tab, pane);
    fit(tab);
    startPolling(tab);
    if (tab._term?.focus) {
      try {
        tab._term.focus();
      } catch {
        /* 无头环境没有焦点可给 */
      }
    }
  }

  /** 切走到别的标签页：停轮询（pane 留着，切回来内容还在） */
  function blur() {
    stopTimer();
  }

  /** 关掉标签页：停轮询 + dispose 终端 + 摘掉 pane */
  function close(tab) {
    stopTimer();
    if (!tab) return;
    if (tab._term) {
      try {
        tab._term.dispose();
      } catch {
        /* 已 dispose 过 */
      }
      tab._term = null;
    }
    const pane = paneEl(tab);
    if (pane && pane.remove) pane.remove();
  }

  /**
   * 打开某个托管进程的输出。
   *
   * `proc` 是 `proc_list` 里的一行（pid / cmd / state）—— 面板和模型看的是**同一份**，
   * 不另立一份事实。
   */
  function open(proc) {
    const s = window.state;
    if (!s || !proc || !proc.pid) return null;
    const existing = s.tabs.find((t) => t._isProcLog && t._procPid === proc.pid);
    if (existing) {
      switchTab(existing.id);
      return existing;
    }
    const tab = {
      id: `proclog-${proc.pid}-${Date.now().toString()}`,
      name: `${T("proc_log.title")} ${proc.pid}`,
      path: "",
      content: "",
      _isProcLog: true,
      _procPid: proc.pid,
      _procCmd: proc.cmd || "",
      _procState: proc.state || "",
      _offset: null, // null = 首读（后端只回看尾部）
      _term: null,
      _follow: true,
      _ended: false,
      _notedHead: false,
      _busy: false,
    };
    s.tabs.push(tab);
    renderTabs();
    switchTab(tab.id);
    return tab;
  }

  /** 切语言：标签页名与面板内的固定文案都要跟着走 */
  function relabel() {
    const s = window.state;
    if (!s) return;
    for (const t of s.tabs) {
      if (t._isProcLog) t.name = `${T("proc_log.title")} ${t._procPid}`;
    }
    const root = paneRoot();
    if (root) {
      root.querySelectorAll("[data-i18n]").forEach((el) => {
        el.textContent = T(el.dataset.i18n);
      });
    }
    renderTabs();
  }

  function attach() {
    window.addEventListener?.("resize", () => {
      const s = window.state;
      if (!s) return;
      const tab = s.tabs.find((t) => t.id === s.activeTabId && t._isProcLog);
      if (tab) fit(tab);
    });
  }

  return { attach, open, render, blur, close, relabel, poll };
})();
