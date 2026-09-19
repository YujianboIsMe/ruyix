# ruyix × darkhorse-harness 融合计划（v0.2）

> 目标：把 `/Users/mac/PycharmProjects/darkhorse-harness`（下称 **harness**）**整体搬迁**进
> **ruyix** IDE（Agent 引擎 + 规约包一次迁入，此后在 ruyix 内演化），实现 ai-harness-ide，
> 并给出一份**团队协作、git 不冲突**的排班计划。
>
> 本文基于 2026-09-18 对两仓库的完整代码勘察。
> **v0.2 修订（同日）**：融合模式由「长期双仓库 vendor 单向同步」改为「**一次性搬迁，
> harness 归档**」；同步更新已过时的事实盘点（harness v0.7 已全部提交、已有远端、
> 单测 177 个、ruyix 命令 43 定义 / 42 注册）。

> **✅ 执行进度（2026-09-18 夜间，单人按 CLAUDE.md master 直开发，未启用 §6 分支窗口）**
>
> - **P0–P3 已完成**：引擎落位（`7e11a07`，含 tools/lint）→ 命令桥 12 个 `agent_*`
>   注册 + config_bridge 单测（`859e46c`）→ 面板接真实命令 + 无后端演示回退（`8bc8d54`）。
>   harness 仓已打本地标签 `archive/pre-fusion`（对应 8388e1c；**推送远端与仓库描述
>   改名待用户执行**）。
> - **门禁实测**：`cargo fmt --check` ✓；`cargo clippy --all-targets` 0 warning
>   （引擎 79 条新工具链 lint 已清零）；`cargo test` 引擎 175 收集（159 通过 + 8 忽略：
>   1 真调 DeepSeek / 7 要 Docker）+ ruyix 35 通过（含 4 个 config_bridge 单测）；
>   `check-style.js` 0 error。8 个 ignored 即 harness 原有 live 测试，语义未变。
> - **与 D1 清单的偏差**：`eval.rs` 随引擎一并迁入（tauri-free，保住全部单测，
>   R1 的「eval 是否迁入」就此了结）；`kb/cli.rs` 同迁。`plan_stage`/`generate_stage`/
>   `verify_stage`/`repair_stage`/`run_lint` 由 `pub(crate)` 放开为 `pub`（命令桥需要，
>   引擎语义未改）。
> - **P4 剩余**：配好 Key 后在真机 GUI 跑一次 plan-only 端到端验收；ui-smoke 增加
>   agent 场景；归档标签推送远端。

> **✅ 执行进度（2026-09-19，Windows 机 P4 收尾；旧 harness 仓归档/推送按用户决定取消，不再执行）**
>
> - **ui-smoke 落地**：新增 `scripts/ui-smoke.js`（零依赖 Node，与 check-style 同款
>   自研路线）。U1–U5 契约静态断言（DOM 锚点 / `agent` 动词路由 / `agent://*` 事件
>   前后端一致 / 附录 A 12 命令注册 / zh-en i18n 全量对齐）+ U6–U8 agent 场景
>   （微型 DOM stub 驱动 `ui/agent.js` 内置演示回放：plan-only、全流程、运行中取消），
>   22 项全绿。
> - **plan-only 端到端验收（真调 LLM）**：新增 `crates/harness-engine/examples/plan_only.rs`
>   （P5 引擎 CLI 的雏形），`DEEPSEEK_API_KEY` 环境变量供 Key（不落盘）真调
>   DeepSeek：20s / 3266 tokens / 3 步（含测试步骤），run.json 落
>   `~/.ruyix/code/agent/runs/` 且无 Key 明文（R6 断言通过）。前端面板另经真实
>   浏览器（静态服务 `ui/`）跑演示回放验收，DOM 与截图均正常；Tauri 窗口本身
>   无法外部驱动，GUI→invoke→引擎链路由命令桥单测 + 本 example 分段覆盖。
> - **配套修正**：config_bridge 接通引擎环境变量纪律（`DEEPSEEK_API_KEY` 等优先于
>   配置文件、不落盘，GUI 与脚本同一来源，含单测）；两处存量测试在 Windows 上的
>   适配（`workspace_root` 断言归一路径分隔符；`save_is_fast…` 守卫阈值 60s→180s ——
>   Windows 并行测试下 git 子进程 spawn 开销所致，"抓卡死"语义不变）。
> - **门禁实测（Windows 11）**：fmt ✓ / clippy 0 warning / 引擎 159 通过 + 8 忽略 /
>   ruyix 41 通过 / check-style 0 error / ui-smoke 22 项全绿。
> - **P4 至此收口**（原"归档标签推送远端"一项取消）；§6 分支窗口未启用过，
>   master 直开发规则全程有效。

