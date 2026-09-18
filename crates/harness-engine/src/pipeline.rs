//! harness 管道的**唯一实现**：计划 → 生成 → 规约检查 → 自我纠正 → 验证 → 落盘。
//!
//! 为什么要有这个模块（v0.7 还的技术债）：这套阶段序列原来只活在 `main.rs` 的 GUI 命令里，
//! 评估器只能**抄一份**（`eval.rs` 里重写一遍顺序与停止条件）。两份实现迟早会走样 ——
//! 于是"评估测出来的东西"和"用户实际跑的东西"就不是一个东西了。
//!
//! 现在两边都调这里：
//! - GUI：`GuiSink`（把事件 emit 到前端 + 写观测日志）；
//! - 评估：`EvalSink`（写 stdout，指标从返回的 `RunRecord` 里取）。
//!
//! 输出的差别只在 Sink，**阶段顺序、停止条件、错误处理完全共用**。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::config::{self, AppConfig};
use crate::generate::StepOutcome;
use crate::repair::RepairRound;
use crate::verify::VerifyReport;
use crate::workspace::RunRecord;
use crate::{exec, generate, kb, lint, observe, plan, repair, verify, workspace};

// ============================================
// Sink：管道对外说话的唯一出口
// ============================================

/// 观测日志（trace/logs.jsonl）+ stdout。GUI 的 Sink 会在它之外再补一个 emit。
pub fn log_observing(level: &str, msg: &str) {
    observe::log(level, "", msg);
    println!("[{level}] {msg}");
}

/// 阶段边界：开/关一个 span，并按状态打一行日志。
pub fn stage_observing(name: &str, status: &str, detail: &str) {
    if status == "start" {
        observe::stage_open(name, observe::attrs(&[("detail", detail)]));
    } else {
        observe::stage_close(name, status, observe::attrs(&[("detail", detail)]));
    }
}

/// 管道事件的接收方。默认实现只做"观测 + stdout"；GUI 覆写它补 emit。
pub trait Sink: Send + Sync {
    fn log(&self, level: &str, msg: String) {
        log_observing(level, &msg);
    }

    fn stage(&self, name: &str, status: &str, detail: String) {
        stage_observing(name, status, &detail);
        if !detail.is_empty() {
            let level = match status {
                "error" => "error",
                "done" => "ok",
                _ => "info",
            };
            self.log(level, format!("[{name}] {status} — {detail}"));
        }
    }

    /// 生成阶段的每一步（GUI 用来更新步骤列表）
    fn step(&self, _i: usize, _total: usize, _st: &StepOutcome) {}
    /// 规约检查结果（GUI 用来刷新诊断面板）
    fn lint(&self, _outcome: &lint::LintOutcome) {}
    /// 每一轮自我纠正（GUI 用来追加修复记录）
    fn repair(&self, _r: &RepairRound) {}
}

// ============================================
// 管道入口
// ============================================

/// 跑一次管道的参数。GUI 用默认值；评估用它来固定 run_id、预置既有文件、限制修复轮数。
#[derive(Clone, Debug, Default)]
pub struct RunOpts {
    pub task: String,
    /// 预设运行 id（评估要"先建目录放既有文件、再跑模型"）。
    /// None = 按计划的 slug 自动生成（GUI 的行为）。
    pub run_id: Option<String>,
    /// 预置到产物目录的既有文件：(产物内相对路径, 源文件绝对路径)
    pub seed: Vec<(String, String)>,
    /// 覆盖 `lint.max_repair_rounds`
    pub max_repair_rounds: Option<u32>,
}

