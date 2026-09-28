//! 项目与项目状态桶命令 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第七刀）。
//!
//! 都围着"当前项目"转：打开/切换（`open_project` 写全局 current）、清单（`get_projects`）、
//! 改名与语言（`update_project` / `set_project_lang`）、从清单里移除（`delete_project` ——
//! **只删条目，绝不删用户的文件夹**）、以及 v1.0.0 的**状态桶**（`project_buckets` /
//! `project_bucket_delete`：便携根里那些暂存/备份/会话存档，用户要能看见、能删）。
//! 另有单实例判定（`is_another_instance`）与运行目标清单（`get_run_targets`）。

use crate::{config, instance, paths};
use std::path::Path;
use std::sync::Mutex;

#[derive(serde::Serialize, Clone)]
pub struct ProjectInfo {
    name: String,
    path: String,
    lang: String,
}

#[tauri::command]
pub fn open_project(
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
    let clean = crate::clean_path(&canonical);
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

    // 项目桶自证（v1.0.0）：`<根>/projects/<key>/project.toml` —— 项目改名/移动后
    // 旧桶成孤儿，这份文件让"这桶是谁的"一眼可见（面板据此提示，绝不自动删）。
    let _ = paths::current().stamp_project(&clean);

    Ok(ProjectInfo {
        name,
        path: clean,
        lang,
    })
}

/// 项目状态桶清单（v1.0.0 孤儿面板）：key / 体积 / 自证的原始项目路径 / 那路径还在不在。
/// 所有状态都在便携根里，用户要能**看见**并**删掉**它们 —— 这就是"绿色"的另一半。
#[tauri::command]
pub fn project_buckets() -> Vec<paths::BucketInfo> {
    paths::current().list_buckets()
}

/// 删除一个项目状态桶（整桶）。**只在用户确认后调用**：我们绝不自动删用户的任何东西
/// （桶里是暂存、备份、会话存档 —— 自动删等于替用户做决定）。
#[tauri::command]
pub fn project_bucket_delete(key: String) -> Result<String, String> {
    paths::current().delete_bucket(&key)
}

/// 是否有其他实例正在运行（第二实例不自动打开上次项目）
#[tauri::command]
pub fn is_another_instance() -> bool {
    instance::is_other_instance()
}

/// 获取上次打开的项目路径（供前端启动时自动打开）
#[tauri::command]
pub fn get_last_project(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Option<String>, String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    Ok(cfg.load_projects().current)
}

/// 获取所有已知项目列表（含 name/path/lang）
#[tauri::command]
pub fn get_projects(
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Vec<config::ProjectEntry>, String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    Ok(cfg.load_projects().list)
}

/// 设置项目语言
#[tauri::command]
pub fn set_project_lang(
    path: String,
    lang: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    cfg.set_project_lang(&path, &lang)
}

/// 更新项目名称与语言（路径不可修改）
#[tauri::command]
pub fn update_project(
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
pub fn delete_project(
    path: String,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<(), String> {
    let cfg = config_mgr.lock().map_err(|e| e.to_string())?;
    cfg.delete_project(&path)
}

#[tauri::command]
pub fn get_run_targets(
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<config::ConfigManager>>,
) -> Result<Vec<config::RunTarget>, String> {
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.load_run_targets(project_root.as_deref())
}
