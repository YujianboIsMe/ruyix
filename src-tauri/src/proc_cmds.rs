//! 托管进程与运行目标 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第四刀）。
//!
//! 两个域，都在"跑东西"这件事上：
//! · **托管进程**（"服务"面板）：agent 用 `background` 起的服务是**宿主** spawn 的，却不在宿主的
//!   进程树里 —— 以前只活在引擎的进程表里，UI 上等于不存在（起得来、看不见、停不掉）。面板读的
//!   是引擎那**同一份**表（`harness_engine::proc::listing`），不另立一份，否则面板与模型各说各话。
//! · **运行目标**：`run_target` 跑完才回（一次性），工作目录由 `resolve_run_dir` 从 `bind`
//!   清单文件的位置推出来；`spawn_terminal` 是新开一个 OS 控制台窗口（不走 PTY）。
//!
//! `main.rs` 只留注册（`proc_cmds::*`，见 invoke_handler）。测试用 [`resolve_run_dir`]，
//! 见 `src-tauri/src/tests.rs`。

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;

/// CREATE_NEW_CONSOLE — 为新进程创建独立控制台窗口
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x00000010;

/// CREATE_NO_WINDOW — 阻止子进程新建控制台窗口（release GUI 子系统无控制台，
/// 不加此标志控制台子进程会闪黑窗口；输出仍通过管道捕获）
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

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
pub fn proc_list() -> Vec<harness_engine::proc::ProcInfo> {
    harness_engine::proc::listing()
}

/// 按 pid 停掉一个托管进程，**连子进程树一起**（只杀 mvn 不杀 java 就是又造一个孤儿）。
///
/// 走 `spawn_blocking`：`kill_tree` + `wait` 会等进程真的退掉（秒级），
/// 放在主线程会把 UI 卡住这段。
#[tauri::command]
pub async fn proc_stop(pid: u32) -> Result<harness_engine::proc::ProcInfo, String> {
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
pub fn proc_log_read(
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
pub struct RunOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub killed: bool,
}

/// 运行目标的工作目录解析。
///
/// - 未绑定文件 → 项目根目录
/// - 绑定了文件 → 该文件**所在目录**（如 `admin-web\package.json` → `<项目根>\admin-web`），
///   `npm start` 这类必须在清单文件所在目录执行的命令才能正确运行
/// - 绑定了目录 → 该目录本身
/// - 目录不存在、或解析后越出项目根（bind 写成 `../..`）→ 回退到项目根
pub(crate) fn resolve_run_dir(project_root: Option<&str>, bind: Option<&str>) -> Option<String> {
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
            Some(crate::clean_path(&dir_canon))
        }
        _ => Some(root.to_string()),
    }
}

/// 运行目标：执行一次性命令并回传输出。
///
/// `bind` 为运行目标绑定的清单文件（项目相对路径），用于决定工作目录：
/// 绑定 `admin-web\package.json` 时命令在 `<项目根>\admin-web` 下执行。
#[tauri::command]
pub async fn run_target(
    cmd: String,
    project_root: Option<String>,
    bind: Option<String>,
) -> Result<RunOutput, String> {
    let parts = crate::split_cmd(&cmd);
    if parts.is_empty() {
        return Err("空命令".to_string());
    }

    let program = crate::resolve_windows_cmd(&parts[0]);
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
pub fn spawn_terminal(cmd: String, project_root: Option<String>) -> Result<(), String> {
    let parts = crate::split_cmd(&cmd);
    if parts.is_empty() {
        return Err("空命令".to_string());
    }

    let program = crate::resolve_windows_cmd(&parts[0]);
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
