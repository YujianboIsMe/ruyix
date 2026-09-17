# BUG-001：运行目标绑定了清单文件，命令却在项目根目录执行

- 归属版本：0.0.4
- 状态：已修复（待验收）
- 严重程度：高 —— 「绑定」功能实际不可用：绑定了子目录工程的目标一律跑不起来

## 现象

运行目标绑定了子目录里的清单文件，点击运行时报错：**命令在项目根目录下执行**，
而真正的工程文件在子目录里，于是找不到清单文件（如 npm 读不到 `<项目根>\package.json`，
因为工程实际在 `<项目根>\admin-web\`）。

实测输出（在项目根执行 `npm start`，工程只有 `admin-web\package.json`）：

```
npm error code ENOENT
npm error syscall open
npm error path C:\...\dh-run-demo\package.json
npm error errno -4058
npm error enoent Could not read package.json: Error: ENOENT: no such file or directory, open 'C:\...\dh-run-demo\package.json'
```

退出码 `38`。同一命令在 `admin-web\` 下执行则正常（`exit=0`）。

## 复现方法

1. 项目结构（子目录里才是真正的工程）：

```
<项目根>\
├── admin-web\
│   └── package.json
└── .darkhorse\code\run.toml
```

2. `<项目根>\.darkhorse\code\run.toml`：

```toml
[run]
"target0.bind" = 'admin-web\package.json'
"target0.cmd" = "npm start"
"target0.name" = "package.json"
```

3. 在 IDE 里打开该项目 → 导航区【运行目标】→ 点击 `package.json` 运行

- **期望**：在 `<项目根>\admin-web` 下执行 `npm start`
- **实际**：在 `<项目根>` 下执行 → npm 找不到清单文件，命令失败（退出码 38）

> 复现步骤已按上面结构实测：目录里只有 `admin-web\package.json`（`scripts.start` 打印 `process.cwd()`）时，
> 在项目根执行 `npm start` 报 `ENOENT ... \<项目根>\package.json`；换到 `admin-web\` 下执行，
> 输出 `CWD=<项目根>\admin-web` 且退出码 0 —— 与修复后的运行目录一致。

## 原因

后端 `run_target` 只接收 `cmd` 与 `project_root`，工作目录**无条件**设为项目根：

```rust
// 修复前：src-tauri/src/main.rs
if let Some(ref dir) = project_root {
    cmd.current_dir(dir);      // ← 永远是项目根
}
```

运行目标其实早就有 `bind` 字段（绑定的清单文件，项目相对路径）：前端列表用它展示 🔗、
`get_execute_status` 用它判断"文件是否已绑定运行目标"。但**执行时没有把 bind 传给后端**：

```js
// 修复前：ui/main.js
async function runTargetCmd(name, cmd) { ... }
await invoke("run_target", { cmd, projectRoot });   // ← bind 没进去
```

即：`bind` 只参与了"识别是哪个运行目标"，从未参与"在哪个目录运行"。

## 修改方法

### 1. 后端：新增工作目录解析 `resolve_run_dir()`

`src-tauri/src/main.rs`：

| 情况 | 工作目录 |
|---|---|
| 未绑定文件（`bind` 缺失或空白） | 项目根 |
| 绑定文件 `admin-web\package.json` | `<项目根>\admin-web`（该文件所在目录） |
| 绑定的是目录 `admin-web` | `<项目根>\admin-web` |
| 目录不存在 / 解析后越出项目根（`../..`） | 回退项目根 |

- 用 `canonicalize()` 解析真实路径，并校验结果必须落在项目根内（防 bind 路径逃逸）
- `run_target` 新增可选参数 `bind: Option<String>`；工作目录在进入执行闭包前算好

### 2. 前端：把 `bind` 传到执行层

`ui/main.js`：

- `runTargetCmd(name, cmd, bind)` 新增第三参 → `invoke("run_target", { cmd, projectRoot, bind })`
- 导航区运行目标列表：条目增加 `data-bind`，点击时读取并传入
- 右键【运行】命中已有目标的分支：传 `target.bind`
- 顺带修正运行结果标签页的去重键：**命令 + bind 都相同**才算同一次运行
  （同一个 `npm start` 绑到不同子目录，本来就是两次不同的运行）

## 验证

1. **单测**（`cargo test`，本轮新增 7 个用例，全部通过）：
   - `run_dir_uses_bind_file_parent_dir` —— 本 BUG 的复现用例，覆盖 `\` 与 `/` 两种 bind 写法
   - `run_dir_defaults_to_project_root`、`run_dir_bind_at_project_root_stays_at_root`
   - `run_dir_bind_dir_uses_dir_itself`、`run_dir_missing_dir_falls_back_to_root`
   - `run_dir_blocks_path_escape` —— 越出项目根时回退，不逃逸
2. **端到端**：`run_target_executes_in_bind_dir` 走真实 `run_target` 执行 `cmd /c cd`，
   断言回显的工作目录落在 `admin-web`（证明修好的是真实执行链路，不只是路径计算）
3. **前端行为**：抽出真实 `runTargetCmd` 源码 + 假 invoke，断言
   ① bind 确实传给后端 ② 相同 命令+bind 不重复执行 ③ 不同 bind 各自执行 ④ 未绑定目标不传 bind

## 影响范围与兼容性

- 只影响运行目标的一次性执行路径（`run_target`）
- 终端资源（`spawn_terminal`）与 xterm 终端（`pty_spawn`）与运行目标无关，未改动
- `bind` 为可选参数：**旧配置（无 bind）行为不变**，仍在项目根运行
- 绑定目录不存在（如绑定文件被删）时回退项目根，命令自身的报错照常回显，不会崩
