//! 便携根：**全局路径的唯一解析点**（v1.0.0 P1）。
//!
//! 三条规矩，改动时别绕过：
//!
//! 1. **根判定只有一处实现**：`RUYIX_HOME`（非空）→ 否则 **exe 所在目录**。
//!    `exe 同目录 = 程序的家`，卸载 = 删这个目录（零残留）。
//! 2. **除本文件外，任何地方不许再拼 `~/.ruyix` / `.darkhorse` / `dirs::`** ——
//!    由 `no_global_path_construction_outside_this_file` 静态守住；`.ruyix` 的存量有一张
//!    `P3_DEBT` 棘轮表（v1.0.0 P3 已**清零**，表留着：以后谁再写出一处就红）。
//! 3. 顶层只有三个目录，按**归属**分（不是按"配置/数据"分）：
//!    - `global/`　全局配置（`*.toml`）+ 全局数据（`runs/ logs/ cache/ webview/`）
//!    - `projects/<key>/`　某个项目的**配置与状态同一个桶**（stage / backups / proc / env /
//!      sessions / verify / plugins）
//!    - `plugins/`　全局插件

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 便携根。构造一次（启动最早期），注入给各处 —— 也允许测试注入临时根。
///
/// 门面上的方法**故意一次摆全**：`webview_data` / `cache_dir` / `plugins_dir` / `project_bucket`
/// 要到 P2–P4 才有调用点。bin crate 里"尚未使用的公开方法"会报 `dead_code`，这里显式豁免 ——
/// 比"先删掉、下个阶段再加回来"少一次来回，也少一次漏。
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
    /// 这个根是**用户显式指定**的（`RUYIX_HOME`）还是**自动判定**的（exe 同目录）。
    ///
    /// 只有自动判定出来的根才做"临时目录"判定：`RUYIX_HOME` 是用户显式选的家，
    /// 他在哪指我们就在哪写（哪怕那个位置在临时目录里 —— 那是他的选择，不是 Windows
    /// 悄悄解压出来的）。
    explicit: bool,
}

