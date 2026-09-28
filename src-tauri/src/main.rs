#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod a2a;
mod agent;
mod ai;
mod capability;
mod config;
mod fs_cmds;
mod git;
mod highlight;
mod instance;
mod mcp;
mod mem_cmds;
mod nav;
mod paths;
mod plugin;
mod plugin_cmds;
mod preinstalled;
mod proc_cmds;
mod project_cmds;
mod pty;
mod pty_cmds;
mod runner;

use std::path::Path;
use std::sync::Mutex;
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{MenuBuilder, MenuItemBuilder};

// ============================================
// 辅助函数
// ============================================

/// Windows 上 `Path::canonicalize()` 会加上 `\\?\` 扩展路径前缀，这里去掉它。
fn clean_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        stripped.to_string()
    } else {
        s.to_string()
    }
}

/// `--debug` / `-d` / `-v` / `--verbose`：打开详细日志（完整提示词 + 模型原始响应）。
///
/// 两条刻意的设计：
/// - **未知参数一律忽略**。Tauri / WebView2 / 打包器都可能塞自己的参数进来，
///   "不认识就退出"等于"多打一个参数就起不来"。
/// - 匹配是**整词相等**。`--debugx`、`--no-debug`、路径里含 `debug` 的一律不命中
///   —— 宽松匹配把"看起来像"当成"是"，正是本项目反复踩过的那类坑。
fn parse_debug_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .any(|a| matches!(a.as_ref(), "--debug" | "-d" | "--verbose" | "-v"))
}

