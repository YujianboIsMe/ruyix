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

/// 解码子进程输出：**先按 UTF-8 严格解，不合法再按 Windows 活动代码页（中文机器 = GBK/CP936）解**。
///
/// 为什么不能只用 `from_utf8_lossy`：中文 Windows 上 `cmd` / `java` / `mvn` 的输出是 GBK，
/// lossy 之后模型读到的是 `'\' �����ڲ����...`，**读不出"不是内部或外部命令"**——
/// 于是它换一个更离谱的命令继续试，这就是"模型返回的命令总是不对"的机制。
/// 实测字节（`cmd /C \admin-run\` 的 stderr，70 字节）：
/// `27 5C 61 64 6D 69 6E 2D 72 75 6E 5C 27 20`（ASCII：`'\admin-run\' `）
/// `B2 BB CA C7 C4 DA B2 BF BB F2 CD E2 B2 BF …`（GBK："不是内部或外部命令…"）。
///
/// 先试 UTF-8 是必须的：本机同样有大量工具（cargo / node / 设了 `PYTHONUTF8` 的 python）
/// 本来就吐 UTF-8，不能一律当 GBK。误判窗口很小（GBK 字节恰好构成合法 UTF-8 的概率低），
/// 且真命中时也只是显示问题，不会让命令本身失败。
pub fn decode_output(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    #[cfg(target_os = "windows")]
    if let Some(s) = decode_ansi_codepage(bytes) {
        return s;
    }
    String::from_utf8_lossy(bytes).to_string()
}

// 按活动代码页（ANSI code page）解字节。引擎保持零依赖，这类系统调用直接声明 FFI，
// 比为一个编码问题拉一个编码库划算；宿主侧的输出解码也走这里（一处实现多处消费）。
#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetACP() -> u32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        mb: *const u8,
        mb_len: i32,
        wide: *mut u16,
        wide_len: i32,
    ) -> i32;
}

/// 当前活动代码页：中文机器 936(GBK)、英文机器 1252。测试用它判断该不该断言中文。
#[cfg(target_os = "windows")]
pub fn ansi_codepage() -> u32 {
    unsafe { GetACP() }
}

/// `None` = 系统也解不了（调用方退回 lossy）
#[cfg(target_os = "windows")]
fn decode_ansi_codepage(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return Some(String::new());
    }
    let cp = ansi_codepage();
    let len = i32::try_from(bytes.len()).ok()?;
    // 先问长度再要内容（不确定尾部填充）
    let need = unsafe { MultiByteToWideChar(cp, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return None;
    }
    let mut buf = vec![0u16; need as usize];
    let got = unsafe { MultiByteToWideChar(cp, 0, bytes.as_ptr(), len, buf.as_mut_ptr(), need) };
    if got <= 0 {
        return None;
    }
    buf.truncate(got as usize);
    Some(String::from_utf16_lossy(&buf))
}