#[allow(dead_code)]
impl Paths {
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            explicit: false,
        }
    }

    /// 根判定（**唯一实现**）。
    ///
    /// `RUYIX_HOME` 非空 → 用它（脚本 / 多环境 / 只读目录的逃生口）；否则 **exe 所在目录**。
    pub fn discover(exe_dir: &Path) -> Self {
        Self::discover_with(exe_dir, std::env::var_os("RUYIX_HOME"))
    }

    /// 同上，但环境值由调用方给 —— 单测要能测两种分支而不改进程环境
    /// （测试是并行的，动 env 会互相污染）。
    pub fn discover_with(exe_dir: &Path, ruyix_home: Option<std::ffi::OsString>) -> Self {
        match ruyix_home {
            Some(v) if !v.to_string_lossy().trim().is_empty() => Self {
                root: PathBuf::from(v),
                explicit: true,
            },
            _ => Self {
                root: exe_dir.to_path_buf(),
                explicit: false,
            },
        }
    }

    /// 这个根是用户显式指定的吗（见 `explicit` 字段）
    pub fn is_explicit(&self) -> bool {
        self.explicit
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn global_dir(&self) -> PathBuf {
        self.root.join("global")
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    /// 记忆库（v1.1 核心模块，**不可插件化**）：`<根>/global/memory/`。
    /// 模型与账本都在这一个目录下 —— 删文件夹即卸载，仓库里一个字都不写。
    pub fn memory_dir(&self) -> PathBuf {
        self.global_dir().join("memory")
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }

    /// 项目状态桶：`<root>/projects/<key>`。
    ///
    /// **参数是项目路径，不是 key** —— key 由内部算。这样调用方没有"把路径当 key 用"的
    /// 机会（那种错会静默建出一个怪目录，而不是报错）。
    pub fn project_dir(&self, project: &str) -> PathBuf {
        self.projects_dir().join(self.project_key(project))
    }

    /// 项目桶内的子目录（`stage` / `backups` / `proc` / `env` / `sessions` / `verify` / `plugins`）。
    pub fn project_bucket(&self, project: &str, bucket: &str) -> PathBuf {
        self.project_dir(project).join(bucket)
    }

    /// 项目路径 → 目录名（规则见 [`project_key`]）。
    pub fn project_key(&self, project: &str) -> String {
        project_key(project)
    }

    // ---- 全局数据（都在 global/ 下）----

    /// agent 沙箱产物根
    pub fn runs_root(&self) -> PathBuf {
        self.global_dir().join("runs")
    }

    /// `--debug` 落盘位置。刻意**不放在项目目录**：开关是启动参数，那一刻还不知道会打开哪个项目。
    pub fn debug_log(&self) -> PathBuf {
        self.global_dir().join("logs").join("debug.log")
    }

    /// WebView2 的用户数据目录（便携的关键：不指定就外溢到 `%LOCALAPPDATA%`，
    /// 实测见 `doc/架构-便携根与项目状态搬迁-v1.0.0.md` §八）
    pub fn webview_data(&self) -> PathBuf {
        self.global_dir().join("webview")
    }

    /// 命令发现等缓存
    pub fn cache_dir(&self) -> PathBuf {
        self.global_dir().join("cache")
    }

    /// 根的短指纹（8 字节 sha256 → 16 位 hex）。
    ///
    /// 用途：**单实例互斥体的名字**。便携形态天生允许多份副本并存，而互斥体若写死名字，
    /// 两份副本会互相当成"另一个自己"（P0 实测：写死名字时第二个 `first_instance=false`）。
    /// 同一个根 ⇒ 同一个名字（同一份副本的第二次启动仍然被识别）；不同根 ⇒ 不同名字（互不干扰）。
    pub fn root_fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(norm_for_compare(&self.root).as_bytes());
        h.finalize()[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    // ---- 项目桶的自我说明与清单（v1.0.0：孤儿"看得见 + 能删"）----

    /// 在项目桶里写一份 `project.toml` 自证（**这个桶属于哪个项目**）。
    ///
    /// 为什么值得一个文件：桶名是 slug，项目改名/移动后它就成了孤儿，而"这桶是谁的"光看目录名
    /// 猜不出来（大小写拿不准、换盘、UNC 都可能变）。留一份原始路径，孤儿清理时能告诉用户
    /// "这是给 `D:\Projects\xxx` 的"，而不是让人对着目录名猜。
    ///
    /// **失败不报错**（返回 `None`）：它是附带说明，不该反过来让"打开项目"失败。
    pub fn stamp_project(&self, project: &str) -> Option<PathBuf> {
        let dir = self.project_dir(project);
        std::fs::create_dir_all(&dir).ok()?;
        let body = format!(
            "# ruyix 的项目状态桶（自证）\n\
             # 本文件由 ruyix 维护：桶名是项目路径的 slug，本文件留一份原始路径便于识别孤儿桶。\n\
             key = \"{key}\"\n\
             path = \"{path}\"\n\
             last_opened = \"{ts}\"\n",
            key = self.project_key(project),
            // TOML 基本字符串里反斜杠是转义符：Windows 路径必须双写
            path = project.replace('\\', "\\\\"),
            ts = harness_engine::workspace::now_iso(),
        );
        let f = dir.join("project.toml");
        std::fs::write(&f, body).ok()?;
        Some(f)
    }

    /// 全部项目桶（按 key 排序）。体积是递归求和，"这是谁的桶"取自自证文件。
    pub fn list_buckets(&self) -> Vec<BucketInfo> {
        let dir = self.projects_dir();
        let mut out: Vec<BucketInfo> = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return out;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let key = e.file_name().to_string_lossy().into_owned();
            let project_path = std::fs::read_to_string(p.join("project.toml"))
                .ok()
                .and_then(|s| s.parse::<toml::Value>().ok())
                .and_then(|v| {
                    v.get("path")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                });
            let exists = project_path
                .as_deref()
                .map(|s| Path::new(s).is_dir())
                .unwrap_or(false);
            out.push(BucketInfo {
                key,
                path: p.to_string_lossy().to_string(),
                bytes: dir_bytes(&p),
                project_path,
                exists,
            });
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// 删掉一个项目桶（整桶，**只在用户确认后调用**）。
    ///
    /// 只允许 `projects/` 的直接子目录：key 由 UI 给（来自桶清单），这里再挡一次路径逃逸 ——
    /// 一个"删除"入口不值得赌调用方永远传对。
    pub fn delete_bucket(&self, key: &str) -> Result<String, String> {
        if key.is_empty()
            || key.contains('\\')
            || key.contains('/')
            || key.contains("..")
            || key.contains(':')
        {
            return Err(format!("非法桶名: {key}"));
        }
        let dir = self.projects_dir().join(key);
        if !dir.is_dir() {
            return Err(format!("项目桶不存在: {}", dir.display()));
        }
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除失败（{}）：{e}", dir.display()))?;
        Ok(dir.to_string_lossy().to_string())
    }

    // ---- 边界判定（v1.0.0 P2：启动时的**唯一**一次判定）----

    /// 启动时问一次：这个根能用吗？三种结论，每个都有明确动作（不许"看情况"）。
    ///
    /// 顺序即优先级：**不可写**先判（那是连目录都建不出来的硬故障）。
    pub fn root_verdict(&self) -> RootVerdict {
        if !is_writable(&self.root) {
            return RootVerdict::NotWritable;
        }
        // 用户显式 `RUYIX_HOME` ⇒ 不做临时目录判定（他在哪指我们就在哪写）
        if !self.explicit && self.is_in_temp() {
            return RootVerdict::TempDir;
        }
        RootVerdict::Ok
    }

    /// 根是不是落在系统临时目录里（`%TEMP%` / `%TMP%`）。
    ///
    /// 这一条只用来识别"**从压缩包里直接双击**"：Windows 的 ZipFolder 会把整包解到
    /// `%TEMP%\Temp<n>_<名字>.zip\…` 再运行，而那个位置会被磁盘清理/存储感知收走 ——
    /// 用户看到的现象是"我的设置怎么每次都丢"，没人会联想到临时目录。
    pub fn is_in_temp(&self) -> bool {
        ["TEMP", "TMP"].iter().any(|k| {
            std::env::var_os(k).is_some_and(|v| {
                let p = PathBuf::from(v);
                !p.as_os_str().is_empty() && is_under(&self.root, &p)
            })
        })
    }

    /// 首启按模板创建缺失的目录与说明文件。
    ///
    /// **幂等且绝不覆盖**：只在目标不存在时写（用户改过的 README 不会被我们改回去）。
    /// 建不成就是"根不可写"那条路 —— 调用方据此弹框退出。
    pub fn ensure_layout(&self) -> Result<Vec<String>, String> {
        let mut made = Vec::new();

        let dirs = [
            self.global_dir(),
            self.global_dir().join("logs"),
            self.global_dir().join("cache"),
            self.global_dir().join("webview"),
            self.runs_root(),
            self.projects_dir(),
            self.plugins_dir(),
        ];
        for d in dirs {
            if !d.exists() {
                std::fs::create_dir_all(&d)
                    .map_err(|e| format!("创建目录失败 {}：{e}", d.display()))?;
                made.push(format!("{}{}", d.display(), std::path::MAIN_SEPARATOR));
            }
        }

        for (rel, body) in TEMPLATE_FILES {
            let p = self.root.join(rel);
            if !p.exists() {
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&p, body).map_err(|e| format!("写模板失败 {}：{e}", p.display()))?;
                made.push(rel.to_string());
            }
        }
        Ok(made)
    }

    /// 这个**绝对路径**是不是在我们自己的家里（全局目录 / 某个项目的状态桶）？
    ///
    /// 用来替换"按名字拒绝 `.ruyix`"那条老规则：判据应该是"**是不是我们的家**"，
    /// 而不是"叫什么名字"—— 名字可以随便起，家只有一个（v1.0.0 P3）。
    /// 比较用归一化后的字符串（Windows 大小写不敏感、分隔符两种都收、短路径不猜）。
    pub fn is_inside_state(&self, abs: &Path) -> bool {
        if is_under(abs, &self.root) {
            return true;
        }
        // 项目桶可能落在别的盘（`projects/` 是根下的，但调用方可能传相对/另外拼的路径），
        // 所以两棵树都比一次：根 + 所有已知桶的父层。
        is_under(abs, &self.projects_dir())
    }
}

/// 根判定结论（`Paths::root_verdict` 的唯一产出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootVerdict {
    Ok,
    /// 根不可写：Program Files 的 ACL 拒绝，或只读介质。
    /// 实测（2026-09-24）：这种根下 WebView2 直接 `os error 5` —— 没有"降级可跑"这条路。
    NotWritable,
    /// 根在系统临时目录里（从 zip 里直接双击）。
    /// 实测：功能完全正常，但那个位置会被系统清理 ⇒ 产品决定：必须解压后用。
    TempDir,
}

impl RootVerdict {
    /// 弹框文案（标题，正文）。`Ok` → `None`。
    ///
    /// **文案与判定放在一起**：判定改了文案不改就会撒谎，而这两句是用户唯一能看到的东西。
    /// 这里只产出文本（原生 `MessageBoxW` 由主程序弹），所以可以单测。
    pub fn message(self, root: &Path) -> Option<(String, String)> {
        match self {
            RootVerdict::Ok => None,
            RootVerdict::NotWritable => Some((
                "ruyix —— 绿色软件无法安装在系统级目录".to_string(),
                format!(
                    "这个目录不可写：\n{}\n\n\
                     ruyix 是绿色软件：程序自己所在的那个文件夹就是它的家，\n\
                     配置、项目状态、浏览器缓存都写在里面。\n\
                     系统级目录（Program Files）或只读介质上写不进去，所以不能在这里运行。\n\n\
                     请把整个文件夹放到可写位置（例如 D:\\ruyix）后再运行；\n\
                     或用环境变量 RUYIX_HOME 指定一个可写目录（命令行：set RUYIX_HOME=D:\\ruyix-data）。",
                    root.display()
                ),
            )),
            RootVerdict::TempDir => Some((
                "ruyix —— 必须解压后试用".to_string(),
                format!(
                    "当前运行位置是系统临时目录：\n{}\n\n\
                     直接从压缩包里双击运行时，Windows 会把程序解到临时目录再运行 ——\n\
                     那个位置会被系统清理（磁盘清理 / 存储感知），配置、会话和项目状态都会丢。\n\n\
                     请把压缩包解压到固定位置（例如 D:\\ruyix）后再运行 ruyix.exe。",
                    root.display()
                ),
            )),
        }
    }
}

/// 一个项目桶的清单项（孤儿面板用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BucketInfo {
    /// 目录名（= 项目路径的 slug）
    pub key: String,
    pub path: String,
    /// 递归体积（字节）
    pub bytes: u64,
    /// 桶内自证文件里的原始项目路径（老桶可能没有）
    pub project_path: Option<String>,
    /// 那个项目路径**现在还在不在**（不在 = 项目被移动/删除了）
    pub exists: bool,
}

