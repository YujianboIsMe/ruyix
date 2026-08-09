# darkhorse-code

## 架构
Tauri 2 + Monaco Editor + Rust后端
这个方案已经被SideX等开源IDE验证，是目前Rust+WebView路线做IDE的最优解。
核心优势是把编辑器渲染交给久经考验的Monaco，后端用Rust处理所有重逻辑，通信走Tauri原生IPC替代WebSocket。
语法高亮架构：tree-sitter+ arborium
终端架构：xterm.js + Rust PTY

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