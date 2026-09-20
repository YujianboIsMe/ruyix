//! 项目上下文采集（"让大模型知道自己在给谁干活"）。
//!
//! 背景：引擎 `harness-engine` 的规划提示词（`plan.rs` 的 `PLAN_SYSTEM`）天生是
//! **从零规划一个新项目**的语义 —— `project_name` / `entry` / `test_command` 都是
//! "新建一个工程"的字段，而 `language` 只给 `python / rust / javascript / go` 四选一
//! 让模型自由发挥。结果就是：在已打开的真实项目里问"帮我编译 mac 包"，
//! 模型因为没有项目信息，自己挑了 rust，规划出一整套 Cargo 骨架。
//!
//! 本模块负责把"当前项目是什么"讲清楚，由调用方拼进任务描述送给模型：
//! - 项目名 / 绝对路径
//! - 推断语言（从 manifest 推断，不依赖用户手选的 `lang` —— 它默认是 unknown）
//! - 两级目录树（截断）
//! - 关键文件片段（go.mod / Cargo.toml / package.json / README 头若干行）
//!
//! 两条纪律：
//! 1. **必须有预算**。真实仓库动辄上万文件，采集必须能被 `budget` 掐断，
//!    宁可少给也不能把 prompt 撑爆（宁可让模型说"信息不足"也好过它瞎编）。
//! 2. **只读**。本模块绝不写任何文件，也不建立持久化索引（那是知识库的活）。

use std::path::{Path, PathBuf};

/// 上下文块的哨兵标记。UI 展示时要按这对标记把上下文剥掉，
/// 只给用户看他真正输入的那句话；但 RunRecord 里保留完整版 ——
/// 事后回溯"当时到底喂了什么给模型"必须有据可查。
pub const CONTEXT_BEGIN: &str = "<<<ruyix-project-context>>>";
pub const CONTEXT_END: &str = "<<<ruyix-project-context-end>>>";

/// 上下文块的字符预算上限。
/// 约 1.5~2k token：够装下一个中型项目的骨架，又不会挤占模型的思考空间。
/// 具体的每次预算由调用方按 `max_context_chars` 折算后再用这个值封顶。
pub const DEFAULT_BUDGET: usize = 6000;

const MAX_DEPTH: usize = 2;
/// 目录树最多列出多少条
const MAX_TREE_ENTRIES: usize = 120;
/// 单个文件片段最多读多少字符
const MAX_SNIPPET_CHARS: usize = 1200;

/// 这些目录既不进树也不进片段 —— 它们只会污染上下文，且通常是海量构建产物
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".idea",
    ".vscode",
    ".gradle",
    "vendor",
    "bin",
    "obj",
    ".next",
];

/// manifest 文件 → 推断语言。顺序即优先级（同目录多个 manifest 时取靠前的）。
/// 返回值统一用**引擎认识的语言名**（`PLAN_SYSTEM` 的四选一 + 扩展项）。
const MANIFEST_LANGS: &[(&str, &str)] = &[
    ("Cargo.toml", "rust"),
    ("go.mod", "go"),
    ("package.json", "javascript"),
    ("pom.xml", "java"),
    ("build.gradle", "java"),
    ("build.gradle.kts", "java"),
    ("requirements.txt", "python"),
    ("pyproject.toml", "python"),
    ("setup.py", "python"),
    ("Pipfile", "python"),
    ("CMakeLists.txt", "c"),
    ("Makefile", "c"),
    ("*.csproj", "csharp"),
    ("composer.json", "php"),
];

/// 值得给模型看全貌的关键文件（顺序即优先级）
const SNIPPET_FILES: &[&str] = &[
    "Cargo.toml",
    "go.mod",
    "package.json",
    "pom.xml",
    "build.gradle",
    "pyproject.toml",
    "requirements.txt",
    "setup.py",
    "CMakeLists.txt",
    "Makefile",
    "README.md",
    "README.MD",
    "readme.md",
    "README.rst",
    "README",
];

#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    /// 项目名（目录名）
    pub name: String,
    /// 绝对路径
    pub root: String,
    /// 推断出的主语言；无法确定时为空串
    pub lang: String,
    /// 命中的 manifest（如 "go.mod"）
    pub manifest: String,
    /// 相对路径树，形如 "cmd/server/"、"go.mod"，已排序、已截断
    pub tree: Vec<String>,
    /// 关键文件片段
    pub snippets: Vec<FileSnippet>,
    /// 是否发生过截断（树或片段装不下了）
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct FileSnippet {
    pub path: String,
    pub body: String,
}

fn to_slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn is_skip(name: &str) -> bool {
    SKIP_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
}

