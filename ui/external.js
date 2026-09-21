/**
 * ruyix — 外链闸门（前端这一重）
 *
 * ## 治的是什么病（P0）
 *
 * agent 回一句「服务已起，访问 http://localhost:8080」，点一下那个链接，**整块 IDE 被那张网页替换**
 * —— 标签栏、文件树、会话全没了，而且回不去。原因是前端全活在**一个文档**里（标签页只是 DOM 状态），
 * 而 WebView2 对普通 `<a href>` 的默认动作就是**在本 WebView 里导航过去**：文档一换，状态一起没。
 *
 * ## 两层闸门，缺一不可
 *
 * - 后端 `on_navigation`（`src-tauri/src/main.rs::build_main_window`）是**兜底**：任何来源的导航都得
 *   过它，非自家文档一律拒掉，外站改用系统浏览器打开。
 * - 本文件是**显式**的那一重：在按下那一刻就 `preventDefault`，把"打开链接"直接变成"交给系统浏览器"，
 *   连一次失败导航都不发生，并且能对拦下的链接给出**说明**（后端那一层没地方说话）。
 *
 * 只靠这一层 ＝ 下一处漏网（`location.href`、form、以后某个忘了拦的角落）就是又一次 P0；
 * 只靠后端 ＝ 用户点了没反应（浏览器不开）。所以两重都要有。
 *
 * ## 判定表与后端**完全一致**（改了这里必须同步改 `main.rs::nav_verdict`）
 *
 * | 链接 | 结局 |
 * |------|------|
 * | `#锚点` | 放行（同一个文档内的跳转） |
 * | 自家文档（`/`、`*.html`） | 放行 |
 * | http / https / mailto 且非自家站点 | 交给系统浏览器 |
 * | 同源但非文档（`src/main.rs` 这类相对链接） | 拦下并说明（导航过去只是 404 白页） |
 * | `javascript:` / `data:` / `file:` 等 | 拦下并说明（白名单之外一律不交给系统） |
 */
window.ExternalLinks = (() => {
  "use strict";

  /** 能交给系统浏览器的 scheme —— 白名单，其余连试都不试 */
  const OPENABLE = ["http:", "https:", "mailto:"];

  const T = (k, p) => (window.I18N && typeof I18N.t === "function" ? I18N.t(k, p) : k);

  /** 应用的"自家站点"：Tauri 自定义协议在 Windows 上是 http://tauri.localhost */
  function appOrigin() {
    return (typeof window !== "undefined" && window.location && window.location.origin) || "";
  }

  /** 是不是"一个文档"：自家站点上的 `/` 或 `*.html`（子资源不走导航，同源的其它路径导航过去是白页） */
  function isOwnDocument(url) {
    if (url.origin !== appOrigin()) return false;
    const p = url.pathname;
    return p === "" || p === "/" || p.endsWith(".html");
  }

  /**
   * 一条 href 该怎么处理（与后端 `nav_verdict` 同一张表）。
   * 返回 `{ kind: "leave" }` / `{ kind: "external", url }` / `{ kind: "refuse", reason }`。
   */
  function verdict(href, base) {
    const raw = String(href === null || href === undefined ? "" : href).trim();
    // 空 href / 纯锚点：同一个文档内的跳转，放行
    if (raw === "" || raw.startsWith("#")) return { kind: "leave" };

    let url = null;
    try {
      url = new URL(raw, base || appOrigin());
    } catch {
      return { kind: "refuse", reason: T("link.bad") };
    }

    if (isOwnDocument(url)) return { kind: "leave" };
    // 同源但不是文档：没有"交给浏览器"的去处（tauri.localhost 在外面根本解析不了）
    if (url.origin === appOrigin()) return { kind: "refuse", reason: T("link.own_not_doc") };
    if (OPENABLE.includes(url.protocol)) return { kind: "external", url: url.href };
    return { kind: "refuse", reason: T("link.blocked", { scheme: url.protocol.replace(":", "") }) };
  }

  /** 从点击目标往上找最近的 `<a>`（用 parentNode 走，文本节点也算数） */
  function anchorOf(node) {
    let el = node;
    while (el && el.nodeType === 1) {
      if (String(el.tagName).toLowerCase() === "a") return el;
      el = el.parentNode;
    }
    return null;
  }

  /** 外链的**唯一出口**：交给操作系统浏览器 */
  function openExternal(url) {
    // 走命令系统（前后端唯一桥），不直接 invoke —— 与"GUI 操作都过 handleCommand"同一条纪律
    Promise.resolve(handleCommand(`open url ${url}`)).catch(() => {});
  }

  /** 点击拦截：捕获阶段，先于页面上任何自己的 click handler */
  function onClick(e) {
    const a = anchorOf(e.target);
    if (!a) return;
    const href = a.getAttribute("href");
    if (href === null) return; // 没有 href 的不是链接（如会话里 data-run 的 run 徽章）

    const v = verdict(href, typeof window !== "undefined" && window.location ? window.location.href : "");
    if (v.kind === "leave") return;

    e.preventDefault();
    if (v.kind === "external") {
      openExternal(v.url);
    } else if (typeof setStatus === "function") {
      setStatus(v.reason, "error");
    }
  }

  /** 装上闸门（幂等：只挂一次） */
  let installed = false;
  function install() {
    if (installed) return;
    installed = true;
    // 捕获阶段：免得先走一步"导航已经发起、再补救"
    document.addEventListener("click", onClick, true);
    // 中键 / Ctrl+点击走 auxclick，别绕过去
    document.addEventListener("auxclick", onClick, true);
  }

  return { install, verdict, openExternal };
})();
