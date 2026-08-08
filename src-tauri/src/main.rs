#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;

use std::path::Path;
use std::sync::Mutex;

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
fn highlight_python(code: String) -> Result<Vec<LineHighlight>, String> {
    use arborium::Highlighter;

    let mut highlighter = Highlighter::new();
    let spans = highlighter
        .highlight_spans("python", &code)
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
async fn run_target(cmd: String) -> Result<RunOutput, String> {
    let parts = split_cmd(&cmd);
    if parts.is_empty() {
        return Err("空命令".to_string());
    }

    let program = parts[0].clone();
    let args = parts[1..].to_vec();

    // 后台线程执行，避免阻塞 UI
    tauri::async_runtime::spawn_blocking(move || {
        use std::process::Command;

        let output = Command::new(&program)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
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

/// 按空格拆分命令行，支持引号包裹，保留 `\`（不转义）
fn split_cmd(cmd: &str) -> Vec<String> {
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

    tauri::Builder::default()
        .manage(config_mgr)
        .invoke_handler(tauri::generate_handler![
            open_project,
            list_dir,
            read_file,
            highlight_python,
            get_last_project,
            get_projects,
            get_run_targets,
            run_target,
            config_get,
            config_set,
            config_delete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
