# ruyix × darkhorse-harness 融合计划（v0.1）

> 目标：把 `D:\Projects\Rust\darkhorse-harness`（下称 **harness**）的 Agent 引擎能力
> （规划 → 生成 → 验证 → 规约 → 自纠正闭环）集成进 **ruyix** IDE，
> 并给出一份**团队协作、git 不冲突**的排班计划。
>
> 本文基于 2026-09-18 对两仓库的完整代码勘察。

---

## 0. 摘要（TL;DR）

1. **融合方式**：把 harness 的引擎层抽成一个**零 Tauri 依赖的 lib crate**
   （`crates/harness-engine`），落位到 ruyix 的 Cargo workspace；ruyix 侧新增
   `agent.rs` 命令桥 + `ui/agent.js` 前端面板。引擎在 harness 仓库继续演化，
   由"re-vendor 脚本 + 定点落位提交"单向同步。
2. **为什么可行**：harness 引擎（plan/generate/verify/lint/repair/exec/sandbox/
   gitops/entropy/kb/pipeline）对 Tauri 的耦合只有 2 处 `spawn_blocking` 调用
   （pipeline.rs:444/516，可用 `tokio::task::spawn_blocking` 平替）和 kb/commands.rs
   的命令包装层；UI 交互全部走 `pipeline::Sink` trait —— 天然的可移植边界。
3. **团队分工**：4 个角色（A 引擎 / B 后端桥 / C 前端 / D 基建与验收），
   按**文件所有权分区**保证 git 不冲突：harness 仓库只有 A 动；ruyix 仓库里
   `Cargo.toml` 只有 D 动、`main.rs` 只有 B 动、`ui/` 只有 C 动。
4. **节奏**：契约先行（第 1 天定稿命令 + 事件清单）→ 3 周完成 P0–P4
   （Agent 面板全链路可用）→ P5（项目模式 / rag-kb 统一 / 熵管理内置）单独立项。
5. **临时工作流变更**：ruyix 的"直接在 master 开发"规则在融合期间暂停，
   改用 `feat/agent/*` 短命分支 + 每周两次合并窗口；版本快照分支照旧冻结。

---

## 1. 现状盘点（勘察结论）

### 1.1 两个仓库的事实

| 维度 | ruyix（IDE） | darkhorse-harness（Agent 引擎） |
|---|---|---|
| 技术栈 | Tauri 2 + Rust 2024 + 原生 JS（无 npm） | 完全相同 |
| crate 结构 | 单 crate `ruyix`（src-tauri），workspace 仅 1 成员 | 单 crate `darkhorse-harness`，**引擎未拆分** |
| 后端模块 | main(45 命令)/config(3-scope)/pty/ai/git/runner/rag/instance | config/llm/plan/generate/verify/exec/workspace/lint/repair/sandbox/gitops/entropy/observe/kb/pipeline/eval |
| 前端 | command.js 命令系统为唯一 GUI↔后端桥；nav-tab/nav-panel 面板模式 | harness://* 事件驱动渲染；面板含历史/知识库/验证/修复 |
| LLM | `ai.rs`：async reqwest，OpenAI 兼容，默认 DeepSeek，键 `ruyix.code.ai.*` | `llm.rs`：async reqwest + 重试 + json 模式 + 脏 JSON 抽取，硬编码 DeepSeek 形状 |
| 检索 | `rag.rs`：embedding + qdrant-edge（整文件向量，"智搜"） | `kb/`：FTS5 BM25 + chunk + 六道闸门（Agent 上下文注入） |
| 子进程 | `run_target`/`spawn_terminal`/PtyManager 三套 + `run_git` | `exec.rs`：并发排空管道、超时、taskkill /F /T 进程树强杀、CancelFlag |
| Git | `git.rs`：status / 任意命令透传（面向用户） | `gitops.rs`：快照 / 回滚 / 分支回滚（面向 Agent 产物） |
| 沙箱 | 无（mlua 沙箱 + 路径校验属局部手段） | `sandbox.rs`：Docker 硬隔离 + 三档模式 + 不允许静默降级 |
| 测试 | 内联单测（各模块 `#[cfg(test)]`） | **174 个单测**，全部纯引擎测试，不构造 AppHandle |
| git 状态 | master，仅 `.gitignore` 未提交；远端 origin 存在 | master，**v0.7 大量改动未提交**（eval.rs、kb/、pipeline.rs 均为 untracked）；无远端 |

