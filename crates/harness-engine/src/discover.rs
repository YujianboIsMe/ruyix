//! 命令发现：这台机器 + 这个项目，到底有哪些命令**真的能用**。
//!
//! ## 为什么要有这一层
//!
//! 实测 run `agent-20260920-152312`（cloud-shop 修一个 Maven 依赖）跑了 65 轮，其中
//! 5~6 轮纯粹在试探 `mvn` / `java` 在不在（`dir /s /b %USERPROFILE%\.m2\...`、
//! `jar tf`、`type pom.xml`、`python -c "import zipfile"`）。引擎其实**早就探过**这台机器
//! （`exec::probe`），但结果只喂给了设置页的环境检查，模型一个字节都看不到 —— 于是它只能
//! 自己试。
//!
//! **事实不告诉模型，模型就会花轮次去试。** 这一层只做一件事：把"有什么、没有什么"
//! 实测出来，放进模型的上下文。
//!
//! ## 为什么不是"每个语言一个探测分支"
//!
//! 工具表是**一张** `const` 表（[`TOOLS`]），加一个工具是加一行。探测按**工程标记文件 /
//! 扩展名**筛出与本项目相关的子集（`pom.xml` 在 → 探 `mvn` 与 `java`），再叠上常驻项
//! （`markers` 为空者，如 `git`）；用户还能用 `[discover] extra` 追加自己的。表项一多，
//! 长的仍然是**数据**，不是分支。
//!
//! ## 平台现实（这一步不做，Windows 上等于没做）
//!
//! 本机实测（`target/probe_cmd.rs`，Maven 装在 `D:\Tools\Maven\...\bin\mvn.cmd`）：
//!
//! | 做法 | 结果 |
//! |---|---|
//! | `Command::new("mvn")` | `program not found` —— `CreateProcess` 不认 PATHEXT |
//! | `Command::new("java")` | 正常（`java.exe`） |
//! | `where mvn` | `D:\Tools\Maven\apache-maven-3.9.16\bin\mvn` |
//! | `cmd /C mvn --version` | `Apache Maven 3.9.16` |
//! | `cmd /C zzz_no_such_zzz` | 退出码 **1**（不是 9009） |
//!
//! 三条结论决定了实现：① 直接 `Command::new(bin)` 对 `.cmd` / `.bat` / `.ps1` 工具
//! **天然失明**；② "在不在"只能由 `where` / `command -v` 的退出码回答；③ 版本命令必须
//! **过 shell** 跑，且不能拿退出码当可用性判据。所以探测固定是**两步**：解析路径 →
//! 过 shell 取版本。这两步落在 [`exec::resolve_bin`] / [`exec::bin_version`]，
//! `exec::probe`（宿主的环境面板）与本模块**共用同一份** —— 两套探测必然会在某个平台上
//! 分叉：一处认得 `mvn.cmd`，一处不认得。

use crate::config::DiscoverConfig;
use crate::exec;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 一个候选工具。
pub struct ToolSpec {
    /// 展示名（提示词里那个词）
    pub name: &'static str,
    /// 默认二进制名（`python` / `node` 会被配置里的路径覆盖）
    pub bin: &'static str,
    /// 取版本的参数
    pub version_args: &'static [&'static str],
    /// 命中即视为"本项目相关"的根目录标记文件；**空 = 常驻探测**
    pub markers: &'static [&'static str],
    /// 命中即视为相关的根目录扩展名（含点，如 `.java`）
    pub extensions: &'static [&'static str],
}

