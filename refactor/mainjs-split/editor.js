/**
 * ruyix — 编辑器（虚拟化渲染 + 大纲 + 帮助页 + 图片预览）
 *
 * 这个模块管：编辑器标签页的打开/保存/自动保存、只画可视窗口的虚拟化渲染（含宽行只读
 * 视图）、大纲解析（md / rs / py / java）与大纲区多标签、markdown 帮助页的渲染，
 * 以及图片文件预览。
 *
 * 经典脚本（非 ES module）：与 main.js 共享同一个全局作用域，顶层 function 声明仍是
 * 全局的（initApp 按原次序裸名调用）；应用状态一律读 window.state（main.js 里显式导出）。
 * 门禁：ui-smoke U22/U31/U32/U34/U47/U50/U56。
 */


function hideEditorView() {
  hidePane("editor-view");
}
/**
 * 编辑区里的面板是**互斥**的：同一时刻只允许一个可见。
 *
 * 为什么要收这个口（bug 3，用户实测）：以前每个 `show*View()` 只负责点亮自己、顺手关掉它**当时
 * 认得**的那几个，于是"谁该关"散落在七八个函数里 —— 漏一个就会**两块面板并排**。实测症状：
 * 打开配置后从标题栏切项目，`#config-view` 只剩一半宽度、还被挤到右边 —— 因为切项目时
 * **服务面板会自己刷新并显示**（`ServiceUI` 跟着新项目走），而 `showConfigView()` 不关
 * `#service-view`；`#editor-body` 是 flex 行、两块都是 `flex:1`，于是各占一半，
 * 而 config 在 DOM 里靠后 ⇒ 显示在右半边。
 *
 * 现在所有面板的显示/隐藏都过这里：**先全关，再点亮一个**。这样无论谁在什么时候调，
 * "只有一块可见"都是结构性成立的，不再依赖各调用点记得关谁。
 */
const EDITOR_PANES = [
  "editor-empty",
  "editor-view",
  "service-view",
  "config-view",
  "terminal-view",
  "proc-log-view",
  "session-view",
  "image-view",
  "memory-view",
];

/** 只显示 `id` 这一块（其余全关）。见 [`EDITOR_PANES`]。 */
function showPane(id) {
  for (const pane of EDITOR_PANES) {
    const el = document.getElementById(pane);
    if (el) el.style.display = pane === id ? "" : "none";
  }
}

/** 关掉某一块（不动其它）。只有"确实要让位给编辑区之外的视图"时才用它。 */
function hidePane(id) {
  const el = document.getElementById(id);
  if (el) el.style.display = "none";
}

// 编辑区渲染
// ============================================

function showEditor() {
  // 编辑区上层视图归位：显示标签页 = 欢迎页/帮助页/Agent 控制台都让位
  // （标签栏在 editor-body 里，无项目状态下它整体隐藏，必须先放出来）
  const welcome = document.getElementById("welcome-content");
  const help = document.getElementById("help-page");
  const agent = document.getElementById("agent-page");
  const editorBody = document.getElementById("editor-body");
  if (welcome) welcome.style.display = "none";
  if (help) help.style.display = "none";
  if (agent) agent.style.display = "none";
  if (editorBody) editorBody.style.display = "";
  // 统一入口：点亮 editor-view 的同时关掉其余面板（见 EDITOR_PANES 的说明）
  showPane("editor-view");
  // 编辑区刚重新可见：视口高度可能和上次不同，强制重画一次窗口
  if (editorModel) {
    editorModel.paintedStart = -1;
    paintEditorWindow();
  }
}

function hideEditor() {
  showPane("editor-empty");   // 统一入口：其余面板一起关掉（bug 3）
  const serviceView = document.getElementById("service-view");
  if (serviceView) serviceView.style.display = "none";
  const procLogView = document.getElementById("proc-log-view");
  if (procLogView) procLogView.style.display = "none";
  document.getElementById("editor-gutter").innerHTML = "";
  document.getElementById("editor-code-backdrop").innerHTML = "";
  const ta = document.getElementById("editor-textarea");
  if (ta) {
    ta.value = "";
    // 清掉显式高度：下一次装载会重新给。留着旧文件的高度 = 空编辑器也撑出长滚动条
    ta.style.height = "";
    // 宽行只读视图把 textarea 藏了起来，切走时必须放回来（否则下一个文件没有输入面）
    ta.style.display = "";
  }
  const flowSpacer = document.getElementById("editor-flow-spacer");
  if (flowSpacer) flowSpacer.style.height = "";
  editorModel = null;
}

// ============================================
// 编辑器渲染：虚拟化（只画可视窗口）
// ============================================
// 症状：5000 行的文件一打开就卡死。
// 原因：gutter + backdrop 按**全文件行数**建 DOM（每行 1 个 gutter-line + 1 个
//   code-line + 若干 span，5000 行 ≈ 1.9 万节点），重建一次的布局就要 200ms 以上，
//   而这条路径挂在自动保存上 —— 打字停顿 1 秒就跑一遍（见 setupTextareaSync）。
// 做法：只渲染「可视窗口 + 上下各 EDITOR_OVERSCAN 行缓冲」，窗口外用两条零内容
//   spacer 撑出与原来**逐像素相同**的滚动高度。
//
// 三条实测得来的硬约束（动之前先看 doc/ui.md 的「编辑器渲染」一节）：
//   1) backdrop 必须绝对定位 —— 理由写在 styles.css 那段注释里（不做 = 每次重绘 110ms）；
//   2) textarea 必须由我们给显式高度 —— 全文高度原本是 backdrop 撑出来的，
//      backdrop 移出流后没人撑它，不设就只有 2 行高，点击可视区下半部分点不到它；
//   3) 水平滚动宽度完全由 backdrop 决定，窗口里必须留一条「最宽行」的零高占位。

/** 行高，与 .code-line / .editor-textarea 的 line-height 同步（门禁 U31 钉住） */
const EDITOR_LINE_H = 20;
/** .editor-textarea 上下 padding 之和（门禁 U31 钉住） */
const EDITOR_VPAD = 16;
/** 窗口上下各多渲染的行数：快速滚动时不露空白 */
const EDITOR_OVERSCAN = 24;
/** 量不到视口高度时（测试桩 / 元素处于 display:none）按这个算 */
const EDITOR_FALLBACK_VH = 600;
/**
 * 超长行触发「宽行只读视图」的阈值（**视觉列**，尺子是 editorVisualCols）。
 * 2000 列 ≈ 15.6k px —— 正常代码最长行不过几百列，2000 列已经是"压缩文件"的地盘。
 * 实测的边界（v0.13）：ui/packages/xterm.js 283,404 字节只有 2 行，最长行 283,184 列 ≈ 2.2e6 px，
 * 打开即崩编辑器 —— 那一行同时喂给背板（一条 28 万字符的 .code-line + 十万级 span）
 * 和 textarea（原文即 28 万字符的单行）。纵向虚拟化对它无效：它就是"可见行"。
 */
