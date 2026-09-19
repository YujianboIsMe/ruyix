# 需求：Agent 机械验证 + 反思（v0.3）

状态：已批复（2026-09-19，P1–P6 全部按推荐）→ 已实施（见文末「实施记录」）
归属版本：v0.3（ruyix Agent 会话链路）
提出日期：2026-09-19
关联：[融合计划-Agent集成-v0.2.md](融合计划-Agent集成-v0.2.md)（四原语工具循环 + 附录 D）、[capability.md](capability.md)

## 背景

工具循环（Read / Write / Execute / Connect）已经把"能干活"打通了，但**质量闭环没有**：

1. **模型自己宣布完成**。循环跑到模型输出 `{"final": ...}` 就结束（`crates/harness-engine/src/agent.rs`），
   没有任何独立判据。改完不跑测试、跑不跑由模型自觉 —— 它说"已验证"可能就是没跑。
2. **没有反思**。改完的问题（漏了需求某一条、破坏了既有行为、边界没处理）只能靠用户 review。
3. **问答也会迷航**。没有外部证据约束时，模型容易用确定语气输出没有依据的结论
   （"这个项目用的是 X"，其实没读过那个文件）。

这两件事是**不变形的零件**：机械验证是确定性的（无 LLM），反思是"换一个干净上下文复核"。
复杂任务分步、任务分类这些上层编排都建立在它们之上 —— 所以先做这两件（用户 2026-09-19 拍板）。

**本版范围**：只做机械验证 + 反思。**不做**：任务分类器、规划模式（一律不动，见 §7）。

### 可复用的现成件（不重写）

| 能力 | 位置 | 复用方式 |
|---|---|---|
| 语言识别 | `verify::detect_languages`（Cargo.toml / package.json / pyproject 等标记） | 窄验证选哪套命令 |
| 语法 + 单测 | `verify::run` → `VerifyReport{checks: Vec<CheckResult>}` | 全量验证主体 |
| 失败→修复提示 | `verify::hint_for` | 回灌给模型的观察文本 |
| 规约检查 | `lint::run_lint` + `tools/lint`（13 类规则 + `conventions/*.md`） | 全量验证的 lint 段 |
| 命令执行 | `exec::{run, clip, Probe}`（超时/裁剪/取消） | 验证命令落地 |
| 运行命令推断 | `src-tauri/src/runner.rs`（从清单推断"项目该怎么跑"） | 判据来源第二优先级 |
| 快照/回滚 | `gitops.rs`（分步 commit + 回滚） | 第二阶段分步的兜底，本版不用 |
| 事件通道 | `pipeline::Sink` trait + `agent://*` 事件 | 新增 `verify` / `reflect` 回调 |

## 目标

1. **机械验证门禁**：产生改动的 run，在交付（`final`）之前必须有一次**由事实触发**的确定性验证；
   未通过不许交付，报告回灌主循环继续修。
2. **反思**：交付前用**另一个上下文干净的 agent**复核产物（或答案），产出可证伪的结构化结论，
   结论回灌主循环自修，并透明呈现给用户。

## 方案（推荐）

### 一、机械验证（无 LLM，确定性）

#### 1. 触发：由**事实**决定，不用分类器

| 运行中的事实（可观测） | 动作 | 超时 | 失败语义 |
|---|---|---|---|
| 本轮**产生了文件改动**（`ctx.changes` 非空） | 每轮改动后 → **窄验证** | 60s | 观察（回灌），不阻断继续干活 |
| 模型要输出 `final` 且**有改动** | **全量验证**（强制门禁） | 300s | **拒绝 final**，报告回灌 |
| 模型要 `final` 但**无改动** | 不跑项目级验证（走反思的依据核对） | — | — |
| 轮次/预算耗尽 | 收尾前强制全量验证一次 | 300s | 答复必须写明验证结论（通过/未通过/跳过） |

为什么不用"问答/任务"分类器：分类是**预测**（还没读仓库就下判断），而验证需求是**事实**
（有没有写文件）。事实不会误判、不需要额外一次模型调用、还能中途修正（先以为是问答，
写文件那一刻自动进入被验证状态）。

