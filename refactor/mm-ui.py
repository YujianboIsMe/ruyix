#!/usr/bin/env python3
"""v1.3 多模态第 3 步：会话前端的三个附件入口 + 缩略图（一次性、幂等、可重放）。

为什么写成脚本而不是手改：这一刀要动 `ui/scripts/session.js` 的十来处（助手区结构、发送路径、
渲染、持久化），每处都要恰好命中一次；脚本把"锚点命中数"变成断言，漏一处立刻报错而不是
悄悄改错地方。

用法：`python refactor/mm-ui.py`
"""
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
P = ROOT / "ui/scripts/session.js"

# ---------------------------------------------------------------- 助手块（附件核心）
HELPERS = r'''
  // ============================================
  // 截图附件（v1.3 多模态）：粘贴 / 选图 / 拖拽 → 缩图 → 随消息发出去
  //
  // 分工：**浏览器缩图**（canvas 是这台机器上现成的图像编解码器，宿主与引擎都不为此引依赖）；
  // 宿主只管闸与落盘（`agent::attachments`）；引擎只管按协议把字节摆对位置。
  // 图**不进会话文件**：会话里存的是宿主回执的 `shots`（路径 + 元数据），字节在
  // `<便携根>/projects/<项目 key>/shots/`，重开会话按路径回读（`hydrateShots`）。
  // ============================================

  /** 长边上限（与宿主侧口径一致）；小图**原样发** —— 不重编码就没有画质损失 */
  const SHOT_MAX_EDGE = 1600;
  /** 超过这个字节数也重编码（原图能不动就不动，但 2 MB 以上的往返会明显拖慢） */
  const SHOT_SOFT_BYTES = 2 * 1024 * 1024;
  /** 一条消息最多几张 —— 与宿主 `attachments::MAX_PER_MESSAGE` 同值（前端先拦，省一次往返） */
  const MAX_SHOTS = 6;

  /** 路径 → data URL：重开会话时按路径回读，一张只读一次 */
  const shotCache = new Map();

  function dataUrlBytes(url) {
    const i = String(url || "").indexOf(",");
    const b64 = i < 0 ? "" : String(url).slice(i + 1);
    return Math.floor((b64.length * 3) / 4);
  }

  function fileToDataUrl(file) {
    return new Promise((res, rej) => {
      const r = new FileReader();
      r.onload = () => res(String(r.result || ""));
      r.onerror = () => rej(new Error(L("这个文件读不出来", "this file cannot be read")));
      r.readAsDataURL(file);
    });
  }

  function loadImageEl(url) {
    return new Promise((res, rej) => {
      const im = new Image();
      im.onload = () => res(im);
      im.onerror = () => rej(new Error(L("这不是一张能解的图", "not a decodable image")));
      im.src = url;
    });
  }

  /**
   * 一张用户给的图 → `{name, mime, data_base64, _preview}`（发给宿主的那份 + 本地预览）。
   *
   * `mime` **从编码回执的头里取**，不从 `file.type` 取：浏览器拿不准原类型时会退回 png，
   * 而"字节实际是什么"才是事实（宿主那边按魔数再嗅一次 —— 两边都不信声明）。
   */
  async function toAttachment(file) {
    const url0 = await fileToDataUrl(file);
    if (!/^data:image\//.test(url0)) {
      throw new Error(L(`不是图片：${file.type || file.name}`, `not an image: ${file.type || file.name}`));
    }
    const im = await loadImageEl(url0);
    const edge = Math.max(im.naturalWidth || 0, im.naturalHeight || 0);
    let url = url0;
    if (edge > SHOT_MAX_EDGE || dataUrlBytes(url0) > SHOT_SOFT_BYTES) {
      const scale = Math.min(1, SHOT_MAX_EDGE / (edge || 1));
      const w = Math.max(1, Math.round((im.naturalWidth || 1) * scale));
      const h = Math.max(1, Math.round((im.naturalHeight || 1) * scale));
      const cv = document.createElement("canvas");
      cv.width = w;
      cv.height = h;
      const cx = cv.getContext("2d");
      if (!cx) throw new Error(L("画不出缩放用的画布", "no canvas context available"));
      cx.drawImage(im, 0, 0, w, h);
      // 编码格式跟原图走（截图多是 PNG，照片多是 JPEG）；`toDataURL` 的回执头才是真实类型
      const want =
        file.type === "image/jpeg" ? "image/jpeg"
          : file.type === "image/webp" ? "image/webp"
            : "image/png";
      url = cv.toDataURL(want, 0.92);
    }
    const mime = (url.match(/^data:([^;,]+)/) || [])[1] || file.type || "image/png";
    return {
      name: file.name || "shot.png",
      mime,
      data_base64: url.slice(url.indexOf(",") + 1),
      _preview: url,
    };
  }

  /** 这条会话待发的图（`s._pending`；**不进会话文件** —— 发出去之后宿主的回执才是事实） */
  function pendingOf(s) {
    if (!Array.isArray(s._pending)) s._pending = [];
    return s._pending;
  }

  /** 待发条的 HTML：张数 + 体积 + 每张一个 ✕ */
  function pendingHtml(s) {
    const list = pendingOf(s);
    if (!list.length) return "";
    const mb = list.reduce((n, a) => n + dataUrlBytes(a._preview), 0) / 1024 / 1024;
    const head = L(
      `待发 ${list.length} 张 · ${mb.toFixed(1)} MB`,
      `${list.length} attached · ${mb.toFixed(1)} MB`
    );
    const items = list
      .map((a, i) =>
        `<span class="session-shot">` +
        `<img class="session-shot-thumb" src="${esc(a._preview)}" alt="${esc(a.name)}" title="${esc(a.name)}">` +
        `<button class="session-shot-x" data-shot-drop="${i}" title="${L("移除", "remove")}">✕</button>` +
        `</span>`
      )
      .join("");
    return `<div class="session-pending-head">${esc(head)}</div>` +
      `<div class="session-pending-list">${items}</div>`;
  }

  /** 重画待发条（只在附件变化时调） */
  function renderPending(wrap, s) {
    const el = wrap.querySelector("[data-pending]");
    if (!el) return;
    const html = pendingHtml(s);
    el.innerHTML = html;
    el.hidden = !html;
  }

  /**
   * 当前模型能不能读图（宿主 `ai_vendor` 的 caps —— 与那颗 🌏 用的是同一份能力事实）。
   * 拿不到能力就按"不能"处理并说清：宁可发不出去（有原因），也不要发出去之后被引擎拒。
   */
  function visionGate(wrap) {
    const caps = wrap._vendor && wrap._vendor.caps;
    if (!caps) {
      return { ok: false, why: L("还不知道当前模型能不能读图（没取到模型能力）", "the model's vision capability is unknown") };
    }
    if (!caps.multimodal) {
      const m = caps.model || L("当前模型", "the current model");
      return {
        ok: false,
        why: L(
          `当前模型 ${m} 读不了图 —— 换成能读图的模型（如 deepseek-flash），或者用文字描述`,
          `model ${m} cannot read images — switch to a vision model (e.g. deepseek-flash) or describe it in words`
        ),
      };
    }
    return { ok: true, why: "" };
  }

  /** 一条消息随行的截图（缩略图）。没有 src 的先留占位，交给 `hydrateShots` 读盘补上。 */
  function shotStripHtml(m) {
    const list = m.attachments ?? [];
    if (!list.length) return "";
    const items = list
      .map((a, i) => {
        const src = (m._preview && m._preview[i]) || shotCache.get(a.path) || "";
        const kb = a.bytes ? ` · ${Math.round(a.bytes / 1024)} KB` : "";
        const attrs = src ? `src="${esc(src)}"` : `data-shot-src="${esc(a.path)}"`;
        return `<img class="session-shot-thumb session-shot-thumb--msg" ${attrs}` +
          ` data-shot="${esc(a.path)}" alt="${esc(a.name)}" title="${esc(a.name)}${kb}">`;
      })
      .join("");
    return `<div class="session-shots">${items}</div>`;
  }

  /** 把还没有 src 的缩略图按路径补上（读一次、缓存住）—— 重开会话的图就靠它 */
  async function hydrateShots(scope) {
    const invoke = getInvoke();
    const imgs = Array.from(scope.querySelectorAll("img[data-shot-src]"));
    if (!imgs.length || !invoke) return;
    for (const img of imgs) {
      const path = img.dataset.shotSrc;
      try {
        let url = shotCache.get(path);
        if (!url) {
          const f = await invoke("read_file_base64", { path });
          url = "data:" + (f.mime || "image/png") + ";base64," + f.base64;
          shotCache.set(path, url);
        }
        img.src = url;
        delete img.dataset.shotSrc;
      } catch (e) {
        // 图被删了就照实说（不静默留一个破图框），并说清是哪一张
        img.classList.add("session-shot-thumb--missing");
        img.alt = L("图不在了", "image missing");
        img.title = String(e);
        delete img.dataset.shotSrc;
      }
    }
  }

  /**
   * 点缩略图看大图。**自己家的浮层**，不是新窗口/新标签：整个 IDE 活在一个文档里
   * （标签栏、文件树、会话都只是 DOM 状态），导航出去就回不来 —— 见 nav 闸门那条 P0。
   */
  function showShotLarge(src, title) {
    if (!src) return;
    const box = document.createElement("div");
    box.className = "shot-lightbox";
    box.innerHTML = `<img src="${esc(src)}" alt="${esc(title || "")}">` +
      `<div class="shot-lightbox-hint">${esc(title || "")} ${L("（点击任意处关闭）", "(click anywhere to close)")}</div>`;
    const close = () => {
      box.remove();
      document.removeEventListener("keydown", onKey);
    };
    const onKey = (e) => {
      if (e.key === "Escape") close();
    };
    box.addEventListener("click", close);
    document.addEventListener("keydown", onKey);
    document.body.appendChild(box);
  }
'''

