# darkhorse-code

## 架构
Tauri 2 + Monaco Editor + Rust后端
这个方案已经被SideX等开源IDE验证，是目前Rust+WebView路线做IDE的最优解。
核心优势是把编辑器渲染交给久经考验的Monaco，后端用Rust处理所有重逻辑，通信走Tauri原生IPC替代WebSocket。
语法高亮架构：tree-sitter+ arborium
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
暂时先不实现真正的运行，只是保存运行。
