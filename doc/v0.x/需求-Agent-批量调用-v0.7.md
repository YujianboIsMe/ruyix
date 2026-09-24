# 需求：Agent 批量调用（一轮多个原语调用，v0.7）

状态：已实施（2026-09-21）
归属版本：v0.7（ruyix Agent 会话链路）
提出日期：2026-09-21（用户原话：*"react 循环里，让 LLM 一次返回多个原子命令调用，然后 ruyix 可以并发执行，
收集结果后回复 LLM，这样可以节省轮数"*；第二轮口径：*"要并发就一起并发"*）
关联：[融合计划-Agent集成-v0.2.md](融合计划-Agent集成-v0.2.md)、[需求-Agent-验证与反思-v0.3.md](需求-Agent-验证与反思-v0.3.md)、[execute.md](../execute.md)

## 1. 动机：一轮到底贵在哪

循环的一轮 = **一次模型往返 + 一次整份上下文的重新发送**。原先一轮只换回一个调用，于是：

- 「先读 5 个文件再决定」花 **5 轮**，其中 4 轮纯属排队；
- 每轮回灌的 `read` 最多 8K 字符、`execute` 4K，且全 run 不裁剪 —— 轮数越多，每轮要重发的历史越长，
  token 成本近似**平方**增长；
- 子步骤更吃紧：`step.max_steps` 当时只有 24 轮（v0.10 已提到 96，见
  `doc/v0.x/需求-Agent-子步预算-v0.10.md`），读文件很容易把预算吃掉。

本版做两件事：**模型一轮发多个互不依赖的调用**；**引擎把这一批并发跑**，结果一次性回灌。

## 2. 协议（模型侧）

单动作形状**不变**（历史里模型学过的形状不动），新增批形状：

```json
{"actions":[{"tool":"read","args":{"path":"a.rs"}},{"tool":"read","args":{"path":"b.rs"}}]}
```

- `calls` 是同一件东西的别名（模型两种都写过，认全了省一轮）。
- **控制动作不许进批**：`final`（完成）与 `plan`（清单）混进批里，"谁先谁后"没有合理解释 →
  解析期当面拒，并明确让它单独发一轮。**不替模型猜顺序**。
- **超上限不静默截断**：`batch_max`（默认 8）超了就把上限报回去让它拆批 —— 截断等于**丢调用**，
  而模型还以为发出去了。
- 批里坏的那条要点名是**第几个**（`actions 第 2 个解析失败：…`），否则模型不知道该改哪条。

## 3. 执行语义：整批并发，只有两种冲突保序

### 3.1 冲突判定（结构性，不猜命令语义）

一批动作先切成**执行波次**：波内并发、波与波之间按声明顺序。分层规则是贪心的 ——
每条落在"它所有冲突前驱的下一波"。互不相关的一串读、一串命令**只有一波**，那就是"一起并发"。

只有两种冲突（`conflicts`，只看原语与路径）：

| 冲突 | 为什么 |
| --- | --- |
| **同一条路径上的写**（`write` 与该路径的 `read`/`write`） | 覆盖层是"写完立刻读回来"的依据；同文件两次写的先后也是模型表达的意思 |
| **托管进程的生命周期操作**（`background` 起 / `status`·`log`·`stop`） | 那张进程表是引擎自己持有的共享状态，句柄又由引擎赋值 —— 同批里"起完再查"必须保序 |

**其它一律并发**：`execute` 之间共享什么（构建缓存、锁、端口）引擎不知道，凭命令文本猜就是
app 知识泄漏（同"就绪判据是一条命令"的原则）—— 这属于**模型的依赖声明**，提示词已写明
"同一批并发跑，只有这两种情况会自动排序，有别的依赖就分两轮发"。

例：`[read a, read b, write a, read a]` → `[[0,1],[2],[3]]`（两次读并发；写 a 等读 a；
最后一个读 a 等写 a）。`[read, write, read, execute]` → `[[0,3],[1],[2]]`（execute 与它俩都不冲突，
并进第一波）。

### 3.2 波内怎么并发

| 原语 | 落地方式 |
| --- | --- |
| `read` | `read_group`：scoped 线程借 `&Ctx`（同步文件 I/O，不占 runtime；也不要求多线程 runtime —— 单测跑在 `new_current_thread` 上，`block_in_place` 在那儿会 panic） |
| `write` | **拆两段**：磁盘阶段（备份 + 落盘）在 scoped 线程里并行，记账（覆盖层 / 变更表）回主线程按声明顺序补 —— 内存状态只有一份，不能并发改 |
| `execute` / 托管进程 | 每个 scoped 线程自己起一个进程（这些函数只吃 `&Path` 与 `&AppConfig`，与 `Ctx` 无关） |
| `connect` | 同一任务里 [`JoinAll`]：future 借 `&dyn Connector`（非 `'static`，spawn 装不下），也不引 `futures` 依赖，30 行等价物 |

