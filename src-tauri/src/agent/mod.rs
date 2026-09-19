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

pub mod apply;
pub mod config_bridge;
pub mod connect;
pub mod project_context;
pub mod sessions;
pub mod sink;
pub mod stage;

use crate::config::ConfigManager;
use engine::pipeline::Sink as _;
use harness_engine as engine;
use serde::Serialize;
use sink::AgentSink;
use std::path::Path;
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

/// 把「当前项目是什么」拼进任务描述。
///
/// 引擎的规划提示词天生是"从零规划一个新项目"的语义（`plan.rs` 的 `PLAN_SYSTEM`），
/// 只看任务原文的话，模型不知道用户在哪个项目里干活 —— 于是会自由发挥选语言、
/// 规划出一整套与真实项目无关的骨架。这里在 ruyix 侧把项目上下文补上。
///
/// 预算取 `max_context_chars` 的三分之一：剩下的留给任务原文和模型输出。
/// 采集失败（目录不在了等）会退回原任务，少一段上下文不该让整个会话失败。
fn contextualize(
    cfg: &engine::config::AppConfig,
    task: &str,
    project_root: Option<&str>,
) -> String {
    // 按 max_context_chars 折算，再用 DEFAULT_BUDGET 封顶：上下文再有用也不能吃掉全部窗口
    let budget = (cfg.max_context_chars / 3).clamp(2000, project_context::DEFAULT_BUDGET);
    project_context::with_context(task, project_root, budget)
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

/// 全流程公共段：agent_run 与 agent_reply（任务分支）共用。
async fn run_task_pipeline(
    app: AppHandle,
    state: &State<'_, AgentState>,
    cfg: engine::config::AppConfig,
    task: String,
    project_root: Option<&str>,
) -> Result<engine::workspace::RunRecord, String> {
    state.cancel.store(false, Ordering::Relaxed);
    let sink = AgentSink::new(app);
    let opts = engine::pipeline::RunOpts {
        task: contextualize(&cfg, task.trim(), project_root),
        ..Default::default()
    };
    engine::pipeline::run(&cfg, state.cancel.clone(), opts, &sink).await
}

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
    run_task_pipeline(app, &state, cfg, task, project_root.as_deref()).await
}

/// 会话消息的统一入口：Agent 工具循环（Read / Write / Execute / Connect）。
///
/// 不再做"问答/任务"预分类 —— 边界划不清（问项目要读代码，修报错也要读文件）。
/// 模型在循环里自己决定下一步用哪个能力，问读答、任务改验，直到给出最终答复。
/// 写入策略由会话工具栏模式映射：确认 → Stage（暂存 `.ruyix/stage/`，UI 确认后落盘）；
/// 写入/自主 → Apply（直接写进项目，覆盖前备份）。
///
/// Connect 的落地端是 `connect::RuyixConnector`：MCP 工具 + A2A 远端 Agent。
/// 引擎只认 `Connector` 契约，所以这里必须显式把 `McpManager` 传进去 —— 工具循环
/// 不是在命令面板里跑，面板那套 UI 状态帮不上它。
#[derive(Serialize)]
pub struct ReplyAgent {
    pub answer: String,
    pub steps: Vec<engine::agent::StepTrace>,
    pub changes: Vec<engine::agent::FileChange>,
    /// Stage 策略：暂存目录（确认面板从这里读差异）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_dir: Option<String>,
    /// Apply 策略：被覆盖文件的备份目录
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
    /// 机械验证的每一次结论（窄层 + 全量层，v0.3）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifications: Vec<engine::agent::VerifyOutcome>,
    /// 复核（干净上下文反思）的每一次结论（v0.3）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reflections: Vec<engine::reflect::Reflection>,
    pub usage: engine::llm::Usage,
    pub elapsed_ms: u128,
}

#[tauri::command]
// Tauri 命令的参数由前端按名字传（外加框架注入的 State），只能平铺；
// 打包成结构体会把 JS 侧的参数名与 Rust 结构体字段耦死，得不偿失。
#[allow(clippy::too_many_arguments)]
pub async fn agent_reply(
    app: AppHandle,
    state: State<'_, AgentState>,
    config: State<'_, Mutex<ConfigManager>>,
    mcp_mgr: State<'_, crate::mcp::McpManager>,
    task: String,
    history: Vec<engine::agent::HistoryMsg>,
    mode: Option<String>,
    project_root: Option<String>,
) -> Result<ReplyAgent, String> {
    let Some(root) = project_root.filter(|s| !s.trim().is_empty()) else {
        return Err("未打开项目 —— Agent 工具循环需要一个项目作为工作区".into());
    };
    let proj = Path::new(&root);
    if !proj.is_dir() {
        return Err(format!("项目目录不存在: {root}"));
    }
    let cfg = build_cfg(&config, Some(&root))?;
    let policy = match mode.as_deref() {
        Some("write") | Some("auto") => engine::agent::WritePolicy::Apply,
        _ => engine::agent::WritePolicy::Stage,
    };
    // 取消位跨命令共享：起跑清零，agent_cancel 置位。机械验证（cargo test 级）和复核
    // 都可能跑很久，没有取消路径会很糟。
    state.cancel.store(false, Ordering::Relaxed);
    let conn = connect::RuyixConnector::new(&app, mcp_mgr.inner(), Some(&root));
    let sink = AgentSink::new(app);
    let out = engine::agent::run(
        &cfg,
        proj,
        &task,
        &history,
        policy,
        &conn,
        &state.cancel,
        &sink,
    )
    .await?;
    Ok(ReplyAgent {
        answer: out.answer,
        steps: out.steps,
        changes: out.changes,
        stage_dir: out.stage_dir,
        backup_dir: out.backup_dir,
        verifications: out.verifications,
        reflections: out.reflections,
        usage: out.usage,
        elapsed_ms: out.elapsed_ms,
    })
}

