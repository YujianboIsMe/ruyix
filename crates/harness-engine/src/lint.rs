//! lint 阶段：调 harness-lint 拿结构化诊断，并把它渲染成"能直接驱动修复的 Prompt"。
//!
//! 这一层的关键不是"跑个命令"，而是 **诊断文本的二次加工**：
//! harness-lint 的 JSON 里每条诊断自带 `为什么 / 怎么改 / 参考文档 / 建议替换`，
//! 所以这里不需要再补任何规则知识——`to_prompt_text()` 出来的东西直接就是给模型看的
//! 修复指令。这正是"错误信息本身就应该是一段 Prompt"的落地方式。

use crate::config::AppConfig;
use crate::exec;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct LintTextLine {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub highlight_start: u32,
    #[serde(default)]
    pub highlight_end: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct LintSpan {
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub line_start: u32,
    #[serde(default)]
    pub column_start: u32,
    #[serde(default)]
    pub line_end: u32,
    #[serde(default)]
    pub column_end: u32,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub suggested_replacement: Option<String>,
    #[serde(default)]
    pub suggestion_applicability: String,
    #[serde(default)]
    pub text: Vec<LintTextLine>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct LintChild {
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct LintDiagnostic {
    pub rule: String,
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub spans: Vec<LintSpan>,
    #[serde(default)]
    pub children: Vec<LintChild>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub doc_resolved: Option<String>,
    #[serde(default)]
    pub fixable: bool,
    #[serde(default)]
    pub fix_blocked_reason: Option<String>,
}

impl LintDiagnostic {
    pub fn file(&self) -> String {
        self.spans
            .first()
            .map(|s| s.file_name.clone())
            .unwrap_or_else(|| "<project>".into())
    }

    pub fn line(&self) -> u32 {
        self.spans.first().map(|s| s.line_start).unwrap_or(0)
    }

    fn child(&self, level: &str) -> Option<&str> {
        self.children
            .iter()
            .find(|c| c.level == level)
            .map(|c| c.message.as_str())
    }

    /// 把一条诊断渲染成"自包含"的修复指令。
    ///
    /// 自包含的意思是：**只给这段文本，不给规则文档、不给 linter 源码，也应该能改对**。
    /// 所以四段齐全（违反了什么 / 源码位置 / 为什么 / 怎么改）+ 参考文档路径。
    pub fn to_prompt_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "[{}] {} {}:{} — {}\n",
            self.level.to_uppercase(),
            self.rule,
            self.file(),
            self.line(),
            self.message
        ));
        if let Some(span) = self.spans.first() {
            if !span.text.is_empty() {
                out.push_str("  相关源码：\n");
                for (i, tl) in span.text.iter().enumerate() {
                    let no = span.line_start as i64 - 1 + i as i64;
                    let marker = if no as u32 == span.line_start {
                        ">>"
                    } else {
                        "  "
                    };
                    out.push_str(&format!("    {} {:>4} | {}\n", marker, no.max(1), tl.text));
                }
            }
            if let Some(rep) = &span.suggested_replacement {
                out.push_str(&format!(
                    "  位置：{}:{} 第 {}–{} 列（0 基右开区间可据此定位）\n",
                    span.file_name, span.line_start, span.column_start, span.column_end
                ));
                out.push_str(&format!(
                    "  建议替换（applicability={}）：{:?}\n",
                    span.suggestion_applicability, rep
                ));
            }
        }
        if let Some(why) = self.child("note") {
            out.push_str(&format!("  为什么：{why}\n"));
        }
        if let Some(how) = self.child("help") {
            out.push_str(&format!("  怎么改：{how}\n"));
        }
        if let Some(doc) = &self.doc {
            match &self.doc_resolved {
                Some(path) => out.push_str(&format!("  参考：{doc}（{path}）\n")),
                None => out.push_str(&format!("  参考：{doc}\n")),
            }
        }
        if !self.fixable
            && let Some(reason) = &self.fix_blocked_reason
        {
            out.push_str(&format!("  不能自动修的原因：{reason}\n"));
        }
        out
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct LintReport {
    pub root: String,
    #[serde(default)]
    pub files_scanned: u32,
    #[serde(default)]
    pub files_skipped: u32,
    #[serde(default)]
    pub test_count: u32,
    #[serde(default)]
    pub counts: std::collections::BTreeMap<String, u32>,
    #[serde(default)]
    pub by_rule: std::collections::BTreeMap<String, u32>,
    #[serde(default)]
    pub diagnostics: Vec<LintDiagnostic>,
    #[serde(default)]
    pub suppressions: serde_json::Value,
    #[serde(default)]
    pub ok: bool,
}

impl LintReport {
    pub fn errors(&self) -> u32 {
        self.counts.get("error").copied().unwrap_or(0)
    }

    pub fn warnings(&self) -> u32 {
        self.counts.get("warning").copied().unwrap_or(0)
    }

    pub fn total(&self) -> u32 {
        self.diagnostics.len() as u32
    }

    pub fn summary_line(&self) -> String {
        format!(
            "error {} / warning {}（{} 个文件，测试用例 {}）",
            self.errors(),
            self.warnings(),
            self.files_scanned,
            self.test_count
        )
    }

    /// 组装给模型的"诊断包"：**只有诊断 + 被点到的文件内容**，不含规则文档与 linter 源码。
    ///
    /// 刻意只给诊断点到的文件（而不是整个项目）：既省 token，也让"只凭报错能不能改对"
    /// 这件事保持可证伪——加了别处代码就分不清是诊断起作用还是上下文起作用。
    pub fn prompt_package(&self, project: &Path, budget: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "共 {} 条违规：error {} / warning {}\n\n",
            self.total(),
            self.errors(),
            self.warnings()
        ));

        let mut used = 0usize;
        let mut files_seen: Vec<String> = Vec::new();

        for d in &self.diagnostics {
            let block = d.to_prompt_text();
            if used + block.len() > budget {
                out.push_str(&format!(
                    "\n…（已达上下文预算 {} 字符，其余 {} 条诊断从略；优先修上面的）\n",
                    budget,
                    self.total() as usize - files_seen.len()
                ));
                break;
            }
            used += block.len();
            out.push_str(&block);
            out.push('\n');
            let f = d.file();
            if !files_seen.contains(&f) {
                files_seen.push(f);
            }
        }

        out.push_str("\n=== 相关文件当前内容（用于生成精确替换） ===\n");
        for rel in files_seen {
            let path = project.join(&rel);
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let clipped = exec::clip(&text, 6000);
                    out.push_str(&format!("\n--- {rel} ---\n{clipped}\n"));
                }
                Err(e) => out.push_str(&format!("\n--- {rel} ---\n（读取失败：{e}）\n")),
            }
        }
        out
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct LintOutcome {
    /// 命令本身是否跑起来并返回了可解析的 JSON
    pub ran: bool,
    pub exit_code: Option<i32>,
    pub cmd: String,
    pub report: Option<LintReport>,
    pub parse_error: Option<String>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub duration_ms: u128,
}

