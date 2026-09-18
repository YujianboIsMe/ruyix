//! harness-engine —— darkhorse-harness 的引擎层（融合计划 D1）。
//!
//! 零 tauri 依赖：GUI/命令桥通过 [`pipeline::Sink`] trait 注入事件回调，
//! 取消通过 `exec::CancelFlag`（`Arc<AtomicBool>`）。所有交互入口：
//! - [`pipeline::run`]：全流程 plan → generate → lint/verify → repair
//! - [`pipeline::plan_stage`]：只出规划（agent_plan）
//! - [`workspace`]：运行记录的落盘/加载/列举/删除
//! - [`verify::run`] / [`lint::run_lint`] / [`repair::propose`]：单阶段能力

pub mod config;
pub mod entropy;
pub mod eval;
pub mod exec;
pub mod generate;
pub mod gitops;
pub mod kb;
pub mod lint;
pub mod llm;
pub mod observe;
pub mod pipeline;
pub mod plan;
pub mod repair;
pub mod sandbox;
pub mod verify;
pub mod workspace;
