//! AgentSink：引擎 `pipeline::Sink` → Tauri 事件 `agent://*`。
//!
//! payload 与 harness 的 `harness://*` 逐字段同形（融合计划 D4 / 附录 B），
//! 因此 `ui/agent.js` 的渲染器与 harness `ui/main.js` 共用一套数据形状。
//!
//! 观测纪律不变：覆写 Sink 的同时仍调用 `log_observing` / `stage_observing`，
//! 保证 trace/logs.jsonl 与 stdout 照旧落盘，emit 只是"额外补一份给 GUI"。

use harness_engine as engine;
use tauri::AppHandle;
use tauri::Emitter;

pub struct AgentSink {
    app: AppHandle,
}

impl AgentSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

#[derive(Clone, serde::Serialize)]
struct LogPayload<'a> {
    level: &'a str,
    msg: &'a str,
}

#[derive(Clone, serde::Serialize)]
struct StagePayload<'a> {
    stage: &'a str,
    status: &'a str,
    detail: &'a str,
}

#[derive(Clone, serde::Serialize)]
struct StepPayload<'a> {
    index: usize,
    total: usize,
    title: &'a str,
    status: &'a str,
    notes: &'a str,
    files: &'a [String],
    error: Option<&'a String>,
}

#[derive(Clone, serde::Serialize)]
struct DiagPayload {
    file: String,
    line: u32,
    rule: String,
    msg: String,
}

#[derive(Clone, serde::Serialize)]
struct LintPayload {
    ran: bool,
    ok: bool,
    errors: u32,
    warnings: u32,
    summary: String,
    cmd: String,
    parse_error: Option<String>,
    diagnostics: Vec<DiagPayload>,
}

#[derive(Clone, serde::Serialize)]
struct RepairPayload<'a> {
    round: u32,
    before: u32,
    after: u32,
    status: &'a str,
    applied: Vec<String>,
    detail: &'a str,
    notes: &'a str,
}

impl engine::pipeline::Sink for AgentSink {
    fn log(&self, level: &str, msg: String) {
        engine::pipeline::log_observing(level, &msg);
        let _ = self
            .app
            .emit("agent://log", LogPayload { level, msg: &msg });
    }

    fn stage(&self, name: &str, status: &str, detail: String) {
        engine::pipeline::stage_observing(name, status, &detail);
        let _ = self.app.emit(
            "agent://stage",
            StagePayload {
                stage: name,
                status,
                detail: &detail,
            },
        );
    }

    fn step(&self, i: usize, total: usize, st: &engine::generate::StepOutcome) {
        let _ = self.app.emit(
            "agent://step",
            StepPayload {
                index: i,
                total,
                title: &st.title,
                status: &st.status,
                notes: &st.notes,
                files: &st.files,
                error: st.error.as_ref(),
            },
        );
    }

    /// 规划完成即推送完整计划 —— 前端大纲区据此渲染任务列表（agent://step 推进三态）
    fn plan(&self, p: &engine::plan::Plan) {
        let _ = self.app.emit("agent://plan", p);
    }

    /// 机械验证结论（v0.3）：会话里「验证」小节实时更新，不等 run 结束
    fn verify(&self, v: &engine::agent::VerifyOutcome) {
        let _ = self.app.emit("agent://verify", v);
    }

    /// 复核结论（v0.3）：干净上下文复核 agent 的判定，会话里「复核」小节
    fn reflect(&self, r: &engine::reflect::Reflection) {
        let _ = self.app.emit("agent://reflect", r);
    }

    fn lint(&self, outcome: &engine::lint::LintOutcome) {
        let _ = self.app.emit("agent://lint", lint_payload(outcome));
    }

    fn repair(&self, r: &engine::repair::RepairRound) {
        let _ = self.app.emit(
            "agent://repair",
            RepairPayload {
                round: r.round,
                before: r.before,
                after: r.after,
                status: &r.status,
                applied: r.applied.clone(),
                detail: &r.detail,
                notes: &r.notes,
            },
        );
    }
}

/// `LintOutcome` → `agent://lint` payload（前端诊断卡直接可渲染的形状）。
fn lint_payload(outcome: &engine::lint::LintOutcome) -> LintPayload {
    match &outcome.report {
        Some(report) => {
            let errors = report.counts.get("error").copied().unwrap_or(0);
            let warnings = report.counts.get("warning").copied().unwrap_or(0);
            let diagnostics = report
                .diagnostics
                .iter()
                .map(|d| DiagPayload {
                    file: d.file(),
                    line: d.spans.first().map(|s| s.line_start).unwrap_or(0),
                    rule: d.rule.clone(),
                    msg: d.message.clone(),
                })
                .collect();
            LintPayload {
                ran: outcome.ran,
                ok: report.ok,
                errors,
                warnings,
                summary: String::new(),
                cmd: outcome.cmd.clone(),
                parse_error: outcome.parse_error.clone(),
                diagnostics,
            }
        }
        None => LintPayload {
            ran: outcome.ran,
            ok: false,
            errors: 0,
            warnings: 0,
            summary: String::new(),
            cmd: outcome.cmd.clone(),
            parse_error: Some(
                outcome
                    .parse_error
                    .clone()
                    .unwrap_or_else(|| "lint 没有产出报告".into()),
            ),
            diagnostics: Vec::new(),
        },
    }
}
