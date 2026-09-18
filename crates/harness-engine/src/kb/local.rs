//! 本地目录来源：遍历目录 → 过滤 → 读成 [`KbDoc`]。
//!
//! 过滤规则是**安全相关的**，不是洁癖：
//!
//! - **不索引密钥类文件**（`.env*`、`*.pem`、`id_rsa*`、`*secret*`…）：知识库会被检索进
//!   prompt、还会写进日志与 trace —— 一旦把 `.env` 索引进去，等于把密钥喂给模型。
//!   这与 v0.4 的"凭据只进 config"是同一条纪律；
//! - 跳过构建产物与依赖目录（`target/`、`node_modules/`、`.venv/`…）：它们是**同一份信息的
//!   大量拷贝**，最典型的"花 5 份预算换 1 份信息"；
//! - 扩展名白名单 + 必须能当 UTF-8 读：二进制不进库（识别不出内容的检索结果就是噪声）；
//! - 单文件大小上限：大文件（日志、数据集）对 Agent 没价值，但会吃光预算。
//!
//! **过滤只写一遍**：`walk` 只做遍历 + 过滤（不读内容），`scan` 在它的结果上读内容。
//! 陈旧检测也走 `walk`，于是"索引时收录什么"与"陈旧检测比较什么"永远是同一套规则 ——
//! 两处实现同一套规则，是 bug 的经典产地（v0.2 的 `# noqa` 口径就是这么出事的）。

use super::{KbDoc, KbScan, KbSource};
use crate::exec::{is_cancelled, CancelFlag};
use std::path::{Path, PathBuf};

/// 目录级黑名单（名字精确匹配）
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "__pycache__",
    "site-packages",
    "vendor",
    "gen",
    "coverage",
    "Pods",
];

/// 目录名后缀黑名单（打包元数据/缓存目录：里面是生成物，不是知识）
const SKIP_DIR_SUFFIXES: &[&str] = &[".egg-info", ".dist-info", "-packages", ".harness-target"];

/// 文件名级黑名单（小写后的包含匹配）—— **密钥优先**
const SECRET_HINTS: &[&str] = &[
    ".env",
    "env.",
    ".pem",
    ".pfx",
    ".key",
    ".p12",
    "id_rsa",
    "id_ed25519",
    "secret",
    "credential",
    "password",
    ".netrc",
];

/// 可索引的文本扩展名
const TEXT_EXTS: &[&str] = &[
    "md", "markdown", "rst", "txt", "org", "tex", "adoc", "rs", "py", "js", "mjs", "cjs", "ts",
    "tsx", "jsx", "java", "kt", "go", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php", "lua",
    "sh", "bash", "zsh", "ps1", "bat", "cmd", "sql", "toml", "yaml", "yml", "json", "jsonl",
    "ini", "cfg", "conf", "properties", "gradle", "tf", "proto", "vue", "svelte", "html", "css",
    "scss", "less",
];

/// 没有扩展名但确实是文本的文件
const TEXT_NAMES: &[&str] = &[
    "readme",
    "license",
    "makefile",
    "dockerfile",
    "procfile",
    "justfile",
];

/// 目录里的一个候选文件（只 stat，不读内容）。
#[derive(Debug, Clone, Default)]
pub struct FileStat {
    /// 相对来源根的 posix 路径
    pub path: String,
    pub abs: PathBuf,
    pub mtime_ms: u64,
    pub size: u64,
}


pub struct LocalSource {
    pub root: PathBuf,
    pub label: String,
    pub max_file_bytes: u64,
}

impl LocalSource {
    pub fn new(root: &Path, label: &str, max_file_bytes: u64) -> LocalSource {
        LocalSource {
            root: root.to_path_buf(),
            label: label.to_string(),
            max_file_bytes,
        }
    }

    /// 遍历 + 过滤（不读内容）。返回 (候选文件, 被跳过且有理由说明的文件)。
    pub fn walk(&self, cancel: &CancelFlag) -> Result<(Vec<FileStat>, Vec<(String, String)>), String> {
        if !self.root.is_dir() {
            return Err(format!(
                "知识库来源不是一个目录（或不存在）：{}",
                self.root.display()
            ));
        }
        let mut files: Vec<PathBuf> = Vec::new();
        let mut skipped: Vec<(String, String)> = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            if is_cancelled(cancel) {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                // 读不了的子目录要说出来，别装作它不存在
                skipped.push((
                    rel(&self.root, &dir),
                    "目录不可读（权限或已被删除）".to_string(),
                ));
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                let lower = name.to_lowercase();
                if p.is_dir() {
                    if name.starts_with('.')
                        || SKIP_DIRS.contains(&name.as_str())
                        || SKIP_DIR_SUFFIXES.iter().any(|s| lower.ends_with(s))
                    {
                        continue;
                    }
                    stack.push(p);
                } else {
                    files.push(p);
                }
            }
        }
        files.sort();