/// 候选工具表。**加一个工具 = 加一行**，不要另起分支。
///
/// `markers` 只看项目根：多模块工程的聚合 `pom.xml` 在根上，够用；递归扫描会把
/// `target/`、`node_modules/` 里别人的标记也算进来，反而失真。
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "git",
        bin: "git",
        version_args: &["--version"],
        markers: &[],
        extensions: &[],
    },
    ToolSpec {
        name: "java",
        bin: "java",
        version_args: &["-version"],
        markers: &[
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "gradlew",
        ],
        extensions: &[".java", ".kt", ".gradle"],
    },
    ToolSpec {
        name: "mvn",
        bin: "mvn",
        version_args: &["--version"],
        markers: &["pom.xml"],
        extensions: &[],
    },
    ToolSpec {
        name: "gradle",
        bin: "gradle",
        version_args: &["--version"],
        markers: &[
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "gradlew",
        ],
        extensions: &[".gradle"],
    },
    ToolSpec {
        name: "node",
        bin: "node",
        version_args: &["--version"],
        markers: &["package.json"],
        extensions: &[".js", ".mjs", ".cjs", ".ts", ".tsx"],
    },
    ToolSpec {
        name: "npm",
        bin: "npm",
        version_args: &["--version"],
        markers: &["package.json"],
        extensions: &[],
    },
    ToolSpec {
        name: "python",
        bin: "python",
        version_args: &["--version"],
        markers: &["pyproject.toml", "setup.py", "requirements.txt", "Pipfile"],
        extensions: &[".py"],
    },
    ToolSpec {
        name: "cargo",
        bin: "cargo",
        version_args: &["--version"],
        markers: &["Cargo.toml"],
        extensions: &[".rs"],
    },
    ToolSpec {
        name: "go",
        bin: "go",
        version_args: &["version"],
        markers: &["go.mod"],
        extensions: &[".go"],
    },
    ToolSpec {
        name: "make",
        bin: "make",
        version_args: &["--version"],
        markers: &["Makefile", "makefile", "GNUmakefile"],
        extensions: &[],
    },
    ToolSpec {
        name: "docker",
        bin: "docker",
        version_args: &["--version"],
        markers: &["Dockerfile", "docker-compose.yml", "compose.yml"],
        extensions: &[],
    },
];

/// 一个工具的探测结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    /// 实际探的二进制名（`python` / `node` 会是配置里的路径）
    pub bin: String,
    pub available: bool,
    /// 版本首行（拿不到就空串，**不代表不可用**）
    pub version: String,
    /// 解析到的路径，可核对
    pub path: String,
}

/// 探测结果缓存。**key 是二进制名**（工具是机器全局的，与项目无关），
/// "本项目相关"那部分每次现算 —— 那只是一次根目录读，比进程探测便宜得多。
fn cache() -> &'static Mutex<HashMap<String, (Instant, Found)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, Found)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 清空探测缓存（换工具链、装完东西、单测隔离用）。
pub fn invalidate_cache() {
    if let Ok(mut g) = cache().lock() {
        g.clear();
    }
    if let Ok(mut g) = avail_cache().lock() {
        g.clear();
    }
}

/// "这个二进制在不在"的缓存。与版本缓存分开存：**存在性判定不取版本**。
fn avail_cache() -> &'static Mutex<HashMap<String, (Instant, bool)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, bool)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 存在性判定的超时 / 缓存 TTL。只调一次 `where`（几十毫秒），可以给得宽松些。
const AVAIL_TIMEOUT: Duration = Duration::from_secs(10);
const AVAIL_TTL: Duration = Duration::from_secs(300);

/// 本机有没有这个命令（`where` / `command -v`，进程内缓存）。
///
/// 与 [`probe_bin`] 的区别：**不取版本**。执行前闸门只要一个布尔值 ——
/// 为它起一次 JVM 去问 `mvn -v`（~1s）毫无意义。
pub fn is_available(bin: &str) -> bool {
    if let Ok(g) = avail_cache().lock()
        && let Some((at, ok)) = g.get(bin)
        && at.elapsed() < AVAIL_TTL
    {
        return *ok;
    }
    let ok = exec::resolve_bin(bin, AVAIL_TIMEOUT).is_some();
    if let Ok(mut g) = avail_cache().lock() {
        g.insert(bin.to_string(), (Instant::now(), ok));
    }
    ok
}