/// 边界故障时说话的地方：**原生弹框**。
///
/// 为什么必须是原生的：这两条边界都在 `tauri::Builder` 之前判定（实测 builder 里失败是
/// panic —— `app.rs:1425`，优雅不了），那一刻**还没有任何 webview** 可以承载 HTML 文案；
/// 而 release 版是 windows 子系统、没有控制台 —— 不弹框就等于"双击了没反应"，是最坏的失败模式。
///
/// `RUYIX_NO_DIALOG=1` 时只打 stderr、不弹框：给自动化（判据脚本）留的口子 ——
/// 否则模态框会让脚本挂在那里等人点确定。**它只跳过弹框，不改变判定与退出码**。
fn native_alert(title: &str, body: &str) {
    eprintln!("[ruyix] {title}\n{body}");
    if std::env::var_os("RUYIX_NO_DIALOG").is_some_and(|v| !v.to_string_lossy().trim().is_empty()) {
        return;
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
        };
        let w = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        MessageBoxW(
            std::ptr::null_mut(),
            w(body).as_ptr(),
            w(title).as_ptr(),
            MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
    #[cfg(not(windows))]
    {
        let _ = (title, body);
    }
}

/// 详细日志的落盘位置：`<便携根>/global/logs/debug.log`（v1.0.0 起）。
///
/// 刻意**不放在项目目录**：开关是启动参数，那一刻还不知道会打开哪个项目；
/// 放在根内固定位置、好找 —— 正式版没有控制台，日志路径必须能被文档和用户事先知道，
/// 否则"开了开关找不到文件"等于没有这个功能。
fn debug_log_path() -> std::path::PathBuf {
    paths::current().debug_log()
}

// ============================================
// 启动助手：载入插件注册表
// ============================================
//
// 为什么它不属于任何"命令"模块：它没有 `#[tauri::command]`、不进 invoke_handler，
// 是 `main()` 用来**构造应用状态**的（载入结果作为 `Mutex<plugin::Registry>` 注入）。
// 原来的横幅叫"Tauri 命令"—— 那一坨 2026-09-28 已按域拆成 fs_cmds / project_cmds /
// plugin_cmds，只剩它守着旧名字，于是名字跟着内容改。

/// 载入插件注册表（全局 + 项目级）并把结果写进 `<根>/global/logs/plugins.jsonl`。
///
/// 为什么**成功也要留痕**：出问题时第一个要回答的是"这台机器上到底加载了哪些插件"，
/// 而"没加载成功"与"根本没装"在界面上长得一样（与 `env install` 同一套纪律）。
fn reload_plugins(
    plugins_root: &Path,
    root: &Path,
    current_project: Option<String>,
) -> plugin::Registry {
    let project_plugins = current_project.as_deref().map(|p| {
        paths::current()
            .project_bucket(p, "plugins")
            .join(plugin::PLUGIN_SUBDIR)
    });
    let builtin = preinstalled::builtin_available();
    let reg = plugin::Registry::load(
        &plugins_root.join(plugin::PLUGIN_SUBDIR),
        project_plugins.as_deref(),
        builtin,
    );
    if let Err(e) = plugin::log_records(
        &root.join("global").join("logs"),
        &reg,
        preinstalled::mode(),
    ) {
        eprintln!("[plugin] 写留痕失败: {e}");
    }
    println!(
        "[plugin] 模式={} 内置解析器={} 插件={} 语言={}",
        preinstalled::mode(),
        builtin,
        reg.plugins.len(),
        reg.plugins.iter().map(|p| p.langs.len()).sum::<usize>()
    );
    for n in &reg.notes {
        println!("[plugin] {n}");
    }
    reg
}

// ============================================
// 辅助：命令行拆分 / Windows .cmd 解析（crate 根共享）
// ============================================
//
// 为什么留在 crate 根而不是某个域模块里：`git.rs` / `pty.rs`（PTY 管理器）/ 搬出去的
// `proc_cmds.rs` / `pty_cmds.rs` 都用 `crate::split_cmd` / `crate::resolve_windows_cmd`
// 调它们 —— 是**跨模块**工具（crate 根的私有项对后代模块可见），不是谁家的私有实现。

/// 按空格拆分命令行，支持引号包裹。
/// 引号内 `\"` / `\'` 转义为字面引号；其余 `\` 保留（不转义）。
pub fn split_cmd(cmd: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut quote_char = '"';
    let mut chars = cmd.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quote {
            if ch == '\\' && chars.peek() == Some(&quote_char) {
                // 转义引号: \" 或 \' → 字面引号
                cur.push(quote_char);
                chars.next();
            } else if ch == quote_char {
                in_quote = false;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = true;
            quote_char = ch;
        } else if ch == ' ' || ch == '\t' {
            if !cur.is_empty() {
                parts.push(cur.clone());
                cur.clear();
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// Windows: 无扩展名的程序可能是 npm 全局脚本（如 claude、code）
/// 磁盘上同名无扩展名文件是 Unix shell 脚本，真正可执行的是 .cmd 版本
pub(crate) fn resolve_windows_cmd(program: &str) -> String {
    if cfg!(windows) {
        let p = std::path::Path::new(program);
        if p.extension().is_none()
            && let Ok(path_var) = std::env::var("PATH")
        {
            for dir in path_var.split(';') {
                let candidate = std::path::Path::new(dir).join(format!("{}.cmd", program));
                if candidate.exists() {
                    return format!("{}.cmd", program);
                }
            }
        }
    }
    program.to_string()
}
// ============================================
// AI 命令
// ============================================

#[tauri::command]
async fn ai_translate(
    input: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
    project_root: Option<String>,
) -> Result<String, String> {
    ai::translate(&config_mgr, project_root.as_deref(), &input).await
}

/// 厂商模型列表（GET /models）—— 配置表单的模型下拉框用它。
///
/// **拿不到就报错，绝不虚构列表**（虚构的清单会让用户选到一个跑不通的模型）。
/// 前端拿到错误后渲染成**只读**行 —— 早先是"降级成文本框让用户手填"，那正是
/// `model = "on"` 的来源：厂商对不认识的名字只回一句 400，用户还问不到该填什么。
///
/// `section` 决定探哪个端点：`ai`（默认，主用）或 `ai_fallback`（备用 LLM 自己的
/// url/key/协议）—— 备用端点的模型集未必与主用相同，不能拿主用的清单去凑。
#[tauri::command]
async fn ai_list_models(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
    project_root: Option<String>,
    section: Option<String>,
) -> Result<Vec<String>, String> {
    let cfg = {
        let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
        agent::config_bridge::build_app_config(&mgr, project_root.as_deref())?
    };
    let want_fallback = section
        .as_deref()
        .map(|s| s.eq_ignore_ascii_case("ai_fallback"))
        .unwrap_or(false);
    let llm = if want_fallback {
        cfg.llm_fallback
            .clone()
            .ok_or("未配置备用 LLM（ai_fallback.*）—— 先把它的 api_url / api_key 填上")?
    } else {
        cfg.llm.clone()
    };
    harness_engine::llm::probe(&llm).await
}

/// 厂商清单的一条 → 给前端的带能力条目。
///
/// 为什么把 `input_modalities` 一起给前端：那是**厂商自己声明的**"收什么输入"（实测
/// DeepSeek 是 text/image）。录音按钮该不该出现、能不能点，必须由这份声明决定，
/// 而不是由我们猜 —— 猜错的代价是一个按下去什么都没发生的按钮。
fn ai_model_info(m: harness_engine::llm::ModelInfo, api_format: &str) -> AiModelInfo {
    let caps = harness_engine::llm::model_caps(&m.id);
    AiModelInfo {
        web_search: harness_engine::llm::web_search_capable(&m.id, api_format),
        // 读图：能力表说能、或厂商声明里就有 image（两处任一为真都算有能力）
        multimodal: caps.multimodal || m.accepts("image"),
        // 收音频：**只看厂商声明**（能力表里没有这一维，也不该靠猜）
        audio: m.accepts("audio"),
        name: m.display_name().to_string(),
        id: m.id,
        input_modalities: m.input_modalities,
        context_window: m.context_window,
    }
}

/// 某个模型具备哪些服务端能力（联网 / 多模态）—— dto 组装。
///
/// 能力表在引擎里（`llm::model_caps`），宿主不抄一份 —— 否则加一个模型要改两处，
/// 而两处不一致的表现就是"配置里选得到、跑起来没反应"。
///
/// **联网是"（协议 × 模型）"的**：同一个模型在 anthropic 路上能搜、在 `/responses` 上搜不了
/// （实测 flash 就是这样）。所以 dto 里给三样东西：
/// · `web_search` —— **按当前 `api_format` 算出来的有效值**（开关可见性只看它，前端不必懂协议）；
/// · 两条协议各自的原始值 —— 界面要说清"哪个协议下能搜"时用；
/// · `api_format` —— 说明上面那个有效值是按哪条协议算的（文案里要写出来，否则用户看不懂为什么突然不能搜）。
fn model_caps_dto(name: &str, api_format: &str) -> ModelCapsDto {
    let c = harness_engine::llm::model_caps(name);
    ModelCapsDto {
        model: name.to_string(),
        web_search: harness_engine::llm::web_search_capable(name, api_format),
        web_search_openai: c.web_search,
        web_search_anthropic: c.web_search_anthropic,
        api_format: api_format.to_string(),
        multimodal: c.multimodal,
    }
}

/// 模型厂商对象（会话工具栏的**单一读取源**）。
///
/// 一发把会话里要的四样取齐：厂商身份（宿主 runtime 里那份对象）+ 厂商模型清单 +
/// 当前模型 + 该模型能力。过去前端要分三发（`ai_models` / `ai_model_caps` / 隐式解析），
/// 三发之间厂商可能已经换了；现在一发就是**同一版对象**，`identity.generation` 说清是哪一版。
///
/// 清单与 `ai_list_models`（配置表单用）**同源**：都走 `llm::models`，所以两处的
/// "有哪些模型"永远一致 —— 各拉一份的口径差就会变成"配置里选得到、会话里没有"。
///
/// 拿不到模型清单**不失败**：把原因放进 `models_error`，会话面板据此把下拉变成
/// "取不到清单"并说清为什么（拿不到就编一份清单，等于让用户选一个跑不通的模型）。
#[tauri::command]
async fn ai_vendor(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
    vendor: tauri::State<'_, Mutex<agent::vendor::ModelVendor>>,
    project_root: Option<String>,
) -> Result<VendorDto, String> {
    // 同步段：引擎配置 + 厂商对象快照（配置锁与 vendor 锁都**不跨 await**）
    let (cfg, identity) = {
        let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
        let cfg = agent::config_bridge::build_app_config(&mgr, project_root.as_deref())?;
        let identity = vendor.lock().map_err(|e| e.to_string())?.snapshot();
        (cfg, identity)
    };
    let (models, models_error) = match harness_engine::llm::models(&cfg.llm).await {
        Ok(list) => (
            list.into_iter()
                .map(|m| ai_model_info(m, &cfg.llm.api_format))
                .collect(),
            None,
        ),
        Err(e) => (Vec::new(), Some(e)),
    };
    // 当前模型按**配置解析**（runtime 覆盖已生效）—— 与配置面板同一个口径
    let model = cfg.llm.model.clone();
    Ok(VendorDto {
        caps: model_caps_dto(&model, &cfg.llm.api_format),
        identity,
        model,
        models,
        models_error,
    })
}

/// 会话工具栏要的**一份**厂商对象（含身份 / 清单 / 当前模型 / 能力）。
#[derive(serde::Serialize)]
struct VendorDto {
    /// 厂商身份快照（宿主 runtime 里维护的那份对象）
    identity: agent::vendor::VendorSnapshot,
    /// 当前模型（按当前配置解析，含 runtime 覆盖）
    model: String,
    /// 当前模型能力（联网/读图）—— 按钮可用性只看它
    caps: ModelCapsDto,
    /// 厂商模型清单（拿不到 = 空 + `models_error`）
    models: Vec<AiModelInfo>,
    models_error: Option<String>,
}

#[derive(serde::Serialize)]
struct AiModelInfo {
    id: String,
    /// 显示名（厂商给的，如 `DeepSeek-V4.1-Flash`）
    name: String,
    /// 厂商声明的输入模态（空 = 厂商没声明）
    input_modalities: Vec<String>,
    context_window: Option<u64>,
    /// 按**当前协议**算的服务端联网能力（与 🌏 按钮同一口径）
    web_search: bool,
    /// 读图
    multimodal: bool,
    /// 收音频（录音按钮据此出现/启用）
    audio: bool,
}

#[derive(serde::Serialize)]
struct ModelCapsDto {
    model: String,
    /// **按当前协议**的服务端联网能力（前端只看它）
    web_search: bool,
    /// 两条协议各自的原始能力（界面要说清"哪个协议下能搜"时用）
    web_search_openai: bool,
    web_search_anthropic: bool,
    /// 上面那个有效值是按哪条协议算的
    api_format: String,
    multimodal: bool,
}

// ============================================
// Config 命令
// ============================================

#[tauri::command]
fn config_get(
    scope: String,
    key: String,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Option<String>, String> {
    let s = config::Scope::from_str(&scope).ok_or_else(|| {
        format!(
            "无效的作用域: {}。可用: g/global, p/project, r/runtime",
            scope
        )
    })?;
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.config_read(&s, &key, project_root.as_deref())
}

#[tauri::command]
fn config_set(
    scope: String,
    key: String,
    value: String,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let s = config::Scope::from_str(&scope).ok_or_else(|| {
        format!(
            "无效的作用域: {}。可用: g/global, p/project, r/runtime",
            scope
        )
    })?;
    let mut mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.config_write(&s, &key, &value, project_root.as_deref())
}

#[tauri::command]
fn config_delete(
    scope: String,
    key: String,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let s = config::Scope::from_str(&scope).ok_or_else(|| {
        format!(
            "无效的作用域: {}。可用: g/global, p/project, r/runtime",
            scope
        )
    })?;
    let mut mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.config_delete(&s, &key, project_root.as_deref())
}

/// 配置菜单：扫描一个作用域 → 配置表单的数据源（本作用域 ∪ 回退链）
#[tauri::command]
fn config_form_load(
    scope: String,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<config::ScopeEntriesDump, String> {
    let s = config::Scope::from_str(&scope).ok_or_else(|| {
        format!(
            "无效的作用域: {}。可用: g/global, p/project, r/runtime",
            scope
        )
    })?;
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.scan_scope_entries(&s, project_root.as_deref())
}

/// 配置改完之后**广播一次"哪些键变了"**，让订阅者自己决定要不要重取。
///
/// 为什么必须是事件、而不是"前端保存完顺手再刷一遍"：
/// 配置表单与会话面板是**两个互不相识的模块** —— 表单不知道谁在用这些键，面板也不知道谁改了它们。
/// 旧行为是"面板打开时探一次"，于是：**用户刚在配置里填完 API Key，回到会话面板，
/// 红框里还写着「✗ key 未配置」** —— 界面在撒谎，用户只能以为"填了没用"（用户报障即此）。
/// 事件是这两端之间唯一诚实的连接：改的一方只说"我改了这些键"，用的一方自己决定怎么刷新。
///
/// 载荷带 `keys`（section + key）而不是"某个布尔"，是为了让订阅者能**按相关度过滤**：
/// 改个 `ui.lang` 不必把模型列表和 API key 状态全重探一遍。
///
/// **调用前必须已经放掉 config 锁**：监听者（会话面板）收到事件就会回头调
/// `agent_env_probe` / `ai_vendor` —— 那些命令同样要锁 config，握着锁发事件就是自己撞自己。
///
/// 这个键是不是**由宿主托管、前端用户无法编辑**、且广播出去只会引起无用重探的？
///
/// 目前只有模型名这一类（宿主键 `ai.model` / `ai_fallback.model`）：它既**不改变厂商身份**
/// （见 `refresh_vendor` —— 身份只由端点 / 密钥 / 协议决定），也不影响那排环境 chip，
/// 所以不进 `config://changed`，免得面板为一个与自己无关的键跑一次环境探针。
/// （口径含 `ends_with(".model")` 是为了兼容历史上错写成 `ai.model` 的那种键。）
fn is_engine_managed_key(section: &str, key: &str) -> bool {
    let k = key.trim();
    (section == "ai" || section == "ai_fallback") && (k == "model" || k.ends_with(".model"))
}

fn emit_config_changed(
    app: &tauri::AppHandle,
    scope: &str,
    entries: &[config::ConfigEntryInput],
    applied: bool,
) {
    // 模型名这类**宿主托管键不广播**：前端只能从下拉框里选它，收到广播也做不了什么，
    // 只会白跑一次环境探针（"换厂商"由 refresh_vendor 的 `model://vendor-changed` 负责）。
    let meaningful: Vec<&config::ConfigEntryInput> = entries
        .iter()
        .filter(|e| !is_engine_managed_key(&e.section, &e.key))
        .collect();
    if meaningful.is_empty() {
        return;
    }
    let keys: Vec<serde_json::Value> = meaningful
        .iter()
        .map(|e| serde_json::json!({ "section": e.section, "key": e.key }))
        .collect();
    let _ = app.emit(
        "config://changed",
        serde_json::json!({ "scope": scope, "applied": applied, "keys": keys }),
    );
}

/// 配置改完 → **厂商身份真的变了**才刷新宿主 runtime 里的厂商对象并广播 `model://vendor-changed`。
///
/// **为什么不是"配置一变就发"**：会话面板拿这个事件当"重取模型清单/能力"的信号，而清单只取决于
/// 端点 / 密钥 / 协议（换一家厂商、换一把钥匙、换一条协议，能列出、能联网的模型才可能变）。
/// 拧一下 🌏（写 `harness.llm.web_search`）与厂商无关；发事件只会让面板拿配置里的默认模型
/// 把用户刚选的那一个画回去 —— 那正是"toggle 联网就跳回 pro"的成因。
///
/// **身份从 `AppConfig` 解析、不逐个读键**：必须与**引擎实际要用的端点**同源
/// （见 `agent::vendor::ModelVendor::from_app` 的注释）。
///
/// **调用前 config 锁必须已释放**（同 `emit_config_changed`）。
fn refresh_vendor(
    app: &tauri::AppHandle,
    vendor: &Mutex<agent::vendor::ModelVendor>,
    config_mgr: &Mutex<config::ConfigManager>,
    project_root: Option<&str>,
) {
    let next = {
        let Ok(mgr) = config_mgr.lock() else { return };
        match agent::vendor::ModelVendor::read(&mgr, project_root) {
            Some(v) => v,
            None => return,
        }
    };
    let Ok(mut cur) = vendor.lock() else { return };
    if cur.same_vendor(&next) {
        return; // 同一家厂商：清单不会变，别打扰面板
    }
    cur.adopt(next); // 版本 +1
    let snapshot = cur.snapshot();
    drop(cur);
    let _ = app.emit("model://vendor-changed", snapshot);
}

/// 配置菜单：保存表单（增量写；空值 = 删除该键）
#[tauri::command]
fn config_form_save(
    app: tauri::AppHandle,
    scope: String,
    entries: Vec<config::ConfigEntryInput>,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
    vendor: tauri::State<'_, Mutex<agent::vendor::ModelVendor>>,
) -> Result<config::ScopeSaveReport, String> {
    let s = config::Scope::from_str(&scope).ok_or_else(|| {
        format!(
            "无效的作用域: {}。可用: g/global, p/project, r/runtime",
            scope
        )
    })?;
    let report = {
        let mut mgr = config_mgr.lock().map_err(|e| e.to_string())?;
        mgr.save_scope_entries(&s, &entries, project_root.as_deref())?
    };
    // 上面那个作用域结束 = 配置锁已释放（见 emit_config_changed 的注释：
    // 握着锁发事件会撞自己 —— 监听者收到事件就会回头调要锁的配置命令）
    emit_config_changed(&app, &scope, &entries, false);
    refresh_vendor(
        &app,
        vendor.inner(),
        config_mgr.inner(),
        project_root.as_deref(),
    );
    Ok(report)
}

/// 配置菜单：应用表单（= 保存 + 刷新 IDE 运行时内存里的配置对象）
#[tauri::command]
fn config_form_apply(
    app: tauri::AppHandle,
    scope: String,
    entries: Vec<config::ConfigEntryInput>,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
    vendor: tauri::State<'_, Mutex<agent::vendor::ModelVendor>>,
) -> Result<config::ScopeSaveReport, String> {
    let s = config::Scope::from_str(&scope).ok_or_else(|| {
        format!(
            "无效的作用域: {}。可用: g/global, p/project, r/runtime",
            scope
        )
    })?;
    let report = {
        let mut mgr = config_mgr.lock().map_err(|e| e.to_string())?;
        mgr.apply_scope_entries(&s, &entries, project_root.as_deref())?
    };
    // 「应用」比「保存」更该广播：运行时内存真的变了，界面里所有派生状态当场就旧了
    emit_config_changed(&app, &scope, &entries, true);
    refresh_vendor(
        &app,
        vendor.inner(),
        config_mgr.inner(),
        project_root.as_deref(),
    );
    Ok(report)
}

/// 配置菜单：引擎声明的配置键（键名 / 类型 / 默认值 / 枚举取值）。
///
/// 配置表单的 harness 段由它生成 —— 键名只在引擎里声明一次，前端不再手抄一份
/// （"同一个键名写四遍"正是过去加一个键要改六处的成因）。
#[tauri::command]
fn config_schema() -> Vec<harness_engine::config::KeySpec> {
    harness_engine::config::schema()
}

// ============================================
// MCP 命令（stdio 客户端，配置在 mcp.toml）
// ============================================

#[tauri::command]
async fn mcp_servers(
    project_root: Option<String>,
    mcp_mgr: tauri::State<'_, mcp::McpManager>,
) -> Result<Vec<mcp::ServerStatus>, String> {
    Ok(mcp_mgr.status(project_root.as_deref()).await)
}

#[tauri::command]
fn mcp_add_server(
    name: String,
    command: String,
    args: Option<Vec<String>>,
    env: Option<std::collections::HashMap<String, String>>,
    project_root: Option<String>,
) -> Result<Vec<mcp::McpServerCfg>, String> {
    let name = name.trim();
    if name.is_empty() || command.trim().is_empty() {
        return Err("名称与命令不能为空".into());
    }
    let cfg = mcp::McpServerCfg {
        name: name.to_string(),
        command: command.trim().to_string(),
        args: args.unwrap_or_default(),
        env: env.unwrap_or_default(),
        enabled: true,
    };
    mcp::save_server(&cfg, project_root.as_deref())?;
    Ok(mcp::load_servers(project_root.as_deref()))
}

#[tauri::command]
async fn mcp_remove_server(
    name: String,
    project_root: Option<String>,
    mcp_mgr: tauri::State<'_, mcp::McpManager>,
) -> Result<Vec<mcp::ServerStatus>, String> {
    mcp_mgr.stop(&name).await;
    mcp::remove_server(&name, project_root.as_deref())?;
    Ok(mcp_mgr.status(project_root.as_deref()).await)
}

#[tauri::command]
async fn mcp_start(
    name: String,
    project_root: Option<String>,
    mcp_mgr: tauri::State<'_, mcp::McpManager>,
) -> Result<mcp::ServerStatus, String> {
    let cfg = mcp::load_servers(project_root.as_deref())
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("MCP 服务器不存在: {name}"))?;
    if !cfg.enabled {
        return Err(format!("MCP 服务器已停用: {name}"));
    }
    mcp_mgr.start(&cfg).await
}

#[tauri::command]
async fn mcp_stop(
    name: String,
    project_root: Option<String>,
    mcp_mgr: tauri::State<'_, mcp::McpManager>,
) -> Result<Vec<mcp::ServerStatus>, String> {
    mcp_mgr.stop(&name).await;
    Ok(mcp_mgr.status(project_root.as_deref()).await)
}

#[tauri::command]
async fn mcp_tools(
    name: String,
    mcp_mgr: tauri::State<'_, mcp::McpManager>,
) -> Result<Vec<mcp::ToolInfo>, String> {
    mcp_mgr.tools(&name).await
}

#[tauri::command]
async fn mcp_call_tool(
    name: String,
    tool: String,
    args_json: String,
    mcp_mgr: tauri::State<'_, mcp::McpManager>,
) -> Result<mcp::CallOutcome, String> {
    mcp_mgr.call_tool(&name, &tool, &args_json).await
}

// ============================================
// A2A 命令（远端 agent 发现与任务委托）
// ============================================

#[tauri::command]
fn a2a_agents(project_root: Option<String>) -> Result<Vec<a2a::A2aAgentCfg>, String> {
    Ok(a2a::load_agents(project_root.as_deref()))
}

/// 发现远端 agent card 并保存到全局配置
#[tauri::command]
async fn a2a_discover(url: String) -> Result<a2a::A2aAgentCfg, String> {
    let cfg = a2a::discover_card(&url).await?;
    a2a::save_agent(&cfg)?;
    Ok(cfg)
}

#[tauri::command]
fn a2a_remove(name: String, project_root: Option<String>) -> Result<Vec<a2a::A2aAgentCfg>, String> {
    a2a::remove_agent(&name, project_root.as_deref())?;
    Ok(a2a::load_agents(project_root.as_deref()))
}

/// 委托任务给远端 agent；轮询进度经 a2a://status 事件推送
#[tauri::command]
async fn a2a_send(
    app: tauri::AppHandle,
    name: String,
    text: String,
    project_root: Option<String>,
) -> Result<a2a::A2aTaskResult, String> {
    let cfg = a2a::load_agents(project_root.as_deref())
        .into_iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("A2A agent 不存在: {name}"))?;
    let handle = app.clone();
    let agent_name = cfg.name.clone();
    let result = a2a::send_task(&cfg, &text, move |state, poll, max| {
        let _ = handle.emit(
            "a2a://status",
            serde_json::json!({"name": agent_name, "state": state, "poll": poll, "max": max}),
        );
    })
    .await?;
    let _ = app.emit(
        "a2a://status",
        serde_json::json!({"name": result.agent, "state": result.state, "poll": 0, "max": 0}),
    );
    Ok(result)
}

// ============================================
// 能力命令（工具白名单 + SKILL，tools.toml / skills.toml）
// ============================================

#[tauri::command]
fn tools_list(project_root: Option<String>) -> Result<Vec<capability::ToolCfg>, String> {
    Ok(capability::load_tools(project_root.as_deref()))
}

#[tauri::command]
fn tools_add(
    name: String,
    command: Option<String>,
    args_hint: Option<String>,
    description: Option<String>,
    project_root: Option<String>,
) -> Result<Vec<capability::ToolCfg>, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("工具名不能为空".into());
    }
    let cfg = capability::ToolCfg {
        name: name.to_string(),
        command: command.unwrap_or_default().trim().to_string(),
        args_hint: args_hint.unwrap_or_default().trim().to_string(),
        description: description.unwrap_or_default().trim().to_string(),
        enabled: true,
    };
    capability::save_tool(&cfg, project_root.as_deref())?;
    Ok(capability::load_tools(project_root.as_deref()))
}

#[tauri::command]
fn tools_remove(
    name: String,
    project_root: Option<String>,
) -> Result<Vec<capability::ToolCfg>, String> {
    capability::remove_tool(&name, project_root.as_deref())?;
    Ok(capability::load_tools(project_root.as_deref()))
}

/// 探测本机是否装有该命令（`<command> --version`，短超时）
#[tauri::command]
async fn tools_probe(name: String) -> Result<capability::ToolProbe, String> {
    let cfg = capability::load_tools(None)
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("工具不存在: {name}"))?;
    Ok(capability::probe_tool(&cfg).await)
}

#[tauri::command]
fn skills_list(project_root: Option<String>) -> Result<Vec<capability::SkillCfg>, String> {
    Ok(capability::load_skills(project_root.as_deref()))
}

#[tauri::command]
fn skills_save(
    name: String,
    description: String,
    content: String,
    project_root: Option<String>,
) -> Result<Vec<capability::SkillCfg>, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("技能名不能为空".into());
    }
    let cfg = capability::SkillCfg {
        name: name.to_string(),
        description: description.trim().to_string(),
        enabled: true,
        content,
    };
    capability::save_skill(&cfg, project_root.as_deref())?;
    Ok(capability::load_skills(project_root.as_deref()))
}