### 1.2 冲突 / 重叠点（融合必须决策的）

| # | 重叠 | 决策（见 §2） |
|---|---|---|
| 1 | 双 LLM 客户端（ai.rs vs llm.rs） | D3：保留两实现，配置层桥接统一 |
| 2 | 双检索系统（rag.rs vs kb/） | D6：并存，职责不同，P5 再议统一 |
| 3 | 多套子进程执行器 | D7：agent 链路独占 exec.rs，不迁移存量 |
| 4 | 双配置系统（3-scope vs AppConfig） | D3：`ruyix.code.harness.*` → AppConfig 适配器 |
| 5 | harness v0.7 未提交 | A 的第 0 号任务：先落基线提交 |
| 6 | "master 直开发"规则 vs 团队分支协作 | §6 临时豁免 + 合并窗口 |

---

## 2. 关键架构决策

### D1：引擎 crate 化（去 Tauri 化）——融合的地基

在 **harness 仓库**内把 `src-tauri/src/` 拆成两层：

```
darkhorse-harness/
├── engine/                      # 新 lib crate：harness-engine，零 tauri 依赖
│   ├── Cargo.toml               # serde/tokio(time)/reqwest/rusqlite(bundled)/chrono/regex/toml/dirs
│   └── src/
│       ├── lib.rs               # pub mod config, exec, observe, llm, plan, generate,
│       │                        #   verify, lint, repair, sandbox, gitops, entropy,
│       │                        #   workspace, pipeline, kb（kb 不含 commands.rs）
│       └── （16 个模块原样迁入，含全部 174 个单测）
└── src-tauri/                   # 瘦应用层：GuiSink、AppState、kb/commands.rs、CLI 分发、自己的 ui/
```

工程量预估：耦合面已被勘察证实极小——
- `pipeline.rs:444/516` 两处 `tauri::async_runtime::spawn_blocking` → 换 `tokio::task::spawn_blocking`（engine 增加 `tokio = "1"` 依赖）；
- `kb/commands.rs`（AppHandle 包装）留在应用层，engine 的 kb 只留核心；
- `workspace.rs` 对 `RunRecord` 的引用闭合在 engine 内部，无需改动。

验收：`cargo test -p harness-engine` 174 个单测全绿；harness 自身的 GUI 与 CLI
（entropy/sandbox/rollback/logs/trace/context/kb/eval）行为不变。

### D2：引擎落户方式——定点拷贝，单向同步

`crates/harness-engine/` 与 `tools/lint/`（python 规约包）由 **D 角色用脚本定点拷入**
ruyix 仓库（`scripts/vendor-engine.*`），每次落位是**一个原子提交**
（`chore(agent): vendor harness-engine @ <harness commit>`）。

- 理由：harness 仓库无远端、是实验室；ruyix 是唯一产品仓库。产品仓库自足，
  团队成员不需要同时克隆两个仓库（A 除外）。
- 纪律：**ruyix 内不直接改 `crates/harness-engine/`**（发现 bug → 提给 A 在
  harness 仓库修 → D 重新 vendor）。这保证单一事实来源，也保证 git 不冲突。
  （唯一例外：B 首次落位后的编译级修补，如 feature flag 合并，由 D 在落位提交内完成。）

### D3：配置桥——ruyix 三 scope 配置喂 AppConfig

引擎不改（仍然吃 `&AppConfig`）；ruyix 侧新增 `src-tauri/src/agent/config_bridge.rs`：

```
ruyix.code.ai.api_url / api_key / model        ──┐
ruyix.code.harness.llm.temperature/max_tokens   ├─→ AppConfig.llm
                                                 │
ruyix.code.harness.sandbox.mode/image/...      ──→ AppConfig.sandbox
ruyix.code.harness.lint.enabled/...            ──→ AppConfig.lint
ruyix.code.harness.verify.*                    ──→ AppConfig.verify
ruyix.code.harness.kb.*                        ──→ AppConfig.kb
ruyix.code.harness.workspace_root（默认 ~/.ruyix/code/agent/runs）→ AppConfig.workspace_root
```

