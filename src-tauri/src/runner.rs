//! 运行目标命令推断：读清单文件内容生成运行命令。
//!
//! 为什么需要这个模块：以前 `package.json` 的建议命令是写死的 `npm start`
//! （`config.rs` 的 `MANIFEST_FILES` 常量），不管里面写的是 `dev` 还是别的脚本，
//! 一律套 `npm start`。这里改为真正读文件内容：
//!
//! - `package.json` → `scripts` 里**每个脚本各生成一个运行目标**（`dev` / `start` / `serve` 排前面），
//!   包管理器按同目录锁文件识别（pnpm / yarn / bun / npm）
//! - `Cargo.toml` / `Makefile` → 固定命令（保持原有行为）
//!
//! 注意：`serde_json` 的 Map 默认按 key 排序（未开 preserve_order），所以
//! 同优先级脚本之间的顺序是**字典序**，不是 package.json 里的书写顺序。

use std::path::Path;

/// 一个建议的运行目标：名称 + 命令
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunSpec {
    /// 运行目标名称（package.json 用脚本名，如 dev）
    pub name: String,
    /// 运行命令（如 npm run dev）
    pub cmd: String,
}

/// 清单文件 → 建议的运行目标列表。
///
/// 返回值语义：
/// - `None`：不是可识别的清单文件，或文件读不出/语法错 → 调用方走原有兜底逻辑
/// - `Some(vec![])`：是清单文件，但没有可运行的脚本（如 package.json 无 scripts）
/// - `Some(specs)`：建议的运行目标（至少一条）
pub fn manifest_run_specs(path: &Path) -> Option<Vec<RunSpec>> {
    let raw_name = path.file_name()?.to_string_lossy().to_string();
    match raw_name.to_lowercase().as_str() {
        "cargo.toml" => Some(vec![RunSpec {
            name: raw_name,
            cmd: "cargo run".to_string(),
        }]),
        "makefile" | "gnumakefile" => Some(vec![RunSpec {
            name: raw_name,
            cmd: "make".to_string(),
        }]),
        "package.json" => package_json_specs(path),
        _ => None,
    }
}

/// `package.json`：`scripts` 里每个条目生成一个运行目标
fn package_json_specs(path: &Path) -> Option<Vec<RunSpec>> {
    let text = std::fs::read_to_string(path).ok()?;
    // JSON 解析失败 → None，调用方回退到静态命令表
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;

    // 是 package.json 但没有（或不是）scripts → 明确的"没有可运行脚本"，
    // 不能返回 None：那会退回写死的 npm start，等于把这个 BUG 放回去
    let Some(scripts) = json.get("scripts").and_then(|v| v.as_object()) else {
        return Some(Vec::new());
    };

    let pm = detect_package_manager(path);
    let mut names: Vec<String> = scripts.keys().cloned().collect();
    // dev / start / serve 排前面；同优先级内保持稳定顺序
    names.sort_by_key(|n| script_priority(n));

    Some(
        names
            .into_iter()
            .map(|n| RunSpec {
                cmd: format!("{pm} run {n}"),
                name: n,
            })
            .collect(),
    )
}

/// 脚本排序优先级：dev → start → serve → 其他
fn script_priority(name: &str) -> u8 {
    match name {
        "dev" => 0,
        "start" => 1,
        "serve" => 2,
        _ => 3,
    }
}

/// 按同目录锁文件判断包管理器（缺省 npm）
fn detect_package_manager(manifest: &Path) -> &'static str {
    let dir = manifest.parent().unwrap_or_else(|| Path::new("."));
    if dir.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if dir.join("yarn.lock").exists() {
        "yarn"
    } else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    }
}

