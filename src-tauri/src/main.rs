#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod a2a;
mod agent;
mod ai;
mod capability;
mod config;
mod git;
mod instance;
mod mcp;
mod pty;
mod runner;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::sync::Mutex;
use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{MenuBuilder, MenuItemBuilder};

/// CREATE_NEW_CONSOLE — 为新进程创建独立控制台窗口
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x00000010;

/// CREATE_NO_WINDOW — 阻止子进程新建控制台窗口（release GUI 子系统无控制台，
/// 不加此标志控制台子进程会闪黑窗口；输出仍通过管道捕获）
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

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

// ============================================
// 数据结构
// ============================================

#[derive(serde::Serialize, Clone)]
struct ProjectInfo {
    name: String,
    path: String,
    lang: String,
}

#[derive(serde::Serialize, Clone)]
struct DirEntry {
    name: String,
    path: String,
    is_dir: bool,
}

#[derive(serde::Serialize, Clone)]
struct FileContent {
    path: String,
    content: String,
}

#[derive(serde::Serialize, Clone)]
struct FileBase64 {
    path: String,
    mime: String,
    base64: String,
}

#[derive(serde::Serialize, Clone)]
struct LineHighlight {
    line_number: usize,
    text: String,
    spans: Vec<LineSpan>,
}

#[derive(serde::Serialize, Clone)]
struct LineSpan {
    start_col: usize,
    end_col: usize,
    tag: String,
}

// ============================================
// Tauri 命令
// ============================================