- scope 解析沿用 ruyix 既有惯例：runtime → project → global（同 ai.rs:39-51 的读法）。
- 环境变量 `HARNESS_LINT_DIR` 由桥接层显式注入（默认指向 ruyix 仓内 `tools/lint`）。
- **沙箱默认值差异**（有意决策）：ruyix 集成默认 `sandbox.mode = "prefer"`
  （Docker 不可用 → 宿主执行 + 明确"未隔离"标注），实验室 harness 默认 `require`。
  理由：IDE 要开箱即用；"不允许静默降级"的原则不变（未隔离必须标注）。

### D4：事件与命令契约——payload 与 harness:// 完全同形

ruyix 侧事件名用 `agent://` 前缀，**payload 结构与 harness 的 `harness://*` 逐字段一致**
（`agent://log|stage|step|lint|repair|kb`）。收益：harness `ui/main.js` 的渲染函数
可以近乎原样移植进 `ui/agent.js`，且契约文档一份即可覆盖两端。

命令层（ruyix `main.rs` 注册，全部新增、不触碰既有 45 个命令）：
`agent_run / agent_plan / agent_generate / agent_verify / agent_lint / agent_repair /
agent_cancel / agent_runs / agent_run_load / agent_run_delete / agent_read_artifact /
agent_env_probe`。完整签名见附录 A。

### D5：运行目录与"项目模式"分期

- **P1–P4（本期）**：沿用 harness 的隔离运行目录模型，
  `~/.ruyix/code/agent/runs/<run_id>/{run.json, project/, logs.jsonl, trace.jsonl}`，
  Agent 在隔离沙盒里生成并验证，产物可回看、可经 gitops 回滚——**不直接碰用户项目**。
- **P5（下期，单独立项）**：项目模式——Agent 在打开项目的 git worktree/分支上工作，
  验证通过后提供"应用到工作区"（先 rescue 提交再合并，复用 gitops 纪律）。
  这依赖 P4 的稳定链路，不与本期混做。

### D6：rag.rs 与 kb/ 并存

职责不同，本期不统一：`rag.rs`（向量"智搜"，面向用户搜索代码）与
`kb/`（FTS5+闸门，面向 Agent 的上下文注入，预算/去重/工作区优先/注入边界都是
为 prompt 设计的）。P5 之后再评估：embedding 召回接入 kb 候选池，或统一存储。

### D7：exec.rs 不迁移存量执行器

ruyix 现有 `run_target`/`spawn_terminal`/PtyManager 保持原样（避免回归风险）；
exec.rs 仅服务 agent 链路。后续是否让 `run_target` 复用 exec.rs 的超时/进程树
强杀能力，作为独立小需求评估。

### D8：LLM 双实现并存，配置单源

`ai.rs`（命令翻译，阻塞语义、learn.lua 回写）与 engine `llm.rs`（json 模式、
重试、用量统计）行为差异大，本期不合并实现，只做**配置单源**（都读
`ruyix.code.ai.*`）。合并成一个带 provider trait 的客户端放 P5 评估。

---

## 3. 融合后架构

```
ruyix 仓库（唯一产品仓库）
├── Cargo.toml                       # workspace: [src-tauri, crates/harness-engine]
├── crates/harness-engine/           # D2：定点拷入，禁止就地修改（D 拥有落位提交）
│   └── src/                         #   config/exec/observe/llm/plan/generate/verify/
│                                    #   lint/repair/sandbox/gitops/entropy/workspace/
│                                    #   pipeline/kb —— 零 tauri，174 单测
├── tools/lint/                      # python 规约包（harness_lint），随 vendor 落位
├── src-tauri/src/
│   ├── main.rs                      # B：invoke_handler 追加 agent_* 注册块（唯一改动点）
│   └── agent/                       # B：新增目录
│       ├── mod.rs                   #   AgentState（cfg 快照 + CancelFlag）、命令实现
│       ├── sink.rs                  #   AgentSink: pipeline::Sink → emit agent://*
│       └── config_bridge.rs         #   ruyix.code.harness.* → engine AppConfig
└── ui/
    ├── index.html                   # C：nav 区新增 <button data-tab="agent"> + #nav-panel-agent
    ├── agent.js                     # C：面板逻辑（移植 harness ui/main.js 渲染器）+ mock 自测模式
    ├── command.js                   # C：新增 `agent <task>` 动词（default AI fallback 不变）
    ├── styles.css / lang/*.json     # C：面板样式 + i18n 文案
    └── main.js                      # C：load agent.js、状态栏监听（少量）

darkhorse-harness 仓库（实验室，仅 A 触碰）
├── engine/                          # D1：抽取后的 lib crate（持续演化）
└── src-tauri/                       # 瘦应用层（GUI/CLI 继续可用，作为引擎的试验台）
```

