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

/// 语法高亮的 tag（前端 CSS 类是 `tok-<name>`）。
///
/// 取值集合是**闭的**：`arborium_theme::tag_to_name` 的 match 只有 27 个出口，
/// 而它上游的 `.and_then()` 已经把不认识的捕获名滤掉了 —— 也就是说**今天**这条链路
/// 最多也只能产出这 27 个名字。既然集合是闭的，就没有理由让每个 span 各自扛一个
/// `String`：枚举化之后打错一个名字是编译错误，而不是"这个 span 悄悄没颜色"。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok {
    Keyword,
    Function,
    String,
    Comment,
    Type,
    Variable,
    Constant,
    Number,
    Operator,
    Punctuation,
    Property,
    Attribute,
    Tag,
    Macro,
    Label,
    Namespace,
    Constructor,
    Title,
    Strong,
    Emphasis,
    Link,
    Literal,
    Strikethrough,
    DiffAdd,
    DiffDelete,
    Embedded,
    Error,
}

/// 名字 ↔ 枚举的**唯一来源**。`from_name` / `name` 都查这张表，不各写一份 match ——
/// 两份 match 迟早会有一份忘了改。
const TOK_TABLE: &[(&str, Tok)] = &[
    ("keyword", Tok::Keyword),
    ("function", Tok::Function),
    ("string", Tok::String),
    ("comment", Tok::Comment),
    ("type", Tok::Type),
    ("variable", Tok::Variable),
    ("constant", Tok::Constant),
    ("number", Tok::Number),
    ("operator", Tok::Operator),
    ("punctuation", Tok::Punctuation),
    ("property", Tok::Property),
    ("attribute", Tok::Attribute),
    ("tag", Tok::Tag),
    ("macro", Tok::Macro),
    ("label", Tok::Label),
    ("namespace", Tok::Namespace),
    ("constructor", Tok::Constructor),
    ("title", Tok::Title),
    ("strong", Tok::Strong),
    ("emphasis", Tok::Emphasis),
    ("link", Tok::Link),
    ("literal", Tok::Literal),
    ("strikethrough", Tok::Strikethrough),
    ("diff-add", Tok::DiffAdd),
    ("diff-delete", Tok::DiffDelete),
    ("embedded", Tok::Embedded),
    ("error", Tok::Error),
];

impl Tok {
    /// 表外的名字 → `None`（丢弃该 span）。与旧版 `.and_then(tag_to_name)` 的行为一致。
    fn from_name(name: &str) -> Option<Tok> {
        TOK_TABLE.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
    }

    /// CSS 类名后缀。
    fn name(self) -> &'static str {
        TOK_TABLE
            .iter()
            .find(|(_, t)| *t == self)
            .map(|(n, _)| *n)
            .expect("Tok 的每个取值都必须在 TOK_TABLE 里")
    }
}