const EDITOR_WIDE_MAX_COLS = 2000;
/** 宽行只读视图里一个显示段最多多少视觉列（超长行按它切块） */
const EDITOR_WIDE_SEG_COLS = 200;
/** 宽行只读视图顶部那条**虚拟行**的文字；它占一行高，但**不进行号**（见 paintEditorWindow） */
const EDITOR_WIDE_BANNER = "行太宽，触发只读限制";

/** 当前编辑器内容的渲染模型；null = 无内容 */
let editorModel = null;
/** 滚动重绘的 rAF 句柄（0 = 没排队） */
let editorScrollRaf = 0;

/** 从真实元素上量行高与上下 padding；量不到就退回与 CSS 一致的常量 */
function editorMetrics() {
  let lineH = EDITOR_LINE_H;
  let vpad = EDITOR_VPAD;
  try {
    const ta = document.getElementById("editor-textarea");
    if (ta && typeof getComputedStyle === "function") {
      const cs = getComputedStyle(ta);
      const lh = parseFloat(cs.lineHeight);
      if (isFinite(lh) && lh > 0) lineH = lh;
      const pt = parseFloat(cs.paddingTop);
      const pb = parseFloat(cs.paddingBottom);
      if (isFinite(pt) && isFinite(pb)) vpad = pt + pb;
    }
  } catch {
    /* 量不到就用常量 */
  }
  return { lineH: lineH, vpad: vpad };
}

/** 一行占多少「列」：ASCII 1 列，全角/emoji 2 列，tab 走到下一个 tab 位（4） */
function editorVisualCols(s) {
  let col = 0;
  for (const ch of s) {
    if (ch === "\t") {
      col = Math.floor(col / 4) * 4 + 4;
      continue;
    }
    const c = ch.codePointAt(0);
    const wide =
      (c >= 0x1100 && c <= 0x115f) ||
      (c >= 0x2e80 && c <= 0xa4cf) ||
      (c >= 0xac00 && c <= 0xd7a3) ||
      (c >= 0xf900 && c <= 0xfaff) ||
      (c >= 0xfe30 && c <= 0xfe6f) ||
      (c >= 0xff00 && c <= 0xff60) ||
      (c >= 0xffe0 && c <= 0xffe6) ||
      (c >= 0x1f300 && c <= 0x1faff);
    col += wide ? 2 : 1;
  }
  return col;
}

/**
 * 内容里有没有"超长行"（视觉列 > `EDITOR_WIDE_MAX_COLS`）—— 宽行只读视图的**唯一判据**。
 *
 * 量**视觉列**而不是字符数：制表符最多占 4 列、全角/emoji 占 2 列，"字符数不大但列数很大"
 * 的行一样能把编辑器打崩（尺子就是 `editorVisualCols`）。
 *
 * 先按字符数粗筛、再逐字符精确量：列数 ≤ 4 × 字符数（tab 也封顶 4 列），所以
 * `字符数 ≤ MAX / 4` 的行**必不可能**触发 —— 正常文件因此几乎不付逐字符扫描的钱，
 * 只有真的长的行才过那把尺。
 */
function isWideText(input) {
  const lines = typeof input === "string" ? input.split("\n") : input || [];
  const cheap = Math.floor(EDITOR_WIDE_MAX_COLS / 4);
  for (const line of lines) {
    if (!line || line.length <= cheap) continue;
    if (lineColsExceed(line, EDITOR_WIDE_MAX_COLS)) return true;
  }
  return false;
}

/** 只回答"这一行的视觉列有没有超过 max"，一超就立刻返回 —— 28 万字符不必数完 */
function lineColsExceed(line, max) {
  let col = 0;
  for (const ch of line) {
    col += editorVisualCols(ch);
    if (col > max) return true;
  }
  return false;
}

/**
 * 把一行按**视觉列**切成若干显示段（宽行只读视图用）。
 * 按**码点**走：切在代理对中间会把 emoji / 生僻字切坏。
 */
function splitWideLine(line, maxCols) {
  if (!line) return [""];
  const out = [];
  let cur = "";
  let cols = 0;
  for (const ch of line) {
    const w = editorVisualCols(ch);
    if (cols + w > maxCols && cur) {
      out.push(cur);
      cur = "";
      cols = 0;
    }
    cur += ch;
    cols += w;
  }
  out.push(cur);
  return out;
}

/**
 * 按**新的段宽**重切宽行模型（视口尺寸变了的时候用）。
 *
 * 为什么必须重切：显示段宽是按视口算出来的（见 editorSegCols），窗口一窄，旧段就比
 * 容器还宽 → 横向滚动条又回来了，而且**行数与 spacer 高度也全错**（滚不到最后几行）。
 * 素材是 `model.rawLines`（原始逻辑行）——显示段是切出来的，拿它回切不出原文。
 */
function recutWideModel(seg) {
  const m = editorModel;
  if (!m || !m.wide || !m.rawLines) return;
  const wr = wideRowsOf(m.rawLines, seg);
  m.texts = wr.rows;
  m.lineOf = wr.lineOf;
  m.total = wr.rows.length;
  m.segCols = seg;
  m.paintedStart = -1;
  m.paintedEnd = -1;
  m.widthSynced = false;
  const spacer = document.getElementById("editor-flow-spacer");
  if (spacer) spacer.style.height = m.total * m.lineH + m.vpad + "px";
  paintEditorWindow();
}

/**
 * 一个等宽字符的宽度（px）。字体链是固定的（见 styles.css 的 `.editor-code-backdrop`），
 * 所以量一次就缓存 —— 这里量的是**真布局**（Range），不是拿字号估。
 */
let editorCharWPx = 0;
function editorCharWidth() {
  if (editorCharWPx > 0) return editorCharWPx;
  const backdrop = document.getElementById("editor-code-backdrop");
  if (!backdrop || typeof document.createRange !== "function") return 0;
  try {
    const probe = document.createElement("div");
    probe.className = "code-line";
    probe.style.position = "absolute";
    probe.style.visibility = "hidden";
    probe.style.whiteSpace = "pre";
    probe.textContent = "a".repeat(100);
    backdrop.appendChild(probe);
    const range = document.createRange();
    range.selectNodeContents(probe);
    const w = range.getBoundingClientRect().width;
    probe.remove();
    if (w > 0) editorCharWPx = w / 100;
  } catch {
    /* 量不到就退回常量上限：宁可有一点横向滚动，也不能不渲染 */
  }
  return editorCharWPx;
}

/**
 * 宽行只读视图里**一段**最多多少列 = min(常量上限, 视口放得下多少列)。
 *
 * 必须按视口算：显示段是 `white-space: pre`、不折行，段一旦比容器宽，`.editor-view` 立刻
 * 长出横向滚动条（实测：固定 200 列在 560px 视口的探针里撑出 1587px 的滚动区）。
 * 视口量不到（标签页还没显示）时退回常量上限，下一次重绘会纠正。
 */