数据流（一条 Agent 任务的完整路径）：

```
ui/agent.js 输入任务
  → invoke("agent_run", {task})
  → agent/mod.rs: config_bridge 组装 AppConfig → pipeline::run(cfg, cancel, opts, &AgentSink)
  → engine: plan → generate → lint → verify → repair（每个阶段回调 Sink / 检查 CancelFlag）
  → AgentSink → emit("agent://stage|step|log|lint|repair", …)
  → ui/agent.js 渲染；产物落 ~/.ruyix/code/agent/runs/<run_id>/
```

依赖增量（src-tauri/Cargo.toml 仅 +1 行 path 依赖）：engine 侧引入
`chrono / tokio(time) / rusqlite(bundled) / regex`；reqwest/serde/toml/dirs 两边
版本一致（0.12/1/0.8/6），feature 取并集（rustls + json + blocking）。
capabilities 无需改动（事件与 State 走 core:default；引擎子进程用 std::process，
不需要 tauri shell 插件权限）。

---

## 4. 阶段计划与验收

| 阶段 | 内容 | 验收（可机械判定） |
|---|---|---|
| **P0 契约与地基**（第 1 周前半） | A：harness v0.7 基线提交 → 引擎抽取（D1）。B：`agent/` 骨架 + 配置桥设计。C：面板静态布局 + mock 渲染。D：契约文档定稿（附录 A/B）、ruyix workspace 改造、vendor 脚本 | 附录 A/B 评审签字；`cargo test -p harness-engine` 174 绿；harness GUI/CLI 回归通过 |
| **P1 引擎落位**（第 1 周后半） | D：首次 vendor drop（engine + tools/lint 原子提交）。B：AgentSink + config_bridge 编译通过并注册最小命令 `agent_plan` | ruyix `cargo build && cargo clippy --all-targets` 0 warning；`agent_plan` 在 devtools 里能返回真计划（配好 Key） |
| **P2 全命令桥**（第 2 周） | B：12 个 agent_* 命令全量 + 取消位 + 历史读写。A：引擎适配反馈修（在 harness 仓库修，D 中途一次 re-vendor）。C：事件渲染全家（stage/step/log/lint/repair/kb）+ 计划编辑再生成 | 命令层单测（mock LLM）全绿；`agent_cancel` 在 plan 阶段取消 <1s 生效 |
| **P3 前端面板成型**（第 2–3 周） | C：面板完整交互（任务输入、运行中状态、验证报告、修复轮次、历史列表、产物查看、env 探针展示）+ i18n 全量 + command bar `agent` 动词 + 状态栏集成 | `node scripts/check-style.js` 0 error；中英文切换无缺 key；未配 Key 时显示引导视图而非报错 |
| **P4 集成验收**（第 3 周末） | D：ui-smoke 增加 agent 场景（真点按钮跑 plan-only）；全门禁；文档收口（README/CLAUDE.md 更新模块清单） | 端到端：打开项目 → 面板输入任务 → plan-only 全链路事件流 → 报告渲染；`cargo fmt --check && cargo clippy --all-targets && cargo test && check-style.js` 全绿 |
| **P5 后续（另立项）** | 项目模式（worktree 隔离 + 应用到工作区）、rag/kb 统一评估、熵管理内置 IDE、LLM 客户端合并、run_target 复用 exec.rs | 届时定义 |

---

## 5. 团队分工与文件所有权矩阵（git 不冲突的核心机制）

> 角色可由人或 AI agent 承担。原则：**每个文件/目录任一时刻只有一个 owner 分支有权改**；
> 跨区需求走契约文档 + 指派给该区 owner，不允许直接代改。

