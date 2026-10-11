//! 环境准备（`kind = "env"` 的 Connect 落地）：按需安装缺失的命令行工具。
//!
//! 哲学与命令发现（引擎 `discover` 模块）一致：**引擎不碰包管理器**。引擎只认
//! `Connector` 契约与 `agent::ENV_CONNECTOR_KIND`，装什么、用哪个包管理器、装不装，
//! 全在这里裁量 —— 所以换平台是改这张表，不是改引擎里一格一格的 `if`。
//!
//! 纪律：
//! - 只在模型**显式** `connect` 请求时动手（不预装、不猜、不扫盘）；
//! - 每次动作**留记录**（`<根>/projects/<项目 key>/env/installs.jsonl`）并播 UI 事件 —— "有记录" 是这条
//!   能力的硬要求，装了什么、用什么命令装的、成没成，都要能事后追；
//! - 包管理器按平台优先级挑**第一个可用的**，挑不到就把原因回给模型（不硬凑）。

use harness_engine as engine;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 用户侧的一条包管理器行（`plugins/tools/<id>/tools.toml` 的 `[[pm]]`，v1.4 §4.2）。
///
/// **为什么要有它**：内置那七个管理器是 `enum` 里的变体 + `match` 臂，"再加一个包管理器"
/// 就得改源码 —— 而自改闭环要求这类增长**只改数据**。这是加载点，形状与命令发现表同一套。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmSpec {
    /// 行标识（日志里认这个名字，例如 `volta`）
    pub id: String,
    /// 探测与执行用的二进制名
    pub bin: String,
    /// 安装命令模板：`{pkg}` 会被替换成包名（**过 shell 执行**，见 `exec::run_line`）
    pub install: String,
    /// 适用平台（`windows` / `macos` / `linux`）；**空 = 全平台**
    pub platforms: Vec<String>,
    /// 安装要不要提权（unix 走 `sudo -n`）
    pub elevate: bool,
    /// 工具名 → 本管理器认的包名；表里没有的原样透传
    pub packages: std::collections::BTreeMap<String, String>,
}

/// 用户侧追加的行（启动时装载一次，见 [`register_custom`]）。
fn custom_rows() -> &'static std::sync::Mutex<Vec<&'static PmSpec>> {
    static C: std::sync::OnceLock<std::sync::Mutex<Vec<&'static PmSpec>>> =
        std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// 登记用户侧的包管理器行（宿主启动时调用一次）。返回这次加了几行。
///
/// 行本身**有意泄漏**成 `'static`：一是 `Pm::Custom` 要 `&'static PmSpec`（枚举是 `Copy`，
/// 存引用比存 String 便宜），二是装载只发生一次、行数有界（用户侧文件）。
/// 注意：登记必须发生在**第一次** `pick_manager()` / `candidates()` 之前 —— 那两处各有一份
/// 进程内缓存（启动顺序天然满足：装载在构造应用状态时，探测要等一次 run）。
pub fn register_custom(specs: Vec<PmSpec>) -> usize {
    let n = specs.len();
    if n == 0 {
        return 0;
    }
    if let Ok(mut g) = custom_rows().lock() {
        g.extend(specs.into_iter().map(|s| &*Box::leak(Box::new(s))));
    }
    n
}

/// 本平台适用的自定义行（`platforms` 为空 ⇒ 全平台）。
fn custom_for_this_platform() -> Vec<Pm> {
    let me = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let Ok(g) = custom_rows().lock() else {
        return Vec::new();
    };
    g.iter()
        .filter(|s| s.platforms.is_empty() || s.platforms.iter().any(|p| p == me))
        .map(|s| Pm::Custom(s))
        .collect()
}

/// 单次安装的超时。包要下载、可能要编译，给足 —— 这是后台动作，模型在等它。
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);
/// 探测包管理器是否存在（`where` / `command -v`）的超时
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// 宿主摆进 connect 清单的这个目标名。模型照清单里的名字发起 `connect`，
/// 所以它既是清单标识、也是 `ConnectRequest::server` 的取值。
pub const ENV_SERVER: &str = "env";
/// 这个目标下的工具名（`ConnectRequest::tool`）
pub const TOOL_INSTALL: &str = "install";