波内没有依赖，所以"谁先谁后"无所谓（只读先跑完再起写/执行，纯粹是为了复用同一份并发读实现）；
**只有波与波之间按声明顺序**。结果一律按**声明顺序**落回槽位 —— 顺序错位是静默故障
（模型会拿 B 文件的内容当 A 的依据，且无从察觉）。

写入拆段的三个新原语（并发路径与单动作路径**共用同一份**实现）：

- `Ctx::before_of(&self, rel)` —— 取被覆盖前的原内容（覆盖层优先）；
- `flush_write_disk(proj, backup_dir, rel, before, after)` —— 真正落盘的那一段（纯 `&Path`，可并发）；
- `Ctx::record(&mut self, rel, before, after)` —— 只动内存的记账。

`Ctx::tool_write` = 这三步顺序调一遍（老语义逐字不变），并发波只是把"磁盘那一段"搬到线程里。

### 3.3 结果回灌

批：一条 user 消息，`results` 数组按声明顺序逐条给，**每条带自己的 `ok`**：

```json
{"ok": true, "results":[{"index":1,"tool":"read","call":"read a.rs","ok":true,"result":"…"}],
 "note":"以上是同一轮里的多个调用（一批并发跑；只有同一条路径上的写与读、以及托管进程的起停查会按你给的顺序排），results 按你声明的顺序排列。"}
```

顶层 `ok` 只是"全成"的汇总 —— 拿它当"这轮有没有事"的判据会**漏掉单条失败**。
单动作仍走老形状 `{"ok":…, "result":…}`。

### 3.4 与既有机制的关系（一处都没绕过）

- **窄层机械验证**：批里有写 → 收尾按老判据跑一次（`any_write_ok`，与单动作同一实现）。
- **进度对账** `emit_step_progress`：判据从"这一轮是 write"改成"这一轮里有写成功"。
- **证据集合** `read_paths`：批里每条成功的 read 都进（复核员看的就是这份清单）。
- **轨迹与日志**：批里每条**各占一行**、**轮号相同**（`第 1 轮 [2/3] read ✓ …`），末尾再加一行
  `一批 N 个调用（M 波，同波并发 K 条）`—— 省下来的轮在日志里看得见；单动作时日志格式与老版本**逐字一致**。
- **门禁 / 复核 / 会话存档**：全在批之外，不感知批的存在。

## 4. 父子两份提示词各自自洽

批协议必须**写进提示词**（模型不会凭空发明一个没见过的字段名 —— 同 `keep_alive` 那次教训），
但父子两份要各自自洽：

- 主循环：`batch_hint(max, has_plan = true)`；
- 子步骤：`batch_hint(max, has_plan = false)` —— **不提 `plan`**。`STEP_SYSTEM` 里没有清单工具，
  提了模型会去找一个不存在的能力（由测试钉住：`!batch_hint(8, false).contains("plan")`）。

共用件（不许在父子两处各长一遍）：

| 件 | 作用 |
| --- | --- |
| `parse_actions` / `parse_one` | 唯一解析入口；`parse_step_actions` 只做收窄（connect/plan → Unsupported） |
| `conflicts` / `waves_by` / `shape_of(_step)` | 唯一冲突判定与分层内核（父子两种动作都映射成 `Shape`）；`batch_waves` / `batch_waves_for_step` 只是取形状的壳 |
| `read_group` | 唯一并发读实现 |
| `flush_write_disk` + `Ctx::before_of` / `record` | 唯一写入实现（磁盘段可与记账段分开调） |
| `step_exec_one` / `exec_one` | 单条执行体：单波单条与并发波共用 |
| `json_result` / `batch_json_result` + `CallResult` | 唯一回灌形状（子 agent 原先的本地副本已删） |

**开关与接受判定绑在同一个开关上**：`batch = false` 时提示词一个字都不提，引擎也一律拒批 ——
提示词不许广告一个引擎会拒的形状（"不虚报能力"）。`batch_parallel = false` 则保留"一批一次往返"
但**波内串行**（一行回退到老时序，便于排查并发问题）。

## 5. 配置（三键，都能一行回退）

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `ruyix.code.harness.agent.batch` | `true` | 关掉即回到"一轮一个调用"的老协议 |
| `ruyix.code.harness.agent.batch_max` | `8` | 单批上限；非法值（0 / 负数 / 非数字）保持默认 —— 0 会把所有批都拒掉，等于把功能悄悄关死 |
| `ruyix.code.harness.agent.batch_parallel` | `true` | 波内是否并发（关掉=一批仍一次往返，但按声明顺序串行） |

