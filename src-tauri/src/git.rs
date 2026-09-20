//! Git 集成：状态解析 + 通用 git 命令执行。
//! 直接调用系统 git（不引入 git 库依赖），在项目根目录下运行。
//!
//! 两个命令：
//! - `git_status` — `git status --porcelain` 解析为 staged/unstaged 文件列表
//! - `git_run` — 执行任意 git 子命令（AI 返回的 git 命令、GUI 的
//!   stage/unstage/commit/pull/push 都走这里）

use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// CREATE_NO_WINDOW — 阻止子进程新建控制台窗口。
/// release 版主程序是 GUI 子系统（无控制台），git.exe 是控制台程序，
/// 不加此标志每次调用都会闪一个黑色终端窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

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
    /// 项目根是否位于 git 仓库中（`rev-parse --is-inside-work-tree`）
    pub is_repo: bool,
    /// 当前分支名（无提交的新仓库为 "(no commits)"）
    pub branch: String,
    /// 工作区是否干净
    pub clean: bool,
    /// origin 远程地址（未配置远程仓库时为 None）
    pub remote: Option<String>,
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
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(project_root)
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("无法启动 git（请确认已安装 Git 并加入 PATH）: {}", e))?;

    Ok(GitOutput {
        exit_code: output.status.code(),
        // git 的本地化提示（"不是 git 仓库"之类）是活动代码页编码，不能当 UTF-8 硬解
        stdout: harness_engine::exec::decode_output(&output.stdout),
        stderr: harness_engine::exec::decode_output(&output.stderr),
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
    // 1) 是否在 git 仓库内。用 rev-parse 而不是检测 .git 目录：
    //    项目根位于父目录仓库内时，git 语义上也算"在仓库中"，状态面板应能正常工作。
    let inside = run_git(project_root, &["rev-parse", "--is-inside-work-tree"])?;
    if inside.exit_code != Some(0) || inside.stdout.trim() != "true" {
        // 非仓库：不是错误，交由前端渲染"仓库初始化引导"视图
        return Ok(GitStatus {
            is_repo: false,
            branch: String::new(),
            clean: true,
            remote: None,
            staged: Vec::new(),
            unstaged: Vec::new(),
        });
    }

    // 2) origin 远程地址（未配置远程仓库时命令失败，降级为 None）
    let remote = run_git(project_root, &["remote", "get-url", "origin"])
        .ok()
        .filter(|o| o.exit_code == Some(0))
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty());

    // 3) 当前分支（无提交的新仓库会失败，降级显示）
    let branch = run_git(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|o| o.exit_code == Some(0))
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no commits)".to_string());

    // core.quotepath=false: 路径原样输出（UTF-8，不转义中文）
    let out = run_git(
        project_root,
        &[
            "-c",
            "core.quotepath=false",
            "status",
            "--porcelain",
            "-uall",
        ],
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
        is_repo: true,
        branch,
        clean: staged.is_empty() && unstaged.is_empty(),
        remote,
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

// ============================================
// 测试
// ============================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 临时目录，Drop 时自动清理
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间异常")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "dh-code-git-test-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("创建临时目录失败");
            TempDir(dir)
        }

        fn path(&self) -> String {
            self.0.to_string_lossy().to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 在指定目录执行 git 命令（测试辅助）
    fn git_in(dir: &str, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("无法启动 git，请确认已安装 Git 并加入 PATH");
        assert!(
            out.status.success(),
            "git {:?} 失败: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 非仓库目录：应返回 is_repo=false 的初始状态，而不是 Err
    /// （回归测试：修复前这里返回 "fatal: not a git repository" 错误）
    #[test]
    fn non_repo_returns_init_state_not_error() {
        let dir = TempDir::new("plain");
        std::fs::write(dir.0.join("main.rs"), "fn main() {}").expect("写入文件失败");

        let status = status_impl(&dir.path()).expect("非仓库不应报错");
        assert!(!status.is_repo, "普通目录不应被判定为 git 仓库");
        assert!(status.branch.is_empty(), "非仓库不应有分支名");
        assert!(status.remote.is_none(), "非仓库不应有远程地址");
        assert!(status.staged.is_empty() && status.unstaged.is_empty());
    }

    /// git init 后：is_repo=true、无远程仓库、未跟踪文件进入 unstaged
    #[test]
    fn fresh_repo_has_no_remote_and_lists_untracked() {
        let dir = TempDir::new("fresh");
        std::fs::write(dir.0.join("main.rs"), "fn main() {}").expect("写入文件失败");
        git_in(&dir.path(), &["init"]);

        let status = status_impl(&dir.path()).expect("仓库状态读取失败");
        assert!(status.is_repo, "git init 后应判定为仓库");
        assert!(status.remote.is_none(), "新仓库不应有 origin");
        assert_eq!(
            status.branch, "(no commits)",
            "新仓库无提交时分支应降级显示"
        );
        assert!(
            status.unstaged.iter().any(|f| f.path == "main.rs"),
            "未跟踪文件应出现在 unstaged，实际: {:?}",
            status.unstaged.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }

    /// 配置 origin 后：remote 字段应回读地址，工作区脏状态正确
    #[test]
    fn repo_with_origin_reports_remote_url() {
        let dir = TempDir::new("remote");
        git_in(&dir.path(), &["init"]);
        git_in(
            &dir.path(),
            &["remote", "add", "origin", "https://example.com/demo.git"],
        );

        let status = status_impl(&dir.path()).expect("仓库状态读取失败");
        assert!(status.is_repo);
        assert_eq!(
            status.remote.as_deref(),
            Some("https://example.com/demo.git")
        );
        assert!(status.clean, "空仓库应为干净状态");
    }

    /// 集成：走 UI 的真实链路 git_run("init") 后，状态应从"非仓库"翻转为"仓库"
    /// （对应前端【初始化仓库】按钮 → handleCommand("git init") → git_run → 刷新面板）
    #[test]
    fn git_run_init_flips_status_to_repo() {
        let dir = TempDir::new("init-flow");
        assert!(
            !status_impl(&dir.path()).expect("读取状态失败").is_repo,
            "初始化前不应是仓库"
        );

        let out = tauri::async_runtime::block_on(git_run(dir.path(), "init".to_string()))
            .expect("git init 执行失败");
        assert_eq!(out.exit_code, Some(0), "git init 应成功: {}", out.stderr);

        let status = status_impl(&dir.path()).expect("读取状态失败");
        assert!(status.is_repo, "git init 后状态应翻转为仓库");
        assert!(status.remote.is_none(), "尚未配置远程仓库");
    }
}