---

## 0. 摘要（TL;DR）

1. **融合方式（一次性搬迁）**：把 harness 的引擎层抽成一个**零 Tauri 依赖的 lib crate**
   （`crates/harness-engine`），连同 `tools/lint` 规约包**一次性迁入** ruyix 的 Cargo
   workspace；ruyix 侧新增 `agent/` 命令桥 + `ui/agent.js` 前端面板。
   **搬迁完成后，引擎的唯一事实来源就是 ruyix**；darkhorse-harness 仓库打归档标签后
   冻结，不再演化。
2. **为什么可行**：harness 引擎（plan/generate/verify/lint/repair/exec/sandbox/
   gitops/entropy/kb/pipeline）对 Tauri 的耦合只有 2 处 `spawn_blocking` 调用
   （pipeline.rs:444/516，可用 `tokio::task::spawn_blocking` 平替）和 kb/commands.rs
   的命令包装层；UI 交互全部走 `pipeline::Sink` trait —— 天然的可移植边界。
3. **团队分工**：4 个角色（A 引擎 / B 后端桥 / C 前端 / D 基建与验收），
   按**文件所有权分区**保证 git 不冲突：全部工作都在 ruyix 仓库内 ——
   `crates/harness-engine` 只有 A 动（首次落位提交由 D 执行）、根 `Cargo.toml` 只有
   D 动、`main.rs` 只有 B 动、`ui/` 只有 C 动；harness 仓库归档后无人触碰。
4. **节奏**：契约先行（第 1 天定稿命令 + 事件清单）→ 3 周完成 P0–P4
   （Agent 面板全链路可用）→ P5（项目模式 / rag-kb 统一 / 熵管理内置）单独立项。
5. **临时工作流变更**：ruyix 的"直接在 master 开发"规则在融合期间暂停，
   改用 `feat/agent/*` 短命分支 + 每周两次合并窗口；版本快照分支照旧冻结。

---

## 1. 现状盘点（勘察结论，v0.2 复核更新）

### 1.1 两个仓库的事实

| 维度 | ruyix（IDE） | darkhorse-harness（Agent 引擎） |
|---|---|---|
| 技术栈 | Tauri 2 + Rust 2024 + 原生 JS（无 npm） | 完全相同 |
| crate 结构 | 单 crate `ruyix`（src-tauri），workspace 仅 1 成员 | 单 crate `darkhorse-harness`，**引擎未拆分** |
| 后端模块 | main(43 命令，42 注册)/config(3-scope)/pty/ai/git/runner/rag/instance | config/llm/plan/generate/verify/exec/workspace/lint/repair/sandbox/gitops/entropy/observe/kb/pipeline/eval |
| 前端 | command.js 命令系统为唯一 GUI↔后端桥；nav-tab/nav-panel 面板模式 | harness://* 事件驱动渲染；面板含历史/知识库/验证/修复 |
| LLM | `ai.rs`：async reqwest，OpenAI 兼容，默认 DeepSeek，键 `ruyix.code.ai.*` | `llm.rs`：async reqwest + 重试 + json 模式 + 脏 JSON 抽取，硬编码 DeepSeek 形状 |
| 检索 | `rag.rs`：embedding + qdrant-edge（整文件向量，"智搜"） | `kb/`：FTS5 BM25 + chunk + 六道闸门（Agent 上下文注入） |
| 子进程 | `run_target`/`spawn_terminal`/PtyManager 三套 + `run_git` | `exec.rs`：并发排空管道、超时、taskkill /F /T 进程树强杀、CancelFlag |
| Git | `git.rs`：status / 任意命令透传（面向用户） | `gitops.rs`：快照 / 回滚 / 分支回滚（面向 Agent 产物） |
| 沙箱 | 无（mlua 沙箱 + 路径校验属局部手段） | `sandbox.rs`：Docker 硬隔离 + 三档模式 + 不允许静默降级 |
| 测试 | 内联单测（各模块 `#[cfg(test)]`） | **177 个单测**，全部纯引擎测试，不构造 AppHandle |
| git 状态 | master，工作区干净；远端 origin 存在 | master，**工作区干净**（v0.7 已全部提交，含 eval.rs/kb//pipeline.rs）；远端 origin 已配置（gitcode） |