pub fn lint_argv(cfg: &AppConfig, project: &Path) -> Vec<String> {
    let l = &cfg.lint;
    let mut args = vec![
        "-m".to_string(),
        "harness_lint".to_string(),
        "--json".to_string(),
        "--max-suppressions".to_string(),
        l.max_suppressions.to_string(),
    ];
    if l.strict {
        args.push("--strict".to_string());
    }
    args.extend(l.extra_args.iter().cloned());
    args.push(project.to_string_lossy().to_string());
    args
}

/// 跑一遍外部 linter。**命令跑不起来 / 输出不是 JSON 都要如实上报**，
/// 不能把"没跑成"渲染成"没问题"（那正是本项目的核心纪律）。
pub fn run_lint(project: &Path, cfg: &AppConfig) -> LintOutcome {
    let args = lint_argv(cfg, project);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let pkg = cfg.lint.package_dir.clone();

    let out = exec::run_with_cap(
        project,
        &cfg.verify.python_bin,
        &arg_refs,
        Duration::from_secs(cfg.verify.cmd_timeout_secs.max(60)),
        &[
            ("PYTHONPATH", pkg.as_str()),
            ("PYTHONIOENCODING", "utf-8"),
            ("PYTHONDONTWRITEBYTECODE", "1"),
        ],
        1 << 20,
    );

    let mut outcome = LintOutcome {
        ran: false,
        exit_code: out.exit_code,
        cmd: out.cmd.clone(),
        stdout_tail: out.stdout.clone(),
        stderr_tail: out.stderr.clone(),
        duration_ms: out.duration_ms,
        ..Default::default()
    };

    if let Some(e) = &out.spawn_error {
        outcome.parse_error = Some(format!(
            "lint 命令无法启动：{e}（检查 lint.package_dir 与 python 配置）"
        ));
        return outcome;
    }
    // 输出被截断时必须说清楚是"截断"，不能报成"JSON 解析失败"误导排查方向
    if out.stdout.contains("[省略") && !out.stdout.trim_end().ends_with('}') {
        outcome.parse_error = Some(format!(
            "lint 输出超过捕获上限被截断（{} 字节），无法解析。请调大 max_stdout 或缩小检查范围",
            out.stdout.len()
        ));
        return outcome;
    }
    if out.stdout.trim().is_empty() {
        outcome.parse_error = Some(format!(
            "lint 没有输出 JSON（exit={:?}）：{}",
            out.exit_code,
            crate::exec::clip(out.stderr.trim(), 400)
        ));
        return outcome;
    }

    match serde_json::from_str::<LintReport>(out.stdout.trim()) {
        Ok(report) => {
            outcome.ran = true;
            outcome.report = Some(report);
        }
        Err(e) => {
            outcome.parse_error = Some(format!(
                "lint JSON 解析失败：{e}；输出片段：{}",
                crate::exec::clip(out.stdout.trim(), 300)
            ));
        }
    }
    outcome
}

