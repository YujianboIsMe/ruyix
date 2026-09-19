//! 熵管理的控制侧（控制器 + 执行器）。
//!
//! 上一阶段（lint/repair）是**单次任务内**的闭环；熵管理是**跨时间**的闭环：
//! 没有"任务"这个起点，触发源是"指标退化"。所以本模块的形状完全不同。
//!
//! ## 传感器 / 控制器分离
//!
//! 测量在 `harness_lint --entropy`（确定性、零 token、可复现），本模块只做：
//! 取参考基线 → 判定退化 → 有界选目标 → 修复 → 验证 → 提交 → 开 PR。
//! 阈值全在这边，所以调策略不用动测量代码。
//!
//! ## 五条不可省的纪律
//!
//! 1. **未退化就什么都不做**（静默）。定时任务一旦"没变化也刷消息"，就会被忽略。
//! 2. **有界**：一次最多改 `max_fix_targets` 条、`max_changed_files` 个文件。
//!    定时任务失控的典型方式就是"扫全库 + 让模型放开改"→ token 爆炸。
//! 3. **必须证明改善**：改完重算指标，**质量分没升就整轮放弃**。
//!    宁可这轮不算，也不要让"修复 PR"把代码改差。
//! 4. **隔离**：所有改动发生在 `git worktree` 里（对上理论篇的"Git Worktree 集成"）。
//!    用户的工作区**一个字节都不碰**，放弃时 `worktree remove` 干净退出。
//! 5. **只提议、不合入**：产出的是 PR（待审）。没有 `gh` 就只产 PR 正文并**如实说明**，
//!    绝不谎称"已发起 PR"。

use crate::config::{AppConfig, EntropyConfig};
use crate::exec::{self, CancelFlag};
use crate::lint::{self, LintDiagnostic, LintReport};
use crate::repair;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

