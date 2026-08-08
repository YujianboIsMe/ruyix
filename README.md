# darkhorse-code

Tauri 2 + Monaco Editor + Rust后端
这个方案已经被SideX等开源IDE验证，是目前Rust+WebView路线做IDE的最优解，核心优势是把编辑器渲染交给久经考验的Monaco，后端用Rust处理所有重逻辑，通信走Tauri原生IPC替代WebSocket。
架构设计
前端层：Monaco Editor作为编辑器内核，TypeScript写IDE外壳（侧边栏、终端、状态栏、文件树），不需要编译为WASM，直接跑在系统原生WebView里，享受Monaco亿级用户验证的渲染性能，支持百万行文件流畅编辑、虚拟滚动、增量语法解析。
后端层：Tauri 2的Rust后端直接管理pylsp进程，通过std::process::Command启动pylsp，用tokio异步处理LSP的JSON-RPC消息，不需要额外起WebSocket服务。
通信层：用Tauri的原生IPC替代WebSocket，同步调用（比如打开文件、保存文件）用invoke()，异步推送（比如diagnostics、completions结果）用Tauri的事件系统emit()/listen()，消息直接走进程内通信，不需要走网络协议栈，序列化开销比WebSocket低一个量级。