#### 2. 验证在哪跑：按写入策略分档

| 写入策略 | 改动在哪 | 验证档位 |
|---|---|---|
| 写入 / 自主（`WritePolicy::Apply`） | 已落项目磁盘 | 窄验证 + 全量验证（都在项目上跑） |
| 确认（`WritePolicy::Stage`） | 只在 `.ruyix/stage/`，磁盘未动 | 默认**静态层**：单文件语法/编译到 metadata（`rustc --emit=metadata`、`py_compile`、`node --check`…）；可选"临时副本"档（配置开关，把项目复制到临时目录后按全量验证跑） |

**明确不用"临时落盘 → 验证 → 回滚"**：那会破坏"确认模式不动磁盘"这条既有不变量（UI 上
"还没确认就已经改了"是最难解释的 bug）。

#### 3. 判据来源（优先级写死）

1. 项目配置的 run targets（`ruyix.code.run.target<N>.cmd`，用户显式配的）
2. 清单 / 语言推断（`runner.rs` + `verify::detect_languages`）
3. 模型自报的验证命令（**仅当**前两者都给不出可跑命令时才采纳）

#### 4. "通过"的定义（写死，不让反思 agent 去猜）

- 语法/编译：`failed = 0`
- 测试：`failed = 0`（没有测试也算通过，但要在报告里写"无测试"）
- lint：`error = 0`（warning 只记录，不阻断 —— 与前端 `check-style` 的 0 error 口径一致）
- **项目里找不到任何可跑命令** → `status = skipped`（**不是 failed**），答复里必须写跳过原因

#### 5. 失败语义

验证失败是**信息**不是工具故障（不走 `Err`）：报告作为观察回灌主循环 → 模型继续修（消耗轮次）。
但**禁止 final 放行**；预算用尽时输出"未通过验证"的诚实答复 + 完整报告，绝不假装完成。

#### 6. 护栏

- **取消位**：`agent_cancel` 目前只对旧流水线生效，要贯通到工具循环（长验证必须有取消路径）
- 超时按层给（窄 60s / 全量 300s，可配），输出用 `exec::clip` 裁剪
- 验证**只读**：验证过程不许写文件（它只跑检查命令）
- 阻塞调用走 `tokio::task::spawn_blocking`（`verify_stage` 已有先例），别占住 async worker

#### 7. 引擎契约（待实现）

```text
agent::VerifyGate { narrow: bool, full: bool, timeout_secs_narrow, timeout_secs_full }
agent::VerifyOutcome { layer: "narrow"|"full", status: "passed"|"failed"|"skipped",
                       checks: Vec<verify::CheckResult>, hint: String, skipped_reason: Option<String> }
pipeline::Sink::verify(&self, v: &VerifyOutcome)   // 默认实现空转；ruyix 侧 emit agent://verify
```

### 二、反思（LLM，必须干净上下文）

#### 1. 为什么必须新开上下文

主循环的上下文里有推理轨迹、失败尝试、被推翻的假设 —— 让**同一个上下文**自我复核，等于让它
给自己背书。反思 agent 只拿**客观产物**：任务原文 + 变更 diff（或答案）+ 机械验证报告 + 项目根，
自己用 `read` 去看文件。**不给**：主循环 messages、工具调用轨迹、usage。

代价是"反思不知道主循环的意图"，所以喂给它的必须是**事实**（改了什么、验证结果如何），
而不是"你看看他做得对不对"这种主观问法。

#### 2. rubric 由事实选（仍然不用分类器）

| 事实 | rubric | 复核什么 |
|---|---|---|
| 有改动 | **产物评审** | 需求覆盖（用户要的每条都做了吗）、边界/错误路径、是否破坏既有行为、测试是否真的证明了行为 |
| 无改动（问答） | **依据核对** | 答案里每条断言是否有读过的证据（防幻觉/迷航）；没依据的要标出来 |

#### 3. 能力约束

- 允许：`read`（自己去看文件）
- **不给** `write`（不引入第二个写手，避免冲突与"谁改的"歧义）
- **不给** `execute`（验证命令归机械验证层；反思不需要跑命令，给了只会扩大攻击面）