| 区 | 仓库 | 文件 / 目录 | Owner | 分支前缀 |
|---|---|---|---|---|
| Z1 引擎 | darkhorse-harness | `src-tauri/src/*`（拆分）、新 `engine/*`、`tools/lint/*`、harness 的 `ui/`、`doc/*` | **A** | harness 仓 master 直做（单人仓库，无冲突） |
| Z2 落位 | ruyix | 根 `Cargo.toml`、`src-tauri/Cargo.toml`、`crates/harness-engine/**`、`tools/lint/**`、`scripts/vendor-engine.*` | **D** | `feat/agent/vendor-*` |
| Z3 后端桥 | ruyix | `src-tauri/src/agent/**`（新目录）、`src-tauri/src/main.rs`（**仅** invoke_handler 注册块与 `.manage()` 两处） | **B** | `feat/agent/cmd-*` |
| Z4 前端 | ruyix | `ui/agent.js`、`ui/index.html`、`ui/styles.css`、`ui/lang/*.json`、`ui/main.js` | **C** | `feat/agent/ui-*` |
| Z5 命令接入 | ruyix | `ui/command.js`（`agent` 动词；与 Z4 同 owner，避免 command.js 多人编辑） | **C** | `feat/agent/ui-cmd` |
| Z6 验收文档 | ruyix | `doc/*`、`scripts/ui-smoke*`、`README.md`、`CLAUDE.md` | **D** | `feat/agent/docs-*` |

**冲突点逐条排除**：

- `main.rs`（1429 行热文件）：只有 B 的"注册块"一处改动（append-only，约 +15 行），
  且安排在 D 的 vendor drop 合并**之后**（P1），无并行窗口。
- 根/子 `Cargo.toml`：只有 D 动（workspace 成员 + path 依赖），B/C 不加依赖；
  引擎需要的新依赖都在 engine 自己的 Cargo.toml 里（A/D 拥有）。
- `ui/command.js`（1426 行热文件）：只有 C 动；`agent` 动词插在 switch 的已知动词区，
  default AI fallback 语义不变。
- `ui/index.html` / `main.js`：只有 C 动。nav 面板走既有 `data-tab` 机制
  （`setupNavigatorTabs()` 自动接管，无需改 main.js 的 tab 逻辑）。
- `crates/harness-engine/`：只有 D 的原子落位提交有权写入（见 D2 纪律），
  B 发现问题提给 A，不在产品仓库就地修。
- 两个仓库物理隔离：A 的工作完全不进 ruyix 的 git 历史（经 vendor 脚本单向流入）。

---

## 6. Git 工作流（融合期间临时规则）

1. **临时豁免**：CLAUDE.md 的"直接在 master 开发"规则融合期间暂停。恢复条件：
   P4 验收通过后，删除本文档 §6 并回归 master 直开发。
2. **分支命名**：`feat/agent/<zone>-<topic>`，存活 ≤1 个合并窗口；窗口内不合并即
   rebase 到最新 master 后进下一窗口。
3. **合并窗口**：每周**二、五 17:00**，由 D 执行。流程：
   `cargo fmt --check && cargo clippy --all-targets && cargo test &&
   node scripts/check-style.js` 全绿 → 按下方顺序 rebase 合并 → master 上跑一遍
   ui-smoke → 在群里发合并摘要（分支、提交、影响面）。
4. **合并顺序**（结构性依赖，不可调换）：
   ```
   窗口1: Z2 vendor drop → Z3 最小命令（依赖 engine crate 存在）
   窗口2: Z3 命令增量 / Z4 面板（面板与命令桥并行，靠契约解耦）
   窗口3+: 各区自由，仍按 Z2 → Z3 → Z4/Z5 → Z6 顺序重放 rebase
   ```
5. **热文件冻结**：合并窗口期间 `main.rs`、`command.js`、两个 `Cargo.toml` 冻结
   （owner 提前半天在群里声明将合并的内容）。
6. **冲突兜底**：出现计划外冲突时，以"区归属"裁断（改了别人的文件 → revert 后
   走指派），不以先来后到裁断。
7. **回滚**：每个分支在窗口内原子合并，出问题 revert 整个 merge commit，
   不做热修丁式打补丁。

---

## 7. 排班表（3 周，4 角色）

> ●=主要工作 ○=支持/评审 ▲=合并窗口（D 主持） ★=里程碑

### 第 1 周（地基周）