贯通链路：引擎 `AgentConfig` → 宿主 `config_bridge` → `ui/config.js` 表单 → i18n（U10 契约要求）。

## 6. 验证（都是真跑出来的）

| 项 | 结果 |
| --- | --- |
| 引擎单测 | **274** passed + 8 ignored（本版 +10） |
| 宿主单测 | **103** passed + 3 ignored（+2：配置桥三键） |
| 端到端冒烟 | `examples/agent_loop_smoke.rs` **arm 4**：25 项断言全绿 |
| 静态契约 | `ui-smoke.js` **U25 batch-calls**（125/125） |
| 门禁 | `cargo clippy --all-targets` 0 warning / `cargo fmt --check` 干净 / `check-style.js` 0 error |

**"并发是真的"这条单独用对照臂证明**（`two_commands_in_one_batch_really_run_in_parallel`）：
同样两条 2 秒命令，一轮发一条（串行）vs 一批发两条（并发），断言 `3×并发 < 2×串行` ——
用**相对判据**而不是绝对秒数，所以慢机器、全套测试并跑抢 CPU 时同样成立（两条臂被一起拉长）。
谁把 execute 挪回串行，这条立刻变红。

端到端冒烟的日志原文（`cargo run -p harness-engine --example agent_loop_smoke`）：

```
[info] [agent] 第 1 轮 [1/3] read ✓ read a.txt
[info] [agent] 第 1 轮 [2/3] read ✓ read b.txt
[info] [agent] 第 1 轮 [3/3] read ✓ read c.txt
[info] [agent] 第 1 轮 一批 3 个调用（1 波，同波并发 3 条）
[ok] [agent] 完成，共 2 轮          ← 改前是 4 轮（3 次 read + 1 次 final）
```

## 7. 本版不做 / 已知边界

- **不猜测命令之间的依赖**：同一批里的 `execute` 是并发跑的。两条命令互相依赖（先 build 后 test、
  抢同一个端口、拿上一条的输出当参数）必须由**模型分两轮发** —— 提示词已把这条纪律写明。
  引擎知道的是"哪些原语与路径冲突"，不知道也不该知道 `mvn` / `cargo` / `git` 在做什么。
- **不做"并行 LLM 调用"**：那是并行 agent（另开上下文），与"一轮多调用、省同一份上下文"是两件事。
- **不改 `MAX_STEPS` 语义**：轮数上限仍是 96，只是每轮能装的活变多。

## 8. 实施记录（2026-09-21）

| 文件 | 改动 |
| --- | --- |
| `crates/harness-engine/src/agent.rs` | 批协议（`parse_actions`/`parse_one`）、波次模型（`Shape`/`conflicts`/`waves_by`/`batch_waves`）、并发执行（`run_wave`/`read_group`/`JoinAll`、`exec_one`）、写入拆段（`Ctx::before_of`/`ensure_backup_dir`/`record` + `flush_write_disk`/`write_ok_text`）、回灌（`json_result`/`batch_json_result`/`CallResult`）、`connect_brief`/`connect_future`、提示词 `batch_hint` |
| `crates/harness-engine/src/step_agent.rs` | 子步骤同款（`parse_step_actions`/`batch_waves_for_step`/`step_exec_one`/`run_step_wave`），删掉本地 `json_str`/`json_result` 副本 |
| `crates/harness-engine/src/config.rs` | `AgentConfig` 三字段 + 默认值 |
| `crates/harness-engine/examples/agent_loop_smoke.rs` | arm 4：真实循环里一批 3 个 read = 1 轮 |
| `src-tauri/src/agent/config_bridge.rs` | 三键读取与应用（+2 测试） |
| `ui/config.js` / `ui/lang/zh-CN.json` / `ui/lang/en.json` | 配置表单三字段 + i18n |
| `scripts/ui-smoke.js` | U25 batch-calls（7 条静态契约） |

新增测试（引擎）：`a_batch_parses_in_declared_order_and_rejects_control_actions`、
`an_oversized_batch_is_refused_with_the_cap`、`a_batch_is_refused_when_the_switch_is_off`、
`waves_parallelize_everything_except_conflicts`、`parallel_reads_land_in_declared_order`、
`a_read_batch_costs_one_round_and_returns_every_result`、
`a_write_followed_by_a_read_in_one_batch_keeps_order`、
`two_commands_in_one_batch_really_run_in_parallel`、`the_batch_hint_follows_the_switch`、
`a_step_can_read_a_batch_in_one_round`。