#### 4. 输出契约（可证伪，不要自由文本意见）

```json
{"verdict": "ok|suspect",
 "findings": [{"severity": "high|medium|low",
               "claim": "结论一句话",
               "evidence": "path:line 或命令输出片段（没证据写 unknown）",
               "verdict": "supported|unsupported|unknown",
               "suggest": "建议怎么改"}],
 "summary": "一句话"}
```

解析失败 → 重试 1 次 → 仍失败 → 记 `warn` 降级，不阻断交付。

#### 5. 结论去向（推荐）

- `suspect` / findings 非空 → 作为**新一轮观察**回灌主循环让它自己修，最多 `R` 轮（默认 2）
- 同时**原样附在最终答复里**（透明：用户看到复核意见，也能看到有没有被采纳）
- 不引入第二个写手（README/代码只有主循环在改）

#### 6. 预算与降级

- 每次 run 反思轮数 ≤ `R`（默认 2）；问答（无改动）固定 1 轮
- 反思失败 / 超时 / 解析失败 **不判任务失败**，只在会话里记"复核未完成 + 原因"
- 模型可单独配（本版先同模型，留 `ruyix.code.harness.reflect.model` 配置位）
- 取消位贯通（用户点取消，反思立刻停）

#### 7. UI

- 事件：`agent://verify`（layer/status/摘要/诊断数）、`agent://reflect`（verdict/summary/findings 数）
- 会话气泡内新增"验证 / 复核"小节（徽章 + 可展开详情），让用户一眼看出"这轮验过没有"
- 大纲区的改造（换成步骤/验证状态）**放第二阶段**，本版只做气泡内小节

## 与既有纪律的关系（不变量，不得破坏）

1. 路径封闭、写前备份、三模式写回唯一入口 `applyPaths`（ui-smoke U14）
2. 事件契约新增两个事件，要同步进 ui-smoke U3 的 required 列表
3. 新增守门断言：
   - **U17 verify-gate**：引擎里"有改动 → final 前必有验证结论"的门禁存在；UI 有 `agent://verify` 渲染；
     源码里能看出"验证未通过不放行"的分支
   - **U18 reflect-clean-context**：反思调用只由 `REFLECT_SYSTEM` + 结构化包构成（源码断言：不含 history 参数）、
     反思路径无 `write` 注入

## 验收（可证伪）

### 1. 单测（引擎）

- 门禁判定表：无改动 → 跳过；窄验证失败 → 回灌；全量失败 → 拒绝 final；预算耗尽 → 诚实答复；
  `skipped` 不算 `failed`
- rubric 选择（有改动/无改动）与 findings 解析容错（含非 JSON、缺字段、verdict 乱填）
- 反思只读：不存在通向 `write` 的注入路径（构造性测试）
- 取消位贯通：循环中置位 → 停止，已写内容落盘 + 报告状态正确

### 2. e2e（扩展 `crates/harness-engine/examples/agent_loop_smoke.rs`）

剧本：`read → write → 窄验证 → 全量验证 → 反思 → final`，断言：

- 写入后轨迹/事件里出现验证层（窄 + 全量各一次）
- **干净上下文**：在假 LLM 那一侧按请求序号断言，反思那次的 messages 里**不含**主循环的
  read/plan 轨迹文本
- arm 2：预埋验证失败 → `final` 被拦 + 出现回灌轮次
- arm 3：全量验证持续失败 + 预算耗尽 → 答复明确写"未通过验证"，且带报告

### 3. 对照实验（判据先写死，跑完不许改口径）

夹具：小项目 + 预埋一处测试失败（或其他可机械发现的缺陷）。三臂：

| 臂 | 配置 | 观察量 |
|---|---|---|
| A | 无验证无反思 | 是否在 final 前发现预埋问题 |
| B | 只机械验证 | 同上 + token/时延 |
| C | 机械验证 + 反思 | 同上 + 反思发现的"机械验证抓不到"的问题数 |

