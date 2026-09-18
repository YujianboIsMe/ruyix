//! 运行时隔离：把"生成出来的代码"关进 Docker 沙箱里跑。
//!
//! ## 这一层唯一重要的设计：**不允许静默降级**
//!
//! 最危险的失败长这样：
//!
//! ```text
//! Docker 不可用 → 代码退回在宿主机执行 → 一切看起来正常
//! → 你以为有隔离，其实一点都没有
//! ```
//!
//! 所以本模块所有输出都带一个显式的 `Isolation`：要么 `Sandboxed{...}`
//! （附镜像与限制，可核对），要么 `Unsandboxed{reason}`（写明为什么）。
//! 报告、UI、日志里永远能回答"**这一次到底隔离了没有**"。
//!
//! 三档策略（`[sandbox] mode`）：
//!
//! | mode | Docker 可用 | Docker 不可用 |
//! |---|---|---|
//! | `require`（默认） | 容器里跑 | **拒绝执行**，检查标 skipped 并写明原因 |
//! | `prefer` | 容器里跑 | 宿主跑，但每条结果都标 `unsandboxed` |
//! | `off` | 宿主跑（知情选择） | 宿主跑（知情选择） |
//!
//! 默认 `require`：宁可"这次没验证成"，也不要"以为验证了"。

use crate::config::{AppConfig, SandboxConfig};
use crate::exec::{self, CancelFlag, CmdOutput};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 这一次执行**到底隔离了没有**。不是布尔值，因为"没隔离"必须带原因。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Isolation {
    /// 在容器里跑。字段可核对（用户能拿这些参数自己复现同一条 docker run）。
    Sandboxed {
        engine: String,
        image: String,
        network: String,
        memory: String,
        cpus: String,
        pids_limit: u32,
        read_only_rootfs: bool,
        user: String,
    },
    /// 在宿主机跑。必须写明原因，且这个原因会一路带到报告里。
    Unsandboxed { reason: String },
}

impl Isolation {
    pub fn is_sandboxed(&self) -> bool {
        matches!(self, Isolation::Sandboxed { .. })
    }

    /// 一行摘要，用于日志/UI/报告。
    pub fn label(&self) -> String {
        match self {
            Isolation::Sandboxed { image, network, .. } => {
                format!("已隔离（docker · {image} · network={network}）")
            }
            Isolation::Unsandboxed { reason } => format!("未隔离（{reason}）"),
        }
    }
}

/// 沙箱可用性探针结果。
///
/// 刻意分成多个字段：只看 `docker --version` 有输出是不够的
/// （Docker Desktop 装了但守护进程没起来是最常见的情况）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Probe {
    pub cli_found: bool,
    pub cli_version: String,
    /// 守护进程真的连上了吗
    pub daemon_ok: bool,
    pub daemon_version: String,
    /// 镜像在本地吗（`--network none` 的容器没法临时拉镜像）
    pub image_present: bool,
    pub image: String,
    /// 查镜像时**出错**（而不是确定不存在）——这两件事完全不同
    #[serde(default)]
    pub image_probe_error: Option<String>,
    /// 综合结论：能不能真的开一个满足策略的沙箱
    pub usable: bool,
    /// 不可用时给出**可执行的**修复建议
    pub hint: String,
    /// 探针自己跑的原始命令（便于用户复现）
    pub cmd: String,
    pub duration_ms: u128,
}

impl Probe {
    pub fn summary(&self) -> String {
        if self.usable {
            format!(
                "沙箱可用：docker {} / {} · 镜像 {}",
                self.daemon_version, self.cli_version, self.image
            )
        } else if !self.cli_found {
            "沙箱不可用：找不到 docker CLI".to_string()
        } else if !self.daemon_ok {
            "沙箱不可用：docker 守护进程没起来".to_string()
        } else if !self.image_present {
            match &self.image_probe_error {
                // "查询失败"不等于"镜像不存在"：别让人白拉一遍
                Some(e) => format!(
                    "沙箱不可用：查询镜像 {} 时出错（**不等于**镜像不存在）：{e}",
                    self.image
                ),
                None => format!("沙箱不可用：本地没有镜像 {}（需要先拉取）", self.image),
            }
        } else {
            "沙箱不可用".to_string()
        }
    }
}

