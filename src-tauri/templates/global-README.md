# global/ —— 全局配置与全局数据

这里是 **ruyix 程序自己所在文件夹**（便携根）的一部分。卸载 = 删掉整个文件夹，不留残留。

| 内容 | 说明 |
|---|---|
| `*.toml` | 全局配置（`ai.toml` / `ui.toml` / `mcp.toml` / `a2a.toml` / `tools.toml` / `skills.toml` …） |
| `projects.toml` | 已打开过的项目清单（每条含项目路径与它的目录 key） |
| `runs/` | agent 沙箱运行产物（每次运行一个子目录） |
| `logs/debug.log` | 只在 `--debug` 启动时产生 |
| `cache/` | 可随时删的缓存（命令探测结果等） |
| `webview/` | WebView2 用户数据目录（浏览器缓存，删了会重建） |

- **备份**：把整个 ruyix 文件夹拷走即可（配置 + 项目状态 + 缓存一起）。
- **搬家**：先删 `cache/` 与 `webview/` 再拷，体积小很多。
- ruyix **不会**往你打开的项目里写任何东西。