function editorSegCols() {
  const view = document.getElementById("editor-view");
  const gutter = document.getElementById("editor-gutter");
  const cw = editorCharWidth();
  if (!view || !(view.clientWidth > 0) || !(cw > 0)) return EDITOR_WIDE_SEG_COLS;
  // 可用宽度 = 视口 − 行号列宽 − 左右 padding − 亚像素余量。
  // 两个坑都在这一行里：① 视口宽**不等于**代码区宽（行号列占掉 48px，实测 200 列/段
  // 在 560px 视口下撑出 1587px 滚动区）；② 不能拿容器宽（`#editor-code-container`）——
  // 它可能还挂着上一个文件留下的 min-width（内容宽），要等 syncCodeWidth 才清掉，那时已经晚了。
  const gw = (gutter && gutter.offsetWidth) || 48;
  const avail = view.clientWidth - gw - 32 - 2;
  const fit = Math.floor(avail / cw);
  return Math.max(1, Math.min(EDITOR_WIDE_SEG_COLS, fit));
}

/**
 * 超长行 → **显示行**模型（逻辑行与显示行不再一一对应）。
 *
 * `lineOf[i]` 是**完整的**映射：显示行 i 属于哪个逻辑行（0 基），顶部那条虚拟横幅记 -1。
 * 行号**从它推导**（见 paintEditorWindow）：一条逻辑行占多格时只有第一格给号、续格留空 ——
 * 与"虚拟行不计入行号"是同一条规则，所以只需要这一个映射，不需要"编号"与"归属"两份数组
 * （v0.13 第一版就是错在这里：只存了"要不要编号"，于是显示段拼不回原文，被探针当场抓住）。
 */
function wideRowsOf(texts, segCols) {
  const rows = [EDITOR_WIDE_BANNER];
  const lineOf = [-1];
  const seg = segCols > 0 ? segCols : EDITOR_WIDE_SEG_COLS;
  for (let i = 0; i < texts.length; i++) {
    const segs = splitWideLine(texts[i], seg);
    for (let k = 0; k < segs.length; k++) {
      rows.push(segs[k]);
      lineOf.push(i);
    }
  }
  return { rows: rows, lineOf: lineOf };
}

/**
 * 把"宽行只读"这个事实记到标签上（图标跟着变成 🔒）。
 * 判定只在 `setEditorContent` 里做一次（事实落在 `editorModel.wide`），这里只负责抄给标签 ——
 * 三个渲染入口都调它，别的入口因此没有"忘了判"的机会。
 */
function noteWideReadOnly(tab) {
  const wide = !!(editorModel && editorModel.wide);
  if (!tab || tab._wideReadOnly === wide) return;
  tab._wideReadOnly = wide;
  renderTabs();
}

/**
 * 全局最宽的那一行文本。
 * 编辑器字体链（Cascadia / Fira / JetBrains / Consolas / monospace）全是等宽字体，
 * 所以「列数最大」= 像素最宽，不需要真去量像素。
 */
function editorWidestText(texts) {
  let best = "";
  let bestCols = -1;
  for (const t of texts) {
    const c = editorVisualCols(t);
    if (c > bestCols) {
      bestCols = c;
      best = t;
    }
  }
  return best;
}

/** 窗口一次渲染多少行（可视行数 + 上下 overscan） */
function editorWindowRows(lineH) {
  const view = document.getElementById("editor-view");
  const vh = (view && Math.round(view.clientHeight || 0)) || EDITOR_FALLBACK_VH;
  return Math.max(1, Math.ceil(vh / lineH) + 1 + EDITOR_OVERSCAN * 2);
}

/**
 * 一行 → .code-line 的内层 HTML（有高亮片段就按片段切）。
 *
 * 片段是**扁平三元组** `[start, end, tagIdx, ...]`（后端 `HighlightPayload.lines`），
 * 这里直接按下标读、不建对象 —— 5000 行的文件有上万个片段，逐个 `{start_col, end_col, tag}`
 * 建出来就是上万次分配。`m.tags` 是 tag 名表，第三个数是在它里面的下标。
 *
 * ⚠️ `start`/`end` 是**行内 UTF-16 码元**偏移（后端已经把 tree-sitter 的字节偏移换算过），
 * 也就是 `String.prototype.slice()` 的单位 —— 直接切即可，**不要**再用 `TextEncoder`、
 * `codePointAt` 之类换单位，否则含中文/emoji 的行会整体错位（着色起点跑了、顺带染上标点）。
 */
function editorLineHtml(m, i) {
  const text = m.texts[i];
  if (!text) return " ";
  const flat = m.spans && m.spans[i];
  if (!flat || !flat.length) return escapeHtml(text);
  const tags = m.tags || [];
  let html = "";
  let pos = 0;
  for (let k = 0; k + 2 < flat.length; k += 3) {
    const s = flat[k];
    const e = flat[k + 1];
    if (s > pos) html += escapeHtml(text.slice(pos, s));
    html +=
      '<span class="tok-' + (tags[flat[k + 2]] || "text") + '">' +
      escapeHtml(text.slice(s, e)) +
      "</span>";
    pos = e;
  }
  if (pos < text.length) html += escapeHtml(text.slice(pos));
  return html || " ";
}

/** 只画可视窗口。窗口没变就直接返回 —— 滚动事件连发几十次，重画是白做 */
function paintEditorWindow() {
  const m = editorModel;
  if (!m) return;
  const view = document.getElementById("editor-view");
  const gutter = document.getElementById("editor-gutter");
  const backdrop = document.getElementById("editor-code-backdrop");
  if (!view || !gutter || !backdrop) return;

  const total = m.total;
  if (total <= 0) {
    gutter.innerHTML = "";
    backdrop.innerHTML = "";
    return;
  }

  const lineH = m.lineH;
  const win = editorWindowRows(lineH);
  let start = Math.max(0, Math.floor((view.scrollTop || 0) / lineH) - EDITOR_OVERSCAN);
  if (start + win > total) start = Math.max(0, total - win);
  const end = Math.min(total, start + win);
  if (start === m.paintedStart && end === m.paintedEnd) return;
  m.paintedStart = start;
  m.paintedEnd = end;

  // 两条 spacer 的高度 + 窗口行高 = 总行数 × 行高：滚动高度与虚拟化前逐像素一致
  const topPad = start * lineH;
  const botPad = Math.max(0, (total - end) * lineH);
  const pad = (h) => '<div class="editor-virt-pad" style="height:' + h + 'px"></div>';

  let gutterHtml = pad(topPad);
  let codeHtml =
    pad(topPad) +
    (m.keeper ? '<div class="code-line editor-virt-keeper">' + escapeHtml(m.keeper) + "</div>" : "");
  const cls = m.lineClass ? " " + m.lineClass : "";
  for (let i = start; i < end; i++) {
    // 行号：宽行只读视图里一条逻辑行可能占多格（显示行），只有**该行第一格**给号，
    // 续格与顶部那条虚拟横幅都留空 —— "虚拟行不计入行号"与"续段不重复编号"是同一条规则
    const no = m.lineOf
      ? m.lineOf[i] < 0 || (i > 0 && m.lineOf[i] === m.lineOf[i - 1])
        ? ""
        : m.lineOf[i] + 1
      : i + 1;
    gutterHtml += '<div class="gutter-line">' + no + "</div>";
    // 显示行 0 是那条虚拟横幅（只有 wide 模型有）：加警示类，它讲的是"为什么只读"
    const rowCls = cls + (m.wide && i === 0 ? " editor-warn-row" : "");
    codeHtml += '<div class="code-line' + rowCls + '">' + editorLineHtml(m, i) + "</div>";
  }
  gutterHtml += pad(botPad);
  codeHtml += pad(botPad);

  gutter.innerHTML = gutterHtml;
  backdrop.innerHTML = codeHtml;
  // 宽度要在**内容/视口变化**后同步一次（不变量：容器宽 == 最宽行宽）。
  // 已经在本次内容上同步过就直接返回 —— 滚动路径不能重量宽，更不能改宽
  // （改宽 = textarea 全文重排，正是 P0/P2 花力气消掉的那笔开销）。
  syncCodeWidth();
}