/// `highlight_code` 的返回载荷。两处是刻意的：
///
/// 1. **不回传正文**。前端手里就有（`tab.content`），逐行回传一遍等于把文件复制一份
///    再走一遍 JSON。而且后端用 `code.lines()` 收行（**吃掉末尾空行**），前端
///    `split("\n")` 不吃 —— 拿后端的行去拼 textarea 的值，就会让"以换行结尾的文件"
///    少一个末尾 `\n`，用户一按键 `tab.content = textarea.value` 把差值固化，写回时
///    末尾换行就真没了。所以**行由前端自己切**，后端只回答"第 i 行的片段在哪"。
/// 2. **tag 走名表 + 下标**。整份响应里名表只出现一次（≤27 项），span 里只放一个下标。
///    比"每个 span 带一个 tag 字符串"省掉几乎全部字节；也不会因为两边枚举顺序不一致
///    而整体错色 —— 顺序漂移在这个设计下最多是"查到另一个名字"，而裸数字编码会是全篇错色。
#[derive(serde::Serialize, Clone)]
struct HighlightPayload {
    /// tag 名表：按首次出现顺序去重，span 里的第三个数是它的下标
    tags: Vec<&'static str>,
    /// 每行一串扁平三元组 `[start, end, tag_idx, start, end, tag_idx, ...]`；
    /// 下标就是行号（0 基），空行是 `[]`。行数与 `code.lines().count()` 一致，
    /// 可能比前端的 `split("\n")` **少一行**（末行换行）—— 前端按不足处理即可。
    lines: Vec<Vec<u32>>,
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
async fn highlight_code(language: String, code: String) -> Result<HighlightPayload, String> {
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

/// **增量**读一段托管进程的日志 —— 面板的"输出"标签页靠它做 `tail -f`。
///
/// `offset = None` 是首读（只回看尾部 128KB）；之后带上一轮返回的 `next_offset` 续读。
/// 切分点只落在换行上，理由见 `harness_engine::proc::read_log_chunk`。
///
/// 读文件是微秒级的事（不像 `proc_stop` 要等进程退），不必 `spawn_blocking`。
#[tauri::command]
fn proc_log_read(
    pid: u32,
    offset: Option<u64>,
    max_bytes: Option<usize>,
) -> Result<harness_engine::proc::LogChunk, String> {
    harness_engine::proc::read_log_chunk(
        pid,
        offset,
        max_bytes.unwrap_or(harness_engine::proc::LOG_CHUNK_MAX),
    )
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

/// 把 tree-sitter 报的字节区间摊到每一行上。
///
/// **性能约束（硬要求，不要再退化）**：只允许对源码做一次线性扫描。
///
/// 旧版每行都调用一次 `line_byte_offset(code, n)`，而那个函数每次都从文件头重新数
/// `\n` —— 整体 O(行数 × 文件字节数)；内层又每行遍历全部 span —— 再加一层
/// O(行数 × span 数)。release 实测：5 千行 172ms、1 万行 552ms、2.5 万行 4.0s、
/// 5 万行 16.4s（tree-sitter 自己是线性的）。而高亮挂在自动保存上 —— 打字每停顿
/// 一秒就重跑一次，于是文件一大就整个编辑器无响应。
///
/// 现在：一次扫出每行起始偏移 → 每个 span 二分定位所在行 → 跨行 span 按行切分。
/// 片段语义（切点、排序、去重）与旧实现逐字节相同（`slow_reference` + 等价性测试钉住），
/// 只是输出换成了紧凑载荷（见 `HighlightPayload`）。
fn build_line_highlights(
    code: &str,
    spans: &[(u32, u32, &str)],
) -> Result<HighlightPayload, String> {
    let lines: Vec<&str> = code.lines().collect();

    // 第 k 行的起始字节偏移 = 第 k 个 '\n' 之后。一次扫完，
    // 与旧版反复调用 line_byte_offset(code, k + 1) 的结果逐一相同。
    let mut starts: Vec<usize> = Vec::with_capacity(lines.len() + 1);
    starts.push(0);
    for (i, b) in code.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }

    // 二分定位每个 span 覆盖的行区间，再按行切分（跨行的注释/字符串会横跨多行）
    let mut per_line: Vec<Vec<(usize, usize, Tok)>> = vec![Vec::new(); lines.len()];
    for &(start, end, tag) in spans {
        // 名字 → 枚举在这里就做掉：下游全是 Copy 的小整数，不再碰字符串
        let Some(tok) = Tok::from_name(tag) else {
            continue;
        };
        let (s, e) = (start as usize, end as usize);
        if e <= s {
            continue;
        }
        let mut li = starts.partition_point(|&x| x <= s).saturating_sub(1);
        while li < lines.len() {
            let line_start = starts[li];
            let line_end = line_start + lines[li].len();
            if line_start >= e {
                break;
            }
            let rel_start = s.max(line_start) - line_start;
            let rel_end = e.min(line_end) - line_start;
            if rel_start < rel_end {
                per_line[li].push((rel_start, rel_end, tok));
            }
            li += 1;
        }
    }

    let mut tags: Vec<&'static str> = Vec::new();
    let mut out_lines: Vec<Vec<u32>> = Vec::with_capacity(per_line.len());
    // 直接消费 per_line：每行取走自己的片段，免得再用下标索引一遍
    for mut line_spans in per_line {
        // 排序并去重：tree-sitter 会对同一段文本产生多个重叠 capture
        line_spans.sort_by_key(|a| a.0);
        let mut flat: Vec<u32> = Vec::new();
        let mut covered = 0usize;
        for (start_col, end_col, tok) in line_spans {
            if end_col <= covered {
                continue;
            }
            let start_col = start_col.max(covered);
            covered = end_col;
            // 名表按首次出现顺序收集；重复出现的只留下来一次
            let idx = match tags.iter().position(|t| *t == tok.name()) {
                Some(i) => i,
                None => {
                    tags.push(tok.name());
                    tags.len() - 1
                }
            };
            flat.push(start_col as u32);
            flat.push(end_col as u32);
            flat.push(idx as u32);
        }
        out_lines.push(flat);
    }

    Ok(HighlightPayload {
        tags,
        lines: out_lines,
    })
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
            proc_log_read,
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
            config_schema,
            ai_translate,
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
mod tests {
    use super::*;
    use arborium::Highlighter;

    /// tree-sitter 跑一遍，转成 build_line_highlights 吃的 (start, end, tag)
    fn themed_spans(lang: &str, src: &str) -> Vec<(u32, u32, String)> {
        let mut highlighter = Highlighter::new();
        let spans = highlighter.highlight_spans(lang, src).expect("高亮失败");
        spans
            .iter()
            .filter_map(|s| {
                arborium_theme::tag_for_capture(&s.capture)
                    .and_then(arborium_theme::tag_to_name)
                    .map(|name| (s.start, s.end, name.to_string()))
            })
            .collect()
    }

    /// 旧实现的等价物，**只作测试参照**：每行重扫全文件找行首 + 每行遍历全部 span。
    /// 刻意保留它的 O(n²) —— 用来钉住线性重写的输出，并给出复杂度比值。
    /// 片段语义（切点 / 排序 / 去重）与生产实现各自独立写一遍，两边都错成同一个样子
    /// 才会通过，所以它同时也是"没把语义顺手改歪"的参照。
    fn slow_reference(code: &str, spans: &[(u32, u32, &str)]) -> HighlightPayload {
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

        let lines: Vec<&str> = code.lines().collect();
        let mut tags: Vec<&'static str> = Vec::new();
        let mut out_lines: Vec<Vec<u32>> = Vec::new();
        for (line_idx, line_text) in lines.iter().enumerate() {
            let line_start = line_byte_offset(code, line_idx + 1);
            let line_end = line_start + line_text.len();

            let mut line_spans: Vec<(usize, usize, Tok)> = Vec::new();
            for &(start, end, tag) in spans {
                let Some(tok) = Tok::from_name(tag) else {
                    continue;
                };
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
                        line_spans.push((rel_start, rel_end, tok));
                    }
                }
            }

            line_spans.sort_by_key(|a| a.0);
            let mut flat: Vec<u32> = Vec::new();
            let mut covered = 0usize;
            for (start_col, end_col, tok) in line_spans {
                if end_col <= covered {
                    continue;
                }
                let start_col = start_col.max(covered);
                covered = end_col;
                let idx = match tags.iter().position(|t| *t == tok.name()) {
                    Some(i) => i,
                    None => {
                        tags.push(tok.name());
                        tags.len() - 1
                    }
                };
                flat.push(start_col as u32);
                flat.push(end_col as u32);
                flat.push(idx as u32);
            }
            out_lines.push(flat);
        }
        HighlightPayload {
            tags,
            lines: out_lines,
        }
    }

    /// 线性重写必须与旧实现**逐字节相同**。
    /// 覆盖的形态：空文件 / 只有换行 / 无尾换行 / CRLF / 跨行块注释 / 跨行字符串 /
    /// 以及一份有规模的源码（让 span 数量和跨行覆盖接近真实文件）。
    #[test]
    fn highlight_output_is_unchanged_by_the_linear_rewrite() {
        let mut sources = vec![
            String::new(),
            "\n".into(),
            "\n\n\n".into(),
            "no trailing newline".into(),
            "a\nb\nc".into(),
            "// 注释\nfn main() {\n    let s = \"a\\nb\";\n}\n".into(),
            "/* 跨行\n   注释 */\nfn f() {}\n".into(),
            "fn a() {}\r\nfn b() {}\r\n".into(),
            "let x = 1;".into(),
        ];
        let mut big = String::new();
        for i in 0..400 {
            big.push_str(&format!(
                "fn f{i}(x: i32) -> i32 {{\n    // note {i}\n    x + {i}\n}}\n"
            ));
        }
        sources.push(big);

        for src in &sources {
            for lang in ["rust", "javascript", "python", "markdown"] {
                let owned = themed_spans(lang, src);
                let spans: Vec<(u32, u32, &str)> =
                    owned.iter().map(|(s, e, t)| (*s, *e, t.as_str())).collect();

                let linear = build_line_highlights(src, &spans).unwrap();
                let reference = slow_reference(src, &spans);
                assert_eq!(
                    serde_json::to_string(&linear).unwrap(),
                    serde_json::to_string(&reference).unwrap(),
                    "{lang} 上线性重写与旧实现输出不一致（源码 {} 字节）",
                    src.len()
                );
            }
        }
    }

    /// **复杂度金丝雀**：有人再把"逐行重扫全文件"写回来，这条必须转红。
    ///
    /// 判据用**比值**而不是绝对耗时 —— 机器快慢不影响结论。阈值放到 3 倍是刻意留的
    /// 余量（实测余量在 10 倍以上），避免慢机器 / CI 上假红。
    #[test]
    fn highlight_does_not_rescan_the_file_per_line() {
        let mut src = String::new();
        for i in 0..400 {
            src.push_str(&format!(
                "fn f{i}(x: i32) -> i32 {{\n    // note {i}\n    x + {i}\n}}\n"
            ));
        }
        let owned = themed_spans("rust", &src);
        let spans: Vec<(u32, u32, &str)> =
            owned.iter().map(|(s, e, t)| (*s, *e, t.as_str())).collect();

        let t0 = std::time::Instant::now();
        let linear = build_line_highlights(&src, &spans).unwrap();
        let d_linear = t0.elapsed().as_secs_f64();

        let t1 = std::time::Instant::now();
        let reference = slow_reference(&src, &spans);
        let d_reference = t1.elapsed().as_secs_f64();

        let span_count = |p: &HighlightPayload| p.lines.iter().map(|l| l.len() / 3).sum::<usize>();
        assert_eq!(linear.lines.len(), reference.lines.len(), "行数不一致");
        assert_eq!(
            span_count(&linear),
            span_count(&reference),
            "span 总数不一致"
        );

        assert!(
            d_linear * 3.0 < d_reference,
            "build_line_highlights 疑似又变成逐行重扫：线性版 {:.1}ms，旧算法 {:.1}ms（比值 {:.1}×，要求 >3×）",
            d_linear * 1e3,
            d_reference * 1e3,
            d_reference / d_linear.max(1e-9)
        );
    }

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
        let payload = build_line_highlights(src, &themed).expect("构建行级高亮失败");
        assert_eq!(payload.lines.len(), 6, "6 行源码应生成 6 行高亮");
        assert!(
            payload.lines.iter().any(|l| !l.is_empty()),
            "至少一行应包含高亮 span"
        );
    }