/// 推断项目语言。manifest 优先；manifest 都没有时看是否有 README-only / 混入情况。
fn detect_lang(root: &Path) -> (String, String) {
    for (file, lang) in MANIFEST_LANGS {
        if file.starts_with('*') {
            // 通配后缀（如 *.csproj）
            let suffix = file.trim_start_matches('*');
            if let Ok(rd) = std::fs::read_dir(root)
                && rd
                    .filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().ends_with(suffix))
            {
                return (lang.to_string(), format!("*{suffix}"));
            }
            continue;
        }
        if root.join(file).is_file() {
            return (lang.to_string(), file.to_string());
        }
    }
    (String::new(), String::new())
}

/// 两级目录树。目录以 `/` 结尾，便于模型一眼看出层级。
fn collect_tree(root: &Path) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::new();

    fn walk(dir: &Path, rel: &Path, depth: usize, out: &mut Vec<String>) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        if out.len() >= MAX_TREE_ENTRIES {
            return true;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return false;
        };
        let mut names: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        names.sort();
        for p in names {
            if out.len() >= MAX_TREE_ENTRIES {
                return true;
            }
            let file_name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if p.is_dir() && is_skip(&file_name) {
                continue;
            }
            let rel_p = rel.join(&file_name);
            if p.is_dir() {
                out.push(format!("{}/", to_slash(&rel_p)));
                if walk(&p, &rel_p, depth + 1, out) {
                    return true;
                }
            } else {
                out.push(to_slash(&rel_p));
            }
        }
        false
    }

    let truncated = walk(root, Path::new(""), 1, &mut out);
    (out, truncated)
}

/// 读一个纯文本文件的前若干字符。非 UTF-8（二进制）直接放弃。
fn snippet_of(root: &Path, rel: &str) -> Option<FileSnippet> {
    let p = root.join(rel);
    if !p.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&p).ok()?;
    let body: String = text.chars().take(MAX_SNIPPET_CHARS).collect();
    let body = body.trim_end().to_string();
    if body.is_empty() {
        return None;
    }
    Some(FileSnippet {
        path: rel.to_string(),
        body,
    })
}

/// 采集项目上下文。`budget` 是整块文本的字符上限。
pub fn collect(root: &str, budget: usize) -> Result<ProjectContext, String> {
    let path = Path::new(root);
    if !path.is_dir() {
        return Err(format!("项目目录不存在或不是目录: {root}"));
    }
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let name = abs
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string());

    let (lang, manifest) = detect_lang(&abs);
    let (tree, tree_truncated) = collect_tree(&abs);

    let mut ctx = ProjectContext {
        name,
        root: to_slash(&abs),
        lang,
        manifest,
        tree,
        snippets: Vec::new(),
        truncated: tree_truncated,
    };

    // 预算：以**实际渲染长度**为准，超了就回退一步。
    // 早先用"估算字符数"容易漏算树条目与固定段落，最后实际输出照样超预算；
    // 这里改成加完即量、量超即退，做到"渲染出来是多少就是多少"。
    while render(&ctx).len() > budget {
        if ctx.tree.len() > 8 {
            ctx.tree.pop();
            ctx.truncated = true;
        } else if !ctx.snippets.is_empty() {
            ctx.snippets.pop();
            ctx.truncated = true;
        } else {
            // 预算小到连骨架都装不下（异常用法）：清空并如实标记截断
            ctx.tree.clear();
            ctx.truncated = true;
            break;
        }
    }

    // 在剩余预算里逐个塞关键文件片段：先塞，再量，超了回退并停止
    for rel in SNIPPET_FILES {
        let Some(snip) = snippet_of(&abs, rel) else {
            continue;
        };
        ctx.snippets.push(snip);
        if render(&ctx).len() > budget {
            ctx.snippets.pop();
            ctx.truncated = true;
            break;
        }
    }

    Ok(ctx)
}

/// 不含片段的裸文本长度（用于预算计算）
fn render_without_snippets(ctx: &ProjectContext) -> String {
    let mut s = String::new();
    s.push_str(&format!("项目路径：{}\n", ctx.root));
    s.push_str(&format!("项目名：{}\n", ctx.name));
    if !ctx.lang.is_empty() {
        s.push_str(&format!(
            "项目语言：{}（依据：{}）\n",
            ctx.lang,
            if ctx.manifest.is_empty() {
                "目录结构推断".to_string()
            } else {
                ctx.manifest.clone()
            }
        ));
    }
    s
}

