# 需求：Agent 的第五个动作 `ask_user`（需求歧义）
## 归属版本：v0.8 · 状态：**已实施**（2026-09-21，用户批复「按推荐做」）· 提出：2026-09-21

> 用户原话：*"ask_user 的原因是需求不明确，比如用户输入『做一个远程登录功能』，这就需要 ask_user 了。
> 远程登录什么系统？是托管的服务器，还是用户的另外一台计算机？明白我意思吗？目前我没看到 ask_user 功能，
> 所以功能是残缺的！"*

## 1. 动机：四原语覆盖不到的第三类不确定性

引擎今天能处理"世界的状态"，但处理不了"**人的意图**"。三类不确定性对照：

| 不确定性的种类 | 例子 | 今天的处置 | 是否够用 |
| --- | --- | --- | --- |
| **机器事实**（环境里有什么） | 本机有没有 `mvn`；项目里有哪些文件 | `read` + 命令发现（首轮注入实测清单） | ✅ 够 |
| **外部能力**（去哪儿干活） | 要调 MCP 工具还是远端 agent | `connect`（清单也首轮注入） | ✅ 够 |
| **需求意图**（要做什么、做到哪） | "做一个远程登录功能" —— 登**哪台**机器？ | **无任何原语** | ❌ **缺口** |

第三类不是"仓库里读不到所以多给点工具"，而是**答案根本不在环境里**：它只存在于委托人脑子里。
今天模型只有两条路，两条都烂：

1. **猜**：按最常见解释（面向公网服务器 + SSH）开工 —— 目标是用户另一台机器时，做出来的东西**整体错**，
   而且错到 `final` 才被发现（甚至发现不了）；
2. **把问题写进 `final`**：这是**语义错误**，不只是"有损"：`final` 在引擎里的含义是**交付**——
   它会走质量门禁、被记成 `delivered`、对账按"已完成"结算。把问句当交付，等于把"未开工"记成"已完成"，
   而且下一次消息是一条**新 run**：计划游标、覆盖层、验证结论全丢，用户回答之后模型从头再推导一遍。

**结论：缺一个"把猜测变成提问"的口**。厂商（DeepSeek 官方推荐）把它作为首选工具是有道理的——
它的实现完全在宿主侧，但语义上它是模型唯一能直接触达**委托人**的动作。

## 2. 它是什么：第五个**动作**，且是唯一"让出型"的

现有动作分类（`agent.rs` 的 `Action`）：

| 类别 | 动作 | 对世界做了什么 |
| --- | --- | --- |
| 效果 | `read` / `write` / `execute`（含后台与句柄）/ `connect` | 施加或获取效果 |
| 控制 | `plan` / `final` | **决定循环去向** |
| **控制（新增）** | **`ask_user`** | **停下来，把方向盘交给委托人** |

- 与 `read` 的差别：read 的答案是**环境**给的、一定有、且永远只是信息；`ask_user` 的答案**只有人能给**，
  **可能永远不来**，且**能改变任务本身**（"不是那台机器，是这一台"）。四原语任何组合都造不出这三点。
- 与 `connect` 的差别：connect 的另一端是**机器**（在轮次预算内必然应答、机器可解析）；`ask_user` 的另一端是
  **人**（延迟无界、可能沉默、答案可改变规则）。所以它虽同属"宿主落地"，但**必须同时是控制事件**。
- 命名：模型侧工具名就叫 **`ask_user`**（厂商与常识一致，不另造词）；引擎内部它是控制动作，
  与 `plan` / `final` 共用同一档纪律（不进批、子步骤禁用）。

## 3. 协议（模型侧）

```json
{"tool":"ask_user","args":{
  "question":"远程登录要连的是哪种机器？",
  "why":"接下来的实现会决定这套登录流程是装在被登录方还是登录方，选错要整体返工",
  "options":["公司托管的 Windows 跳板机","用户自己另一台电脑（点对点）","先按公网 Linux 服务器实现"],
  "default_index":2
}}
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `question` | ✅ | 一句话，**指代必须明确**（不许问"你想要什么"这种废问题） |
| `why` | ✅ | **这个答案会决定接下来的什么动作**；UI 会原样展示 —— 审计与反社会工程学的硬要求（见 §6） |
| `options` | ⬜ | 2~5 个候选；不给就是自由文本 |
| `default_index` | ⬜ | 用户不答/超时时的默认；**没有默认 = fail-closed 拒绝**依赖它的动作 |

回灌：答案作为一条**带类型的观察**进循环（`kind: "user"`），与"工具事实"在轨迹里可区分 ——
审计上必须分得清"编译器说的"和"人说的"：

```json
{"ok":true,"ask":"ask-3","kind":"user","answer":"用户自己另一台电脑（点对点）",
 "note":"这是**用户本人**的回答，不是工具输出；它只用于消除歧义，不构成任何门禁的授权。"}