    // ============================================
    // 高亮载荷的形状（P3）
    // ============================================

    /// 一段有规模的真源码：400 行、每行都有 span、还带中文注释（顺带覆盖多字节行）。
    fn sample_source() -> String {
        let mut src = String::new();
        for i in 0..400 {
            src.push_str(&format!(
                "fn f{i}(x: i32) -> i32 {{\n    // 注释 {i}\n    x + {i}\n}}\n"
            ));
        }
        src
    }

    fn payload_of(lang: &str, src: &str) -> HighlightPayload {
        let owned = themed_spans(lang, src);
        let spans: Vec<(u32, u32, &str)> =
            owned.iter().map(|(s, e, t)| (*s, *e, t.as_str())).collect();
        build_line_highlights(src, &spans).expect("构建载荷失败")
    }

    /// **载荷里不许出现文件正文。**
    ///
    /// 后端回传逐行 `text` 是纯浪费（前端手里就有 `tab.content`），而且 `code.lines()`
    /// 会吃掉末尾空行、前端 `split("\n")` 不吃 —— 拿后端的行拼 textarea 的值，就会把
    /// "以换行结尾的文件"的末尾换行弄丢。有人为了"省前端一次 split"把它加回来，这条要红。
    #[test]
    fn highlight_payload_does_not_echo_the_source_text() {
        let src = "fn main() {\n    let secret = \"UNIQUE_MARKER_9f3a\";\n}\n";
        let json = serde_json::to_string(&payload_of("rust", src)).unwrap();
        assert!(
            !json.contains("UNIQUE_MARKER_9f3a") && !json.contains("main"),
            "载荷里出现了源码正文（逐行回传等于把文件复制一份再走一遍 JSON）：{json}"
        );
        assert!(
            !json.contains("start_col") && !json.contains("line_number"),
            "载荷退回了逐 span 的对象编码 —— key 名重复才是载荷的大头，扁平成三元组才有意义：{json}"
        );
    }

