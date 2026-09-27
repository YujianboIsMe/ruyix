# main.js 拆分：交接单（未完成，2026-09-27）

**一句话**：`ui/scripts/main.js` 已经搬成 7 个模块（成果在 `refactor/mainjs-split/`），
只差 **①接线 ②删掉 main.js 里两份重复副本** 两步；当前仓库是**绿的**，拆到一半的东西
**没有**进 `ui/`。

## 现状（实测，不是推测）

| 项 | 状态 |
|---|---|
| `ui-smoke` / `style` | **415/415**、0 error（工作区干净、`ui/` 是拆分前的状态） |
| 搬运成果 | `refactor/mainjs-split/{titlebar,menus,contextmenu,commandbar,navigator,editor,terminal}.js`（7 个，语法全过、style 0 error） |
| 命名空间 | 按方案暴露 `window.TitleBarUI / MenusUI / ContextMenuUI / CommandBarUI / NavigatorUI / EditorUI / TerminalUI` |
| main.js | 拆分版是 **1052 行**（原 4250）；当前仓库里是原版 |
| 差的两步 | 见下 |

## ① 接线（CRLF 坑已踩过，照这个写）

把 7 个文件放回 `ui/scripts/`，然后在 `ui/index.html` 里**按行插入**（该文件是 CRLF，
用 LF 锚点会一条也匹配不上）：在含 `scripts/external.js` 的那一行之后插入 7 行，
`main.js` 仍**最后**加载。

```python
CRLF = chr(13) + chr(10)
p = "ui/index.html"
lines = io.open(p, encoding="utf-8", newline="").read().split(CRLF)
i = [k for k, l in enumerate(lines) if "scripts/external.js" in l][0]
lines[i+1:i+1] = ['  <script src="scripts/%s.js"></script>' % n for n in
                  ["titlebar","menus","contextmenu","commandbar","navigator","editor","terminal"]]
io.open(p, "w", encoding="utf-8", newline="").write(CRLF.join(lines))
```

## ② 删掉 main.js 里两份"搬了一半"的副本 ← **唯一实质工作**

经典脚本共享**一个全局作用域**：顶层 `let` 重名时，**后加载的脚本整份不执行**。
`main.js` 最后加载 ⇒ 直接白屏（`state=false`）。

实测到的重复（`node scripts/ui-smoke.js` 的 U56/U57 报出来的）：

| 撞名的顶层声明 | 在模块里 | main.js 里还留着一份 | 说明 |
|---|---|---|---|
| `_ctxPath` / `_ctxIsDir` / `_ctxIsRoot` | `contextmenu.js:17-19` | `main.js:2599-2601` | main.js 里还留着**一份 context-menu 实现**（三个 `let` + 用到它们的函数） |
| `_welcomeLoadToken` | `navigator.js:14` | `main.js:242` | main.js 里还留着**一份 loadWelcome**（`let` + 函数） |

**做法**：在 `main.js` 里把这两块（顶层声明 **连同** 对应的函数实现）整块删掉，
让模块里那份接手 —— 两边是逐字搬运，行为不变（`initApp` 调的是裸函数名，
经典脚本里**最后加载的定义胜出**；删掉 main.js 的副本，模块版本自然接手）。

**别做**：
- 改名绕开撞车（`_ctxPath` → `_cmCtxPath`）：门禁会绿，但**两份实现都还在**，留下死代码；
- 放宽/删掉 U56、U57：它们正是抓出这个 bug 的判据。

## 判据（已存在，不用新写）

- `U56 startup-scope`：顶层词法声明不许重名（会点名是哪两个文件、哪个标识符）；
- `U57 startup-real`：真启动探针 —— 零 SyntaxError + `state` / `showPane` / `L` /
  `EDITOR_PANES` 真的被赋值（脚本没执行时这几个都是 false）。

## 收尾（拆完必须跑）

```bash
node scripts/check-style.js            # 0 error
node scripts/ui-smoke.js               # 全绿（415 条）
node scripts/editor-layout.js          # 真浏览器布局（找不到浏览器时自行 SKIP，如实报告）
node scripts/session-trace-layout.js
```

## 背景（为什么没一次做完）

搬运由子代理做（41 次调用），它撞上迭代上限，**同时** DeepSeek 余额耗尽（HTTP 402）被掐断，
最终报告为空；而且那次 `index.html` 接线与断言重定向都没做。调用方把 `ui/` 恢复成绿、
把成果暂存、把这两处重复的诊断做实 —— 诊断是这活里最难的部分，剩下的删块 + 接线约 15 分钟。
