//! Agent 命令桥（融合计划 Z3 / P1 / P2）。
//!
//! 职责：Tauri 命令 → 引擎调用 → `agent://*` 事件流。引擎能力全部来自
//! `crates/harness-engine`（零 tauri 依赖），本模块只做三件事：
//! - config_bridge：ruyix 配置 → 引擎 AppConfig
//! - AgentSink：引擎 Sink 回调 → Tauri emit
//! - AgentState：跨命令共享的取消位
//!
//! 状态与错误分离（附录 A 约定）：可预期状态（未配 Key 等）由引擎以
//! `Ok` + 状态字段返回；只有"答不了"才 `Err(String)`。

pub mod config_bridge;
pub mod sink;

use crate::config::ConfigManager;
use engine::pipeline::Sink as _;
use harness_engine as engine;
use serde::Serialize;
use sink::AgentSink;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

/// 引擎运行态。取消位跨命令共享：`agent_run` 在起跑时清零，`agent_cancel` 置位。
/// 单用户 IDE，不设计并发多 run；重复 `agent_run` 会重置取消位。
pub struct AgentState {
    cancel: Arc<AtomicBool>,
}

impl AgentState {
    pub fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Default for AgentState {
    fn default() -> Self {
        Self::new()
    }
}

type ConfigState<'a> = State<'a, Mutex<ConfigManager>>;

/// 读配置。锁只在同步段持有，绝不跨 `.await`（Send 约束）。
fn build_cfg(
    config: &ConfigState<'_>,
    project_root: Option<&str>,
) -> Result<engine::config::AppConfig, String> {
    let mgr = config.lock().map_err(|e| e.to_string())?;
    config_bridge::build_app_config(&mgr, project_root)
}

/// 锁当前配置管理器（agent_env_probe 之类需要多次读键的命令用）
fn lock_config<'a>(
    config: &'a ConfigState<'_>,
) -> Result<std::sync::MutexGuard<'a, ConfigManager>, String> {
    config.lock().map_err(|e| e.to_string())
}

// ============================================
// 事件驱动的主链路
// ============================================

/// 全流程：plan → generate → lint/verify → repair。事件流驱动 UI。
#[tauri::command]
pub async fn agent_run(
    app: AppHandle,
    state: State<'_, AgentState>,
    config: State<'_, Mutex<ConfigManager>>,
    task: String,
    project_root: Option<String>,
) -> Result<engine::workspace::RunRecord, String> {
    let cfg = build_cfg(&config, project_root.as_deref())?;
    state.cancel.store(false, Ordering::Relaxed);
    let sink = AgentSink::new(app);
    let opts = engine::pipeline::RunOpts {
        task,
        ..Default::default()
    };
    engine::pipeline::run(&cfg, state.cancel.clone(), opts, &sink).await
}

/// 只出规划（可编辑后走 agent_generate）
#[tauri::command]
pub async fn agent_plan(
    app: AppHandle,
    state: State<'_, AgentState>,
    config: State<'_, Mutex<ConfigManager>>,
    task: String,
    project_root: Option<String>,
) -> Result<engine::workspace::RunRecord, String> {
    let cfg = build_cfg(&config, project_root.as_deref())?;
    state.cancel.store(false, Ordering::Relaxed);
    let sink = AgentSink::new(app);
    let task = task.trim().to_string();
    if task.is_empty() {
        return Err("任务描述不能为空".into());
    }
    engine::pipeline::plan_stage(&sink, &cfg, &task, None).await
}

/// 按既有 run 的（可能被编辑过的）计划重新生成
#[tauri::command]
pub async fn agent_generate(
    app: AppHandle,
    state: State<'_, AgentState>,
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    plan_json: String,
    project_root: Option<String>,
) -> Result<engine::workspace::RunRecord, String> {
    let cfg = build_cfg(&config, project_root.as_deref())?;
    state.cancel.store(false, Ordering::Relaxed);
    let root = engine::config::runs_root(&cfg);
    let mut rec = engine::workspace::load(&root, &run_id)?;
    rec.plan = Some(
        serde_json::from_str::<engine::plan::Plan>(&plan_json)
            .map_err(|e| format!("计划 JSON 解析失败: {e}"))?,
    );
    let sink = AgentSink::new(app);
    engine::pipeline::generate_stage(&sink, &cfg, state.cancel.clone(), &mut rec).await?;
    Ok(rec)
}

/// 对既有 run 重跑验证（语法检查 + 单元测试）
#[tauri::command]
pub async fn agent_verify(
    app: AppHandle,
    state: State<'_, AgentState>,
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    project_root: Option<String>,
) -> Result<engine::verify::VerifyReport, String> {
    let cfg = build_cfg(&config, project_root.as_deref())?;
    state.cancel.store(false, Ordering::Relaxed);
    let root = engine::config::runs_root(&cfg);
    let mut rec = engine::workspace::load(&root, &run_id)?;
    let sink = AgentSink::new(app);
    engine::pipeline::verify_stage(&sink, &cfg, state.cancel.clone(), &mut rec).await
}