# ---------------------------------------------------------------- 待发条 + 📎 的 DOM
DOM_PENDING = '''      `<div class="session-msgs" data-msgs></div>` +
      `<div class="session-pending" data-pending hidden></div>` +
      `<div class="session-input-row">` +'''

DOM_ATTACH_BTN = '''      `<button class="agent-btn session-attach" data-attach title="${L("附件：截图（选图 / 粘贴 / 直接拖进来）", "Attach screenshots (pick / paste / drop)")}">📎</button>` +
      `<input type="file" class="session-file" data-file accept="image/*" multiple hidden>` +
      `<button class="agent-btn session-web" data-web'''

# ---------------------------------------------------------------- 入口接线（📎 / 粘贴 / 拖拽）
WIRING = r'''    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        // Ctrl+Enter = 发送（**不是**那颗按钮的 click）：运行中那颗按钮是"停止"，
        // 手快连按 Ctrl+Enter 不该把正在跑的任务停掉。
        const text = input.value.trim();
        const shots = pendingOf(s);
        // 只有图没有字也算一条消息（引擎要求非空任务，`sendMessage` 会给一句中性说明）
        if ((!text && !shots.length) || busy) return;
        input.value = "";
        sendMessage(s, wrap, text, takePending(s));
      }
    });
    // ---- 截图入口（v1.3）：📎 选图 / 粘贴 / 拖拽 ----
    // 三个入口共用一个落点（`addFiles`）：一份代码、一份闸、一份报错文案。
    const attachBtn = wrap.querySelector("[data-attach]");
    const fileInput = wrap.querySelector("[data-file]");
    /** 能力未知/不支持时禁用 📎 并说明原因（不给一个按下去没反应的按钮） */
    function paintAttach() {
      const g = visionGate(wrap);
      attachBtn.disabled = !g.ok;
      attachBtn.title = g.ok
        ? L(`附件：截图（选图 / 粘贴 / 拖进来）—— 最多 ${MAX_SHOTS} 张`, `Attach screenshots (pick / paste / drop) — up to ${MAX_SHOTS}`)
        : g.why;
    }
    async function addFiles(files) {
      const list = Array.from(files || []);
      if (!list.length) return;
      const g = visionGate(wrap);
      if (!g.ok) return void status(g.why, "error");
      const cur = pendingOf(s);
      if (cur.length + list.length > MAX_SHOTS) {
        return void status(L(`一条消息最多 ${MAX_SHOTS} 张图`, `at most ${MAX_SHOTS} images per message`), "error");
      }
      for (const f of list) {
        try {
          cur.push(await toAttachment(f));
        } catch (e) {
          status(String((e && e.message) || e), "error");
        }
      }
      renderPending(wrap, s);
      input.focus();
    }
    attachBtn.addEventListener("click", () => fileInput.click());
    fileInput.addEventListener("change", () => {
      addFiles(fileInput.files);
      fileInput.value = ""; // 同一个文件连选两次也要有反应
    });
    // 粘贴：Win+Shift+S / 微信 / 截图工具的图都落在这里；纯文字粘贴照旧不管
    input.addEventListener("paste", (e) => {
      const files = Array.from((e.clipboardData && e.clipboardData.files) || []).filter((f) =>
        String(f.type).startsWith("image/")
      );
      if (!files.length) return;
      e.preventDefault();
      addFiles(files);
    });
    // 拖拽：接整块会话区（拖到输入框外面也算"发给这个会话"）
    wrap.addEventListener("dragover", (e) => {
      if (!Array.from((e.dataTransfer && e.dataTransfer.types) || []).includes("Files")) return;
      e.preventDefault();
      wrap.classList.add("session-chat--drop");
    });
    wrap.addEventListener("dragleave", () => wrap.classList.remove("session-chat--drop"));
    wrap.addEventListener("drop", (e) => {
      const files = Array.from((e.dataTransfer && e.dataTransfer.files) || []).filter((f) =>
        String(f.type).startsWith("image/")
      );
      if (!files.length) return;
      e.preventDefault();
      wrap.classList.remove("session-chat--drop");
      addFiles(files);
    });
    renderPending(wrap, s);'''