判读规则（先写死）：**B 相对 A 必须能发现预埋缺陷**（否则是门禁实现问题）；
**C 相对 B 的增益若为 0**，结论是"反思 rubric 需要重做"，而不是"反思无用就删"。

## 待拍板点

| 编号 | 问题 | 推荐 |
|---|---|---|
| P1 | 确认模式（Stage）下验证在哪跑？ | 默认静态层（单文件语法/编译）；"临时副本"作为可选开关，不做"落盘再回滚" |
| P2 | 全量验证不通过时怎么处理？ | 拒绝 `final` + 回灌继续修；预算耗尽才放行，且答复必须写明未通过 |
| P3 | 反思结论去向 | 回灌主循环自修 + 原样附在最终答复（透明） |
| P4 | 问答（无改动）也反思吗？ | 反思，但 rubric = 依据核对、固定 1 轮（成本受限） |
| P5 | 验证命令与超时默认值 | 窄 60s / 全量 300s；来源优先级见 §一.3；都进配置 |
| P6 | 反思用哪个模型 | 先同模型，留 `ruyix.code.harness.reflect.model` 配置位 |

## 第二阶段（本版不做）：去掉分类器与规划模式

本版（验证 + 反思）落地后，按既定方向**彻底删掉分类器与规划模式**，只留"一条循环 + 事实驱动"。
删除清单与爆炸半径先记在这里，第二阶段单独立需求 doc 拍删除力度：

- **引擎**：`plan.rs`（PLAN_SYSTEM/GEN_SYSTEM/parse_plan/slugify）、`pipeline` 的
  `plan_stage → generate_stage` 两段式、`generate.rs`、`workspace::RunRecord` 的 plan/generation 字段、
  `observe.rs` 的 plan 段、`examples/plan_only.rs`
- **ruyix 命令**：`agent_run` / `agent_plan` / `agent_generate` / `agent_verify` / `agent_lint` /
  `agent_repair` / `agent_apply_preview` / `agent_apply_run` / `agent_read_artifact`（UI 侧已无调用者）
- **UI**：大纲区的计划渲染（`session.js` 的 `_plan` / `STEP_EMOJI` / `hydratePlan` 死代码、
  `agent://plan|step`、`.outline-plan-*` 样式）、ui-smoke **U15**
- **需单独议**：`eval.rs`（1693 行评估器）、`entropy.rs`（1475 行技术债巡检）—— 引擎自用、未暴露 UI，
  但 `eval` 依赖 generate/verify/lint，`entropy` 依赖 lint/repair/verify
- **必须保留**：`verify` / `lint` / `gitops` / `exec`（机械验证零件）、`stage.rs`（暂存）、
  `apply.rs` 的三条安全约束

删除力度（档 1 薄删 / 档 2 中删流程编排留零件 / 档 3 连 eval+entropy+沙箱一起删）在第二阶段需求 doc 里拍。

## 附录：术语

- **窄验证**：改动后立刻跑的廉价检查（语法/编译/受影响文件），60s 级
- **全量验证**：交付前的完整检查（编译 + 测试 + lint），300s 级
- **门禁**：有改动时，"没有验证结论就不许输出 `final`"这条硬约束
- **rubric**：反思的判据表（产物评审 / 依据核对），由"有无改动"这个事实选择
- **干净上下文**：反思 agent 的 messages 只含 REFLECT_SYSTEM + 结构化输入包，不含主循环轨迹
- **findings**：反思的结构化输出（claim + evidence + verdict），可证伪，不做自由文本意见

## 实施记录（2026-09-19）

**落地位置**