/// 探针：**真的连一次守护进程**，而不是看 `--version`。
pub fn probe(cfg: &AppConfig) -> Probe {
    let sc = &cfg.sandbox;
    let mut p = Probe {
        image: sc.image.clone(),
        ..Default::default()
    };
    let to = Duration::from_secs(sc.probe_timeout_secs.max(5));

    let ver = exec::run(Path::new("."), &sc.engine, &["--version"], to, &[]);
    p.cli_version = ver.stdout.trim().to_string();
    p.cli_found = ver.passed() && !p.cli_version.is_empty();
    if !p.cli_found {
        p.hint = format!(
            "装 Docker 后重试；本机也可以是 Docker Desktop（Windows 上需先启动它，\
             守护进程常处于停止状态）。engine={}",
            sc.engine
        );
        return p;
    }

    let info = exec::run(
        Path::new("."),
        &sc.engine,
        &["info", "--format", "{{.ServerVersion}}"],
        to,
        &[],
    );
    p.daemon_version = info.stdout.trim().to_string();
    p.daemon_ok = info.passed() && !p.daemon_version.is_empty();
    if !p.daemon_ok {
        p.hint = format!(
            "docker 守护进程连不上。Windows：启动 Docker Desktop 后重试\
             （`docker info` 报 pipe 找不到就是这个原因）。原始错误：{}",
            exec::clip(info.stderr.trim(), 200)
        );
        return p;
    }

    // flags 放在位置参数**之前**：docker 的 flag 解析遇到第一个位置参数就停，
    // 放后面会被当成镜像名（这条踩过一次）。
    let inspect = || {
        exec::run(
            Path::new("."),
            &sc.engine,
            &["image", "inspect", "--format", "{{.Id}}", &sc.image],
            to,
            &[],
        )
    };
    let mut img = inspect();
    if !(img.passed() && !img.stdout.trim().is_empty()) {
        // **重试一次**：daemon 偶发抖动会假报"镜像不存在"，而 require 模式下
        // 这一条会把**全部**验证变成"拒绝执行" —— 代价太大，不能只试一次就下结论。
        img = inspect();
    }
    p.image_present = img.passed() && !img.stdout.trim().is_empty();
    if !p.image_present {
        // 把三种失败分开：只有明确说"没有这个镜像"才是"不存在"，
        // 其余（起不来、空 stderr、别的报错）都算"不知道"。
        // 曾经因为空 stderr 落进 else 分支，被误报成"本地没有镜像"，
        // 而 require 模式下这一条会把**全部**验证变成拒绝执行。
        let err = img.stderr.trim();
        p.image_probe_error = if err.contains("No such image") {
            None
        } else if let Some(sp) = &img.spawn_error {
            Some(format!("起不来 {}：{}", sc.engine, exec::clip(sp, 160)))
        } else if err.is_empty() {
            Some(format!(
                "{} image inspect 退出码 {:?} 且没有 stderr —— 原因未知，不代表镜像不存在",
                sc.engine, img.exit_code
            ))
        } else {
            Some(exec::clip(err, 200))
        };
    }
    p.cmd = format!(
        "{} info && {} image inspect {} ",
        sc.engine, sc.engine, sc.image
    );
    p.usable = p.daemon_ok && p.image_present;
    if !p.image_present {
        p.hint = match &p.image_probe_error {
            Some(e) => format!(
                "查询镜像时出错：{e}\n（这**不代表**镜像不存在。先确认 daemon 正常：{} info）",
                sc.engine
            ),
            None => format!(
                "本地没有镜像 {}。先拉一次（需要网络，且 `--network none` 的容器拉不了镜像）：\n  {} pull {}",
                sc.image, sc.engine, sc.image
            ),
        };
    }
    p
}

