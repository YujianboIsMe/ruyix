# 编辑区

编辑区是多标签页的。
标签页下面是编辑器
编辑器分左右两部分：

* 左边：也叫gutter,用于显示行号
* 右边：语法高亮编辑器

## 渲染：只画可视窗口（虚拟化）

编辑器的代码区是三层叠加（`ui/index.html` / `ui/styles.css`）：

| 层 | 作用 | 定位 |
|---|---|---|
| `#editor-gutter` | 行号 | 在流内，flex 行高拉伸 |
| `#editor-code-backdrop` | 着色文字 | **绝对定位**（`top:0;left:0;width:100%`） |
| `#editor-textarea` | 真身：输入、光标、选区（透明字） | 在流内，**由 JS 给显式高度** |

**为什么必须这样**（每一条都有实测支撑，不是风格偏好）：

1. **backdrop 必须绝对定位。** 它和 textarea 原来同处一个 grid 单元（`grid-area:1/1`）。
   这个结构下 backdrop 的内容一变，layout 就得重新问 textarea 的「内在高度」，
   于是把**全文每一行**重排一遍 —— 25000 行时每次窗口重绘 **110ms**；backdrop 移出流后
   **1.4ms**（16~90 倍）。改回 grid 叠加 = 编辑器重新卡死。门禁 U31 钉住。
2. **textarea 必须拿到显式高度**（`setEditorContent` 里设）。全文高度原本是 backdrop
   撑出来的（grid 行高把两者一起拉伸）；backdrop 移出流后没人撑它，不设就只剩 2 行高，
   点击可视区下半部分点不到 textarea（光标不跟手）。
3. **水平滚动宽度完全由 backdrop 决定。** textarea 对 `scrollWidth` 的贡献**恒为 0**
   （它内部自己换行，实测），所以窗口里必须留一条「全局最宽行」的零高占位行
   （`.editor-virt-keeper`）。不留的话，滚到宽行不在窗口的位置时水平滚动条会缩掉、
   `scrollLeft` 被钳住（实测 5000 行缩 55px、含超长行的文件缩 1370px）。
   最宽行的判定用 `editorVisualCols()`（等宽字体下「列数最大」= 像素最宽，已与
   canvas `measureText` 交叉验证一致）。
4. **textarea 必须 `wrap="off"`。** 它默认软换行，长行会在它内部折成两行，而 backdrop
   按「一行」画 —— 光标与高亮从折行处起整体错开。

窗口：`可视行数 + 上下各 24 行`（`EDITOR_OVERSCAN`）。窗口外用两条零内容 spacer 撑高度，
**占位之和必须恒等于「总行数 × 行高」** —— 这样滚动条长度、位置，以及大纲区
`updateOutline()` 的 `scrollTo(0, (line-1)*20)` 都与虚拟化前逐像素一致。

滚动重绘按 rAF 节流，且**窗口没变就直接 return**（滚动事件会连发几十次，重画是白做）。

实测（无头 Edge + 真实 `styles.css`/`main.js`，视口 870×573）：

| 行数 | 渲染的 code-line | 节点数（纯文本） | 滚动重绘 | 打字重画 |
|---|---|---|---|---|
| 5,000 | 78 | 161（原 18,831） | 2.4ms（原 ~130ms） | 3.4ms（原 137ms） |
| 10,000 | 78 | 161 | 3.8ms | 6.3ms |
| 25,000 | 78 | 161 | 9.1ms | 13.0ms |

> 改动位置：`ui/main.js` 的 `setEditorContent` / `paintEditorWindow` /
> `setupEditorVirtualScroll`（三个渲染入口 `renderHighlightedCode`、`renderPlainCode`、
> `renderTerminalOutput` **都必须**走 `setEditorContent`）。
> 门禁 `node scripts/ui-smoke.js` 的 U31 覆盖行为与样式两侧，并已反向验证会转红。

## 语法高亮
语法高亮架构选型：tree-sitter+ arborium
语法高亮支持：

1. python
2. rust
3. html/css/javascript. 请注意：大部分html文件里会嵌入js/css代码。
4. markdown,只高亮、不预览
5. sql文件
6. java文件（`.java`）

> 新增语言的落地位置：`src-tauri/Cargo.toml` 的 arborium `lang-*` feature、
> `ui/main.js` 的 `extToLanguage()`（扩展名 → arborium 语言名）与 `fileIcon()`（标签页/文件树图标）。
> 三者缺一，表现为"打开文件没有高亮"。

## 标签页图标
参考[icon](./icon.md)

## 图片预览
若打开的是图片，默认以1:1进行图片预览