/// 供 `--list-rules` 用：把规则表拉出来（UI 里展示"这个项目在管哪些约定"）
pub fn list_rules(cfg: &AppConfig) -> Result<serde_json::Value, String> {
    let args = ["-m", "harness_lint", "--list-rules"];
    let out = exec::run(
        Path::new(&cfg.lint.package_dir),
        &cfg.verify.python_bin,
        &args,
        Duration::from_secs(60),
        &[
            ("PYTHONPATH", cfg.lint.package_dir.as_str()),
            ("PYTHONIOENCODING", "utf-8"),
        ],
    );
    if !out.passed() {
        return Err(format!(
            "列举规则失败：{}",
            crate::exec::clip(&out.stderr, 300)
        ));
    }
    serde_json::from_str(&out.stdout).map_err(|e| format!("规则表解析失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LintDiagnostic {
        LintDiagnostic {
            rule: "HX201".into(),
            level: "error".into(),
            message: "使用了裸 except".into(),
            spans: vec![LintSpan {
                file_name: "service/a.py".into(),
                line_start: 17,
                column_start: 5,
                line_end: 19,
                column_end: 1,
                label: "异常处理".into(),
                suggested_replacement: Some("is None".into()),
                suggestion_applicability: "MachineApplicable".into(),
                text: vec![LintTextLine {
                    text: "    except:".into(),
                    highlight_start: 5,
                    highlight_end: 11,
                }],
            }],
            children: vec![
                LintChild {
                    level: "note".into(),
                    message: "裸 except 会吞掉 KeyboardInterrupt".into(),
                },
                LintChild {
                    level: "help".into(),
                    message: "捕获具体异常类型并记日志".into(),
                },
            ],
            doc: Some("docs/conventions/error-handling.md".into()),
            doc_resolved: Some("/abs/error-handling.md".into()),
            fixable: false,
            fix_blocked_reason: Some("需要人决定捕获哪些异常".into()),
        }
    }

    #[test]
    fn prompt_text_is_self_contained() {
        let t = sample().to_prompt_text();
        // 四段齐全：违反了什么 / 源码 / 为什么 / 怎么改 / 参考
        assert!(t.contains("HX201"), "{t}");
        assert!(t.contains("service/a.py:17"), "{t}");
        assert!(t.contains("except:"), "{t}");
        assert!(t.contains("为什么："), "{t}");
        assert!(t.contains("怎么改："), "{t}");
        assert!(
            t.contains("参考：docs/conventions/error-handling.md"),
            "{t}"
        );
        assert!(t.contains("不能自动修的原因"), "{t}");
    }

    #[test]
    fn prompt_package_excludes_rule_docs_and_includes_sources() {
        let dir = std::env::temp_dir().join(format!("dh-lint-pkg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("service"));
        std::fs::write(
            dir.join("service/a.py"),
            "try:\n    pass\nexcept:\n    pass\n",
        )
        .unwrap();
        let report = LintReport {
            diagnostics: vec![sample()],
            ..Default::default()
        };
        let pkg = report.prompt_package(&dir, 20_000);
        assert!(pkg.contains("相关文件当前内容"), "{pkg}");
        assert!(pkg.contains("except:"), "必须带上被点到的文件内容");
        // 诊断包里不能出现规则实现/约定文档正文——这是隔离实验成立的前提
        assert!(!pkg.contains("class BareExcept"), "不能泄漏规则实现");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_counts_and_summary() {
        let report = LintReport {
            counts: [("error".to_string(), 2u32), ("warning".to_string(), 3u32)]
                .into_iter()
                .collect(),
            diagnostics: vec![sample(); 5],
            files_scanned: 4,
            test_count: 6,
            ..Default::default()
        };
        assert_eq!(report.errors(), 2);
        assert_eq!(report.warnings(), 3);
        assert_eq!(report.total(), 5);
        assert!(report.summary_line().contains("error 2 / warning 3"));
    }

    #[test]
    fn argv_includes_budget_and_extra_args() {
        let mut cfg = AppConfig::default();
        cfg.lint.max_suppressions = 0;
        cfg.lint.strict = true;
        cfg.lint.extra_args = vec!["--conventions-dir".into(), "/tmp/c".into()];
        let argv = lint_argv(&cfg, Path::new("proj"));
        assert!(argv.contains(&"--max-suppressions".to_string()));
        assert!(argv.contains(&"0".to_string()));
        assert!(argv.contains(&"--strict".to_string()));
        assert_eq!(argv.last().unwrap(), "proj");
    }

    /// 回归测试：lint 的 JSON 输出曾经被**人类展示用的 24KB 阈值**截断，
    /// 表现为"JSON 解析失败：expected `,` or `}`"——一个会让人往错误方向排查的报错。
    /// 真机演示里跑出来的，所以在这里钉死：示例项目（10+ 条违规、30KB+ JSON）必须完整解析。
    #[test]
    fn lint_parses_full_report_without_truncation() {
        let cfg = AppConfig::default();
        let demo = Path::new(&cfg.lint.package_dir)
            .join("examples")
            .join("demo_app");
        assert!(
            demo.exists(),
            "找不到示例项目 {}：请确认 lint.package_dir 指向 tools/lint",
            demo.display()
        );
        let out = run_lint(&demo, &cfg);
        assert!(out.ran, "lint 必须能跑起来：{:?}", out.parse_error);
        assert!(
            !out.stdout_tail.contains("[省略"),
            "输出被截断说明上限设小了（机器可读输出不能走人类展示阈值）"
        );
        let report = out.report.expect("应当有报告");
        assert!(
            report.total() >= 10,
            "示例项目应报出 10 条以上违规，实际 {} —— 少报通常意味着输出被截断或规则没跑",
            report.total()
        );
        // JSON 必须能被完整反序列化（含 children / spans 这类嵌套结构）
        let with_children = report
            .diagnostics
            .iter()
            .filter(|d| !d.children.is_empty())
            .count();
        assert!(
            with_children >= 5,
            "诊断应当带 help/note 子项，实际 {with_children} 条有"
        );
    }
}