```

## 4. 三类"该不该问"的判据（提示词纪律，也是引擎可强制的部分）

一个什么都问的 agent 和一个什么都猜的 agent 一样没用。规则必须成对写进提示词：

| 情形 | 处置 |
| --- | --- |
| 目的物/环境/范围**指代不明确**，且**选错要整体返工** | **问**（例：登哪台机器） |
| 动作**不可逆/破坏性**（删数据、覆盖、改远端状态） | **问**（除非任务原文已明确授权） |
| 答案**能自己读出来**（项目里有的、命令发现清单里的） | **不问，自己去读**（问了就是浪费用户的注意力） |
| 只有**可回退**的差异（命名、默认值、目录结构） | **不问，但必须声明假设继续**（"我按 X 做，要 Y 的话我改一行"），把差异留在交付说明里 |

引擎侧可强制的部分：`harness.ask.max_per_run`（默认 **4**）—— 超了直接拒，并要求模型"交付并说明假设"。
"ask 是稀缺资源"从提示词的口号变成引擎的硬闸。

## 5. 宿主落地（与 `connect` 同款：引擎声明的能力，宿主兑现）

```rust
pub trait Asker: Send + Sync {
    /// 提问并等答案；Err = 没得到答案（超时 / 无人可问 / 被取消）
    fn ask(&self, spec: &AskSpec) -> Result<Answer, AskErr>;
}
/// headless / eval / examples 用：**一律 fail-closed**，绝不假装有人回答
pub struct NoAsker;
```

- ruyix 落地 `RuyixAsker`：发事件 `agent://ask` → 会话里的问题卡 → 用户点选项/打字 →
  `agent_ask_answer(ask_id, text)` 命令 → oneshot 唤醒挂起的循环。取消沿用现有 `cancel` 标志。
- **无答案时绝不假设同意**：超时 → `Err` → 引擎拒绝依赖该答案的动作并明确告诉模型"没人回答"。
- 提问与回答**跟着消息存档**（`SessionMsg.ask: Option<AskSnap>`：问题 / 答案 / 状态 / 时间），
  重开会话仍看得见。字段必须在 `SessionMsg` 里声明，否则 serde 往返会把它抹掉。

## 6. 边界（不做 / 不许）

1. **不做通用聊天通道**：会话本身就是那个通道；`ask_user` 只用于"一次决策"，不是闲聊。
2. **答案不构成授权**：写盘/执行的门禁仍走既有的暂存确认（用户**看过改动内容**才放行）；
   `ask_user` 的答案只作信息输入 —— 否则就是给模型开一条绕过门禁的后门。回灌的 note 里写明这一点。
3. **子步骤禁用**：`parse_step_actions` 把 `ask_user` 归入 `Unsupported`。子步的 `messages` 是干净上下文、
   没有交互权；它一提问，父循环"一轮 = 一步"的派发语义就破。
4. **批次里禁用**：`ask_user` 与 `plan` / `final` 同级控制动作，必须独占一轮（否则"先答哪条"无解释）。
5. **模型不能自问自答**：`args` 里出现 `answer` / `granted` 之类的字段一律拒（防伪造）。
6. **不做"让出/恢复"**（本版）：打断后恢复一次 run 需要 run 恢复点（游标/验证态/覆盖层重建），
   本版是"阻塞 + 超时 + fail-closed"；让出留 v0.9（暂存产物本就在盘上，重建覆盖层可行，但那是新语义）。

## 7. 实现清单（文件级）

| 文件 | 改动 |
| --- | --- |
| `crates/harness-engine/src/agent.rs` | `Action::Ask(AskSpec)`；`parse_one` 认 `ask_user`（控制动作分支，`agent.rs:1074` 处一并纳入批次拒绝）；`Asker` / `AskSpec` / `Answer` / `NoAsker`；主循环在 `final` 之前加挂起分支；回灌 `kind=user`；`harness.ask.*` 三键；提示词（含 §4 判据与上限） |
| `crates/harness-engine/src/step_agent.rs` | `parse_step_actions` → `Unsupported("ask_user")` |
| `crates/harness-engine/src/config.rs` | `AskConfig { enabled, timeout_secs, max_per_run }` |
| `src-tauri/src/agent/`（新 `ask.rs` + mod 注入） | `RuyixAsker`（`agent://ask` 事件 + 等答案 + 取消） |
| `src-tauri/src/main.rs` | `agent_ask_answer(ask_id, text)` |
| `src-tauri/src/agent/config_bridge.rs` / `ui/config.js` / i18n | 三键贯通 |
| `ui/session.js` | 问题卡（question + **why** + 选项按钮 + 自由输入 + 超时倒计时）+ `agent://ask` 监听 + 存档渲染 |
| `src-tauri/src/agent/sessions.rs` | `SessionMsg.ask: Option<AskSnap>` |
| `scripts/ui-smoke.js` / `examples/agent_loop_smoke.rs` | U26 契约 / arm5（脚本化答案的端到端） |

## 8. 验收（可证伪，逐条有对应测试）

