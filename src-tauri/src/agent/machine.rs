//! 本机环境一瞥（"让模型别用错 shell、别瞎猜算力"）。
//!
//! 会话 Agent 的 execute 在 Windows 上经 `cmd /C`、Unix 上经 `sh -c` 执行（引擎
//! `agent::run_shell`），但系统提示词是静态的、不含机器信息 —— 模型只能靠项目路径里
//! 的反斜杠猜操作系统，猜错就是一整轮试错。这里在每轮 run 的任务头补几行运行环境。
//!
//! 探测纪律：
//! - CPU 型号零成本（Windows 读 `PROCESSOR_IDENTIFIER` 环境变量、Linux 读
//!   `/proc/cpuinfo`）；
//! - GPU 没有环境变量可读，必须跑探测子进程（nvidia-smi / wmic / reg / lspci /
//!   system_profiler）—— 复用引擎 `exec::run`（超时、管道排空、Windows 不闪黑窗），
//!   全部 best-effort，一条都探不到就不输出 GPU 行；
//! - 机器配置在应用生命周期内不变：整段结果用 `OnceLock` 缓存，只在首次调用时探测
//!   一次（调用方应放 `spawn_blocking`，别在异步上下文里首次触发）。

use harness_engine as engine;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// 渲染运行环境说明（纯函数，便于测试）。
fn format_note(os: &str, arch: &str, cores: usize, cpu: Option<&str>, gpu: Option<&str>) -> String {
    let mut s = format!("运行环境：{os} {arch}");
    if cores > 0 {
        s.push_str(&format!(" · {cores} 核"));
    }
    if let Some(b) = cpu.map(str::trim).filter(|b| !b.is_empty()) {
        s.push_str(&format!(" · {b}"));
    }
    if let Some(g) = gpu.map(str::trim).filter(|g| !g.is_empty()) {
        s.push_str(&format!("\nGPU：{g}"));
    }
    s.push_str(&format!("\n命令执行：{}", shell_desc(os)));
    s
}

/// 引擎 run_shell 的执行方式。os 用 `std::env::consts::OS` 的取值（windows/macos/linux）。
fn shell_desc(os: &str) -> &'static str {
    match os {
        "windows" => "cmd /C（Windows CMD 语法，如 dir / findstr / 2>nul；不是 PowerShell）",
        _ => "sh -c（POSIX shell 语法）",
    }
}

/// CPU 型号 best-effort：拿不到返回 None。
fn cpu_brand() -> Option<String> {
    #[cfg(target_os = "windows")]
    return std::env::var("PROCESSOR_IDENTIFIER").ok();
    #[cfg(target_os = "linux")]
    return std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("model name"))
            .map(|rest| rest.trim_start_matches([':', ' ']).trim().to_string())
    });
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    None
}

/// 机器那一半（`运行环境：…` / `GPU：…` / `命令执行：…`）。
///
/// **时间不在这里**：这段的前提是"机器配置在应用生命周期内不变"，所以整段缓存
/// （GPU 要跑子进程，不该每条消息探一遍）。塞进去就成了"永远停在第一次调用那一刻"。
fn machine_note() -> &'static str {
    static NOTE: OnceLock<String> = OnceLock::new();
    NOTE.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0);
        format_note(
            std::env::consts::OS,
            std::env::consts::ARCH,
            cores,
            cpu_brand().as_deref(),
            gpu_desc().as_deref(),
        )
    })
    .as_str()
}

/// 每轮 run 注入的环境说明 = **实时时间** + 缓存的机器那一半。
///
/// 时间不用 `date /t` / `Get-Date` 去探：它们的输出随机器区域设置变（中文环境给
/// `2026/09/22 周二`，英文环境给 `Tue 09/22/2026`），模型先猜命令再猜格式，
/// 光是"今天星期几"就能烧掉五六轮。引擎手里本来就有这个事实 —— 直接说。
pub fn note() -> String {
    format!(
        "当前时间：{}\n{}",
        engine::workspace::now_human(),
        machine_note()
    )
}