#[tauri::command]
fn open_project(
    path: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<ProjectInfo, String> {
    let p = Path::new(&path);

    if !p.exists() {
        return Err(format!("路径不存在: {}", p.display()));
    }
    if !p.is_dir() {
        return Err("路径不是目录，请输入项目文件夹路径".to_string());
    }

    let canonical = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let clean = clean_path(&canonical);
    let name = canonical
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // 持久化到 projects 配置（返回保存的项目语言）
    let lang = config_mgr
        .lock()
        .map_err(|e| e.to_string())?
        .set_current_project(&clean, &name)
        .unwrap_or_else(|_| config::default_lang());

    Ok(ProjectInfo {
        name,
        path: clean,
        lang,
    })
}

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<DirEntry>, String> {
    let p = Path::new(&path);
    if !p.is_dir() {
        return Err("路径不是目录".to_string());
    }

    let dir_iter = std::fs::read_dir(p).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();

    for entry in dir_iter {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

        if is_dir && name.starts_with('.') {
            continue;
        }

        entries.push(DirEntry {
            name,
            path: clean_path(&entry.path()),
            is_dir,
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(entries)
}

#[tauri::command]
fn read_file(path: String) -> Result<FileContent, String> {
    let p = Path::new(&path);

    if !p.exists() {
        return Err(format!("文件不存在: {}", p.display()));
    }
    if !p.is_file() {
        return Err("路径不是文件".to_string());
    }

    let content = std::fs::read_to_string(p).map_err(|e| e.to_string())?;

    Ok(FileContent {
        path: clean_path(p),
        content,
    })
}

/// 根据文件扩展名返回 MIME 类型
fn mime_from_ext(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("ico") | Some("icon") => "image/x-icon",
        _ => "image/png",
    }
}

#[tauri::command]
fn read_file_base64(path: String) -> Result<FileBase64, String> {
    use base64::Engine;

    let p = Path::new(&path);

    if !p.exists() {
        return Err(format!("文件不存在: {}", p.display()));
    }
    if !p.is_file() {
        return Err("路径不是文件".to_string());
    }

    let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
    let mime = mime_from_ext(p);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

    Ok(FileBase64 {
        path: clean_path(p),
        mime: mime.to_string(),
        base64: b64,
    })
}

#[tauri::command]
fn write_file(path: String, content: String) -> Result<(), String> {
    let p = Path::new(&path);
    std::fs::write(p, &content).map_err(|e| format!("保存失败: {}", e))
}

#[tauri::command]
fn create_file(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    if p.exists() {
        return Err(format!("文件已存在: {}", p.display()));
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建父目录失败: {}", e))?;
    }
    std::fs::write(p, "").map_err(|e| format!("创建文件失败: {}", e))
}

#[tauri::command]
fn create_dir(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    std::fs::create_dir_all(p).map_err(|e| format!("创建目录失败: {}", e))
}

#[tauri::command]
fn delete_path(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    if !p.exists() {
        return Err(format!("路径不存在: {}", p.display()));
    }
    if p.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| format!("删除目录失败: {}", e))
    } else {
        std::fs::remove_file(p).map_err(|e| format!("删除文件失败: {}", e))
    }
}

/// 执行状态返回
#[derive(serde::Serialize)]
struct ExecuteStatus {
    /// None = 未知, Some(true) = 可运行, Some(false) = 不可运行
    known: Option<bool>,
    /// 如果可运行，是否已有运行目标绑定了该文件
    has_target: bool,
    /// 绑定的目标名称（若有）
    target_name: Option<String>,
    /// 建议的运行命令（若来自预置清单）
    suggested_cmd: Option<String>,
    /// 建议的运行目标列表：package.json 的每个 scripts 各一条
    suggested_targets: Vec<runner::RunSpec>,
}

#[tauri::command]
fn get_execute_status(
    path: String,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<ExecuteStatus, String> {
    let p = std::path::Path::new(&path);
    let file_name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    let exec_map = mgr.load_execute_map();

    // 三层查找：预置清单 → 文件名匹配 → 扩展名匹配
    let mut known: Option<bool> = None;
    let mut suggested_cmd: Option<String> = None;
    let mut suggested_targets: Vec<runner::RunSpec> = Vec::new();

    // 1) 预置清单（优先级最高，保证知名文件不被扩展名级条目覆盖）
    //    命令来自文件内容：package.json 按 scripts 逐条生成运行目标，
    //    而不是写死的 npm start
    match runner::manifest_run_specs(p) {
        Some(specs) => {
            known = Some(true);
            suggested_cmd = specs.first().map(|s| s.cmd.clone());
            suggested_targets = specs;
        }
        None => {
            // 不是清单文件或内容解析失败 → 回退到静态命令表
            if let Some(manifest) = config::manifest_for_path(p) {
                known = Some(true);
                suggested_cmd = Some(manifest.cmd.to_string());
            }
        }
    }

    // 2) execute.toml 文件名精确匹配
    if known.is_none() && !file_name.is_empty() {
        known = exec_map.get(&file_name).copied();
    }

    // 3) execute.toml 扩展名匹配
    if known.is_none() && !ext.is_empty() {
        known = exec_map.get(&ext).copied();
    }

    // 扩展名为空（如 Makefile、Dockerfile）且以上均未匹配 → 未知
    if known.is_none() && ext.is_empty() {
        known = Some(false);
    }

    let mut has_target = false;
    let mut target_name = None;
    if known == Some(true)
        && let Some(root) = project_root
        && let Ok(targets) = mgr.load_run_targets(Some(&root))
    {
        for t in &targets {
            // 匹配 bind 字段：显式绑定的文件路径
            if let Some(ref bind) = t.bind {
                let full_bind = std::path::Path::new(&root).join(bind);
                if let Ok(full) = full_bind.canonicalize() {
                    let bind_path = clean_path(&full);
                    let input_path = clean_path(std::path::Path::new(&path));
                    if input_path == bind_path {
                        has_target = true;
                        target_name = t.name.clone().or_else(|| Some(t.key.clone()));
                        break;
                    }
                }
            }
            // 匹配 cmd 字段：命令中包含文件路径或文件名
            if let Some(ref cmd) = t.cmd
                && (cmd.contains(&path) || cmd.contains(&file_name))
            {
                has_target = true;
                target_name = t.name.clone().or_else(|| Some(t.key.clone()));
                break;
            }
        }
    }
    Ok(ExecuteStatus {
        known,
        has_target,
        target_name,
        suggested_cmd,
        suggested_targets,
    })
}

#[tauri::command]
fn set_execute_entry(
    path: String,
    can_run: bool,
    as_file: Option<bool>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    let p = std::path::Path::new(&path);
    // 确定存储键：知名清单文件 → 文件名；否则按 as_file 决定
    let is_file = as_file.unwrap_or(false) || config::manifest_for_path(p).is_some();
    let key = if is_file {
        p.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase()
    } else {
        p.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase()
    };
    if key.is_empty() {
        return Err("无法确定存储键".to_string());
    }
    mgr.save_execute_entry(&key, can_run)
}

#[tauri::command]
async fn ai_execute_check(
    path: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<String, String> {
    ai::check_executable(&config_mgr, &path).await
}

/// Lua 脚本翻译：在沙箱中执行 learn.lua，匹配自然语言 → 标准命令
#[tauri::command]
fn lua_translate(
    input: String,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Option<String>, String> {
    eprintln!("[RUST-LUA] 输入: {}", input);
    let root = match project_root {
        Some(r) => r,
        None => {
            eprintln!("[RUST-LUA] 无项目，跳过");
            return Ok(None);
        }
    };
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    let lua_content = mgr.load_lua_script(&root)?;
    if lua_content.trim().is_empty() {
        eprintln!("[RUST-LUA] learn.lua 为空，跳过");
        return Ok(None);
    }
    eprintln!(
        "[RUST-LUA] learn.lua 内容 ({} 字节):\n{}",
        lua_content.len(),
        lua_content
    );

    let full_script = format!("local input = ...\n{}\nreturn nil", lua_content);

    let lua = mlua::Lua::new();
    // 沙箱：移除危险全局函数
    for name in ["os", "io", "require", "loadfile", "dofile", "load"] {
        lua.globals()
            .set(name, mlua::Value::Nil)
            .map_err(|e| format!("Lua 沙箱失败: {}", e))?;
    }

    let result: mlua::Value = lua.load(&full_script).call(input).map_err(|e| {
        eprintln!("[RUST-LUA] 执行失败: {}", e);
        format!("Lua 执行失败: {}", e)
    })?;

    eprintln!("[RUST-LUA] Lua 返回值类型: {:?}", result.type_name());
    // 返回值：nil → None，字符串 → Some
    if result.is_nil() {
        eprintln!("[RUST-LUA] → nil (未命中)");
        Ok(None)
    } else if let Some(s) = result.as_str() {
        let s = s.trim().to_string();
        eprintln!("[RUST-LUA] → 命中: {}", s);
        if s.is_empty() { Ok(None) } else { Ok(Some(s)) }
    } else {
        eprintln!("[RUST-LUA] → 非字符串返回值，忽略");
        Ok(None)
    }
}

#[tauri::command]
fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).exists()
}

#[tauri::command]
fn rename_path(from: String, to: String) -> Result<(), String> {
    let src = Path::new(&from);
    if !src.exists() {
        return Err(format!("路径不存在: {}", src.display()));
    }
    std::fs::rename(src, Path::new(&to)).map_err(|e| format!("重命名失败: {}", e))
}

#[tauri::command]
async fn highlight_code(language: String, code: String) -> Result<Vec<LineHighlight>, String> {
    // 在后台线程中执行 CPU 密集的语法高亮，避免阻塞异步运行时
    tauri::async_runtime::spawn_blocking(move || {
        use arborium::Highlighter;

        let mut highlighter = Highlighter::new();
        let spans = highlighter
            .highlight_spans(&language, &code)
            .map_err(|e| e.to_string())?;

        let themed: Vec<(u32, u32, &str)> = spans
            .iter()
            .filter_map(|s| {
                arborium_theme::tag_for_capture(&s.capture)
                    .and_then(arborium_theme::tag_to_name)
                    .map(|name| (s.start, s.end, name))
            })
            .collect();

        build_line_highlights(&code, &themed)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 是否有其他实例正在运行（第二实例不自动打开上次项目）
#[tauri::command]
fn is_another_instance() -> bool {
    instance::is_other_instance()
}

/// 获取上次打开的项目路径（供前端启动时自动打开）
#[tauri::command]
fn get_last_project(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Option<String>, String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    Ok(cfg.load_projects().current)
}

/// 获取所有已知项目列表（含 name/path/lang）
#[tauri::command]
fn get_projects(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Vec<config::ProjectEntry>, String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    Ok(cfg.load_projects().list)
}

/// 设置项目语言
#[tauri::command]
fn set_project_lang(
    path: String,
    lang: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    cfg.set_project_lang(&path, &lang)
}

/// 更新项目名称与语言（路径不可修改）
#[tauri::command]
fn update_project(
    path: String,
    name: String,
    lang: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    cfg.update_project(&path, &name, &lang)
}

/// 从项目列表删除项目（不删除项目文件夹）
#[tauri::command]
fn delete_project(
    path: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    cfg.delete_project(&path)
}

/// 迁移旧版项目配置（纯路径列表 → name/path/lang 条目），返回迁移数量
#[tauri::command]
fn migrate_projects(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<usize, String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    cfg.migrate_projects()
}

#[tauri::command]
fn get_run_targets(
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Vec<config::RunTarget>, String> {
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.load_run_targets(project_root.as_deref())
}

// ============================================
// 托管进程（"服务"面板）
// ============================================
//
// 为什么宿主必须能看见它们：agent 用 background 起的服务（mvn spring-boot:run / java -jar）
// 是**宿主** spawn 的，却不在宿主的进程树里 —— 以前它们只活在引擎的进程表里，
// UI 上等于不存在：起得来、看不见、也停不掉（只能靠任务管理器按 pid 找）。
// 面板读的是引擎那**同一份**表（`harness_engine::proc::listing`），不另立一份，
// 否则面板和模型就会各说各话。

/// 全部托管进程（含已退出待查的），状态已刷新。
#[tauri::command]
fn proc_list() -> Vec<harness_engine::proc::ProcInfo> {
    harness_engine::proc::listing()
}

/// 按 pid 停掉一个托管进程，**连子进程树一起**（只杀 mvn 不杀 java 就是又造一个孤儿）。
///
/// 走 `spawn_blocking`：`kill_tree` + `wait` 会等进程真的退掉（秒级），
/// 放在主线程会把 UI 卡住这段。
#[tauri::command]
async fn proc_stop(pid: u32) -> Result<harness_engine::proc::ProcInfo, String> {
    tauri::async_runtime::spawn_blocking(move || harness_engine::proc::stop_pid(pid))
        .await
        .map_err(|e| format!("停止任务失败：{e}"))?
}

#[derive(serde::Serialize, Clone)]
struct RunOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    killed: bool,
}

/// 运行目标的工作目录解析。
///
/// - 未绑定文件 → 项目根目录
/// - 绑定了文件 → 该文件**所在目录**（如 `admin-web\package.json` → `<项目根>\admin-web`），
///   `npm start` 这类必须在清单文件所在目录执行的命令才能正确运行
/// - 绑定了目录 → 该目录本身
/// - 目录不存在、或解析后越出项目根（bind 写成 `../..`）→ 回退到项目根
fn resolve_run_dir(project_root: Option<&str>, bind: Option<&str>) -> Option<String> {
    let root = project_root?;
    let root_path = Path::new(root);

    let Some(bind) = bind.map(str::trim).filter(|b| !b.is_empty()) else {
        return Some(root.to_string());
    };

    let bind_path = root_path.join(bind);
    let dir = if bind_path.is_dir() {
        bind_path
    } else {
        match bind_path.parent() {
            // parent 为空串表示 bind 就在项目根下（如 "package.json"）
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => root_path.to_path_buf(),
        }
    };

    // 越界防护：解析后的真实路径必须落在项目根内
    match (root_path.canonicalize(), dir.canonicalize()) {
        (Ok(root_canon), Ok(dir_canon)) if dir_canon.starts_with(&root_canon) => {
            Some(clean_path(&dir_canon))
        }
        _ => Some(root.to_string()),
    }
}

/// 运行目标：执行一次性命令并回传输出。
///
/// `bind` 为运行目标绑定的清单文件（项目相对路径），用于决定工作目录：
/// 绑定 `admin-web\package.json` 时命令在 `<项目根>\admin-web` 下执行。
#[tauri::command]
async fn run_target(
    cmd: String,
    project_root: Option<String>,
    bind: Option<String>,
) -> Result<RunOutput, String> {
    let parts = split_cmd(&cmd);
    if parts.is_empty() {
        return Err("空命令".to_string());
    }

    let program = resolve_windows_cmd(&parts[0]);
    let args = parts[1..].to_vec();

    // 工作目录必须在进闭包前算好（project_root 会被 move）
    let run_dir = resolve_run_dir(project_root.as_deref(), bind.as_deref());

    // 后台线程执行，避免阻塞 UI
    tauri::async_runtime::spawn_blocking(move || {
        use std::process::Command;

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Windows 专属：阻止子进程闪黑控制台窗口（见 CREATE_NO_WINDOW 说明）
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        if let Some(ref dir) = run_dir {
            cmd.current_dir(dir);
        }

        let output = cmd.output().map_err(|e| format!("执行失败: {}", e))?;

        Ok(RunOutput {
            exit_code: output.status.code(),
            // 按活动代码页解码：中文 Windows 上 mvn / java / cmd 的输出是 GBK，
            // 直接 from_utf8_lossy 会把这行输出变成乱码（与 agent 侧同一处实现）。
            stdout: harness_engine::exec::decode_output(&output.stdout),
            stderr: harness_engine::exec::decode_output(&output.stderr),
            killed: false,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 在新控制台窗口中启动终端程序（CREATE_NEW_CONSOLE 标志，不经过 PTY）
#[tauri::command]
fn spawn_terminal(cmd: String, project_root: Option<String>) -> Result<(), String> {
    let parts = split_cmd(&cmd);
    if parts.is_empty() {
        return Err("空命令".to_string());
    }

    let program = resolve_windows_cmd(&parts[0]);
    let args = &parts[1..];

    // CLI 程序（powershell、python 等）在 Windows 上需 CREATE_NEW_CONSOLE
    // 创建独立窗口；GUI 程序（如 git-bash.exe）会自行创建窗口，此标志对其无影响。
    // macOS/Linux 上 spawn 默认不新建终端窗口，由调用方所在的终端决定呈现。
    let mut c = std::process::Command::new(program);
    c.args(args);
    #[cfg(windows)]
    c.creation_flags(CREATE_NEW_CONSOLE);
    if let Some(ref dir) = project_root {
        c.current_dir(dir);
    }
    c.spawn().map_err(|e| format!("启动失败: {}", e))?;

    Ok(())
}

// ============================================
// PTY 终端命令
// ============================================

#[tauri::command]
fn pty_spawn(
    window: tauri::Window,
    pty_mgr: tauri::State<'_, Mutex<pty::PtyManager>>,
    cmd: String,
    tab_id: String,
    project_root: Option<String>,
) -> Result<(), String> {
    let mut mgr = pty_mgr.lock().map_err(|e| e.to_string())?;
    mgr.spawn(window, tab_id, &cmd, project_root.as_deref())
}

#[tauri::command]
fn pty_write(
    pty_mgr: tauri::State<'_, Mutex<pty::PtyManager>>,
    tab_id: String,
    data: String,
) -> Result<(), String> {
    let mgr = pty_mgr.lock().map_err(|e| e.to_string())?;
    mgr.write(&tab_id, &data)
}

#[tauri::command]
fn pty_resize(
    pty_mgr: tauri::State<'_, Mutex<pty::PtyManager>>,
    tab_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    let mgr = pty_mgr.lock().map_err(|e| e.to_string())?;
    mgr.resize(&tab_id, rows, cols)
}

#[tauri::command]
fn pty_close(
    pty_mgr: tauri::State<'_, Mutex<pty::PtyManager>>,
    tab_id: String,
) -> Result<(), String> {
    let mut mgr = pty_mgr.lock().map_err(|e| e.to_string())?;
    mgr.close(&tab_id);
    Ok(())
}

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

fn build_line_highlights(
    code: &str,
    spans: &[(u32, u32, &str)],
) -> Result<Vec<LineHighlight>, String> {
    let lines: Vec<&str> = code.lines().collect();
    let mut result = Vec::new();

    for (line_idx, line_text) in lines.iter().enumerate() {
        let line_start = line_byte_offset(code, line_idx + 1);
        let line_end = line_start + line_text.len();

        let mut line_spans = Vec::new();
        for &(start, end, tag) in spans {
            let s = start as usize;
            let e = end as usize;

            if e > line_start && s < line_end {
                let rel_start = s.saturating_sub(line_start);
                let rel_end = if e < line_end {
                    e - line_start
                } else {
                    line_text.len()
                };
                if rel_start < rel_end {
                    line_spans.push(LineSpan {
                        start_col: rel_start,
                        end_col: rel_end,
                        tag: tag.to_string(),
                    });
                }
            }
        }

        // 排序并去重：tree-sitter 会对同一段文本产生多个重叠 capture
        line_spans.sort_by_key(|a| a.start_col);
        let mut deduped: Vec<LineSpan> = Vec::new();
        let mut covered = 0usize;
        for span in line_spans {
            if span.end_col <= covered {
                continue;
            }
            let mut s = span;
            if s.start_col < covered {
                s.start_col = covered;
            }
            covered = s.end_col;
            deduped.push(s);
        }

        result.push(LineHighlight {
            line_number: line_idx + 1,
            text: line_text.to_string(),
            spans: deduped,
        });
    }

    Ok(result)
}

fn line_byte_offset(source: &str, line_number: usize) -> usize {
    if line_number <= 1 {
        return 0;
    }
    source
        .bytes()
        .enumerate()
        .filter(|(_, b)| *b == b'\n')
        .nth(line_number - 2)
        .map(|(i, _)| i + 1)
        .unwrap_or(source.len())
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

/// 配置菜单：保存表单（增量写；空值 = 删除该键）
#[tauri::command]
fn config_form_save(
    scope: String,
    entries: Vec<config::ConfigEntryInput>,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<config::ScopeSaveReport, String> {
    let s = config::Scope::from_str(&scope).ok_or_else(|| {
        format!(
            "无效的作用域: {}。可用: g/global, p/project, r/runtime",
            scope
        )
    })?;
    let mut mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.save_scope_entries(&s, &entries, project_root.as_deref())
}

/// 配置菜单：应用表单（= 保存 + 刷新 IDE 运行时内存里的配置对象）
#[tauri::command]
fn config_form_apply(
    scope: String,
    entries: Vec<config::ConfigEntryInput>,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<config::ScopeSaveReport, String> {
    let s = config::Scope::from_str(&scope).ok_or_else(|| {
        format!(
            "无效的作用域: {}。可用: g/global, p/project, r/runtime",
            scope
        )
    })?;
    let mut mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.apply_scope_entries(&s, &entries, project_root.as_deref())
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

// ============================================
// 外链：WebView 只是我们的画布，不是浏览器（P0）
// ============================================
//
// 症状：agent 回一句「服务已起，访问 http://localhost:8080」，点一下链接，**整块 IDE 被那张网页替换**
// —— 标签栏、文件树、会话全没了，而且回不去。
//
// 为什么一次点击能拆掉整个 IDE：聊天里的 agent 输出走 markdown-it 且开了 `linkify`，裸 URL 会变成
// 真 `<a href>`；而 WebView2 对普通导航的默认动作就是**在本 WebView 里导航过去**。我们的前端全活在
// **一个文档**里（标签页只是 DOM 状态），文档一换，状态跟着一起没。
// （`target="_blank"` 反而不会：wry 没设新窗口处理器时直接把请求吞掉 —— 所以真凶只有一种：普通导航。
//  加上 `on_new_window` 之后连这一种也不再"点了没反应"。）
//
// 这道闸门必须在**后端**：`on_navigation` 是 WebView 的导航闸门，任何来源（a 标签 / location.href /
// form / window.open / 以后某个忘了拦的角落）都得过它。前端的拦截（`ui/external.js`）是**显式**的
// 那一重 —— 在按下那一刻就把意图变成"交给系统浏览器"，连一次失败的导航都不发生。
// 两重都要有：只靠前端＝下一处漏网就是又一次 P0；只靠后端＝点了没反应（浏览器不开）。
//
// 唯一的判据是"这条 URL 是不是应用自己的文档"，结局只有三种：放行 / 交给系统浏览器 / 拒掉。

/// 一次导航的三种结局。
#[derive(Debug, PartialEq, Eq)]
enum Nav {
    /// 应用自己的文档 —— 放行
    Allow,
    /// 外站 —— 拒掉导航，改用系统浏览器打开
    External,
    /// 既不是自家文档、也没有可交给浏览器的地方（同源的非文档路径，如 `tauri.localhost/src/main.rs`）
    /// —— 拒掉。这种链接点下去只会换来一张 404 白页，和跳去外站一样丢状态
    Refuse,
}

/// 应用自己的站点（自定义协议）在各平台的落地：Windows / Android 上是 `http://tauri.localhost`，
/// 其它平台是 `tauri://localhost`。
fn is_app_host(url: &tauri::Url) -> bool {
    match url.scheme() {
        "tauri" => true,
        "http" | "https" => url.host_str() == Some("tauri.localhost"),
        _ => false,
    }
}

/// 两个 URL 是不是同一个站点（scheme + host + port）。
/// 不能用 `Url::origin()`：`tauri://` 这类非特殊 scheme 的 origin 是 opaque，序列化出来是 `null`。
fn same_site(a: &tauri::Url, b: &tauri::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// 这条 URL 是不是"一个文档"（`/` 或 `*.html`）。
/// 只看文档：子资源（css/js）不走导航事件，而同源的 `src/main.rs` 这种相对链接一旦导航过去就是白页。
fn is_document_path(url: &tauri::Url) -> bool {
    let path = url.path();
    path.is_empty() || path == "/" || path.ends_with(".html")
}

/// 导航闸门（纯函数，可测）。
///
/// `dev_url` = `tauri.conf.json` 的 `build.devUrl`（本项目没配）。留着这条是因为 `tauri dev` 一旦改用
/// 开发服务器，判据必须认它 —— 认的依据是**配置里写的那一条**，而不是"凡是 localhost 都放行"：
/// agent 起的服务就在 localhost 上，那正是这次要拦的东西。
fn nav_verdict(url: &tauri::Url, dev_url: Option<&tauri::Url>) -> Nav {
    // WebView 给 iframe 用的空文档，不是"去了别的站点"
    if url.as_str() == "about:blank" {
        return Nav::Allow;
    }
    let own_site = is_app_host(url) || dev_url.is_some_and(|dev| same_site(url, dev));
    if !own_site {
        return Nav::External;
    }
    if is_document_path(url) {
        Nav::Allow
    } else {
        Nav::Refuse
    }
}

/// 外链白名单：只放行 http / https / mailto。
///
/// 这是**能执行之前**的闸门，不是措辞洁癖：`file:` 会让一句"打开 file:///C:/…"变成在系统里点开本地
/// 文件，`javascript:` / `data:text/html` 更是直接执行代码。链接可能来自模型输出、用户手输或页面上
/// 的任意文本，白名单之外一律不交给系统。
fn check_open_url(raw: &str) -> Result<String, String> {
    let url = tauri::Url::parse(raw.trim()).map_err(|e| format!("不是合法的链接：{e}"))?;
    match url.scheme() {
        "http" | "https" | "mailto" => Ok(url.to_string()),
        other => Err(format!(
            "只允许 http / https / mailto 链接，这条是 `{other}:`，已拦下"
        )),
    }
}

/// 用操作系统的默认处理程序打开链接 —— 外链**唯一**的出口。
#[tauri::command]
fn open_external(url: String) -> Result<String, String> {
    let url = check_open_url(&url)?;
    launch_in_browser(&url)?;
    Ok(url)
}

/// 交给操作系统。Windows 走 `ShellExecuteW`（"用默认处理程序打开"就是它），**不走 shell** ——
/// URL 里带 `&` / 空格 / 中文是常态，`cmd /C start` 那套引号规则本项目已经踩过一次。
#[cfg(windows)]
fn launch_in_browser(url: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let hinstance = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    // 返回值**不是**句柄：<= 32 全是错误码（微软文档写明了），所以"成功"的判据是 > 32 而不是"非零"。
    let code = hinstance as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!(
            "系统没有能打开它的程序（ShellExecuteW 返回 {code}）"
        ))
    }
}

#[cfg(not(windows))]
fn launch_in_browser(url: &str) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("系统没有能打开它的程序（{opener}: {e}）"))
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
        .title("Darkhorse Code")
        .inner_size(1200.0, 800.0)
        .decorations(false)
        .center()
        .devtools(true)
        .on_navigation(move |url| match nav_verdict(url, nav_dev_url.as_ref()) {
            Nav::Allow => true,
            Nav::External => {
                // 外站：不放行，改用系统浏览器 —— 用户要的是"打开这个链接"，
                // 不是"把 IDE 换成这个页面"
                if let Err(e) = launch_in_browser(url.as_str()) {
                    eprintln!("[ruyix] 外链打不开：{e}（{url}）");
                }
                false
            }
            Nav::Refuse => {
                eprintln!("[ruyix] 拦下一次应用内导航（不是文档路径）：{url}");
                false
            }
        })
        .on_new_window(move |url, _features| {
            // `target="_blank"` / window.open 走的是这条路，不是导航。默认没人接就被静默吞掉，
            // 于是"点了没反应"；这里跟导航用**同一张判定表**：外站交给系统浏览器。
            if nav_verdict(&url, dev_url.as_ref()) == Nav::External
                && let Err(e) = launch_in_browser(url.as_str())
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
    let config_mgr = Mutex::new(config::ConfigManager::new());
    let pty_mgr = Mutex::new(pty::PtyManager::new());

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(config_mgr)
        .manage(mcp::McpManager::new())
        .manage(pty_mgr)
        .manage(agent::AgentState::new())
        .setup(|app| {
            // 主窗口在这里建（不是 tauri.conf.json 的 app.windows）——
            // 只有 Rust 侧的 Builder 挂得上 on_navigation，也就是外链的道闸，详见 build_main_window
            build_main_window(app.handle())?;

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
            open_project,
            list_dir,
            read_file,
            read_file_base64,
            write_file,
            create_file,
            create_dir,
            delete_path,
            get_execute_status,
            set_execute_entry,
            ai_execute_check,
            lua_translate,
            path_exists,
            rename_path,
            highlight_code,
            is_another_instance,
            get_last_project,
            get_projects,
            set_project_lang,
            update_project,
            delete_project,
            migrate_projects,
            get_run_targets,
            run_target,
            proc_list,
            proc_stop,
            open_external,
            spawn_terminal,
            pty_spawn,
            pty_write,
            pty_resize,
            pty_close,
            config_get,
            config_set,
            config_delete,
            config_form_load,
            config_form_save,
            config_form_apply,
            ai_translate,
            // Agent 命令桥（融合计划 Z3，append-only 注册块）
            agent::agent_run,
            agent::agent_reply,
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
mod tests {
    use super::*;
    use arborium::Highlighter;

    /// SQL 语法高亮：验证 lang-sql feature 启用后 arborium 能识别 "sql" 语言
    #[test]
    fn sql_highlight_works() {
        let mut highlighter = Highlighter::new();
        let spans = highlighter
            .highlight_spans("sql", "SELECT id, name FROM users WHERE age > 18;")
            .expect("SQL 高亮失败");
        assert!(!spans.is_empty(), "SQL 高亮应返回 span");
    }

    /// Java 语法高亮：验证 lang-java feature 启用后 arborium 能识别 "java" 语言
    #[test]
    fn java_highlight_works() {
        let mut highlighter = Highlighter::new();
        let src = "public class Main {\n    public static void main(String[] args) {\n        // 打印\n        System.out.println(\"hello\");\n    }\n}";
        let spans = highlighter
            .highlight_spans("java", src)
            .expect("Java 高亮失败");
        assert!(!spans.is_empty(), "Java 高亮应返回 span");

        // 关键字 public/class/static 应被识别为关键字或类型捕获
        let captures: Vec<&str> = spans.iter().map(|s| s.capture.as_str()).collect();
        assert!(
            captures.iter().any(|c| c.contains("keyword")),
            "Java 应至少产生一个 keyword 捕获，实际: {:?}",
            captures
        );

        // 注释与字符串也应被捕获
        assert!(
            captures.iter().any(|c| c.contains("comment")),
            "Java 应捕获注释，实际: {:?}",
            captures
        );
        assert!(
            captures.iter().any(|c| c.contains("string")),
            "Java 应捕获字符串，实际: {:?}",
            captures
        );

        // 走一遍 highlight_code 的映射链路：capture → theme tag → CSS 类名
        // 若此步为空，说明前端拿不到任何 span（表现为"无高亮"）
        let themed: Vec<(u32, u32, &str)> = spans
            .iter()
            .filter_map(|s| {
                arborium_theme::tag_for_capture(&s.capture)
                    .and_then(arborium_theme::tag_to_name)
                    .map(|name| (s.start, s.end, name))
            })
            .collect();
        assert!(!themed.is_empty(), "Java 捕获应能映射到主题 tag");
        for expected in ["keyword", "string", "comment", "type"] {
            assert!(
                themed.iter().any(|(_, _, name)| *name == expected),
                "Java 高亮应包含 {} 主题 tag，实际: {:?}",
                expected,
                themed.iter().map(|(_, _, n)| *n).collect::<Vec<_>>()
            );
        }

        // 端到端：行级高亮结果应携带 span（前端据此渲染 <span class="tok-*">）
        let lines = build_line_highlights(src, &themed).expect("构建行级高亮失败");
        assert_eq!(lines.len(), 6, "6 行源码应生成 6 行高亮");
        assert!(
            lines.iter().any(|l| !l.spans.is_empty()),
            "至少一行应包含高亮 span"
        );
    }

    // ============================================
    // 运行目录解析（0.0.4 修复：绑定清单文件后应在该文件所在目录运行）
    // ============================================

    /// 临时项目目录，Drop 时自动清理
    struct TempProj(std::path::PathBuf);

    impl TempProj {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间异常")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "dh-code-run-test-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("创建临时目录失败");
            TempProj(dir)
        }

        fn path(&self) -> String {
            self.0.to_string_lossy().to_string()
        }

        /// 写文件（自动创建父目录）
        fn write(&self, rel: &str, content: &str) {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("创建父目录失败");
            }
            std::fs::write(&p, content).expect("写文件失败");
        }
    }

    impl Drop for TempProj {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 路径比较用规范化：统一分隔符、去尾部斜杠、忽略大小写
    fn norm(p: &str) -> String {
        p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
    }

    /// 期望路径对齐 canonicalize 结果：resolve_run_dir 返回真实路径，
    /// 而 macOS 的 temp_dir() 是 /private/var 的符号链接（/var/...），未解析
    fn canon(p: &std::path::Path) -> String {
        clean_path(&p.canonicalize().expect("canonicalize 失败"))
    }

    #[test]
    fn run_dir_defaults_to_project_root() {
        let proj = TempProj::new("root");
        assert_eq!(
            resolve_run_dir(Some(&proj.path()), None).map(|d| norm(&d)),
            Some(norm(&proj.path())),
            "未绑定文件时应使用项目根目录"
        );
        assert_eq!(
            resolve_run_dir(Some(&proj.path()), Some("  ")).map(|d| norm(&d)),
            Some(norm(&proj.path())),
            "空白 bind 应等价于未绑定"
        );
    }

    /// 复现用例：bind = admin-web\package.json、cmd = npm start
    /// 期望工作目录是 <项目根>/admin-web（修复前是项目根 → 报错）
    #[test]
    fn run_dir_uses_bind_file_parent_dir() {
        let proj = TempProj::new("bind");
        proj.write("admin-web/package.json", "{\"name\":\"admin-web\"}");

        // Windows 配置里的 bind 可能写作反斜杠分隔；
        // Unix 上反斜杠是普通文件名字符，不作为分隔符解析，只测正斜杠
        #[cfg(windows)]
        let binds = [r"admin-web\package.json", "admin-web/package.json"];
        #[cfg(not(windows))]
        let binds = ["admin-web/package.json"];

        for bind in binds {
            let dir = resolve_run_dir(Some(&proj.path()), Some(bind)).expect("应解析出工作目录");
            assert_eq!(
                norm(&dir),
                norm(&canon(&proj.0.join("admin-web"))),
                "bind={} 时应在该文件所在目录运行",
                bind
            );
        }
    }

    #[test]
    fn run_dir_bind_at_project_root_stays_at_root() {
        let proj = TempProj::new("rootbind");
        proj.write("package.json", "{}");
        let dir = resolve_run_dir(Some(&proj.path()), Some("package.json")).expect("应有工作目录");
        assert_eq!(
            norm(&dir),
            norm(&canon(&proj.0)),
            "根目录下的清单文件 → 项目根"
        );
    }

    #[test]
    fn run_dir_bind_dir_uses_dir_itself() {
        let proj = TempProj::new("dirbind");
        std::fs::create_dir_all(proj.0.join("admin-web")).expect("创建目录失败");
        let dir = resolve_run_dir(Some(&proj.path()), Some("admin-web")).expect("应有工作目录");
        assert_eq!(norm(&dir), norm(&canon(&proj.0.join("admin-web"))));
    }

    #[test]
    fn run_dir_missing_dir_falls_back_to_root() {
        let proj = TempProj::new("missing");
        let dir = resolve_run_dir(Some(&proj.path()), Some("not-exist/package.json"))
            .expect("应有工作目录");
        assert_eq!(norm(&dir), norm(&proj.path()), "目录不存在应回退项目根");
    }

    #[test]
    fn run_dir_blocks_path_escape() {
        let proj = TempProj::new("escape");
        let outside_name = format!("dh-code-run-test-escape-outside-{}", std::process::id());
        let outside = proj
            .0
            .parent()
            .expect("临时目录应有父目录")
            .join(&outside_name);
        std::fs::create_dir_all(&outside).expect("创建目录失败");

        let dir = resolve_run_dir(
            Some(&proj.path()),
            Some(&format!("../{outside_name}/package.json")),
        )
        .expect("应有工作目录");
        assert_eq!(
            norm(&dir),
            norm(&proj.path()),
            "越出项目根的 bind 应回退项目根"
        );

        let _ = std::fs::remove_dir_all(&outside);
    }

    /// 端到端：走真实 run_target（命令解析 + 工作目录设置），
    /// Windows 用 `cmd /c cd`、Unix 用 `pwd` 回显实际工作目录
    #[test]
    fn run_target_executes_in_bind_dir() {
        let proj = TempProj::new("e2e");
        proj.write("admin-web/package.json", "{}");

        #[cfg(windows)]
        let (cmd, bind) = ("cmd /c cd", r"admin-web\package.json");
        #[cfg(not(windows))]
        let (cmd, bind) = ("pwd", "admin-web/package.json");

        let out = tauri::async_runtime::block_on(run_target(
            cmd.to_string(),
            Some(proj.path()),
            Some(bind.to_string()),
        ))
        .expect("run_target 执行失败");

        assert_eq!(out.exit_code, Some(0), "命令应执行成功: {}", out.stderr);
        assert!(
            out.stdout.to_lowercase().contains("admin-web"),
            "实际工作目录应包含 admin-web，实际输出: {:?}",
            out.stdout
        );
    }

    // ---- 外链闸门（P0：一次链接点击不能顶掉整个 IDE）----

    fn u(s: &str) -> tauri::Url {
        tauri::Url::parse(s).expect("测试用 URL 应当合法")
    }

    /// 用户踩到的那个 P0 原样复现：agent 起了服务、回了 `http://localhost:8080`，点一下整块 IDE 被换掉。
    /// 判据是"是不是自家文档"，所以 **localhost 不是自家页面** —— 这正是这次要拦的东西。
    #[test]
    fn localhost_is_not_an_app_document() {
        assert_eq!(
            nav_verdict(&u("http://localhost:8080/"), None),
            Nav::External
        );
        assert_eq!(
            nav_verdict(&u("http://localhost:8080/actuator/health"), None),
            Nav::External
        );
        assert_eq!(
            nav_verdict(&u("http://127.0.0.1:8080/"), None),
            Nav::External
        );
        assert_eq!(nav_verdict(&u("http://[::1]:8080/"), None), Nav::External);
        assert_eq!(
            nav_verdict(&u("https://newest-ai.com"), None),
            Nav::External
        );
    }

    /// 自家文档放行；同文档锚点还得放行 —— 拦了"跳到某节"就成了点了没反应。
    #[test]
    fn app_documents_are_allowed_along_with_in_page_anchors() {
        assert_eq!(nav_verdict(&u("http://tauri.localhost/"), None), Nav::Allow);
        assert_eq!(
            nav_verdict(&u("http://tauri.localhost/index.html"), None),
            Nav::Allow
        );
        assert_eq!(
            nav_verdict(&u("tauri://localhost/index.html"), None),
            Nav::Allow
        );
        assert_eq!(nav_verdict(&u("about:blank"), None), Nav::Allow);
        assert_eq!(
            nav_verdict(&u("http://tauri.localhost/index.html#sessions"), None),
            Nav::Allow
        );
    }

    /// 同源但**不是文档**的路径（markdown 里的相对链接 `src/main.rs` 解析出来就是它）不能放行：
    /// 导航过去是一张 404 白页，和跳去外站一样丢状态；而它也没有"交给浏览器"的出口 → 拒。
    #[test]
    fn same_site_non_documents_are_refused_without_an_exit() {
        assert_eq!(
            nav_verdict(&u("http://tauri.localhost/src/main.rs"), None),
            Nav::Refuse
        );
        assert_eq!(
            nav_verdict(&u("http://tauri.localhost/styles.css"), None),
            Nav::Refuse
        );
    }

    /// 开发服务器（配置里写了才认）：认的是**配置那一条**，不是"凡是 localhost"。
    #[test]
    fn a_configured_dev_url_is_recognized_without_whitelisting_localhost() {
        let dev = u("http://localhost:1420/");
        assert_eq!(
            nav_verdict(&u("http://localhost:1420/index.html"), Some(&dev)),
            Nav::Allow
        );
        assert_eq!(
            nav_verdict(&u("http://localhost:8080/"), Some(&dev)),
            Nav::External,
            "另一个 localhost 端口不是开发服务器"
        );
        assert_eq!(
            nav_verdict(&u("https://newest-ai.com"), Some(&dev)),
            Nav::External
        );
    }

    /// 外链出口的白名单：只有三种 scheme 能落到操作系统，
    /// `file:` / `javascript:` / `data:` 连试都不试（这是**能执行之前**的闸门）。
    #[test]
    fn open_url_whitelists_exactly_three_schemes() {
        assert_eq!(
            check_open_url("  https://newest-ai.com/x?a=1&b=2  ").expect("https 应当放行"),
            "https://newest-ai.com/x?a=1&b=2",
            "放行时要带上完整查询串"
        );
        assert!(check_open_url("http://localhost:8080/actuator").is_ok());
        assert!(check_open_url("mailto:yujianboisme@outlook.com").is_ok());
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "not a url",
            "",
        ] {
            assert!(check_open_url(bad).is_err(), "{bad} 必须被拦下");
        }
    }
}
