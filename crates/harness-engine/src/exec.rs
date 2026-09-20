//! 子进程执行器：并发读管道 + 超时 + Windows 进程树强杀。
//!
//! 为什么不直接 `Command::output()`：它会阻塞到进程退出，无法超时；
//! 而"用户让模型生成的代码"里出现死循环/等待输入是完全可能的，
//! 所以必须能强制结束，并且要连子进程树一起杀（cargo test 会再 fork 测试二进制）。

use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// CREATE_NO_WINDOW —— release 下 GUI 子系统没有控制台，不加这个标志
/// 每次跑子进程都会闪一个黑窗口。
#[cfg(target_os = "windows")]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 取消位。用 `Arc<AtomicBool>` 而不是 `&dyn Fn() -> bool`：后者不是 Send，
/// 逃不出 tauri 命令的 `Future: Send` 约束（这个坑实测踩过一次）。
pub type CancelFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;

pub fn new_cancel_flag() -> CancelFlag {
    CancelFlag::new(std::sync::atomic::AtomicBool::new(false))
}

pub fn is_cancelled(flag: &CancelFlag) -> bool {
    flag.load(std::sync::atomic::Ordering::Relaxed)
}

/// 单条命令的输出快照。所有字段都来自真实子进程，不做任何"推测"。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct CmdOutput {
    /// 完整命令行（用于回显给用户，让他能自己复现）
    pub cmd: String,
    pub exit_code: Option<i32>,
    /// 是否因为超时被强杀
    pub timed_out: bool,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    /// 连二进制都起不来（例如 python 不在 PATH）时的错误
    pub spawn_error: Option<String>,
}

impl CmdOutput {
    pub fn passed(&self) -> bool {
        !self.timed_out && self.spawn_error.is_none() && self.exit_code == Some(0)
    }
}

/// 关键字截断：保留头尾，中间省略。验证输出动辄上百 KB，全塞进 UI 没意义。
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let head = max * 2 / 3;
    let tail = max - head;
    let mut out = String::new();
    out.push_str(&s[..floor_char_boundary(s, head)]);
    out.push_str(&format!("\n… [省略 {} 字节] …\n", s.len() - max));
    out.push_str(&s[floor_char_boundary(s, s.len() - tail)..]);
    out
}

