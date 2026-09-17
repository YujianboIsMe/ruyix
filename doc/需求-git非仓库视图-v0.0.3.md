# 需求：Git 非仓库视图（0.0.3）

状态：已批复（2026-09-14，按推荐方案：P1 初始化仓库 + 添加远程仓库，P2-P5 取文档推荐默认值）→ 已实施
归属版本：0.0.3
提出日期：2026-09-14

## 现象（BUG）

打开一个**不是 git 仓库**的项目，点大纲区【Git】标签，面板显示：

```
⚠ fatal: not a git repository (or any of the parent directories): .git
```

## 根因

1. `src-tauri/src/git.rs:105` `status_impl()` 在项目根执行 `git status --porcelain`，
   非仓库时 git 退出码 128 → 函数直接 `Err(first_line(stderr))`，把原样英文报错抛给前端。
2. `ui/main.js:934-941` `loadGitStatus()` 的 catch 分支把 `err` 直接渲染成 `⚠ {err}`，
   没有"这个项目还不是 git 仓库"这个状态。

**结论**：后端把「非仓库」误当异常，前端缺对应空状态视图。

## 目标

项目不是 git 仓库时，Git 面板展示**仓库初始化引导**，而不是英文报错：

1. 明确告知：当前项目还不是 Git 仓库
2. 提供动作：**初始化仓库**（`git init`）、**添加远程仓库**（`git remote add origin <url>`）
3. 初始化完成 → 自动刷新为正常的 staged/unstaged 视图
4. 仓库已存在但没有远程仓库 → 顶部提示 + 添加远程仓库入口

## 方案（推荐）

### 后端 `src-tauri/src/git.rs`

- `GitStatus` 增加两个字段（不破坏现有调用方，前端按需读）：
  - `is_repo: bool` —— 由 `git rev-parse --is-inside-work-tree` 判定
  - `remote: Option<String>` —— 由 `git remote get-url origin` 读取
- `status_impl()` 非仓库时返回 `GitStatus { is_repo: false, ... }`（**不再 Err**）；
  `Err` 只保留给"git 没装 / 启动失败"这类真异常。
- 用 `rev-parse` 而不是"检测 .git 目录"：项目根在父目录仓库内时也应视为在仓库中（git 语义一致）。

### 前端 `ui/main.js`

- `loadGitStatus()`：`!status.is_repo` → 渲染**初始化引导视图**（`renderGitInitView()`）
- `status.is_repo && !status.remote` → 正常视图 + 顶部 banner「尚未配置远程仓库」+ 按钮
- 有 remote → 分支行显示 `🌿 <branch> · origin <url>`

### 命令系统（架构约束）

GUI 按钮**必须**经 `handleCommand()`，不直接 `invoke()`：

| 按钮 | 命令 | 说明 |
|---|---|---|
| 初始化仓库 | `git init` | 复用现有 `git <子命令>` 透传 |
| 添加远程仓库 | `git remote add origin "<url>"` | URL 由 `showPrompt()` 弹窗收集 |

**零新增命令动词** —— `handleGitCommand()` 已支持任意 git 子命令透传。
执行后 `handleGitCommand` 已有的 `loadGitStatus()` 会刷新面板。

### i18n / 样式 / 文档

- `ui/lang/zh-CN.json` + `en.json`：新增 `git.init.*` / `git.remote.*` 键
- `ui/styles.css`：`.git-init-view`、`.git-banner`（尽量复用现有按钮与 `.git-empty` 风格）
- `doc/git.md`：补「非仓库视图」章节

## 待拍板点

| 编号 | 问题 | 推荐 |
|---|---|---|
| P1 | 空仓库视图提供哪些动作？ | 初始化仓库 + 添加远程仓库（本次核心） |
| P2 | `git init` 后是否强制改默认分支为 `main`？ | **不改**，尊重用户 git 全局配置（`init.defaultBranch`） |
| P3 | 已存在 `origin` 时再次"添加远程"如何处理？ | 用 `git remote set-url origin <url>` 覆盖，并提示"已更新" |
| P4 | 是否要"从远程克隆到当前项目目录"？ | 不做：当前场景是本机已有项目，克隆属于新项目入口，另立需求 |
| P5 | 是否加"首次提交"（`git add -A` + `git commit`）？ | 可加，但建议放 P1 之外单独确认（影响面稍大） |

## 验收

1. 非仓库项目 → Git 面板显示中文引导 + 两个按钮，不再出现英文 fatal 报错
2. 点【初始化仓库】→ 面板变为正常视图（分支 `(no commits)`，文件列表出现），状态栏提示成功
3. 点【添加远程仓库】输入 URL → 面板 banner 消失，分支行显示远程地址
4. 真仓库项目行为不变（回归）
5. `cargo test` 覆盖 `is_repo` 判定（临时目录 `git init` 前后各一例）