#[tauri::command]
fn skills_remove(
    name: String,
    project_root: Option<String>,
) -> Result<Vec<capability::SkillCfg>, String> {
    capability::remove_skill(&name, project_root.as_deref())?;
    Ok(capability::load_skills(project_root.as_deref()))
}

/// 建主窗口。
///
/// **窗口必须在 Rust 里建，不能留在 `tauri.conf.json` 的 `app.windows`** —— 只有 Builder 上挂得了
/// `on_navigation`，而那就是这道 P0 的闸门；配置里生出来的窗口没有闸门，链接一点就顶掉整个 IDE。
/// 原来的配置项逐条照抄（标题 / 尺寸 / 无边框 / 居中 / devtools），漏一个就是启动时的观感回归。
fn build_main_window(app: &tauri::AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    let dev_url = app.config().build.dev_url.clone();
    let nav_dev_url = dev_url.clone();
    tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
        .title("ruyix")
        .inner_size(1200.0, 800.0)
        .decorations(false)
        .center()
        .devtools(true)
        // WebView2 的用户数据目录落进**便携根**（`<根>/global/webview`）。不指定就外溢到
        // `%LOCALAPPDATA%\com.ruyix.code`（实测 41MB）—— 那样"删文件夹即净"就不成立。
        // 与单实例互斥体（`instance.rs`）**必须同批按根区分**：只做一半会得到
        // "缓存被两份副本共用、第二个实例却起不来"的更坏状态（P0 实测）。
        .data_directory(paths::current().webview_data())
        .on_navigation(move |url| match nav::nav_verdict(url, nav_dev_url.as_ref()) {
            nav::Nav::Allow => true,
            nav::Nav::External => {
                // 外站：不放行，改用系统浏览器 —— 用户要的是"打开这个链接"，
                // 不是"把 IDE 换成这个页面"
                if let Err(e) = nav::launch_in_browser(url.as_str()) {
                    eprintln!("[ruyix] 外链打不开：{e}（{url}）");
                }
                false
            }
            nav::Nav::Refuse => {
                eprintln!("[ruyix] 拦下一次应用内导航（不是文档路径）：{url}");
                false
            }
        })
        .on_new_window(move |url, _features| {
            // `target="_blank"` / window.open 走的是这条路，不是导航。默认没人接就被静默吞掉，
            // 于是"点了没反应"；这里跟导航用**同一张判定表**：外站交给系统浏览器。
            if nav::nav_verdict(&url, dev_url.as_ref()) == nav::Nav::External
                && let Err(e) = nav::launch_in_browser(url.as_str())
            {
                eprintln!("[ruyix] 外链打不开：{e}（{url}）");
            }
            tauri::webview::NewWindowResponse::<tauri::Wry>::Deny
        })
        .build()
}

