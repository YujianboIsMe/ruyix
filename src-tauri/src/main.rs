#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::Path;

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
fn open_project(path: String) -> Result<ProjectInfo, String> {
    let p = Path::new(&path);

    if !p.exists() {
        return Err(format!("路径不存在: {}", p.display()));
    }
    if !p.is_dir() {
        return Err("路径不是目录，请输入项目文件夹路径".to_string());
    }

    let canonical = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let name = canonical
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    Ok(ProjectInfo {
        name,
        path: clean_path(&canonical),
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

        // 隐藏以 . 开始的文件夹，但不隐藏以 . 开始的文件
        if is_dir && name.starts_with('.') {
            continue;
        }

        entries.push(DirEntry {
            name,
            path: clean_path(&entry.path()),
            is_dir,
        });
    }

    // 文件夹在前，然后按名称字母排序（不区分大小写）
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

    // 获取原始 highlight spans（字节偏移 + capture 名称）
    let spans = highlighter
        .highlight_spans("python", &code)
        .map_err(|e| e.to_string())?;

    // 为每个 span 解析出 CSS 类名 tag
    // 使用 theme 系统: capture 名 → tag (如 "keyword", "function", "string" 等)
    let themed: Vec<(u32, u32, &str)> = spans
        .iter()
        .filter_map(|s| {
            arborium_theme::tag_for_capture(&s.capture)
                .map(|tag| (s.start, s.end, tag))
        })
        .collect();

    // 将 spans 按行组织
    build_line_highlights(&code, &themed)
}

/// 将字节偏移的 spans 转换为按行组织的 highlight 数据
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

        result.push(LineHighlight {
            line_number: line_idx + 1,
            text: line_text.to_string(),
            spans: line_spans,
        });
    }

    Ok(result)
}

/// 计算第 N 行（1-based）在源代码中的字节偏移量
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
// 入口
// ============================================

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            open_project,
            list_dir,
            read_file,
            highlight_python
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
