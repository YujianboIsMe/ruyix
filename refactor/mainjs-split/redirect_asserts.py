#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""接线第二半：把门禁里「按文件名找代码」的断言重定向到**前端全量源码**。

背景（见 doc/重构-main.js拆分-交接.md）：`ui/scripts/main.js` 拆成 7 个模块之后，
ui-smoke 里那一批 `read("ui/scripts/main.js")` 与 5 个布局探针还都钉着旧文件名 ——
它们报的是「文件挪了」，不是「契约破了」。这里一律改成读 `scripts/ui-sources.js` 给出的
**前端全量源码**（按 index.html 里真实的 <script> 顺序），只有指明 main.js 独有职责的
断言（`window.state` 的显式导出、main.js 不许再声明顶层 L）继续读 main.js。

铁律（skill: scripted-source-edits）：锚点只认文本、命中数必须等于期望、CRLF 感知、
幂等（已改过就跳过）、改完立刻 `node --check`。
"""
import io
import os
import sys

# 脚本住在 refactor/mainjs-split/ ⇒ 仓库根要上溯三层
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FAILED = []


def load(p):
    return io.open(os.path.join(ROOT, p), encoding="utf-8", newline="").read()


def save(p, t):
    io.open(os.path.join(ROOT, p), "w", encoding="utf-8", newline="").write(t)


def nl_of(t):
    return "\r\n" if "\r\n" in t else "\n"


def sub(t, old, new, expect, note):
    """整段替换：命中数必须等于 expect；幂等 —— 新文本已在（或要删的已没有）就跳过。"""
    NL = nl_of(t)
    o, n = old.replace("\n", NL), new.replace("\n", NL)
    if n and t.count(n) >= 1:
        print("  = 已改过：%s" % note)
        return t
    if not n and t.count(o) == 0:
        print("  = 已改过（要删的不在了）：%s" % note)
        return t
    c = t.count(o)
    if c != expect:
        FAILED.append("%s：命中 %d 次，期望 %d" % (note, c, expect))
        print("  ! 未命中：%s（实测 %d 次）" % (note, c))
        return t
    print("  + %s（%d 处）" % (note, c))
    return t.replace(o, n)


# ---------------------------------------------------------------- 共用清单
UI_SOURCES = '''/**
 * 前端一方脚本的源码清单 —— 从 `ui/index.html` 里**真的** `<script src="scripts/*.js">` 取，
 * 按页面的加载顺序。
 *
 * 为什么要有这个文件：`ui/scripts/main.js` 拆成 7 个模块（titlebar / menus / contextmenu /
 * commandbar / navigator / editor / terminal）之后，"把前端源码拼起来跑一遍"的探针
 * （面板回放 / 布局探针 / 宽行探针 / 标签菜单探针…）若各自硬编码 `main.js`，一次合法的文件切分
 * 就会让它们集体失灵 —— 而它们的判据本来与文件名无关（同一个道理见 ui-smoke 的 readEngineAgent：
 * 契约的对象是模块，不是文件名）。
 *
 * 顺手的好处：清单从 index.html 现取，"页面漏挂某个脚本"在探针侧就可见了
 * （真浏览器那条端到端判据是 U57 / scripts/startup-probe.js）。
 */
const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");

/** index.html 里按顺序被引用的 `ui/scripts/*.js`（同一文件重复 include 只算一次）。 */
function uiScriptFiles() {
  const html = fs.readFileSync(path.join(ROOT, "ui", "index.html"), "utf8");
  const seen = new Set();
  const out = [];
  for (const m of html.matchAll(/<script\\s+src="scripts\\/([\\w.-]+\\.js)"/g)) {
    if (seen.has(m[1])) continue;
    seen.add(m[1]);
    out.push(m[1]);
  }
  return out;
}

/** 相对仓库根的路径，如 `ui/scripts/titlebar.js`。 */
function uiScriptPaths() {
  return uiScriptFiles().map((f) => "ui/scripts/" + f);
}

/** 全量源码（行尾归一 LF，按页面顺序用换行拼）。 */
function uiScriptSource() {
  return uiScriptPaths()
    .map((p) => fs.readFileSync(path.join(ROOT, p), "utf8").replace(/\\r\\n/g, "\\n"))
    .join("\\n");
}

module.exports = { uiScriptFiles, uiScriptPaths, uiScriptSource };
'''


def write_ui_sources():
    p = os.path.join(ROOT, "scripts", "ui-sources.js")
    if os.path.exists(p):
        print("  = scripts/ui-sources.js 已存在")
        return
    io.open(p, "w", encoding="utf-8", newline="").write(UI_SOURCES.replace("\n", "\r\n"))
    print("  + 新建 scripts/ui-sources.js")


# ---------------------------------------------------------------- ui-smoke.js
def patch_ui_smoke():
    p = "scripts/ui-smoke.js"
    t = load(p)

    t = sub(t,
            'const { spawnSync } = require("child_process");\n',
            'const { spawnSync } = require("child_process");\n'
            'const { uiScriptSource } = require("./ui-sources.js");\n',
            1, "require ui-sources")

    t = sub(t,
            'const readLf = (p) => read(p).replace(/\\r\\n/g, "\\n");\n',
            'const readLf = (p) => read(p).replace(/\\r\\n/g, "\\n");\n'
            '\n'
            '/**\n'
            ' * 前端一方脚本的**全量源码**：按 ui/index.html 里真实的 <script src="scripts/*.js">\n'
            ' * 顺序拼接（行尾归一 LF）。清单在 scripts/ui-sources.js —— 与布局探针共用同一份。\n'
            ' *\n'
            ' * 为什么不再钉死 ui/scripts/main.js：契约的对象是**前端模块**，不是某个文件名 ——\n'
            ' * 引擎侧早就这么读（见 readEngineAgent：主循环从 agent.rs 搬去 tool_loop.rs 时，\n'
            ' * 钉死文件名的 8 条契约集体报"文件挪了"，那不是在报"契约破了"）。main.js 拆成 7 个\n'
            ' * 模块（titlebar / menus / contextmenu / commandbar / navigator / editor / terminal）\n'
            ' * 之后同理：凡"代码里写了 X"、"不许出现 X"、"切一段源码出来跑"的断言都读这里；\n'
            ' * 只有**指明 main.js 独有职责**的断言（window.state 的显式导出、main.js 不许再\n'
            ' * 声明顶层 L）才继续读 main.js。\n'
            ' */\n'
            'const readUiScripts = () => uiScriptSource();\n',
            1, "readUiScripts 助手")

    # runStaticChecks：三处「读 main.js」合并成一处「读前端全量」
    head, rest = t.split("function runStaticChecks() {", 1)
    body, tail = rest.split("\nfunction makeEl(id) {", 1)
    body = sub(body,
               '  const commandJs = read("ui/scripts/command.js");\n',
               '  const commandJs = read("ui/scripts/command.js");\n'
               '  // 前端一方脚本全量（7 个模块 + main.js 按页面顺序拼，见 readUiScripts）\n'
               '  const uiSrc = readUiScripts();\n',
               1, "runStaticChecks 声明 uiSrc")
    body = sub(body, '  const ctxMainJs = readLf("ui/scripts/main.js");\n', "", 1, "去掉 ctxMainJs 声明")
    body = sub(body, '  const mainJs = read("ui/scripts/main.js");\n', "", 1, "去掉 mainJs 声明")
    body = sub(body, '  const uiMainJs = read("ui/scripts/main.js");\n', "", 1, "去掉 uiMainJs 声明")
    for old, new, note in [("ctxMainJs", "uiSrc", "变量 ctxMainJs → uiSrc"),
                           ("uiMainJs", "uiSrc", "变量 uiMainJs → uiSrc")]:
        c = body.count(old)
        if c == 0:
            if "uiSrc" in body:      # 上一趟已经改过，名字不在了
                print("  = 已改过：%s" % note)
                continue
            FAILED.append("runStaticChecks 里找不到 %s" % old)
        body = body.replace(old, new)
        print("  + %s（%d 处）" % (note, c))
    c = body.count("mainJs")
    body = body.replace("mainJs", "uiSrc")
    print("  + 变量 mainJs → uiSrc（%d 处）" % c)
    t = head + "function runStaticChecks() {" + body + "\nfunction makeEl(id) {" + tail

    t = sub(t, '    "main.js 的右键 switch 没有 copy-path / copy-full-path 两个分支");',
            '    "前端脚本的右键 switch 没有 copy-path / copy-full-path 两个分支");',
            1, "U33 文案")
    # U34 的切片判据后来被 fix_scope.py 收到编辑器模块（连同这条文案）——
    # 重跑本脚本时它可能已经不存在了，那是"已改过"，不是"没命中"。
    if '编辑器模块里定位不到 doAutoSave' in t:
        print("  = 已改过：U34 文案（已被 fix_scope.py 收到编辑器模块）")
    else:
        t = sub(t, '    "main.js 里定位不到 doAutoSave（切片锚点失效）");',
                '    "前端脚本里定位不到 doAutoSave（切片锚点失效）");', 1, "U34 文案")
    t = sub(t, '    "main.js 缺少帮助文档链路（按语言加载 md → markdown-it 渲染）");',
            '    "前端脚本缺少帮助文档链路（按语言加载 md → markdown-it 渲染）");', 1, "U22 文案")
    t = sub(t, '      "main.js 中定位不到帮助页代码段（区间标记变了，请同步本回放）");',
            '      "前端脚本中定位不到帮助页代码段（区间标记变了，请同步本回放）");', 1, "U22 回放文案")
    t = sub(t, '    "main.js 里定位不到 closePlan…setupTabContextMenu 这段源码（切片锚点失效）");',
            '    "前端脚本里定位不到 closePlan…setupTabContextMenu 这段源码（切片锚点失效）");',
            1, "U45 文案")
    t = sub(t, '    "main.js 里定位不到 toRelativePath…handleContextCopyPath 这段源码（切片锚点失效）");',
            '    "前端脚本里定位不到 toRelativePath…handleContextCopyPath 这段源码（切片锚点失效）");',
            1, "U33 回放文案")

    # 其余函数里的「读 main.js」→ 读前端全量
    # （runStartupScopeChecks 里那句 `const mainJs = read("ui/scripts/main.js")` 必须**继续**读
    #   main.js —— 它判的正是「main.js 不许再声明顶层 L」，所以这里只锚 runHelpChecks 这一段）
    t = sub(t,
            '  const mainJs = read("ui/scripts/main.js");\n'
            '  const start = mainJs.indexOf("/** markdown-it 渲染器懒构造");\n'
            '  const endMark = mainJs.indexOf("// 编辑器 textarea 同步");\n'
            '  const end = endMark > 0 ? mainJs.lastIndexOf("// ====", endMark) : -1;\n',
            '  const uiSrc = readUiScripts();\n'
            '  const start = uiSrc.indexOf("/** markdown-it 渲染器懒构造");\n'
            '  const endMark = uiSrc.indexOf("// 编辑器 textarea 同步");\n'
            '  const end = endMark > 0 ? uiSrc.lastIndexOf("// ====", endMark) : -1;\n',
            1, "runHelpChecks 读全量")
    t = sub(t, '  const helpSrc = mainJs.slice(start, end);\n',
            '  const helpSrc = uiSrc.slice(start, end);\n', 1, "runHelpChecks 切片")
    t = sub(t, '  const mainSrc = readLf("ui/scripts/main.js");\n',
            '  const mainSrc = readUiScripts();\n', 4, "切片回放读全量（U45 / U33 / U48 / U51）")
    t = sub(t, '  const mjs = readLf("ui/scripts/main.js");\n',
            '  const mjs = readUiScripts();\n', 1, "U53 静态断言读全量")
    t = sub(t, '      read("ui/scripts/main.js") +\n',
            '      readUiScripts() +\n', 2, "U31 / U50 回放装全量")
    t = sub(t, '        readLf("ui/scripts/main.js") +\n',
            '        readUiScripts() +\n', 1, "U53 回放装全量")
    t = sub(t, '  const panelMainJs = read("ui/scripts/main.js");\n',
            '  const panelMainJs = readUiScripts();\n', 1, "U54 读全量")

    # 回放沙箱：模块里读的是 window.state（照真页面装配补上）
    t = sub(t,
            '    window: {\n'
            '      location: { origin: "https://ruyix.localhost" },\n'
            '      markdownit,\n'
            '    },\n',
            '    window: {\n'
            '      location: { origin: "https://ruyix.localhost" },\n'
            '      markdownit,\n'
            '      // 模块里读的是 window.state（main.js 顶层 const 不挂 window，靠显式导出）：\n'
            '      // 切片改成取自模块之后，回放沙箱也要照真页面的装配给上这一份\n'
            '      state: appState,\n'
            '    },\n',
            1, "U22 回放沙箱给 window.state")

    t = sub(t,
            '  const mod = new Function(\n'
            '    "document",\n'
            '    "state",\n'
            '    "closeTab",\n',
            '  const mod = new Function(\n'
            '    "document",\n'
            '    "window",\n'
            '    "state",\n'
            '    "closeTab",\n',
            1, "U45 回放沙箱加 window 形参")
    t = sub(t,
            '  )(\n'
            '    docStub,\n'
            '    state,\n'
            '    (id) => closed.push(id),\n',
            '  )(\n'
            '    docStub,\n'
            '    { state },\n'
            '    state,\n'
            '    (id) => closed.push(id),\n',
            1, "U45 回放喂 window")

    t = sub(t,
            '  const factory = new Function("state", "I18N", "setStatus", "navigator", "document",\n'
            '    `${body}\\nreturn { toRelativePath, handleContextCopyPath };`);\n'
            '  const mod = factory(state, I18N, setStatus, navigatorStub, documentStub);\n',
            '  const factory = new Function("window", "state", "I18N", "setStatus", "navigator",\n'
            '    "document", `${body}\\nreturn { toRelativePath, handleContextCopyPath };`);\n'
            '  const mod = factory({ state }, state, I18N, setStatus, navigatorStub, documentStub);\n',
            1, "U33 回放沙箱加 window 形参")

    save(p, t)


# ---------------------------------------------------------------- 布局探针
def patch_probe(p, edits, note):
    t = load(p)
    for old, new, expect, what in edits:
        t = sub(t, old, new, expect, "%s：%s" % (note, what))
    save(p, t)


def main():
    print("① 新建共享清单")
    write_ui_sources()

    print("② ui-smoke.js")
    patch_ui_smoke()

    REQ = 'const read = (p) => fs.readFileSync(path.join(ROOT, p), "utf8");'
    REQ_NEW = REQ + '\nconst { uiScriptSource } = require("./ui-sources.js");'

    print("③ editor-layout.js")
    patch_probe("scripts/editor-layout.js", [
        (REQ, REQ_NEW, 1, "require"),
        ('const mainSrc = read("ui/scripts/main.js").replace(',
         'const mainSrc = uiScriptSource().replace(', 1, "读全量"),
        (' * ui/scripts/main.js 拼成一个自包含页面，在无头 Edge 里打开、喂一份 5000 行的合成源码',
         ' * ui/scripts/*.js（按 index.html 的顺序，见 scripts/ui-sources.js）拼成一个自包含页面，\n'
         ' * 在无头 Edge 里打开、喂一份 5000 行的合成源码', 1, "头注释"),
    ], "editor-layout")

    print("④ editor-wide-line.js")
    patch_probe("scripts/editor-wide-line.js", [
        (REQ, REQ_NEW, 1, "require"),
        ('const mainSrc = read("ui/scripts/main.js").replace(',
         'const mainSrc = uiScriptSource().replace(', 1, "读全量"),
        (' * 做法：把**真实的** ui/index.html（剥 <script>/<link>）+ ui/styles.css + ui/scripts/main.js\n'
         ' * 拼成一个自包含页面（main.js 的引导块剥掉，只借函数），在无头 Edge 里跑四个臂：',
         ' * 做法：把**真实的** ui/index.html（剥 <script>/<link>）+ ui/styles.css + ui/scripts/*.js\n'
         ' * （按 index.html 的顺序，见 scripts/ui-sources.js）拼成一个自包含页面（引导块剥掉，\n'
         ' * 只借函数），在无头 Edge 里跑四个臂：', 1, "头注释"),
        ('  console.log("FAIL: 在 main.js 里定位不到阈值声明（探针锚点失效）：" + THRESHOLD_DECL);',
         '  console.log("FAIL: 在 ui/scripts/ 里定位不到阈值声明（探针锚点失效）：" + THRESHOLD_DECL);',
         1, "文案"),
        ('    // 同一份 main.js 装两遍：真实阈值一遍、阈值调到天上（= 无闸门）一遍',
         '    // 同一份前端源码装两遍：真实阈值一遍、阈值调到天上（= 无闸门）一遍', 1, "注释"),
    ], "editor-wide-line")

    print("⑤ terminal-layout.js")
    patch_probe("scripts/terminal-layout.js", [
        (REQ, REQ_NEW, 1, "require"),
        (' * 真 main.js，形参照旧走 `window.__TAURI__` 桩），在无头 Edge 里：`showTerminalView()` →',
         ' * 真 ui/scripts/*.js，形参照旧走 `window.__TAURI__` 桩），在无头 Edge 里：`showTerminalView()` →',
         1, "头注释"),
        ('    `<script>eval(${enc(read("ui/scripts/main.js"))})</script>` +',
         '    `<script>eval(${enc(uiScriptSource())})</script>` +', 1, "读全量"),
    ], "terminal-layout")

    print("⑥ memory-layout.js")
    patch_probe("scripts/memory-layout.js", [
        (REQ, REQ_NEW, 1, "require"),
        (' * 做法：拼一个自包含页面（真 index.html 骨架 + 真 styles.css + 真 main.js + 真 memory.js，',
         ' * 做法：拼一个自包含页面（真 index.html 骨架 + 真 styles.css + 真 ui/scripts/*.js，', 1, "头注释"),
        ('    `<script>eval(${enc(read("ui/scripts/main.js"))})</script>` +\n'
         '    `<script>eval(${enc(read("ui/scripts/memory.js"))})</script>` +',
         '    // 全量清单里已经带了 memory.js（index.html 的真实顺序），不再单独装一遍\n'
         '    `<script>eval(${enc(uiScriptSource())})</script>` +', 1, "读全量"),
    ], "memory-layout")

    print("⑦ tab-menu-layout.js")
    patch_probe("scripts/tab-menu-layout.js", [
        (REQ, REQ_NEW, 1, "require"),
        ('const mainSrc = read("ui/scripts/main.js");',
         'const mainSrc = uiScriptSource();', 1, "读全量"),
        ('  console.log("FAIL: main.js 里定位不到 closePlan…setupTabContextMenu 这段源码（切片锚点失效）");',
         '  console.log("FAIL: 前端源码里定位不到 closePlan…setupTabContextMenu 这段源码（切片锚点失效）");',
         1, "文案"),
        ('    const mod = new Function(\n'
         '      "document",\n'
         '      "state",\n',
         '    const mod = new Function(\n'
         '      "document",\n'
         '      "window",\n'
         '      "state",\n', 1, "回放沙箱加 window 形参"),
        ('    )(\n'
         '      document,\n'
         '      state,\n',
         '    )(\n'
         '      document,\n'
         '      { state },\n'
         '      state,\n', 1, "回放喂 window"),
    ], "tab-menu")

    if FAILED:
        print("\n未命中（必须修）：")
        for f in FAILED:
            print("  - " + f)
        return 1
    print("\n全部锚点命中。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