| 层 | 文件 | 内容 |
|---|---|---|
| 引擎 | `crates/harness-engine/src/agent.rs` | `VerifyOutcome` / `CheckItem`（对外形状）、`GateState`、`narrow_verify`、`full_verify`、`lint_item`、`gate_before_final`；`run()` 增加 `cancel: &CancelFlag`，`Final` 走门禁，`write` 后跑窄层 |
| 引擎 | `crates/harness-engine/src/reflect.rs`（新） | `REFLECT_SYSTEM`、`Rubric` + `select_rubric`、`Finding`/`Reflection`、`ReflectInput`、`build_user_prompt`、`parse_reflection`、只读 `run()` |
| 引擎 | `crates/harness-engine/src/verify.rs` | 新增 `staged_syntax_checks`（暂存内容单文件语法检查，写系统临时目录）；构建产物目录 `.harness-target`/`.harness-out` → `.ruyix/target-verify`/`.ruyix/out-verify` |
| 引擎 | `config.rs` / `pipeline.rs` / `lib.rs` | `GateConfig` + `ReflectConfig`；`Sink::verify` / `Sink::reflect` 默认空实现；注册 `reflect` 模块 |
| ruyix | `agent/sink.rs` | 发 `agent://verify` / `agent://reflect` |
| ruyix | `agent/mod.rs` | `agent_reply` 接 `AgentState` 取消位（起跑清零）+ `ReplyAgent` 透传 `verifications` / `reflections` |
| ruyix | `agent/config_bridge.rs` | 7 个新键（gate 4 + reflect 3）+ 单测 |
| UI | `ui/session.js` / `ui/styles.css` | `gateHtml` 渲染验证/复核小节、`statusLabel`/`gateStatus`、`appendGate` 实时更新、样式 |
| 守门 | `scripts/ui-smoke.js` | U3 增 verify/reflect 双向断言；新增 **U17 verify-gate**、**U18 reflect-clean-context** |

**验收（真实执行）**

- `cargo test`：引擎 **194 passed**（新增 15 条：门禁四态 / 汇总不造假 / lint 映射 / 暂存语法层 / 复核解析容错 / 只读无写路径 / 降级 / 取消）
- `cargo run -p harness-engine --example agent_loop_smoke`：三臂 **21 项断言全绿**（真跑 python 语法检查 + unittest）
  - arm 1 正常路径：窄层→全量通过→复核，且断言**复核请求里没有主循环的轨迹**
  - arm 2 验证失败被拦：`["narrow:passed","full:failed","narrow:passed","full:passed"]`，打回信息含验证报告
  - arm 3 预算用尽：放行但答复含"按预算放行 + 未通过"
- `node scripts/ui-smoke.js`：**58/58**；`node scripts/check-style.js` PASS；`cargo fmt --check` / `cargo clippy --all-targets` 0 warning
- `cargo test -p ruyix live_mcp_connect -- --ignored`（connect 通路，v0.3 未改）

**与方案的偏差（据实记录）**

1. **窄层统一成"暂存内容单文件语法检查"**，确认/写入两种策略共用一套实现（作用在覆盖层内容上，天然不碰项目磁盘）。方案里提过的 `verify::run_syntax`（在项目上跑整包语法）**没有采用**：会给用户仓库留构建产物，且代价远大于需要。顺带把产物目录挪到 `.ruyix/` 下。
2. **P1 的"临时副本档"未实现**：本版确认模式只有静态层。要全量验证就切写入/自主模式；跳过时答复与验证小节都会写明原因。
3. 全量验证超时**沿用** `verify.cmd_timeout_secs` / `test_timeout_secs`（120/300），没有新增 300s 键；只有暂存层有独立超时 `gate.staged_timeout_secs`。
4. 复核**不给 execute**（按推荐只给 read）：验证命令归机械验证层，复核只需读文件。
5. 达到轮次上限 / 用户取消时**不做强制验证**（预算语义已在 `Final` 分支处理）；取消时答复写明已完成轮数与变更数。
6. 顺手修了 `done` 事件文案（原来"N 轮 · N 次工具调用"同一个数重复且"轮"不准确）→ "工具调用 N 次 · 变更 M 个 · 验证 X 次 · 复核 Y 次"。

**未做（留待第二阶段）**

- **对照实验 A/B/C**（用户实验纪律要求的三臂对照）：本版给的是三臂 e2e（功能正确性），"反思相对机械验证的纯增益"还没测。建议在删分类器/规划模式之前先跑一次，用数据决定反思的 rubric 是否要改。
- §7 的删除清单**一行未动**（分类器、规划模式、旧流水线都还在原位）。