| | 周一 | 周二 | 周三 | 周四 | 周五 |
|---|---|---|---|---|---|
| **A 引擎** | harness v0.7 基线提交；引擎抽取开工 | 抽取完成：174 单测绿、GUI/CLI 回归 | ○ 支持配置桥字段核对 | 引擎侧 review B 的接入反馈 | ○ 修引擎问题（若有） |
| **B 后端桥** | 契约评审；`agent/` 目录骨架 + config_bridge 设计稿 | config_bridge 单测（纯逻辑，不依赖 engine） | 基于 vendor 分支联调 AgentSink | `main.rs` 注册 `agent_plan` 最小链路 | ○ 联调支持 |
| **C 前端** | 面板静态布局（nav-panel-agent）+ mock 渲染模式 | 事件监听骨架（agent://*，用 mock payload 驱动） | stage/step/log 渲染器移植自 harness ui | 验证/修复渲染器移植 | ○ 联调支持 |
| **D 基建** | 契约定稿（附录 A/B）★；workspace 改造分支 | vendor 脚本编写 + 演练 | ▲窗口1：vendor drop 合入；B 基于其开发 | ui-smoke 现状基线跑通 | ▲窗口2：Z3 最小命令合入；周报 |

### 第 2 周（功能周）

| | 周一 | 周二 | 周三 | 周四 | 周五 |
|---|---|---|---|---|---|
| **A 引擎** | 引擎在 ruyix 语境的适配项（lint dir 注入、runs 根目录） | ○ P2 命令反馈修复 | 修复提交 | ○ 支持 | ○ 支持 |
| **B 后端桥** | agent_run/runs/load/delete 命令 | agent_generate/verify/lint/repair 命令 | 取消位贯通（CancelFlag 状态管理） | env_probe + read_artifact；命令层 mock-LLM 单测 | ▲窗口3 合并前自检 |
| **C 前端** | 面板 ↔ 真命令联调（plan 全链路） | 计划编辑 + 按计划再生成 | 验证报告 + 修复轮次 UI | 历史列表 + 产物查看 | ▲窗口3：Z4 大版本合入 |
| **D 基建** | ▲窗口3 准备：全门禁预跑 | re-vendor（A 的适配项）▲窗口3.5 | ui-smoke agent 场景开发 | doc：使用说明草稿 | ▲窗口4；周报；★P2 完成 |

### 第 3 周（体验与验收周）

| | 周一 | 周二 | 周三 | 周四 | 周五 |
|---|---|---|---|---|---|
| **A 引擎** | ○ 验收问题修复 | ○ | ○ | P5 项目模式预研文档 | P5 预研文档 ★ |
| **B 后端桥** | 边界加固：长任务 async/spawn_blocking、错误→状态视图映射 | 配置项补齐（sandbox prefer 默认、kb 开关） | ○ 验收问题修复 | P5 技术预案（worktree 应用流） | ○ |
| **C 前端** | command bar `agent` 动词 + 状态栏集成 | i18n 全量（zh/en 无缺 key） | 设置面板（harness 配置节） | 视觉打磨 + 未配 Key 引导视图 | ▲窗口5：收尾合入 |
| **D 基建** | 全门禁 + ui-smoke agent 场景接入 | README/CLAUDE.md 模块清单更新 | 验收清单逐项过（P4 表） | 缺陷登记与分派 | ▲窗口6；**★P4 验收**；恢复 master 直开发决议 |

**里程碑**：契约定稿（W1 周一）→ 引擎落位编译通过（W1 周三窗口1）→
最小链路 plan 跑通（W1 周五）→ 全命令桥完成（W2 周五）→ 端到端验收（W3 周五）。

**并行度说明**：B 与 C 全程并行不互等——C 用 mock payload 开发（`agent.js`
内置 dev 自测模式，渲染样例数据与真实 payload 同形），契约文档是唯一对齐物。
A 与 ruyix 侧全并行（不同仓库）。D 的两次 vendor 是唯一跨仓同步点。

---

## 8. 风险与对策