/// 杀**整棵进程树**（不是只杀直接子进程）。
///
/// 公开给 [`crate::proc`] 复用：托管服务多是 `cmd → mvn.cmd → java` 这样的多层树，
/// 只杀最外层会留下占着端口的 `java`，而那个孤儿会让后面每一次启动都拿到假的失败信号。
#[cfg(target_os = "windows")]
pub fn kill_tree(pid: u32) {
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
pub fn kill_tree(pid: u32) {
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
    cmd.args(args).current_dir(cwd).stdin(Stdio::null());
    // Windows：release 下 GUI 子系统没有控制台，不加标志会闪黑窗；
    // Unix：放进独立进程组，超时强杀才能连子进程树一起杀掉。
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    cmd.process_group(0);
    for (k, v) in envs {
        cmd.env(k, v);
    }

    run_prepared(cmd, display, started, timeout, max_stdout)
}

/// spawn + 收输出 + 超时强杀的公共部分（[`run_with_cap`] 与 [`run_line`] 共用）。
///
/// 分开的理由不是好看：两条路构造 [`Command`] 的方式**必须不同**（见 [`shell_command`]
/// 的 `raw_arg` 说明），但"喂管道 → 排空 → 超时杀树 → 按代码页解码"这套必须**只有一份**。
fn run_prepared(
    mut cmd: Command,
    display: String,
    started: Instant,
    timeout: Duration,
    max_stdout: usize,
) -> CmdOutput {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

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

    // 关键：不能 from_utf8_lossy —— 中文 Windows 的 cmd/java/mvn 输出是 GBK，
    // lossy 之后模型读到乱码，读不出失败原因（见 decode_output 的说明）。
    let stdout = decode_output(&out_buf.lock().unwrap());
    let stderr = decode_output(&err_buf.lock().unwrap());

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

/// 跑一条**命令行**（交给命令解释器，不是把可执行名丢给 `CreateProcess`）。
///
/// 为什么必须有它：`.cmd` / `.bat` / `.ps1` 不能被 `CreateProcess` 直接执行（见
/// [`resolve_bin`]）——`scoop.cmd`、`mvn.cmd`、`gradle.bat` 这类全中招。凡是"拼接出来的
/// 命令行"（安装、构建、探版本）都要走这里。Windows 用 `cmd /C`，Unix 用 `sh -c`，
/// 与 [`crate::agent`] 的 shell 执行保持同一套做法。
///
/// 需要完整输出（退出码 / stdout / stderr）用这个；只要版本首行用 [`bin_version`]。
pub fn run_line(cwd: &Path, cmdline: &str, timeout: Duration) -> CmdOutput {
    let cmd = shell_command(cwd, cmdline);
    run_prepared(
        cmd,
        cmdline.to_string(),
        Instant::now(),
        timeout,
        DEFAULT_STDOUT_CAP,
    )
}

/// 构造一个"过 shell 跑**一条拼接出来的命令行**"的 [`Command`]（不 spawn，由调用方自己
/// 决定输出去哪）。
///
/// **Windows 上这两条缺一不可**，都是实测踩出来的（复现：`examples/proc_demo.rs`）：
///
/// 1. 用 `raw_arg` 而不是 `arg`。std 的 `arg` 会按 Windows 参数规则先加工：参数含空格就
///    整体加引号、并把内部引号转义成 `\"`。于是 cmd 收到的第一个 token 变成
///    `\"C:\path\x.exe\"`，认不出可执行文件，回一句 `'\"...\"' 不是内部或外部命令`。
/// 2. 用 `/S /C` 并**自己把整条命令行裹一层引号**。`cmd /C` 对"以引号开头的串"有一套
///    额外的剥引号规则，条件不满足就不剥 —— 命令行里带引号时正好不满足，于是 cmd 把
///    `"C:\x.exe" --hold "C:\y" 30` 当成文件名去解析，回
///    `文件名、目录名或卷标语法不正确。`。加 `/S` 后 cmd **无条件**剥掉最外层那一对引号，
///    里面的内容原样执行（包括内部的引号）。
///
/// Unix 侧没有这些问题（`execvp` 直接传 argv，不经过任何字符串往返），但为了"只有一份
/// shell 构造"仍走同一个入口。
///
/// 公开给 [`crate::proc`] 复用：托管进程要把 stdout/stderr 重定向到**引擎指定的日志文件**，
/// 不能走 [`run_line`]（它内部收管道），所以要拿到 [`Command`] 自己配 IO 再 spawn。
pub fn shell_command(cwd: &Path, line: &str) -> Command {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/S").arg("/C").raw_arg(format!("\"{line}\""));
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", line]);
        c
    };
    cmd.current_dir(cwd).stdin(Stdio::null());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
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
    let out = run_line(&std::env::temp_dir(), &cmdline, timeout);
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

    /// 病根回归：命令行里带引号必须**原样**到 shell。
    ///
    /// 实测病根（`examples/proc_demo.rs` 暴露）：把整条命令行当普通参数交给 std，
    /// Windows 下 std 会给它整体加引号并把内部引号转义成 `\"`，cmd 收到的第一个 token 是
    /// `\"C:\path\x.exe\"` → `不是内部或外部命令`；换成 `raw_arg` 后又撞上 `cmd /C` 对
    /// "以引号开头的串"的剥引号规则 → `文件名、目录名或卷标语法不正确。`。
    /// 修法是 `raw_arg` + 自己裹一层引号 + `/S /C`（无条件剥最外层）。
    /// 这里用 `echo "a b"` 验：引号转义会留下反斜杠，一眼可见。
    #[test]
    fn run_line_passes_quotes_to_the_shell_verbatim() {
        let out = run_line(
            &std::env::temp_dir(),
            "echo \"a b\"",
            Duration::from_secs(20),
        );
        assert_eq!(out.exit_code, Some(0), "stderr={}", out.stderr);
        assert!(out.stdout.contains("a b"), "stdout={:?}", out.stdout);
        assert!(
            !out.stdout.contains('\\'),
            "引号被 std 转义成了反斜杠（raw_arg / /S /C 回归）：stdout={:?}",
            out.stdout
        );
    }

    /// 合法 UTF-8 必须直通 —— 本机大量工具（cargo / node / 设了 `PYTHONUTF8` 的 python）
    /// 本来就吐 UTF-8，不能一律当 GBK 解。
    #[test]
    fn decode_output_passes_valid_utf8_through() {
        assert_eq!(decode_output("中文 ok".as_bytes()), "中文 ok");
        assert_eq!(decode_output(b""), "");
    }

    /// 非法字节序列不许 panic（兜底 lossy），可打印部分要保留。
    #[test]
    fn decode_output_never_panics_on_garbage() {
        let s = decode_output(&[0xFF, 0xFE, 0x00, 0x41]);
        assert!(s.contains('A'), "可打印部分该保留: {s:?}");
    }

    /// 病根回归：中文 Windows 上 cmd 的报错是 GBK，解码后必须可读。
    #[cfg(target_os = "windows")]
    #[test]
    fn decode_output_reads_gbk_error_output() {
        if ansi_codepage() != 936 {
            return; // 非中文 Windows 本来就不是 GBK
        }
        // 实测取样的真实字节：`cmd /C \admin-run\` 的 stderr
        let bytes: &[u8] = &[
            0x27, 0x5C, 0x61, 0x64, 0x6D, 0x69, 0x6E, 0x2D, 0x72, 0x75, 0x6E, 0x5C, 0x27, 0x20,
            0xB2, 0xBB, 0xCA, 0xC7, 0xC4, 0xDA, 0xB2, 0xBF, 0xBB, 0xF2, 0xCD, 0xE2, 0xB2, 0xBF,
            0xC3, 0xFC, 0xC1, 0xEE,
        ];
        let s = decode_output(bytes);
        assert!(s.starts_with("'\\admin-run\\'"), "ASCII 前缀应保留: {s:?}");
        assert!(s.contains("不是内部或外部命令"), "GBK 应解成中文: {s:?}");
    }

    /// 端到端：跑一条不存在的命令，agent 拿到的错误必须是**可读中文**。
    /// 修之前这里是乱码，模型读不出"不是内部或外部命令"，只能换个更离谱的命令继续试。
    #[cfg(target_os = "windows")]
    #[test]
    fn run_reports_a_readable_chinese_error() {
        let out = run(
            &std::env::temp_dir(),
            "cmd",
            &["/C", "zzz-no-such-tool-zzz"],
            Duration::from_secs(20),
            &[],
        );
        assert_eq!(out.exit_code, Some(1));
        if ansi_codepage() == 936 {
            assert!(
                out.stderr.contains("不是内部或外部命令"),
                "实际: {:?}",
                out.stderr
            );
        }
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
