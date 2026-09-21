# 帮助

在底部**命令栏**输入命令，回车执行；命令不识别时会交给 AI 翻译成标准命令。

## 命令系统

| 命令 | 说明 |
| --- | --- |
| `open project <路径>` | 打开 / 切换项目文件夹 |
| `open file <路径>` | 打开文件 |
| `close project` | 关闭当前项目 |
| `close all` | 关闭所有标签页 |
| `close <序号>` | 按序号关闭标签页（负数从右数） |
| `close other` | 关闭除当前外的标签页 |
| `close left` | 关闭当前标签页左侧所有 |
| `close right` | 关闭当前标签页右侧所有 |
| `new py\|rs\|md\|c <名称>` | 创建文件（自动加后缀） |
| `new file <相对路径>` | 创建文件 |
| `new folder\|dir <相对路径>` | 创建目录 |
| `rename\|mv <旧路径> <新名称>` | 重命名文件/目录 |
| `del\|delete\|remove\|rm <相对路径>` | 删除文件/目录（弹窗确认） |
| `refresh [路径]` | 从磁盘刷新文件树/文件夹 |
| `run <名称>=<命令>` | 快捷添加项目运行目标 |
| `config add\|get\|update\|remove [-g\|-p\|-r] <key>[=<value>]` | 配置管理（默认 `-r` 运行时） |
| `agent [消息]` | 新建 Agent 会话；带消息则直接发出 |
| `service` | 打开服务面板（agent 起的常驻进程） |
| `service log <pid>` | 实时查看某个服务的终端输出 |
| `git <参数...>` | 执行原生 git 命令（如 `git status`） |
| `project lang <语言> <项目路径>` | 设置项目语言 |
| `project edit "<路径>" "<名称>" <语言>` | 修改项目名称与图标 |
| `project delete <项目路径>` | 从项目列表删除（不删文件夹） |
| `project migrate` | 迁移旧版项目配置 |
| `help` | 打开本帮助 |

## 快捷键

| 快捷键 | 说明 |
| --- | --- |
| `Ctrl+S` | 保存当前文件 |
| `Ctrl+Enter` | Agent 会话输入框发送消息 |
| `Tab` | 编辑器内插入缩进 |

编辑器内容停止输入 1 秒后自动保存，切换标签页或失焦时立即保存。

## 语法高亮支持

| 语言 | 扩展名 |
| --- | --- |
| Python | `.py` |
| Rust | `.rs` |
| HTML（含嵌入 CSS/JS） | `.html`, `.htm` |
| CSS | `.css` |
| JavaScript | `.js`, `.mjs`, `.cjs` |
| Markdown | `.md`, `.markdown` |
| SQL | `.sql` |
| Java | `.java` |

Markdown 文件在**大纲区**会解析标题层级，便于跳转。

## 联系方式

关注 <https://newest-ai.com>，邮箱 yujianboisme@outlook.com。