/// 闸门拒绝一条命令时附的"那有什么"：本项目该有、且本机确实有的命令名。
///
/// 只用内置工具表按项目标记筛一次（不跑版本探测）。`python` / `node` 用表里的默认名，
/// 不做配置替换 —— 这是**错误提示里的参考清单**，不是给模型下结论的事实来源
/// （后者是 [`discover`] + [`render_note`]）。
pub fn available_names(root: &Path) -> Vec<String> {
    plan(root, &[], "python", "node")
        .into_iter()
        .filter(|j| is_available(&j.bin))
        .map(|j| j.name)
        .collect()
}

/// 待探的一项。
struct Job {
    name: String,
    bin: String,
    version_args: &'static [&'static str],
}

/// 探测本项目相关的那批命令。缓存未过期就不重新起进程。
pub fn discover(root: &Path, cfg: &DiscoverConfig, python_bin: &str, node_bin: &str) -> Vec<Found> {
    if !cfg.enabled {
        return Vec::new();
    }
    let ttl = Duration::from_secs(cfg.ttl_secs);
    let timeout = Duration::from_secs(cfg.timeout_secs.max(1));
    let jobs = plan(root, &cfg.extra, python_bin, node_bin);

    // **并发**探测：JVM 工具（java / mvn）各自要 ~1s 启动，串起来冷探测实测 2.7s
    // （cloud-shop 三项）。并发后等于最慢的那一个，冷路径少等一秒多。
    // 缓存命中时这里几乎零成本（实测 ~100µs）。
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|j| scope.spawn(|| probe_cached(j, ttl, timeout)))
            .collect();
        handles
            .into_iter()
            .zip(jobs.iter())
            .map(|(h, j)| {
                h.join().unwrap_or_else(|_| Found {
                    name: j.name.clone(),
                    bin: j.bin.clone(),
                    available: false,
                    version: String::new(),
                    path: String::new(),
                })
            })
            .collect()
    })
}

/// 只做筛选，不起进程（可单测）。
fn plan(root: &Path, extra: &[String], python_bin: &str, node_bin: &str) -> Vec<Job> {
    let entries = list_root(root);
    let has_marker = |m: &str| entries.contains(&m.to_ascii_lowercase());
    let has_ext = |x: &str| entries.iter().any(|e| e.ends_with(x));

    let mut jobs: Vec<Job> = Vec::new();
    for t in TOOLS {
        let relevant = t.markers.is_empty()
            || t.markers.iter().any(|m| has_marker(m))
            || t.extensions.iter().any(|x| has_ext(x));
        if !relevant {
            continue;
        }
        let bin = match t.bin {
            "python" => python_bin.to_string(),
            "node" => node_bin.to_string(),
            other => other.to_string(),
        };
        jobs.push(Job {
            name: t.name.to_string(),
            bin,
            version_args: t.version_args,
        });
    }
    // 用户追加的：表没覆盖的工具，`--version` 探一把
    for x in extra {
        let x = x.trim();
        if x.is_empty() || jobs.iter().any(|j| j.bin.eq_ignore_ascii_case(x)) {
            continue;
        }
        jobs.push(Job {
            name: x.to_string(),
            bin: x.to_string(),
            version_args: &["--version"],
        });
    }
    jobs
}

/// 根目录下一层的条目名（小写）。错误一律当"空目录"处理 —— 探测失败不该让对话失败。
fn list_root(root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
        .collect()
}

fn probe_cached(job: &Job, ttl: Duration, timeout: Duration) -> Found {
    if ttl.as_secs() > 0
        && let Ok(g) = cache().lock()
        && let Some((at, f)) = g.get(&job.bin)
        && at.elapsed() < ttl
    {
        return f.clone();
    }
    let f = probe_bin(&job.name, &job.bin, job.version_args, timeout);
    if let Ok(mut g) = cache().lock() {
        g.insert(job.bin.clone(), (Instant::now(), f.clone()));
    }
    f
}