DOM_ATTACH_TAIL = '''    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        // Ctrl+Enter = 发送（**不是**那颗按钮的 click）：运行中那颗按钮是"停止"，
        // 手快连按 Ctrl+Enter 不该把正在跑的任务停掉。
        const text = input.value.trim();
        if (!text || busy) return;
        input.value = "";
        sendMessage(s, wrap, text);
      }
    });'''

SEND_BTN_OLD = '''    wrap.querySelector("[data-run]").addEventListener("click", () => {
      // 同一个按钮两件事：运行中 = 停止；空闲 = 发送。判据只有这一个 `busy`。
      if (busy) return void stopRun();
      const text = input.value.trim();
      if (!text) return;
      input.value = "";
      sendMessage(s, wrap, text);
    });'''
SEND_BTN_NEW = '''    wrap.querySelector("[data-run]").addEventListener("click", () => {
      // 同一个按钮两件事：运行中 = 停止；空闲 = 发送。判据只有这一个 `busy`。
      if (busy) return void stopRun();
      const text = input.value.trim();
      const shots = pendingOf(s);
      if (!text && !shots.length) return;
      input.value = "";
      sendMessage(s, wrap, text, takePending(s));
    });'''

DELEGATED_OLD = '''    wrap.addEventListener("click", (e) => {
      const opt = e.target.closest("[data-ask-opt]");'''