/// 主机路径 → 容器能用的挂载源路径。
///
/// Windows 上要把 `D:\x\y` 转成 `D:/x/y`（Docker Desktop 认这种原生风格；
/// MSYS 的 `/d/x/y` 它不认）。Linux/macOS 原样返回。
pub fn mount_source(host: &Path) -> Result<String, String> {
    let s = host
        .to_str()
        .ok_or_else(|| format!("路径不是合法 UTF-8：{}", host.display()))?
        .to_string();
    let s = s.replace('\\', "/");
    // `\\?\D:\x` 这类 verbatim 前缀 Docker 不认，去掉
    let s = s.strip_prefix("//?/").unwrap_or(&s).to_string();
    if s.contains(':') {
        // Windows 盘符路径：确保是 `D:/x` 而不是 `D:x`
        let (drive, rest) = s.split_at(s.find(':').unwrap_or(0));
        let rest = rest.trim_start_matches(':');
        Ok(format!("{drive}:/{}", rest.trim_start_matches('/')))
    } else {
        Ok(s)
    }
}

/// 项目目录在容器里的挂载点。固定为 `/work`，于是容器内命令可以用相对路径。
pub const WORKDIR_IN_CONTAINER: &str = "/work";

/// 构造 `docker run` 的参数（不含 `docker` 本身）。**纯粹函数，便于单测逐条核对限制项。**
pub fn run_argv(
    sc: &SandboxConfig,
    project: &Path,
    name: &str,
    cmd: &[String],
) -> Result<Vec<String>, String> {
    if cmd.is_empty() {
        return Err("容器里要跑什么？命令为空".into());
    }
    let src = mount_source(project)?;
    let mut a: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--name".into(),
        name.into(),
        // T4：越权相关
        "--read-only".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--user".into(),
        sc.user.clone(),
        // T3：资源耗尽相关
        "--memory".into(),
        sc.memory.clone(),
        "--memory-swap".into(),
        sc.memory.clone(),
        "--cpus".into(),
        sc.cpus.clone(),
        "--pids-limit".into(),
        sc.pids_limit.to_string(),
        // 需要写 /tmp 的检查仍然可用，但落不到宿主机
        "--tmpfs".into(),
        format!("/tmp:rw,size={},mode=1777", sc.tmpfs_size),
        // T2：默认无网
        "--network".into(),
        sc.network.clone(),
        // T1：只挂项目目录
        "-v".into(),
        format!("{src}:{WORKDIR_IN_CONTAINER}:rw"),
        "-w".into(),
        WORKDIR_IN_CONTAINER.into(),
        "-e".into(),
        "PYTHONIOENCODING=utf-8".into(),
        "-e".into(),
        "PYTHONDONTWRITEBYTECODE=1".into(),
        // 镜像
        sc.image.clone(),
    ];
    a.extend(cmd.iter().cloned());
    Ok(a)
}

/// 由配置描述"沙箱长什么样"。与 `run_argv` 里真正下发的参数保持一一对应。
pub fn sandboxed_isolation(sc: &SandboxConfig) -> Isolation {
    Isolation::Sandboxed {
        engine: sc.engine.clone(),
        image: sc.image.clone(),
        network: sc.network.clone(),
        memory: sc.memory.clone(),
        cpus: sc.cpus.clone(),
        pids_limit: sc.pids_limit,
        read_only_rootfs: true,
        user: sc.user.clone(),
    }
}

/// 三档策略的判定结果。
#[derive(Debug, Clone)]
pub enum Decision {
    /// 可以跑，并且明确知道隔离状态是什么
    Run(Isolation),
    /// 拒绝执行（mode=require 且沙箱不可用）
    Refuse(String),
}

/// 策略门：**这是"不允许静默降级"的落点**。
pub fn decide(cfg: &AppConfig, probe: &Probe) -> Decision {
    let mode = cfg.sandbox.mode.trim().to_ascii_lowercase();
    match mode.as_str() {
        "off" => Decision::Run(Isolation::Unsandboxed {
            reason: "配置 sandbox.mode = off：知情选择在宿主机执行".into(),
        }),
        "prefer" => {
            if probe.usable {
                Decision::Run(sandboxed_isolation(&cfg.sandbox))
            } else {
                Decision::Run(Isolation::Unsandboxed {
                    reason: format!(
                        "沙箱不可用，按 mode=prefer 降级到宿主机（结果会标记为未隔离）：{}",
                        probe.summary()
                    ),
                })
            }
        }
        // 未认识的取值也按最严格处理：安全策略不该因为拼错而放松
        _ => {
            if probe.usable {
                Decision::Run(sandboxed_isolation(&cfg.sandbox))
            } else {
                Decision::Refuse(format!(
                    "沙箱不可用且 mode=require：拒绝在宿主机执行生成出来的代码。{}",
                    probe.hint
                ))
            }
        }
    }
}

