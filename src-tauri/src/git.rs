//! Git 集成：状态解析 + 通用 git 命令执行。
//! 直接调用系统 git（不引入 git 库依赖），在项目根目录下运行。
//!
//! 两个命令：
//! - `git_status`  — `git status --porcelain` 解析为 staged/unstaged 文件列表
//! - `git_run`     — 执行任意 git 子命令（AI 返回的 git 命令、GUI 的
//!                   stage/unstage/commit/pull/push 都走这里）

use std::process::Command;

// ============================================
// 数据结构
// ============================================

#[derive(serde::Serialize, Clone)]
pub struct GitFile {
    /// 仓库相对路径
    pub path: String,
    /// porcelain XY 状态码（如 "M "、"??"、"MM"）
    pub code: String,
    /// 本列表相关的状态字母: M/A/D/R/C/U/?
    pub kind: String,
}

#[derive(serde::Serialize, Clone)]
pub struct GitStatus {
    /// 当前分支名（无提交的新仓库为 "(no commits)"）
    pub branch: String,
    /// 工作区是否干净
    pub clean: bool,
    pub staged: Vec<GitFile>,
    pub unstaged: Vec<GitFile>,
}

#[derive(serde::Serialize, Clone)]
pub struct GitOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

// ============================================
// 执行辅助函数
// ============================================

/// 执行 git 命令（仅 spawn 失败才返回 Err，非零退出码由调用方判断）
fn run_git(project_root: &str, args: &[&str]) -> Result<GitOutput, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(project_root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("无法启动 git（请确认已安装 Git 并加入 PATH）: {}", e))?;

    Ok(GitOutput {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// 取字符串第一行非空内容（用于错误摘要）
fn first_line(s: &str) -> Option<String> {
    s.lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

// ============================================
// Tauri 命令
// ============================================

#[tauri::command]
pub async fn git_status(project_root: String) -> Result<GitStatus, String> {
    tauri::async_runtime::spawn_blocking(move || status_impl(&project_root))
        .await
        .map_err(|e| e.to_string())?
}

fn status_impl(project_root: &str) -> Result<GitStatus, String> {
    // 当前分支（无提交的新仓库会失败，降级显示）
    let branch = run_git(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|o| o.exit_code == Some(0))
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no commits)".to_string());

    // core.quotepath=false: 路径原样输出（UTF-8，不转义中文）
    let out = run_git(
        project_root,
        &["-c", "core.quotepath=false", "status", "--porcelain", "-uall"],
    )?;
    if out.exit_code != Some(0) {
        return Err(first_line(&out.stderr).unwrap_or_else(|| "git status 失败".to_string()));
    }

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    for line in out.stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        // XY: X = 暂存区状态, Y = 工作区状态
        let mut chars = line.chars();
        let x = chars.next().unwrap_or(' ');
        let y = chars.next().unwrap_or(' ');
        // 重命名/复制: "R  old -> new"，取新路径
        let raw = line[3..].trim();
        let path = raw.rsplit(" -> ").next().unwrap_or(raw).to_string();

        if x != ' ' && x != '?' && x != '!' {
            staged.push(GitFile {
                path: path.clone(),
                code: format!("{x}{y}"),
                kind: x.to_string(),
            });
        }
        if y != ' ' && y != '!' {
            unstaged.push(GitFile {
                path,
                code: format!("{x}{y}"),
                kind: y.to_string(),
            });
        }
    }
    staged.sort_by(|a, b| a.path.cmp(&b.path));
    unstaged.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(GitStatus {
        branch,
        clean: staged.is_empty() && unstaged.is_empty(),
        staged,
        unstaged,
    })
}

/// 通用 git 命令执行：`git <args>` 在项目根目录运行
#[tauri::command]
pub async fn git_run(project_root: String, args: String) -> Result<GitOutput, String> {
    let parts = crate::split_cmd(args.trim());
    if parts.is_empty() {
        return Err("空命令".to_string());
    }

    // 后台线程执行，避免阻塞 UI（pull/push 可能耗时较长）
    tauri::async_runtime::spawn_blocking(move || {
        let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
        run_git(&project_root, &refs)
    })
    .await
    .map_err(|e| e.to_string())?
}