// ---------------------------------------------------------------- 指标

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Metrics {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub code_lines: u32,
    #[serde(default)]
    pub violations_total: u32,
    #[serde(default)]
    pub violations_error: u32,
    #[serde(default)]
    pub violations_warning: u32,
    #[serde(default)]
    pub density: f64,
    #[serde(default)]
    pub error_density: f64,
    #[serde(default)]
    pub warning_density: f64,
    #[serde(default)]
    pub suppressions_total: u32,
    #[serde(default)]
    pub suppressions_used: u32,
    #[serde(default)]
    pub suppressions_without_reason: u32,
    #[serde(default)]
    pub suppression_density: f64,
    #[serde(default)]
    pub no_reason_density: f64,
    #[serde(default)]
    pub test_count: u32,
    #[serde(default)]
    pub test_density: f64,
    #[serde(default)]
    pub over_limit_files: u32,
    #[serde(default)]
    pub duplicate_groups: u32,
    #[serde(default)]
    pub duplicate_functions: u32,
    #[serde(default)]
    pub duplicate_lines: u32,
    #[serde(default)]
    pub duplicate_density: f64,
    #[serde(default)]
    pub placeholder_hits: u32,
    #[serde(default)]
    pub placeholder_density: f64,
    #[serde(default)]
    pub max_file_lines: u32,
    #[serde(default)]
    pub file_lines_p50: f64,
    #[serde(default)]
    pub file_lines_p90: f64,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub by_rule: std::collections::BTreeMap<String, u32>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ScanOut {
    pub metrics: Metrics,
    #[serde(default)]
    pub reference: Option<Metrics>,
    /// 窗口内质量分最高的那条（判「离自己最近的最好水平有多远」）
    #[serde(default)]
    pub reference_best: Option<Metrics>,
    #[serde(default)]
    pub history_len: u32,
}

// ---------------------------------------------------------------- 判定

#[derive(Serialize, Clone, Debug)]
pub struct Signal {
    pub kind: String,
    pub detail: String,
    pub delta: f64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Decision {
    /// 是否退化（有任一信号命中）
    pub degraded: bool,
    /// 第一条历史（没有基线可比）——不是"没退化"，是"还不知道"
    pub baseline_only: bool,
    pub signals: Vec<Signal>,
    pub reference_ts: Option<String>,
}

/// 变化百分比，正数 = 上升，负数 = 下降。
/// `from` 为 0 时没有百分比可言：退化为 "0 → 有" 记 100%，"0 → 0" 记 0%。
fn pct_change(from: f64, to: f64) -> f64 {
    if from <= 0.0 {
        return if to > 0.0 { 100.0 } else { 0.0 };
    }
    (to - from) / from * 100.0
}

/// 判定退化。**纯函数**（不碰 IO），所以策略可以单独测。
pub fn evaluate(
    cur: &Metrics,
    reference: Option<&Metrics>,
    reference_best: Option<&Metrics>,
    cfg: &EntropyConfig,
) -> Decision {
    let mut signals = Vec::new();
    let r = match reference {
        Some(r) => r,
        None => {
            return Decision {
                degraded: false,
                baseline_only: true,
                signals,
                reference_ts: None,
            };
        }
    };

    // 与窗口内最好水平对比：防止「永久退化被滚动基线吸收成新常态」。
    // 滚动中位数只能发现**突然**的退化，发现不了**持续**的退化。
    if let Some(b) = reference_best
        && b.score - cur.score >= cfg.score_drop
    {
        signals.push(Signal {
            kind: "score_vs_best".into(),
            detail: format!(
                "质量分 {:.2}，低于窗口内最好水平 {:.2}（差 {:.2}）",
                cur.score,
                b.score,
                b.score - cur.score
            ),
            delta: round2(b.score - cur.score),
        });
    }
    if r.score - cur.score >= cfg.score_drop {
        signals.push(Signal {
            kind: "score".into(),
            detail: format!("质量分 {:.2} → {:.2}", r.score, cur.score),
            delta: round2(r.score - cur.score),
        });
    }
    let d_pct = pct_change(r.density, cur.density);
    if d_pct >= cfg.density_up_pct {
        signals.push(Signal {
            kind: "density".into(),
            detail: format!(
                "违规密度 {:.3}/千行 → {:.3}/千行（+{:.1}%）",
                r.density, cur.density, d_pct
            ),
            delta: round2(d_pct),
        });
    }
    let new_supp = cur.suppressions_total as i64 - r.suppressions_total as i64;
    if new_supp >= cfg.new_suppressions as i64 {
        signals.push(Signal {
            kind: "suppressions".into(),
            detail: format!(
                "豁免数 {} → {}（新增 {}）",
                r.suppressions_total, cur.suppressions_total, new_supp
            ),
            delta: new_supp as f64,
        });
    }
    if cur.suppressions_without_reason > r.suppressions_without_reason {
        signals.push(Signal {
            kind: "suppression_without_reason".into(),
            detail: format!(
                "无理由豁免 {} → {}",
                r.suppressions_without_reason, cur.suppressions_without_reason
            ),
            delta: (cur.suppressions_without_reason - r.suppressions_without_reason) as f64,
        });
    }
    if cur.test_count < r.test_count {
        signals.push(Signal {
            kind: "tests".into(),
            detail: format!("测试用例数 {} → {}（减少）", r.test_count, cur.test_count),
            delta: (r.test_count - cur.test_count) as f64,
        });
    }
    if cur.duplicate_groups > r.duplicate_groups {
        signals.push(Signal {
            kind: "duplicates".into(),
            detail: format!(
                "重复实现 {} → {} 组",
                r.duplicate_groups, cur.duplicate_groups
            ),
            delta: (cur.duplicate_groups - r.duplicate_groups) as f64,
        });
    }

    Decision {
        degraded: !signals.is_empty(),
        baseline_only: false,
        signals,
        reference_ts: Some(r.ts.clone()),
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---------------------------------------------------------------- 有界选目标

/// 挑本轮要修的目标：有界、按严重度排序、限制改动文件数。
pub fn select_targets(report: &LintReport, cfg: &EntropyConfig) -> Vec<LintDiagnostic> {
    let mut cands: Vec<LintDiagnostic> = report
        .diagnostics
        .iter()
        .filter(|d| d.level == "error" || d.level == "warning")
        // HX000/HX998 是"检查没跑成"，属于基础设施问题而不是代码熵，
        // 交给模型改代码解决不了，别浪费预算
        .filter(|d| d.rule != "HX000" && d.rule != "HX998")
        .cloned()
        .collect();

    cands.sort_by_key(|d| {
        (
            if d.level == "error" { 0 } else { 1 },
            // 有机器可应用建议的排前面（改起来最稳）
            if d.fixable { 0 } else { 1 },
            d.rule.clone(),
            d.file(),
            d.line(),
        )
    });

    let mut seen_files: Vec<String> = Vec::new();
    let mut out: Vec<LintDiagnostic> = Vec::new();
    for d in cands {
        let f = d.file();
        if !seen_files.iter().any(|x| x == &f) {
            if seen_files.len() >= cfg.max_changed_files {
                continue; // 这个文件的改动本轮不做
            }
            seen_files.push(f);
        }
        out.push(d);
        if out.len() >= cfg.max_fix_targets {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------- PR 正文

pub fn pr_body(
    repo: &str,
    cur: &Metrics,
    reference: Option<&Metrics>,
    decision: &Decision,
    fixes: &[String],
    verify_verdict: &str,
) -> String {
    let mut b = String::new();
    b.push_str("## 熵管理：定期的偏差清扫\n\n");
    b.push_str(&format!("仓库：`{repo}`\n\n"));
    b.push_str("### 为什么开这个 PR\n\n");
    if decision.signals.is_empty() {
        b.push_str("（无信号）\n\n");
    }
    for s in &decision.signals {
        b.push_str(&format!("- **{}**：{}\n", s.kind, s.detail));
    }
    b.push_str("\n### 指标对比\n\n");
    b.push_str("| 指标 | 参考基线 | 本次 | 变化 |\n|---|---|---|---|\n");
    let rows: Vec<(&str, String, String, String)> = vec![
        (
            "质量分",
            fmt_ref(reference, |m| format!("{:.2}", m.score)),
            format!("{:.2}", cur.score),
            delta_of(reference, cur.score, |m| m.score, true),
        ),
        (
            "违规",
            fmt_ref(reference, |m| m.violations_total.to_string()),
            cur.violations_total.to_string(),
            delta_of(
                reference,
                cur.violations_total as f64,
                |m| m.violations_total as f64,
                false,
            ),
        ),
        (
            "违规密度/千行",
            fmt_ref(reference, |m| format!("{:.3}", m.density)),
            format!("{:.3}", cur.density),
            delta_of(reference, cur.density, |m| m.density, false),
        ),
        (
            "豁免",
            fmt_ref(reference, |m| m.suppressions_total.to_string()),
            cur.suppressions_total.to_string(),
            delta_of(
                reference,
                cur.suppressions_total as f64,
                |m| m.suppressions_total as f64,
                false,
            ),
        ),
        (
            "测试用例",
            fmt_ref(reference, |m| m.test_count.to_string()),
            cur.test_count.to_string(),
            delta_of(
                reference,
                cur.test_count as f64,
                |m| m.test_count as f64,
                true,
            ),
        ),
        (
            "重复实现（组）",
            fmt_ref(reference, |m| m.duplicate_groups.to_string()),
            cur.duplicate_groups.to_string(),
            delta_of(
                reference,
                cur.duplicate_groups as f64,
                |m| m.duplicate_groups as f64,
                false,
            ),
        ),
    ];
    for (name, r, c, d) in rows {
        b.push_str(&format!("| {name} | {r} | {c} | {d} |\n"));
    }
    b.push_str("\n### 本 PR 改了什么\n\n");
    for f in fixes {
        b.push_str(&format!("- {f}\n"));
    }
    b.push_str(&format!("\n### 验证\n\n{verify_verdict}\n"));
    b.push_str(
        "\n---\n\n> 由 `darkhorse-entropy` 自动生成：**只提议，不自动合入**。\n\
         > 请重点确认：改动是否真的修掉了指标上的问题，而不是把问题挪到了别处。\n",
    );
    b
}

fn fmt_ref(reference: Option<&Metrics>, f: impl Fn(&Metrics) -> String) -> String {
    match reference {
        Some(r) => f(r),
        None => "（无基线）".into(),
    }
}

fn delta_of(
    reference: Option<&Metrics>,
    cur: f64,
    get: impl Fn(&Metrics) -> f64,
    higher_is_better: bool,
) -> String {
    let Some(r) = reference else {
        return "—".into();
    };
    let d = round2(cur - get(r));
    if d == 0.0 {
        return "不变".into();
    }
    let good = if higher_is_better { d > 0.0 } else { d < 0.0 };
    format!(
        "{}（{}）",
        if d > 0.0 {
            format!("+{d}")
        } else {
            format!("{d}")
        },
        if good { "改善" } else { "恶化" }
    )
}

// ---------------------------------------------------------------- 结果

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntropyOutcome {
    /// 指标没退化 → 什么都不做（定时任务应当静默）
    NoChange {
        metrics: Metrics,
        decision: Decision,
    },
    /// 第一次跑，只有基线可比（不是"健康"，是"还不知道"）
    BaselineRecorded { metrics: Metrics, history_len: u32 },
    /// 只感知模式：指标退化了，等一个动作（exit 1，便于调度器分流）
    Degraded {
        metrics: Metrics,
        decision: Decision,
    },
    /// 因为配额或环境跳过
    Skipped { reason: String, metrics: Metrics },
    /// 改好了并开出了 PR
    Opened {
        branch: String,
        pr_url: String,
        metrics_before: Metrics,
        metrics_after: Metrics,
        fixes: Vec<String>,
        decision: Decision,
    },
    /// 改好了但没能开 PR（没 gh / 没 remote）—— 如实说明，产出 PR 正文
    Prepared {
        branch: String,
        pr_body_path: String,
        metrics_before: Metrics,
        metrics_after: Metrics,
        fixes: Vec<String>,
        reason: String,
        decision: Decision,
    },
    /// 改完没变好 → 整轮放弃（不留分支、不留改动）
    Abandoned {
        reason: String,
        metrics_before: Metrics,
        metrics_after: Option<Metrics>,
        decision: Decision,
    },
}

impl EntropyOutcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            EntropyOutcome::Opened { .. } | EntropyOutcome::Prepared { .. } => 0,
            EntropyOutcome::NoChange { .. } | EntropyOutcome::BaselineRecorded { .. } => 0,
            EntropyOutcome::Skipped { .. } => 0,
            EntropyOutcome::Degraded { .. } => 1,
            EntropyOutcome::Abandoned { .. } => 3,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            EntropyOutcome::NoChange { metrics, .. } => format!(
                "指标未退化（质量分 {:.2}，违规 {}），不做任何改动",
                metrics.score, metrics.violations_total
            ),
            EntropyOutcome::BaselineRecorded {
                metrics,
                history_len,
            } => format!(
                "第一次运行，已记录基线（质量分 {:.2}，历史 {history_len} 条）；下次才有可比对象",
                metrics.score
            ),
            EntropyOutcome::Skipped { reason, .. } => format!("跳过本轮：{reason}"),
            EntropyOutcome::Degraded { metrics, decision } => {
                let mut s = format!(
                    "指标退化，共 {} 个信号（质量分 {:.2}，违规 {}）：",
                    decision.signals.len(),
                    metrics.score,
                    metrics.violations_total
                );
                for sig in &decision.signals {
                    s.push_str(&format!("\n  - [{}] {}", sig.kind, sig.detail));
                }
                s
            }
            EntropyOutcome::Opened {
                branch,
                pr_url,
                metrics_before,
                metrics_after,
                ..
            } => format!(
                "已开 PR：{pr_url}（分支 {branch}）\n质量分 {:.2} → {:.2}",
                metrics_before.score, metrics_after.score
            ),
            EntropyOutcome::Prepared {
                branch,
                pr_body_path,
                metrics_before,
                metrics_after,
                reason,
                ..
            } => format!(
                "已准备改动（分支 {branch}），但没能开 PR：{reason}\nPR 正文已写到 {pr_body_path}\n质量分 {:.2} → {:.2}",
                metrics_before.score, metrics_after.score
            ),
            EntropyOutcome::Abandoned { reason, .. } => format!("整轮放弃：{reason}"),
        }
    }
}

// ---------------------------------------------------------------- git / gh

fn run_git(cwd: &Path, args: &[&str]) -> exec::CmdOutput {
    let cwd_s = cwd.to_string_lossy().to_string();
    let mut full: Vec<&str> = vec!["-C", &cwd_s];
    full.extend_from_slice(args);
    exec::run_with_cap(cwd, "git", &full, Duration::from_secs(120), &[], 1 << 20)
}

pub fn is_git_repo(repo: &Path) -> bool {
    run_git(repo, &["rev-parse", "--git-dir"]).passed()
}

pub fn has_remote(repo: &Path, remote: &str) -> bool {
    run_git(repo, &["remote", "get-url", remote]).passed()
}

pub fn gh_available() -> bool {
    exec::run_with_cap(
        Path::new("."),
        "gh",
        &["--version"],
        Duration::from_secs(20),
        &[],
        4096,
    )
    .passed()
}

/// 数一数已经开着的、由熵管理发起的 PR。
///
/// **幂等/配额的关键**：同一个问题不该每周开一个新 PR。
/// 拿不到 `gh` 就返回 None（调用方必须如实说明"配额未校验"，不能假装查过了）。
pub fn open_entropy_prs(repo: &Path, cfg: &EntropyConfig) -> Option<usize> {
    if !gh_available() {
        return None;
    }
    let out = exec::run_with_cap(
        repo,
        "gh",
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "headRefName",
        ],
        Duration::from_secs(60),
        &[],
        1 << 20,
    );
    if !out.passed() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(out.stdout.trim()).ok()?;
    let arr = v.as_array()?;
    Some(
        arr.iter()
            .filter(|p| {
                p.get("headRefName")
                    .and_then(|x| x.as_str())
                    .map(|b| b.starts_with(&cfg.branch_prefix))
                    .unwrap_or(false)
            })
            .count(),
    )
}

/// 数远端已有的熵分支 —— 没有 gh 时的配额兜底。
///
/// 这条很重要：配额失效就等于"每周开一个新分支/新 PR"，机制很快会被忽略。
/// `git ls-remote` 不需要任何额外工具，所以**配额在任何机器上都能生效**。
pub fn remote_entropy_branches(repo: &Path, cfg: &EntropyConfig) -> Option<usize> {
    let pattern = format!("refs/heads/{}*", cfg.branch_prefix);
    let out = run_git(repo, &["ls-remote", "--heads", &cfg.remote, &pattern]);
    if !out.passed() {
        return None;
    }
    Some(out.stdout.lines().filter(|l| !l.trim().is_empty()).count())
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    run_git(
        repo,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    )
    .passed()
}

// ---------------------------------------------------------------- 扫描

/// 调测量层拿指标（并把这次快照写进历史）。
pub fn scan(root: &Path, cfg: &AppConfig) -> Result<ScanOut, String> {
    scan_impl(root, cfg, true)
}

/// 复算指标但**不写历史**：worktree 是临时目录，往里写快照会污染时间序列。
fn scan_quiet(root: &Path, cfg: &AppConfig) -> Result<ScanOut, String> {
    scan_impl(root, cfg, false)
}

fn scan_impl(root: &Path, cfg: &AppConfig, write_history: bool) -> Result<ScanOut, String> {
    let args = vec![
        "-m".to_string(),
        "harness_lint".to_string(),
        "--entropy".to_string(),
        "--entropy-json".to_string(),
        "--entropy-window".to_string(),
        cfg.entropy.window.to_string(),
        root.to_string_lossy().to_string(),
    ];
    let mut args = args;
    if write_history {
        args.insert(3, "--entropy-write".to_string());
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = exec::run_with_cap(
        root,
        &cfg.verify.python_bin,
        &refs,
        Duration::from_secs(300),
        &[
            ("PYTHONPATH", cfg.lint.package_dir.as_str()),
            ("PYTHONIOENCODING", "utf-8"),
            ("PYTHONDONTWRITEBYTECODE", "1"),
        ],
        1 << 22,
    );
    if let Some(e) = &out.spawn_error {
        return Err(format!("熵扫描无法启动：{e}"));
    }
    if !out.passed() {
        return Err(format!(
            "熵扫描失败（exit={:?}）：{}",
            out.exit_code,
            exec::clip(out.stderr.trim(), 400)
        ));
    }
    serde_json::from_str::<ScanOut>(out.stdout.trim()).map_err(|e| {
        format!(
            "熵指标 JSON 解析失败：{e}；片段：{}",
            exec::clip(&out.stdout, 300)
        )
    })
}

/// 只感知、不动作：算指标 → 与基线比 → 报告退化与否。
///
/// **定时任务默认用这个模式**：零 token、零副作用、可放在任何低配机器上跑。
/// 只有真的需要「修 + 开 PR」时才用 `run_once`。
pub fn plan_only(repo: &Path, cfg: &AppConfig) -> Result<EntropyOutcome, String> {
    let scan0 = scan(repo, cfg)?;
    let metrics = scan0.metrics;
    let decision = evaluate(
        &metrics,
        scan0.reference.as_ref(),
        scan0.reference_best.as_ref(),
        &cfg.entropy,
    );
    if scan0.reference.is_none() {
        return Ok(EntropyOutcome::BaselineRecorded {
            metrics,
            history_len: scan0.history_len,
        });
    }
    if !decision.degraded {
        return Ok(EntropyOutcome::NoChange { metrics, decision });
    }
    Ok(EntropyOutcome::Degraded { metrics, decision })
}

// ---------------------------------------------------------------- 一次完整运行

pub fn run_once(repo: &Path, cfg: &AppConfig, flag: CancelFlag) -> Result<EntropyOutcome, String> {
    if !cfg.entropy.enabled {
        return Err("熵管理未启用（entropy.enabled = false）".into());
    }
    if !is_git_repo(repo) {
        return Err(format!(
            "{} 不是 git 仓库（熵管理要靠分支/PR 交付）",
            repo.display()
        ));
    }

    // 1) 感知
    let scan0 = scan(repo, cfg)?;
    let before = scan0.metrics.clone();
    let decision = evaluate(
        &before,
        scan0.reference.as_ref(),
        scan0.reference_best.as_ref(),
        &cfg.entropy,
    );

    if scan0.reference.is_none() {
        return Ok(EntropyOutcome::BaselineRecorded {
            metrics: before,
            history_len: scan0.history_len,
        });
    }
    if !decision.degraded {
        return Ok(EntropyOutcome::NoChange {
            metrics: before,
            decision,
        });
    }

    // 2) 配额：已经开着一堆没人看的 PR 时，别再制造噪声
    // 优先用 gh 数"待审 PR"；没有 gh 就退化成数"远端熵分支"。
    // 两条路径都能防住"同一问题反复开新 PR"这个致命失败模式。
    let (pending, source) = match open_entropy_prs(repo, &cfg.entropy) {
        Some(n) => (Some(n), "gh pr list"),
        None => (remote_entropy_branches(repo, &cfg.entropy), "git ls-remote"),
    };
    if let Some(n) = pending
        && n >= cfg.entropy.max_open_prs
    {
        return Ok(EntropyOutcome::Skipped {
            reason: format!(
                "已有 {n} 个未处理的 {}* 分支/PR（上限 {}，来源 {source}）——先处理完再开新的",
                cfg.entropy.branch_prefix, cfg.entropy.max_open_prs
            ),
            metrics: before,
        });
    }

    // 3) 隔离：所有改动在独立 worktree 里做，用户的工作区一字节不碰
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let kind = decision
        .signals
        .first()
        .map(|s| s.kind.clone())
        .unwrap_or_else(|| "drift".into());
    let branch = format!("{}{}-{}", cfg.entropy.branch_prefix, stamp, kind);
    if branch_exists(repo, &branch) {
        return Ok(EntropyOutcome::Skipped {
            reason: format!("分支 {branch} 已存在（幂等：不重复开同一主题的 PR）"),
            metrics: before,
        });
    }
    let wt = repo.join(".harness-entropy").join(format!("wt-{stamp}"));
    if let Some(parent) = wt.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let wt_s = wt.to_string_lossy().to_string();
    let add = run_git(repo, &["worktree", "add", "-b", &branch, &wt_s, "HEAD"]);
    if !add.passed() {
        return Err(format!(
            "创建 worktree 失败：{}",
            exec::clip(add.stderr.trim(), 300)
        ));
    }

    // 4) 在 worktree 里跑规约检查 → 有界选目标 → 让模型修
    let outcome = repair_in_worktree(&wt, cfg, &flag, &before, &decision, &branch, &stamp);

    // 5) 收尾：失败/放弃都把 worktree 拆掉，不留痕
    if !cfg.entropy.keep_worktree {
        let _ = run_git(repo, &["worktree", "remove", "--force", &wt_s]);
        let _ = run_git(repo, &["worktree", "prune"]);
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn repair_in_worktree(
    wt: &Path,
    cfg: &AppConfig,
    flag: &CancelFlag,
    before: &Metrics,
    _decision: &Decision,
    branch: &str,
    stamp: &str,
) -> Result<EntropyOutcome, String> {
    use crate::config::runs_root;

    let out = lint::run_lint(wt, cfg);
    let report = match out.report {
        Some(r) => r,
        None => {
            return Ok(EntropyOutcome::Skipped {
                reason: format!(
                    "worktree 里 lint 没跑成：{}",
                    out.parse_error.unwrap_or_default()
                ),
                metrics: before.clone(),
            });
        }
    };

    if exec::is_cancelled(flag) {
        return Ok(EntropyOutcome::Skipped {
            reason: "用户取消".into(),
            metrics: before.clone(),
        });
    }

    let targets = select_targets(&report, &cfg.entropy);
    if targets.is_empty() {
        return Ok(EntropyOutcome::Abandoned {
            reason: "退化信号没有对应的可修目标（可能是跨文件结构问题，需要人来决定）".into(),
            metrics_before: before.clone(),
            metrics_after: None,
            decision: Decision::default(),
        });
    }

    // 只把选中的诊断放进诊断包 —— 有界，且模型不会顺手改无关代码
    let subset = LintReport {
        root: wt.to_string_lossy().to_string(),
        diagnostics: targets.clone(),
        ..Default::default()
    };
    let package = subset.prompt_package(wt, cfg.lint.prompt_budget_chars);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("建运行时失败：{e}"))?;
    let task = format!(
        "（熵管理自动任务）请只修下面这些违规，不要顺手重构无关代码。目标分支 {}",
        branch
    );
    let (attempt, _tokens, _elapsed) = rt.block_on(repair::propose(
        &cfg.llm,
        &task,
        &package,
        1,
        cfg.entropy.max_repair_attempts,
        None,
        None,
        None,
    ))?;
    let applied = match repair::apply_edits(wt, &attempt.edits) {
        Ok(v) => v,
        Err(e) => {
            return Ok(EntropyOutcome::Abandoned {
                reason: format!("模型给的改动不合法（已回滚）：{e}"),
                metrics_before: before.clone(),
                metrics_after: None,
                decision: Decision::default(),
            });
        }
    };

    // 验证：语法 + 单元测试必须过（这是"改对了"的底线）
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let verify = crate::verify::run(cfg, &format!("entropy-{stamp}"), wt, &cancel);
    if verify.failed > 0 {
        return Ok(EntropyOutcome::Abandoned {
            reason: format!(
                "改动后验证失败（{} 项）——宁可放弃也不提交一个让测试变红的 PR",
                verify.failed
            ),
            metrics_before: before.clone(),
            metrics_after: None,
            decision: Decision::default(),
        });
    }

    // **必须证明改善**：重算指标，质量分没升就整轮放弃
    let after = scan_quiet(wt, cfg)?.metrics;
    if after.score <= before.score {
        return Ok(EntropyOutcome::Abandoned {
            reason: format!(
                "改完质量分没有提升（{:.2} → {:.2}）——本轮改动不提交",
                before.score, after.score
            ),
            metrics_before: before.clone(),
            metrics_after: Some(after),
            decision: Decision::default(),
        });
    }

    // 提交
    let msg = format!(
        "chore(entropy): 例行熵清扫（{stamp}）\n\n质量分 {:.2} → {:.2}；违规 {} → {}\n\n{}",
        before.score,
        after.score,
        before.violations_total,
        after.violations_total,
        applied
            .iter()
            .map(|a| format!("- {a}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let _ = run_git(wt, &["add", "-A"]);
    // 熵历史文件属于运行产物，不该进 PR（用户没写 .gitignore 时兜住）
    let _ = run_git(wt, &["reset", "-q", "--", ".harness-entropy"]);
    let commit = run_git(wt, &["commit", "-m", &msg]);
    if !commit.passed() {
        return Err(format!(
            "提交失败：{}",
            exec::clip(commit.stderr.trim(), 300)
        ));
    }

    let decision = Decision::default();
    let body = pr_body(
        &wt.to_string_lossy(),
        &after,
        Some(before),
        &decision,
        &applied,
        &verify.verdict,
    );
    let body_path = runs_root(cfg)
        .join("entropy")
        .join(format!("pr-{stamp}.md"));
    if let Some(p) = body_path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(&body_path, &body);

    // 开 PR：有 remote + gh 才真开；否则如实说明
    if !has_remote(wt, &cfg.entropy.remote) {
        return Ok(EntropyOutcome::Prepared {
            branch: branch.to_string(),
            pr_body_path: body_path.to_string_lossy().to_string(),
            metrics_before: before.clone(),
            metrics_after: after,
            fixes: applied,
            reason: format!("没有名为 {} 的 remote，无法推送", cfg.entropy.remote),
            decision: Decision::default(),
        });
    }
    let push = run_git(wt, &["push", "-u", &cfg.entropy.remote, branch]);
    if !push.passed() {
        return Ok(EntropyOutcome::Prepared {
            branch: branch.to_string(),
            pr_body_path: body_path.to_string_lossy().to_string(),
            metrics_before: before.clone(),
            metrics_after: after,
            fixes: applied,
            reason: format!("推送失败：{}", exec::clip(push.stderr.trim(), 200)),
            decision: Decision::default(),
        });
    }
    if !cfg.entropy.create_pr || !gh_available() {
        return Ok(EntropyOutcome::Prepared {
            branch: branch.to_string(),
            pr_body_path: body_path.to_string_lossy().to_string(),
            metrics_before: before.clone(),
            metrics_after: after,
            fixes: applied,
            reason: "已推送分支，但本机没有 gh CLI，未创建 PR（正文已落盘）".into(),
            decision: Decision::default(),
        });
    }

    let title = format!(
        "chore(entropy): 例行熵清扫 · 质量分 {:.1} → {:.1}",
        before.score, after.score
    );
    let pr = exec::run_with_cap(
        wt,
        "gh",
        &[
            "pr",
            "create",
            "--title",
            &title,
            "--body-file",
            &body_path.to_string_lossy(),
            "--head",
            branch,
        ],
        Duration::from_secs(120),
        &[],
        1 << 20,
    );
    if !pr.passed() {
        return Ok(EntropyOutcome::Prepared {
            branch: branch.to_string(),
            pr_body_path: body_path.to_string_lossy().to_string(),
            metrics_before: before.clone(),
            metrics_after: after,
            fixes: applied,
            reason: format!("gh pr create 失败：{}", exec::clip(pr.stderr.trim(), 200)),
            decision: Decision::default(),
        });
    }

    Ok(EntropyOutcome::Opened {
        branch: branch.to_string(),
        pr_url: pr.stdout.trim().lines().last().unwrap_or("").to_string(),
        metrics_before: before.clone(),
        metrics_after: after,
        fixes: applied,
        decision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(score: f64, density: f64) -> Metrics {
        Metrics {
            ts: "2026-01-01T00:00:00+08:00".into(),
            code_lines: 1000,
            test_count: 5,
            score,
            density,
            ..Default::default()
        }
    }

    fn cfg() -> EntropyConfig {
        EntropyConfig::default()
    }

    #[test]
    fn no_history_means_baseline_not_healthy() {
        let d = evaluate(&m(90.0, 1.0), None, None, &cfg());
        assert!(d.baseline_only, "没有基线时不能宣称'未退化'");
        assert!(!d.degraded);
    }

    #[test]
    fn stable_metrics_are_not_degradation() {
        let d = evaluate(&m(90.0, 1.0), Some(&m(90.0, 1.0)), None, &cfg());
        assert!(!d.degraded, "{:?}", d.signals);
        assert!(d.signals.is_empty());
    }

    #[test]
    fn score_drop_triggers_a_signal() {
        let d = evaluate(&m(80.0, 1.0), Some(&m(90.0, 1.0)), None, &cfg());
        assert!(d.degraded);
        assert_eq!(d.signals[0].kind, "score");
        assert_eq!(d.signals[0].delta, 10.0);
    }

    #[test]
    fn density_increase_triggers_a_signal() {
        let d = evaluate(&m(90.0, 3.0), Some(&m(90.0, 1.0)), None, &cfg());
        assert!(d.degraded);
        assert!(
            d.signals.iter().any(|s| s.kind == "density"),
            "{:?}",
            d.signals
        );
    }

    #[test]
    fn persistent_degradation_keeps_alarming_via_best_in_window() {
        // 漏洞回归：滚动中位数会把「持续退化」吸收成新常态，于是永久退化不再报警。
        // 第二个信号拿窗口内最好水平做对比，专门堵这个洞。
        let stable_at_75 = m(75.0, 2.0);
        let best = m(95.0, 0.5);
        let d = evaluate(&stable_at_75, Some(&stable_at_75), Some(&best), &cfg());
        assert!(d.degraded, "持续退化必须继续报警");
        assert!(
            d.signals.iter().any(|s| s.kind == "score_vs_best"),
            "{:?}",
            d.signals
        );
    }

    #[test]
    fn best_in_window_signal_stays_quiet_when_close_to_best() {
        let cur = m(93.0, 1.0);
        let best = m(95.0, 1.0);
        let d = evaluate(&cur, Some(&cur), Some(&best), &cfg());
        assert!(!d.degraded, "只差 2 分不该打扰人：{:?}", d.signals);
    }

    #[test]
    fn small_noise_does_not_trigger() {
        // +5% 密度、跌 1 分 —— 都在阈值内，不该打扰人
        let mut cur = m(89.0, 1.05);
        cur.suppressions_total = 1;
        let mut ref_ = m(90.0, 1.0);
        ref_.suppressions_total = 1;
        let d = evaluate(&cur, Some(&ref_), None, &cfg());
        assert!(!d.degraded, "{:?}", d.signals);
    }

    #[test]
    fn new_suppressions_and_lost_tests_are_signals() {
        let mut cur = m(90.0, 1.0);
        cur.suppressions_total = 4;
        let mut ref_ = m(90.0, 1.0);
        ref_.suppressions_total = 1;
        assert!(
            evaluate(&cur, Some(&ref_), None, &cfg())
                .signals
                .iter()
                .any(|s| s.kind == "suppressions")
        );

        let mut fewer = m(90.0, 1.0);
        fewer.test_count = 2;
        assert!(
            evaluate(&fewer, Some(&m(90.0, 1.0)), None, &cfg())
                .signals
                .iter()
                .any(|s| s.kind == "tests")
        );
    }

    #[test]
    fn reasonless_suppression_is_its_own_signal() {
        let mut cur = m(90.0, 1.0);
        cur.suppressions_without_reason = 1;
        let d = evaluate(&cur, Some(&m(90.0, 1.0)), None, &cfg());
        assert!(
            d.signals
                .iter()
                .any(|s| s.kind == "suppression_without_reason")
        );
    }

    #[test]
    fn duplicate_growth_is_a_signal() {
        let mut cur = m(90.0, 1.0);
        cur.duplicate_groups = 3;
        let mut ref_ = m(90.0, 1.0);
        ref_.duplicate_groups = 1;
        assert!(
            evaluate(&cur, Some(&ref_), None, &cfg())
                .signals
                .iter()
                .any(|s| s.kind == "duplicates")
        );
    }

    fn diag(rule: &str, level: &str, file: &str, fixable: bool) -> LintDiagnostic {
        use crate::lint::LintSpan;
        LintDiagnostic {
            rule: rule.into(),
            level: level.into(),
            message: "x".into(),
            spans: vec![LintSpan {
                file_name: file.into(),
                line_start: 1,
                ..Default::default()
            }],
            fixable,
            ..Default::default()
        }
    }

    #[test]
    fn selects_errors_first_then_fixable() {
        let report = LintReport {
            diagnostics: vec![
                diag("HX101", "warning", "a.py", false),
                diag("HX204", "warning", "b.py", true),
                diag("HX201", "error", "c.py", false),
            ],
            ..Default::default()
        };
        let sel = select_targets(&report, &cfg());
        assert_eq!(sel[0].rule, "HX201", "error 优先");
        assert_eq!(sel[1].rule, "HX204", "同为 warning 时有建议的优先");
    }

    #[test]
    fn selection_is_bounded_by_count_and_files() {
        let mut c = cfg();
        c.max_fix_targets = 3;
        c.max_changed_files = 2;
        let report = LintReport {
            diagnostics: vec![
                diag("HX101", "warning", "a.py", false),
                diag("HX102", "warning", "a.py", false),
                diag("HX103", "warning", "b.py", false),
                diag("HX104", "warning", "c.py", false),
                diag("HX105", "warning", "d.py", false),
            ],
            ..Default::default()
        };
        let sel = select_targets(&report, &c);
        assert!(sel.len() <= 3, "条数必须有上限：{}", sel.len());
        let mut files: Vec<String> = sel.iter().map(|d| d.file()).collect();
        files.sort();
        files.dedup();
        assert!(files.len() <= 2, "文件数必须有上限：{files:?}");
    }

    #[test]
    fn infrastructure_errors_are_not_targets() {
        let report = LintReport {
            diagnostics: vec![
                diag("HX000", "error", "broken.py", false),
                diag("HX998", "error", "a.py", false),
            ],
            ..Default::default()
        };
        assert!(
            select_targets(&report, &cfg()).is_empty(),
            "HX000/HX998 是检查没跑成，不是代码熵，不该让模型去改"
        );
    }

    #[test]
    fn pr_body_shows_metric_diff_and_says_it_is_not_auto_merged() {
        let before = m(70.0, 2.0);
        let after = m(85.0, 1.2);
        let body = pr_body(
            "repo",
            &after,
            Some(&before),
            &Decision {
                degraded: true,
                signals: vec![Signal {
                    kind: "score".into(),
                    detail: "质量分 70 → 85".into(),
                    delta: 15.0,
                }],
                ..Default::default()
            },
            &["a.py：except: → except ValueError".into()],
            "通过：语法检查 3 项全过，单元测试已执行并通过",
        );
        assert!(body.contains("质量分"), "{body}");
        assert!(body.contains("70.00"), "必须给出基线值");
        assert!(body.contains("85.00"), "必须给出本次值");
        assert!(body.contains("改善"), "必须标出改善/恶化");
        assert!(body.contains("只提议，不自动合入"), "必须写明不自动合入");
        assert!(body.contains("通过："), "必须带上验证结论");
    }

    #[test]
    fn pr_body_handles_missing_baseline_honestly() {
        let body = pr_body("r", &m(80.0, 1.0), None, &Decision::default(), &[], "v");
        assert!(body.contains("无基线"), "没有基线不能假装有对比");
    }

    #[test]
    fn abandoned_outcome_is_reported_as_nonzero_exit() {
        let o = EntropyOutcome::Abandoned {
            reason: "质量分没升".into(),
            metrics_before: m(80.0, 1.0),
            metrics_after: Some(m(79.0, 1.0)),
            decision: Decision::default(),
        };
        assert_ne!(o.exit_code(), 0, "放弃不是成功，退出码要能区分");
        assert!(o.summary().contains("放弃"));
    }

    /// **熵管理端到端**（真调 DeepSeek + 真 git 操作）。
    ///
    /// 用**本地 bare 仓库当 origin**：分支、提交、推送都是真的，而本机没有 gh
    /// 正好验证"没开成 PR 时如实说明"这条纪律 —— 不许把"准备好了"说成"已开 PR"。
    ///   cargo test -- --ignored --nocapture live_entropy_cycle
    #[test]
    #[ignore = "真实调用 DeepSeek API + 真 git 操作（熵管理闭环）"]
    fn live_entropy_cycle_on_temp_repo() {
        use std::process::Command;

        let cfg = crate::config::load().expect("读配置失败");
        assert!(!cfg.llm.api_key.trim().is_empty(), "需要先配置 API Key");

        let base = std::env::temp_dir().join(format!(
            "dh-entropy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        ));
        let origin = base.join("origin.git");
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let git = |dir: &std::path::Path, args: &[&str]| -> exec::CmdOutput {
            exec::run_with_cap(dir, "git", args, Duration::from_secs(60), &[], 1 << 20)
        };
        let must = |dir: &std::path::Path, args: &[&str]| {
            let o = git(dir, args);
            assert!(o.passed(), "git {args:?} 失败：{}", o.stderr);
            o
        };

        // ---- 一个"本地 GitHub"：bare 仓库当 origin
        must(
            &base,
            &["init", "--bare", origin.to_string_lossy().as_ref()],
        );
        must(&repo, &["init"]);
        must(&repo, &["config", "user.email", "entropy@test.local"]);
        must(&repo, &["config", "user.name", "entropy-test"]);
        must(
            &repo,
            &["remote", "add", "origin", origin.to_string_lossy().as_ref()],
        );

        // ---- 种子：干净代码 + 真断言测试（基线应当是好状态）
        std::fs::write(repo.join(".gitignore"), ".harness-entropy/\n").unwrap();
        std::fs::write(
            repo.join("mathx.py"),
            "def total(items):\n    return sum(items)\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("test_mathx.py"),
            "from mathx import total\n\n\ndef test_total():\n    assert total([1, 2]) == 3\n",
        )
        .unwrap();
        must(&repo, &["add", "-A"]);
        must(&repo, &["commit", "-m", "seed"]);
        must(&repo, &["push", "-u", "origin", "HEAD"]);

        // ---- 第 1 次：只有基线可比（不能宣称"健康"）
        let o1 = plan_only(&repo, &cfg).expect("plan_only 失败");
        println!("\n[1] {}", o1.summary());
        assert!(
            matches!(o1, EntropyOutcome::BaselineRecorded { .. }),
            "第一次运行只能落基线：{}",
            o1.summary()
        );

        // ---- 让熵涨起来：一堆典型漂移，然后提交
        std::fs::write(
            repo.join("messy.py"),
            "import logging\n\n\ndef total(items):\n    try:\n        return sum(items)\n    except:\n        pass\n\n\ndef find(name):\n    if name == None:\n        return None\n    return name\n\n\ndef add_note(note, notes=[]):\n    notes.append(note)\n    return notes\n\n\ndef listing(rows):\n    print(\"listing\")\n    return rows\n",
        )
        .unwrap();
        must(&repo, &["add", "-A"]);
        must(&repo, &["commit", "-m", "drift"]);
        must(&repo, &["push", "origin", "HEAD"]);

        // ---- 第 2 次：必须判出退化
        let o2 = plan_only(&repo, &cfg).expect("plan_only 失败");
        println!("\n[2] {}", o2.summary());
        let degraded_signals = match &o2 {
            EntropyOutcome::Degraded { decision, .. } => decision.signals.len(),
            other => panic!("应当判出退化，实际：{}", other.summary()),
        };
        assert!(degraded_signals > 0);

        // ---- 完整闭环：修复 + 提交 + 推送（本机无 gh → 走 Prepared 分支）
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        let o3 = run_once(&repo, &cfg, flag).expect("run_once 失败");
        println!("\n[3] {}", o3.summary());

        let (branch, body_path, before, after) = match &o3 {
            EntropyOutcome::Prepared {
                branch,
                pr_body_path,
                metrics_before,
                metrics_after,
                reason,
                ..
            } => {
                println!("    （未开 PR 的原因：{reason}）");
                (
                    branch.clone(),
                    pr_body_path.clone(),
                    metrics_before.clone(),
                    metrics_after.clone(),
                )
            }
            EntropyOutcome::Opened {
                branch,
                metrics_before,
                metrics_after,
                ..
            } => (
                branch.clone(),
                String::new(),
                metrics_before.clone(),
                metrics_after.clone(),
            ),
            other => panic!("期望 Opened/Prepared，实际：{}", other.summary()),
        };

        // 必须证明改善，否则整轮该放弃
        assert!(
            after.score > before.score,
            "只允许提交「让指标变好」的改动：{:.2} → {:.2}",
            before.score,
            after.score
        );
        assert!(branch.starts_with("entropy/"), "{branch}");

        // 分支真的推到 origin 了
        let ls = must(&repo, &["ls-remote", "--heads", "origin", &branch]);
        assert!(
            ls.stdout.contains(&branch),
            "分支必须已推送到 origin，ls-remote 输出：{:?}",
            ls.stdout
        );

        // 用户的工作区一个字节都没动（只有未跟踪的 .harness-entropy/ 被 ignore 掉了）
        let st = must(&repo, &["status", "--porcelain", "--untracked-files=no"]);
        assert!(
            st.stdout.trim().is_empty(),
            "主工作区不该有任何改动，实际：{:?}",
            st.stdout
        );
        // 退化文件还原样在（我们没去改用户的工作区）
        let messy = std::fs::read_to_string(repo.join("messy.py")).unwrap();
        assert!(messy.contains("except:"), "主工作区里的违规必须原样保留");

        // PR 正文：有指标对比、有验证结论、明说"不自动合入"
        if !body_path.is_empty() {
            let body = std::fs::read_to_string(&body_path).expect("PR 正文应当落盘");
            println!("\n--- PR 正文 ---\n{body}\n---------------");
            assert!(body.contains("只提议，不自动合入"), "必须写明不自动合入");
            assert!(body.contains("改善"), "必须标出指标是改善还是恶化");
            assert!(body.contains("通过："), "必须带验证结论");
        }

        // worktree 必须清理干净
        let wt_dir = repo.join(".harness-entropy");
        let leftovers: Vec<_> = std::fs::read_dir(&wt_dir)
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.starts_with("wt-"))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "worktree 应当被清理，残留：{leftovers:?}"
        );

        // ---- 幂等/配额：配额设成 1，第二次必须跳过而不是再开一个分支
        let mut cfg2 = cfg.clone();
        cfg2.entropy.max_open_prs = 1;
        let flag2: CancelFlag = Arc::new(AtomicBool::new(false));
        let o4 = run_once(&repo, &cfg2, flag2).expect("第二次 run_once 失败");
        println!("\n[4] {}", o4.summary());
        match &o4 {
            EntropyOutcome::Skipped { reason, .. } => {
                assert!(reason.contains("上限"), "{reason}")
            }
            other => panic!("配额应当拦住第二次运行，实际：{}", other.summary()),
        }
        let branches = must(&repo, &["ls-remote", "--heads", "origin", "entropy/*"]);
        assert_eq!(
            branches
                .stdout
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count(),
            1,
            "不该产生第二个熵分支：{:?}",
            branches.stdout
        );

        println!("\n结论：感知 → 判定 → 有界修复 → 验证 → 提交 → 推送，全链路真实跑通；");
        println!("      用户工作区零改动，配额拦住重复开分支。");
        let _ = std::fs::remove_dir_all(&base);
        let _ = Command::new("git").arg("--version").output();
    }
}