/// 探一个二进制：**两步** —— 先解析路径（"在不在"的唯一判据），再过 shell 取版本。
///
/// 两步都借 [`exec::resolve_bin`] / [`exec::bin_version`]（宿主的环境面板用的是同一对），
/// 一处实现两处消费 —— 两套探测必然会在某个平台上分叉：一处认得 `mvn.cmd`，一处不认得。
pub fn probe_bin(name: &str, bin: &str, version_args: &[&str], timeout: Duration) -> Found {
    let Some(path) = exec::resolve_bin(bin, timeout) else {
        return Found {
            name: name.to_string(),
            bin: bin.to_string(),
            available: false,
            version: String::new(),
            path: String::new(),
        };
    };
    // 版本取不到**不代表不可用**：可用性只看 resolve_bin 那一步
    let version = exec::bin_version(bin, version_args, timeout);
    Found {
        name: name.to_string(),
        bin: bin.to_string(),
        available: true,
        version,
        path,
    }
}

/// 一次注入最多列这么多条，超了折叠成一行。
const NOTE_MAX: usize = 12;

/// 探测结果 → 一段上下文。**只由引擎生成**：模型自己猜"有没有 mvn"正是要治的病。
///
/// 可用与**不可用都要写**：只说"有 mvn"治不了空转，得同时说"没有 gradle"，
/// 否则模型仍会去试那个不存在的。
///
/// `env_connector` = 宿主有没有提供"环境准备"连接。有就让模型走 connect 请求，
/// 没有就让它如实说明 —— 提示词不虚报能力（与 `connect_note` 同一条纪律）。
pub fn render_note(tools: &[Found], env_connector: bool) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let ok: Vec<&Found> = tools.iter().filter(|t| t.available).collect();
    let miss: Vec<&Found> = tools.iter().filter(|t| !t.available).collect();

    let mut s = String::from("本机命令（引擎已实测，别再花轮次试它们在不在）：\n");
    if ok.is_empty() {
        s.push_str("- 本项目相关的命令一个都没探到\n");
    }
    for t in ok.iter().take(NOTE_MAX) {
        let mut line = format!("- {}", t.name);
        let ver = short_version(&without_name_prefix(&t.version, &t.name));
        if !ver.is_empty() {
            line.push_str(&format!(" {ver}"));
        }
        if !t.path.trim().is_empty() {
            line.push_str(&format!("  →  {}", t.path.trim()));
        }
        s.push_str(&line);
        s.push('\n');
    }
    if ok.len() > NOTE_MAX {
        s.push_str(&format!("- …还有 {} 个没列出\n", ok.len() - NOTE_MAX));
    }
    if !miss.is_empty() {
        let names: Vec<&str> = miss
            .iter()
            .take(NOTE_MAX)
            .map(|t| t.name.as_str())
            .collect();
        let tail = if env_connector {
            "确实需要就走 connect 请求宿主准备环境）"
        } else {
            "确实需要就在最终答复里说明，不要自己跑安装命令）"
        };
        s.push_str(&format!(
            "- 本机**没有**：{}（别直接跑，会白烧一轮；{tail}\n",
            names.join("、")
        ));
    }
    Some(s.trim_end().to_string())
}

/// 版本首行常常自带工具名（`git version 2.54.0...`）。提示词里前面已经写了名字，
/// 去掉重复的那一截，省 token 也更顺口。
fn without_name_prefix(version: &str, name: &str) -> String {
    let v = version.trim();
    let lower = v.to_ascii_lowercase();
    let n = name.trim().to_ascii_lowercase();
    // `lower` 与 `v` 只差 ASCII 大小写，字节偏移一致，所以这个切片落在字符边界上
    if !n.is_empty() && lower.starts_with(&n) && lower.as_bytes().get(n.len()) == Some(&b' ') {
        return v[n.len()..].trim().to_string();
    }
    v.to_string()
}

