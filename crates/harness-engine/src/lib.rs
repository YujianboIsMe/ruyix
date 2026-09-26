//! harness-engine —— darkhorse-harness 的引擎层（融合计划 D1）。
//!
//! 零 tauri 依赖：GUI/命令桥通过 [`pipeline::Sink`] trait 注入事件回调，
//! 外部能力（MCP / A2A…）通过 [`agent::Connector`] trait 注入连接实现，
//! 取消通过 `exec::CancelFlag`（`Arc<AtomicBool>`）。所有交互入口：
//! - [`agent::run`]：编程 Agent 工具循环（Read / Write / Execute / Connect 四大原子能力）
//! - [`step_agent::run_step`]：计划步骤的执行体（子 agent，上下文干净、只做一步）
//! - [`pipeline::run`]：全流程 plan → generate → lint/verify → repair
//! - [`pipeline::plan_stage`]：只出规划（agent_plan）
//! - [`workspace`]：运行记录的落盘/加载/列举/删除
//! - [`verify::run`] / [`lint::run_lint`] / [`repair::propose`]：单阶段能力
//! - [`discover`]：命令发现（把"本机有什么命令"实测出来喂进模型上下文）
//! - [`debug`]：详细调试日志（`--debug` 启动开关：完整提示词 + 模型原始响应，落盘）

pub mod agent;
pub mod config;
pub mod debug;
pub mod discover;
pub mod entropy;
pub mod eval;
pub mod exec;
pub mod generate;
pub mod gitops;
pub mod kb;
pub mod lint;
pub mod llm;
pub mod mem;
/// 模型缓存（自检 + 按需下载 + 逐文件校验）：记忆与语音共用一份机制
pub mod modelstore;
pub mod observe;
pub mod pipeline;
pub mod plan;
pub mod proc;
pub mod reflect;
pub mod repair;
pub mod sandbox;
pub mod step_agent;
pub mod testllm;
pub mod verify;
/// 本地语音转写（candle whisper + 按需下载的量化权重）
pub mod workspace;
