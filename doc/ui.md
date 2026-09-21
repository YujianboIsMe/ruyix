# 界面设计
## 布局
- 眉部：菜单区在Windows的窗口顶部以节省空间，左边是菜单，右边是Windows的 - 口 X 三个按钮。
- 工作区：布局待定，如果没有打开项目则显示欢迎页。
- 命令区：注意，不是终端！是一个单行命令输入框而已。命令待设计。
- 状态栏：待设计

## 欢迎页
上下两部分：
- 上面九宫格
- 下面是欢迎文本
### 九宫宣传格子

| 多语种支持 | 任意编程语言 | 丰富终端类型 |
---------------------------------------
| 文本命令架构| 自我成长  | AI安全可控|
--------------------------------------
|Rust 轻量化！极速！ | Rust内存安全 | Rust线程安全|
九宫格用AI润色下。

### 欢迎文本：
> ruyix，越来越懂你

## 菜单
待定

## 工作区
工作区分左中右三块：
- 左侧：[导航区](navigator.md)
- 中间：[编辑区](editor.md)
- 右侧：大纲区

### 导航区
[导航区](navigator.md)

### 编辑区
详见[编辑区](editor.md)

### 大纲区

![大纲区](outline_area.md)

## 命令区
是一个单行命令输入框而已，没有按钮，回车键发送命令。
命令系统待设计。

## 状态栏
随工作区变化，待设计。

## 帮助页
分运行时状态
- 开启项目状态：点击顶部菜单帮助，在编辑器区打开帮助页（占一个标签页）。
- 未开启项目状态：替换欢迎页为帮助页

帮助页正文的**源文件是 markdown**，按语言分成两份，打开时用 markdown-it 渲染进 `#help-body`：

|语言|源文件|
|:----:|:----:|
|中文|`ui/help-zh.md`|
|English|`ui/help-en.md`|

正文不再写在 `index.html` 里（原手写表格）、也不再由 `command.js` 拼纯文本
（原 `getHelpText()` 已删）——要改文案就改 md 文件。渲染器沿用会话气泡那个
vendor 的 `ui/markdown-it.min.js`（`html: false`，不执行内嵌 HTML）。

帮助页内容：
1. 命令系统
2. 快捷键
3. 语法高亮支持
4. 联系方式：关注 https://newest-ai.com ，邮箱 yujianboisme@outlook.com

## 服务页

顶部菜单【服务/Service】→ 中央编辑区打开一个标签页，列出 agent 用后台模式
（`execute` 的 `background`）起的常驻服务。

- 治的病：那些进程是宿主 spawn 的，却不在宿主的进程树里（cmd → mvn.cmd → java），
  以前只活在引擎的进程表里 —— 起得来、看不见、也停不掉。
- 面板读的是引擎**同一份**表（后端 `proc_list` → `harness_engine::proc::listing`），
  不另扫端口、不另猜进程，面板和模型看同一份事实。
- 进程表的主键是 **pid**（`HashMap<u32, Managed>`）：面板 / 用户 / netstat / tasklist
  手里的一手证据只有 pid，所以"停止"直接按 pid 发（`proc_stop`，连子进程树一起收）。
- 表格是**学术三线表**：只有顶线、表头下线、底线三条横线，没有竖线、没有内部行线。
  字段三项：PID / 启动时间 / 完整命令行。启动时间形如
  `2026-09-20 19:07:00 (2h23m36s)` —— 时刻与已启动时长都要给全。
- 已退出的进程不进面板（没有可管理的对象）。面板打开时每秒刷新一次时长。

## 外链（WebView 只是画布，不是浏览器）

点一下链接就把整个 IDE 换成那张网页，是这套架构**最致命**的一类 bug：前端全活在一个文档里
（标签页、文件树、会话都只是 DOM 状态），文档一换，状态一起没，而且回不去。

病根不是链接本身，是**没人拦导航**：agent 输出走 markdown-it 且开了 `linkify`，裸 URL 会变成真
`<a href>`；而 WebView2 对普通导航的默认动作就是**在本 WebView 里导航过去**。agent 一句
"服务已起，访问 `http://localhost:8080`" 就能触发。

所以规矩只有一条：**外链一律交给操作系统浏览器，WebView 只准待在自家文档里。**两层闸门：

| 层 | 位置 | 职责 |
|:--:|------|------|
| 兜底 | `src-tauri/src/main.rs::nav_verdict` + 窗口的 `on_navigation` | 任何来源的导航都要过它（`a` 标签 / `location.href` / form / `window.open` / 以后某个忘了拦的角落） |
| 显式 | `ui/external.js`（捕获阶段点 `click` / `auxclick`） | 在按下那一刻就 `preventDefault`，把"打开链接"变成"交给系统浏览器"，并给拦下的链接一句说明 |

两个后果值得记住：

- **主窗口必须建在 Rust 里**（`build_main_window`），`tauri.conf.json` 的 `app.windows` 因此留空 ——
  只有 Builder 挂得上 `on_navigation`，配置里生出来的窗口没有闸门。ui-smoke U24 钉住这一点。
- **判定是"是不是自家文档"**，不是"是不是 localhost"：`localhost` 恰恰是 agent 起的服务所在，
  放行它就等于没拦。同源但非文档的路径（`tauri.localhost/main.rs` 这类）也拒 —— 导航过去只是白页。

出口 `open_external` 只放行 `http` / `https` / `mailto`，Windows 走 `ShellExecuteW`（系统默认处理程序），
不走 shell。`file:` / `javascript:` / `data:` 一律拦在"能执行之前"。

可复现证明（真窗口实测，六条路子）：`node scripts/nav-guard-probe.mjs`，用法见脚本头注释。

## 多语言

|语言|文件|
|:----:|:----:|
|中文|zh-CN.json|
|English|en.json|


顶部菜单 加语言
下来三个选项
- 中文
- 英文

中文和英文互斥
详见[菜单](menu.md)

## 弹窗
chromium自带的弹窗和现有UI风格不符合。
所以我们要自己设计一个弹窗，与现有UI风格一致。

## 窗口
当开启多个ruyix进程时，在Windows系统的窗口预览（Alt+Tab）里，这两个窗口的标题必须显示为项目名，若未打开项目则显示Darkhorse Code。