/// 渲染主体（项目段 + 树 + 片段 + 截断说明）：管线版与 agent 版共用。
fn render_body(ctx: &ProjectContext) -> String {
    let mut s = String::new();
    s.push_str("# 当前项目（这是**已存在的真实项目**，不要新建项目）\n\n");
    s.push_str(&render_without_snippets(ctx));

    if !ctx.tree.is_empty() {
        s.push_str("\n目录结构（两级）：\n");
        for t in &ctx.tree {
            s.push_str(&format!("  {t}\n"));
        }
    }

    if !ctx.snippets.is_empty() {
        s.push_str("\n关键文件内容：\n");
        for snip in &ctx.snippets {
            s.push_str(&format!("--- {} ---\n{}\n", snip.path, snip.body));
        }
    }

    if ctx.truncated {
        s.push_str("\n（上下文已被截断：项目较大，以上只包含主要结构。）\n");
    }
    s
}

/// 渲染成给大模型看的文本块（不含哨兵标记）。
/// 调用方负责加 `CONTEXT_BEGIN` / `CONTEXT_END`。
pub fn render(ctx: &ProjectContext) -> String {
    let mut s = render_body(ctx);
    s.push_str(
        "\n要求：\n\
         1. 所有文件路径都相对上面这个项目根目录，禁止另起一个新项目。\n\
         2. 语言必须与上面标注的项目语言一致，不要引入别的构建体系。\n\
         3. 如果任务需要改动既有文件，请在步骤里明确写出这些既有文件的路径。\n",
    );
    s
}

/// 把项目上下文拼到任务描述前面（带哨兵标记）。
/// 采集失败（目录不存在等）时**退回原任务**并记录原因 ——
/// 少一段上下文不该让整个会话失败。
pub fn with_context(task: &str, root: Option<&str>, budget: usize) -> String {
    let Some(root) = root.map(str::trim).filter(|r| !r.is_empty()) else {
        return task.to_string();
    };
    match collect(root, budget) {
        Ok(ctx) => format!(
            "{}\n{}\n{}\n{}",
            CONTEXT_BEGIN,
            render(&ctx),
            CONTEXT_END,
            task.trim()
        ),
        Err(_) => task.to_string(),
    }
}

/// 会话 Agent 版注入：与 [`with_context`] 同一份采集，但**不带**结尾的
/// "要求"段 —— 那是规划管线的口吻（"在步骤里写出路径"），工具循环里只是噪音；
/// 路径相对根、改前先读这些纪律 `AGENT_SYSTEM` 已经讲了。
/// 哨兵对与管线版相同，`strip_context` 通用。
pub fn with_context_for_agent(task: &str, root: Option<&str>, budget: usize) -> String {
    let Some(root) = root.map(str::trim).filter(|r| !r.is_empty()) else {
        return task.to_string();
    };
    match collect(root, budget) {
        Ok(ctx) => format!(
            "{}\n{}\n{}\n{}",
            CONTEXT_BEGIN,
            render_body(&ctx),
            CONTEXT_END,
            task.trim()
        ),
        Err(_) => task.to_string(),
    }
}

