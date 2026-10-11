# BUG：模拟终端大小不随窗口变化
## 归属版本：v0.11 · 状态：**已修复**（2026-09-24）· 报告：2026-09-24（用户自测发现）

> 用户原话：*"在模拟终端发现问题，模拟终端大小不随窗口变化。"*
> 附 DevTools 现场：`#terminal-container > div > div.xterm-screen` 的 style 是
> `width: 763px; height: 368px` —— 一个**固定的**像素尺寸。

## 1. 根因（两条，缺一不可）

**① 没有任何 resize 路径。** `ui/main.js::spawnTerminal` 里
`new Terminal({ rows: 24, cols: 100 })` 之后，前端**从不**调用后端已有的
`pty_resize`（`src-tauri/src/main.rs` 里那个命令一直躺着没人用），也没有观察容器尺寸。
763×368 正是 100 列 × 24 行在 fontSize 13 下的像素尺寸 —— 与截图逐一对应。

后果有两层：
- 窗口变大 → 终端仍只占一块（其余是空白）；
- 窗口变小 → 容器被内容撑住，外层出横向滚动条；
- 而 PTY 的 winsize 也一直是 24×100 → shell 按旧宽度折行，`ls` 这类输出会错位。

**② CSS 让容器**撑到内容宽度**。** `.terminal-container { min-width: fit-content }` 把容器
的宽度下限锁在"内容（xterm 那一块固定像素）"上 —— 于是窗口变小时容器**不肯缩**，连"尺寸变了"
这件事都观察不到（ResizeObserver 收不到回调），修①也救不回来。这是同一个坑的第二次：
编辑器那边（U32）也踩过"谁该定尺寸"这件事。

## 2. 修法

| 落点 | 改动 |
|---|---|
| `ui/main.js::cellMetrics` | 单元格像素：优先问 xterm 的 render service，量不到就量它维护的 `.xterm-char-measure-element`；**两条都没有就返回 null**（量不出来不许乱改尺寸） |
| `ui/main.js::fitDims` | 纯函数：可用像素 ÷ 单元格 → cols/rows（下限 2×1：0 会让 xterm 抛错） |
| `ui/main.js::fitTerminal` | 算完 **同时** 改 xterm（`term.resize`）与 PTY（`invoke("pty_resize")`）；**幂等**（尺寸没变什么都不做，免得每帧一次 SIGWINCH 重绘）；容器尺寸为 0（标签页被切走）时不动 |
| `ui/main.js::watchTerminalResize` | 一个 ResizeObserver 盯 `#terminal-container`（窗口缩放/面板拖动都会让容器变尺寸），60ms 防抖后适配**当前激活**的终端标签 |
| 调用点 | ① `spawnTerminal` 里 `term.open()` 之后立刻适配一次 + 挂观察器；② `switchTab` 重挂终端时再适配一次（切回来时容器尺寸可能已经不同） |
| `ui/styles.css` | `.terminal-container { min-width: 0; min-height: 0; overflow: hidden }`（尺寸只由 flex 布局决定）；`#terminal-view { overflow: hidden }`（唯一滚动容器是 xterm 自己的 `.xterm-viewport`）。**用 id 限定**：`.terminal-view` 这个类会话面板也在用，别误伤 |

## 3. 判据

### 真机几何探针（`scripts/terminal-layout.js`，ui-smoke U43）

用**真的 xterm** + 真 index.html/styles.css/xterm.css + 真 main.js，在无头 Edge 里走
"宽屏 → 缩小 → 放大"：

```
宽屏：外层 1100px｜容器 620×551px｜屏幕 595×540px｜cols×rows 78×36
窄屏：外层  720px｜容器 240×551px｜屏幕 221×540px｜cols×rows 29×36
  ok  打开后就铺满容器（屏幕与容器差 25×11px —— 边距/回滚条的量级内）
  ok  外层缩小时终端跟着缩：cols 78 → 29，屏幕宽 595 → 221px（容器 620 → 240px）
  ok  窄屏下也铺满容器
  ok  放大回去也跟随：cols 29 → 78
  ok  缩小时 PTY 同步改了：pty_resize 29×36
  ok  放大回来也同步了：pty_resize 78×36（与 xterm 一致）
  ok  终端区没有横向滚动条（唯一滚动容器是 xterm 自己的 .xterm-viewport）
```

`ui-smoke`：**274/274**（新增 U43）、check-style PASS；U42 仍绿（证明 CSS 那笔没误伤会话面板）。

### 探针为什么走 CDP（而不是 `--dump-dom`）

**实测**：`--dump-dom` 模式下只出一次初始帧，之后 **rAF 不跑、ResizeObserver 不再派发**
—— 最小页面里挂上观察器后改宽度能收到 1 次回调，而在真页面里改宽度收到 **0 次**（脚本还会
`await requestAnimationFrame` 挂死）。而"尺寸变了要重新适配"恰恰是这条链路的事，所以必须走
CDP（`Runtime.evaluate` + `awaitPromise`，Node 22+ 内置 fetch/WebSocket，零 npm）。
这条坑记在这里：**下一张几何探针别再用 `--dump-dom` 测事件驱动的路径**。

## 4. 边界

- 探针里 `#app` 的宽度是被脚本改的（模拟窗口缩放）；真机上是窗口/面板拖动 —— 两者都只是
  "让容器变尺寸"，走的是同一条 ResizeObserver 路径。
- 没验的：真机 WebView2 里拖窗口的手感（重启 ruyix 即可肉眼确认）；不同字体下的列数取整
  （`fitDims` 是纯函数，行为可读）。
