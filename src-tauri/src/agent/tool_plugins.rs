//! 用户侧的工具表（v1.4 §4.2 拍板 (a) 的宿主半边）。
//!
//! 读 `plugins/tools/<id>/tools.toml`，把两类行**并入**引擎与宿主的表：
//!
//! ```toml
//! # 命令发现表（`engine::discover::TOOLS`）：加一个工具 = 加一段
//! [[discover]]
//! name = "pnpm"
//! bin = "pnpm"
//! version_args = ["--version"]
//! markers = ["pnpm-lock.yaml"]     # 空 = 常驻探测（每个项目都探）
//! extensions = []
//!
//! # 包管理器候选（`env_setup::Pm`）：加一个管理器 = 加一段
//! [[pm]]
//! id = "volta"
//! bin = "volta"
//! install = "volta install {pkg}"  # {pkg} 会被替换成包名
//! platforms = ["windows", "macos"] # 空 = 全平台
//! elevate = false                  # unix 上要不要 sudo -n
//! [pm.packages]
//! node = "node@lts"
//! ```
//!
//! **为什么这是"唯一的加载点"而不是把 RSI 编进源码**：它做的只是"读文件 + 并表"，
//! 形状与 `plugins/highlight/<id>/theme.css` 被启动注入完全一样。一次接线之后，
//! "加一个工具 / 加一个包管理器"就变成**只改数据**—— 这正是自改闭环能成立的前提
//! （见 `doc/v1.4/需求-递归自我改进-v1.4.md` §4.2）。
//!
//! 纪律：
//! - **坏文件不静默**：解析失败 / 字段非法 ⇒ 跳过该文件并留一条能指名到文件与字段的 note
//!   （静默跳过等于"用户改了但没生效，且没人告诉他"）；
//! - **解析与登记分开**（`parse_dir` 纯函数 / `load` 登记）：单测验解析时不许污染进程级的
//!   引擎表 —— 这是引擎侧那一刀踩过的坑（注册一条常驻行就让别的用例红）；
//! - 目录顺序按 id 排序，装载结果**可复现**。

use harness_engine as engine;
use serde::Deserialize;
use std::path::Path;

/// 每张表要读的文件名（`plugins/tools/<id>/tools.toml`）
pub const FILE_NAME: &str = "tools.toml";
/// `plugins/` 下这个类别所在的子目录
pub const SUBDIR: &str = "tools";

/// 一次装载的回执（启动日志里打出来，也方便单测断言）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// 并进命令发现表的行数
    pub discover_rows: usize,
    /// 并进包管理器候选的行数
    pub pm_rows: usize,
    /// 被跳过 / 需要人看一眼的问题（每条都指名文件）
    pub notes: Vec<String>,
}

/// `tools.toml` 的形状。
#[derive(Debug, Deserialize)]
struct PluginFile {
    #[serde(default)]
    discover: Vec<DiscoverRow>,
    #[serde(default)]
    pm: Vec<PmRow>,
}