    /// 载荷必须**显著小于**旧形状（逐行对象 + 逐 span 对象 + 回传正文）。
    ///
    /// 判据用**比值**，和复杂度金丝雀同一个路子：机器、内容都不影响结论。
    /// 旧形状在同一份片段上现搭出来比 —— 这样比的只是"编码"，不是"高亮质量"。
    #[test]
    fn highlight_payload_is_much_smaller_than_the_legacy_shape() {
        let src = sample_source();
        let payload = payload_of("rust", &src);

        let lines: Vec<&str> = src.lines().collect();
        let legacy: Vec<serde_json::Value> = lines
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let flat = payload.lines.get(i).cloned().unwrap_or_default();
                let spans: Vec<serde_json::Value> = (0..flat.len())
                    .step_by(3)
                    .map(|k| {
                        serde_json::json!({
                            "start_col": flat[k],
                            "end_col": flat[k + 1],
                            "tag": payload.tags[flat[k + 2] as usize],
                        })
                    })
                    .collect();
                serde_json::json!({ "line_number": i + 1, "text": text, "spans": spans })
            })
            .collect();

        let new_len = serde_json::to_string(&payload).unwrap().len();
        let old_len = serde_json::to_string(&legacy).unwrap().len();
        assert!(
            new_len * 3 <= old_len,
            "载荷没瘦下来：新 {new_len} 字节 vs 旧 {old_len} 字节（要求至少 3 倍）。\
             检查是不是把 text 加回来了、或片段又变成每 span 一个对象。"
        );
    }

    /// 载荷形状：行号即下标、每行是 3 的倍数、下标落在名表内、片段升序且不重叠。
    /// 前端**直接顺序切片**渲染（不做排序/重叠检查），所以这些前提必须由后端保证。
    #[test]
    fn highlight_payload_shape_is_dense_and_flat() {
        let src = sample_source();
        let payload = payload_of("rust", &src);

        assert_eq!(
            payload.lines.len(),
            src.lines().count(),
            "载荷行数必须等于 code.lines().count()（下标即行号）"
        );
        assert!(
            !payload.tags.is_empty() && payload.tags.len() <= TOK_TABLE.len(),
            "名表只该含用到的 tag 且不超过全集：{:?}",
            payload.tags
        );
        let uniq: std::collections::BTreeSet<&str> = payload.tags.iter().copied().collect();
        assert_eq!(uniq.len(), payload.tags.len(), "名表必须去重");

        for (i, flat) in payload.lines.iter().enumerate() {
            assert_eq!(flat.len() % 3, 0, "第 {i} 行不是 3 的倍数：{flat:?}");
            let mut prev_end = 0usize;
            for k in (0..flat.len()).step_by(3) {
                let (s, e, t) = (flat[k] as usize, flat[k + 1] as usize, flat[k + 2] as usize);
                assert!(t < payload.tags.len(), "第 {i} 行的 tag 下标越界：{t}");
                assert!(s < e, "第 {i} 行有空/倒置片段：{s}..{e}");
                assert!(s >= prev_end, "第 {i} 行的片段重叠或未升序：{flat:?}");
                prev_end = e;
            }
        }
    }

    /// `Tok` 必须覆盖高亮器**实际会产出**的每个名字。
    /// 漏一个的后果不是报错，而是那一类片段从此没有颜色（`from_name` 返回 `None` 被丢弃）。
    #[test]
    fn tok_table_covers_every_name_the_highlighter_produces() {
        let corpus: &[(&str, &str)] = &[
            (
                "rust",
                "fn main() { let v = vec![1, 2]; println!(\"{:?}\", v); }",
            ),
            ("python", "def f(x):\n    return {'a': 1}\n"),
            ("javascript", "const f = (x) => x + 1; // note\n"),
            (
                "html",
                "<!DOCTYPE html>\n<html lang=\"zh\"><body class=\"a\">t</body></html>\n",
            ),
            ("css", ".a { color: #fff; }\n"),
            ("markdown", "# T\n\n[l](http://x)\n\n~~s~~ **b** *i* `c`\n"),
            ("sql", "SELECT id, name FROM t WHERE id = 1; -- note\n"),
            ("java", "public class A { int f(int x) { return x; } }\n"),
        ];

        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (lang, src) in corpus {
            for (_, _, name) in themed_spans(lang, src) {
                assert!(
                    Tok::from_name(&name).is_some(),
                    "{lang} 产出了表外的 tag `{name}`：这一类片段会静默失去颜色。\
                     加进 TOK_TABLE，并补上 .tok-{name} 的样式"
                );
                seen.insert(name);
            }
        }
        assert!(
            seen.len() >= 8,
            "语料太弱，只覆盖了 {} 个 tag，钉不住表：{seen:?}",
            seen.len()
        );
    }

    /// 每个 `Tok` 都必须在 `styles.css` 里有 `.tok-<name>`。
    ///
    /// 枚举化之后"表里有的 tag 却没人给它配色"是**新的**一类坏：以前未知名字至少还会
    /// 拼出一个类名（配不配上色另说），现在得显式确认每个枚举值都真能画出颜色。
    /// 反向读源码而不是靠人记 —— 加枚举值忘了配色时这条会红。
    #[test]
    fn every_tag_has_a_css_class() {
        let css = include_str!("../../ui/styles.css");

        // **先去掉注释**：注释里提一句 `.tok-macro` 不该算数，否则门禁会被一句说明骗过
        // （这正是反向验证抓出来的：把规则换成一句含类名的注释，门禁照过）。
        let mut code = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(i) = rest.find("/*") {
            code.push_str(&rest[..i]);
            match rest[i + 2..].find("*/") {
                Some(j) => rest = &rest[i + 2 + j + 2..],
                None => {
                    rest = "";
                    break;
                }
            }
        }
        code.push_str(rest);

        let mut declared: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut rest = code.as_str();
        while let Some(i) = rest.find(".tok-") {
            let tail = &rest[i + 5..];
            let end = tail
                .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                .unwrap_or(tail.len());
            // 只有"类名后面跟 `{`"才算真声明（选择器列表 `.a, .tok-b {` 也认）
            if tail[end..].trim_start().starts_with('{') {
                declared.insert(tail[..end].to_string());
            }
            rest = &tail[end..];
        }

        let missing: Vec<&str> = TOK_TABLE
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| !declared.contains(*n))
            .collect();
        assert!(
            missing.is_empty(),
            "下列 tag 没有 CSS 规则（会渲染成默认色，看起来像「高亮丢了」）：{missing:?}；\
             TOK_TABLE 里声明的名字必须与 styles.css 的 .tok-* 对得上"
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