        let mut out: Vec<FileStat> = Vec::new();
        for p in files {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if SECRET_HINTS.iter().any(|h| name.contains(h)) {
                skipped.push((rel(&self.root, &p), "疑似密钥文件，不索引".into()));
                continue;
            }
            if name.starts_with('.') {
                // 隐藏文件（.gitignore 之类）：信息量低、噪音高，静默跳过
                continue;
            }
            if !is_text_like(&name) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&p) else {
                skipped.push((rel(&self.root, &p), "读元信息失败".into()));
                continue;
            };
            if meta.len() > self.max_file_bytes {
                skipped.push((
                    rel(&self.root, &p),
                    format!("超过单文件上限（{} 字节）", self.max_file_bytes),
                ));
                continue;
            }
            out.push(FileStat {
                path: rel(&self.root, &p),
                abs: p,
                mtime_ms: mtime_ms(&meta),
                size: meta.len(),
            });
        }
        Ok((out, skipped))
    }

    /// 读内容 → [`KbDoc`]。`fetch` 只是它的瘦包装。
    pub fn scan(&self, cancel: &CancelFlag) -> Result<KbScan, String> {
        let (files, mut skipped) = self.walk(cancel)?;
        let mut out = KbScan {
            docs: Vec::new(),
            skipped: Vec::new(),
            cancelled: is_cancelled(cancel),
        };
        for f in files {
            if is_cancelled(cancel) {
                out.cancelled = true;
                break;
            }
            let bytes = match std::fs::read(&f.abs) {
                Ok(b) => b,
                Err(e) => {
                    skipped.push((f.path.clone(), format!("读取失败: {e}")));
                    continue;
                }
            };
            // 内容 hash 用**字节**算（行尾/编码差异都算"变了"），且只读一次文件
            let content_hash = super::index::content_hash(&bytes);
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(_) => {
                    skipped.push((f.path.clone(), "不是 UTF-8 文本".into()));
                    continue;
                }
            };
            if text.trim().is_empty() {
                // 空文件**照样登记**（只是产出 0 个 chunk）。为什么不留理由跳过：
                // 陈旧检测是拿"磁盘上 walk 出来的候选"与"索引里登记的文件"比，
                // 跳过它就会每次都被报成"新增文件" —— 一个永远亮着的假警报，
                // 比没有警报更糟（人会学会忽略它）。
                out.docs.push(KbDoc {
                    path: f.path.clone(),
                    title: "（空文件）".into(),
                    text: String::new(),
                    fetched_at: crate::workspace::now_iso(),
                    rev: content_hash,
                    mtime_ms: f.mtime_ms,
                    size: f.size,
                });
                continue;
            }
            out.docs.push(KbDoc {
                path: f.path.clone(),
                title: title_of(&text),
                text,
                fetched_at: crate::workspace::now_iso(),
                rev: content_hash,
                mtime_ms: f.mtime_ms,
                size: f.size,
            });
        }
        out.skipped = skipped;
        Ok(out)
    }
}

impl KbSource for LocalSource {
    fn kind(&self) -> &'static str {
        "local"
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn fetch(&self, cancel: &CancelFlag) -> Result<KbScan, String> {
        self.scan(cancel)
    }

    fn walk_stats(
        &self,
        cancel: &CancelFlag,
    ) -> Result<(Vec<FileStat>, Vec<(String, String)>), String> {
        self.walk(cancel)
    }
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_text_like(lower_name: &str) -> bool {
    if let Some(ext) = lower_name.rsplit_once('.').map(|(_, e)| e) {
        if TEXT_EXTS.contains(&ext) {
            return true;
        }
    }
    TEXT_NAMES.contains(&lower_name)
}

/// chunk 的默认标题：优先第一个 markdown 标题，其次第一行非空文本。
pub fn title_of(text: &str) -> String {
    for line in text.lines().take(40) {
        let t = line.trim();
        if t.starts_with('#') {
            let h = t.trim_start_matches('#').trim();
            if !h.is_empty() {
                return crate::exec::clip(h, 80);
            }
        }
    }
    text.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| crate::exec::clip(l, 80))
        .unwrap_or_else(|| "（无标题）".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::new_cancel_flag;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "dh-kb-local-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn write(&self, rel: &str, content: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scan(d: &TempDir) -> KbScan {
        LocalSource::new(&d.0, "t", 64 * 1024)
            .scan(&new_cancel_flag())
            .unwrap()
    }

    #[test]
    fn indexes_text_and_docs_but_skips_build_dirs_and_binaries() {
        let d = TempDir::new("filter");
        d.write("README.md", "# 标题\n\n说明\n");
        d.write("src/a.py", "def f():\n    return 1\n");
        d.write("node_modules/pkg/index.js", "module.exports = 1\n");
        d.write("target/debug/a.rlib", "binary-ish\n");
        std::fs::write(d.0.join("blob.bin"), [0u8, 159, 146, 150, 200]).unwrap();
        let s = scan(&d);
        let paths: Vec<String> = s.docs.iter().map(|x| x.path.clone()).collect();
        assert!(paths.contains(&"README.md".to_string()), "{paths:?}");
        assert!(paths.contains(&"src/a.py".to_string()), "{paths:?}");
        assert!(!paths.iter().any(|p| p.contains("node_modules")), "{paths:?}");
        assert!(!paths.iter().any(|p| p.contains("target/")), "{paths:?}");
        assert!(!paths.contains(&"blob.bin".to_string()), "{paths:?}");
    }

