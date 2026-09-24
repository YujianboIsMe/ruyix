# projects/ —— 每个项目一个桶

打开过的项目在这里各有一个文件夹，名字是项目路径的**可读 slug**
（`D:\Projects\Rust\ruyix` → `D-Projects-Rust-ruyix`）。

桶里是**这个项目的 IDE 状态**（它不属于你的仓库，所以不放在你的仓库里）：

| 子目录 | 说明 |
|---|---|
| `*.toml` | 该项目作用域的配置 |
| `stage/` | 确认模式下 agent 的暂存改动（你确认后才写回项目） |
| `backups/` | 写回/覆盖前自动备份的原文件 |
| `proc/` | agent 起的常驻服务日志 |
| `env/installs.jsonl` | 环境安装留痕 |
| `sessions/` | 会话存档 |
| `verify/` | 验证产物（如 cargo 的 target 目录） |
| `project.toml` | 这个桶属于哪个项目（自证，便于识别孤儿） |

项目改名或移动后会生成**新的桶**，旧桶成为孤儿 —— 可以在 ruyix 里查看并删除（绝不会自动删）。