### 1.2 冲突 / 重叠点（融合必须决策的）

| # | 重叠 | 决策（见 §2） |
|---|---|---|
| 1 | 双 LLM 客户端（ai.rs vs llm.rs） | D3：保留两实现，配置层桥接统一 |
| 2 | 双检索系统（rag.rs vs kb/） | D6：并存，职责不同，P5 再议统一 |
| 3 | 多套子进程执行器 | D7：agent 链路独占 exec.rs，不迁移存量 |
| 4 | 双配置系统（3-scope vs AppConfig） | D3：`ruyix.code.harness.*` → AppConfig 适配器 |
| 5 | ~~harness v0.7 未提交~~ **已解决**（v0.7 已全部提交，工作区干净） | 无需处理，直接搬迁 |
| 6 | "master 直开发"规则 vs 团队分支协作 | §6 临时豁免 + 合并窗口 |

---

## 2. 关键架构决策

### D1：引擎 crate 化（去 Tauri 化）——融合的地基

把 harness 的 `src-tauri/src/` **迁入 ruyix** 并拆出 lib crate（抽取与落位一次完成，
直接在 ruyix 仓库分支上进行，不在旧仓留中间态）：

```
ruyix/
├── crates/
│   └── harness-engine/           # 新 lib crate：harness-engine，零 tauri 依赖
│       ├── Cargo.toml            # serde/tokio(time)/reqwest/rusqlite(bundled)/chrono/regex/toml/dirs
│       └── src/                  # 16 个模块原样迁入，含全部 177 个单测
│           ├── lib.rs            # pub mod config, exec, observe, llm, plan, generate,
│           │                     #   verify, lint, repair, sandbox, gitops, entropy,
│           │                     #   workspace, pipeline, kb（kb 不含 commands.rs）
│           └── …
└── src-tauri/                    # ruyix 应用层：新增 agent/ 桥（见 §3）
```

工程量预估：耦合面已被勘察证实极小——
- `pipeline.rs:444/516` 两处 `tauri::async_runtime::spawn_blocking` → 换 `tokio::task::spawn_blocking`（engine 增加 `tokio = "1"` 依赖）；
- `kb/commands.rs`（AppHandle 包装）**不迁**：其职责由 ruyix 侧 `agent/` 桥的 AgentSink 承接，旧实现留在归档仓可考；
- `workspace.rs` 对 `RunRecord` 的引用闭合在 engine 内部，无需改动。

验收：`cargo test -p harness-engine` 177 个单测全绿；ruyix `cargo build &&
cargo clippy --all-targets` 0 warning；归档冻结前在旧仓跑**最后一次** GUI/CLI 回归
（entropy/sandbox/rollback/logs/trace/context/kb/eval）确认迁移基线无丢失。
原 harness 独立 GUI/CLI 随仓库归档退役；若仍需要 CLI，P5 评估在 ruyix 加 bin target。