#[derive(Debug, Deserialize)]
struct DiscoverRow {
    name: String,
    bin: String,
    #[serde(default)]
    version_args: Vec<String>,
    #[serde(default)]
    markers: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PmRow {
    id: String,
    bin: String,
    install: String,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    elevate: bool,
    #[serde(default)]
    packages: std::collections::BTreeMap<String, String>,
}

/// 解析结论：**还没登记**的两张表（登记会改进程级状态，留给 [`load`]）。
#[derive(Default)]
pub struct Parsed {
    pub tools: Vec<engine::discover::ToolSpec>,
    pub pms: Vec<crate::agent::env_setup::PmSpec>,
    pub report: Report,
}

/// 扫 `plugins/tools/*/tools.toml` 并解析（纯函数：不碰任何全局状态）。
pub fn parse_dir(plugins_root: &Path) -> Parsed {
    let mut out = Parsed::default();
    let dir = plugins_root.join(SUBDIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out; // 没有这个目录 = 没人要用这个能力，不是错误
    };
    // 按 id 排序：装载顺序可复现（同一份插件，两次启动并表结果一致）
    let mut ids: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    ids.sort();
    for id_dir in ids {
        let id = id_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = id_dir.join(FILE_NAME);
        if !file.is_file() {
            out.report
                .notes
                .push(format!("{SUBDIR}/{id}：没有 {FILE_NAME}，跳过"));
            continue;
        }
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                out.report
                    .notes
                    .push(format!("{SUBDIR}/{id}/{FILE_NAME}：读不出来（{e}）"));
                continue;
            }
        };
        let parsed: PluginFile = match toml::from_str(&text) {
            Ok(p) => p,
            Err(e) => {
                out.report.notes.push(format!(
                    "{SUBDIR}/{id}/{FILE_NAME}：解析失败（{e}）—— 这个文件整份跳过，其它插件不受影响"
                ));
                continue;
            }
        };
        // ---- 命令发现行 ----
        for (i, row) in parsed.discover.iter().enumerate() {
            let n = i + 1;
            if row.name.trim().is_empty() || row.bin.trim().is_empty() {
                out.report.notes.push(format!(
                    "{SUBDIR}/{id}/discover#{n}：name / bin 不能为空，跳过"
                ));
                continue;
            }
            out.tools.push(engine::discover::ToolSpec {
                name: engine::discover::leak_str(row.name.trim().to_string()),
                bin: engine::discover::leak_str(row.bin.trim().to_string()),
                version_args: engine::discover::leak_strs(if row.version_args.is_empty() {
                    vec!["--version".to_string()]
                } else {
                    row.version_args.clone()
                }),
                markers: engine::discover::leak_strs(row.markers.clone()),
                extensions: engine::discover::leak_strs(row.extensions.clone()),
            });
            out.report.discover_rows += 1;
        }
        // ---- 包管理器行 ----
        for (i, row) in parsed.pm.iter().enumerate() {
            let n = i + 1;
            let bad = |why: &str, out: &mut Parsed| {
                out.report
                    .notes
                    .push(format!("{SUBDIR}/{id}/pm#{n}：{why}，跳过"));
            };
            if row.id.trim().is_empty() || row.bin.trim().is_empty() {
                bad("id / bin 不能为空", &mut out);
                continue;
            }
            // 模板里没有 `{pkg}` ⇒ 这条命令装的东西与工具名无关，多半是写漏了（不是"能跑就行"）
            if !row.install.contains("{pkg}") {
                bad(
                    "install 模板里必须出现 {pkg}（否则包装不上，且错误会很难看懂）",
                    &mut out,
                );
                continue;
            }
            let unknown: Vec<&str> = row
                .platforms
                .iter()
                .map(|s| s.as_str())
                .filter(|p| !matches!(*p, "windows" | "macos" | "linux"))
                .collect();
            if !unknown.is_empty() {
                bad(
                    &format!(
                        "platforms 只认 windows / macos / linux（收到 {}）",
                        unknown.join(", ")
                    ),
                    &mut out,
                );
                continue;
            }
            out.pms.push(crate::agent::env_setup::PmSpec {
                id: row.id.trim().to_string(),
                bin: row.bin.trim().to_string(),
                install: row.install.trim().to_string(),
                platforms: row.platforms.iter().map(|p| p.trim().to_string()).collect(),
                elevate: row.elevate,
                packages: row.packages.clone(),
            });
            out.report.pm_rows += 1;
        }
    }
    out
}