    #[test]
    fn never_indexes_secret_like_files() {
        // 知识库会被检索进 prompt、还会写进 trace —— 密钥绝不能进库
        let d = TempDir::new("secret");
        d.write(".env", "API_KEY=sk-real-secret-value-123456\n");
        d.write("deploy/.env.production", "TOKEN=abc\n");
        d.write("keys/id_rsa", "-----BEGIN PRIVATE KEY-----\n");
        d.write("config/secret.yaml", "password: hunter2\n");
        d.write("app.py", "print(1)\n");
        let s = scan(&d);
        let paths: Vec<String> = s.docs.iter().map(|x| x.path.clone()).collect();
        assert_eq!(paths, vec!["app.py".to_string()], "{paths:?}");
        assert!(
            s.skipped
                .iter()
                .any(|(p, why)| p.contains("id_rsa") && why.contains("密钥")),
            "跳过原因要能说出来：{:?}",
            s.skipped
        );
    }

    #[test]
    fn oversized_files_are_skipped_with_a_reason() {
        let d = TempDir::new("big");
        d.write("big.md", &"x".repeat(2000));
        d.write("small.md", "ok\n");
        let s = LocalSource::new(&d.0, "t", 1000)
            .scan(&new_cancel_flag())
            .unwrap();
        assert_eq!(s.docs.len(), 1);
        assert_eq!(s.docs[0].path, "small.md");
        assert!(
            s.skipped
                .iter()
                .any(|(p, w)| p == "big.md" && w.contains("上限")),
            "{:?}",
            s.skipped
        );
    }

    #[test]
    fn walk_and_scan_agree_on_which_files_count() {
        // 陈旧检测走 walk、索引走 scan：两者对"哪些文件算数"必须一致，
        // 否则会天天报"新增了 N 个文件"（其实是过滤器不一致）。
        // 不变量：**walk 收录的文件，必须出现在 docs 或 skipped 里**，不许悄悄消失。
        let d = TempDir::new("agree");
        d.write("a.md", "x\n");
        d.write("b.txt", "");
        d.write("c.md", "  \n\n");
        std::fs::write(d.0.join("bad.md"), [0xffu8, 0xfe, 0x00, 0x41]).unwrap();
        d.write("node_modules/c.js", "x\n");
        d.write(".env", "K=v\n");
        let src = LocalSource::new(&d.0, "t", 64 * 1024);
        let (files, _) = src.walk(&new_cancel_flag()).unwrap();
        let s = src.scan(&new_cancel_flag()).unwrap();
        let mut accounted: Vec<String> = s
            .docs
            .iter()
            .map(|x| x.path.clone())
            .chain(s.skipped.iter().map(|(p, _)| p.clone()))
            .collect();
        accounted.sort();
        accounted.dedup();
        for f in &files {
            assert!(
                accounted.contains(&f.path),
                "walk 收录但 scan 没交代去向：{}（docs={:?} skipped={:?}）",
                f.path,
                s.docs.iter().map(|x| &x.path).collect::<Vec<_>>(),
                s.skipped
            );
        }
        assert!(
            s.skipped.iter().any(|(p, w)| p == "bad.md" && w.contains("UTF-8")),
            "非 UTF-8 文件必须有理由：{:?}",
            s.skipped
        );
    }

    #[test]
    fn cancel_stops_the_walk_early() {
        let d = TempDir::new("cancel");
        for i in 0..20 {
            d.write(&format!("f{i}.md"), "# t\n\nbody\n");
        }
        let flag = new_cancel_flag();
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let s = LocalSource::new(&d.0, "t", 64 * 1024).scan(&flag).unwrap();
        assert!(s.cancelled);
        assert!(s.docs.is_empty());
    }

    #[test]
    fn title_prefers_heading_then_first_line() {
        assert_eq!(title_of("# 标题一\n正文"), "标题一");
        assert_eq!(title_of("没有标题\n正文"), "没有标题");
        assert_eq!(title_of("\n\n   \n"), "（无标题）");
    }

    #[test]
    fn missing_root_is_an_error_not_an_empty_result() {
        // 来源目录被删 → 必须报错（否则"0 条"会被当成"这里没知识"）
        let p = std::env::temp_dir().join("dh-kb-local-does-not-exist-xyz");
        let e = LocalSource::new(&p, "t", 1024)
            .scan(&new_cancel_flag())
            .unwrap_err();
        assert!(e.contains("不是一个目录"), "{e}");
    }
}