/// 把既有文件预置进产物目录（建目录 + 拷贝），返回产物目录。
///
/// 为什么要有它：评估任务里的"复用仓库里已有的模块"必须让模型**真的看到**那些文件，
/// 否则生成代码 `import store_util` 直接 ModuleNotFoundError，测的就不是"会不会复用"。
/// 管道与评估的 raw 臂共用这一份实现（别各写一遍）。
pub fn seed_files(
    root: &std::path::Path,
    run_id: &str,
    seeds: &[(String, String)],
) -> Result<PathBuf, String> {
    let dir = workspace::run_dir(root, run_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建运行目录失败 {}: {e}", dir.display()))?;
    let proj = workspace::project_dir(root, run_id)?;
    for (rel, src) in seeds {
        let to = proj.join(rel);
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::copy(src, &to).map_err(|e| format!("预置既有文件失败 {src} → {rel}: {e}"))?;
    }
    Ok(proj)
}

/// **唯一**的管道实现：GUI 与评估都走这里。
pub async fn run(
    cfg: &AppConfig,
    flag: Arc<AtomicBool>,
    opts: RunOpts,
    sink: &dyn Sink,
) -> Result<RunRecord, String> {
    let task = opts.task.trim().to_string();
    if task.is_empty() {
        return Err("任务描述不能为空".into());
    }
    let mut cfg = cfg.clone();
    if let Some(r) = opts.max_repair_rounds {
        cfg.lint.max_repair_rounds = r;
    }
    let root = config::runs_root(&cfg);
    sink.log("info", format!("=== 新任务 ===\n{task}"));
    let t0 = Instant::now();

    // 预设 run_id（评估用）：先把目录建好、既有文件放好，模型才看得到"仓库里已有的东西"
    if let Some(id) = opts.run_id.as_deref() {
        let proj = seed_files(&root, id, &opts.seed)?;
        if !opts.seed.is_empty() {
            sink.log(
                "info",
                format!("预置既有文件 {} 个：{}", opts.seed.len(), proj.display()),
            );
        }
    }

    let mut rec = match plan_stage(sink, &cfg, &task, opts.run_id.as_deref()).await {
        Ok(r) => r,
        Err(e) => {
            sink.stage("plan", "error", e.clone());
            return Err(e);
        }
    };
    if let Err(e) = generate_stage(sink, &cfg, flag.clone(), &mut rec).await {
        rec.status = "failed".into();
        rec.error = Some(e.clone());
        let _ = workspace::save(&root, &rec);
        return Err(e);
    }
    if let Err(e) = lint_and_repair_stage(sink, &cfg, flag.clone(), &mut rec).await {
        rec.error = Some(e.clone());
        let _ = workspace::save(&root, &rec);
        return Err(e);
    }
    let _ = workspace::save(&root, &rec);
    sink.log(
        "ok",
        format!(
            "=== 完成 === 总耗时 {:.1}s，tokens {}/{}，{}",
            t0.elapsed().as_millis() as f64 / 1000.0,
            rec.usage.prompt_tokens,
            rec.usage.completion_tokens,
            rec.verify
                .as_ref()
                .map(|v| v.verdict.clone())
                .unwrap_or_default()
        ),
    );
    Ok(rec)
}

// ============================================
// 阶段实现（从 main.rs 搬过来，逻辑一字未改）
// ============================================

pub async fn plan_stage(
    sink: &dyn Sink,
    cfg: &AppConfig,
    task: &str,
    preset_run_id: Option<&str>,
) -> Result<RunRecord, String> {
    // 知识库（v0.6）：规划阶段的查询就是**任务原文** —— 期望补的是项目约定、
    // 目录结构、既有模块（避免规划出重复的东西）。
    let engine = kb::Engine::from_config(cfg);
    let plan_inj = if engine.enabled {
        kb::retrieve::retrieve(&engine, "plan", task, &kb::retrieve::Workspace::default())
    } else {
        // 关着的时候也要留一条"没注入"的记录：否则事后根本分不清
        // "知识库没开"与"知识库开了但没命中"
        kb::retrieve::KbInjection {
            stage: "plan".into(),
            query: task.trim().to_string(),
            enabled: false,
            reason: engine.reason.clone(),
            budget_chars: engine.cfg.token_budget,
            ..Default::default()
        }
    };
    sink.log(
        if plan_inj.hits.is_empty() {
            "info"
        } else {
            "ok"
        },
        format!("[plan] 知识库：{}", plan_inj.summary()),
    );
    let plan_block = kb::retrieve::render_block(&plan_inj);

    sink.stage("plan", "start", format!("调用 {} 拆解任务…", cfg.llm.model));
    let t0 = Instant::now();
    let (plan, usage, model, ms) = plan::generate(&cfg.llm, task, plan_block.as_deref()).await?;

    let root = config::runs_root(cfg);
    // 预设 id（评估：先建目录放既有文件、再跑模型）优先；否则按计划的 slug 生成（GUI）
    let run_id = match preset_run_id {
        Some(id) => id.to_string(),
        None => workspace::new_run_id(&plan.slug()),
    };
    let dir = workspace::run_dir(&root, &run_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建运行目录失败 {}: {e}", dir.display()))?;

    let rec = RunRecord {
        run_id,
        task: task.trim().to_string(),
        created_at: workspace::now_iso(),
        model,
        status: "planned".into(),
        dir: dir.to_string_lossy().to_string(),
        plan: Some(plan.clone()),
        kb: vec![plan_inj],
        usage,
        ..Default::default()
    };
    workspace::save(&root, &rec)?;

    sink.log(
        "ok",
        format!(
            "规划完成：{} 步（{}），语言 {}，耗时 {} ms，tokens {}/{}",
            plan.steps.len(),
            if plan.has_test_step() {
                "含测试步骤"
            } else {
                "⚠ 缺测试步骤"
            },
            plan.language,
            ms,
            rec.usage.prompt_tokens,
            rec.usage.completion_tokens
        ),
    );
    for s in &plan.steps {
        sink.log(
            "info",
            format!(
                "  {}. [{}] {} → {}",
                s.id,
                s.kind,
                s.title,
                s.files.join(", ")
            ),
        );
    }
    if !plan.has_test_step() {
        sink.log(
            "warn",
            "规划里没有 kind=test 的步骤，验证阶段很可能只能做语法检查".to_string(),
        );
    }
    sink.stage(
        "plan",
        "done",
        format!(
            "{} 个步骤 · {} ms · {}",
            plan.steps.len(),
            t0.elapsed().as_millis(),
            rec.run_id
        ),
    );
    Ok(rec)
}

pub async fn generate_stage(
    sink: &dyn Sink,
    cfg: &AppConfig,
    flag: Arc<AtomicBool>,
    rec: &mut RunRecord,
) -> Result<(), String> {
    let plan = rec
        .plan
        .clone()
        .ok_or_else(|| "这次运行还没有计划，请先生成计划".to_string())?;
    let root = config::runs_root(cfg);
    let proj = workspace::project_dir(&root, &rec.run_id)?;

    sink.stage(
        "generate",
        "start",
        format!("{} 个步骤 → {}", plan.steps.len(), proj.display()),
    );
    sink.log("info", format!("输出目录: {}", proj.display()));

    let t0 = Instant::now();
    // 知识库：查询用"当前步骤标题 + 详情"，工作区 = 已生成的文件（见 generate::run_all）
    let engine = kb::Engine::from_config(cfg);
    let outcome = generate::run_all(
        &cfg.llm,
        cfg,
        &rec.task,
        &plan,
        &rec.run_id,
        &proj,
        move |st, i, total| sink.step(i, total, st),
        &flag,
        if engine.enabled { Some(&engine) } else { None },
    )
    .await?;

    // 每步的知识注入记录进 run.json（并打一行日志，别让"注入了什么"只活在 trace 里）
    for st in &outcome.steps {
        if let Some(k) = &st.kb {
            if !k.hits.is_empty() || !k.reason.is_empty() {
                sink.log(
                    if k.hits.is_empty() { "info" } else { "ok" },
                    format!("[generate:step-{}] 知识库：{}", st.step_id, k.summary()),
                );
            }
            rec.kb.push(k.clone());
        }
    }

    for st in &outcome.steps {
        match st.status.as_str() {
            "done" => sink.log(
                "ok",
                format!(
                    "步骤 {}. {} → {} 个文件（{} ms）",
                    st.step_id,
                    st.title,
                    st.files.len(),
                    st.elapsed_ms
                ),
            ),
            "skipped" => sink.log(
                "warn",
                format!("步骤 {}. {} 已跳过：{}", st.step_id, st.title, st.notes),
            ),
            _ => sink.log(
                "error",
                format!(
                    "步骤 {}. {} 失败：{}",
                    st.step_id,
                    st.title,
                    st.error.clone().unwrap_or_default()
                ),
            ),
        }
        if st.no_files {
            sink.log(
                "warn",
                format!(
                    "步骤 {}. {} 没有产出文件 —— 检查它对任务的贡献是否落空",
                    st.step_id, st.title
                ),
            );
        }
        if !st.notes.trim().is_empty() && st.status == "done" {
            sink.log(
                "info",
                format!("   备注: {}", crate::exec::clip(&st.notes, 400)),
            );
        }
    }
    for r in &outcome.rejected {
        sink.log("warn", format!("拒绝写入（路径越界/非法）: {r}"));
    }

    let failed = outcome.steps.iter().filter(|s| s.status == "error").count();
    let done = outcome.steps.iter().filter(|s| s.status == "done").count();

    rec.usage.add(&outcome.usage);
    rec.generation = Some(outcome);
    rec.status = "generated".to_string();
    if failed > 0 {
        sink.log(
            "warn",
            format!(
                "{failed} 个步骤生成失败，但已生成的 {} 个步骤仍会参与验证",
                done
            ),
        );
    }
    workspace::save(&root, rec)?;

    sink.stage(
        "generate",
        if failed > 0 { "error" } else { "done" },
        format!(
            "{done} 步成功 / {failed} 步失败 · {} 个文件 · {} ms",
            rec.generation.as_ref().map(|g| g.files.len()).unwrap_or(0),
            t0.elapsed().as_millis()
        ),
    );
    Ok(())
}

pub async fn verify_stage(
    sink: &dyn Sink,
    cfg: &AppConfig,
    flag: Arc<AtomicBool>,
    rec: &mut RunRecord,
) -> Result<VerifyReport, String> {
    let root = config::runs_root(cfg);
    let proj = workspace::project_dir(&root, &rec.run_id)?;
    if !proj.exists() {
        return Err(format!(
            "项目目录不存在（{}），请先生成代码",
            proj.display()
        ));
    }

    sink.stage(
        "verify",
        "start",
        format!("语法检查 + 单元测试 @ {}", proj.display()),
    );
    let cfg2 = cfg.clone();
    let rid = rec.run_id.clone();
    let proj2 = proj.clone();
    let flag2 = flag.clone();
    let t0 = Instant::now();

    // 验证是纯阻塞的（等子进程），丢到 blocking 线程池，别占着 tokio worker
    let report = tokio::task::spawn_blocking(move || verify::run(&cfg2, &rid, &proj2, &flag2))
        .await
        .map_err(|e| format!("验证线程异常: {e}"))?;

    for c in &report.checks {
        let icon = match c.status.as_str() {
            "passed" => "ok",
            "failed" => "error",
            _ => "warn",
        };
        let hint = verify::hint_for(c);
        sink.log(
            icon,
            format!(
                "[{}] {} / {} — {}{}{}",
                c.kind,
                c.language,
                c.target,
                c.status,
                if c.cmd.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", c.cmd)
                },
                if hint.is_empty() {
                    String::new()
                } else {
                    format!(" · {hint}")
                }
            ),
        );
        // 失败时把真实 stderr 前几行打出来，用户不用点开详情就知道错在哪
        if c.status == "failed" {
            let detail = if c.stderr.trim().is_empty() {
                &c.stdout
            } else {
                &c.stderr
            };
            for line in detail.lines().filter(|l| !l.trim().is_empty()).take(6) {
                sink.log("error", format!("    ├ {line}"));
            }
        }
    }

    rec.verify = Some(report.clone());
    rec.status = if report.failed > 0 {
        "failed".into()
    } else {
        "verified".into()
    };
    if report.failed > 0 {
        rec.error = Some(report.verdict.clone());
    }
    workspace::save(&root, rec)?;

    sink.stage(
        "verify",
        if report.failed > 0 { "error" } else { "done" },
        format!(
            "{}（通过 {} / 失败 {} / 跳过 {}）· {} ms",
            report.verdict,
            report.passed,
            report.failed,
            report.skipped,
            t0.elapsed().as_millis()
        ),
    );
    Ok(report)
}

pub async fn run_lint(cfg: &AppConfig, proj: PathBuf) -> lint::LintOutcome {
    let cfg2 = cfg.clone();
    match tokio::task::spawn_blocking(move || lint::run_lint(&proj, &cfg2)).await {
        Ok(out) => out,
        Err(e) => lint::LintOutcome {
            parse_error: Some(format!("lint 线程异常：{e}")),
            ..Default::default()
        },
    }
}

pub(crate) fn zero_means_clean(lint_enabled: bool, lint_ran: bool) -> bool {
    !(lint_enabled && !lint_ran)
}

pub(crate) fn issue_count(rec: &RunRecord) -> u32 {
    let lint_total = rec.lint.as_ref().map(|l| l.total()).unwrap_or(0);
    let verify_failed = rec.verify.as_ref().map(|v| v.failed as u32).unwrap_or(0);
    lint_total + verify_failed
}

pub(crate) fn verify_failures_text(report: &VerifyReport) -> String {
    let mut out = String::new();
    let mut any = false;
    for c in &report.checks {
        if c.status != "failed" {
            continue;
        }
        any = true;
        out.push_str(&format!(
            "\n[验证失败] {} / {} — {}\n  命令：{}\n",
            c.kind, c.target, c.reason, c.cmd
        ));
        let detail = if c.stderr.trim().is_empty() {
            &c.stdout
        } else {
            &c.stderr
        };
        for line in detail.lines().filter(|l| !l.trim().is_empty()).take(12) {
            out.push_str(&format!("  | {line}\n"));
        }
    }
    if any {
        out.push_str("\n（以上是测试/编译的真实输出，优先修它）\n");
    }
    out
}

pub async fn repair_stage(
    sink: &dyn Sink,
    cfg: &AppConfig,
    flag: &exec::CancelFlag,
    rec: &mut RunRecord,
    proj: &PathBuf,
) -> Result<(), String> {
    let max_rounds = cfg.lint.max_repair_rounds;
    if max_rounds == 0 {
        sink.log(
            "info",
            "自我纠正循环已关闭（lint.max_repair_rounds = 0）".to_string(),
        );
        return Ok(());
    }

    let mut prev: Option<RepairRound> = None;
    for round in 1..=max_rounds {
        if exec::is_cancelled(flag) {
            sink.log("warn", "用户取消，自我纠正循环提前结束".to_string());
            break;
        }
        let before = issue_count(rec);
        if before == 0 {
            if zero_means_clean(cfg.lint.enabled, rec.lint.is_some()) {
                sink.log("ok", "没有需要修复的问题，循环结束".to_string());
            } else {
                sink.log(
                    "warn",
                    "问题数为 0，但规约检查没有跑成 —— 这个 0 只代表验证通过，不代表没有规约违规；请先修好 lint 命令（见上面的报错）再依赖这个结论。".to_string(),
                );
            }
            break;
        }

        // 诊断包：只含诊断（自带为什么/怎么改/参考）+ 被点到的文件内容
        let mut package = String::new();
        if let Some(l) = &rec.lint {
            package.push_str(&l.prompt_package(proj, cfg.lint.prompt_budget_chars));
        }
        if let Some(v) = &rec.verify {
            package.push_str(&verify_failures_text(v));
        }

        sink.stage(
            "repair",
            "start",
            format!(
                "第 {round}/{max_rounds} 轮：当前 {before} 个问题（规约 {} + 验证失败 {}）",
                rec.lint.as_ref().map(|l| l.total()).unwrap_or(0),
                rec.verify.as_ref().map(|v| v.failed).unwrap_or(0)
            ),
        );

        // 知识库（v0.6）：自纠正阶段的查询是**诊断里的规则 ID** ——
        // 第二节证明了"只给诊断就够改"，而把规则文档正文一起给上会更快更稳。
        let engine = kb::Engine::from_config(cfg);
        let rule_ids = repair::rule_ids(rec);
        let kb_inj = if engine.enabled {
            let query = if rule_ids.is_empty() {
                crate::exec::clip(&package, 200)
            } else {
                rule_ids.join(" ")
            };
            let ws = kb::retrieve::Workspace::of(
                std::slice::from_ref(proj),
                &rec.generation
                    .as_ref()
                    .map(|g| {
                        g.files
                            .iter()
                            .map(|f| f.path.clone())
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default(),
            );
            kb::retrieve::retrieve(&engine, &format!("repair:{round}"), &query, &ws)
        } else {
            kb::retrieve::KbInjection {
                stage: format!("repair:{round}"),
                query: rule_ids.join(" "),
                enabled: false,
                reason: engine.reason.clone(),
                budget_chars: engine.cfg.token_budget,
                ..Default::default()
            }
        };
        sink.log(
            if kb_inj.hits.is_empty() { "info" } else { "ok" },
            format!("[repair:{round}] 知识库：{}", kb_inj.summary()),
        );
        let kb_block = kb::retrieve::render_block(&kb_inj);
        rec.kb.push(kb_inj);

        let proposed = repair::propose(
            &cfg.llm,
            &rec.task,
            &package,
            round,
            max_rounds,
            prev.as_ref(),
            kb_block.as_deref(),
        )
        .await;
        let (attempt, tokens, elapsed) = match proposed {
            Ok(v) => v,
            Err(e) => {
                let r = RepairRound {
                    round,
                    before,
                    after: before,
                    status: "rejected".into(),
                    detail: e.clone(),
                    ..Default::default()
                };
                sink.repair(&r);
                rec.repair.push(r);
                sink.log("error", format!("第 {round} 轮模型没给出可用改动：{e}"));
                break;
            }
        };

        for ed in &attempt.edits {
            sink.log(
                "info",
                format!(
                    "  计划改动 {}：{}",
                    ed.path,
                    crate::exec::clip(&ed.reason, 80)
                ),
            );
        }

        match repair::apply_edits(proj, &attempt.edits) {
            Ok(applied) => {
                for a in &applied {
                    sink.log("ok", format!("  已应用：{a}"));
                }
                // 重跑验证 + 规约检查
                match verify_stage(sink, cfg, flag.clone(), rec).await {
                    Ok(_) => {}
                    Err(e) => sink.log("warn", format!("重跑验证失败：{e}")),
                }
                let lint_out = run_lint(cfg, proj.clone()).await;
                sink.lint(&lint_out);
                if !lint_out.ran {
                    sink.log(
                        "error",
                        format!(
                            "重跑规约检查失败：{}",
                            lint_out.parse_error.clone().unwrap_or_default()
                        ),
                    );
                }
                rec.lint = lint_out.report.clone();

                let after = issue_count(rec);
                let status = if after < before { "ok" } else { "no_progress" };
                let r = RepairRound {
                    round,
                    before,
                    after,
                    applied: applied.clone(),
                    notes: attempt.notes.clone(),
                    usage_tokens: tokens,
                    elapsed_ms: elapsed,
                    status: status.into(),
                    detail: if status == "ok" {
                        format!("{before} → {after}")
                    } else {
                        format!("{before} → {after}（没下降，停止循环）")
                    },
                };
                sink.repair(&r);
                sink.log(
                    if status == "ok" { "ok" } else { "warn" },
                    format!("第 {round} 轮：{}", r.detail),
                );
                rec.repair.push(r.clone());
                prev = Some(r);

                let root = config::runs_root(cfg);
                workspace::save(&root, rec)?;
                if after == 0 {
                    sink.log("ok", "全部问题已修复".to_string());
                    break;
                }
                if status == "no_progress" {
                    break;
                }
            }
            Err(e) => {
                let r = RepairRound {
                    round,
                    before,
                    after: before,
                    status: "rejected".into(),
                    detail: e.clone(),
                    notes: attempt.notes.clone(),
                    usage_tokens: tokens,
                    elapsed_ms: elapsed,
                    ..Default::default()
                };
                sink.repair(&r);
                sink.log(
                    "error",
                    format!("第 {round} 轮改动被拒绝（已回滚，未改盘）：{e}"),
                );
                rec.repair.push(r);
                break;
            }
        }
    }

    let final_issues = issue_count(rec);
    let clean = final_issues == 0 && zero_means_clean(cfg.lint.enabled, rec.lint.is_some());
    let detail = if clean {
        format!("循环结束，剩余 0 个问题；共 {} 轮", rec.repair.len())
    } else if rec.lint.is_none() {
        format!(
            "循环结束（规约检查未跑成，结论不完整）：验证失败 {} 项；共 {} 轮",
            rec.verify.as_ref().map(|v| v.failed).unwrap_or(0),
            rec.repair.len()
        )
    } else {
        format!(
            "循环结束，剩余 {final_issues} 个问题；共 {} 轮",
            rec.repair.len()
        )
    };
    sink.stage("repair", if clean { "done" } else { "skip" }, detail);
    Ok(())
}

pub(crate) async fn lint_and_repair_stage(
    sink: &dyn Sink,
    cfg: &AppConfig,
    flag: exec::CancelFlag,
    rec: &mut RunRecord,
) -> Result<(), String> {
    let root = config::runs_root(cfg);
    let proj = workspace::project_dir(&root, &rec.run_id)?;

    if cfg.lint.enabled {
        sink.stage(
            "lint",
            "start",
            format!("自定义规则检查 @ {}", proj.display()),
        );
        let out = run_lint(cfg, proj.clone()).await;
        sink.lint(&out);
        match &out.report {
            Some(r) => {
                sink.log(
                    if r.ok { "ok" } else { "error" },
                    format!("规约检查：{} · {}", r.summary_line(), out.cmd),
                );
                for (rule, n) in &r.by_rule {
                    sink.log("info", format!("  {rule} × {n}"));
                }
                for d in r.diagnostics.iter().take(8) {
                    sink.log(
                        if d.level == "error" { "error" } else { "warn" },
                        format!("  {} {}:{} {}", d.rule, d.file(), d.line(), d.message),
                    );
                }
                if r.total() as usize > 8 {
                    sink.log(
                        "info",
                        format!("  …（共 {} 条，详见 UI 规约面板）", r.total()),
                    );
                }
            }
            None => sink.log(
                "error",
                format!(
                    "规约检查没跑成：{}",
                    out.parse_error.clone().unwrap_or_else(|| "未知原因".into())
                ),
            ),
        }
        rec.lint = out.report.clone();
        sink.stage(
            "lint",
            if out.report.as_ref().map(|r| r.ok).unwrap_or(false) {
                "done"
            } else {
                "error"
            },
            out.report
                .as_ref()
                .map(|r| r.summary_line())
                .unwrap_or_else(|| out.parse_error.clone().unwrap_or_default()),
        );
    } else {
        sink.stage(
            "lint",
            "skip",
            "规约检查已关闭（lint.enabled = false）".to_string(),
        );
    }

    // 先跑一次验证，让修复循环同时掌握"编译/测试失败"与"规约违规"两类问题
    verify_stage(sink, cfg, flag.clone(), rec).await?;

    repair_stage(sink, cfg, &flag, rec, &proj).await?;

    // 终态再验一次：循环过程中最后一轮改动必须被验证覆盖
    verify_stage(sink, cfg, flag, rec).await?;
    workspace::save(&root, rec)?;
    Ok(())
}
