# 配置

配置系统分为三级
- 全局配置
- 项目配置
- 环境配置
- 运行配置

配置有固定前缀ruyix.code
在toml文件里可以省略ruyix.code前缀
toml文件的文件名也是前缀的一部分
但是为了配置统一，读取toml文件加载到运行时配置结构体后要补上ruyix.code前缀

层级举例
┌─────────────────────────────────┬──────────┬──────────────┐
│               key               │ section  │   key        │
├─────────────────────────────────┼──────────┼──────────────┤
│ ruyix.code.projects.current │ projects │ current      │
├─────────────────────────────────┼──────────┼──────────────┤
│ ruyix.code.projects.list    │ projects │ list         │
├─────────────────────────────────┼──────────┼──────────────┤
│ ruyix.code.run.target0.cmd  │ run      │ target0.cmd  │
├─────────────────────────────────┼──────────┼──────────────┤
│ ruyix.code.run.target0.name │ run      │ target0.name │
└─────────────────────────────────┴──────────┴──────────────┘

## 全局配置
位于~/.ruyix/code
格式为toml文件

## 项目配置
位于./.ruyix/code
格式为toml文件

## 环境配置
启动ruyix.exe时带的命令行参数
待定
## 运行配置
使用命令系统修改的配置。


## 配置项
### 项目

为了避免每次都打开项目。
所以在~/.ruyix/code/projects.toml里保存项目
项目配置项举例（标准运行时）
```
[projects]
current = 'C:\Users\yujia\PycharmProjects\tmf'

[[projects.list]]
name = 'rag'
path = 'C:\Users\yujia\PycharmProjects\rag'
lang = 'python'

[[projects.list]]
name = 'tmf'
path = 'C:\Users\yujia\PycharmProjects\tmf'
lang = 'unknown'
```

项目属性（0.0.2，未来扩展）：
1. name 项目名称
2. path 项目路径
3. lang 项目语言，默认为 unknown（未知语言）

语言可选值（10种）：
| key | 图标 | 说明 |
|:---:|:---:|------|
| unknown | Ⓤ | 未知语言 |
| mix | Ⓜ | 混合语言 |
| java | Ⓙ | Java语言 |
| c | Ⓒ | C/C++/C#语言 |
| python | Ⓟ | Python语言 |
| rust | Ⓡ | Rust语言 |
| web | Ⓦ | Html/JavaScript/CSS语言 |
| golang | Ⓖ | Golang语言 |
| document | Ⓓ | Document或笔记类：pdf/word/markdown |
| kotlin | Ⓚ | Kotlin语言 |

旧版配置（0.0.1）只有纯路径列表：
```
[projects]
current = 'C:\Users\yujia\PycharmProjects\tmf'
list = ['C:\Users\yujia\PycharmProjects\rag', 'C:\Users\yujia\PycharmProjects\tmf']
```
读取时兼容旧版（自动补默认 name/lang），通过菜单【项目→迁移配置】或命令 `project migrate` 显式迁移为新格式。

### 运行
运行目标通过配置来持久化
运行目标不能保存为全局，如果遇到保存全局运行命令，则报错【运行目标不能保存为全局】！
运行目标需要以配置的形式持久化。
./.ruyix/code/run.toml里保存运行目标
配置项举例（标准运行时）
```
ruyix.code.run.target0.cmd="C:\Users\yujia\PycharmProjects\rag\.venv\Scripts\python.exe C:\Users\yujia\PycharmProjects\rag\src\P05-es\pdf2es.py"
ruyix.code.run.target0.name="pdf转储es"
ruyix.code.run.target0.bind=src\P05-es\pdf2es.py
```
target0 是用户随意取的名字，不是系统递增的（IDE不维护计数器）。
只有IDE自动创建运行目标时，才遍历target开头的code/key，然后取最大的数字+1。

**bind 与运行目录**：`bind` 是绑定的清单文件（项目相对路径），决定命令在哪个目录执行：

| bind | 运行目录 |
|---|---|
| 无 | 项目根 |
| `admin-web\package.json` | `<项目根>\admin-web` |
| `admin-web`（目录） | `<项目根>\admin-web` |
| 目录不存在 / 越出项目根 | 回退项目根 |