/**
 * 装载一份内容并首绘。三个渲染入口（高亮 / 纯文本 / 终端输出）**都走这里** ——
 * 「textarea 显式高度」和「窗口渲染」必须成套出现，只做一半编辑器就不可用。
 *
 * texts      每行文本
 * spans      每行的高亮片段，**扁平三元组** `[start, end, tagIdx, ...]`
 *            （可直接用后端 `HighlightPayload.lines`；比 texts 短也算合法，缺的行按纯文本画）
 * tags       tag 名表，spans 里的第三个数是它的下标；null = 全部按纯文本
 * text       写进 textarea 的完整文本（**必须与 tab.content 逐字节相同**）
 * lineClass  附加到 .code-line 的类（终端输出用 terminal-line）
 * readOnly   textarea 是否只读
 */
function setEditorContent(opts) {
  const texts = opts.texts || [];
  const { lineH, vpad } = editorMetrics();
  const rows = editorWindowRows(lineH);
  // 宽行判定在**这里**做一次：三个渲染入口（高亮 / 纯文本 / 终端输出）都走本函数，
  // 判定放这儿就没人能"忘了判"（与"入口必须收敛"同一条纪律）。
  const wide = isWideText(texts);
  const model = {
    texts: texts,
    spans: opts.spans || null,
    tags: opts.tags || null,
    lineClass: opts.lineClass || "",
    total: texts.length,
    lineH: lineH,
    vpad: vpad,
    // 宽行只读视图的素材与账目：rawLines = **原始逻辑行**（重切段的唯一素材：显示段是切出来的，
    // 拿它回切不出原文），segCols = 本次用的段宽（视口变化时拿它比对，决定要不要重切）
    rawLines: null,
    segCols: 0,
    // 只有真会开虚拟化（行数超出窗口）才需要宽度占位，小文件白算一遍没必要
    keeper: texts.length > rows ? editorWidestText(texts) : "",
    paintedStart: -1,
    paintedEnd: -1,
    // 宽度是否已按本次内容同步过（paintEditorWindow 里用，避免滚动路径重量宽）
    widthSynced: false,
    wide: false,
    lineOf: null,
  };

  if (wide) {
    // 宽行只读视图（v0.13）。两件事必须**同时**做 —— 少任何一半，那一行 28 万字符都还在
    // 被浏览器排版/着色，等于白改：
    //   ① 超长行切成显示段（每段 ≤ SEG_COLS 列）：背板那一刻不再出现 28 万字符的文本节点，
    //      也不再出现十万级 span；
    //   ② textarea 退出布局（见下面 ta.style.display）—— 它装着全文，留在流里就等于让
    //      layout 去排版那条 28 万字符的单行（它的 scrollWidth 会到 2.2e6 px）。
    // 宽度占位（keeper）这里**不给**：撑到几百万像素的那个宽度正是病根之一，
    // 没有 keeper 时 syncCodeWidth 会把 minWidth 清掉，容器回到视口宽，横滚条消失。
    const seg = editorSegCols();
    const wr = wideRowsOf(texts, seg);
    model.texts = wr.rows;
    model.lineOf = wr.lineOf;
    model.rawLines = texts;
    model.segCols = seg;
    model.total = wr.rows.length;
    model.spans = null;
    model.tags = null;
    model.keeper = "";
    model.wide = true;
  }
  editorModel = model;

  const ta = document.getElementById("editor-textarea");
  if (ta) {
    ta.value = opts.text;
    ta.readOnly = !!opts.readOnly || wide;
    // wide：整个退出布局，必须 display:none —— visibility:hidden 仍会被排版，等于白改。
    // 高度也交出去：滚动高度改由下面的流内 spacer 给。
    ta.style.display = wide ? "none" : "";
    ta.style.height = wide ? "" : texts.length * lineH + vpad + "px";
  }
  // wide：全文高度由**流内** spacer 给（textarea 出去以后没人撑高，没有它滚不动）
  const spacer = document.getElementById("editor-flow-spacer");
  if (spacer) spacer.style.height = wide ? model.total * lineH + vpad + "px" : "";
  paintEditorWindow();
}

/**
 * 代码区宽度 = 「最宽那一行」的宽度。
 *
 * 为什么必须显式给：backdrop 改成绝对定位后没人再撑宽容器，容器就只剩 `flex:1`
 * （= 视口宽度）。于是正文比格子宽时，**textarea 会自己长出滚动条**（它天生是滚动
 * 容器，Chromium 把作者写的 overflow:visible 当 auto），而且它会为了"露出光标"
 * 内部滚动 —— 光标跑到框右边、与背板字形错开。实测（agent.rs 5511 行，光标放最长行末尾）：
 *   容器 = 视口宽 512px → textarea.scrollLeft = 2883（自己滚），外层不动，双滚动条；
 *   容器 = 内容宽 3412px → textarea.scrollLeft = 0，外层滚到 2900（与 P2 之前逐像素一致）。
 *
 * ⚠️ 只在**内容或视口变化**后同步一次（`widthSynced` 标志），滚动路径直接返回：
 * 改宽度会让 textarea 把全文重排一遍（就是 P0/P2 花力气消掉的那 110ms）。
 * 量的是最宽行的**文本**宽度（Range）而不是 scrollWidth —— 后者在容器够宽之后
 * 就等于容器宽，会"量一次长一点"，16px 一次地无限自增。
 */
function syncCodeWidth() {
  const container = document.getElementById("editor-code-container");
  if (!container) return;
  const m = editorModel;
  if (m && m.widthSynced) return;
  if (m) m.widthSynced = true;

  const keeper = document.querySelector("#editor-code-backdrop .editor-virt-keeper");
  if (!keeper) {
    // 小文件（没虚拟化 → 没有宽度占位）：让 flex 自己撑满，别留一个旧文件的大宽度
    if (container.style.minWidth) container.style.minWidth = "";
    return;
  }

  let textW = 0;
  try {
    const range = document.createRange();
    range.selectNodeContents(keeper);
    textW = range.getBoundingClientRect().width;
  } catch {
    // 量不出来（老引擎）就退回 scrollWidth：最坏是宽度偏一点，不影响正确性
    textW = keeper.scrollWidth || 0;
  }
  if (!(textW > 0)) return;

  // 容器宽 = 文本宽 + 左右 padding（与 backdrop / textarea 的 padding 一致）
  const ta = document.getElementById("editor-textarea");
  const box = ta || document.getElementById("editor-code-backdrop");
  let padX = 32;
  if (box && typeof getComputedStyle === "function") {
    const cs = getComputedStyle(box);
    const l = parseFloat(cs.paddingLeft);
    const r = parseFloat(cs.paddingRight);
    if (isFinite(l) && isFinite(r)) padX = l + r;
  }
  const want = Math.ceil(textW + padX) + "px";
  if (container.style.minWidth !== want) container.style.minWidth = want;
}

