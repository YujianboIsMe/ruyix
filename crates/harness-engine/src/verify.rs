//! 基础验证：语法检查 + 单元测试运行。
//!
//! 两条铁律：
//! 1. **不伪造结果**。每条检查都带真实命令、真实退出码、真实 stdout/stderr；
//!    没执行的检查标 `skipped` 并写明原因，绝不写成"通过"。
//! 2. **可预期状态不是错误**。"这个项目里没有 Python 文件" / "没有找到测试"
//!    都是正常状态，返回 Ok + skipped，让 UI 渲染引导信息，而不是弹一个红色错误。

use crate::config::{AppConfig, VerifyConfig};
use crate::exec::{self, CancelFlag, CmdOutput, is_cancelled};
use crate::sandbox;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CheckResult {
    /// syntax | test
    pub kind: String,
    /// 检查对象的展示名（文件名 / 项目级）
    pub target: String,
    pub language: String,
    /// passed | failed | skipped
    pub status: String,
    pub reason: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub duration_ms: u128,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    /// 实际执行的命令行（用户能自己复现）
    #[serde(default)]
    pub cmd: String,
}

impl CheckResult {
    fn passed(kind: &str, language: &str, target: &str, out: &CmdOutput) -> Self {
        Self {
            kind: kind.into(),
            target: target.into(),
            language: language.into(),
            status: "passed".into(),
            reason: String::new(),
            exit_code: out.exit_code,
            timed_out: out.timed_out,
            duration_ms: out.duration_ms,
            stdout: out.stdout.clone(),
            stderr: out.stderr.clone(),
            cmd: out.cmd.clone(),
        }
    }
    fn from_output(kind: &str, language: &str, target: &str, out: &CmdOutput) -> Self {
        if out.passed() {
            return Self::passed(kind, language, target, out);
        }
        let reason = if let Some(e) = &out.spawn_error {
            format!("命令无法启动（{}）", e)
        } else if out.timed_out {
            format!("超时被强杀（>{} ms）", out.duration_ms)
        } else {
            format!("退出码 {:?}", out.exit_code)
        };
        Self {
            kind: kind.into(),
            target: target.into(),
            language: language.into(),
            status: "failed".into(),
            reason,
            exit_code: out.exit_code,
            timed_out: out.timed_out,
            duration_ms: out.duration_ms,
            stdout: out.stdout.clone(),
            stderr: out.stderr.clone(),
            cmd: out.cmd.clone(),
        }
    }
    fn skipped(kind: &str, language: &str, target: &str, reason: &str) -> Self {
        Self {
            kind: kind.into(),
            target: target.into(),
            language: language.into(),
            status: "skipped".into(),
            reason: reason.into(),
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct VerifyReport {
    pub run_id: String,
    pub dir: String,
    pub languages: Vec<String>,
    pub file_count: usize,
    pub checks: Vec<CheckResult>,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 一句话结论（UI 顶部直接显示）
    pub verdict: String,
    pub elapsed_ms: u128,
    /// 这一次**到底隔离了没有**。不是布尔值：没隔离必须带原因。
    #[serde(default)]
    pub isolation: Option<sandbox::Isolation>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Python,
    Rust,
    Node,
    Go,
}

impl VerifyReport {
    /// **拒绝执行**时的报告：什么都不跑，并且把原因写在最显眼的位置。
    ///
    /// 这条挡的是最危险的失败模式：沙箱不可用 → 悄悄在宿主机跑 → 看起来一切正常。
    /// 宁可"这次没验证成"，也不要"以为验证了"。
    pub fn refused(run_id: &str, root: &Path, reason: String) -> Self {
        Self {
            run_id: run_id.to_string(),
            dir: root.display().to_string(),
            languages: Vec::new(),
            file_count: 0,
            checks: vec![CheckResult::skipped(
                "sandbox",
                "-",
                "全部检查",
                &format!("沙箱不可用，按 mode=require 拒绝在宿主机执行生成出来的代码：{reason}"),
            )],
            passed: 0,
            failed: 0,
            skipped: 1,
            verdict: format!("未执行任何检查（沙箱不可用）：{reason}"),
            elapsed_ms: 0,
            isolation: Some(sandbox::Isolation::Unsandboxed { reason }),
        }
    }
}

impl Lang {
    pub fn name(&self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::Rust => "rust",
            Lang::Node => "javascript",
            Lang::Go => "go",
        }
    }
}

/// 按"工程标志文件优先"的顺序识别语言。一个项目可能同时有 Python 脚本和 package.json，
/// 这里只挑一个主语言做深度检查，避免把验证时间拖到不可控（其余语言只做语法检查）。
pub fn detect_languages(root: &Path, files: &[String]) -> Vec<Lang> {
    let has = |f: &str| files.iter().any(|p| p.eq_ignore_ascii_case(f));
    let ext_count = |e: &str| {
        files
            .iter()
            .filter(|p| p.to_ascii_lowercase().ends_with(e))
            .count()
    };

    let mut langs = Vec::new();
    if has("Cargo.toml") || ext_count(".rs") > 0 {
        langs.push(Lang::Rust);
    }
    if has("package.json") {
        langs.push(Lang::Node);
    }
    let py = ext_count(".py");
    if py > 0 {
        langs.push(Lang::Python);
    }
    if has("go.mod") || ext_count(".go") > 0 {
        langs.push(Lang::Go);
    }
    let _ = root;
    langs
}

fn files_with_ext(files: &[String], ext: &str) -> Vec<String> {
    files
        .iter()
        .filter(|p| p.to_ascii_lowercase().ends_with(ext))
        .cloned()
        .collect()
}

/// "主语言"：决定跑谁的单测。优先顺序 Rust > Node > Python > Go
/// （有 Cargo.toml / package.json 通常意味着这是一个"工程"，其测试更有意义）。
fn primary(langs: &[Lang]) -> Option<Lang> {
    [Lang::Rust, Lang::Node, Lang::Python, Lang::Go]
        .into_iter()
        .find(|l| langs.contains(l))
}

// ---------------------------------------------------------------- 隔离出口

/// **所有检查都从这里出去** —— 隔离状态是一处决定的。
///
/// 如果每个检查点各自决定"要不要进容器"，就一定会漏掉某一个，
/// 而漏掉的那一个正好是没隔离的那一个。
fn run_check(
    plan: &sandbox::Plan,
    seq: &AtomicUsize,
    cwd: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
    envs: &[(&str, &str)],
) -> CmdOutput {
    run_check_capped(
        plan,
        seq,
        cwd,
        program,
        args,
        timeout,
        envs,
        exec::DEFAULT_STDOUT_CAP,
    )
}

// 沙箱计划 + 定位 + 命令 + 输出上限，缺一不可；拆结构体反而掩盖调用形状
#[allow(clippy::too_many_arguments)]
fn run_check_capped(
    plan: &sandbox::Plan,
    seq: &AtomicUsize,
    cwd: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
    envs: &[(&str, &str)],
    cap: usize,
) -> CmdOutput {
    // 记一条子进程 span：命令行、退出码、耗时、以及**这一次到底隔离了没有**
    let t0 = crate::observe::current().map(|t| t.now()).unwrap_or(0);
    let label = format!("{} {}", program, args.join(" "));
    let started = std::time::Instant::now();
    let out = run_check_inner(plan, seq, cwd, program, args, timeout, envs, cap);
    crate::observe::span_once(
        "cmd",
        &label,
        if out.timed_out {
            "error"
        } else if out.passed() {
            "ok"
        } else {
            "error"
        },
        t0,
        crate::observe::attrs(&[
            (
                "exit",
                &out.exit_code
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "无".into()),
            ),
            ("elapsed_ms", &started.elapsed().as_millis().to_string()),
            (
                "isolated",
                if plan.isolation.is_sandboxed() {
                    "yes"
                } else {
                    "no"
                },
            ),
            ("timed_out", if out.timed_out { "true" } else { "false" }),
            ("stdout_head", &out.stdout),
            ("stderr_head", &out.stderr),
        ]),
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn run_check_inner(
    plan: &sandbox::Plan,
    seq: &AtomicUsize,
    cwd: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
    envs: &[(&str, &str)],
    cap: usize,
) -> CmdOutput {
    if !plan.sandboxed {
        return exec::run_with_cap(cwd, program, args, timeout, envs, cap);
    }
    // 容器里跑：约定项目根挂在 /work，因此**相对路径语义与宿主一致**，
    // 检查命令不用为沙箱改写（改写就意味着两边跑的不是同一件事）。
    let name = format!(
        "harness-{}-{}",
        plan.tag,
        seq.fetch_add(1, Ordering::SeqCst)
    );
    let mut cmd: Vec<String> = vec![program.to_string()];
    cmd.extend(args.iter().map(|s| s.to_string()));
    match sandbox::run_in_sandbox(
        &plan.cfg,
        &plan.mount_root,
        &name,
        &cmd,
        timeout,
        &plan.cancel,
    ) {
        Ok((out, _iso)) => out,
        Err(e) => {
            // 起不来容器不算"检查失败"，算"这个检查没跑成" —— 用 spawn_error 表达
            CmdOutput {
                cmd: format!("[sandbox] {e}"),
                exit_code: None,
                timed_out: false,
                duration_ms: 0,
                stdout: String::new(),
                stderr: e.clone(),
                spawn_error: Some(e),
            }
        }
    }
}

pub fn run(app_cfg: &AppConfig, run_id: &str, root: &Path, flag: &CancelFlag) -> VerifyReport {
    // 隔离方案先定一次：拒绝就不再往下跑任何检查。
    let plan = match sandbox::plan_with(app_cfg, root, run_id, flag.clone()) {
        Ok(p) => p,
        Err(reason) => return VerifyReport::refused(run_id, root, reason),
    };
    let seq = AtomicUsize::new(0);
    let v = &app_cfg.verify;
    let started = std::time::Instant::now();
    let files = crate::generate::file_listing(root);
    let langs = detect_languages(root, &files);

    let mut report = VerifyReport {
        run_id: run_id.to_string(),
        dir: root.to_string_lossy().to_string(),
        languages: langs.iter().map(|l| l.name().to_string()).collect(),
        file_count: files.len(),
        // **必须带上**：报告要能回答"这一次到底隔离了没有"。
        // （这条曾经漏了 —— 正常路径不写 isolation，于是只有"拒绝"时才带状态，
        //   等于最难核对的那种情况反而没有记录。被单测抓到。）
        isolation: Some(plan.isolation.clone()),
        ..Default::default()
    };

    if files.is_empty() {
        report.checks.push(CheckResult::skipped(
            "syntax",
            "-",
            "项目文件",
            "运行目录里没有任何文件，无法验证",
        ));
        report.finish(started);
        return report;
    }

    for lang in &langs {
        if is_cancelled(flag) {
            report.checks.push(CheckResult::skipped(
                "syntax",
                lang.name(),
                "（剩余检查）",
                "用户取消",
            ));
            break;
        }
        report
            .checks
            .extend(syntax_checks(&plan, &seq, *lang, v, root, &files));
    }

    if let Some(p) = primary(&langs) {
        if !is_cancelled(flag) {
            report.checks.extend(test_checks(
                &plan,
                &seq,
                p,
                v,
                root,
                &files,
                &report.checks,
                app_cfg,
            ));
        }
    } else {
        report.checks.push(CheckResult::skipped(
            "test",
            "-",
            "单元测试",
            "未识别出可运行测试的语言（支持 python / rust / javascript / go）",
        ));
    }

    report.finish(started);
    report
}

impl VerifyReport {
    fn finish(&mut self, started: std::time::Instant) {
        self.passed = self.checks.iter().filter(|c| c.status == "passed").count();
        self.failed = self.checks.iter().filter(|c| c.status == "failed").count();
        self.skipped = self.checks.iter().filter(|c| c.status == "skipped").count();
        self.elapsed_ms = started.elapsed().as_millis();

        let tests_passed = self
            .checks
            .iter()
            .filter(|c| c.kind == "test" && c.status == "passed")
            .count();
        self.verdict = if self.failed > 0 {
            let first = self
                .checks
                .iter()
                .find(|c| c.status == "failed")
                .map(|c| format!("{} [{}]", c.target, c.reason))
                .unwrap_or_default();
            format!("未通过：{} 项失败（首个：{}）", self.failed, first)
        } else if tests_passed > 0 {
            format!(
                "通过：语法检查 {} 项全过，单元测试已执行并通过",
                self.passed - tests_passed
            )
        } else if self.passed > 0 {
            "部分通过：语法检查通过，但未执行到单元测试（见跳过原因）".to_string()
        } else {
            "未验证：没有找到可执行的检查项".to_string()
        };
    }
}

fn duration(v: &VerifyConfig, is_test: bool) -> Duration {
    Duration::from_secs(if is_test {
        v.test_timeout_secs
    } else {
        v.cmd_timeout_secs
    })
}

fn syntax_checks(
    plan: &sandbox::Plan,
    seq: &AtomicUsize,
    lang: Lang,
    v: &VerifyConfig,
    root: &Path,
    files: &[String],
) -> Vec<CheckResult> {
    let mut out = Vec::new();
    match lang {
        Lang::Python => {
            let py = files_with_ext(files, ".py");
            if py.is_empty() {
                out.push(CheckResult::skipped(
                    "syntax",
                    "python",
                    "*.py",
                    "没有 Python 文件",
                ));
                return out;
            }
            for f in py {
                // py_compile 比 `python -c "import x"` 安全：不执行模块顶层代码
                let o = run_check(
                    plan,
                    seq,
                    root,
                    &v.python_bin,
                    &["-m", "py_compile", &f],
                    duration(v, false),
                    &[("PYTHONIOENCODING", "utf-8")],
                );
                out.push(CheckResult::from_output("syntax", "python", &f, &o));
            }
        }
        Lang::Rust => {
            if root.join("Cargo.toml").exists() {
                let o = run_check(
                    plan,
                    seq,
                    root,
                    &v.cargo_bin,
                    &["check", "--message-format=short", "--color", "never"],
                    duration(v, true),
                    // 把构建产物约束在运行目录内，别污染全局 target
                    // （.ruyix 是 IDE 自己的命名空间：暂存/备份/验证产物都在这，仓库 .gitignore 里已排除）
                    &[
                        ("CARGO_TERM_COLOR", "never"),
                        ("CARGO_TARGET_DIR", ".ruyix/target-verify"),
                    ],
                );
                out.push(CheckResult::from_output(
                    "syntax",
                    "rust",
                    "cargo check（整包）",
                    &o,
                ));
            } else {
                let rs = files_with_ext(files, ".rs");
                if rs.is_empty() {
                    out.push(CheckResult::skipped(
                        "syntax",
                        "rust",
                        "*.rs",
                        "没有 Rust 文件",
                    ));
                }
                for f in rs {
                    // 没有 Cargo.toml 就用 rustc 单文件语法+类型检查（不链接）
                    let o = run_check(
                        plan,
                        seq,
                        root,
                        "rustc",
                        &[
                            "--edition",
                            "2021",
                            "--crate-type",
                            "lib",
                            "--emit=metadata",
                            "--out-dir",
                            ".ruyix/out-verify",
                            "-A",
                            "warnings",
                            &f,
                        ],
                        duration(v, false),
                        &[],
                    );
                    out.push(CheckResult::from_output("syntax", "rust", &f, &o));
                }
            }
        }
        Lang::Node => {
            let js = files_with_ext(files, ".js");
            if js.is_empty() {
                out.push(CheckResult::skipped(
                    "syntax",
                    "javascript",
                    "*.js",
                    "没有 JS 文件",
                ));
                return out;
            }
            for f in js {
                let o = run_check(
                    plan,
                    seq,
                    root,
                    &v.node_bin,
                    &["--check", &f],
                    duration(v, false),
                    &[],
                );
                out.push(CheckResult::from_output("syntax", "javascript", &f, &o));
            }
        }
        Lang::Go => {
            let go = files_with_ext(files, ".go");
            if go.is_empty() {
                out.push(CheckResult::skipped("syntax", "go", "*.go", "没有 Go 文件"));
                return out;
            }
            for f in go {
                let o = run_check(
                    plan,
                    seq,
                    root,
                    &v.go_bin,
                    &["fmt", "-e", &f],
                    duration(v, false),
                    &[("GOFLAGS", "-mod=mod")],
                );
                out.push(CheckResult::from_output("syntax", "go", &f, &o));
            }
        }
    }
    out
}

/// **暂存内容的"纯语法"检查**（v0.3 机械验证的窄层）。
///
/// 只做**单文件就能判定**的检查：内容写到系统临时目录再跑对应工具，**绝不碰项目磁盘**
/// —— 确认模式"磁盘未动"的不变量靠它守住。
///
/// 判不了的语言显式跳过并写明原因（Rust 必须整包编译：单文件 `rustc` 会因为跨文件引用
/// 和外部 crate 误报）。宁可"这次没验成"，也不要给模型一个假失败。
pub fn staged_syntax_checks(
    v: &VerifyConfig,
    files: &[(String, String)],
    timeout: Duration,
) -> Vec<CheckResult> {
    let mut out = Vec::new();
    if files.is_empty() {
        return out;
    }
    let dir =
        std::env::temp_dir().join(format!("ruyix-syntax-{}", crate::workspace::now_compact()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return vec![CheckResult::skipped(
            "syntax",
            "-",
            "语法检查",
            &format!("临时目录建不出来，本次未检查：{e}"),
        )];
    }
    let mut rust = 0usize;
    let mut other = 0usize;
    for (i, (rel, content)) in files.iter().enumerate() {
        let ext = Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        // 临时文件名带序号：不同目录下的同名文件不能互相覆盖
        let name = format!("{i}-{}", file_name(rel));
        match ext.as_str() {
            "py" | "js" | "mjs" | "cjs" => {
                let tmp = dir.join(&name);
                if let Err(e) = std::fs::write(&tmp, content) {
                    out.push(CheckResult::skipped(
                        "syntax",
                        lang_of(&ext),
                        rel,
                        &format!("临时文件写不进去：{e}"),
                    ));
                    continue;
                }
                let o = if ext == "py" {
                    exec::run(
                        &dir,
                        &v.python_bin,
                        &["-m", "py_compile", &name],
                        timeout,
                        &[("PYTHONIOENCODING", "utf-8")],
                    )
                } else {
                    exec::run(&dir, &v.node_bin, &["--check", &name], timeout, &[])
                };
                out.push(CheckResult::from_output("syntax", lang_of(&ext), rel, &o));
            }
            // 纯数据文件：进程内解析就够，不用起子进程
            "json" => out.push(parse_check(
                "json",
                rel,
                serde_json::from_str::<serde_json::Value>(content)
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
            )),
            "toml" => out.push(parse_check(
                "toml",
                rel,
                toml::from_str::<toml::Value>(content)
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
            )),
            "rs" => rust += 1,
            _ => other += 1,
        }
    }
    if rust > 0 {
        out.push(CheckResult::skipped(
            "syntax",
            "rust",
            "*.rs",
            &format!(
                "{rust} 个 Rust 文件未检查：Rust 必须整包编译（确认模式下不落盘）—— \
                 写入/自主模式会跑全量验证"
            ),
        ));
    }
    if other > 0 {
        out.push(CheckResult::skipped(
            "syntax",
            "-",
            "其它文件",
            &format!("{other} 个文件没有单文件语法检查器（只支持 py / js / json / toml）"),
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn file_name(rel: &str) -> String {
    rel.rsplit(['/', '\\']).next().unwrap_or(rel).to_string()
}

fn lang_of(ext: &str) -> &'static str {
    match ext {
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "json" => "json",
        "toml" => "toml",
        _ => "-",
    }
}

/// 进程内解析结果 → CheckResult（成功/失败都带真实原因，不含猜测）
fn parse_check(kind: &str, target: &str, parsed: Result<(), String>) -> CheckResult {
    match parsed {
        Ok(()) => CheckResult::passed(
            "syntax",
            kind,
            target,
            &CmdOutput {
                cmd: "（内置解析）".into(),
                exit_code: Some(0),
                timed_out: false,
                duration_ms: 0,
                stdout: String::new(),
                stderr: String::new(),
                spawn_error: None,
            },
        ),
        Err(e) => CheckResult::from_output(
            "syntax",
            kind,
            target,
            &CmdOutput {
                cmd: "（内置解析）".into(),
                exit_code: Some(1),
                timed_out: false,
                duration_ms: 0,
                stdout: String::new(),
                stderr: e,
                spawn_error: None,
            },
        ),
    }
}

fn syntax_ok(checks: &[CheckResult], lang: Lang) -> bool {
    !checks
        .iter()
        .any(|c| c.kind == "syntax" && c.language == lang.name() && c.status == "failed")
}

// 与 run_check_* 同形：检查上下文 8 项都是调用方已就绪的事实
#[allow(clippy::too_many_arguments)]
fn test_checks(
    plan: &sandbox::Plan,
    seq: &AtomicUsize,
    lang: Lang,
    v: &VerifyConfig,
    root: &Path,
    files: &[String],
    previous: &[CheckResult],
    app_cfg: &AppConfig,
) -> Vec<CheckResult> {
    let mut out = Vec::new();

    // 前置：语法/编译都没过，跑测试没有意义 —— 但要说清是"因为编译失败而跳过"
    if !syntax_ok(previous, lang) {
        out.push(CheckResult::skipped(
            "test",
            lang.name(),
            "单元测试",
            "语法/编译检查未通过，先修上面的错误",
        ));
        return out;
    }

    match lang {
        Lang::Python => {
            let tests: Vec<String> = files
                .iter()
                .filter(|p| {
                    let name = p.rsplit('/').next().unwrap_or(p);
                    name.starts_with("test_") || name.ends_with("_test.py") || p.contains("tests/")
                })
                .cloned()
                .collect();
            if tests.is_empty() {
                out.push(CheckResult::skipped(
                    "test",
                    "python",
                    "单元测试",
                    "没有找到 test_*.py / *_test.py",
                ));
                return out;
            }
            // pytest 不在就退到 unittest（标准库，一定有）
            let has_pytest = run_check(
                plan,
                seq,
                root,
                &v.python_bin,
                &["-c", "import pytest"],
                Duration::from_secs(30),
                &[("PYTHONIOENCODING", "utf-8")],
            )
            .passed();
            let _ = app_cfg;
            let o = if has_pytest {
                run_check(
                    plan,
                    seq,
                    root,
                    &v.python_bin,
                    &[
                        "-m",
                        "pytest",
                        "-q",
                        "--no-header",
                        "-p",
                        "no:cacheprovider",
                    ],
                    duration(v, true),
                    &[
                        ("PYTHONIOENCODING", "utf-8"),
                        ("PYTHONDONTWRITEBYTECODE", "1"),
                    ],
                )
            } else {
                run_check(
                    plan,
                    seq,
                    root,
                    &v.python_bin,
                    &["-m", "unittest", "discover", "-v"],
                    duration(v, true),
                    &[
                        ("PYTHONIOENCODING", "utf-8"),
                        ("PYTHONDONTWRITEBYTECODE", "1"),
                    ],
                )
            };
            let mut r = CheckResult::from_output(
                "test",
                "python",
                if has_pytest {
                    "pytest（自动发现）"
                } else {
                    "unittest discover（未装 pytest，已降级）"
                },
                &o,
            );
            if !has_pytest {
                r.reason = format!("{}；环境里没有 pytest，用标准库 unittest 代替", r.reason);
            }
            out.push(r);
        }
        Lang::Rust => {
            if !root.join("Cargo.toml").exists() {
                out.push(CheckResult::skipped(
                    "test",
                    "rust",
                    "单元测试",
                    "没有 Cargo.toml，无法 cargo test",
                ));
                return out;
            }
            let o = run_check(
                plan,
                seq,
                root,
                &v.cargo_bin,
                &["test", "--color", "never"],
                duration(v, true),
                &[
                    ("CARGO_TERM_COLOR", "never"),
                    ("CARGO_TARGET_DIR", ".ruyix/target-verify"),
                ],
            );
            out.push(CheckResult::from_output("test", "rust", "cargo test", &o));
        }
        Lang::Node => {
            let pkg = root.join("package.json");
            let has_test_script = std::fs::read_to_string(&pkg)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| v.get("scripts").and_then(|s| s.get("test")).map(|_| true))
                .unwrap_or(false);
            let test_files: Vec<String> = files
                .iter()
                .filter(|p| p.contains(".test.js") || p.contains(".spec.js") || p.contains("test/"))
                .cloned()
                .collect();
            if has_test_script {
                let o = run_check(
                    plan,
                    seq,
                    root,
                    "npm",
                    &["test", "--silent"],
                    duration(v, true),
                    &[],
                );
                out.push(CheckResult::from_output(
                    "test",
                    "javascript",
                    "npm test",
                    &o,
                ));
            } else if !test_files.is_empty() {
                let o = run_check(
                    plan,
                    seq,
                    root,
                    &v.node_bin,
                    &["--test"],
                    duration(v, true),
                    &[],
                );
                out.push(CheckResult::from_output(
                    "test",
                    "javascript",
                    "node --test",
                    &o,
                ));
            } else {
                out.push(CheckResult::skipped(
                    "test",
                    "javascript",
                    "单元测试",
                    "package.json 里没有 test 脚本，也没有 .test.js",
                ));
            }
        }
        Lang::Go => {
            if !root.join("go.mod").exists() {
                out.push(CheckResult::skipped(
                    "test",
                    "go",
                    "单元测试",
                    "没有 go.mod",
                ));
                return out;
            }
            let o = run_check(
                plan,
                seq,
                root,
                &v.go_bin,
                &["test", "./..."],
                duration(v, true),
                &[],
            );
            out.push(CheckResult::from_output("test", "go", "go test ./...", &o));
        }
    }
    out
}

/// 供 UI 展示的"这条检查该看什么"提示（main.rs 在日志里用它出提示语）
pub fn hint_for(check: &CheckResult) -> String {
    match (check.kind.as_str(), check.status.as_str()) {
        ("syntax", "failed") => "语法/编译错误，先改这个再谈测试".into(),
        ("test", "failed") => "测试失败：看 stdout 里的断言与 traceback".into(),
        (_, "skipped") => check.reason.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 常规单测验的是**验证逻辑本身**（命令怎么拼、结论怎么算），
    /// 不该依赖 Docker：机器上没有 Docker、或探针抖一下，都会让它们从
    /// "测逻辑"变成"测环境"（真机踩到过：3 个测试随 daemon 抖动时红时绿）。
    /// 隔离本身有专门的活体测试（`sandbox::tests::live_*` 与 `checks_are_actually_*`）。
    fn dev_cfg() -> AppConfig {
        let mut c = AppConfig::default();
        c.sandbox.mode = "off".into();
        c
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "dh-harness-verify-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }

        fn write(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(name), body).unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// **防回归**：曾经把"接线脚本写好了但忘了执行"这种事混过去 ——
    /// 测试照样通过，因为验证在宿主机上也是通的。
    /// 所以这里不测"能跑通"，而是断言**证据**：命令行里必须有 docker，
    /// 隔离状态必须是 Sandboxed。
    #[test]
    #[ignore = "需要 Docker（真起容器）"]
    fn checks_are_actually_executed_inside_the_container() {
        let d = TempDir::new("insbx");
        d.write("m.py", "def add(a, b):\n    return a + b\n");
        d.write(
            "test_m.py",
            "from m import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n",
        );
        let flag: CancelFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = run(&dev_cfg(), "t-sbx", &d.0, &flag);

        let iso = r.isolation.clone().expect("报告必须带隔离状态");
        assert!(iso.is_sandboxed(), "{}", iso.label());
        assert!(iso.label().contains("docker"), "{}", iso.label());

        let c = r
            .checks
            .iter()
            .find(|c| c.status == "passed")
            .unwrap_or_else(|| panic!("至少应有一条检查通过：{:?}", r.checks));
        println!(
            "容器里的检查命令：
  {}",
            c.cmd
        );
        println!("隔离状态：{}", iso.label());
        for must in ["docker", "run", "--read-only", "--network", "--pids-limit"] {
            assert!(c.cmd.contains(must), "检查命令里缺少 {must}：{}", c.cmd);
        }
        assert!(r.verdict.contains("通过"), "{}", r.verdict);
    }

    /// **核心不变量**：沙箱不可用时，mode=require 必须**拒绝执行**，
    /// 而不是悄悄退回宿主机。
    ///
    /// 这个项目在宿主机上本来是能通过的 —— 所以"passed == 0"证明的是
    /// **我们拒绝了执行**，而不是"检查失败了"。
    #[test]
    fn unusable_sandbox_refuses_instead_of_running_on_the_host() {
        let d = TempDir::new("refuse");
        d.write("m.py", "def add(a, b):\n    return a + b\n");
        d.write(
            "test_m.py",
            "from m import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n",
        );
        let mut cfg = AppConfig::default();
        cfg.sandbox.mode = "require".into();
        // 指向一个不存在的容器引擎 → 探针必然失败
        cfg.sandbox.engine = "definitely-not-a-container-engine".into();

        let flag: CancelFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = run(&cfg, "t-refuse", &d.0, &flag);

        assert_eq!(r.passed, 0, "一条都不该跑：{:?}", r.checks);
        assert!(
            r.checks.iter().all(|c| c.status != "passed"),
            "绝不允许任何检查被标成通过：{:?}",
            r.checks
        );
        assert!(r.verdict.contains("未执行任何检查"), "{}", r.verdict);
        assert!(
            !r.isolation.as_ref().unwrap().is_sandboxed(),
            "必须显式标为未隔离"
        );
    }

    /// mode=prefer 时降级是允许的，但**必须被标注出来**（不能悄悄降级）。
    #[test]
    fn prefer_mode_labels_the_downgrade_instead_of_hiding_it() {
        let d = TempDir::new("prefer");
        d.write("m.py", "x = 1\n");
        let mut cfg = AppConfig::default();
        cfg.sandbox.mode = "prefer".into();
        cfg.sandbox.engine = "definitely-not-a-container-engine".into();

        let flag: CancelFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = run(&cfg, "t-prefer", &d.0, &flag);
        let iso = r.isolation.clone().expect("必须带隔离状态");
        assert!(!iso.is_sandboxed());
        assert!(iso.label().contains("未隔离"), "{}", iso.label());
        assert!(
            iso.label().contains("prefer"),
            "降级原因要能看出来：{}",
            iso.label()
        );
    }

    #[test]
    fn detects_languages_from_project_markers() {
        let files = vec!["Cargo.toml".to_string(), "src/main.rs".to_string()];
        assert_eq!(detect_languages(Path::new("."), &files), vec![Lang::Rust]);

        let files = vec!["package.json".to_string(), "index.js".to_string()];
        assert_eq!(detect_languages(Path::new("."), &files), vec![Lang::Node]);

        let files = vec!["main.py".to_string()];
        assert_eq!(detect_languages(Path::new("."), &files), vec![Lang::Python]);
    }

    #[test]
    fn empty_project_yields_skip_not_error() {
        let d = TempDir::new("empty");
        let cfg = dev_cfg();
        let r = run(&cfg, "run-x", &d.0, &crate::exec::new_cancel_flag());
        assert_eq!(r.failed, 0);
        assert!(r.skipped >= 1);
        assert!(r.checks.iter().any(|c| c.status == "skipped"));
    }

    #[test]
    fn python_syntax_and_tests_pass_on_good_project() {
        let d = TempDir::new("py-ok");
        let proj = d.0.join("project");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
        std::fs::write(
            proj.join("test_calc.py"),
            "import unittest\nfrom calc import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n    def test_negative(self):\n        self.assertEqual(add(-1, -1), -2)\n\nif __name__ == '__main__':\n    unittest.main()\n",
        )
        .unwrap();

        let cfg = dev_cfg();
        let r = run(&cfg, "run-ok", &proj, &crate::exec::new_cancel_flag());
        assert_eq!(r.failed, 0, "不应有失败项: {:#?}", r.checks);
        assert!(
            r.checks
                .iter()
                .any(|c| c.kind == "test" && c.status == "passed"),
            "单元测试必须真的跑起来并通过: {:#?}",
            r.checks
        );
        assert!(r.verdict.contains("通过"), "{}", r.verdict);
    }

    #[test]
    fn python_syntax_error_is_reported_with_real_command() {
        let d = TempDir::new("py-bad");
        let proj = d.0.join("project");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("broken.py"), "def f(:\n    pass\n").unwrap();

        let cfg = dev_cfg();
        let r = run(&cfg, "run-bad", &proj, &crate::exec::new_cancel_flag());
        assert_eq!(r.failed, 1, "应当恰好一个语法检查失败: {:#?}", r.checks);
        let c = r.checks.iter().find(|c| c.status == "failed").unwrap();
        assert!(c.cmd.contains("py_compile"), "命令要回显给用户: {}", c.cmd);
        assert!(!c.stderr.is_empty(), "要有真实 stderr");
        // 编译没过 → 测试必须标 skipped，不能假装跑过
        assert!(
            r.checks
                .iter()
                .any(|c| c.kind == "test" && c.status == "skipped"),
            "语法失败后测试应跳过: {:#?}",
            r.checks
        );
        assert!(r.verdict.starts_with("未通过"));
    }

    #[test]
    fn failing_test_is_reported_as_failure() {
        let d = TempDir::new("py-fail");
        let proj = d.0.join("project");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(
            proj.join("test_calc.py"),
            "import unittest\nfrom calc import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == '__main__':\n    unittest.main()\n",
        )
        .unwrap();

        let cfg = dev_cfg();
        let r = run(&cfg, "run-fail", &proj, &crate::exec::new_cancel_flag());
        assert!(r.failed >= 1);
        let t = r.checks.iter().find(|c| c.kind == "test").unwrap();
        assert_eq!(t.status, "failed");
        assert!(t.stdout.contains("FAILED") || t.stderr.contains("FAILED") || !t.stdout.is_empty());
    }

    /// 窄层（确认模式专用）：单文件语法检查作用在**内容**上，不碰项目磁盘。
    /// 三条判据：好文件过、坏文件挂、判不了的语言显式跳过（绝不假失败）。
    #[test]
    fn staged_syntax_checks_flags_broken_code_without_touching_the_project() {
        let v = VerifyConfig::default();
        let files = vec![
            ("ok.py".to_string(), "x = 1\n".to_string()),
            ("bad.py".to_string(), "def f(:\n".to_string()),
            ("a.rs".to_string(), "fn main() {}\n".to_string()),
            ("b.json".to_string(), "{\"a\": 1}".to_string()),
            ("c.json".to_string(), "{oops}".to_string()),
        ];
        let checks = staged_syntax_checks(&v, &files, Duration::from_secs(30));
        let status = |t: &str| {
            checks
                .iter()
                .find(|c| c.target == t)
                .map(|c| c.status.clone())
                .unwrap_or_else(|| format!("缺 {t}"))
        };
        assert_eq!(status("ok.py"), "passed");
        assert_eq!(status("bad.py"), "failed");
        assert_eq!(status("b.json"), "passed");
        assert_eq!(status("c.json"), "failed");
        // Rust 必须整包编译：单文件 rustc 会因为跨文件/外部 crate 误报 → 显式跳过并写原因
        let rust = checks.iter().find(|c| c.language == "rust").unwrap();
        assert_eq!(rust.status, "skipped");
        assert!(rust.reason.contains("整包编译"), "{}", rust.reason);
    }
}