/// 目录递归体积。读不了的项按 0 算 —— 清单只是给人看的，不该因为一个坏符号链接就整块失败。
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            total += dir_bytes(&e.path());
        } else {
            total += md.len();
        }
    }
    total
}

/// 试写探针：在 `dir` 里建一个临时文件再删掉。
///
/// **不看权限位**：Windows 上目录的"只读"属性对文件没有约束力，而 Program Files 那种
/// 拒绝是 **ACL**（实测 `PermissionError`）—— 只有真写一次才知道。一次探针同时覆盖
/// "ACL 不可写"与"只读介质"两种情况。
pub fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// `child` 是不是在 `parent` 之下（**按路径成分**，不是字符串前缀）。
///
/// 三条实测过的坑：① Windows 大小写不敏感（`C:\Temp` 与 `c:\temp` 是同一个）；
/// ② `canonicalize` 出来的 `\\?\` 前缀要剥掉才能比；③ 尾部反斜杠不能把 `C:\ab` 判成
/// `C:\a` 的儿子（所以比的时候两边都补一个分隔符）。
pub fn is_under(child: &Path, parent: &Path) -> bool {
    let c = norm_for_compare(child);
    let p = norm_for_compare(parent);
    if p.is_empty() {
        return false;
    }
    c == p || c.starts_with(&format!("{p}\\"))
}

