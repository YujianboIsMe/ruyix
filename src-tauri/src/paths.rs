//! 便携根：**全局路径的唯一解析点**（v1.0.0 P1）。
//!
//! 三条规矩，改动时别绕过：
//!
//! 1. **根判定只有一处实现**：`RUYIX_HOME`（非空）→ 否则 **exe 所在目录**。
//!    `exe 同目录 = 程序的家`，卸载 = 删这个目录（零残留）。
//! 2. **除本文件外，任何地方不许再拼 `~/.ruyix` / `.darkhorse` / `dirs::`** ——
//!    由 `no_global_path_construction_outside_this_file` 静态守住（P3 的 `.ruyix` 存量
//!    用 `P3_DEBT` 棘轮：只许减，不许增）。
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
}

#[allow(dead_code)]
impl Paths {
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
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
            Some(v) if !v.to_string_lossy().trim().is_empty() => Self::from_root(PathBuf::from(v)),
            _ => Self::from_root(exe_dir),
        }
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

    pub fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }

    /// 项目状态桶：`<root>/projects/<key>`。
    ///
    /// **参数是项目路径，不是 key** —— key 由内部算。这样调用方没有"把路径当 key 用"
    /// 的机会（那种错会静默建出一个怪目录，而不是报错）。
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
}

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
            project_key("  D:\\Projects\\Rust\\ruyix  "),
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

    // ---- 静态门禁：路径只许在这里拼 ----

    /// P3 的存量债：项目侧那七类写入还在拼 `<项目>/.ruyix/...`，P3 搬到 `projects/<key>/`。
    /// 这张表是**棘轮**：数量只许减不许增（新增一处就红）。
    const P3_DEBT: &[(&str, usize)] = &[
        // 宿主：项目侧状态写入（P3 搬到 `<根>/projects/<key>/`）
        ("agent/apply.rs", 1),
        ("agent/env_setup.rs", 2),
        ("agent/sessions.rs", 1),
        ("agent/stage.rs", 6),
        // 引擎：暂存 / 进程日志 / 验证产物
        ("agent.rs", 5),
        ("proc.rs", 1),
        ("verify.rs", 3),
    ];

    /// 不是债、**按设计保留**的（P3 之后也该留着）：
    /// - `workspace.rs`：清**沙箱复制品**里的 `.ruyix`（那是复制品，不是用户仓库）
    /// - `kb/local.rs`：知识库扫描跳过 `.ruyix`（IDE 自己的命名空间不进知识库）
    const BY_DESIGN: &[(&str, usize)] = &[("workspace.rs", 1), ("kb/local.rs", 1)];

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
    /// A7：`.ruyix` 只许出现在 P3 的存量清单里，且数量不增。
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