/// 对既有 run 重跑规约检查（自定义 lint 规则）
#[tauri::command]
pub async fn agent_lint(
    app: AppHandle,
    state: State<'_, AgentState>,
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    project_root: Option<String>,
) -> Result<engine::lint::LintOutcome, String> {
    let cfg = build_cfg(&config, project_root.as_deref())?;
    state.cancel.store(false, Ordering::Relaxed);
    let root = engine::config::runs_root(&cfg);
    let proj = engine::workspace::project_dir(&root, &run_id)?;
    let mut rec = engine::workspace::load(&root, &run_id)?;
    let sink = AgentSink::new(app);
    let outcome = engine::pipeline::run_lint(&cfg, proj).await;
    sink.lint(&outcome);
    rec.lint = outcome.report.clone();
    engine::workspace::save(&root, &rec)?;
    Ok(outcome)
}

/// 对既有 run 跑自我纠正循环（默认轮数取配置，可用 max_rounds 覆盖）
#[tauri::command]
pub async fn agent_repair(
    app: AppHandle,
    state: State<'_, AgentState>,
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    max_rounds: Option<u32>,
    project_root: Option<String>,
) -> Result<engine::workspace::RunRecord, String> {
    let mut cfg = build_cfg(&config, project_root.as_deref())?;
    if let Some(r) = max_rounds {
        cfg.lint.max_repair_rounds = r;
    }
    state.cancel.store(false, Ordering::Relaxed);
    let root = engine::config::runs_root(&cfg);
    let proj = engine::workspace::project_dir(&root, &run_id)?;
    let mut rec = engine::workspace::load(&root, &run_id)?;
    let sink = AgentSink::new(app);
    engine::pipeline::repair_stage(&sink, &cfg, &state.cancel, &mut rec, &proj).await?;
    Ok(rec)
}

/// 取消当前运行。返回置位前的状态（false = 当时没有在跑的 run）。
#[tauri::command]
pub fn agent_cancel(state: State<'_, AgentState>) -> bool {
    state.cancel.swap(true, Ordering::Relaxed)
}

// ============================================
// 历史与产物
// ============================================

/// 历史运行列表（newest-first）
#[tauri::command]
pub fn agent_runs(
    config: State<'_, Mutex<ConfigManager>>,
    limit: Option<usize>,
    project_root: Option<String>,
) -> Result<Vec<engine::workspace::RunSummary>, String> {
    let mgr = lock_config(&config)?;
    let cfg = config_bridge::build_app_config(&mgr, project_root.as_deref())?;
    let mut list = engine::workspace::list(&engine::config::runs_root(&cfg));
    if let Some(n) = limit {
        list.truncate(n);
    }
    Ok(list)
}

/// 加载一次运行的完整记录
#[tauri::command]
pub fn agent_run_load(
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    project_root: Option<String>,
) -> Result<engine::workspace::RunRecord, String> {
    let mgr = lock_config(&config)?;
    let cfg = config_bridge::build_app_config(&mgr, project_root.as_deref())?;
    engine::workspace::load(&engine::config::runs_root(&cfg), &run_id)
}

/// 删除一次运行的目录（含产物）
#[tauri::command]
pub fn agent_run_delete(
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    project_root: Option<String>,
) -> Result<(), String> {
    let mgr = lock_config(&config)?;
    let cfg = config_bridge::build_app_config(&mgr, project_root.as_deref())?;
    engine::workspace::delete(&engine::config::runs_root(&cfg), &run_id)
}

#[derive(Serialize)]
pub struct AgentFile {
    pub path: String,
    pub content: String,
}

/// 读一次运行的产物文件（路径安全性由引擎的 read_project_file 保证）
#[tauri::command]
pub fn agent_read_artifact(
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    rel_path: String,
    project_root: Option<String>,
) -> Result<AgentFile, String> {
    let mgr = lock_config(&config)?;
    let cfg = config_bridge::build_app_config(&mgr, project_root.as_deref())?;
    let root = engine::config::runs_root(&cfg);
    let content = engine::workspace::read_project_file(&root, &run_id, &rel_path)?;
    Ok(AgentFile {
        path: rel_path,
        content,
    })
}

// ============================================
// 环境探针
// ============================================

#[derive(Serialize)]
pub struct EnvReport {
    pub docker: engine::sandbox::Probe,
    pub python: engine::exec::Probe,
    pub node: engine::exec::Probe,
    pub git: engine::exec::Probe,
    pub sandbox_mode: String,
    pub llm_key_configured: bool,
    pub runs_root: String,
}

/// 探测执行环境（docker/python/node/git + LLM Key 是否已配）。
/// 探针全是真起子进程的阻塞调用，整体丢进 blocking 线程池。
#[tauri::command]
pub async fn agent_env_probe(
    config: State<'_, Mutex<ConfigManager>>,
    project_root: Option<String>,
) -> Result<EnvReport, String> {
    let cfg = {
        let mgr = lock_config(&config)?;
        config_bridge::build_app_config(&mgr, project_root.as_deref())?
    };
    let runs_root = engine::config::runs_root(&cfg);
    tauri::async_runtime::spawn_blocking(move || {
        let python = engine::exec::probe("python", &cfg.verify.python_bin, &["--version"]);
        let node = engine::exec::probe("node", &cfg.verify.node_bin, &["--version"]);
        let git = engine::exec::probe("git", "git", &["--version"]);
        let docker = engine::sandbox::probe(&cfg);
        EnvReport {
            docker,
            python,
            node,
            git,
            sandbox_mode: cfg.sandbox.mode.clone(),
            llm_key_configured: !cfg.llm.api_key.trim().is_empty(),
            runs_root: runs_root.to_string_lossy().to_string(),
        }
    })
    .await
    .map_err(|e| format!("探针线程异常: {e}"))
}