| # | 风险 | 对策 |
|---|---|---|
| R1 | harness v0.7 未提交改动在抽取时丢失/混入 | A 第 0 号任务先做基线提交（含 eval.rs/kb//pipeline.rs 全部 untracked），抽取在其后 |
| R2 | 双仓库漂移（产品侧悄悄改引擎） | D2 纪律 + vendor 脚本带 `--check`（diff 不为空即报警）+ 代码评审盯 `crates/harness-engine` 的非落位提交 |
| R3 | rusqlite bundled 拉长编译；首次构建失败 | engine 单独 crate 隔离影响面；README 写明需要 C 编译器（Windows 已具备） |
| R4 | sandbox `prefer` 默认带来的安全预期落差 | UI 常驻隔离状态标注（沿用"不允许静默降级"）；设置面板显著展示三档语义 |
| R5 | `main.rs`/`command.js` 热文件意外并行改动 | §5 所有权 + §6 窗口冻结 + 冲突兜底规则 |
| R6 | 长任务（pipeline 数分钟）阻塞 Tauri 命令线程 | 命令层 async + 阻塞段 `spawn_blocking`（引擎 verify 本就是阻塞设计）；取消位贯通验收 <1s |
| R7 | LLM Key 泄漏进 run 产物/日志 | 引擎侧已有写入层脱敏 + kb 密钥文件禁入；集成测试断言 run.json/logs 不含 Key 明文 |
| R8 | 3 周排期偏乐观（引擎抽取遇隐藏耦合） | 抽取是 W1 前两天，最坏情况只推迟窗口1；B/C 的 mock 驱动开发不受牵连；P3 面板交互项可裁到 P4 后 |

---

## 附录 A：Tauri 命令契约（ruyix 侧，全部新增）

```text
agent_run(task: String, project_root: Option<String>) -> RunSummary
    // 全流程：plan → generate → lint → verify → repair；事件流驱动 UI
agent_plan(task: String, project_root: Option<String>) -> PlanJson
    // 只出规划（可编辑后走 agent_generate）
agent_generate(run_id: String, plan_json: String) -> GenSummary
agent_verify(run_id: String) -> VerifyReportJson
agent_lint(run_id: String) -> LintReportJson
agent_repair(run_id: String, max_rounds: Option<u32>) -> RepairSummaryJson
agent_cancel() -> bool
agent_runs(limit: Option<usize>) -> Vec<RunListItem>          // newest-first
agent_run_load(run_id: String) -> RunRecordJson
agent_run_delete(run_id: String) -> ()
agent_read_artifact(run_id: String, rel_path: String) -> FileContent
agent_env_probe() -> EnvReport                                 // docker/python/git/node/…
```

约定：
- 状态与错误分离（沿用 harness 纪律）：可预期状态（未配 Key、无沙箱、语言不识别）
  返回 `Ok` + 状态字段；只有"答不了"才 `Err(String)`。
- 返回 JSON 结构与 harness 对应结构体同形（RunRecord/VerifyReport/LintReport…），
  字段以 harness `workspace.rs`/`verify.rs`/`lint.rs` 现有定义为准。
- 命令实现里 project_root 用于（且仅用于）任务上下文注入与 kb 工作区优先；
  P4 之前不向 project_root 写任何东西。

## 附录 B：事件契约（payload 与 harness:// 逐字段同形）

```text
agent://log    { level, msg }
agent://stage  { stage, status, detail }     // stage: plan|generate|lint|verify|repair
                                              // status: start|done|error|skip
agent://step   { index, total, title, status, notes, files, error }
agent://lint   { ran, ok, errors, warnings, summary, cmd, parse_error, diagnostics }
agent://repair { round, before, after, status, applied, detail, notes }
agent://kb     { id, phase, done, total, msg }
```

## 附录 C：配置键契约（ruyix.code.harness.*，三 scope）

```text
harness.enabled                 bool   默认 true（false = 面板显示功能停用，不加载引擎）
harness.workspace_root          path   默认 ~/.ruyix/code/agent/runs
harness.llm.temperature         f32    默认沿用 engine 默认
harness.llm.max_tokens          u32    同上
harness.sandbox.mode            str    ruyix 默认 "prefer"（D3；harness 实验室默认 require）
harness.sandbox.image           str    默认 python:3.12-slim
harness.lint.enabled            bool   默认 true
harness.lint.package_dir        path   默认 <ruyix 仓>/tools/lint（HARNESS_LINT_DIR 可覆盖）
harness.lint.max_repair_rounds  u32    默认 2
harness.kb.enabled              bool   默认 false（不改变现有行为）
harness.kb.top_k / token_budget / per_source_limit / min_score  同 engine KbConfig 默认
# LLM 端点与密钥复用既有键：ruyix.code.ai.api_url / api_key / model（D3/D8）
```