DELEGATED_NEW = '''    wrap.addEventListener("click", (e) => {
      // 缩略图：点开看大图（自家浮层，不开新窗口）
      const thumb = e.target.closest("img.session-shot-thumb");
      if (thumb && thumb.src) return void showShotLarge(thumb.src, thumb.alt);
      // 待发条上的 ✕：把这张从这次发送里去掉
      const drop = e.target.closest("[data-shot-drop]");
      if (drop) {
        pendingOf(s).splice(Number(drop.dataset.shotDrop), 1);
        renderPending(wrap, s);
        return;
      }
      const opt = e.target.closest("[data-ask-opt]");'''

FILL_OLD = '''    el.innerHTML = s.messages.map(msgHtml).join("");
    el.scrollTop = el.scrollHeight;'''
FILL_NEW = '''    el.innerHTML = s.messages.map(msgHtml).join("");
    // 截图缩略图：有 src 的当场画出来，没有的（重开会话）按路径读盘补上 —— 异步，不挡渲染
    hydrateShots(el).catch(() => {});
    el.scrollTop = el.scrollHeight;'''

MSG_OLD = '''  function msgHtml(m) {
    if (m.role === "system") {'''
MSG_NEW = '''  function msgHtml(m) {
    if (m.role === "system") {'''
SHOT_BUBBLE_OLD = '''    const bubble = mine
      ? `<div class="session-bubble">${esc(m.text)}</div>`'''