/** 滚动时按 rAF 节流重绘（滚动会连发事件，直接重画等于白做几十次） */
function setupEditorVirtualScroll() {
  const view = document.getElementById("editor-view");
  if (!view) return;
  view.addEventListener(
    "scroll",
    () => {
      if (!editorModel || editorScrollRaf) return;
      const raf =
        typeof requestAnimationFrame === "function"
          ? requestAnimationFrame
          : (fn) => setTimeout(fn, 16);
      editorScrollRaf = raf(() => {
        editorScrollRaf = 0;
        paintEditorWindow();
      });
    },
    { passive: true }
  );
  // 视口尺寸变了（窗口缩放 / 面板收放）窗口行数要跟着变，否则会留一条空白带
  if (typeof ResizeObserver === "function") {
    try {
      new ResizeObserver(() => {
        if (!editorModel) return;
        editorModel.paintedStart = -1;
        // 视口变了 → 窗口行数跟着变，"要不要虚拟化/要不要宽度占位"可能翻转，重算一次宽度
        editorModel.widthSynced = false;
        // 宽行只读视图的**段宽也是按视口算的**：视口一变就得重切，否则旧段比容器宽
        // （横向滚动条回来），而且行数与 spacer 高度也全错（滚不到最后几行）
        if (editorModel.wide) {
          const seg = editorSegCols();
          if (seg !== editorModel.segCols) {
            recutWideModel(seg); // 它自己会重绘
            return;
          }
        }
        paintEditorWindow();
      }).observe(view);
    } catch {
      /* 观察不了就算了：下一次滚动仍会重画 */
    }
  }
}

async function highlightAndRender(tab, language) {
  const invoke = getTauriInvoke();
  if (!invoke) return;

  // 宽行只读：不进 tree-sitter（理由见 renderWideReadOnly）。标签上已有标记就用标记 ——
  // 否则每次切回来都要把 28 万字符重扫一遍才肯认账。
  if (tab && (tab._wideReadOnly || isWideText(tab.content))) {
    renderWideReadOnly(tab);
    return;
  }

  try {
    // 快照：请求返回时若内容已变化，丢弃过期的高亮结果
    const snapshot = tab.content;
    // 语言解析在 Rust 侧（插件声明的扩展名优先，其次内置探测）：这里把 path 一并给它，
    // 于是"插件加了 ext = [\"tsx\"]，前端却还不认识它"这种硬编码死角不存在了。
    const payload = await invoke("highlight_code", {
      language: language || "",
      path: tab?.path || null,
      code: snapshot,
    });
    if (tab.content !== snapshot) return;
    tab._highlighted = payload;
    tab._language = language;
    if (tab.id === window.state.activeTabId) {
      renderHighlightedCode(tab);
    }
  } catch {
    // 高亮失败：仅当该标签页仍处于激活状态时才渲染纯文本
    if (tab.id === window.state.activeTabId) {
      renderPlainCode(tab);
    }
  }
}

function renderHighlightedCode(tab) {
  // 后端给的是紧凑载荷 { tags, lines }（见 Rust 侧 HighlightPayload）
  const payload = tab._highlighted;
  if (!payload || !payload.lines) {
    renderPlainCode(tab);
    return;
  }

  // 行由**前端**自己切，不用后端的行数：后端用 `code.lines()` 收行、会吃掉末尾空行，
  // 而 textarea 的值必须与 tab.content 逐字节相同 —— 否则"以换行结尾的文件"一打开就
  // 少一个末尾 \n，用户按一下键 `tab.content = textarea.value` 把差值固化，写回时
  // 末尾换行就真没了。行数不足的部分（最多差一行）按纯文本画。
  const text = tab.content == null ? "" : String(tab.content);
  setEditorContent({
    texts: text.split("\n"),
    spans: payload.lines,
    tags: payload.tags,
    text: text,
    readOnly: false,
  });
  noteWideReadOnly(tab);
}

function renderPlainCode(tab) {
  const content = tab.content == null ? "" : String(tab.content);
  setEditorContent({ texts: content.split("\n"), spans: null, text: content, readOnly: false });
  noteWideReadOnly(tab);
}

/**
 * 宽行只读视图（v0.13）：超长行的出口。
 *
 * 为什么不走 `highlight_code`：那一行约 10 万 token，着色的收益低而代价极高
 * （IPC 载荷 + 背板十万级 span），所以只读视图是**纯文本单色** ——
 * 判据是"能打开、能看、能滚"，不是"能着色"。要着色就等后面那一步（横向窗口）。
 */
function renderWideReadOnly(tab) {
  const content = tab && tab.content != null ? String(tab.content) : "";
  setEditorContent({ texts: content.split("\n"), spans: null, text: content, readOnly: true });
  noteWideReadOnly(tab);
}

function escapeHtml(s) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/** 文件扩展名 → arborium 语言名 */
function extToLanguage(ext) {
  const map = {
    py: "python",
    rs: "rust",
    html: "html",
    htm: "html",
    css: "css",
    js: "javascript",
    mjs: "javascript",
    cjs: "javascript",
    md: "markdown",
    markdown: "markdown",
    sql: "sql",
    java: "java",
  };
  return map[ext] || null;
}

/** 文件名 → 标签页图标 */
function fileIcon(name) {
  // 全名匹配优先级（用于 .gitignore 等）
  const full = name.toLowerCase();
  if (full === ".gitignore" || full === "gitignore") return "🚫";

  const ext = name.split(".").pop()?.toLowerCase();
  // **插件优先**：插件给的语言图标是权威（它才知道自己认领了哪些扩展名）；
  // 下面的静态表是"没有插件认领时"的兜底（图片、配置、无扩展名那些本来就不归高亮管）。
  const fromPlugin = window.state.highlightPlugins?.langs?.find((l) =>
    (l.ext || []).includes(ext)
  )?.icon;
  if (fromPlugin) return fromPlugin;
  const iconMap = {
    py: "🐍",
    rs: "🦀",
    html: "🌏",
    htm: "🌏",
    css: "🎨",
    js: "Ⓙ",
    mjs: "Ⓙ",
    cjs: "Ⓙ",
    md: "Ⓜ️",
    markdown: "Ⓜ️",
    sql: "🛢️",
    java: "☕",
    png: "🖼️",
    jpg: "🖼️",
    jpeg: "🖼️",
    ico: "🖼️",
    icon: "🖼️",
    gitignore: "🚫",
    toml: "⚙️",
    json: "🧩",
  };
  return iconMap[ext] || "📄";
}