/// 一个包管理器。**引擎不认识这些名字** —— 只在宿主这里有意义。
///
/// `allow(dead_code)`：变体按平台构造（[`candidates`] 是 cfg 分平台的），其它平台的变体
/// 在 `match` 里仍要穷举 —— 表是完整的，只是当前平台用不到。
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pm {
    Winget,
    Scoop,
    Choco,
    Brew,
    AptGet,
    Dnf,
    Pacman,
    /// 用户侧追加的行（`plugins/tools/<id>/tools.toml`）
    Custom(&'static PmSpec),
}

impl Pm {
    pub fn bin(self) -> &'static str {
        match self {
            Pm::Winget => "winget",
            Pm::Scoop => "scoop",
            Pm::Choco => "choco",
            Pm::Brew => "brew",
            Pm::AptGet => "apt-get",
            Pm::Dnf => "dnf",
            Pm::Pacman => "pacman",
            Pm::Custom(s) => s.bin.as_str(),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Pm::Custom(s) => s.id.as_str(),
            other => other.bin(),
        }
    }

    /// 工具名 → 本包管理器认的包名 / ID。表里没有的原样透传
    /// （多数管理器能按裸名查，查不到就把错误回给模型）。
    fn package(self, tool: &str) -> String {
        let t = tool.trim();
        let a = match self {
            Pm::Winget => match t {
                "mvn" | "maven" => "Apache.Maven",
                "java" | "jdk" => "Microsoft.OpenJDK.21",
                "python" => "Python.Python.3.12",
                "node" => "OpenJS.NodeJS.LTS",
                "go" => "GoLang.Go",
                "docker" => "Docker.DockerDesktop",
                "git" => "Git.Git",
                "cmake" => "Kitware.CMake",
                "make" => "GnuWin32.Make",
                _ => t,
            },
            Pm::Scoop => match t {
                "mvn" | "maven" => "maven",
                "java" | "jdk" => "openjdk",
                "python" => "python",
                "node" => "nodejs-lts",
                "go" => "go",
                _ => t,
            },
            Pm::Choco => match t {
                "mvn" | "maven" => "maven",
                "java" | "jdk" => "openjdk",
                "python" => "python3",
                "node" => "nodejs-lts",
                "go" => "golang",
                "docker" => "docker-desktop",
                _ => t,
            },
            Pm::Brew => match t {
                "mvn" | "maven" => "maven",
                "java" | "jdk" => "openjdk",
                "python" => "python3",
                "go" => "go",
                "node" => "node",
                "docker" => "docker",
                _ => t,
            },
            Pm::AptGet => match t {
                "mvn" | "maven" => "maven",
                "java" | "jdk" => "default-jdk",
                "python" => "python3",
                "node" => "nodejs",
                "go" => "golang-go",
                "docker" => "docker.io",
                "cmake" => "cmake",
                "make" => "make",
                _ => t,
            },
            Pm::Dnf => match t {
                "mvn" | "maven" => "maven",
                "java" | "jdk" => "java-latest-openjdk",
                "python" => "python3",
                "go" => "golang",
                _ => t,
            },
            Pm::Pacman => match t {
                "mvn" | "maven" => "maven",
                "java" | "jdk" => "jdk-openjdk",
                "python" => "python",
                "go" => "go",
                _ => t,
            },
            // 用户侧的行：包名映射由它的 `[pm.packages]` 给，没有就原样透传
            Pm::Custom(s) => match s.packages.get(t) {
                Some(p) => p.as_str(),
                None => t,
            },
        };
        a.to_string()
    }

    /// 拼出安装命令行（**交给 shell** 执行，见 `exec::run_line` 关于 `.cmd` 的说明）。
    /// 全部带非交互开关：这条命令在后台跑，没有 tty 可以回答 "y/n"。
    pub fn install_line(self, tool: &str) -> String {
        let p = self.package(tool);
        match self {
            Pm::Winget => format!(
                "winget install --id {p} -e --source winget \
                 --accept-package-agreements --accept-source-agreements"
            ),
            Pm::Scoop => format!("scoop install {p}"),
            Pm::Choco => format!("choco install {p} -y --no-progress"),
            Pm::Brew => format!("brew install {p}"),
            Pm::AptGet => elevated(&format!("apt-get install -y {p}")),
            Pm::Dnf => elevated(&format!("dnf install -y {p}")),
            Pm::Pacman => elevated(&format!("pacman -S --noconfirm {p}")),
            Pm::Custom(s) => {
                let line = s.install.replace("{pkg}", &p);
                if s.elevate { elevated(&line) } else { line }
            }
        }
    }
}

/// Unix 的安装要 root。`sudo -n`（非交互）—— 没有免密 sudo 就干净失败，
/// 而不是挂在那里等一个永远等不到的密码输入。Windows 走不到这里。
fn elevated(line: &str) -> String {
    #[cfg(unix)]
    {
        if engine::exec::resolve_bin("sudo", PROBE_TIMEOUT).is_some() {
            return format!("sudo -n {line}");
        }
    }
    line.to_string()
}