SHOT_BUBBLE_NEW = '''    // 用户消息的截图贴在气泡**上面**（先看现场再看话）
    const shots = mine ? shotStripHtml(m) : "";
    const bubble = mine
      ? `<div class="session-bubble">${esc(m.text)}</div>`'''
RETURN_OLD = '''      (mine ? bubble : traceHtml(m, live) + askHtml(m) + gateHtml(m) + bubble) +'''
RETURN_NEW = '''      (mine ? shots + bubble : traceHtml(m, live) + askHtml(m) + gateHtml(m) + bubble) +'''

SEND_HEAD_OLD = '''  async function sendMessage(s, wrap, text) {
    s.title = s.title || truncate(text, 24);'''
SEND_HEAD_NEW = '''  async function sendMessage(s, wrap, text, shots) {
    const withShots = shots ?? [];
    // 只有图没有字：引擎要求非空任务（空消息会被当面拒），给一句中性的说明当任务
    const task = text || (withShots.length ? L("看一下我发的截图", "look at this screenshot") : "");
    s.title = s.title || truncate(task, 24);'''

SEND_PUSH_OLD = '''    s.messages.push({ role: "user", text, ts: nowHms(), run_id: null, status: null });'''
SEND_PUSH_NEW = '''    // `attachments` 先空着，等宿主回执（落盘后的路径）再回填；
    // `_preview` 是内存里的 data URL —— `persist` 会把 `_` 开头的字段剥掉，不进会话文件
    const userMsg = {
      role: "user",
      text: task,
      ts: nowHms(),
      run_id: null,
      status: null,
      attachments: [],
      _preview: withShots.map((p) => p._preview),
    };
    s.messages.push(userMsg);'''

INVOKE_OLD = '''      rep = await invoke("agent_reply", { task: text, history, mode, projectRoot: root(), sessionId: (s && s.id) || null });'''
INVOKE_NEW = '''      rep = await invoke("agent_reply", { task: task, history, mode, projectRoot: root(), sessionId: (s && s.id) || null, attachments: withShots.map((p) => ({ name: p.name, mime: p.mime, data_base64: p.data_base64 })) });'''

BACKFILL_OLD = '''      placeholder.text = rep.answer || L("（空回复）", "(empty reply)");'''
BACKFILL_NEW = '''      placeholder.text = rep.answer || L("（空回复）", "(empty reply)");
      // 宿主回执：图落在 <便携根>/projects/<键>/shots/，路径进会话、预览进内存缓存
      userMsg.attachments = rep.shots ?? [];
      withShots.forEach((p, i) => {
        const shot = userMsg.attachments[i];
        if (shot) shotCache.set(shot.path, p._preview);
      });'''