/// 跑一条探测命令：退出码 0、未超时、stdout 非空才算数。
fn probe(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let out = engine::exec::run(Path::new("."), program, args, timeout, &[]);
    if out.exit_code != Some(0) || out.timed_out {
        return None;
    }
    let t = out.stdout.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// GPU 探测：nvidia-smi 最准（自带显存，CUDA 任务正需要）；没有再退回各 OS 的
/// 通用显示适配器清单。全部失败 → None（不输出 GPU 行）。
fn gpu_desc() -> Option<String> {
    nvidia_gpus().or_else(platform_gpus)
}

/// 有没有可用的 NVIDIA 卡（**复用同一套 nvidia-smi 探测**，绝不另写一份 —— 两套探测就会有两种答案）。
///
/// 注意它只回答"卡在不在"，不回答"能不能用"：能不能用要由引擎真去建一次 CUDA 设备才知道
/// （DLL 缺 / 驱动旧 / 算力代号没编进内核 都会在那一步失败）。见 `voice/asr.rs::pick_device`。
pub fn nvidia_gpu_present() -> bool {
    nvidia_gpus().is_some()
}

fn nvidia_gpus() -> Option<String> {
    let raw = probe(
        "nvidia-smi",
        &["--query-gpu=name,memory.total", "--format=csv,noheader"],
        Duration::from_secs(3),
    )?;
    let gpus: Vec<String> = raw
        .lines()
        .map(pretty_nvidia)
        .filter(|s| !s.is_empty())
        .collect();
    (!gpus.is_empty()).then(|| gpus.join(" · "))
}

/// "NVIDIA GeForce RTX 4070, 12288 MiB" → "NVIDIA GeForce RTX 4070 · 12GB"
fn pretty_nvidia(line: &str) -> String {
    let line = line.trim();
    let Some((name, mem)) = line.split_once(',') else {
        return line.to_string();
    };
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    let mem = mem.trim();
    if let Some(mib) = mem
        .strip_suffix(" MiB")
        .and_then(|n| n.parse::<u64>().ok())
        .filter(|&m| m >= 1024)
    {
        // 四舍五入到 GB：驱动报告的 MiB 通常被预留显存抠掉几十 MiB
        // （8GB 卡报 8151），整除会低估成 7GB
        return format!("{name} · {}GB", (mib + 512) / 1024);
    }
    format!("{name} · {mem}")
}

/// 各 OS 的通用 GPU 清单（无 NVIDIA 驱动时）。
#[cfg(target_os = "windows")]
fn platform_gpus() -> Option<String> {
    // wmic 在新 Win11 上逐步移除；reg 查显示适配器的注册表类键兜底（reg 永远在）
    if let Some(raw) = probe(
        "wmic",
        &["path", "win32_VideoController", "get", "name"],
        Duration::from_secs(5),
    ) {
        let gpus: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("name"))
            .collect();
        if !gpus.is_empty() {
            return Some(gpus.join(" · "));
        }
    }
    let raw = probe(
        "reg",
        &[
            r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}",
            "/v",
            "DriverDesc",
            "/s",
        ],
        Duration::from_secs(3),
    )?;
    // 行形如 "    DriverDesc    REG_SZ    NVIDIA GeForce RTX 4070"；类键下每个实例
    // 子键一条，同名（驱动复制条目）去重
    let mut gpus: Vec<String> = Vec::new();
    for line in raw.lines() {
        if let Some(pos) = line.find("REG_SZ") {
            let name = line[pos + "REG_SZ".len()..].trim();
            if !name.is_empty() && !gpus.iter().any(|g| g == name) {
                gpus.push(name.to_string());
            }
        }
    }
    (!gpus.is_empty()).then(|| gpus.join(" · "))
}