/// 一次运行/一次验证要用的隔离方案。
///
/// **先探一次、再决定所有命令怎么跑**：如果让每个检查点各自判断，
/// 就一定会有漏网的，而漏网的那个正好是没隔离的那个。
pub struct Plan {
    /// 是否真的进容器
    pub sandboxed: bool,
    /// 本次执行的隔离状态（会带进报告，用户永远能核对）
    pub isolation: Isolation,
    /// 沙箱配置（容器参数从这里来）
    pub cfg: AppConfig,
    /// 挂载源：项目根目录（容器里挂在 /work）
    pub mount_root: PathBuf,
    /// 容器名里的标记（便于按名杀容器）
    pub tag: String,
    pub cancel: CancelFlag,
}

/// 定一次隔离方案。返回 `Err` 表示 **mode=require 且沙箱不可用 → 拒绝执行**。
pub fn plan_with(
    cfg: &AppConfig,
    mount_root: &Path,
    tag: &str,
    cancel: CancelFlag,
) -> Result<Plan, String> {
    let p = probe(cfg);
    match decide(cfg, &p) {
        Decision::Run(iso) => Ok(Plan {
            sandboxed: iso.is_sandboxed(),
            isolation: iso,
            cfg: cfg.clone(),
            mount_root: mount_root.to_path_buf(),
            tag: tag.to_string(),
            cancel,
        }),
        Decision::Refuse(reason) => Err(reason),
    }
}

/// 在沙箱里跑一条命令。
///
/// **超时是两步杀**：`docker` CLI 被 kill 之后容器会继续活着，所以还要按名字杀容器
/// （`--rm` 让它自行消失）。这与 v0.1"必须连进程树一起杀"是同一条纪律 ——
/// 只是这里的"进程树"长在容器里。
pub fn run_in_sandbox(
    cfg: &AppConfig,
    project: &Path,
    name: &str,
    cmd: &[String],
    timeout: Duration,
    flag: &CancelFlag,
) -> Result<(CmdOutput, Isolation), String> {
    let sc = &cfg.sandbox;
    let argv = run_argv(sc, project, name, cmd)?;
    let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let out = exec::run_with_cap(project, &sc.engine, &refs, timeout, &[], 1 << 22);

    if out.timed_out || exec::is_cancelled(flag) {
        // 点名杀容器（best-effort；容器若已退出这条会失败，无所谓）
        let _ = exec::run(
            Path::new("."),
            &sc.engine,
            &["kill", name],
            Duration::from_secs(20),
            &[],
        );
    }
    if let Some(e) = &out.spawn_error {
        return Err(format!("启动 {} 失败：{e}", sc.engine));
    }
    Ok((out, sandboxed_isolation(sc)))
}

