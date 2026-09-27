#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""接线的第三半：把**文件作用域**的断言收回它真正的对象。

全量读（readUiScripts）适合「代码里写了 X」；但有两类判据不能读全量，读了会把**合法的**
调用算成违规 —— 那不是更严，那是判错了对象：

  ① 「不许出现 X」：命令层里 `invoke("config_set")` / `invoke("project_bucket_delete")`
     是命令系统的本体，本来就该有（U48 / U51 判的是**面板**不许绕开命令层）；
  ② 「只许有一处调用」：`highlightAndRender` 的另一处调用在 command.js 的显式换语言路径上
     （用户点「重新着色」就该重着色，U34 判的是**编辑器模块**不许在打字路径上跑全量）。

另修 U53 回放的一处装配缺口：真页面里 errors.js 把 BackendMsg 挂到 window 上，
main.js 用的是**裸 BackendMsg**（浏览器里 window 的属性就是全局变量）；而回放把源码塞进
`new Function`，自由变量解析到的是本进程的 globalThis —— 不显式传进去，降级场景走到
setStatus 就 ReferenceError，红出来的方向还是错的。
"""
import io
import os
import sys

# 脚本住在 refactor/mainjs-split/ ⇒ 仓库根要上溯三层
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
P = os.path.join(ROOT, "scripts", "ui-smoke.js")
FAILED = []


def nl_of(t):
    return "\r\n" if "\r\n" in t else "\n"


def sub(t, old, new, expect, note):
    NL = nl_of(t)
    o, n = old.replace("\n", NL), new.replace("\n", NL)
    if n and t.count(n) >= 1:
        print("  = 已改过：%s" % note)
        return t
    c = t.count(o)
    if c != expect:
        FAILED.append("%s：命中 %d，期望 %d" % (note, c, expect))
        print("  ! 未命中：%s（%d 次）" % (note, c))
        return t
    print("  + %s（%d 处）" % (note, c))
    return t.replace(o, n)


MODULE_HELPER = '''
/**
 * 单个前端模块的源码（LF 归一）—— 只在断言的语义**真的**只关乎某一个模块时才用它。
 *
 * 有两类判据不能读全量（readUiScripts）：「不许出现 X」与「只许有一处调用」。
 * 命令层里 `invoke("config_set")` / `invoke("project_bucket_delete")` 是**应该**有的
 * （那是命令系统的本体），`highlightAndRender` 的另一处调用在 command.js 的显式换语言
 * 路径上（用户点「重新着色」就该重着色）—— 读全量会把合法的调用算成违规。
 * 那不是「更严」，那是判错了对象。
 */
const readUiModule = (file) => readLf("ui/scripts/" + file);
'''

BACKENDMSG_HELPER = '''
/**
 * realBackendMsg —— 与 realL 同理：真页面里 errors.js 先加载并把 `BackendMsg` 挂到 window 上，
 * 而 main.js 的 setStatus 用的是**裸 `BackendMsg`**（浏览器里 window 的属性就是全局变量）。
 * 回放把整份前端源码塞进 `new Function` 时，自由变量解析到的是**本进程**的 globalThis，
 * 所以这一份也得显式当形参传进去 —— 否则 U53 的降级场景走到 setStatus 就 ReferenceError，
 * 表现成「状态栏没说人话」（红出来的方向还是错的）。
 */
function realBackendMsg(win, doc) {
  new Function("window", "document", readLf("ui/scripts/errors.js"))(win, doc);
  return win.BackendMsg;
}
'''


def main():
    t = io.open(P, encoding="utf-8", newline="").read()

    # 0) 顺手收掉助手块后面多出来的一个空行
    t = sub(t,
            'const readUiScripts = () => uiScriptSource();\n\n\n/**\n * realL',
            'const readUiScripts = () => uiScriptSource();\n'
            + MODULE_HELPER.rstrip("\n") + '\n\n/**\n * realL',
            1, "readUiModule 助手（并收掉多余空行）")

    # 1) realBackendMsg
    t = sub(t,
            'function realL(win, doc, i18n) {\n'
            '  new Function("window", "document", "console", "I18N", readLf("ui/scripts/command.js"))(\n'
            '    win,\n'
            '    doc,\n'
            '    console,\n'
            '    i18n\n'
            '  );\n'
            '  return win.L;\n'
            '}\n',
            'function realL(win, doc, i18n) {\n'
            '  new Function("window", "document", "console", "I18N", readLf("ui/scripts/command.js"))(\n'
            '    win,\n'
            '    doc,\n'
            '    console,\n'
            '    i18n\n'
            '  );\n'
            '  return win.L;\n'
            '}\n' + BACKENDMSG_HELPER.rstrip("\n") + '\n',
            1, "realBackendMsg 助手")

    # 2) U34 收到编辑器模块
    u34_old = (
        '  // U34 autosave-no-ui：自动保存**只写盘**。着色（全量 tree-sitter + 一趟 IPC）与大纲\n'
        '  // （O(全文) 解析 + 重建 DOM）不许挂在每次打字停顿上，两者只能从 refreshEditorChrome 出去\n'
        '  const autoStart = uiSrc.indexOf("async function doAutoSave(tab) {");\n'
        '  const autoEnd = autoStart >= 0 ? uiSrc.indexOf("\\n}\\n", autoStart) : -1;\n'
        '  check("U34", "autosave-no-ui", autoStart >= 0 && autoEnd > autoStart,\n'
        '    "前端脚本里定位不到 doAutoSave（切片锚点失效）");\n'
        '  const autoBody = autoStart >= 0 && autoEnd > autoStart\n'
        '    ? uiSrc.slice(autoStart, autoEnd) : "";\n'
        '  check("U34", "autosave-no-ui",\n'
        '    autoBody.includes("write_file") && !autoBody.includes("highlightAndRender") &&\n'
        '      !autoBody.includes("updateOutline"),\n'
        '    "doAutoSave 只该有 write_file —— 带 UI 刷新就等于每次打字停顿跑一趟全量 tree-sitter");\n'
        '  const debStart = uiSrc.indexOf("debounceTimer = setTimeout(() => {");\n'
        '  const debEnd = debStart >= 0 ? uiSrc.indexOf("}, 1000);", debStart) : -1;\n'
        '  const debBody = debStart >= 0 && debEnd > debStart ? uiSrc.slice(debStart, debEnd) : "";\n'
        '  check("U34", "autosave-no-ui",\n'
        '    debBody.includes("doAutoSave") && !debBody.includes("updateOutline") &&\n'
        '      !debBody.includes("highlightAndRender"),\n'
        '    "自动保存的防抖回调里只该有 doAutoSave，不该带 UI 刷新");\n'
        '  for (const fn of ["highlightAndRender", "updateOutline"]) {\n'
        '    const calls = [...uiSrc.matchAll(new RegExp(`(?<!function )\\\\b${fn}\\\\(`, "g"))].length;\n'
        '    check("U34", "autosave-no-ui", calls === 1,\n'
        '      `${fn} 的调用点应只有 refreshEditorChrome 里那一处，实际 ${calls} 处`);\n'
        '  }\n'
        '  check("U34", "autosave-no-ui",\n'
        '    [...uiSrc.matchAll(/refreshEditorChrome\\(/g)].length >= 3,\n'
        '    "refreshEditorChrome 要挂在切回标签页 / 失焦 / 显式保存三个时机上");\n'
        '  check("U34", "autosave-no-ui",\n'
        '    /if \\(tab\\._language && !tab\\._highlighted\\)/.test(uiSrc),\n'
        '    "refreshEditorChrome 得先判断高亮是否过期 —— 没过期就别再跑一趟 IPC");\n'
    )
    u34_new = (
        '  // U34 autosave-no-ui：自动保存**只写盘**。着色（全量 tree-sitter + 一趟 IPC）与大纲\n'
        '  // （O(全文) 解析 + 重建 DOM）不许挂在每次打字停顿上，两者只能从 refreshEditorChrome 出去。\n'
        '  // 判据限定在**编辑器模块**：command.js 里还有一处 highlightAndRender（用户显式换语言那条\n'
        '  // 路，是应该有的），读全量会把它算成违规（见 readUiModule 的注释）。\n'
        '  const edSrc = readUiModule("editor.js");\n'
        '  const autoStart = edSrc.indexOf("async function doAutoSave(tab) {");\n'
        '  const autoEnd = autoStart >= 0 ? edSrc.indexOf("\\n}\\n", autoStart) : -1;\n'
        '  check("U34", "autosave-no-ui", autoStart >= 0 && autoEnd > autoStart,\n'
        '    "编辑器模块里定位不到 doAutoSave（切片锚点失效）");\n'
        '  const autoBody = autoStart >= 0 && autoEnd > autoStart\n'
        '    ? edSrc.slice(autoStart, autoEnd) : "";\n'
        '  check("U34", "autosave-no-ui",\n'
        '    autoBody.includes("write_file") && !autoBody.includes("highlightAndRender") &&\n'
        '      !autoBody.includes("updateOutline"),\n'
        '    "doAutoSave 只该有 write_file —— 带 UI 刷新就等于每次打字停顿跑一趟全量 tree-sitter");\n'
        '  const debStart = edSrc.indexOf("debounceTimer = setTimeout(() => {");\n'
        '  const debEnd = debStart >= 0 ? edSrc.indexOf("}, 1000);", debStart) : -1;\n'
        '  const debBody = debStart >= 0 && debEnd > debStart ? edSrc.slice(debStart, debEnd) : "";\n'
        '  check("U34", "autosave-no-ui",\n'
        '    debBody.includes("doAutoSave") && !debBody.includes("updateOutline") &&\n'
        '      !debBody.includes("highlightAndRender"),\n'
        '    "自动保存的防抖回调里只该有 doAutoSave，不该带 UI 刷新");\n'
        '  for (const fn of ["highlightAndRender", "updateOutline"]) {\n'
        '    const calls = [...edSrc.matchAll(new RegExp(`(?<!function )\\\\b${fn}\\\\(`, "g"))].length;\n'
        '    check("U34", "autosave-no-ui", calls === 1,\n'
        '      `${fn} 的调用点应只有 refreshEditorChrome 里那一处，实际 ${calls} 处`);\n'
        '  }\n'
        '  check("U34", "autosave-no-ui",\n'
        '    [...edSrc.matchAll(/refreshEditorChrome\\(/g)].length >= 3,\n'
        '    "refreshEditorChrome 要挂在切回标签页 / 失焦 / 显式保存三个时机上");\n'
        '  check("U34", "autosave-no-ui",\n'
        '    /if \\(tab\\._language && !tab\\._highlighted\\)/.test(edSrc),\n'
        '    "refreshEditorChrome 得先判断高亮是否过期 —— 没过期就别再跑一趟 IPC");\n'
    )
    t = sub(t, u34_old, u34_new, 1, "U34 切片/计数收到编辑器模块")

    # 3) U48：桶面板住在导航区模块
    t = sub(t,
            '  const mainSrc = readUiScripts();\n'
            '  const pathsRs = readLf("src-tauri/src/paths.rs");\n',
            '  // 桶面板住在导航区模块（读全量会让命令层合法的 invoke("project_bucket_delete") 算成违规）\n'
            '  const panelSrc = readUiModule("navigator.js");\n'
            '  const pathsRs = readLf("src-tauri/src/paths.rs");\n',
            1, "U48 收到导航区模块")
    t = sub(t,
            '  check("U48", "bucket-panel-via-command",\n'
            '    mainSrc.includes("handleCommand(`bucket delete ${el.dataset.key}`)") &&\n'
            '      !mainSrc.includes(\'invoke("project_bucket_delete"\'),\n',
            '  check("U48", "bucket-panel-via-command",\n'
            '    panelSrc.includes("handleCommand(`bucket delete ${el.dataset.key}`)") &&\n'
            '      !panelSrc.includes(\'invoke("project_bucket_delete"\'),\n',
            1, "U48 面板不许直接 invoke")
    t = sub(t,
            '  check("U48", "bucket-orphan-marked",\n'
            '    has(mainSrc, "renderProjectBuckets") && has(mainSrc, "bucket.orphan") &&\n'
            '      has(mainSrc, "known.has("),\n',
            '  check("U48", "bucket-orphan-marked",\n'
            '    has(panelSrc, "renderProjectBuckets") && has(panelSrc, "bucket.orphan") &&\n'
            '      has(panelSrc, "known.has("),\n',
            1, "U48 孤儿桶标记")

    # 4) U51：终端面板住在终端模块
    t = sub(t,
            '  const mainSrc = readUiScripts();\n'
            '  const cmdSrc = readLf("ui/scripts/command.js");\n',
            '  // 面板部分只在终端模块里找（读全量会让命令层合法的 invoke("config_set") 算成违规）\n'
            '  const termSrc = readUiModule("terminal.js");\n'
            '  const cmdSrc = readLf("ui/scripts/command.js");\n',
            1, "U51 收到终端模块")
    t = sub(t,
            '    has(html, \'id="terminal-add"\') && has(mainSrc, \'"get_term_targets"\') && has(mainSrc, "renderTerminalTargets"),',
            '    has(html, \'id="terminal-add"\') && has(termSrc, \'"get_term_targets"\') && has(termSrc, "renderTerminalTargets"),',
            1, "U51 终端入口")
    t = sub(t,
            '    has(mainSrc, "handleCommand(`term add ") &&\n'
            '      has(mainSrc, "handleCommand(`term del ") &&\n'
            '      has(cmdSrc, "async function handleTermCommand(") &&\n'
            '      has(cmdSrc, \'case "term":\') &&\n'
            '      // 面板层不许自己写配置（那是命令层的活）\n'
            '      !/terminal[\\s\\S]{0,600}?invoke\\("config_set"/.test(mainSrc),\n',
            '    has(termSrc, "handleCommand(`term add ") &&\n'
            '      has(termSrc, "handleCommand(`term del ") &&\n'
            '      has(cmdSrc, "async function handleTermCommand(") &&\n'
            '      has(cmdSrc, \'case "term":\') &&\n'
            '      // 面板层不许自己写配置（那是命令层的活）\n'
            '      !/terminal[\\s\\S]{0,600}?invoke\\("config_set"/.test(termSrc),\n',
            1, "U51 面板走命令系统")

    # 5) U53 回放：显式传 BackendMsg
    t = sub(t,
            '      const L = realL(sandboxWin, sandboxDoc, sandboxWin.I18N);\n'
            '      new Function(\n'
            '        "window",\n'
            '        "document",\n'
            '        "console",\n'
            '        "I18N",\n'
            '        "L",\n'
            '        readUiScripts() +\n'
            '          "\\n;window.__probe = { loadHighlightPlugins, fileIcon, get state() { return state; } };"\n'
            '      )(sandboxWin, sandboxDoc, console, sandboxWin.I18N, L);\n',
            '      const L = realL(sandboxWin, sandboxDoc, sandboxWin.I18N);\n'
            '      const BackendMsg = realBackendMsg(sandboxWin, sandboxDoc);\n'
            '      new Function(\n'
            '        "window",\n'
            '        "document",\n'
            '        "console",\n'
            '        "I18N",\n'
            '        "L",\n'
            '        "BackendMsg",\n'
            '        readUiScripts() +\n'
            '          "\\n;window.__probe = { loadHighlightPlugins, fileIcon, get state() { return state; } };"\n'
            '      )(sandboxWin, sandboxDoc, console, sandboxWin.I18N, L, BackendMsg);\n',
            1, "U53 回放传 BackendMsg")

    io.open(P, "w", encoding="utf-8", newline="").write(t)
    if FAILED:
        print("\n未命中（必须修）：")
        for f in FAILED:
            print("  - " + f)
        return 1
    print("\n全部锚点命中。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