// ============================================
// 大纲
// ============================================

function updateOutline(tab) {
  const content = document.getElementById("outline-content");
  if (!content) return;

  const ext = tab.name.split(".").pop()?.toLowerCase();
  let items;

  if (ext === "md" || ext === "markdown") {
    items = parseMarkdownOutline(tab.content);
  } else if (ext === "rs") {
    items = parseRustOutline(tab.content);
  } else if (ext === "py") {
    items = parsePythonOutline(tab.content);
  } else if (ext === "java") {
    items = parseJavaOutline(tab.content);
  } else {
    content.innerHTML = `<div class="outline-placeholder">${I18N.t("outline.placeholder")}</div>`;
    return;
  }

  if (items.length === 0) {
    content.innerHTML = `<div class="outline-placeholder">${I18N.t("outline.none")}</div>`;
    return;
  }

  let html = "";
  for (const h of items) {
    const indent = (h.level - 1) * 16;
    html +=
      `<div class="outline-item" style="padding-left:${indent}px" data-line="${h.line}">` +
      `<span class="outline-dot">•</span>` +
      `${escapeHtml(h.text)}` +
      `</div>`;
  }
  content.innerHTML = html;

  content.querySelectorAll(".outline-item").forEach((el) => {
    el.addEventListener("click", () => {
      const line = parseInt(el.dataset.line, 10);
      const lineHeight = 20;
      const scrollTop = Math.max(0, (line - 1) * lineHeight);
      document.querySelector(".editor-view")?.scrollTo(0, scrollTop);
    });
  });
}

/**
 * 编辑器 UI 刷新（着色 + 大纲）—— **唯一出口**，只挂在"用户看得见结果"的时机：
 * 打开 / 显式保存 / 切回标签页 / 离开编辑器（失焦）。
 *
 * 为什么不挂在打字停顿和自动保存上：着色是一趟全量 tree-sitter + 完整 IPC 往返，
 * 大纲是一次 O(全文) 解析 + 整块 innerHTML 重建（每个条目还要挂一次点击监听）。
 * 挂在自动保存上，等于把"停一下手"变成"跑一趟全量"——而保存要的只是把字节写进磁盘，
 * 它跟屏幕长什么样没有关系。所以输入只做"内容上屏"（本地重画，不碰后端），
 * 着色与大纲等到人离开或回来时补一次。
 */
async function refreshEditorChrome(tab) {
  if (!tab) return;
  try {
    // 高亮还在且没过期（打字才会把它置空）就别再跑一趟 IPC —— 切回标签页不该有这开销
    if (tab._language && !tab._highlighted) {
      await highlightAndRender(tab, tab._language);
    }
    updateOutline(tab);
  } catch {
    // 刷新失败不影响保存与切换：最坏就是这一屏没着色（highlightAndRender 会退回纯文本）
  }
}

function parseMarkdownOutline(content) {
  const headings = [];
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const match = lines[i].match(/^(#{1,3})\s+(.+)$/);
    if (match) {
      headings.push({
        level: match[1].length,
        text: match[2].trim(),
        line: i + 1,
      });
    }
  }
  return headings;
}

function parseRustOutline(content) {
  const items = [];
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    // 计算缩进层级（每 4 空格或 1 tab 为一级，上限 3）
    const indent = raw.match(/^(\s*)/)[1];
    const indentLen = indent.replace(/\t/g, "    ").length;
    const level = Math.min(Math.floor(indentLen / 4) + 1, 3);

    const trimmed = raw.trimStart();
    let match;
    if ((match = trimmed.match(/^fn\s+(\w+)/))) {
      items.push({ level, text: `fn ${match[1]}()`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+fn\s+(\w+)/))) {
      items.push({ level, text: `fn ${match[1]}()`, line: i + 1 });
    } else if ((match = trimmed.match(/^struct\s+(\w+)/))) {
      items.push({ level, text: `struct ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+struct\s+(\w+)/))) {
      items.push({ level, text: `struct ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^enum\s+(\w+)/))) {
      items.push({ level, text: `enum ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+enum\s+(\w+)/))) {
      items.push({ level, text: `enum ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^trait\s+(\w+)/))) {
      items.push({ level, text: `trait ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+trait\s+(\w+)/))) {
      items.push({ level, text: `trait ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^impl\b\s*(.+)/))) {
      const detail = match[1].trim().replace(/\s*\{\s*$/, "");
      items.push({ level, text: `impl ${detail}`, line: i + 1 });
    } else if ((match = trimmed.match(/^mod\s+(\w+)/))) {
      items.push({ level, text: `mod ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^pub\s+mod\s+(\w+)/))) {
      items.push({ level, text: `mod ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^macro_rules!\s*(\w+)/))) {
      items.push({ level, text: `macro ${match[1]}!`, line: i + 1 });
    }
  }
  return items;
}

function parsePythonOutline(content) {
  const items = [];
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    const indent = raw.match(/^(\s*)/)[1];
    const indentLen = indent.replace(/\t/g, "    ").length;
    const level = Math.min(Math.floor(indentLen / 4) + 1, 3);

    const trimmed = raw.trimStart();
    let match;
    if ((match = trimmed.match(/^class\s+(\w+)/))) {
      items.push({ level, text: `class ${match[1]}`, line: i + 1 });
    } else if ((match = trimmed.match(/^async\s+def\s+(\w+)/))) {
      items.push({ level, text: `async def ${match[1]}()`, line: i + 1 });
    } else if ((match = trimmed.match(/^def\s+(\w+)/))) {
      items.push({ level, text: `def ${match[1]}()`, line: i + 1 });
    }
  }
  return items;
}

/** Java 类级别名（修饰符 + 声明的组合） */
const JAVA_MODIFIER =
  "(?:public|protected|private|static|final|abstract|synchronized|native|transient|volatile|strictfp|default|sealed|non-sealed)";
/** 类型声明：class / interface / enum / record（含修饰符前缀，如 public final） */
const JAVA_TYPE_DECL = new RegExp(
  `^(?:${JAVA_MODIFIER}\\s+)*(class|interface|enum|record)\\s+(\\w+)`
);
/** 注解类型声明：@interface Foo */
const JAVA_ANNOTATION_TYPE = new RegExp(
  `^(?:${JAVA_MODIFIER}\\s+)*@interface\\s+(\\w+)`
);
/** 构造方法：类名首字母大写 + 参数表，行尾是 { 或 throws（用于与普通调用区分） */
const JAVA_CONSTRUCTOR = new RegExp(
  `^(?:${JAVA_MODIFIER}\\s+)*([A-Z]\\w*)\\s*\\([^)]*\\)\\s*(?:throws\\s+[\\w,.\\s]+)?\\{?\\s*$`
);
/**
 * 方法声明：`修饰符 返回类型 名字(`
 * 要求“类型与名字之间必须有空格”，据此排除调用语句（`System.out.println(` 无空格、`foo(` 无类型）。
 */