### D2：引擎落户方式——一次性搬迁，harness 归档

`crates/harness-engine/` 与 `tools/lint/`（python 规约包）由 **D 角色用脚本一次性迁入**
ruyix 仓库（`scripts/import-engine.*`），落位是**一个原子提交**
（`feat(agent): import harness-engine @ <harness commit 8388e1c>`）。

- **搬迁后引擎的唯一事实来源是 ruyix**：发现 bug、加功能都直接在 ruyix 的
  `crates/harness-engine/` 修改（owner 见 §5 Z1），不绕回旧仓库。
- darkhorse-harness 仓库在搬迁验收（177 单测绿 + ruyix 全门禁绿）后打
  `archive/pre-fusion` 标签并冻结为只读归档：git 历史可考，不再接收改动；
  仓库描述标注「已并入 ruyix」。
- 理由：ruyix 是唯一产品仓库；团队只需克隆一个仓库；避免"改 bug 绕两仓"的
  长期摩擦，也不存在双仓库漂移问题。
- （唯一例外：落位提交内允许 D 做编译级修补，如 feature flag 合并。）

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

命令层（ruyix `main.rs` 注册，全部新增、不触碰既有 43 个命令）：
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
ruyix 仓库（唯一产品仓库，ai-harness-ide）
├── Cargo.toml                       # workspace: [src-tauri, crates/harness-engine]
├── crates/harness-engine/           # D2：一次性迁入；此后由 A 在此演化（Z1）
│   └── src/                         #   config/exec/observe/llm/plan/generate/verify/
│                                    #   lint/repair/sandbox/gitops/entropy/workspace/
│                                    #   pipeline/kb —— 零 tauri，177 单测
├── tools/lint/                      # python 规约包（harness_lint），随搬迁落位
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

darkhorse-harness 仓库（搬迁验收后归档，只读）
└── 打 archive/pre-fusion 标签冻结 —— git 历史可考，不再演化；
    仓库描述标注「已并入 ruyix（ai-harness-ide）」
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
| **P0 契约与地基**（第 1 周前半） | A：引擎抽取落位（D1，直接在 ruyix 分支进行）。B：`agent/` 骨架 + 配置桥设计。C：面板静态布局 + mock 渲染。D：契约文档定稿（附录 A/B）、ruyix workspace 改造、import 脚本 | 附录 A/B 评审签字；`cargo test -p harness-engine` 177 绿；ruyix build/clippy 0 warning |
| **P1 引擎落位**（第 1 周后半） | D：落位原子提交（engine + tools/lint 一个 commit）。B：AgentSink + config_bridge 编译通过并注册最小命令 `agent_plan` | ruyix `cargo build && cargo clippy --all-targets` 0 warning；`agent_plan` 在 devtools 里能返回真计划（配好 Key） |
| **P2 全命令桥**（第 2 周） | B：12 个 agent_* 命令全量 + 取消位 + 历史读写。A：引擎适配反馈修（在 ruyix 就地修）。C：事件渲染全家（stage/step/log/lint/repair/kb）+ 计划编辑再生成 | 命令层单测（mock LLM）全绿；`agent_cancel` 在 plan 阶段取消 <1s 生效 |
| **P3 前端面板成型**（第 2–3 周） | C：面板完整交互（任务输入、运行中状态、验证报告、修复轮次、历史列表、产物查看、env 探针展示）+ i18n 全量 + command bar `agent` 动词 + 状态栏集成 | `node scripts/check-style.js` 0 error；中英文切换无缺 key；未配 Key 时显示引导视图而非报错 |
| **P4 集成验收**（第 3 周末） | D：ui-smoke 增加 agent 场景（真点按钮跑 plan-only）；全门禁；**harness 仓归档**（末次回归 + `archive/pre-fusion` 标签）；文档收口（README/CLAUDE.md 更新模块清单） | 端到端：打开项目 → 面板输入任务 → plan-only 全链路事件流 → 报告渲染；`cargo fmt --check && cargo clippy --all-targets && cargo test && check-style.js` 全绿；归档标签已打 |
| **P5 后续（另立项）** | 项目模式（worktree 隔离 + 应用到工作区）、rag/kb 统一评估、熵管理内置 IDE、LLM 客户端合并、run_target 复用 exec.rs、engine bin（CLI）评估 | 届时定义 |