/// UTF-8 安全切片边界（中文输出被截断在字符中间会 panic）
pub fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(target_os = "windows")]
fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// Unix 侧等价实现：spawn 时已用 `process_group(0)` 把子进程放进独立进程组
/// （pgid == pid），对负 pid 发 SIGKILL 即杀整个进程组，效果等同 `taskkill /T`。
#[cfg(unix)]
fn kill_tree(pid: u32) {
    let _ = Command::new("kill")
        .args(["-9", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 在 `cwd` 下执行一条命令，最多等 `timeout`。
pub fn run(
    cwd: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
    envs: &[(&str, &str)],
) -> CmdOutput {
    run_with_cap(cwd, program, args, timeout, envs, DEFAULT_STDOUT_CAP)
}

/// 人类展示用的默认输出上限。
///
/// **机器可读的输出不要用这个值**：本项目的 lint 阶段返回 30KB+ 的 JSON，
/// 用展示阈值截断会得到"解析失败"这种误导性报错（真踩过）。
/// 需要完整输出的调用方请显式传 `run_with_cap(..., 1 << 20)`。
pub const DEFAULT_STDOUT_CAP: usize = 24_000;

pub fn run_with_cap(
    cwd: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
    envs: &[(&str, &str)],
    max_stdout: usize,
) -> CmdOutput {
    let started = Instant::now();
    let display = if args.is_empty() {
        program.to_string()
    } else {
        format!("{} {}", program, args.join(" "))
    };

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Windows：release 下 GUI 子系统没有控制台，不加标志会闪黑窗；
    // Unix：放进独立进程组，超时强杀才能连子进程树一起杀掉。
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    cmd.process_group(0);
    for (k, v) in envs {
        cmd.env(k, v);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CmdOutput {
                cmd: display,
                exit_code: None,
                timed_out: false,
                duration_ms: started.elapsed().as_millis(),
                stdout: String::new(),
                stderr: String::new(),
                spawn_error: Some(e.to_string()),
            };
        }
    };

    // 必须在等待期间持续排空管道，否则 64KB 管道缓冲写满后子进程直接卡死，
    // 表现就是"莫名其妙超时"。
    let out_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let err_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut readers = Vec::new();
    if let Some(mut s) = child.stdout.take() {
        let buf = out_buf.clone();
        readers.push(std::thread::spawn(move || drain(&mut s, &buf)));
    }
    if let Some(mut s) = child.stderr.take() {
        let buf = err_buf.clone();
        readers.push(std::thread::spawn(move || drain(&mut s, &buf)));
    }

    let mut timed_out = false;
    let mut exit_code = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if started.elapsed() > timeout {
            timed_out = true;
            kill_tree(child.id());
            // 给 taskkill 一点时间，然后再收尸
            for _ in 0..20 {
                if let Ok(Some(s)) = child.try_wait() {
                    exit_code = s.code();
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    for t in readers {
        let _ = t.join();
    }

    let stdout = String::from_utf8_lossy(&out_buf.lock().unwrap()).to_string();
    let stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).to_string();

    CmdOutput {
        cmd: display,
        exit_code,
        timed_out,
        duration_ms: started.elapsed().as_millis(),
        stdout: clip(&stdout, max_stdout),
        stderr: clip(&stderr, 24_000),
        spawn_error: None,
    }
}

fn drain<R: Read>(reader: &mut R, buf: &Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Ok(mut g) = buf.lock() {
                    g.extend_from_slice(&chunk[..n]);
                }
            }
        }
    }
}

/// 解析一个二进制：**在不在、在哪**。只有这一步能回答"在不在"。
///
/// 为什么不能用 `Command::new(bin)` 直接试：Windows 上 `CreateProcess` 不走 PATHEXT，
/// `.cmd` / `.bat` / `.ps1` 一律认不出。本机实测（`examples/cmd_probe.rs`，可复核）：
/// `Command::new("mvn")` → `program not found`，而 `mvn` 真实位置是
/// `D:\Tools\Maven\apache-maven-3.9.16\bin\mvn.cmd`；`where mvn` 则正常返回路径。
/// 也就是说 —— 直接试的写法对 Maven / npm / gradle 这类工具**天然失明**。
///
/// 为什么可用性只看这一步：`cmd /C` 对不存在的命令实测返回退出码 **1**（不是 9009），
/// 和"命令跑了但退出码是 1"分不开；`where` / `command -v` 的退出码才是干净的判据。
pub fn resolve_bin(bin: &str, timeout: Duration) -> Option<String> {
    let cwd = std::env::temp_dir();
    #[cfg(target_os = "windows")]
    let out = run(&cwd, "where", &[bin], timeout, &[]);
    #[cfg(not(target_os = "windows"))]
    let out = run(
        &cwd,
        "sh",
        &["-c", &format!("command -v {bin}")],
        timeout,
        &[],
    );
    if out.exit_code != Some(0) {
        return None;
    }
    out.stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|s| s.to_string())
}

/// 取版本首行。**不看退出码** —— `java -version` 走的是 stderr，不少工具版本也非 0 退出。
///
/// 必须过 shell：即使解析出了完整路径，`.cmd` 也不能被 `CreateProcess` 直接执行
/// （见 [`resolve_bin`]）。Windows 走 `cmd /C`、Unix 走 `sh -c`，与
/// `agent::run_shell` 保持同一套做法。
pub fn bin_version(bin: &str, args: &[&str], timeout: Duration) -> String {
    let cmdline = if args.is_empty() {
        bin.to_string()
    } else {
        format!("{bin} {}", args.join(" "))
    };
    let cwd = std::env::temp_dir();
    #[cfg(target_os = "windows")]
    let out = run(&cwd, "cmd", &["/C", &cmdline], timeout, &[]);
    #[cfg(not(target_os = "windows"))]
    let out = run(&cwd, "sh", &["-c", &cmdline], timeout, &[]);
    let text = if out.stdout.trim().is_empty() {
        out.stderr
    } else {
        out.stdout
    };
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| clip(l, 120))
        .unwrap_or_default()
}