/// 归一化到可比较的字符串：剥 `\\?\`、分隔符统一 `\`、转小写、去尾部分隔符。
fn norm_for_compare(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    s.trim_end_matches('\\').to_ascii_lowercase()
}

/// 首启创建的说明文件（`路径，内容`）。**幂等**：已存在就不动。
///
/// 文本放在 `src-tauri/templates/` 里、用 `include_str!` 读进来：打包脚本拷的是**同一份**
/// 文件，于是"预置目录里的说明"与"程序首启写的说明"不可能各说各话（两处各写一份文案，
/// 迟早会漂移）。
const TEMPLATE_FILES: &[(&str, &str)] = &[
    (
        "global/README.md",
        include_str!("../templates/global-README.md"),
    ),
    (
        "projects/README.md",
        include_str!("../templates/projects-README.md"),
    ),
    (
        "plugins/README.md",
        include_str!("../templates/plugins-README.md"),
    ),
];

// ============================================
// 进程级当前根
// ============================================

static CURRENT: OnceLock<Paths> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// 测试注入的根（线程局部：`cargo test` 每个测试一个线程，互不干扰）。
    /// 测试要的是**隔离**，不是"改全局状态" —— 动 env 或全局变量会让并行测试互相污染。
    static TEST_ROOT: std::cell::RefCell<Option<Paths>> = const { std::cell::RefCell::new(None) };
}

/// exe 所在目录（`current_exe` 失败时退化为 `.`，与"绝不猜"相比这里必须有个值）
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 启动最早期调一次：按 §根判定 定根并记住。
#[allow(dead_code)] // 见 Paths 的说明：P2 起会有更多调用点，先摆全
pub fn init_auto() -> Paths {
    let p = Paths::discover(&exe_dir());
    let _ = CURRENT.set(p.clone());
    p
}

/// 当前根。未初始化时按 exe 目录惰性判定 —— **不 panic**：
/// 一个路径解析点不该让整个程序起不来。
pub fn current() -> Paths {
    #[cfg(test)]
    if let Some(p) = TEST_ROOT.with(|c| c.borrow().clone()) {
        return p;
    }
    CURRENT.get_or_init(|| Paths::discover(&exe_dir())).clone()
}

#[cfg(test)]
pub fn set_test_root(root: impl Into<PathBuf>) {
    TEST_ROOT.with(|c| *c.borrow_mut() = Some(Paths::from_root(root)));
}

// ============================================
// 项目 key
// ============================================