#[cfg(target_os = "linux")]
fn platform_gpus() -> Option<String> {
    let raw = probe("lspci", &[], Duration::from_secs(3))?;
    let gpus: Vec<&str> = raw
        .lines()
        .filter(|l| {
            l.contains("VGA compatible controller")
                || l.contains("3D controller")
                || l.contains("Display controller")
        })
        .filter_map(|l| l.rsplit_once(": ").map(|(_, name)| name.trim()))
        .filter(|n| !n.is_empty())
        .collect();
    (!gpus.is_empty()).then(|| gpus.join(" · "))
}

#[cfg(target_os = "macos")]
fn platform_gpus() -> Option<String> {
    let raw = probe(
        "system_profiler",
        &["SPDisplaysDataType"],
        Duration::from_secs(8),
    )?;
    let gpus: Vec<&str> = raw
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Chipset Model:"))
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    (!gpus.is_empty()).then(|| gpus.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_note_pieces() {
        let n = format_note(
            "linux",
            "x86_64",
            8,
            Some("AMD Ryzen 7 5800X 8-Core"),
            Some("NVIDIA GeForce RTX 4070 · 12GB"),
        );
        assert!(n.contains("linux x86_64"));
        assert!(n.contains("8 核"));
        assert!(n.contains("AMD Ryzen 7 5800X"));
        assert!(n.contains("GPU：NVIDIA GeForce RTX 4070 · 12GB"));
        assert!(n.contains("sh -c"));

        let w = format_note("windows", "x86_64", 0, None, None);
        assert!(w.contains("windows x86_64"));
        assert!(w.contains("cmd /C"));
        assert!(!w.contains("核"), "cores=0 时不输出核数");
        assert!(!w.contains("GPU"), "探不到 GPU 时整行不出现");
        assert!(!w.contains("·"), "无品牌无核数时不留分隔符");

        let m = format_note("macos", "aarch64", 10, Some("  "), None);
        assert_eq!(
            m.lines().next().unwrap(),
            "运行环境：macos aarch64 · 10 核",
            "空白品牌视为没有"
        );
    }

    #[test]
    fn pretty_nvidia_formats() {
        assert_eq!(
            pretty_nvidia("NVIDIA GeForce RTX 4070, 12288 MiB"),
            "NVIDIA GeForce RTX 4070 · 12GB"
        );
        assert_eq!(pretty_nvidia("Tesla T10, 24576 MiB"), "Tesla T10 · 24GB");
        // 8GB 卡驱动报 8151 MiB（显存被预留抠掉），四舍五入回 8GB 而不是低估成 7GB
        assert_eq!(
            pretty_nvidia("NVIDIA GeForce RTX 5060 Laptop GPU, 8151 MiB"),
            "NVIDIA GeForce RTX 5060 Laptop GPU · 8GB"
        );
        // 解析不出的显存原样保留
        assert_eq!(pretty_nvidia("Some GPU, N/A"), "Some GPU · N/A");
        // 没有逗号（驱动版本差异）→ 整行原样
        assert_eq!(pretty_nvidia("NVIDIA A100"), "NVIDIA A100");
        assert_eq!(pretty_nvidia(""), "");
    }

    #[test]
    fn note_matches_current_platform() {
        let n = note();
        assert!(n.contains(std::env::consts::OS));
        assert!(n.contains(std::env::consts::ARCH));
        assert!(n.contains(if cfg!(windows) { "cmd /C" } else { "sh -c" }));

        // 时间那一行：必须有，且是实时取（不能被缓存压成"首次调用那一刻"）
        let now_line = n.lines().next().unwrap();
        assert!(
            now_line.starts_with("当前时间："),
            "首行应是时间：{now_line}"
        );
        assert!(now_line.contains("星期"), "要给出星期几才行：{now_line}");

        // 机器那半仍走缓存（GPU 子进程不该每条消息跑一遍）：两次调用的这部分必须一致
        let strip_now = |s: String| s.lines().skip(1).collect::<Vec<_>>().join("\n");
        let a = note();
        let b = note();
        assert_eq!(strip_now(a), strip_now(b), "机器说明没走缓存");
        assert_eq!(strip_now(n.clone()), machine_note());
    }
}