/// 装载并把两类行**并进**引擎 / 宿主的表（宿主启动时调用一次）。
///
/// 不重复调用：登记是**追加**语义（同名并存），按项目重复装载只会重复追加。
pub fn load(plugins_root: &Path) -> Report {
    let parsed = parse_dir(plugins_root);
    engine::discover::register_extra(parsed.tools);
    crate::agent::env_setup::register_custom(parsed.pms);
    parsed.report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ruyix-toolplug-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_plugin(root: &Path, id: &str, body: &str) {
        let d = root.join(SUBDIR).join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(FILE_NAME), body).unwrap();
    }

    /// 正常装载：两类行都进表，且**解析阶段不碰全局状态**（`parse_dir` 是纯的）。
    #[test]
    fn a_well_formed_plugin_yields_both_kinds_of_rows() {
        let d = tmp("ok");
        write_plugin(
            &d,
            "acme",
            r#"
[[discover]]
name = "pnpm"
bin = "pnpm"
markers = ["pnpm-lock.yaml"]

[[pm]]
id = "volta"
bin = "volta"
install = "volta install {pkg}"
platforms = ["windows"]
[pm.packages]
node = "node@lts"
"#,
        );
        let p = parse_dir(&d);
        assert_eq!(p.report.discover_rows, 1);
        assert_eq!(p.report.pm_rows, 1);
        assert!(p.report.notes.is_empty(), "{:?}", p.report.notes);
        assert_eq!(p.tools[0].name, "pnpm");
        assert_eq!(p.tools[0].bin, "pnpm");
        assert_eq!(
            p.tools[0].version_args,
            &["--version"],
            "没写 version_args 时给个默认（多数工具都认）"
        );
        assert_eq!(p.tools[0].markers, &["pnpm-lock.yaml"]);
        let pm = &p.pms[0];
        assert_eq!((pm.id.as_str(), pm.bin.as_str()), ("volta", "volta"));
        assert_eq!(
            pm.packages.get("node").map(|s| s.as_str()),
            Some("node@lts")
        );
        assert_eq!(pm.platforms, vec!["windows".to_string()]);
    }

    /// 坏文件**被点名**且不影响别的插件：解析失败、缺 `{pkg}`、平台名写错，各留一条 note。
    #[test]
    fn bad_rows_are_named_and_do_not_block_the_good_ones() {
        let d = tmp("bad");
        write_plugin(&d, "broken", "this is not toml at all = = =\n");
        write_plugin(
            &d,
            "sloppy",
            r#"
[[discover]]
name = "ok-tool"
bin = "ok-tool"
markers = ["x.toml"]

[[pm]]
id = "no-placeholder"
bin = "np"
install = "np install something"

[[pm]]
id = "wrong-platform"
bin = "wp"
install = "wp add {pkg}"
platforms = ["plan9"]
"#,
        );
        let p = parse_dir(&d);
        assert_eq!(p.report.discover_rows, 1, "同一份文件里的好行照收");
        assert_eq!(p.report.pm_rows, 0, "两条 pm 行都非法");
        assert_eq!(p.report.notes.len(), 3, "{:?}", p.report.notes);
        assert!(
            p.report
                .notes
                .iter()
                .any(|n| n.contains("broken") && n.contains("解析失败")),
            "坏文件要点名到文件：{:?}",
            p.report.notes
        );
        assert!(
            p.report.notes.iter().any(|n| n.contains("{pkg}")),
            "缺占位符要说清为什么不行：{:?}",
            p.report.notes
        );
        assert!(
            p.report.notes.iter().any(|n| n.contains("plan9")),
            "非法平台值要回显收到的值：{:?}",
            p.report.notes
        );
    }

    /// `load()` 真的把行**并进引擎表**（解析纯函数的测试证明不了这一步）。
    ///
    /// 行刻意**带 marker**（不当常驻项）：`register_extra` 是进程级、追加语义，注册一条常驻行
    /// 会污染同进程里别的用例（引擎侧那一刀踩过）。这里只断言"引擎能看到这一行"。
    #[test]
    fn load_merges_the_rows_into_the_engine_table() {
        let d = tmp("merge");
        write_plugin(
            &d,
            "merge-demo",
            r#"
[[discover]]
name = "rsi-merge-demo"
bin = "zzz-rsi-merge-demo"
markers = ["rsi-merge-marker.toml"]
"#,
        );
        let rep = load(&d);
        assert_eq!(rep.discover_rows, 1);
        let all = engine::discover::all_tools();
        assert!(
            all.iter().any(|t| t.bin == "zzz-rsi-merge-demo"),
            "装载之后引擎的全表里必须有这一行"
        );
    }

    /// 没有 `plugins/tools/` 目录 = 没人用这个能力，**不是错误**（不产生 note）。
    #[test]
    fn a_missing_directory_is_not_an_error() {
        let d = tmp("none");
        let p = parse_dir(&d);
        assert_eq!(
            (
                p.report.discover_rows,
                p.report.pm_rows,
                p.report.notes.len()
            ),
            (0, 0, 0)
        );
    }
}