/// 目录名长度上限（Windows 单段 255 UTF-16，留足余量；路径总长还有 260 的老限制）
const KEY_MAX: usize = 64;
/// 截断后保留的字符数
const KEY_TRUNC: usize = 48;
/// Windows 保留设备名（做目录名非法，且 `con/aux/...` 在资源管理器里根本打不开）
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 项目路径 → 目录名（可读、可排查、稳定）。
///
/// - 存在的路径先 `canonicalize`（拿到磁盘上的真实大小写，顺带消掉 `..`/短路径）
/// - 分隔符 → 单个 `-`；盘符冒号丢掉：`D:\Projects\Rust\ruyix` → `D-Projects-Rust-ruyix`
/// - 中文等 Unicode 字母数字**保留**（换成 `_` 会让"项目/我的项目"这类路径撞成同一个 key）
/// - 非法的 Windows 字符 `<>:"/\|?*` 与控制字符 → `_`
/// - 太长则截前 48 字符 + `-<8 位 hash>`；保留名（`con` 等）追加同样后缀
pub fn project_key(project: &str) -> String {
    let raw = project.trim();
    let canon = match std::fs::canonicalize(raw) {
        // canonicalize 在 Windows 上带 `\\?\` 前缀，剥掉（与 main.rs 的 clean_path 同一口径）
        Ok(p) => crate::clean_path(&p),
        // 不存在（还没建？已删？）→ 原样用，自己归一
        Err(_) => raw.to_string(),
    };

    let mut slug = String::with_capacity(canon.len());
    for ch in canon.chars() {
        match ch {
            '\\' | '/' => slug.push('-'),
            // 盘符冒号**直接丢掉**（`D:\Projects` → `D-Projects`）；其余非法字符换 `_`
            ':' => {}
            '*' | '?' | '"' | '<' | '>' | '|' => slug.push('_'),
            c if c.is_control() => slug.push('_'),
            c if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') => slug.push(c),
            _ => slug.push('_'),
        }
    }
    // 首尾的 `-`/`.` 让名字难看且 Windows 不喜欢结尾的点
    let trimmed = slug.trim_matches(|c| c == '-' || c == '.').to_string();
    let mut key = if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed
    };

    let need_hash =
        key.chars().count() > KEY_MAX || RESERVED.contains(&key.to_ascii_lowercase().as_str());
    if need_hash {
        let digest = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(canon.to_ascii_lowercase().as_bytes());
            let d = h.finalize();
            d[..4]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let head: String = key.chars().take(KEY_TRUNC).collect();
        key = format!("{}-{digest}", head.trim_end_matches('-'));
    }
    key
}

// ============================================
// 测试
// ============================================

#[cfg(test)]
mod tests {
    use super::*;

    fn p(root: &str) -> Paths {
        Paths::from_root(root)
    }

    #[test]
    fn discover_prefers_ruyix_home_then_exe_dir() {
        let exe = Path::new(r"D:\Tools\ruyix");
        // ① 没设 → exe 目录
        assert_eq!(Paths::discover_with(exe, None).root(), exe);
        // ② 设了但空/全空格 → 仍按 exe 目录（"设了个空值"不该把家搬到别处）
        assert_eq!(Paths::discover_with(exe, Some("".into())).root(), exe);
        assert_eq!(Paths::discover_with(exe, Some("   ".into())).root(), exe);
        // ③ 设了 → 用它
        assert_eq!(
            Paths::discover_with(exe, Some(r"D:\PortableRuyix".into())).root(),
            Path::new(r"D:\PortableRuyix")
        );
    }

    #[test]
    fn layout_is_exe_dir_plus_three_dirs() {
        let x = p(r"D:\ruyix");
        assert_eq!(x.global_dir(), Path::new(r"D:\ruyix").join("global"));
        assert_eq!(x.projects_dir(), Path::new(r"D:\ruyix").join("projects"));
        assert_eq!(x.plugins_dir(), Path::new(r"D:\ruyix").join("plugins"));
        assert_eq!(
            x.runs_root(),
            Path::new(r"D:\ruyix").join("global").join("runs")
        );
        assert_eq!(
            x.debug_log(),
            Path::new(r"D:\ruyix")
                .join("global")
                .join("logs")
                .join("debug.log")
        );
        assert_eq!(
            x.webview_data(),
            Path::new(r"D:\ruyix").join("global").join("webview")
        );
        assert_eq!(
            x.cache_dir(),
            Path::new(r"D:\ruyix").join("global").join("cache")
        );
    }

    /// 互斥体名带根指纹：同一根同名、不同根不同名（两份副本并存的前提，P0 实测过）
    #[test]
    fn root_fingerprint_is_16_hex_and_differs_per_root() {
        let a = Paths::from_root(r"D:\ruyix").root_fingerprint();
        let b = Paths::from_root(r"D:\ruyix-copy").root_fingerprint();
        assert_eq!(a.len(), 16, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, b, "两份副本必须得到不同的互斥体名");
        // 大小写 / 分隔符归一后同一个根 → 同一个名字（第二次启动仍被识别为同一份副本）
        assert_eq!(a, Paths::from_root("d:/Ruyix/").root_fingerprint());
    }

    #[test]
    fn project_dir_hangs_under_projects_and_takes_a_path_not_a_key() {
        let x = p(r"D:\ruyix");
        let d = x.project_dir(r"D:\Projects\Rust\ruyix");
        assert_eq!(
            d,
            Path::new(r"D:\ruyix")
                .join("projects")
                .join("D-Projects-Rust-ruyix")
        );
        assert_eq!(
            x.project_bucket(r"D:\Projects\Rust\ruyix", "stage"),
            d.join("stage")
        );
    }

