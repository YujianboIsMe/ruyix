#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod config;
mod pty;

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::sync::Mutex;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::Emitter;
use tauri::Manager;

/// CREATE_NEW_CONSOLE — 为新进程创建独立控制台窗口
const CREATE_NEW_CONSOLE: u32 = 0x00000010;

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

    // 持久化到 projects 配置
    if let Ok(cfg) = config_mgr.lock() {
        let _ = cfg.set_current_project(&clean);
    }

    Ok(ProjectInfo {
        name,
        path: clean,
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
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建父目录失败: {}", e))?;
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
}

#[tauri::command]
fn get_execute_status(
    path: String,
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<ExecuteStatus, String> {
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    if ext.is_empty() {
        return Ok(ExecuteStatus { known: Some(false), has_target: false, target_name: None });
    }

    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    let exec_map = mgr.load_execute_map();
    let known = exec_map.get(&ext).copied();

    let mut has_target = false;
    let mut target_name = None;
    if known == Some(true) {
        if let Some(root) = project_root {
            if let Ok(targets) = mgr.load_run_targets(Some(&root)) {
                for t in &targets {
                    if let Some(ref cmd) = t.cmd {
                        if cmd.contains(&path) {
                            has_target = true;
                            target_name = t.name.clone().or_else(|| Some(t.key.clone()));
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(ExecuteStatus { known, has_target, target_name })
}

#[tauri::command]
fn set_execute_entry(
    ext: String,
    can_run: bool,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.save_execute_entry(&ext, can_run)
}

#[tauri::command]
async fn ai_execute_check(
    path: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<String, String> {
    ai::check_executable(&config_mgr, &path).await
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
    std::fs::rename(src, Path::new(&to))
        .map_err(|e| format!("重命名失败: {}", e))
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

/// 获取上次打开的项目路径（供前端启动时自动打开）
#[tauri::command]
fn get_last_project(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Option<String>, String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    Ok(cfg.load_projects().current)
}

/// 获取所有已知项目列表
#[tauri::command]
fn get_projects(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Vec<String>, String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    Ok(cfg.load_projects().list)
}

#[tauri::command]
fn get_run_targets(
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Vec<config::RunTarget>, String> {
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.load_run_targets(project_root.as_deref())
}

#[derive(serde::Serialize, Clone)]
struct RunOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    killed: bool,
}

#[tauri::command]
async fn run_target(cmd: String, project_root: Option<String>) -> Result<RunOutput, String> {
    let parts = split_cmd(&cmd);
    if parts.is_empty() {
        return Err("空命令".to_string());
    }

    let program = resolve_windows_cmd(&parts[0]);
    let args = parts[1..].to_vec();

    // 后台线程执行，避免阻塞 UI
    tauri::async_runtime::spawn_blocking(move || {
        use std::process::Command;

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(ref dir) = project_root {
            cmd.current_dir(dir);
        }

        let output = cmd.output()
            .map_err(|e| format!("执行失败: {}", e))?;

        Ok(RunOutput {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
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

    // CREATE_NEW_CONSOLE 为 CLI 程序（powershell、python 等）创建独立窗口
    // GUI 程序（如 git-bash.exe）会自行创建窗口，此标志对其无影响
    let mut c = std::process::Command::new(program);
    c.args(args).creation_flags(CREATE_NEW_CONSOLE);
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

/// 按空格拆分命令行，支持引号包裹，保留 `\`（不转义）
pub fn split_cmd(cmd: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut quote_char = '"';

    for ch in cmd.chars() {
        if in_quote {
            if ch == quote_char {
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
        if p.extension().is_none() {
            if let Ok(path_var) = std::env::var("PATH") {
                for dir in path_var.split(';') {
                    let candidate = std::path::Path::new(dir).join(format!("{}.cmd", program));
                    if candidate.exists() {
                        return format!("{}.cmd", program);
                    }
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
                let rel_start = if s > line_start { s - line_start } else { 0 };
                let rel_end = if e < line_end { e - line_start } else { line_text.len() };
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
        line_spans.sort_by(|a, b| a.start_col.cmp(&b.start_col));
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
    let s = config::Scope::from_str(&scope)
        .ok_or_else(|| format!("无效的作用域: {}。可用: g/global, p/project, r/runtime", scope))?;
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
    let s = config::Scope::from_str(&scope)
        .ok_or_else(|| format!("无效的作用域: {}。可用: g/global, p/project, r/runtime", scope))?;
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
    let s = config::Scope::from_str(&scope)
        .ok_or_else(|| format!("无效的作用域: {}。可用: g/global, p/project, r/runtime", scope))?;
    let mut mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.config_delete(&s, &key, project_root.as_deref())
}

// ============================================
// 入口
// ============================================

fn main() {
    let config_mgr = Mutex::new(config::ConfigManager::new());
    let pty_mgr = Mutex::new(pty::PtyManager::new());

    tauri::Builder::default()
        .manage(config_mgr)
        .manage(pty_mgr)
        .setup(|app| {
            // 注册原生 Ctrl+S 快捷键 — 即使 WebView2 拦截了 JS 的 Ctrl+S，
            // 原生菜单 accelerator 仍能在 OS 层面捕获该组合键
            let save = MenuItemBuilder::with_id("save", "保存")
                .accelerator("CmdOrCtrl+S")
                .build(app)?;
            let menu = MenuBuilder::new(app)
                .item(&save)
                .build()?;
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
            write_file,
            create_file,
            create_dir,
            delete_path,
            get_execute_status,
            set_execute_entry,
            ai_execute_check,
            path_exists,
            rename_path,
            highlight_code,
            get_last_project,
            get_projects,
            get_run_targets,
            run_target,
            spawn_terminal,
            pty_spawn,
            pty_write,
            pty_resize,
            pty_close,
            config_get,
            config_set,
            config_delete,
            ai_translate,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