/// 平台默认优先级（挑第一个 `where` / `command -v` 得到的）
fn platform_defaults() -> &'static [Pm] {
    #[cfg(target_os = "windows")]
    {
        &[Pm::Winget, Pm::Scoop, Pm::Choco]
    }
    #[cfg(target_os = "macos")]
    {
        &[Pm::Brew]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        &[Pm::AptGet, Pm::Dnf, Pm::Pacman]
    }
}

/// 候选清单 = **平台默认** + 用户侧追加行（v1.4 §4.2）。
///
/// 进程内缓存一次（合并结果在进程生命期内不变 —— 与 `pick_manager` 同一口径）。
/// 因此 [`register_custom`] 必须在这之前跑：启动时装载、探测要等一次 run，顺序天然满足。
pub fn candidates() -> &'static [Pm] {
    static ALL: std::sync::OnceLock<Vec<Pm>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<Pm> = platform_defaults().to_vec();
        v.extend(custom_for_this_platform());
        v
    })
}

/// 本机可用的第一个包管理器（宿主裁量：优先级在 [`candidates`]）。
///
/// 进程内缓存：`list()` 每次 run 都会问一次"有哪些包管理器"，不能每次都起三个探测进程。
/// 结果在进程生命周期内不变（装/卸包管理器要重启 IDE 才感知 —— 可接受）。
pub fn pick_manager() -> Option<Pm> {
    static CACHE: std::sync::OnceLock<Option<Pm>> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        candidates()
            .iter()
            .copied()
            .find(|pm| engine::exec::resolve_bin(pm.bin(), PROBE_TIMEOUT).is_some())
    })
}

/// 一次安装动作的留痕（写成 `<根>/projects/<项目 key>/env/installs.jsonl` 的一行）
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct InstallRecord {
    /// epoch 毫秒（不引 chrono，够排序与追溯）
    pub ts_ms: u128,
    pub tool: String,
    /// 模型给的理由（为什么需要装）
    pub reason: String,
    /// 实际用的包管理器名（挑不到时为空）
    pub manager: String,
    /// 实际执行的命令行
    pub cmd: String,
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    /// 输出尾巴（裁剪过），方便事后看为什么失败
    pub output: String,
}

impl InstallRecord {
    /// 一条回给模型的说明（也是 UI 事件的文案基础）
    pub fn text(&self) -> String {
        if self.manager.is_empty() {
            return format!("没能安装 {}：{}", self.tool, self.output);
        }
        let head = if self.ok {
            format!("已安装 {}", self.tool)
        } else {
            format!("安装 {} 失败", self.tool)
        };
        let mut s = format!(
            "{head}（{}：{}）退出码 {:?}\n{}",
            self.manager,
            self.cmd,
            self.exit_code,
            self.output.trim()
        );
        if self.ok {
            s.push_str("\n提示：命令发现缓存已失效，重新探测即可看到它。");
        }
        s
    }
}

/// 记录文件：**便携根**里的项目桶 —— 与 stage / backup 同级，都属"这次会话的产物"。
pub fn record_path(project_root: &Path) -> PathBuf {
    crate::paths::current()
        .project_bucket(&project_root.to_string_lossy(), "env")
        .join("installs.jsonl")
}