PERSIST_OLD = '''      const saved = await invoke("agent_session_save", {
        sessionJson: JSON.stringify(s),'''
PERSIST_NEW = '''      // `_` 开头的消息字段是**临时**的（`_preview` = 截图的 data URL，一张几 MB）——
      // 原样 stringify 会把几 MB 的 base64 写进会话文件。落盘只带真字段。
      const onDisk = {
        ...s,
        messages: s.messages.map((m) => {
          const keep = {};
          for (const [k, v] of Object.entries(m)) if (!k.startsWith("_")) keep[k] = v;
          return keep;
        }),
      };
      const saved = await invoke("agent_session_save", {
        sessionJson: JSON.stringify(onDisk),'''

RELOAD_OLD = '''      paintModel();
      paintWeb();
      // 首次就没取到清单'''
RELOAD_NEW = '''      paintModel();
      paintWeb();
      paintAttach();
      // 首次就没取到清单'''

TAIL_OLD = '''    paintModel();
    paintWeb();

    /** 运行中按同一个按钮 = 停止'''
TAIL_NEW = '''    paintModel();
    paintWeb();
    paintAttach();

    /** 运行中按同一个按钮 = 停止'''

EDITS = [
    ("helpers", '''  function nowHms() {
    return new Date().toTimeString().slice(0, 8);
  }
''', '''  function nowHms() {
    return new Date().toTimeString().slice(0, 8);
  }
''' + HELPERS),
    ("pending-dom",
     '      `<div class="session-msgs" data-msgs></div>` +\n      `<div class="session-input-row">` +',
     DOM_PENDING),
    ("attach-btn", '''      `<button class="agent-btn session-web" data-web''', DOM_ATTACH_BTN),
    ("entry-wiring", DOM_ATTACH_TAIL, WIRING),
    ("send-btn", SEND_BTN_OLD, SEND_BTN_NEW),
    ("delegated-click", DELEGATED_OLD, DELEGATED_NEW),
    ("fill-msgs", FILL_OLD, FILL_NEW),
    ("shot-bubble", SHOT_BUBBLE_OLD, SHOT_BUBBLE_NEW),
    ("return-shots", RETURN_OLD, RETURN_NEW),
    ("send-head", SEND_HEAD_OLD, SEND_HEAD_NEW),
    ("send-push", SEND_PUSH_OLD, SEND_PUSH_NEW),
    ("invoke", INVOKE_OLD, INVOKE_NEW),
    ("backfill", BACKFILL_OLD, BACKFILL_NEW),
    ("persist", PERSIST_OLD, PERSIST_NEW),
    ("reload-vendor", RELOAD_OLD, RELOAD_NEW),
    ("paint-tail", TAIL_OLD, TAIL_NEW),
    # takePending：把待发列表交出去并清空（发送路径唯一）
    ("takePending", '''  /** 这条会话待发的图（`s._pending`；**不进会话文件** —— 发出去之后宿主的回执才是事实） */''',
     '''  /** 取走待发列表并清空（发送路径只有这一条，免得两处各自清自己的） */
  function takePending(s) {
    const list = pendingOf(s);
    s._pending = [];
    return list;
  }

  /** 这条会话待发的图（`s._pending`；**不进会话文件** —— 发出去之后宿主的回执才是事实） */'''),
]


def main():
    t = P.read_text(encoding="utf-8")
    NL = "\r\n" if "\r\n" in t else "\n"
    flat = t.replace("\r\n", "\n")
    bad = 0
    for name, old, new in EDITS:
        n = flat.count(old)
        if n != 1:
            print(f"!! {name}: 锚点命中 {n} 次（期望 1）")
            bad += 1
            continue
        flat = flat.replace(old, new)
        print(f"ok {name}")
    if bad:
        print(f"有 {bad} 处没对上，未写盘")
        sys.exit(1)
    P.write_text(flat.replace("\n", NL), encoding="utf-8", newline="")
    print("session.js 已改")


if __name__ == "__main__":
    main()