---

## 5. 团队分工与文件所有权矩阵（git 不冲突的核心机制）

> 角色可由人或 AI agent 承担。原则：**每个文件/目录任一时刻只有一个 owner 分支有权改**；
> 跨区需求走契约文档 + 指派给该区 owner，不允许直接代改。

| 区 | 仓库 | 文件 / 目录 | Owner | 分支前缀 |
|---|---|---|---|---|
| Z1 引擎 | ruyix | `crates/harness-engine/**`（首次落位提交由 D 执行，此后移交）、`tools/lint/**` | **A** | `feat/agent/engine-*` |
| Z2 落位与构建 | ruyix | 根 `Cargo.toml`、`src-tauri/Cargo.toml`、`scripts/import-engine.*`、`crates/harness-engine` 与 `tools/lint` 的首次落位提交 | **D** | `feat/agent/import-*` |
| Z3 后端桥 | ruyix | `src-tauri/src/agent/**`（新目录）、`src-tauri/src/main.rs`（**仅** invoke_handler 注册块与 `.manage()` 两处） | **B** | `feat/agent/cmd-*` |
| Z4 前端 | ruyix | `ui/agent.js`、`ui/index.html`、`ui/styles.css`、`ui/lang/*.json`、`ui/main.js` | **C** | `feat/agent/ui-*` |
| Z5 命令接入 | ruyix | `ui/command.js`（`agent` 动词；与 Z4 同 owner，避免 command.js 多人编辑） | **C** | `feat/agent/ui-cmd` |
| Z6 验收文档 | ruyix | `doc/*`、`scripts/ui-smoke*`、`README.md`、`CLAUDE.md` | **D** | `feat/agent/docs-*` |
| — 归档 | darkhorse-harness | 全仓只读（`archive/pre-fusion` 标签） | 无人 | — |

**冲突点逐条排除**：

- `main.rs`（1429 行热文件）：只有 B 的"注册块"一处改动（append-only，约 +15 行），
  且安排在 D 的落位合并**之后**（P1），无并行窗口。
- 根/子 `Cargo.toml`：只有 D 动（workspace 成员 + path 依赖），B/C 不加依赖；
  引擎需要的新依赖都在 engine 自己的 Cargo.toml 里（A/D 拥有）。
- `ui/command.js`（1426 行热文件）：只有 C 动；`agent` 动词插在 switch 的已知动词区，
  default AI fallback 语义不变。
- `ui/index.html` / `main.js`：只有 C 动。nav 面板走既有 `data-tab` 机制
  （`setupNavigatorTabs()` 自动接管，无需改 main.js 的 tab 逻辑）。
- `crates/harness-engine/`：首次落位提交（D）之后只有 A 有权写入（见 D2）；
  B 发现问题提给 A 就地修。
- harness 仓库归档后无人触碰；A 的引擎提交直接进 ruyix 的 git 历史。

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
   窗口1: Z2 一次性落位（import）→ Z3 最小命令（依赖 engine crate 存在）
   窗口2: Z3 命令增量 / Z4 面板（面板与命令桥并行，靠契约解耦）
   窗口3+: 各区自由，仍按 Z2 → Z1 → Z3 → Z4/Z5 → Z6 顺序重放 rebase
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
| **A 引擎** | 引擎抽取落位开工（ruyix 分支，D1） | 抽取完成：177 单测绿；旧仓 GUI/CLI 末次回归通过 | ○ 支持配置桥字段核对 | 引擎侧 review B 的接入反馈 | ○ 修引擎问题（若有） |
| **B 后端桥** | 契约评审；`agent/` 目录骨架 + config_bridge 设计稿 | config_bridge 单测（纯逻辑，不依赖 engine） | 基于落位分支联调 AgentSink | `main.rs` 注册 `agent_plan` 最小链路 | ○ 联调支持 |
| **C 前端** | 面板静态布局（nav-panel-agent）+ mock 渲染模式 | 事件监听骨架（agent://*，用 mock payload 驱动） | stage/step/log 渲染器移植自 harness ui | 验证/修复渲染器移植 | ○ 联调支持 |
| **D 基建** | 契约定稿（附录 A/B）★；workspace 改造分支 | import 脚本编写 + 演练 | ▲窗口1：落位原子提交合入；B 基于其开发 | ui-smoke 现状基线跑通 | ▲窗口2：Z3 最小命令合入；周报 |