    #[test]
    fn project_key_is_readable_and_separator_insensitive() {
        let want = "D-Projects-Rust-ruyix";
        assert_eq!(project_key(r"D:\Projects\Rust\ruyix"), want);
        assert_eq!(
            project_key(r"D:\Projects\Rust\ruyix\"),
            want,
            "尾部反斜杠应归一"
        );
        assert_eq!(project_key("D:/Projects/Rust/ruyix"), want, "正斜杠应归一");
        assert_eq!(
            project_key("  D:\\\\Projects\\\\Rust\\\\ruyix  "),
            want,
            "首尾空白应归一"
        );
    }

    #[test]
    fn project_key_keeps_unicode_so_chinese_projects_do_not_collide() {
        let a = project_key(r"D:\项目\我的项目");
        let b = project_key(r"D:\项目\另一个项目");
        assert_ne!(a, b, "不同中文项目不能撞成同一个 key");
        assert!(a.contains("我的项目"), "中文应保留：{a}");
        assert!(!a.contains('\\'), "不该残留分隔符：{a}");
    }

    #[test]
    fn project_key_handles_unc_and_illegal_chars() {
        let k = project_key(r"\\server\share\proj:with*illegal?chars");
        assert!(
            !k.contains('\\') && !k.contains(':') && !k.contains('*') && !k.contains('?'),
            "非法字符不该残留：{k}"
        );
        assert!(
            k.contains("server") && k.contains("share") && k.contains("proj"),
            "可读部分应保留：{k}"
        );
        // 同一台共享机上不同项目不能撞（`\\` 归一并被首尾裁剪后仍要可区分）
        assert_ne!(k, project_key(r"\\server\share\other-proj"), "{k}");
    }

    #[test]
    fn project_key_truncates_long_paths_with_a_stable_hash() {
        let deep = format!(r"D:\{}", "very-long-folder-name\\".repeat(12));
        let k1 = project_key(&deep);
        let k2 = project_key(&deep);
        assert_eq!(k1, k2, "同一路径必须稳定");
        assert!(
            k1.chars().count() <= KEY_MAX,
            "长度应封顶：{}",
            k1.chars().count()
        );
        assert!(k1.contains('-'), "截断后的形状应带 hash 分隔：{k1}");
        // 前缀相同的两条长路径不能撞
        let other = format!(r"D:\{}", "very-long-folder-name\\".repeat(12)) + "x";
        assert_ne!(k1, project_key(&other));
    }

    #[test]
    fn project_key_escapes_windows_reserved_names() {
        for name in ["con", "CON", "aux", "nul", "com1", "LPT9"] {
            let key = project_key(&format!(r"D:\{name}"));
            // 形如 `D-con-xxxxxxxx`：不再是裸保留名，且带上 hash 后缀
            assert_ne!(key.to_ascii_lowercase(), name, "{name} 必须被改写");
            assert!(key.len() > name.len(), "{name} 应追加后缀：{key}");
        }
    }

    #[test]
    fn current_is_injectable_per_thread() {
        set_test_root(r"D:\tmp-root");
        assert_eq!(current().root(), Path::new(r"D:\tmp-root"));
        assert_eq!(
            current().global_dir(),
            Path::new(r"D:\tmp-root").join("global")
        );
    }

    // ---- P2：边界判定与目录模板 ----

    fn temp_probe(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ruyix-p2-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn is_under_compares_components_not_string_prefix() {
        assert!(is_under(Path::new(r"C:\a\b"), Path::new(r"C:\a")));
        assert!(
            is_under(Path::new(r"C:\a"), Path::new(r"C:\a")),
            "自己是自己的子层"
        );
        // ① 成分边界：`C:\ab` 不是 `C:\a` 的儿子（字符串前缀比较会误判）
        assert!(!is_under(Path::new(r"C:\ab"), Path::new(r"C:\a")));
        // ② 大小写不敏感（Windows）
        assert!(is_under(Path::new(r"c:\TEMP\x"), Path::new(r"C:\temp")));
        // ③ 分隔符与 `\\?\` 前缀
        assert!(is_under(Path::new(r"\\?\C:\tmp\a\b"), Path::new("C:/tmp/")));
        // ④ 空父层不放行（否则"谁都在家里"）
        assert!(!is_under(Path::new(r"C:\a"), Path::new("")));
    }

    /// 临时根 → `TempDir`；用户显式 `RUYIX_HOME` 指到同一个位置 → `Ok`
    /// （他的选择，不是 Windows 悄悄解压出来的）
    #[test]
    fn root_verdict_flags_temp_root_unless_explicitly_chosen() {
        let t = temp_probe("verdict");
        assert_eq!(Paths::from_root(&t).root_verdict(), RootVerdict::TempDir);
        let explicit = Paths::discover_with(Path::new(r"D:\ruyix"), Some(t.clone().into()));
        assert!(explicit.is_explicit());
        assert_eq!(explicit.root_verdict(), RootVerdict::Ok);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn is_writable_tells_a_real_dir_from_a_missing_one() {
        let t = temp_probe("writable");
        assert!(is_writable(&t), "临时目录应可写");
        // 不存在的目录 → 写不进去（这条比 C:\Program Files 稳：不吃提权状态）
        assert!(!is_writable(&t.join("no-such-subdir").join("deeper")));
        // 探针文件不留痕
        let leftovers: Vec<_> = std::fs::read_dir(&t)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "试写探针不该留文件：{leftovers:?}");
        let _ = std::fs::remove_dir_all(&t);
    }

    /// 幂等 + **绝不覆盖**：跑两遍只建一次，用户改过的 README 不被改回去
    #[test]
    fn ensure_layout_is_idempotent_and_never_overwrites() {
        let t = temp_probe("layout");
        let p = Paths::from_root(&t);
        let first = p.ensure_layout().unwrap();
        assert!(first.iter().any(|s| s.contains("global")), "{first:?}");
        for rel in ["global", "projects", "plugins"] {
            assert!(t.join(rel).is_dir(), "{rel} 应被建出来");
        }
        assert!(t.join("global/logs").is_dir());
        assert!(t.join("global/webview").is_dir());
        assert!(t.join("global/runs").is_dir());
        assert!(t.join("global/README.md").is_file());

        // 用户改过的 README：第二遍不许动它
        std::fs::write(t.join("projects/README.md"), "我的笔记").unwrap();
        let second = p.ensure_layout().unwrap();
        assert!(second.is_empty(), "第二遍不该再建东西：{second:?}");
        assert_eq!(
            std::fs::read_to_string(t.join("projects/README.md")).unwrap(),
            "我的笔记",
            "幂等不等于覆盖"
        );
        let _ = std::fs::remove_dir_all(&t);
    }

    /// 弹框文案必须带着用户指定的那两句话（判定与文案分开写就会撒谎）
    #[test]
    fn verdict_messages_carry_the_agreed_wording() {
        assert!(RootVerdict::Ok.message(Path::new("x")).is_none());
        let (t1, b1) = RootVerdict::NotWritable
            .message(Path::new(r"C:\Program Files\ruyix"))
            .unwrap();
        assert!(t1.contains("绿色软件无法安装在系统级目录"), "{t1}");
        assert!(b1.contains("RUYIX_HOME"), "要给出逃生口：{b1}");
        let (t2, b2) = RootVerdict::TempDir
            .message(Path::new(r"C:\Temp\Temp1_ruyix.zip"))
            .unwrap();
        assert!(t2.contains("必须解压后试用"), "{t2}");
        assert!(b2.contains("解压到固定位置"), "{b2}");
    }

    /// "是不是我们的家"：根与项目桶都算，别处不算
    #[test]
    fn is_inside_state_covers_root_and_buckets() {
        let p = Paths::from_root(r"D:\ruyix");
        assert!(p.is_inside_state(Path::new(r"D:\ruyix\global\ai.toml")));
        assert!(p.is_inside_state(Path::new(r"d:\Ruyix\projects\X\stage\a.txt")));
        assert!(!p.is_inside_state(Path::new(r"D:\Projects\Rust\ruyix\src\main.rs")));
        assert!(!p.is_inside_state(Path::new(r"D:\ruyix-other\global")));
    }

    /// 桶清单与自证：自证文件里的原始路径要能读回来，删桶要挡路径逃逸
    #[test]
    fn buckets_are_self_describing_and_deletion_is_guarded() {
        let t = temp_probe("buckets");
        let p = Paths::from_root(&t);
        // 用一条**不存在**的路径：`exists` 的语义是"那个项目现在还在磁盘上吗"
        let fake = t.join("nope").join("my-proj");
        let fake_s = fake.to_string_lossy().to_string();
        p.stamp_project(&fake_s).unwrap();

        let list = p.list_buckets();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(
            list[0].key,
            p.project_key(&fake_s),
            "清单用的 key 规则要与 project_dir 同一套"
        );
        assert_eq!(
            list[0].project_path.as_deref(),
            Some(fake_s.as_str()),
            "TOML 里的反斜杠要能原样读回来（Windows 路径必须双写才不炸）"
        );
        assert!(list[0].bytes > 0, "自证文件本身也算体积：{}", list[0].bytes);
        assert!(!list[0].exists, "路径不存在时 exists 应为 false");

        // 删桶的路径逃逸守卫（一个"删除"入口不值得赌调用方传对）
        for bad in ["..", "a/b", "a\\b", "C:evil", ""] {
            assert!(p.delete_bucket(bad).is_err(), "{bad} 必须被拒");
        }
        let key = p.project_key(&fake_s);
        assert!(p.delete_bucket(&key).is_ok());
        assert!(!t.join("projects").join(&key).exists());
        let _ = std::fs::remove_dir_all(&t);
    }

    /// 模板文本与打包脚本用的是同一份文件（`include_str!`）——
    /// 这条钉住"两处各写一份文案"的漂移：模板存在且非空、且覆盖三处根目录。
    ///
    /// 计数**不写死**：原来写死 3，加一份 GPU 插件说明就红 —— 那种红只说明测试老了，
    /// 不说明有 bug。"新加了模板文件却忘了登记"由 U66（对照 `templates/` 目录列举）与
    /// 打包脚本负责抓，这里只管每份的质量与覆盖面。
    #[test]
    fn template_files_are_embedded_not_duplicated() {
        assert!(TEMPLATE_FILES.len() >= 3);
        for (rel, body) in TEMPLATE_FILES {
            assert!(rel.ends_with("README.md"), "{rel}");
            assert!(body.len() > 80, "{rel} 的模板文本太短：{} 字符", body.len());
            assert!(body.starts_with('#'), "{rel} 应当是 markdown：{body:.20}");
        }
        for want in [
            "global/README.md",
            "projects/README.md",
            "plugins/README.md",
        ] {
            assert!(
                TEMPLATE_FILES.iter().any(|(r, _)| *r == want),
                "模板路径要覆盖 global / projects / plugins，缺 {want}"
            );
        }
    }

    // ---- 静态门禁：路径只许在这里拼 ----

    /// P3 的存量债（v1.0.0）：**已清零** —— 项目侧那七类写入（暂存 / 备份 / 进程日志 /
    /// 环境留痕 / 会话 / 验证产物 / 提示词文案）全部搬到 `<根>/projects/<项目 key>/`。
    ///
    /// 空表不是装饰：它是**棘轮** —— 清单外的任何一处 `.ruyix` 都会让门禁报红，
    /// 而"要不要留在表里"必须是一次有意识的决定。
    const P3_DEBT: &[(&str, usize)] = &[];

    /// 不是债、**按设计保留**的（P3 之后也该留着）：
    /// - `workspace.rs`：清**沙箱复制品**里的 `.ruyix`（那是复制品，不是用户仓库）
    /// - `kb/local.rs`：知识库扫描跳过 `.ruyix`（IDE 自己的命名空间不进知识库）
    /// - `verify.rs`（1 处，`SANDBOX_STATE` 那个常量）：容器内跑时的产物相对路径 ——
    ///   项目挂在 `/work`，相对路径落在**容器内的那份复制品**里，本来就不碰宿主盘
    ///   （宿主上跑时用的是绝对路径，见 `VerifyDirs`）
    const BY_DESIGN: &[(&str, usize)] = &[
        ("workspace.rs", 1),
        ("kb/local.rs", 1),
        ("verify.rs", 1),
        // 场景测试自己：它要断言"用户项目里不许出现 `.ruyix`"，就必然要写出这个名字。
        // 这不是写入点，是**判据本身**。
        ("agent/repo_clean_tests.rs", 1),
    ];

    fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk_rs(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push(path);
            }
        }
    }