const JAVA_METHOD = new RegExp(
  `^(?:${JAVA_MODIFIER}\\s+)*([\\w<>\\[\\],.?]+)\\s+(\\w+)\\s*\\(`
);
/**
 * 字段：必须带访问修饰符或 static/transient/volatile —— 这些不可能是局部变量。
 * 只写 final 的语句可能是方法内局部变量，故意不收。
 */
const JAVA_FIELD = new RegExp(
  `^(?:(?:public|protected|private|static|transient|volatile)\\s+)+(?:final\\s+)*` +
    `([\\w<>\\[\\],.?]+)\\s+(\\w+)\\s*(?:=[^;]*)?;\\s*$`
);
/** 语句关键字开头：不是声明，直接跳过（含 return/if 等误命中来源） */
const JAVA_STATEMENT_KEYWORD =
  /^(return|throw|break|continue|assert|if|for|while|switch|do|else|catch|case|import|package|new|yield|try)\b/;

function parseJavaOutline(content) {
  const items = [];
  const lines = content.split("\n");

  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    // 缩进层级：类体成员 = 2 级，方法体内部 ≥ 3 级（用于过滤局部变量）
    const indent = raw.match(/^(\s*)/)[1];
    const indentLen = indent.replace(/\t/g, "    ").length;
    const level = Math.min(Math.floor(indentLen / 4) + 1, 3);

    // 剥掉行首注解（@Override / @SuppressWarnings("x")），再判断内容
    const line = raw.replace(/^(?:@\w+(?:\([^)]*\))?\s*)+/, "").trim();
    if (!line || line.startsWith("//") || line.startsWith("*") || line.startsWith("/*")) {
      continue;
    }
    if (JAVA_STATEMENT_KEYWORD.test(line)) continue;

    let match;
    if ((match = raw.trim().match(JAVA_ANNOTATION_TYPE))) {
      items.push({ level, text: `@interface ${match[1]}`, line: i + 1 });
    } else if ((match = line.match(JAVA_TYPE_DECL))) {
      items.push({ level, text: `${match[1]} ${match[2]}`, line: i + 1 });
    } else if ((match = line.match(JAVA_CONSTRUCTOR))) {
      items.push({ level, text: `${match[1]}()`, line: i + 1 });
    } else if ((match = line.match(JAVA_METHOD))) {
      items.push({ level, text: `${match[2]}()`, line: i + 1 });
    } else if (level <= 2 && (match = line.match(JAVA_FIELD))) {
      items.push({ level, text: `${match[1]} ${match[2]}`, line: i + 1 });
    }
  }
  return items;
}

// ============================================
// 大纲区多标签（大纲 / Git）
// ============================================

function setupOutlineTabs() {
  const tabs = document.querySelectorAll(".outline-tab");
  tabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      tabs.forEach((t) => t.classList.remove("active"));
      tab.classList.add("active");

      const isGit = tab.dataset.outlineTab === "git";
      document.getElementById("outline-content").style.display = isGit ? "none" : "";
      document.getElementById("git-panel").style.display = isGit ? "" : "none";
      if (isGit) loadGitStatus();
    });
  });

  // Git 面板按钮 — 全部走命令系统统一入口
  document.getElementById("git-btn-commit")?.addEventListener("click", () => {
    const input = document.getElementById("git-commit-input");
    const msg = input.value.trim();
    if (!msg) {
      setStatus(I18N.t("git.need_msg"), "error");
      input.focus();
      return;
    }
    // 转义引号，保证消息作为一个参数传给 git
    const quoted = msg.replace(/"/g, '\\"');
    handleCommand(`git commit -m "${quoted}"`);
    input.value = "";
  });
  document.getElementById("git-btn-pull")?.addEventListener("click", () => handleCommand("git pull"));
  document.getElementById("git-btn-push")?.addEventListener("click", () => handleCommand("git push"));

  // unstaged 标题右侧 ➕：全部暂存
  document.getElementById("git-btn-stage-all")?.addEventListener("click", () => {
    handleCommand("git add -A");
  });

  // 非仓库引导视图：初始化仓库（初始化后 loadGitStatus 会自动切换为仓库视图）
  document.getElementById("git-btn-init")?.addEventListener("click", () => handleCommand("git init"));
  // 仓库视图顶部提示条：添加 / 更新远程仓库
  document.getElementById("git-btn-add-remote")?.addEventListener("click", addRemoteRepo);
}

async function saveCurrentFile() {
  const tab = window.state.tabs.find((t) => t.id === window.state.activeTabId);
  if (!tab || tab._isTerminal || tab._isImage || tab._isSession) return;
  if (tab._isConfig) return saveConfigTab(tab);
  if (!tab.path) {
    setStatus(I18N.t("save.no_path"), "error");
    return;
  }

  const invoke = getTauriInvoke();
  if (!invoke) {
    setStatus(I18N.t("status.tauri_unavail"));
    return;
  }

  try {
    await invoke("write_file", { path: tab.path, content: tab.content });

    // 清除修改标记
    tab._modified = false;
    renderTabs();

    // 显式保存是用户看得见结果的时机：着色与大纲在这里补一次（统一走 refreshEditorChrome）
    await refreshEditorChrome(tab);

    setStatus(I18N.t("save.ok", { name: tab.name }));
  } catch (err) {
    setStatus(I18N.t("save.fail", { err }), "error");
  }
}

/** markdown-it 渲染器懒构造（vendor 自 ui/packages/markdown-it.min.js） */
let _mdRenderer = null;

/** markdown → HTML；无渲染器时退化为转义文本（保留换行） */
function markdownToHtml(text) {
  if (!_mdRenderer && typeof window.markdownit === "function") {
    _mdRenderer = window.markdownit({ html: false, breaks: true, linkify: true });
  }
  if (_mdRenderer) return _mdRenderer.render(text ?? "");
  return escapeHtml(text ?? "").replace(/\n/g, "<br>");
}

/** 帮助文档缓存：{ lang, html }；按语言缓存，切语言时重新加载 */
let _helpCache = { lang: null, html: "" };
// 加载令牌：防止并发加载时旧请求覆盖新内容
let _helpLoadToken = 0;

/** 按当前语言加载帮助文档（help-zh.md / help-en.md）并渲染为 HTML */
async function loadHelpDoc() {
  const lang = I18N.getLang() || "zh-CN";
  if (_helpCache.lang === lang) return _helpCache.html;

  const file = lang === "en" ? "help-en.md" : "help-zh.md";
  const token = ++_helpLoadToken;

  try {
    const base = window.location.origin || "https://ruyix.localhost";
    const resp = await fetch(`${base}/${file}`);
    if (!resp.ok) return "";
    const md = await resp.text();
    if (token !== _helpLoadToken) return ""; // 已有更新的加载请求，丢弃本次结果
    _helpCache = { lang, html: markdownToHtml(md) };
    return _helpCache.html;
  } catch {
    return ""; // 加载失败：保留现有内容
  }
}

/** 帮助入口。**必须 await showHelpPage**：正文要现取 md 源文件再渲染，
 *  不返回 promise 的话调用方（含回放测试）拿到的是"还没填内容"的空容器。 */
async function openHelp() {
  if (!window.state.currentProject) {
    await showHelpPage();
  } else {
    openHelpTab();
  }
}