`npm start`、`cargo run` 这类必须在清单文件所在目录执行的命令依赖此规则
（缺陷记录：[bug-运行目录错误](./bug-运行目录错误-v0.0.4.md)）。

**命令来源**：`package.json` 的运行目标由 `scripts` 逐条生成（每个脚本一个目标），
命令形如 `npm run dev`，不再写死 `npm start`；详见 [运行机制](./execute.md)。

### AI
AI配置同样遵循配置四级配置机制。

配置项
ruyix.code.ai.api_key
ruyix.code.ai.api_url
ruyix.code.ai.model
ruyix.code.ai.alias 如果没有指定，则取ruyix.code.ai.model

### RAG（智搜）
RAG 配置不走四层配置系统，独立存放于两个 rag.toml：

全局配置 `~/.ruyix/code/rag.toml`（扁平键，无 ruyix.code 前缀）：
```
enabled = true
permanently_disabled = false
api_url = "https://api.deepseek.com/v1/embeddings"
api_key = "sk-xxxx"            # 可选，缺省复用 ruyix.code.ai.api_key
model = "deepseek-embedding"   # 可选，嵌入模型名
dim = 1024                     # 可选，向量维度，默认 1024
```

项目配置 `./.ruyix/code/rag.toml`（索引状态，由系统自动维护）：
```
enabled = true
last_indexed = "1786526560"
files_count = 312

[files]
"src/main/Calc.java" = "a1b2c3d4"
```

修改 api_url / dim 的入口：菜单栏【智搜】→ 确认弹窗 → 接受。
修改 dim 后旧向量库自动清空，需要全量重建索引。

## 配置菜单（表单）

顶栏「配置」菜单提供三个入口，点击后在中央编辑区打开一个**配置表单标签**：

| 子菜单 | 作用域 | 存放位置 |
|---|---|---|
| 全局 | global | `~/.ruyix/code/*.toml` |
| 项目 | project | `<项目>/.ruyix/code/*.toml`（需先打开项目） |
| 运行 | runtime | 内存，不落盘 |

表单内容 = **本作用域 + 回退链**（runtime → project → global）上所有平铺配置项，
按 section 分组渲染：

- **已知项**给出中文标签、说明与控件类型（文本框 / 密码框 / 下拉 / 开关 / 数字），
  即使当前作用域没配过也会列出来，方便直接填。
- **扫描到的未知键**也会渲染成一行（打「扫描」标记），类型按值的形状猜。
- 本作用域没配、值来自回退链的项显示 `未设置 · 当前继承自 <作用域>: <值>`；
  命中密钥类字段（含 `key/secret/token/password`）时值打码为 `••••••`。

### 三个按钮

| 按钮 | 命令 | 行为 |
|---|---|---|
| 保存 | `config save [scope]` | 只把**改动过的行**写回本作用域；清空某一项再保存 = 删除该键 |
| 应用 | `config apply [scope]` | 保存 + 把非空值刷进 IDE **运行时内存**里的配置对象（查找优先级最高，重启失效） |
| 取消 | `config cancel [scope]` | 丢弃未保存的改动并重新扫描（有改动时先弹确认） |

三者都走命令系统，等价于在命令栏输入 `config save|apply|cancel [global|project|runtime]`；
打开表单本身的命令是 `config form [scope]`（省略 scope = global）。

保存是**增量语义**：只动提交上来的键，同一个文件里没提到的键原样保留；
嵌套表 / 数组等非标量内容也不会被压平吃掉。

`ui.lang` / `ui.emoji` 保存或应用后会立即热重载界面语言与 emoji 叠加样式。

### 结构化文件

`projects.toml` / `execute.toml` / `mcp.toml` / `a2a.toml` / `tools.toml` / `skills.toml`
由各自的功能面板管理，平铺写回会破坏它们的格式：

- 扫描时**排除**，表单里看不到；
- 保存时若出现同名 `section`，直接**报错拒绝**。

### 后端命令

`config_form_load(scope, project_root?)` → `ScopeEntriesDump`
`config_form_save(scope, entries, project_root?)` → `ScopeSaveReport`
`config_form_apply(scope, entries, project_root?)` → `ScopeSaveReport`

实现见 `src-tauri/src/config.rs`，前端见 `ui/config.js`。