### 第 2 周（功能周）

| | 周一 | 周二 | 周三 | 周四 | 周五 |
|---|---|---|---|---|---|
| **A 引擎** | 引擎在 ruyix 语境的适配项（lint dir 注入、runs 根目录） | ○ P2 命令反馈修复 | 修复提交（ruyix 就地改） | ○ 支持 | ○ 支持 |
| **B 后端桥** | agent_run/runs/load/delete 命令 | agent_generate/verify/lint/repair 命令 | 取消位贯通（CancelFlag 状态管理） | env_probe + read_artifact；命令层 mock-LLM 单测 | ▲窗口3 合并前自检 |
| **C 前端** | 面板 ↔ 真命令联调（plan 全链路） | 计划编辑 + 按计划再生成 | 验证报告 + 修复轮次 UI | 历史列表 + 产物查看 | ▲窗口3：Z4 大版本合入 |
| **D 基建** | ▲窗口3 准备：全门禁预跑 | 引擎适配项合入 ▲窗口3.5 | ui-smoke agent 场景开发 | doc：使用说明草稿 | ▲窗口4；周报；★P2 完成 |

### 第 3 周（体验与验收周）

| | 周一 | 周二 | 周三 | 周四 | 周五 |
|---|---|---|---|---|---|
| **A 引擎** | ○ 验收问题修复 | ○ | ○ | P5 项目模式预研文档 | P5 预研文档 ★ |
| **B 后端桥** | 边界加固：长任务 async/spawn_blocking、错误→状态视图映射 | 配置项补齐（sandbox prefer 默认、kb 开关） | ○ 验收问题修复 | P5 技术预案（worktree 应用流） | ○ |
| **C 前端** | command bar `agent` 动词 + 状态栏集成 | i18n 全量（zh/en 无缺 key） | 设置面板（harness 配置节） | 视觉打磨 + 未配 Key 引导视图 | ▲窗口5：收尾合入 |
| **D 基建** | 全门禁 + ui-smoke agent 场景接入 | README/CLAUDE.md 模块清单更新 | 验收清单逐项过（P4 表） | 缺陷登记与分派；harness 仓末次回归 + 归档准备 | ▲窗口6；**★P4 验收**；打 `archive/pre-fusion` 标签；恢复 master 直开发决议 |

**里程碑**：契约定稿（W1 周一）→ 引擎落位编译通过（W1 周三窗口1）→
最小链路 plan 跑通（W1 周五）→ 全命令桥完成（W2 周五）→ 端到端验收 + harness 归档（W3 周五）。

**并行度说明**：B 与 C 全程并行不互等——C 用 mock payload 开发（`agent.js`
内置 dev 自测模式，渲染样例数据与真实 payload 同形），契约文档是唯一对齐物。
A 与 D/B/C 在 ruyix 内按文件分区并行。D 的落位合并是唯一结构性依赖点。

---

## 8. 风险与对策