// ============================================
// 入口
// ============================================

fn main() {
    // ---- 便携根（**唯一解析点**）。必须在一切之前：debug 日志落点、配置目录、引擎运行
    // 目录、WebView2 数据目录全都从它派生 —— 晚一步就会有东西按"旧的家"落盘。
    let root_paths = paths::init_auto();

    // ---- 边界判定（v1.0.0 P2）：不可写 / 临时根 → 原生弹框 + **拒绝启动**。
    // 必须在 Builder 之前：实测 builder 里失败是 panic（`app.rs:1425`），那时弹不出人话，
    // 而 release 版是 windows 子系统、没有控制台 ⇒ 用户看到的是"双击了没反应"。
    if let Some((title, body)) = root_paths.root_verdict().message(root_paths.root()) {
        native_alert(&title, &body);
        std::process::exit(2);
    }

    // ---- 目录模板（**幂等、不覆盖**）：只放一个 exe 的空文件夹，也能长成一个能用的家。
    // 建不出来就是"不可写"那条路（判定用的是同一个试写探针，这里只是失败时的第二次机会）。
    let created = match root_paths.ensure_layout() {
        Ok(v) => v,
        Err(e) => {
            let (title, body) = paths::RootVerdict::NotWritable
                .message(root_paths.root())
                .expect("NotWritable 一定有文案");
            native_alert(&title, &format!("{body}\n\n（创建目录失败：{e}）"));
            std::process::exit(2);
        }
    };

    // ---- 详细日志开关（`--debug`）。必须在一切之前：Builder 起来之后引擎随时可能开跑，
    // 那时再设开关，前几轮就已经漏掉了。
    if parse_debug_flag(std::env::args()) {
        let log_path = debug_log_path();
        harness_engine::debug::set_path(log_path.clone());
        harness_engine::debug::set_enabled(true);
        harness_engine::debug::note(&format!(
            "\n########## ruyix 详细日志会话 ##########\n开始时间 : {}\n进程号   : {}\n便携根   : {}\n可写     : 是（试写探针通过）\nwebview  : {}\n落盘     : {}\n首启新建 : {}\n包含     : 完整提示词 / 原始响应体 / 解析结果，以及与终端一致的运行日志行\n说明     : 仅在 `--debug` 启动时产生；常规运行一个字都不多写",
            harness_engine::workspace::now_human(),
            std::process::id(),
            root_paths.root().display(),
            root_paths.webview_data().display(),
            log_path.display(),
            if created.is_empty() {
                "（无：目录已齐）".to_string()
            } else {
                created.join("  ")
            }
        ));
    }

    let config_mgr = Mutex::new(config::ConfigManager::new(root_paths.global_dir()));
    let pty_mgr = Mutex::new(pty::PtyManager::new());

    // ---- 高亮插件：先物化预装的那份（纯净模式什么都不写），再扫插件目录载入注册表。
    // 项目级插件按**当前项目**算，所以切项目时要重载（见 `reload_plugins`）。
    let plugins_root = root_paths.plugins_dir();
    let materialized = preinstalled::materialize(&plugins_root);
    if !materialized.is_empty() {
        println!(
            "[plugin] 预装高亮插件已物化（{} 个文件）：{:?}",
            materialized.len(),
            materialized
        );
    }
    // 项目记忆（v1.1 核心模块，不可插件化）：账本 + 向量都在便携根的 global/memory/ 下。
    // 失败**不阻止启动**：记忆是辅助能力，它坏了不该让 IDE 起不来；但必须留下痕迹。
    {
        let memory_dir = root_paths.memory_dir();
        harness_engine::mem::embed::set_model_root(memory_dir.join("model"));
        match harness_engine::mem::Memory::open(
            memory_dir.join("mem.db"),
            harness_engine::observe::secrets(),
        ) {
            Ok(m) => {
                if let Err(e) = harness_engine::mem::install(m) {
                    eprintln!("[mem] 安装失败（继续跑，记忆不参与本轮）：{e}");
                }
            }
            Err(e) => eprintln!("[mem] 记忆库打开失败（继续跑，记忆不参与本轮）：{e}"),
        }
    }

    let plugin_reg = Mutex::new(reload_plugins(
        &plugins_root,
        root_paths.root(),
        config_mgr
            .lock()
            .map(|c| c.load_projects().current)
            .unwrap_or(None),
    ));

    // 模型厂商对象（宿主 runtime）：启动时按当前配置播种；之后每次配置变化由
    // `refresh_vendor` 比对身份，变了才 +1 版本并广播 `model://vendor-changed` ——
    // 会话面板只认这一个事件，于是"拧一下 🌏 就把选中的模型弹回 pro"的回环从根上没了。
    let vendor_state = Mutex::new(
        config_mgr
            .lock()
            .ok()
            .and_then(|m| agent::vendor::ModelVendor::read(&m, None))
            .unwrap_or_default(),
    );

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(config_mgr)
        .manage(vendor_state)
        .manage(mcp::McpManager::new())
        .manage(pty_mgr)
        .manage(agent::AgentState::new())
        .manage(plugin_reg)
        .setup(|app| {
            // 主窗口在这里建（不是 tauri.conf.json 的 app.windows）——
            // 只有 Rust 侧的 Builder 挂得上 on_navigation，也就是外链的道闸，详见 build_main_window
            build_main_window(app.handle())?;

            // 模型运行时兜底（切片 4）：**单 exe 也要能自检并下载模型**。
            // 形态是"一个可执行文件 + global/ + projects/ + plugins/"，所以"只有单 exe"是主路径之一；
            // 此前只有打包期的取模型步骤 ⇒ 单 exe 用户拿到的是**静默降级**（语义检索关着，没出口打开它）。
            // 规格（文件名/体积/sha256/来源 URL）是编译进二进制的，所以 exe 自己知道该取什么。
            // 开关：`--no-fetch` 关掉自动取（绿色手动路线），`--fetch-model` 强制取一次（幂等，已装就跳过）。
            {
                let args: Vec<String> = std::env::args().collect();
                let no_fetch = args.iter().any(|a| a == "--no-fetch");
                let force = args.iter().any(|a| a == "--fetch-model");
                let st = harness_engine::mem::fetch::status();
                if force || (!no_fetch && !st.is_ready()) {
                    let h = app.handle().clone();
                    let h2 = app.handle().clone();
                    let start_line = st.line();
                    tauri::async_runtime::spawn(async move {
                        let _ = h.emit(
                            "mem://model",
                            serde_json::json!({ "phase": "start", "line": start_line.clone() }),
                        );
                        let res = harness_engine::mem::fetch::fetch(|p| {
                            let _ = h2.emit(
                                "mem://model",
                                serde_json::json!({
                                    "phase": "progress",
                                    "file": p.file,
                                    "index": p.index,
                                    "of": p.of,
                                    "done": p.done,
                                    "total": p.total,
                                    "tag": p.tag,
                                }),
                            );
                        })
                        .await;
                        match res {
                            Ok(rep) => {
                                // 进账本：**这台机器上模型在哪**是一件值得记住的事实（探针即证据）
                                if let Some(m) = harness_engine::mem::current() {
                                    let where_ = harness_engine::mem::fetch::target_dir()
                                        .map(|d| d.display().to_string())
                                        .unwrap_or_default();
                                    let _ = m.record_obs(
                                        harness_engine::mem::GLOBAL_SCOPE,
                                        "mem.embed.model",
                                        &where_,
                                        harness_engine::mem::Origin::Probe,
                                        &[],
                                        None,
                                    );
                                }
                                let _ = h.emit(
                                    "mem://model",
                                    serde_json::json!({
                                        "phase": "done",
                                        "fetched": rep.fetched.len(),
                                        "skipped": rep.skipped.len(),
                                        "bytes": rep.bytes,
                                    }),
                                );
                            }
                            Err(e) => {
                                let _ = h.emit(
                                    "mem://model",
                                    serde_json::json!({ "phase": "error", "line": e }),
                                );
                            }
                        }
                    });
                }
            }

            // 注册原生 Ctrl+S 快捷键 — 即使 WebView2 拦截了 JS 的 Ctrl+S，
            // 原生菜单 accelerator 仍能在 OS 层面捕获该组合键
            let save = MenuItemBuilder::with_id("save", "保存")
                .accelerator("CmdOrCtrl+S")
                .build(app)?;
            let menu = MenuBuilder::new(app).item(&save).build()?;
            app.set_menu(menu)?;
            Ok(())
        })
        .on_menu_event(|app_handle, event| {
            if event.id() == "save" {
                // 通知前端执行保存
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.emit("menu-save", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            project_cmds::open_project,
            fs_cmds::list_dir,
            fs_cmds::read_file,
            fs_cmds::read_file_base64,
            fs_cmds::write_file,
            fs_cmds::create_file,
            fs_cmds::create_dir,
            fs_cmds::delete_path,
            plugin_cmds::get_execute_status,
            plugin_cmds::set_execute_entry,
            plugin_cmds::ai_execute_check,
            plugin_cmds::lua_translate,
            fs_cmds::path_exists,
            fs_cmds::rename_path,
            highlight::highlight_code,
            project_cmds::is_another_instance,
            project_cmds::get_last_project,
            project_cmds::get_projects,
            project_cmds::project_buckets,
            project_cmds::project_bucket_delete,
            project_cmds::set_project_lang,
            project_cmds::update_project,
            project_cmds::delete_project,
            project_cmds::get_run_targets,
            highlight::highlight_plugins,
            pty_cmds::get_term_targets,
            proc_cmds::run_target,
            proc_cmds::proc_list,
            mem_cmds::mem_status,
            mem_cmds::mem_model_status,
            mem_cmds::mem_model_fetch,
            mem_cmds::mem_beliefs,
            mem_cmds::mem_why,
            mem_cmds::mem_as_of,
            mem_cmds::mem_receipts,
            mem_cmds::mem_rebuild,
            mem_cmds::mem_record,
            proc_cmds::proc_stop,
            proc_cmds::proc_log_read,
            nav::open_external,
            proc_cmds::spawn_terminal,
            pty_cmds::pty_spawn,
            pty_cmds::pty_write,
            pty_cmds::pty_resize,
            pty_cmds::pty_close,
            config_get,
            config_set,
            config_delete,
            config_form_load,
            config_form_save,
            config_form_apply,
            config_schema,
            ai_translate,
            ai_list_models,
            ai_vendor,
            // Agent 命令桥（融合计划 Z3，append-only 注册块）
            agent::agent_run,
            agent::agent_reply,
            agent::agent_ask_answer,
            agent::agent_stage_preview,
            agent::agent_stage_apply,
            agent::agent_plan,
            agent::agent_generate,
            agent::agent_verify,
            agent::agent_lint,
            agent::agent_repair,
            agent::agent_cancel,
            agent::agent_runs,
            agent::agent_run_load,
            agent::agent_run_delete,
            agent::agent_read_artifact,
            agent::agent_apply_preview,
            agent::agent_apply_run,
            agent::agent_env_probe,
            agent::agent_session_list,
            agent::agent_session_load,
            agent::agent_session_save,
            agent::agent_session_new,
            agent::agent_session_delete,
            git::git_status,
            git::git_run,
            mcp_servers,
            mcp_add_server,
            mcp_remove_server,
            mcp_start,
            mcp_stop,
            mcp_tools,
            mcp_call_tool,
            a2a_agents,
            a2a_discover,
            a2a_remove,
            a2a_send,
            tools_list,
            tools_add,
            tools_remove,
            tools_probe,
            skills_list,
            skills_save,
            skills_remove,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    // 宿主退出收尾：把引擎托管的进程**全收**（跨项目、且不放行 keep_alive）。
    //
    // 为什么必须在这儿收：托管的进程是宿主 spawn 的，但**不**在宿主的进程树里
    // （Windows 上 `cmd → mvn.cmd → java` 三代），宿主一退，它们就变成占着端口的孤儿 ——
    // 实测那次一个 `java` 孤儿占了 8083 端口 28 分钟，后面每一次重启都拿到假的"端口被占"
    // 失败信号。引擎持有就引擎收，退出这一步是最后一道闸。
    app.run(|_app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            let (stopped, _kept) = harness_engine::proc::shutdown_all(false);
            if !stopped.is_empty() {
                eprintln!(
                    "[ruyix] 退出：已收掉 {} 个托管进程（含子进程树）",
                    stopped.len()
                );
            }
        }
    });
}

// ============================================
// 测试
// ============================================

#[cfg(test)]
mod tests;
