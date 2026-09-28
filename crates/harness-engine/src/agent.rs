//! 编程 Agent 的工具循环：Read / Write / Execute / Connect 四大原子能力。
//!
//! 为什么放弃"先分类问答/任务再走两条链路"：边界根本划不清 ——
//! 「帮我理解这个项目」要读代码才能答，「修一下这个报错」要读文件才知道怎么改。
//! 正确形态是一个循环：模型自己决定下一步用哪个能力（看结构 → 读文件 → 写回 → 跑测试），
//! 直到给出最终回答。能力集刻意只有四条原语，不搞一堆专项工具：
//!
//! - **Read**    读项目结构与文件内容（可给 `offset`/`limit` 只取一段；Git 历史经 Execute 的
//!   `git log` / `git show` 达成）
//! - **Write**   写文件，两种参数形状：`content`（整份，新建/大改）或 `edits`（锚点替换，
//!   改既有文件的主力）。**能力只有一条** —— 变的只是"要传多少东西"，见 [`WriteBody`]
//! - **Execute** 跑系统命令（工作目录钉在项目根、超时、输出裁剪；破坏性模式拒绝执行）
//! - **Connect** 连外部能力：MCP 服务器上的工具、A2A 远端 Agent（[`Connector`]）
//!
//! `plan` 不是第五种能力，只是一条给用户看的任务清单通道（经 `sink.plan` 推给大纲区，
//! 步骤文件全部落地后标 ✅、部分落地 ⛏️）。
//!
//! Connect 的落地端在宿主：引擎（零 tauri）只定 [`Connector`] 契约 —— 连什么是宿主环境的事
//! （子进程、HTTP、三 scope 配置），ruyix 侧用 MCP 客户端 + A2A 客户端实现，引擎搬到别的宿主
//! 时接别的实现即可。宿主没接任何外部能力时用 [`NoConnector`]（清单为空 → 提示词里不出现连接段）。
//!
//! 写入策略（[`WritePolicy`]）由调用方定：Stage = 只暂存到**项目状态根**（`<便携根>/projects/<项目 key>/stage/`），
//! UI 确认后才落盘；Apply = 直接写进项目（被覆盖文件先备份到同一状态根的 `backups/`）。
//! 两者都**不写用户仓库**（v1.0.0：IDE 状态属于 IDE，见 `doc/需求-便携形态与零残留-v1.0.0.md`）。

use crate::config::AppConfig;
use crate::discover;
use crate::exec::{self, CancelFlag, clip, is_cancelled};
use crate::generate::{StepOutcome, safe_rel_path};
use crate::lint;
use crate::llm::{self, ChatMessage, ImagePart, Usage};
use crate::pipeline::Sink;
use crate::plan::{Plan, PlanStep};
use crate::reflect::{self, Reflection};
use crate::repair;
use crate::step_agent::{self, StepInput, StepReport, StepSummary};
use crate::verify;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

// ============================================
// 主循环
// ============================================

/// 跑一次 Agent 工具循环。
/// `conn` 接 Connect 原语（宿主实现 MCP / A2A 等外部能力），没接就用 [`NoConnector`]。
/// `cancel` 是所有长动作（编译 / 测试 / 复核）的取消路径。
// 八项都是调用方已就绪的事实（配置、项目、任务、历史、策略、连接器、取消位、事件出口）；
// 打包成结构体只是把参数换个地方列一遍，同一先例见 `verify` 的 `run_check`。
#[allow(clippy::too_many_arguments)]
/// 工具循环（**没有提问通道**的形态）：headless / eval / 冒烟 / 管线用。
///
/// 没有 `Asker` 时 `ask_user` 一律 fail-closed（拒绝依赖它的动作），**绝不假装有人回答** ——
/// 与 `NoConnector` 同款纪律。宿主（会话）走 [`run_with_ask`]。
pub async fn run(
    cfg: &AppConfig,
    proj: &Path,
    task: &str,
    history: &[HistoryMsg],
    policy: WritePolicy,
    conn: &dyn Connector,
    cancel: &CancelFlag,
    sink: &dyn Sink,
) -> Result<AgentOutcome, String> {
    // 多模态：规划管线（`agent_run` / `agent_plan`，走 `pipeline`）没有附件入口 ——
    // 附件属于**会话**那条路（`agent_reply` → `run_with_ask` 带 images）。
    run_with_ask(
        cfg,
        proj,
        task,
        history,
        &[],
        policy,
        conn,
        &NoAsker,
        cancel,
        sink,
    )
    .await
}

// 循环本体在 `agent/tool_loop.rs` —— 那 780 行搬出去之后，这个文件回到 3.5k 量级。
// 子模块用 `use super::*` 就能看到这里的一切（含私有项），所以是文件切分、不是可见性改造。
// ---------------------------------------------------------------------------
// 子模块：`agent.rs` 到此为止只是**门面 + 主循环**，实现按域住在下面这些文件里
// （2026-09-28 拆的；对外路径 `agent::X` 靠下面的再导出保持不变）。
// ---------------------------------------------------------------------------
mod action;
pub use action::*;
mod connect;
pub use connect::*;
mod dispatch;
pub(crate) use dispatch::*;
mod gate;
pub use gate::*;
mod prompt;
pub use prompt::*;
mod tools;
pub use tools::*;
mod types;
pub use types::*;

pub use findings::Progress;

mod tool_loop;
pub use tool_loop::run_with_ask;

/// 去重账本（v1.2 P2）：纯工具 + 资源版本未变 ⇒ 同一次调用不许执行第二次。
pub mod capsule;
/// 进展记忆与循环守卫（`record_findings` + 引擎账本 + 两条守卫）
///
/// 见 `doc/v1.1/需求-Agent-进展记忆与循环守卫-v1.1.md`。与 `tool_loop` 一样是**文件切分**，
/// 子模块用 `use super::*` 看到这里的一切。
pub mod context;
pub mod findings;
mod keys;
pub(crate) use keys::*;
pub mod ledger;
pub mod scheduler;

#[cfg(test)]
mod tests;