| # | 风险 | 对策 |
|---|---|---|
| R1 | 搬迁遗漏（harness 仓 doc/、eval/、scripts/ 等资产未随迁） | 落位前 D 出**搬迁清单**逐项勾验；未迁资产保留在归档仓只读可查；eval 是否迁入 P5 决策 |
| R2 | rusqlite bundled 拉长编译；首次构建失败 | engine 单独 crate 隔离影响面；README 写明需要 C 编译器（Windows 已具备） |
| R3 | sandbox `prefer` 默认带来的安全预期落差 | UI 常驻隔离状态标注（沿用"不允许静默降级"）；设置面板显著展示三档语义 |
| R4 | `main.rs`/`command.js` 热文件意外并行改动 | §5 所有权 + §6 窗口冻结 + 冲突兜底规则 |
| R5 | 长任务（pipeline 数分钟）阻塞 Tauri 命令线程 | 命令层 async + 阻塞段 `spawn_blocking`（引擎 verify 本就是阻塞设计）；取消位贯通验收 <1s |
| R6 | LLM Key 泄漏进 run 产物/日志 | 引擎侧已有写入层脱敏 + kb 密钥文件禁入；集成测试断言 run.json/logs 不含 Key 明文 |
| R7 | 3 周排期偏乐观（引擎抽取遇隐藏耦合） | 抽取是 W1 前两天，最坏情况只推迟窗口1；B/C 的 mock 驱动开发不受牵连；P3 面板交互项可裁到 P4 后 |
| R8 | 归档后仍有人向旧 harness 仓推送 | 归档时打标签 + 仓库描述标注「已并入 ruyix」；git push 权限收口（如平台支持） |

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
agent_apply_preview(run_id: String, project_root: Option<String>) -> Preview
    // 【只读】沙箱产物 vs 真实项目：逐文件 add/modify/same + before/after + git dirty
agent_apply_run(run_id: String, project_root: Option<String>, paths: Vec<String>, backup: Option<bool>) -> ApplyResult
    // 写回勾选的文件；内容一致项跳过；覆盖前备份到 <项目>/.ruyix/backups/<run_id>-<ts>/
```

约定：
- 状态与错误分离（沿用 harness 纪律）：可预期状态（未配 Key、无沙箱、语言不识别）
  返回 `Ok` + 状态字段；只有"答不了"才 `Err(String)`。
- 返回 JSON 结构与 harness 对应结构体同形（RunRecord/VerifyReport/LintReport…），
  字段以 harness `workspace.rs`/`verify.rs`/`lint.rs` 现有定义为准。
- 命令实现里 project_root 用于任务上下文注入（`agent/project_context.rs`）、kb 工作区优先，
  以及**写回目标**（`agent/apply.rs`）。
- **写回是显式两步**：`agent_apply_preview` 只读，`agent_apply_run` 必须带用户在 UI 上勾选出来的
  `paths`。绝不在 `agent_run` 结束时自动落盘 —— 未确认的机器产出只留在沙箱
  `<runs_root>/<run_id>/project/`。三条硬约束：路径封闭（拒 `..`/绝对路径/`.git`）、
  拒绝写进沙箱自身、写前备份。UI 侧由 ui-smoke U14 守住"写入只能由确认按钮触发"。

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
# v0.3 质量门禁（工具循环；见 doc/需求-Agent-验证与反思-v0.3.md）
harness.gate.narrow             bool   默认 true（改动后跑语法层）
harness.gate.full               bool   默认 true（交付前跑全量：语法 + 单测 + 规约）
harness.gate.max_full_attempts  u32    默认 3（连续失败几次后按预算放行，答复写明"未通过"）
harness.gate.staged_timeout_secs u64   默认 60（暂存内容语法检查的单条命令超时）
harness.reflect.enabled         bool   默认 true（交付前用干净上下文的复核 agent 过一遍）
harness.reflect.max_rounds      u32    默认 2（复核发现问题后最多回灌主循环几轮）
harness.reflect.model           str    默认空 = 与主循环同模型
# LLM 端点与密钥复用既有键：ruyix.code.ai.api_url / api_key / model（D3/D8）
```

## 附录 D：Agent 四原语（Read / Write / Execute / Connect）