    /// A6：`dirs::`（找系统位置）与 `.darkhorse`（老血统）在全仓只许出现在本文件；
    /// A7：`.ruyix` 只许出现在**按设计保留**的那几处（沙箱复制品 / 知识库跳过 / 容器内路径）。
    #[test]
    fn no_global_path_construction_outside_this_file() {
        let host_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let engine_src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join("harness-engine")
            .join("src");

        let mut files = Vec::new();
        walk_rs(&host_src, &mut files);
        walk_rs(&engine_src, &mut files);
        assert!(files.len() > 30, "没扫到源码，路径不对：{}", files.len());

        let mut problems: Vec<String> = Vec::new();
        let mut debt_seen: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for f in &files {
            let rel = f
                .strip_prefix(&host_src)
                .or_else(|_| f.strip_prefix(&engine_src))
                .unwrap_or(f)
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(f).unwrap_or_default();
            // 本文件自己就是那个"唯一解析点"，豁免
            let is_self = f.ends_with("paths.rs");
            if is_self {
                continue; // 本文件就是那个"唯一解析点"，豁免（它自己也提到这些字符串）
            }

            for (n, line) in text.lines().enumerate() {
                let l = format!("{}:{}", rel, n + 1);
                // 只看**代码**：注释里提历史/老路径不算 —— "融合前引擎读 `%APPDATA%\darkhorse-harness`"
                // 这类说明留着比删掉有价值，而注释不构建任何路径。
                let code = match line.find("//") {
                    Some(i) => &line[..i],
                    None => line,
                };
                if code.contains("dirs::") || code.contains(".darkhorse") {
                    problems.push(format!("{l}  {}", line.trim()));
                }
                if code.contains(".ruyix") {
                    match P3_DEBT
                        .iter()
                        .chain(BY_DESIGN.iter())
                        .find(|(name, _)| *name == rel)
                    {
                        Some((name, _)) => *debt_seen.entry(name.to_string()).or_insert(0) += 1,
                        None => {
                            problems.push(format!("{l}  .ruyix 不在清单里：{}", line.trim()));
                        }
                    }
                }
            }
        }

        assert!(
            problems.is_empty(),
            "路径必须在 paths.rs 里拼（v1.0.0 P1 纪律）：\n{}",
            problems.join("\n")
        );

        for (name, limit) in P3_DEBT.iter().chain(BY_DESIGN.iter()) {
            let got = debt_seen.get(*name).copied().unwrap_or(0);
            assert!(
                got <= *limit,
                "P3 存量只许减不许增：{name} 现在 {got} 处，上限 {limit} 处 —— \
                 多出来的那处应该改成 paths::current().project_bucket(..)"
            );
        }
    }
}
