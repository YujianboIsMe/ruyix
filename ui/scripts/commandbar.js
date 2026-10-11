/**
 * ruyix — 键盘快捷键（命令栏之外的全局按键层）
 *
 * 这个模块管：编辑区的全局按键（Ctrl+S 保存 / Ctrl+W 关标签页 / Ctrl+P 等）。
 * 命令栏本体（setupCommandBar）住在 command.js —— 它比本文件先加载，这里只是把它
 * 一并挂进命名空间。
 *
 * 经典脚本（非 ES module）：与 main.js 共享同一个全局作用域，顶层 function 声明仍是
 * 全局的（initApp 按原次序裸名调用）；应用状态一律读 window.state（main.js 里显式导出）。
 * 门禁：ui-smoke U56/U57。
 */


// ============================================
// 键盘快捷键
// ============================================

function setupKeyboardShortcuts() {
  const isSaveShortcut = (e) =>
    (e.ctrlKey || e.metaKey) &&
    (e.code === "KeyS" || e.key === "s" || e.key === "S" || e.keyCode === 83);

  const handleSave = (e) => {
    if (isSaveShortcut(e)) {
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();
      saveCurrentFile().catch((err) => setStatus(I18N.t("save.fail", { err }), "error"));
    }
  };

  // 策略 1: window 捕获阶段 — 最早拦截 WebView2 可能的行为
  window.addEventListener("keydown", handleSave, true);

  // 策略 2: document 冒泡阶段 — 兜底
  document.addEventListener("keydown", handleSave, false);

  // 策略 3: 编辑器 textarea 直连 — 编辑时焦点在此处
  const textarea = document.getElementById("editor-textarea");
  if (textarea) {
    textarea.addEventListener("keydown", handleSave);

    // Tab 键插入缩进（而非切换焦点）
    textarea.addEventListener("keydown", (e) => {
      if (e.key === "Tab" && !textarea.readOnly) {
        e.preventDefault();
        const start = textarea.selectionStart;
        const end = textarea.selectionEnd;
        textarea.setRangeText("\t", start, end, "end");
        textarea.selectionStart = textarea.selectionEnd = start + 1;
        // 触发 input 事件以便 setupTextareaSync 同步内容
        textarea.dispatchEvent(new Event("input", { bubbles: true }));
      }
    });
  }

  // 策略 4: 监听 Rust 端原生菜单快捷键事件（WebView2 拦截 JS Ctrl+S 时的兜底方案）
  try {
    const tauriEvent = window.__TAURI__?.event;
    if (tauriEvent && typeof tauriEvent.listen === "function") {
      tauriEvent.listen("menu-save", () => {
        saveCurrentFile().catch((err) => setStatus(I18N.t("save.fail", { err }), "error"));
      });
    }
  } catch {
    // 浏览器开发模式忽略
  }
}
// ============================================
// 模块导出（经典脚本：这些函数本身也仍是全局的，见文件头）
// ============================================

window.CommandBarUI = {

// setupCommandBar 定义在 command.js（先于本文件加载）—— 这里只是挂进命名空间。
  setupKeyboardShortcuts,
  setupCommandBar,
};