// ============================================
// 测试
// ============================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 临时目录，Drop 时自动清理
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间异常")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "dh-code-runner-test-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("创建临时目录失败");
            TempDir(dir)
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("创建父目录失败");
            }
            std::fs::write(&p, content).expect("写文件失败");
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 核心用例：package.json 里写的是 dev，就必须得到 npm run dev，
    /// 而不是写死的 npm start（修复前 BUG 的复现点）
    #[test]
    fn package_json_scripts_become_run_targets() {
        let dir = TempDir::new("pkg");
        let pkg = dir.write(
            "package.json",
            r#"{"name":"admin-web","scripts":{"build":"vite build","dev":"vite","test":"vitest"}}"#,
        );

        let specs = manifest_run_specs(&pkg).expect("应识别为清单文件");
        let pairs: Vec<(String, String)> =
            specs.into_iter().map(|s| (s.name, s.cmd)).collect();

        assert_eq!(
            pairs,
            vec![
                ("dev".to_string(), "npm run dev".to_string()),
                ("build".to_string(), "npm run build".to_string()),
                ("test".to_string(), "npm run test".to_string()),
            ],
            "dev 应排第一，且命令取自 scripts"
        );
    }

    /// 包管理器按锁文件识别
    #[test]
    fn package_manager_follows_lockfile() {
        for (lock, expected) in [
            ("pnpm-lock.yaml", "pnpm run dev"),
            ("yarn.lock", "yarn run dev"),
            ("bun.lockb", "bun run dev"),
            ("package-lock.json", "npm run dev"),
        ] {
            let dir = TempDir::new("pm");
            dir.write(
                "package.json",
                r#"{"name":"x","scripts":{"dev":"vite"}}"#,
            );
            dir.write(lock, "");
            let pkg = dir.0.join("package.json");

            let specs = manifest_run_specs(&pkg).expect("应识别为清单文件");
            assert_eq!(specs.len(), 1);
            assert_eq!(specs[0].cmd, expected, "有 {} 时应用 {}", lock, expected);
        }
    }

    /// start 优先于其他脚本（没有 dev 时）
    #[test]
    fn start_sorts_before_other_scripts() {
        let dir = TempDir::new("start");
        let pkg = dir.write(
            "package.json",
            r#"{"scripts":{"zip":"zip -r x.zip .","start":"node server.js"}}"#,
        );
        let specs = manifest_run_specs(&pkg).expect("应识别为清单文件");
        assert_eq!(specs[0].name, "start");
        assert_eq!(specs[0].cmd, "npm run start");
    }

    /// 有清单但没有 scripts → 空列表（调用方提示"没有可运行脚本"，不瞎造命令）
    #[test]
    fn package_json_without_scripts_yields_empty() {
        let dir = TempDir::new("noscripts");
        let pkg = dir.write("package.json", r#"{"name":"x","version":"1.0.0"}"#);
        assert_eq!(manifest_run_specs(&pkg), Some(vec![]));
    }

    /// JSON 语法错误 → None（回退到旧的兜底逻辑，不崩）
    #[test]
    fn invalid_package_json_falls_back() {
        let dir = TempDir::new("badjson");
        let pkg = dir.write("package.json", "{ this is not json ");
        assert_eq!(manifest_run_specs(&pkg), None);
    }

    /// Cargo.toml / Makefile 保持原有固定命令
    #[test]
    fn other_manifests_keep_fixed_cmd() {
        let dir = TempDir::new("other");
        let cargo = dir.write("Cargo.toml", "[package]\nname = \"x\"\n");
        let make = dir.write("Makefile", "all:\n\techo hi\n");

        assert_eq!(
            manifest_run_specs(&cargo),
            Some(vec![RunSpec {
                name: "Cargo.toml".to_string(),
                cmd: "cargo run".to_string()
            }])
        );
        assert_eq!(
            manifest_run_specs(&make),
            Some(vec![RunSpec {
                name: "Makefile".to_string(),
                cmd: "make".to_string()
            }])
        );
    }

    /// 非清单文件 → None
    #[test]
    fn non_manifest_returns_none() {
        let dir = TempDir::new("non");
        let f = dir.write("main.py", "print(1)\n");
        assert_eq!(manifest_run_specs(&f), None);
    }
}
