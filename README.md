<img src="ui/logo.svg" width="112" alt="RYX — Darkhorse Code" />

# ruyix

## 来源说明

### 原项目

本项目的早期版本由**一位女性程序员**开发并维护，她以笔名「醒过来摸鱼」在 GitCode 上发布
（CSDN 同名账号），后因**工作繁忙**停止了该项目的维护。

- 原项目地址（GitCode）：<https://gitcode.com/m0_66201040/darkhorse-code>

### 迁移说明

经**原作者同意**，本项目已迁移至 GitHub 继续开发与维护：

- 当前仓库（GitHub）：<https://github.com/YujianboIsMe/ruyix>
- 现维护者：俞建波 `<yujianboisme@outlook.com>`
- 迁移保留了完整提交历史，包括 `master` / `0.0.2` / `0.0.3` 分支与 `0.0.1` 标签
- 历史提交中署名为「醒过来摸鱼」的提交均出自原作者；迁移至 GitHub 后的提交由现维护者提交

原项目地址已在上方注明，用于标明项目来源与原作者的署名。

### 其他说明

- 仓库内第三方内容版权归其原作者，遵循各自许可证（如 vendored 的 `ui/xterm.js`、`ui/xterm.css` 为 MIT）
- 感谢原作者的开源贡献
- 如对来源或署名有异议，请提 issue 联系更正

## 架构
Tauri 2 + Monaco Editor + Rust后端
这个方案已经被SideX等开源IDE验证，是目前Rust+WebView路线做IDE的最优解。
核心优势是把编辑器渲染交给久经考验的Monaco，后端用Rust处理所有重逻辑，通信走Tauri原生IPC替代WebSocket。
语法高亮架构：tree-sitter+ arborium
终端架构：xterm.js + Rust PTY

## AI Agent 引擎（harness-engine）
`crates/harness-engine`：从 darkhorse-harness 一次性迁入的 Agent 引擎（零 tauri 依赖的 lib crate），
实现「规划 → 生成 → 规约 → 验证 → 自修复」闭环，含 194 个单测。
工具循环只有四种原子能力（读 / 写 / 执行 / 连接），交付前有**质量门禁**：机械验证（改动后跑语法层、
交付前跑语法+单测+规约，未通过不放行）+ 干净上下文的复核 agent（只读、结构化结论、结论回灌主循环）。
后端桥 `src-tauri/src/agent/`（`agent_*` 命令 + `agent://*` 事件流），
前端面板见导航区「智能体」标签（无后端时自带演示回放）。详见[融合计划](doc/融合计划-Agent集成-v0.2.md)与
[需求：验证与反思](doc/需求-Agent-验证与反思-v0.3.md)。

## 界面
[界面](doc/ui.md)

## 配置系统
[配置](doc/config.md)

## 命令系统
[命令系统](doc/command.md)

## 运行时
运行时分两种状态：
1. 无项目状态
2. 打开项目状态

## 运行
运行和配置系统有关联。
详见[配置](doc/config.md)
暂时先不实现真正的运行，只是保存和显示运行目标。

在【工作区-导航区-运行目标】里先列举所有的运行目标
显示运行目标的名称，如果运行目标有name属性，则显示name，如果没有则显示它的code，如'target0'
具体的点击运行暂不实现

## 终端
终端是运行的关键，也是调试程序的关键。
xterm.js + Rust PTY 实现终端。
导航区-终端暂不实现。

先实现【导航区-运行目标】

运行时，先在编辑器里打开新标签页，标签页的名字为运行目标名称。
然后在该标签页模拟终端执行运行目标的命令。
分两种场景：
- 运行目标没有系统输出，如notepad.exe，这种情况不必在编辑器打开新的标签页
- 运行目标有系统输出，如python.exe，这种情况在编辑器打开新的标签页使用模拟终端运行。