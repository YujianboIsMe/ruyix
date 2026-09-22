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

### 载荷形状：`{ tags, lines }`（不回传正文）

`highlight_code` 回的是 `HighlightPayload`，不是逐行的对象数组：

```json
{ "tags": ["keyword", "function"],
  "lines": [[0, 2, 0, 3, 6, 1], []] }
```

`lines[i]` 是第 i 行的**扁平三元组** `[start, end, tagIdx, ...]`（空行是 `[]`），
`tagIdx` 是 `tags` 里的下标。`start`/`end` 是**行内 UTF-16 码元**偏移 —— 前端 `slice()` 的单位，
不用再换算（见下面「偏移单位」一节）。三条设计理由，改动前请先读完：

1. **不回传正文**。前端手里就有（`tab.content`），逐行回传等于把文件复制一份再走一遍 JSON。
   更要紧的是**不能回传**：后端用 `code.lines()` 收行（**吃掉末尾空行**），前端
   `split("\n")` 不吃 —— 拿后端的行去拼 textarea 的值，就会让"以换行结尾的文件"少一个末尾
   `\n`，用户一按键 `tab.content = textarea.value` 把差值固化，写回时文件末尾的换行就真没了。
   所以**行由前端自己切**（`renderHighlightedCode` 里的 `text.split("\n")`），后端只回答
   "第 i 行的片段在哪"。前端行数可能比后端多一行，多出来的按纯文本画。
2. **tag 走名表 + 下标**，不是每个 span 带一个字符串。整份响应里名表只出现一次（≤27 项）。
   也没用裸数字编码：那要求两边的枚举顺序永远一致，一旦漂移就是**全篇错色**；名表设计下
   顺序漂移最多是"查到另一个名字"。
3. **扁平三元组**，不是每 span 一个对象。省掉两层 key 名重复。
   实测 400 行样例：**36,374 字节 vs 旧形状 307,644 字节（8.5 倍）**。

后端 tag 是枚举 `Tok`（`main.rs` 的 `TOK_TABLE` 是名字↔枚举的唯一来源）。取值集合是**闭的**：
`arborium_theme::tag_to_name` 只有 27 个出口，上游 `.and_then()` 已经滤掉不认识的捕获名 ——
也就是说这条链路本来最多也只能产出这 27 个名字。枚举化之后打错名字是编译错误，
而不是"这个 span 悄悄没颜色"。

### 四道契约门禁（都在 `cargo test -p ruyix`）

| 测试 | 钉什么 | 反向验证 |
|---|---|---|
| `highlight_payload_does_not_echo_the_source_text` | 载荷里不许出现正文，也不许退回 `start_col`/`line_number` 的对象编码 | 加回 `text` 字段即红 |
| `highlight_payload_is_much_smaller_than_the_legacy_shape` | 新载荷 ≥ 旧形状的 1/3（比值判据，机器不影响结论） | 阈值抬到 100× 即红，并打印实测字节数 |
| `every_tag_has_a_css_class` | `TOK_TABLE` 的每个名字在 `styles.css` 里都有 `.tok-<name>` **规则** | 删一条规则即红（会点名 `["macro"]`） |
| `span_offsets_are_utf16_units` | 片段偏移是 UTF-16 码元：按 JS 语义切回来必须落在同 tag 的原始捕获内 | 生产路径退回"报字节"即红，并点出切歪的文本 |

> `every_tag_has_a_css_class` 的实现细节值得说一句：它**先剥掉 CSS 注释**、再要求类名后面跟
> `{`。第一版没做这两件事，反向验证时把规则换成一句"含 `.tok-macro` 的注释"，门禁照过 ——
> **绿而不会红的门禁是摆设**，这就是反向验证要抓的东西。

前端侧由 ui-smoke `U31` 补三条行为检查：高亮载荷能切成 `tok-*`；含中文的载荷按 UTF-16
码元切出来必须正好是 `"中文"` / `// 注释`（对齐后端的单位换算）；
以及**"末尾换行"**——喂一份 `content` 以 `\n` 结尾、后端只回 1 行的载荷，
断言 `textarea.value === tab.content`（这条在旧实现上是红的）。

### 偏移单位：**UTF-16 码元**（与 `text.slice()` 同一单位）

`lines` 里的 `start`/`end` 是**行内 UTF-16 码元**偏移，不是字节。前端 `editorLineHtml`
直接 `text.slice(start, end)` —— JS 的字符串下标就是 UTF-16 码元，所以两边天然对齐。

换算在后端完成（`main.rs` 的 `unit_table` / `utf16_offset`）：tree-sitter 报字节区间，
中文一个字 3 字节 / 1 码元、emoji 4 字节 / 2 码元，**只要行里出现过非 ASCII，从那里往后就会
整体错位**。修前的实测（`let s = "中文"; // 注释`）：

| 后端报的 | 前端切出来的 | 应该是什么 |
|---|---|---|
| `8..16`（字节）→ | `"中文"; //` ❌ | `"中文"` |
| `18..27`（字节）→ | `释` ❌ | `// 注释` |

表现是"中文注释/字符串的着色起点前移，顺带把旁边的字染上"。现在报 `8..12` / `14..19`。

两条细节值得记：

- **去重仍在字节上做**，只在最终留下的那对切点换算。换算在字符边界上是单调且单射的，
  所以"先换算再裁剪"与"先裁剪再换算"结果相同，而前者要为每个片段重走整行。
- **换算按行建一次表**（`unit_table`，纯 ASCII 行连表都不建）。若放在片段循环里逐片段
  现算，单行超长的压缩文件会退化成 O(片段数 × 行长)。

门禁：`span_offsets_are_utf16_units`（`cargo test -p ruyix`）把载荷的偏移**按 JS 语义**
切回来，要求结果落在**同一个 tag 的原始捕获**内；判据用包含性而不是逐字相等 —— 去重裁过的
片段一定还是原捕获的子集，而单位写错时会**越出**捕获（修前 `"中文"` 报字节 `8..16`，
按码元切出来是 `"中文"; //`，把 `;` 和注释开头都吞进了 string 捕获）。测试里带**正控**：
若语料里没有一个片段能区分"按字节"与"按码元"，直接判红（防止有人把中文语料删掉后门禁变成摆设）。
前端侧由 ui-smoke `U31` 钉住端到端：喂一份含中文的载荷，DOM 里必须正好是
`<span class="tok-string">"中文"</span>`、`<span class="tok-comment">// 注释</span>`。

> 反向验证：生产路径退回"报字节" → Rust 门禁红，报
> `第 0 行片段 8..16（tag=string）切出来是 "\"中文\"; //"，越出了同 tag 的原始捕获`；
> ui-smoke 载荷改成字节值 → 红，并把实际渲染打出来
> （`<span class="tok-string">"中文"; //</span> 注<span class="tok-comment">释</span>`）。

## 标签页图标
参考[icon](./icon.md)

## 图片预览
若打开的是图片，默认以1:1进行图片预览