fn stamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 把记录追加成一行 JSONL。**失败不报错**：记录是附带产物，不该反过来把安装判定搞砸。
fn append_record(path: &Path, rec: &InstallRecord) {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(line) = serde_json::to_string(rec) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// 真正跑一次安装（**阻塞**：调用方要放进 `spawn_blocking`）。
///
/// 返回的 [`InstallRecord`] 同时是"实况"与"存证"——调用方拿它渲染回模型的文本、
/// 播 UI 事件；这里负责写 jsonl。
pub fn install(project_root: &Path, tool: &str, reason: &str) -> InstallRecord {
    let tool = tool.trim().to_string();
    let started = std::time::Instant::now();
    let Some(pm) = pick_manager() else {
        let rec = InstallRecord {
            ts_ms: stamp_ms(),
            tool,
            reason: reason.to_string(),
            manager: String::new(),
            cmd: String::new(),
            ok: false,
            exit_code: None,
            duration_ms: started.elapsed().as_millis(),
            output: "本机没找到可用的包管理器（Windows: winget/scoop/choco；\
                     macOS: brew；Linux: apt-get/dnf/pacman）。请用户手动安装后重试。"
                .to_string(),
        };
        append_record(&record_path(project_root), &rec);
        return rec;
    };

    let cmd = pm.install_line(&tool);
    let out = engine::exec::run_line(project_root, &cmd, INSTALL_TIMEOUT);
    let ok = out.passed();
    // 输出可能很大（下载日志），裁剪后再留痕 / 回给模型
    let mut raw = String::new();
    if let Some(e) = &out.spawn_error {
        raw.push_str(&format!("[启动失败] {e}\n"));
    }
    if !out.stdout.trim().is_empty() {
        raw.push_str(out.stdout.trim());
        raw.push('\n');
    }
    if !out.stderr.trim().is_empty() {
        raw.push_str(out.stderr.trim());
    }
    let output = engine::exec::clip(raw.trim(), 4000);

    let rec = InstallRecord {
        ts_ms: stamp_ms(),
        tool,
        reason: reason.to_string(),
        manager: pm.name().to_string(),
        cmd,
        ok,
        exit_code: out.exit_code,
        duration_ms: started.elapsed().as_millis(),
        output,
    };
    append_record(&record_path(project_root), &rec);
    // 装成功了，之前"缺这个命令"的探测结论就过期了 —— 主动失效，下一次探测立刻看到新工具。
    if rec.ok {
        engine::discover::invalidate_cache();
    }
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_aliases_cover_the_common_tools() {
        assert_eq!(Pm::AptGet.package("mvn"), "maven");
        assert_eq!(Pm::AptGet.package("java"), "default-jdk");
        assert_eq!(Pm::AptGet.package("maven"), "maven");
        assert_eq!(Pm::Brew.package("mvn"), "maven");
        assert_eq!(Pm::Winget.package("mvn"), "Apache.Maven");
        assert_eq!(Pm::Choco.package("node"), "nodejs-lts");
        // 表外工具原样透传（不猜）
        assert_eq!(Pm::Scoop.package("ripgrep"), "ripgrep");
        // 首尾空白被清掉
        assert_eq!(Pm::Brew.package("  go  "), "go");
    }

    #[test]
    fn install_lines_are_non_interactive_and_use_resolved_packages() {
        assert!(Pm::Winget.install_line("mvn").contains("--id Apache.Maven"));
        assert!(
            Pm::Winget
                .install_line("mvn")
                .contains("--accept-package-agreements")
        );
        assert_eq!(Pm::Brew.install_line("mvn"), "brew install maven");
        assert_eq!(Pm::Scoop.install_line("java"), "scoop install openjdk");
        assert!(Pm::Choco.install_line("git").contains("-y"));
        assert!(
            Pm::AptGet
                .install_line("git")
                .ends_with("apt-get install -y git")
        );
        assert!(Pm::Pacman.install_line("git").contains("--noconfirm"));
    }

    #[test]
    fn candidates_are_platform_specific_and_non_empty() {
        let c = candidates();
        assert!(!c.is_empty());
        #[cfg(target_os = "windows")]
        assert_eq!(c, &[Pm::Winget, Pm::Scoop, Pm::Choco][..]);
        #[cfg(target_os = "macos")]
        assert_eq!(c, &[Pm::Brew][..]);
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(c, &[Pm::AptGet, Pm::Dnf, Pm::Pacman][..]);
    }

    #[test]
    fn record_path_lives_under_ruyix_env() {
        let p = record_path(Path::new("/tmp/proj"));
        assert!(p.ends_with("env/installs.jsonl"), "{p:?}");
    }

    /// 没包管理器时的记录：可序列化、有解释、且真的落盘一行。
    #[test]
    fn record_is_appended_as_one_jsonl_line() {
        let dir = std::env::temp_dir().join(format!("ruyix-env-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rec = InstallRecord {
            ts_ms: 1,
            tool: "mvn".into(),
            reason: "构建 Java 项目".into(),
            manager: "brew".into(),
            cmd: "brew install maven".into(),
            ok: false,
            exit_code: Some(1),
            duration_ms: 5,
            output: "boom".into(),
        };
        let path = record_path(&dir);
        append_record(&path, &rec);
        append_record(&path, &rec);
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 2, "两次追加两行");
        let back: InstallRecord = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(back, rec);
        // text() 把成 / 败分别讲清楚
        assert!(rec.text().contains("安装 mvn 失败"));
        let ok = InstallRecord {
            ok: true,
            ..rec.clone()
        };
        assert!(ok.text().contains("已安装 mvn"));
        assert!(ok.text().contains("缓存已失效"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 清单里那条 `install` 工具的说明必须点明参数形状 —— 模型照它拼 `arguments`。
    #[test]
    fn env_target_advertises_the_install_tool() {
        let (detail, tools) = crate::agent::connect::env_target_text();
        assert!(detail.contains("安装"), "{detail}");
        let t = &tools[0];
        assert!(t.starts_with("install("), "{t}");
        assert!(t.contains("\"tool\""), "要写明参数键：{t}");
        assert!(t.contains("\"reason\""), "要写明参数键：{t}");
    }
}