/// 一次性联网准备（拉镜像）。**这是唯一允许联网的入口**，且只在人显式调用时发生。
pub fn prepare(cfg: &AppConfig) -> Result<CmdOutput, String> {
    let sc = &cfg.sandbox;
    let p = probe(cfg);
    if !p.cli_found {
        return Err(p.hint);
    }
    if !p.daemon_ok {
        return Err(p.hint);
    }
    let out = exec::run(
        Path::new("."),
        &sc.engine,
        &["pull", &sc.image],
        Duration::from_secs(900),
        &[],
    );
    if !out.passed() {
        return Err(format!(
            "拉取镜像 {} 失败：{}",
            sc.image,
            exec::clip(out.stderr.trim(), 400)
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc() -> SandboxConfig {
        SandboxConfig::default()
    }

    fn argv(cmd: &[&str]) -> Vec<String> {
        let cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        run_argv(&sc(), Path::new("/tmp/proj"), "harness-t1-1", &cmd).unwrap()
    }

    fn has(a: &[String], pair: [&str; 2]) -> bool {
        a.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1])
    }

    #[test]
    fn argv_carries_every_hardening_flag() {
        // 这一条是"我声称的隔离"与"我实际下发的参数"之间的对照表。
        // 任何一条限制被删掉，这里都会红。
        let a = argv(&["python", "-m", "unittest"]);
        assert!(!has(&a, ["--read-only", ""])); // --read-only 无值，见下
        assert!(a.iter().any(|x| x == "--read-only"), "{a:?}");
        assert!(has(&a, ["--cap-drop", "ALL"]), "{a:?}");
        assert!(has(&a, ["--security-opt", "no-new-privileges"]), "{a:?}");
        assert!(has(&a, ["--network", "none"]), "{a:?}");
        assert!(has(&a, ["--pids-limit", "256"]), "{a:?}");
        assert!(has(&a, ["--memory", "1g"]), "{a:?}");
        assert!(has(&a, ["--memory-swap", "1g"]), "{a:?}");
        assert!(has(&a, ["--cpus", "1.0"]), "{a:?}");
        assert!(has(&a, ["--user", "1000:1000"]), "{a:?}");
        assert!(has(&a, ["-w", WORKDIR_IN_CONTAINER]), "{a:?}");
        assert!(a.iter().any(|x| x == "--rm"), "{a:?}");
        assert!(a.iter().any(|x| x == "--name"), "{a:?}");
        assert!(a.iter().any(|x| x.starts_with("/tmp:rw,size=")), "{a:?}");
    }

    #[test]
    fn argv_mounts_only_the_project_and_is_read_write() {
        let a = argv(&["python", "-c", "print(1)"]);
        let mounts: Vec<&String> = a
            .windows(2)
            .filter(|w| w[0] == "-v")
            .map(|w| &w[1])
            .collect();
        assert_eq!(mounts.len(), 1, "只允许挂一个目录：{mounts:?}");
        assert!(mounts[0].ends_with(":/work:rw"), "{}", mounts[0]);
    }

    #[test]
    fn argv_puts_the_container_command_after_the_image() {
        let a = argv(&["python", "-m", "unittest", "discover"]);
        let img = a.iter().position(|x| x == &sc().image).expect("镜像");
        assert_eq!(&a[img + 1..], &["python", "-m", "unittest", "discover"]);
    }

    #[test]
    fn empty_command_is_rejected_loudly() {
        let err = run_argv(&sc(), Path::new("/tmp/p"), "n", &[]).unwrap_err();
        assert!(err.contains("命令为空"), "{err}");
    }

    #[test]
    fn mount_source_converts_windows_paths() {
        // Docker Desktop 认得 `D:/x`，不认 MSYS 的 `/d/x`，也不认 verbatim 前缀
        assert_eq!(
            mount_source(Path::new(r"D:\Projects\a b")).unwrap(),
            "D:/Projects/a b"
        );
        assert_eq!(mount_source(Path::new(r"\\?\D:\x\y")).unwrap(), "D:/x/y");
    }

    #[test]
    fn mount_source_keeps_posix_paths() {
        assert_eq!(mount_source(Path::new("/home/u/p")).unwrap(), "/home/u/p");
    }

    #[test]
    fn require_mode_refuses_when_sandbox_unavailable() {
        // 默认策略：宁可"这次没验证成"，也不要"以为验证了"
        let mut cfg = AppConfig::default();
        cfg.sandbox.mode = "require".into();
        let d = decide(&cfg, &Probe::default());
        match d {
            Decision::Refuse(msg) => assert!(msg.contains("拒绝"), "{msg}"),
            other => panic!("require 模式下不可用必须拒绝：{other:?}"),
        }
    }

    #[test]
    fn prefer_mode_downgrades_but_labels_it() {
        let mut cfg = AppConfig::default();
        cfg.sandbox.mode = "prefer".into();
        match decide(&cfg, &Probe::default()) {
            Decision::Run(iso) => {
                assert!(!iso.is_sandboxed(), "降级必须是未隔离");
                assert!(iso.label().contains("未隔离"), "{}", iso.label());
                assert!(iso.label().contains("沙箱不可用"), "{}", iso.label());
            }
            other => panic!("prefer 应当降级而不是拒绝：{other:?}"),
        }
    }

    #[test]
    fn off_mode_is_an_explicit_choice() {
        let mut cfg = AppConfig::default();
        cfg.sandbox.mode = "off".into();
        match decide(&cfg, &Probe::default()) {
            Decision::Run(iso) => assert!(iso.label().contains("知情选择"), "{}", iso.label()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_mode_falls_back_to_the_strictest_behaviour() {
        // 拼错的策略不该让安全放松
        let mut cfg = AppConfig::default();
        cfg.sandbox.mode = "requiree".into();
        assert!(matches!(
            decide(&cfg, &Probe::default()),
            Decision::Refuse(_)
        ));
    }

    #[test]
    fn usable_sandbox_runs_in_container_for_all_modes() {
        let probe = Probe {
            daemon_ok: true,
            image_present: true,
            usable: true,
            ..Default::default()
        };
        for mode in ["require", "prefer", "off"] {
            let mut cfg = AppConfig::default();
            cfg.sandbox.mode = mode.into();
            // off 是"知情选择在宿主跑"，其余两档可用时必须进容器
            match decide(&cfg, &probe) {
                Decision::Run(iso) => {
                    assert_eq!(iso.is_sandboxed(), mode != "off", "mode={mode}");
                    if mode != "off" {
                        assert!(iso.label().contains("docker"), "{}", iso.label());
                    }
                }
                other => panic!("mode={mode} 不该拒绝：{other:?}"),
            }
        }
    }

    #[test]
    fn probe_summary_distinguishes_probe_error_from_missing_image() {
        // 真机踩到过：daemon 抖动让 `image inspect` 假报"不存在"，
        // 而 require 模式下这一条会把**全部**验证变成"拒绝执行"。
        // "查询失败"与"确定不存在"必须给用户不同的指令。
        let p = Probe {
            cli_found: true,
            daemon_ok: true,
            image_present: false,
            image: "python:3.12-slim".into(),
            image_probe_error: Some("daemon 忙".into()),
            ..Default::default()
        };
        let s = p.summary();
        assert!(s.contains("出错"), "{s}");
        assert!(s.contains("不等于"), "必须说清这不是「不存在」：{s}");
        assert!(!s.contains("需要先拉取"), "查询失败时不该让人白拉一遍：{s}");
    }

    #[test]
    fn probe_summary_names_the_missing_piece() {
        let p = Probe {
            cli_found: true,
            daemon_ok: false,
            ..Default::default()
        };
        assert!(p.summary().contains("守护进程"), "{}", p.summary());
        let p2 = Probe {
            cli_found: true,
            daemon_ok: true,
            image_present: false,
            image: "python:3.12-slim".into(),
            ..Default::default()
        };
        assert!(
            p2.summary().contains("python:3.12-slim"),
            "{}",
            p2.summary()
        );
    }

    // ---------------------------------------------------------------- 活体：真隔离

    fn live_cfg() -> AppConfig {
        AppConfig::default()
    }

    fn container_run(cfg: &AppConfig, root: &Path, py: &str) -> String {
        let flag: CancelFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cmd = vec!["python".to_string(), "-c".to_string(), py.to_string()];
        let (out, iso) = run_in_sandbox(
            cfg,
            root,
            "harness-live-probe",
            &cmd,
            std::time::Duration::from_secs(120),
            &flag,
        )
        .expect("容器应当能起来");
        assert!(iso.is_sandboxed(), "这一切必须在容器里跑：{}", iso.label());
        format!(
            "exit={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.exit_code,
            out.stdout.trim(),
            out.stderr.trim()
        )
    }

    /// **真隔离证据 1/5：根文件系统只读** —— 写系统目录必须失败。
    #[test]
    #[ignore = "需要 Docker（真起容器）"]
    fn live_read_only_rootfs_rejects_writing_system_dirs() {
        let d = std::env::temp_dir().join("dh-sbx-ro");
        let _ = std::fs::create_dir_all(&d);
        let cfg = live_cfg();
        let r = container_run(
            &cfg,
            &d,
            "open('/etc/harness_hack','w').write('x'); print('WROTE')",
        );
        println!("[只读根文件系统]\n{r}");
        assert!(!r.contains("WROTE"), "根文件系统只读：不该写进 /etc\n{r}");
        assert!(
            r.contains("Read-only") || r.contains("Permission denied") || r.contains("Errno 30"),
            "应当是显式的拒绝（而不是静默成功）\n{r}"
        );
    }

    /// **真隔离证据 2/5：无网络** —— 连外网必须失败。
    #[test]
    #[ignore = "需要 Docker（真起容器）"]
    fn live_network_none_blocks_outbound() {
        let d = std::env::temp_dir().join("dh-sbx-net");
        let _ = std::fs::create_dir_all(&d);
        let cfg = live_cfg();
        let r = container_run(
            &cfg,
            &d,
            "import socket\ns=socket.socket()\ns.settimeout(8)\ns.connect(('1.1.1.1',443))\nprint('CONNECTED')",
        );
        println!("[无网络]\n{r}");
        assert!(!r.contains("CONNECTED"), "容器不该连得上外网\n{r}");
    }

    /// **真隔离证据 3/5：只挂项目目录** —— 宿主机的其它路径在容器里不存在。
    #[test]
    #[ignore = "需要 Docker（真起容器）"]
    fn live_only_the_project_dir_is_mounted() {
        let base = std::env::temp_dir().join("dh-sbx-mount");
        let proj = base.join("proj");
        let outside = base.join("outside");
        let _ = std::fs::create_dir_all(&proj);
        let _ = std::fs::create_dir_all(&outside);
        std::fs::write(outside.join("secret.txt"), "宿主机的秘密").unwrap();
        std::fs::write(proj.join("inside.txt"), "项目内文件").unwrap();

        let cfg = live_cfg();
        let r = container_run(
            &cfg,
            &proj,
            "import os\nprint('INSIDE', os.path.exists('inside.txt'))\nprint('ESCAPE', os.path.exists('/work/../outside/secret.txt'))\nprint('HOSTROOT', os.path.exists('/d') or os.path.exists('/Users') or os.path.exists('/mnt/d'))\nprint('LIST', sorted(os.listdir('/')))",
        );
        println!("[只挂项目目录]\n{r}");
        assert!(r.contains("INSIDE True"), "项目目录必须可见\n{r}");
        assert!(
            r.contains("ESCAPE False"),
            "挂载点之外必须不可见（这是 T1 的关键）\n{r}"
        );
        assert!(r.contains("HOSTROOT False"), "不该看见宿主机的盘符\n{r}");
    }

    /// **真隔离证据 4/5：非 root** —— 容器里不是 root 用户。
    #[test]
    #[ignore = "需要 Docker（真起容器）"]
    fn live_runs_as_non_root() {
        let d = std::env::temp_dir().join("dh-sbx-uid");
        let _ = std::fs::create_dir_all(&d);
        let cfg = live_cfg();
        let r = container_run(&cfg, &d, "import os\nprint('UID', os.getuid())");
        println!("[非 root]\n{r}");
        assert!(r.contains("UID 1000"), "必须以非 root 跑\n{r}");
    }

    /// **真隔离证据 5/5：/tmp 可写但不落宿主机** —— 检查写临时文件不会污染环境。
    #[test]
    #[ignore = "需要 Docker（真起容器）"]
    fn live_tmp_is_tmpfs_not_host() {
        let d = std::env::temp_dir().join("dh-sbx-tmp");
        let _ = std::fs::create_dir_all(&d);
        let cfg = live_cfg();
        let r = container_run(
            &cfg,
            &d,
            "open('/tmp/x','w').write('y'); print('TMPOK', open('/tmp/x').read())",
        );
        println!("[/tmp 是 tmpfs]\n{r}");
        assert!(r.contains("TMPOK y"), "/tmp 要能写（检查需要它）\n{r}");
        assert!(
            !d.join("x").exists() && !d.join("tmp").join("x").exists(),
            "容器里的 /tmp 不该落到项目目录里"
        );
    }

    /// 探针：真的连一次守护进程 + 检查镜像在不在。
    #[test]
    #[ignore = "需要 Docker"]
    fn live_probe_reports_usable_on_this_machine() {
        let cfg = live_cfg();
        let p = probe(&cfg);
        println!("探针：{}", p.summary());
        println!(
            "  cli={} daemon={} image={}",
            p.cli_version, p.daemon_version, p.image_present
        );
        assert!(p.cli_found, "本机应当有 docker CLI：{}", p.hint);
        assert!(p.daemon_ok, "守护进程应当可达：{}", p.hint);
        assert!(p.image_present, "镜像应当已拉取：{}", p.hint);
        assert!(p.usable);
    }
}