会话里的 Agent **不是一堆专项工具**，只有四条能力原语 + 一条展示通道。模型每轮输出一个
JSON 对象选一个能力，直到给出 `{"final": ...}`（上限 24 轮，`agent::MAX_STEPS`）。

| 原语 | 参数 | 边界 |
|------|------|------|
| **Read** | `{"path":"src/ 或 src/main.rs"}` | 目录给结构树、文件给内容；路径封闭在项目内（`safe_rel_path`）；Git 历史用 Execute 跑 `git log` |
| **Write** | `{"path","content"}` | 整文件写入（新增/覆盖）；内容为空拒绝静默清空；写入策略见下 |
| **Execute** | `{"cmd","timeout_secs"}` | cwd 钉项目根，超时 5–120s（默认 30），stdout/stderr 各裁到 4KB，破坏性模式拒绝清单拦"不可逆的系统级破坏"（绊线不是沙箱，真隔离在 verify 的 docker 模式） |
| **Connect** | `{"action":"list"\|"call"\|"send", ...}` | 见下 |

**为什么删掉 `edit`（find/replace 结构化修改）**：四原语是能力集的上限，Write 已经覆盖
"改文件"这件事的语义；多一个 edit 就多一种模型要选的形态、多一类"find 不唯一/不匹配"
的失败模式。代价是改动也走整份内容（先 read 再整份 write），换来的是能力集简单、审查口径统一
（任何变更都是一份完整新内容），差异面板与暂存/备份/三模式写入全部原样复用。
提示词里明确写了"哪怕只改一行也交回整份内容，没把握的地方原样保留"。
管道自修复（`repair.rs`）的 find/replace 是另一条独立通路，不受影响。

**Connect 的契约与落地**：

```text
引擎侧（零 tauri）  trait Connector { list() -> Vec<ConnectTarget>; call(ConnectRequest) -> ConnectOutcome }
                    NoConnector = 没接外部能力的空实现（单测 / 评估臂）
宿主侧（ruyix）     agent/connect.rs::RuyixConnector
                    call  action=call → mcp.toml 里找服务器，没连上先握手启动，再 tools/call
                    call  action=send → a2a.toml 里找远端 Agent，message/send + 轮询到终态
                                      进度走既有 a2a://status 事件（与 a2a_send 同一条）
```

- **清单进提示词**：run 开始取一次 `list()`，非空就拼进首条用户消息（`- mcp fs：已连接；工具：read_file(…)`）。
  空清单不出现 —— 模型不会知道自己没有的能力，也就不会瞎猜名字。
- **失败分两层**：目标不存在/起不来 = `Err`（那一轮记为失败，模型改路子）；外部系统自己报的
  业务错误 = `ConnectOutcome::is_error`（信息，照样交回模型判断）。
- **连什么是宿主的事**：子进程、HTTP、三 scope 配置都在 ruyix 侧；引擎搬到别的宿主换一个
  `Connector` 实现即可，不必被 MCP 客户端与 a2a 表绑住。
- **守门**：`scripts/ui-smoke.js` U16 断言四原语 + `execute_allowed` + `MAX_STEPS` + `trait Connector`，
  并要求 mod.rs 真的建了 `connect::RuyixConnector` 且 connect.rs 同时有 MCP 与 A2A 通路（防 Connect 变空壳）。
- **验收工具**：
  - `cargo run -p harness-engine --example agent_loop_smoke` —— 七轮剧本（read → plan →
    connect list → connect call → write → execute → final）跑真循环：假 LLM 顶在 HTTP 另一头
    （无需 Key），HTTP 往返 / 循环 / 覆盖层 / 暂存目录 / 真实 shell 全真跑，13 项断言自检。
  - `cargo test -p ruyix live_mcp_connect -- --ignored` —— Connect 的 MCP 通路真·往返
    （python 起的 stdio JSON-RPC 回显服务器：未连接 → 自动握手 → tools/call → 清单转"已连接"）。
