//! PTY 终端与终端目标 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第六刀）。
//!
//! 两块：
//! · **PTY**（[`pty_spawn`] / [`pty_write`] / [`pty_resize`] / [`pty_close`]）：命令层，
//!   真正的终端在 `pty::PtyManager`（`portable-pty`）；前端用 xterm.js，输出走 `pty://` 事件。
//! · **终端目标**（[`get_term_targets`]）：导航区「终端资源」里可点的终端条目，用户可增删改；
//!   与**运行目标共用同一份扫描器**（`config::ConfigManager::load_term_targets` →
//!   `scan_target_file`），否则"运行目标里能选、终端里找不到"这种漂移迟早发生。
//!
//! **`split_cmd` / `resolve_windows_cmd` 不在这个文件里**：它们是 crate 根的共享工具
//! （`git.rs` / `pty.rs` / `paths.rs` 都用 `crate::` 调），留在 `main.rs`。

use std::sync::Mutex;

/// 终端目标（导航区「终端资源」里可点的终端）：与运行目标同一套形状，用户可增删改。
#[tauri::command]
pub fn get_term_targets(
    project_root: Option<String>,
    config_mgr: tauri::State<'_, Mutex<crate::config::ConfigManager>>,
) -> Result<Vec<crate::config::RunTarget>, String> {
    let mgr = config_mgr.lock().map_err(|e| e.to_string())?;
    mgr.load_term_targets(project_root.as_deref())
}

#[tauri::command]
pub fn pty_spawn(
    window: tauri::Window,
    pty_mgr: tauri::State<'_, Mutex<crate::pty::PtyManager>>,
    cmd: String,
    tab_id: String,
    project_root: Option<String>,
) -> Result<(), String> {
    let mut mgr = pty_mgr.lock().map_err(|e| e.to_string())?;
    mgr.spawn(window, tab_id, &cmd, project_root.as_deref())
}

#[tauri::command]
pub fn pty_write(
    pty_mgr: tauri::State<'_, Mutex<crate::pty::PtyManager>>,
    tab_id: String,
    data: String,
) -> Result<(), String> {
    let mgr = pty_mgr.lock().map_err(|e| e.to_string())?;
    mgr.write(&tab_id, &data)
}

#[tauri::command]
pub fn pty_resize(
    pty_mgr: tauri::State<'_, Mutex<crate::pty::PtyManager>>,
    tab_id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    let mgr = pty_mgr.lock().map_err(|e| e.to_string())?;
    mgr.resize(&tab_id, rows, cols)
}

#[tauri::command]
pub fn pty_close(
    pty_mgr: tauri::State<'_, Mutex<crate::pty::PtyManager>>,
    tab_id: String,
) -> Result<(), String> {
    let mut mgr = pty_mgr.lock().map_err(|e| e.to_string())?;
    mgr.close(&tab_id);
    Ok(())
}