/// UI 侧用：把任务里可能存在的上下文块剥掉，只留用户输入的原话。
pub fn strip_context(task: &str) -> &str {
    match task.find(CONTEXT_END) {
        Some(i) => task[i + CONTEXT_END.len()..]
            .trim_start_matches(['\r', '\n'])
            .trim(),
        None => task,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ruyix-pctx-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detects_go_from_go_mod() {
        let d = tdir("go");
        std::fs::write(d.join("go.mod"), "module ai-gateway\n\ngo 1.22\n").unwrap();
        std::fs::create_dir_all(d.join("internal/handler")).unwrap();
        std::fs::write(d.join("internal/handler/ping.go"), "package handler\n").unwrap();

        let ctx = collect(&d.to_string_lossy(), DEFAULT_BUDGET).unwrap();
        assert_eq!(ctx.lang, "go");
        assert_eq!(ctx.manifest, "go.mod");
        assert_eq!(ctx.name, d.file_name().unwrap().to_string_lossy());
        assert!(ctx.tree.iter().any(|t| t == "go.mod"));
        assert!(ctx.tree.iter().any(|t| t == "internal/"));
        assert!(ctx.snippets.iter().any(|s| s.path == "go.mod"));
    }

    #[test]
    fn detects_rust_from_cargo_toml() {
        let d = tdir("rust");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let ctx = collect(&d.to_string_lossy(), DEFAULT_BUDGET).unwrap();
        assert_eq!(ctx.lang, "rust");
    }

    #[test]
    fn detects_python_and_javascript() {
        let d = tdir("py");
        std::fs::write(d.join("requirements.txt"), "flask\n").unwrap();
        assert_eq!(
            collect(&d.to_string_lossy(), DEFAULT_BUDGET).unwrap().lang,
            "python"
        );

        let d2 = tdir("js");
        std::fs::write(d2.join("package.json"), "{\"name\":\"y\"}").unwrap();
        assert_eq!(
            collect(&d2.to_string_lossy(), DEFAULT_BUDGET).unwrap().lang,
            "javascript"
        );
    }

    #[test]
    fn skips_noise_dirs_and_respects_depth() {
        let d = tdir("noise");
        std::fs::create_dir_all(d.join("node_modules/pkg")).unwrap();
        std::fs::write(d.join("node_modules/pkg/index.js"), "x").unwrap();
        std::fs::create_dir_all(d.join("target/debug")).unwrap();
        std::fs::create_dir_all(d.join("a/b/c/d")).unwrap();
        std::fs::write(d.join("a/b/c/d/deep.txt"), "deep").unwrap();

        let ctx = collect(&d.to_string_lossy(), DEFAULT_BUDGET).unwrap();
        assert!(
            !ctx.tree.iter().any(|t| t.starts_with("node_modules")),
            "应跳过 node_modules"
        );
        assert!(
            !ctx.tree.iter().any(|t| t.starts_with("target")),
            "应跳过 target"
        );
        // 深度 2：a/ 与 a/b/ 可见，a/b/c/ 不可见
        assert!(ctx.tree.iter().any(|t| t == "a/"));
        assert!(
            !ctx.tree.iter().any(|t| t.starts_with("a/b/c")),
            "不得超过两级"
        );
    }

    #[test]
    fn budget_is_never_exceeded() {
        let d = tdir("budget");
        for i in 0..200 {
            std::fs::write(d.join(format!("file_{i:03}.txt")), "x".repeat(100)).unwrap();
        }
        std::fs::write(d.join("README.md"), "y".repeat(5000)).unwrap();

        let ctx = collect(&d.to_string_lossy(), 1500).unwrap();
        let out = render(&ctx);
        assert!(out.len() <= 1500 + 512, "渲染结果 {} 远超预算", out.len());
        assert!(ctx.truncated, "内容装不下来应标记 truncated");
        // README 片段被整体截断到 MAX_SNIPPET_CHARS
        if let Some(s) = ctx.snippets.iter().find(|s| s.path == "README.md") {
            assert!(s.body.chars().count() <= MAX_SNIPPET_CHARS);
        }
    }

    #[test]
    fn agent_variant_has_no_planner_requirements() {
        let d = tdir("agentctx");
        std::fs::write(d.join("go.mod"), "module gw\n").unwrap();
        let task = "把这个项目的 README 补一段安装说明";

        let full = with_context_for_agent(task, Some(&d.to_string_lossy()), DEFAULT_BUDGET);
        assert!(full.starts_with(CONTEXT_BEGIN) && full.contains(CONTEXT_END));
        assert!(full.ends_with(task), "任务原话压在最后");
        assert!(full.contains("项目语言：go"));
        assert!(!full.contains("要求："), "agent 版不该带规划口吻的要求段");
        // UI 剥离兼容：与管线版共用同一对哨兵
        assert_eq!(strip_context(&full), task);

        // 管线版保持原样（要求段还在）
        let plan = with_context(task, Some(&d.to_string_lossy()), DEFAULT_BUDGET);
        assert!(plan.contains("要求："));
    }

    #[test]
    fn missing_dir_falls_back_to_raw_task() {
        let task = "帮我编译出 mac 版本二进制包";
        let out = with_context(task, Some("D:/no/such/dir/xyz"), DEFAULT_BUDGET);
        assert_eq!(out, task, "采集失败应退回原任务而不是报错");
        assert_eq!(with_context(task, None, DEFAULT_BUDGET), task);
        assert_eq!(with_context(task, Some("   "), DEFAULT_BUDGET), task);
    }

    #[test]
    fn context_block_is_wrapped_and_strippable() {
        let d = tdir("wrap");
        std::fs::write(d.join("go.mod"), "module gw\n").unwrap();
        let task = "帮我编译出 mac 版本二进制包";

        let full = with_context(task, Some(&d.to_string_lossy()), DEFAULT_BUDGET);
        assert!(full.starts_with(CONTEXT_BEGIN) && full.contains(CONTEXT_END));
        assert!(
            full.contains(&d.to_string_lossy().replace('\\', "/")),
            "应带绝对路径"
        );
        assert!(full.contains("不要新建项目"), "应明确禁止新建项目");
        assert!(full.ends_with(task));

        // UI 侧剥掉后应拿回用户原话
        assert_eq!(strip_context(&full), task);
        assert_eq!(strip_context(task), task, "没有上下文时原样返回");
    }
}