async function showHelpPage() {
  // 导航区保持可见；编辑区显示帮助内容
  const welcome = document.getElementById("welcome-content");
  const help = document.getElementById("help-page");
  const editorBody = document.getElementById("editor-body");
  const agent = document.getElementById("agent-page");

  const html = await loadHelpDoc();
  const body = document.getElementById("help-body");
  if (body && html) body.innerHTML = html;

  if (welcome) welcome.style.display = "none";
  if (help) help.style.display = "";
  if (editorBody) editorBody.style.display = "none";
  if (agent) agent.style.display = "none";
}

function hideHelpPage() {
  const help = document.getElementById("help-page");
  if (help) help.style.display = "none";
  // 帮助标签页打开着 → 关闭它，回到相邻标签（或欢迎页）
  const helpTab = window.state.tabs.find((t) => t._isHelp);
  if (helpTab) {
    closeTab(helpTab.id);
    return;
  }
  // 恢复到适合当前项目状态的视图
  if (window.state.currentProject) {
    showProjectWorkspace();
  } else {
    showWelcomePage();
  }
}

function openHelpTab() {
  // 已存在帮助标签页则切换
  const existing = window.state.tabs.find((t) => t._isHelp);
  if (existing) {
    switchTab(existing.id);
    return;
  }

  const tab = {
    id: "help-" + Date.now().toString(),
    name: I18N.t("help.title"),
    path: "",
    content: "",
    _isHelp: true,
  };
  window.state.tabs.push(tab);
  renderTabs();
  switchTab(tab.id);
}

// ============================================
// 编辑器 textarea 同步
// ============================================

function setupTextareaSync() {
  const textarea = document.getElementById("editor-textarea");
  if (!textarea) return;

  let debounceTimer = null;

  // ============================================
  // 核心层：输入事件 + 防抖（1 秒后自动保存）
  // ============================================
  textarea.addEventListener("input", () => {
    const tab = window.state.tabs.find((t) => t.id === window.state.activeTabId);
    if (!tab || tab._isTerminal || tab._isHelp) return;

    const newContent = textarea.value;
    if (tab.content === newContent) return;

    tab.content = newContent;
    // 丢弃过期的高亮数据；语言由扩展名决定保持不变，保存后据此重新高亮
    tab._highlighted = null;

    if (newContent.split("\n").length <= 1000) {
      renderPlainCode(tab);
    }

    if (!tab._modified) {
      tab._modified = true;
      renderTabs();
    }

    // 防抖：重置计时器。只保存，不刷新 UI（着色与大纲有自己的时机，见 refreshEditorChrome）
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      doAutoSave(tab);
    }, 1000);
  });

  // ============================================
  // 边界层：失焦 / 切换标签页时立刻保存
  // ============================================
  textarea.addEventListener("blur", () => {
    clearTimeout(debounceTimer);
    const tab = window.state.tabs.find((t) => t.id === window.state.activeTabId);
    if (tab && tab._modified && !tab._isTerminal && !tab._isHelp) {
      doAutoSave(tab);
    }
    // 离开编辑器 = 打完了：这时才补着色与大纲（打字与自动保存都不跑，见 refreshEditorChrome）
    if (tab && !tab._isTerminal && !tab._isHelp) {
      refreshEditorChrome(tab);
    }
  });

  // 供外部调用：切换标签页前保存当前 tab
  window._saveBeforeSwitch = () => {
    clearTimeout(debounceTimer);
    const tab = window.state.tabs.find((t) => t.id === window.state.activeTabId);
    if (tab && tab._modified && !tab._isTerminal && !tab._isHelp) {
      doAutoSave(tab);
    }
  };

  // ============================================
  // 兜底层：每 5 分钟长间隔定时器
  // ============================================
  setInterval(() => {
    const tab = window.state.tabs.find((t) => t.id === window.state.activeTabId);
    if (tab && tab._modified && !tab._isTerminal && !tab._isHelp) {
      doAutoSave(tab);
    }
  }, 5 * 60 * 1000);
}

/** 自动保存：**只写盘**。UI 刷新（着色 / 大纲）不归它管 —— 见 refreshEditorChrome */
async function doAutoSave(tab) {
  if (!tab || !tab.path || !tab._modified) return;
  const invoke = getTauriInvoke();
  if (!invoke) return;
  try {
    await invoke("write_file", { path: tab.path, content: tab.content });
    tab._modified = false;
    renderTabs();
  } catch {
    // 静默失败，定时器下次会重试
  }
}

/** 支持的图片扩展名集合 */
const IMAGE_EXTENSIONS = new Set([
  "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "ico", "icon"
]);

/** 判断扩展名是否为图片 */
function isImageExt(ext) {
  return IMAGE_EXTENSIONS.has(ext?.toLowerCase());
}

/** 渲染图片预览（1:1 原始尺寸） */
function renderImagePreview(tab) {
  const container = document.getElementById("image-container");
  const img = document.getElementById("image-preview");
  if (!container || !img) return;

  let url;

  // 方式1: 优先使用后端返回的 base64 数据
  if (tab._imageBase64 && tab._imageMime) {
    url = "data:" + tab._imageMime + ";base64," + tab._imageBase64;
  }

  // 方式2: 尝试使用 Tauri 2 的 convertFileSrc 获取资产 URL
  if (!url) {
    try {
      const tauriCore = getTauriCore();
      if (tauriCore && typeof tauriCore.convertFileSrc === "function") {
        url = tauriCore.convertFileSrc(tab.path);
      }
    } catch {
      // 忽略，走 fallback
    }
  }

  // 方式3: fallback — file:// 协议
  if (!url) {
    url = "file:///" + tab.path.replace(/\\/g, "/");
  }

  img.src = url;
  img.alt = tab.name;
  img.title = tab.name + " (1:1)";

  // 图片加载成功后更新状态栏显示尺寸信息
  img.onload = () => {
    setStatus(tab.name + " — " + img.naturalWidth + " × " + img.naturalHeight + " (1:1)");
  };
  img.onerror = () => {
    setStatus(I18N.t("open.file.fail", { err: "无法加载图片: " + tab.name }), "error");
  };
}
// ============================================
// 模块导出（经典脚本：这些函数本身也仍是全局的，见文件头）
// ============================================

window.EditorUI = {
  setupOutlineTabs,
  setupTextareaSync,
  setupEditorVirtualScroll,
  showPane,
  hidePane,
  showEditor,
  hideEditor,
  hideEditorView,
  refreshEditorChrome,
  setEditorContent,
  renderHighlightedCode,
  renderPlainCode,
  renderWideReadOnly,
  highlightAndRender,
  updateOutline,
  fileIcon,
  extToLanguage,
  escapeHtml,
  parseMarkdownOutline,
  parseRustOutline,
  parsePythonOutline,
  parseJavaOutline,
  saveCurrentFile,
  doAutoSave,
  markdownToHtml,
  loadHelpDoc,
  openHelp,
  showHelpPage,
  hideHelpPage,
  openHelpTab,
  renderImagePreview,
  isImageExt,
  editorMetrics,
  editorVisualCols,
};