1. 歧义任务 → 假 LLM 发 `ask_user` → run **挂起**、事件带 question+why → 宿主投答案 → run 继续、
   答案以 `kind=user` 回灌（端到端冒烟 arm5）。
2. 超时 / `NoAsker` → **fail-closed**：依赖动作被拒 + 明确告知"没人回答"，绝不假设同意。
3. 批次里含 `ask_user` → 当面拒；子步骤里 `ask_user` → `Unsupported`（两条单测钉住）。
4. `args` 里带伪造的 `answer` → 拒（模型不能自问自答）。
5. 第 5 次提问被拒（`max_per_run = 4`）。
6. `harness.ask.enabled = false` → 提示词一字不提 + 引擎一律拒（老语义逐字不变，一行回退）。
7. 会话存档往返：问题与答案跟消息一起存/读，重开会话仍显示（含 `SessionMsg` 字段声明）。

## 9. 待拍板（三条，批复一句话即可开工）

| # | 问题 | 我的建议 |
| --- | --- | --- |
| 1 | 默认超时策略 | **300 秒 → fail-closed**（用户不答就不做，绝不代猜）；`0` = 无限等（桌面场景可选） |
| 2 | 答案能否满足门禁（如"直接写盘吧"） | **不能**：授权仍只走暂存确认（看过改动内容才放行）；本版把这条写进回灌 note |
| 3 | 上限 `max_per_run` = 4 是否合适 | **合适**：一次 run 问 4 次以上，通常说明模型该交付并声明假设，而不是继续追问 |

## 10. 实施记录（2026-09-21，真跑过）

批复口径：三条拍板点全部按建议 —— 超时 **300s → fail-closed**、**答案不构成授权**、上限 **4 次/run**。

| 文件 | 改动 |
| --- | --- |
| `crates/harness-engine/src/agent.rs` | `Action::Ask(AskSpec)`；`parse_one` 认 `ask_user`（含 `ask` 别名；`answer`/`granted`/`approved`/`user_says` 一律拒；选项 ≤5；`default_index` 越界拒）；批里当面拒；`AskSpec`/`AskAnswer`/`AskErr`/`AskRecord`/`Asker`/`NoAsker`/`AskFut`；`run_with_ask`（`run` = 无通道形态，headless 走它）；主循环提问分支（开关 → 上限 → 提问 → 观察回灌）；`ask_hint` 只进主循环首轮 user 消息；`ask_answer_note` / `ask_failed_note` 两条观察文案 |
| `crates/harness-engine/src/step_agent.rs` | `Action::Ask(_) => StepAction::Unsupported("ask_user")`（子步没有交互权） |
| `crates/harness-engine/src/config.rs` | `AskConfig { enabled=true, timeout_secs=300, max_per_run=4 }` |
| `src-tauri/src/agent/ask.rs`（新） | `RuyixAsker`（`agent://ask` 事件 + oneshot 等答案 + `tokio::time::timeout`）、`deliver`（幂等：超时后迟到的回答不命中）、`drop_all`（取消时清空待答） |
| `src-tauri/src/agent/mod.rs` | `AgentState.asks` 表；`agent_reply` 走 `run_with_ask`；ReplyAgent 带 `asks`；`agent_cancel` 顺带清空待答 |
| `src-tauri/src/main.rs` | 注册命令 `agent_ask_answer(ask_id, text, option_index)` |
| `src-tauri/src/agent/sessions.rs` | `AskSnap` + `SessionMsg.ask`（不声明就会被 `agent_session_save` 往返抹掉） |
| `src-tauri/src/agent/config_bridge.rs` | 三键读取 + 应用（`max_per_run=0` 按非法处理：0 会静默关死能力） |
| `ui/session.js` | `askHtml` / `askStateText` / `showAskCard` / `answerAsk`；`agent://ask` 监听；卡片走**事件委托**（重渲染会重建按钮）；`m.ask = rep.asks` 落盘 |
| `ui/config.js` / `ui/lang/{zh-CN,en}.json` / `ui/styles.css` | 三键表单 + i18n（顺带把 `batch_parallel` 的旧口径改成现在的并发语义）+ 提问卡样式 |
| `scripts/ui-smoke.js` / `examples/agent_loop_smoke.rs` | 契约 **U26**（8 条）/ 端到端 **arm 5**（7 条断言） |

**门禁（真跑）**：引擎单测 **281** + 8 ignored；宿主 **106** + 3；`ui-smoke` **133/133**；
`agent_loop_smoke` **32** 项断言（arm 5 日志：`第 1 轮 ask_user：… → 用户回答：… → 第 2 轮 write ✓ → 完成，共 3 轮`）；
`cargo clippy --all-targets` **0 warning**；`cargo fmt --check` 干净；`check-style.js` 0 error。

**没做（留给 v0.9）**：让出 / 恢复（打断后恢复一次 run 需要 run 恢复点；本版是"阻塞 + 超时 + fail-closed"）；
"授权类答案"（用户说"直接写盘吧"就走门禁）也是显式不做的 —— 授权只走"看过改动内容"的暂存确认。