/// 探针：二进制是否可用 + 版本首行。用于 UI 提前提示"环境缺什么"，
/// 而不是等用户点了运行才在验证阶段炸。
///
/// 可用性来自 [`resolve_bin`]（不是"跑一下看退出码"）—— 这样 `.cmd` 工具也能认出来，
/// 见该函数的说明。命令发现（[`crate::discover`]）走的是同一对函数，一处实现两处消费。
#[derive(serde::Serialize, Clone, Debug)]
pub struct Probe {
    pub name: String,
    pub bin: String,
    pub available: bool,
    pub version: String,
}

pub fn probe(name: &str, bin: &str, version_args: &[&str]) -> Probe {
    let timeout = Duration::from_secs(15);
    match resolve_bin(bin, timeout) {
        None => Probe {
            name: name.to_string(),
            bin: bin.to_string(),
            available: false,
            version: format!("未找到（where / command -v 解析不到 {bin}）"),
        },
        Some(_) => Probe {
            name: name.to_string(),
            bin: bin.to_string(),
            available: true,
            version: bin_version(bin, version_args, timeout),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_head_and_tail_on_char_boundary() {
        let s = "中文输出".repeat(1000);
        let c = clip(&s, 100);
        assert!(c.len() < s.len());
        assert!(c.contains("省略"));
        // 没有 panic 且仍是合法 UTF-8 即说明切在了字符边界上
        assert!(std::str::from_utf8(c.as_bytes()).is_ok());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn run_captures_stdout_and_exit_code() {
        let out = run(
            &std::env::temp_dir(),
            "cmd",
            &["/C", "echo harness-ok"],
            Duration::from_secs(20),
            &[],
        );
        assert!(out.passed(), "cmd 应正常退出: {:?}", out.spawn_error);
        assert!(out.stdout.contains("harness-ok"));
    }

    #[cfg(unix)]
    #[test]
    fn run_captures_stdout_and_exit_code() {
        let out = run(
            &std::env::temp_dir(),
            "/bin/sh",
            &["-c", "echo harness-ok"],
            Duration::from_secs(20),
            &[],
        );
        assert!(out.passed(), "sh 应正常退出: {:?}", out.spawn_error);
        assert!(out.stdout.contains("harness-ok"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn run_reports_nonzero_exit() {
        let out = run(
            &std::env::temp_dir(),
            "cmd",
            &["/C", "exit 3"],
            Duration::from_secs(20),
            &[],
        );
        assert_eq!(out.exit_code, Some(3));
        assert!(!out.passed());
    }

    #[cfg(unix)]
    #[test]
    fn run_reports_nonzero_exit() {
        let out = run(
            &std::env::temp_dir(),
            "/bin/sh",
            &["-c", "exit 3"],
            Duration::from_secs(20),
            &[],
        );
        assert_eq!(out.exit_code, Some(3));
        assert!(!out.passed());
    }

    #[test]
    fn spawn_error_is_captured_not_panicked() {
        let out = run(
            &std::env::temp_dir(),
            "definitely-not-a-real-binary-xyz",
            &["--version"],
            Duration::from_secs(10),
            &[],
        );
        assert!(out.spawn_error.is_some());
        assert!(!out.passed());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn timeout_kills_process_tree() {
        // ping 是 Windows 上最省事的"睡一会儿"手段
        let out = run(
            &std::env::temp_dir(),
            "cmd",
            &["/C", "ping -n 20 127.0.0.1 > nul"],
            Duration::from_millis(800),
            &[],
        );
        assert!(out.timed_out, "应当超时被强杀: {out:?}");
        assert!(!out.passed());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_process_tree() {
        let out = run(
            &std::env::temp_dir(),
            "/bin/sh",
            &["-c", "sleep 20"],
            Duration::from_millis(800),
            &[],
        );
        assert!(out.timed_out, "应当超时被强杀: {out:?}");
        assert!(!out.passed());
    }
}