/// 暂存产物预览（确认模式）：Agent 循环的写/改清单 vs 项目当前内容，**只读**
#[tauri::command]
pub fn agent_stage_preview(
    stage_id: String,
    project_root: Option<String>,
) -> Result<apply::Preview, String> {
    let root = project_root.ok_or("未打开项目")?;
    stage::preview(Path::new(&root), &stage_id)
}

/// 暂存产物落盘（确认面板的「写入项目」）：只写勾选路径，覆盖前备份
#[tauri::command]
pub fn agent_stage_apply(
    stage_id: String,
    paths: Vec<String>,
    backup: Option<bool>,
    project_root: Option<String>,
) -> Result<apply::ApplyResult, String> {
    let root = project_root.ok_or("未打开项目")?;
    stage::apply(Path::new(&root), &stage_id, &paths, backup.unwrap_or(true))
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
    let raw = task.trim();
    if raw.is_empty() {
        return Err("任务描述不能为空".into());
    }
    let task = contextualize(&cfg, raw, project_root.as_deref());
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
    let mut rec = engine::workspace::load(&engine::config::runs_root(&cfg), &run_id)?;
    // 落盘的 task 是注入过项目上下文的版本；回给 UI 时剥掉，界面上得是用户原话
    let clean = project_context::strip_context(&rec.task).to_string();
    rec.task = clean;
    Ok(rec)
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

// ============================================
// 产物写回真实项目
// ============================================

/// 预览：把这次运行的产物与真实项目逐文件比对，给出 add / modify / same。
/// **只读**。真正写盘要走 [`agent_apply_run`]，且必须由用户在 UI 上勾选确认。
#[tauri::command]
pub fn agent_apply_preview(
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    project_root: Option<String>,
) -> Result<apply::Preview, String> {
    let Some(root) = project_root.filter(|s| !s.trim().is_empty()) else {
        return Err("未打开项目，无法确认写回目标".into());
    };
    let cfg = build_cfg(&config, Some(&root))?;
    apply::preview(
        &harness_engine::config::runs_root(&cfg),
        &run_id,
        Path::new(&root),
    )
}

/// 写回：把勾选的产物写进真实项目。写前先备份被覆盖的文件。
/// `paths` 空数组 = 什么都不写（UI 全不勾选时的显式"我不要"）。
#[tauri::command]
pub fn agent_apply_run(
    config: State<'_, Mutex<ConfigManager>>,
    run_id: String,
    project_root: Option<String>,
    paths: Vec<String>,
    backup: Option<bool>,
) -> Result<apply::ApplyResult, String> {
    let Some(root) = project_root.filter(|s| !s.trim().is_empty()) else {
        return Err("未打开项目，无法写回".into());
    };
    let cfg = build_cfg(&config, Some(&root))?;
    apply::apply(
        &harness_engine::config::runs_root(&cfg),
        &run_id,
        Path::new(&root),
        &paths,
        backup.unwrap_or(true),
    )
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
// 会话（多会话对话模型：会话 = 中央编辑区的聊天 tab）
// ============================================

/// 项目的会话列表（按更新时间新→旧）
#[tauri::command]
pub fn agent_session_list(project_root: Option<String>) -> Result<Vec<sessions::Session>, String> {
    let root = project_root.ok_or("未打开项目")?;
    Ok(sessions::list(&root))
}

#[tauri::command]
pub fn agent_session_load(
    id: String,
    project_root: Option<String>,
) -> Result<sessions::Session, String> {
    let root = project_root.ok_or("未打开项目")?;
    sessions::load(&root, &id)
}

/// 保存整个会话（前端消息流追加后调用；id 由后端签发或沿用既有）
#[tauri::command]
pub fn agent_session_save(
    session_json: String,
    project_root: Option<String>,
) -> Result<sessions::Session, String> {
    let root = project_root.ok_or("未打开项目")?;
    let mut s: sessions::Session =
        serde_json::from_str(&session_json).map_err(|e| format!("会话 JSON 解析失败: {e}"))?;
    if s.id.trim().is_empty() {
        s.id = sessions::new_session_id();
    }
    s.updated_at = harness_engine::workspace::now_iso();
    sessions::save(&s, &root)?;
    Ok(s)
}

#[tauri::command]
pub fn agent_session_new(project_root: Option<String>) -> Result<sessions::Session, String> {
    let root = project_root.ok_or("未打开项目")?;
    let s = sessions::Session {
        id: sessions::new_session_id(),
        title: String::new(),
        created_at: harness_engine::workspace::now_iso(),
        updated_at: harness_engine::workspace::now_iso(),
        messages: Vec::new(),
    };
    sessions::save(&s, &root)?;
    Ok(s)
}

#[tauri::command]
pub fn agent_session_delete(
    id: String,
    project_root: Option<String>,
) -> Result<Vec<sessions::Session>, String> {
    let root = project_root.ok_or("未打开项目")?;
    sessions::delete(&root, &id)?;
    Ok(sessions::list(&root))
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