/// 版本行里的括号基本是构建哈希（`Apache Maven 3.9.16 (2bdd9fdd…)`、
/// `cargo 1.97.1 (c980f4866 …)`），对模型是纯噪声，砍掉。
fn short_version(version: &str) -> String {
    if let Some(i) = version.find('(') {
        let head = version[..i].trim();
        if !head.is_empty() {
            return head.to_string();
        }
    }
    version.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 造一个只含指定文件的临时项目根
    fn tmp_root(files: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ruyix-discover-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::create_dir_all(&dir);
        for f in files {
            let _ = std::fs::write(dir.join(f), "x");
        }
        dir
    }

    fn job_bins(jobs: &[Job]) -> Vec<String> {
        jobs.iter().map(|j| j.bin.clone()).collect()
    }

    #[test]
    fn plan_picks_tools_by_root_marker() {
        let dir = tmp_root(&["pom.xml", "README.md"]);
        let jobs = plan(&dir, &[], "python", "node");
        let bins = job_bins(&jobs);
        // Maven 工程：git 常驻 + java/mvn 由 pom.xml 命中
        assert!(bins.contains(&"git".to_string()), "{bins:?}");
        assert!(bins.contains(&"mvn".to_string()), "{bins:?}");
        assert!(bins.contains(&"java".to_string()), "{bins:?}");
        // 没命中标记的不该出现
        assert!(!bins.contains(&"cargo".to_string()), "{bins:?}");
        assert!(!bins.contains(&"npm".to_string()), "{bins:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_picks_tools_by_extension_without_a_marker() {
        // 没有 build.gradle，但有 .java 源文件 —— 扩展名也要能命中
        let dir = tmp_root(&["Main.java"]);
        let bins = job_bins(&plan(&dir, &[], "python", "node"));
        assert!(bins.contains(&"java".to_string()), "{bins:?}");
        // 没有 pom.xml，mvn 不该被拖进来
        assert!(!bins.contains(&"mvn".to_string()), "{bins:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_always_includes_resident_tools() {
        let dir = tmp_root(&["README.md"]);
        let bins = job_bins(&plan(&dir, &[], "python", "node"));
        assert_eq!(bins, vec!["git".to_string()], "{bins:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_appends_user_extra_without_duplicating() {
        let dir = tmp_root(&["pom.xml"]);
        let extra = vec!["  helm  ".to_string(), "mvn".to_string(), "".to_string()];
        let bins = job_bins(&plan(&dir, &extra, "python", "node"));
        assert!(bins.contains(&"helm".to_string()), "{bins:?}");
        assert_eq!(
            bins.iter().filter(|b| *b == "mvn").count(),
            1,
            "已有的工具不该被 extra 重复加入：{bins:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_uses_configured_python_and_node_bins() {
        let dir = tmp_root(&["pyproject.toml", "package.json"]);
        let bins = job_bins(&plan(&dir, &[], "python3.13", "node22"));
        assert!(bins.contains(&"python3.13".to_string()), "{bins:?}");
        assert!(bins.contains(&"node22".to_string()), "{bins:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_bin_reports_absent_for_a_missing_binary() {
        // 实测驱动的测试：这个名字在任何机器上都不该存在。
        // 顺带证明"解析失败 → available=false"这条链真的走通了（而不是被当成出错）。
        let f = probe_bin(
            "zzz-no-such-tool-zzz",
            "zzz-no-such-tool-zzz",
            &["--version"],
            Duration::from_secs(10),
        );
        assert!(!f.available, "{f:?}");
        assert!(f.path.is_empty(), "{f:?}");
        assert!(f.version.is_empty(), "{f:?}");
    }

    #[test]
    fn discover_is_a_noop_when_disabled() {
        let dir = tmp_root(&["pom.xml"]);
        let cfg = DiscoverConfig {
            enabled: false,
            ..DiscoverConfig::default()
        };
        assert!(discover(&dir, &cfg, "python", "node").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_note_writes_both_the_present_and_the_absent() {
        // 只说"有 mvn"治不了空转，必须同时说"没有 gradle"
        let found = vec![
            Found {
                name: "mvn".into(),
                bin: "mvn".into(),
                available: true,
                version: "Apache Maven 3.9.16".into(),
                path: r"D:\Tools\Maven\bin\mvn".into(),
            },
            Found {
                name: "gradle".into(),
                bin: "gradle".into(),
                available: false,
                version: String::new(),
                path: String::new(),
            },
        ];
        let note = render_note(&found, false).expect("有结果就该出提示");
        assert!(note.contains("mvn"), "{note}");
        assert!(note.contains("3.9.16"), "{note}");
        assert!(note.contains("没有"), "{note}");
        assert!(note.contains("gradle"), "{note}");
        // 没有环境准备连接时，不许提 connect（提示词不虚报能力）
        assert!(!note.contains("connect"), "{note}");
    }

    #[test]
    fn render_note_points_at_connect_only_when_that_connector_exists() {
        let found = vec![Found {
            name: "gradle".into(),
            bin: "gradle".into(),
            available: false,
            version: String::new(),
            path: String::new(),
        }];
        let without = render_note(&found, false).unwrap();
        let with = render_note(&found, true).unwrap();
        assert!(without.contains("不要自己跑安装命令"), "{without}");
        assert!(with.contains("connect"), "{with}");
        assert!(!with.contains("不要自己跑安装命令"), "{with}");
    }

    #[test]
    fn render_note_is_none_without_probe_results() {
        assert!(render_note(&[], true).is_none());
    }

    #[test]
    fn version_line_loses_the_repeated_name_and_the_build_hash() {
        // `git version 2.54.0` —— 名字前面已经写了，去掉重复的那截
        assert_eq!(
            without_name_prefix("git version 2.54.0", "git"),
            "version 2.54.0"
        );
        assert_eq!(without_name_prefix("GIT version 2", "git"), "version 2");
        // 名字不是独立词时不切（否则 gitk 会被切成 k）
        assert_eq!(without_name_prefix("gitk 1.0", "git"), "gitk 1.0");
        assert_eq!(
            without_name_prefix("Apache Maven 3.9.16", "mvn"),
            "Apache Maven 3.9.16"
        );
        // 括号里是构建哈希，模型看了也没用
        assert_eq!(
            short_version("Apache Maven 3.9.16 (2bdd9fddda4b155ebf)"),
            "Apache Maven 3.9.16"
        );
        assert_eq!(
            short_version("cargo 1.97.1 (c980f4866 2026-06-30)"),
            "cargo 1.97.1"
        );
        assert_eq!(short_version("1.0"), "1.0");
    }

    /// 真起进程的测试：只断言"缓存不改变结论 / 常驻项确实常驻"，不断言本机装了什么
    /// （那取决于机器，断言它就变成环境相关测试了）。
    #[test]
    fn discover_reports_consistently_across_the_cache() {
        let dir = tmp_root(&["README.md"]);
        let cfg = DiscoverConfig::default();
        let first = discover(&dir, &cfg, "python", "node");
        let second = discover(&dir, &cfg, "python", "node");
        assert!(!first.is_empty(), "git 是常驻项，不该为空");
        assert_eq!(first, second, "缓存不该改变结论");
        assert!(
            first.iter().all(|t| t.name == "git"),
            "空项目只该探到常驻项：{first:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_note_folds_a_long_available_list() {
        let found: Vec<Found> = (0..NOTE_MAX + 3)
            .map(|i| Found {
                name: format!("tool{i}"),
                bin: format!("tool{i}"),
                available: true,
                version: "1.0".into(),
                path: String::new(),
            })
            .collect();
        let note = render_note(&found, false).unwrap();
        assert!(note.contains("还有 3 个没列出"), "{note}");
    }
}
