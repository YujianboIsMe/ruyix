# BUG-002：package.json 的运行命令被写死成 npm start，不读 scripts

- 归属版本：0.0.4
- 状态：已修复（待验收）
- 严重程度：高 —— 只要项目里没有 `start` 脚本，自动创建出来的运行目标就是废的

## 现象

右键 `package.json` → 【运行】，生成的运行目标命令固定是 `npm start`，
而 `package.json` 里实际写的是 `dev` / `build` / `test`：

```
▶ package.json    npm start    🔗 admin-web\package.json
```

点击运行直接失败（实测，package.json 只有 `dev` 脚本时）：

```
npm error Missing script: "start"
npm error
npm error Did you mean one of these?
npm error   npm star # Mark your favorite packages
npm error   npm stars # View packages marked as favorites
```

退出码 `1`。用户预期的是 `npm run dev`。

## 复现方法

1. `<项目根>\admin-web\package.json`：

```json
{
  "name": "admin-web",
  "scripts": {
    "dev": "vite",
    "build": "vite build",
    "test": "vitest"
  }
}
```

2. 在 IDE 里右键该文件 → 菜单出现【运行】 → 点击
3. **期望**：按 `scripts` 生成运行目标（`npm run dev` / `npm run build` / `npm run test`）
4. **实际**：只有一个 `npm start`，点了报 `Missing script: "start"`

> 补充实测：只有 `dev` 脚本时，在项目目录执行 `npm start` → 退出码 1、`Missing script: "start"`；
> 执行 `npm run dev` → 退出码 0。

## 原因（两处，任一处都足以导致猜错）

### A. 预置清单表把命令写死了，根本没读文件，也没问 LLM

`src-tauri/src/config.rs:25-29`：

```rust
pub const MANIFEST_FILES: &[ManifestDef] = &[
    ManifestDef { name: "cargo.toml",   cmd: "cargo run" },
    ManifestDef { name: "package.json", cmd: "npm start" },   // ← 写死
    ManifestDef { name: "makefile",     cmd: "make" },
];
```

`get_execute_status`（`main.rs`）里预置清单**优先级最高**，命中就直接返回该常量：

```rust
if let Some(manifest) = config::manifest_for_path(p) {
    known = Some(true);
    suggested_cmd = Some(manifest.cmd.to_string());   // ← 永远 npm start
}
```

前端 `main.js` 的兜底 `cmdMap` 里还抄了一份 `"package.json": "npm start"`。
所以无论 package.json 内容是什么，命令都不会变，**也不可能走到 LLM 分支**。

### B. AI 路径的提示词里没有文件内容

即使走 AI 分支（未知类型文件），`ai.rs::check_executable` 的提示词也只有元信息：

```rust
// 修复前
let prompt = format!(
    "文件路径: {}\n文件名: {}\n扩展名: .{}\n\n这个文件可以运行/执行吗？...",
    path, file_name, ext          // ← 没有文件内容
);
```

LLM 看不到 `scripts`，只能凭"package.json"这个文件名猜，猜出 `npm start` 是必然的。

## 修改方法

### A. 新增 `runner` 模块：真正读内容推断命令

`src-tauri/src/runner.rs`（新文件）：

| 清单文件 | 生成的运行目标 |
|---|---|
| `package.json` | `scripts` 里**每个脚本各一条**：命令 `<包管理器> run <脚本名>`，名称 = 脚本名 |
| `Cargo.toml` | `cargo run`（名称 = 文件名，行为不变） |
| `Makefile` / `GNUmakefile` | `make`（同上） |
| 其他 | 不处理，走原有兜底 |

- 包管理器按**同目录锁文件**识别：`pnpm-lock.yaml` → pnpm、`yarn.lock` → yarn、
  `bun.lockb`/`bun.lock` → bun，否则 npm
- 排序：`dev` → `start` → `serve` → 其余（同优先级内为字典序，保持稳定）
- 返回语义区分三种情况：`None`（不是清单/解析失败 → 回退旧逻辑）、
  `Some(vec![])`（是清单但没有 scripts → 明确告知不可运行）、`Some(specs)`（有建议）
  —— 关键点：**没有 scripts 时不能返回 None**，否则会退回写死的 `npm start`，等于把 BUG 放回去

### B. `get_execute_status` 返回全部建议目标

新增字段 `suggested_targets: Vec<RunSpec>`，前端据此批量创建；`suggested_cmd` 保留为第一条（兼容）。

### C. 前端：每个脚本创建一个运行目标（`ui/main.js`）

- 新增 `createRunTargets(fullPath, specs)`：逐个写 `run.target<N>.cmd/name/bind`
  （key 从已有 `target(N)` 最大值 +1 递增）
- 名称冲突时加父目录前缀（`dev` → `admin-web:dev`），再冲突加 `#N`
- 右键【运行】四分支：已有目标 → 执行；有建议目标 → 批量创建并提示
  `已创建 N 个运行目标：dev、build、test`；是清单但无 scripts → 提示错误；未知 → 原兜底
- 删掉前端 `cmdMap` 里写死的 `"package.json": "npm start"`

### D. AI 路径提示词带上内容与线索（`src-tauri/src/ai.rs`）

- 提示词新增：文件内容（按**字符**截断，上限 4000，二进制/读不出则标注）、
  同目录线索（锁文件 / Cargo.toml / pyproject.toml / go.mod / docker-compose.yml 等）
- 明确要求：若是 package.json，必须依 `scripts` 给命令，优先 dev/start，且不要带 `{file}`

## 验证

1. **Rust 单测**（`cargo test`，共 32 个用例全部通过，本轮新增 12 个）：
   - `runner::package_json_scripts_become_run_targets` —— 本 BUG 核心用例：
     只有 `dev/build/test` 的 package.json 必须得到 `npm run dev/build/test`，且 dev 排第一
   - `runner::package_manager_follows_lockfile` —— pnpm / yarn / bun / npm 四种锁文件
   - `runner::package_json_without_scripts_yields_empty` —— 无 scripts 时不得回退成 npm start
   - `runner::invalid_package_json_falls_back`、`runner::non_manifest_returns_none`、`other_manifests_keep_fixed_cmd`
   - `ai::prompt_includes_file_content` —— 提示词必须含 scripts 内容与锁文件线索
   - `ai::read_head_truncates_safely`、`ai::read_head_skips_binary`、`ai::dir_hints_detect_lockfiles`、
     `ai::prompt_marks_unreadable_content`
2. **前端行为**（抽真实 `handleContextRun` / `createRunTargets` + 假 invoke，5 个场景全 PASS）：
   三个脚本 → 三个目标（9 条配置，key `target0/1/2`，bind 正确）；索引从已有 `target7` 续到 `target8`；
   重名 → `admin-web:dev`；无 scripts → 只提示不写配置；已有目标 → 直接执行且带 bind
3. **真实命令实测**：`npm run dev` 在 `admin-web` 下退出码 0（`DEV CWD=...\admin-web`），
   在项目根退出码 38（`ENOENT ... \package.json`）

## 影响范围与迁移注意

- 只影响 `package.json` / `Cargo.toml` / `Makefile` 的运行目标生成；其他语言文件行为不变
- **已存在的旧运行目标不会自动改写**：如果之前已经生成了 `package.json → npm start` 的目标，
  右键再点【运行】会命中旧目标（`has_target` 为真）而直接执行它。
  处理办法：在导航区【运行目标】里删掉旧条目，再右键重来一次即可生成正确的脚本目标
- `Cargo.toml` / `Makefile` 的名称与命令保持原样，老配置继续有效
