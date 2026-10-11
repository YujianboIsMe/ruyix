//! 门禁：**机械验证 + 复核**（v0.3）。
//!
//! 两层，事实驱动（不做任务分类）：
//! - 窄层：写动作触发的单文件语法检查（跑在临时目录里的改动内容上 —— 确认模式下"磁盘没动"这个
//!   不变式不能破）；
//! - 全层：交付（`final`）触发的 `verify::run` + lint（只在 Apply 模式；Stage 模式如实报 `skipped`）。
//!
//! 全层不过就**拦住 final** 并把报告回灌（最多 `gate.max_full_attempts` 次，之后带着诚实的
//! "未通过"脚注放行）；放行前还有**干净上下文的反思 agent**（`engine::reflect`）复核产物。
//! 这两件事的对外形状就是本模块开头的 `CheckItem` / `VerifyOutcome`（原先单独占一段 banner，
//! 与门禁本体的代码相隔 3300 行 —— 2026-09-28 拆 `agent.rs` 时合成一个文件）。

use super::*;

// （原 `agent.rs` 的 banner：机械验证结论（v0.3 门禁的对外形状））

// ============================================
// 机械验证结论（v0.3 门禁的对外形状）
// ============================================

/// 逐项检查的精简形状：**不带** stdout/stderr（几 KB 日志进事件流没意义，UI 只需要知道哪项失败）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CheckItem {
    /// syntax | test | lint
    pub kind: String,
    pub language: String,
    pub target: String,
    /// passed | failed | skipped
    pub status: String,
    pub reason: String,
}

impl From<&verify::CheckResult> for CheckItem {
    fn from(c: &verify::CheckResult) -> Self {
        Self {
            kind: c.kind.clone(),
            language: c.language.clone(),
            target: c.target.clone(),
            status: c.status.clone(),
            reason: c.reason.clone(),
        }
    }
}

/// 一次机械验证的结论：`layer` 区分窄层（改动后的语法检查）与全量层（交付前）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct VerifyOutcome {
    /// narrow | full
    pub layer: String,
    /// passed | failed | skipped
    pub status: String,
    /// 一句话结论（UI 直接显示）
    pub verdict: String,
    pub passed_checks: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 回灌给模型的修复提示（失败时非空）
    #[serde(default)]
    pub hint: String,
    /// 跳过原因（跑不了的时候必须有，不许静默）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    pub checks: Vec<CheckItem>,
    pub elapsed_ms: u128,
}

impl VerifyOutcome {
    pub fn layer_label(&self) -> &'static str {
        match self.layer.as_str() {
            "full" => "全量验证",
            _ => "语法检查",
        }
    }

    pub fn passed(&self) -> bool {
        self.status == "passed"
    }

    pub fn skipped(&self) -> bool {
        self.status == "skipped"
    }

    /// 汇总规则（窄/全量共用）：有 failed → failed；否则有 passed → passed；否则 skipped。
    /// "什么都没跑成"绝不写成"通过"。
    pub(crate) fn summarize(
        layer: &str,
        checks: &[CheckItem],
        elapsed_ms: u128,
        skipped_reason: Option<String>,
    ) -> Self {
        let failed_list: Vec<&CheckItem> = checks.iter().filter(|c| c.status == "failed").collect();
        let passed_checks = checks.iter().filter(|c| c.status == "passed").count();
        let skipped = checks.iter().filter(|c| c.status == "skipped").count();
        let failed = failed_list.len();
        let status = if failed > 0 {
            "failed"
        } else if passed_checks > 0 {
            "passed"
        } else {
            "skipped"
        };
        // 跳过的原因优先用调用方给的；没有就借第一条 skipped 检查的理由
        // （沙箱拒绝这类原因就写在那条检查里，不许退化成"没有可跑的检查项"）
        let reason = skipped_reason
            .or_else(|| {
                checks
                    .iter()
                    .find(|c| c.status == "skipped")
                    .map(|c| c.reason.clone())
            })
            .unwrap_or_else(|| "没有可跑的检查项".into());
        let verdict = match status {
            "failed" => format!(
                "未通过：{} 项失败（首个：{} {}）",
                failed,
                failed_list[0].target,
                clip(&failed_list[0].reason, 120)
            ),
            "passed" => format!("通过：{} 项检查全过", passed_checks),
            _ => format!("未执行检查：{reason}"),
        };
        Self {
            layer: layer.into(),
            status: status.into(),
            verdict,
            passed_checks,
            failed,
            skipped,
            hint: String::new(),
            skipped_reason: if status == "skipped" {
                Some(reason)
            } else {
                None
            },
            checks: checks.to_vec(),
            elapsed_ms,
        }
    }

    /// 从真实检查结果建结论（顺带用 `verify::hint_for` 把失败翻译成修复提示）
    pub(crate) fn from_checks(
        layer: &str,
        checks: &[verify::CheckResult],
        elapsed_ms: u128,
    ) -> Self {
        let items: Vec<CheckItem> = checks.iter().map(CheckItem::from).collect();
        let hint = checks
            .iter()
            .filter(|c| c.status == "failed")
            .map(|c| {
                format!(
                    "- {}（{}）：{}\n  {}",
                    c.target,
                    c.language,
                    c.reason,
                    verify::hint_for(c)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut out = Self::summarize(layer, &items, elapsed_ms, None);
        out.hint = hint;
        out
    }

    /// 跑不了（确认模式不落盘 / 沙箱拒绝 / 没有可跑命令）—— 必须写明原因
    pub(crate) fn skip(layer: &str, reason: String) -> Self {
        let mut out = Self::summarize(layer, &[], 0, Some(reason));
        out.status = "skipped".into();
        out
    }

    /// 回灌主循环的观察文本（模型看到的是"哪一层的什么检查失败了、怎么修"）
    pub(crate) fn to_observation(&self) -> String {
        let mut s = format!("[机械验证·{}] {}\n", self.layer_label(), self.verdict);
        if let Some(r) = &self.skipped_reason {
            s.push_str(&format!("跳过原因：{r}\n"));
        }
        for c in self.checks.iter().filter(|c| c.status == "failed") {
            s.push_str(&format!("✗ {} {}：{}\n", c.kind, c.target, c.reason));
        }
        if !self.hint.trim().is_empty() {
            s.push_str(&self.hint);
            s.push('\n');
        }
        s.push_str("（修完再交付；验证没通过不算完成。）");
        s
    }
}

// （原 `agent.rs` 的 banner：门禁：机械验证 + 反思（v0.3））

// ============================================
// 门禁：机械验证 + 反思（v0.3）
// ============================================

/// 一次 run 内的门禁状态
#[derive(Default)]
pub(crate) struct GateState {
    /// 存在"改动后还没通过全量验证"的内容
    pub(crate) dirty: bool,
    /// 全量验证连续失败次数（防"验证不过就无限修"）
    pub(crate) full_failures: u32,
    /// 复核结论回灌主循环的次数
    pub(crate) reflect_rounds: u32,
    /// 要写进最终答复的补充说明（跳过原因 / 预算用尽 / 复核未完成）
    pub(crate) notes: Vec<String>,
    /// 交付前对账（未声明 keep_alive 的托管进程）提醒过了没有 —— 只提醒一次，不循环
    pub(crate) proc_warned: bool,
}

/// 窄验证：对**这一轮的改动内容**做单文件语法检查。
///
/// 作用在覆盖层内容上（不是磁盘），所以确认模式"磁盘未动"也安全。
/// 代价小是刻意的：它每写一次就跑一次，是给模型的即时反馈，不是交付判据。
pub(crate) async fn narrow_verify(cfg: &AppConfig, changes: &[FileChange]) -> VerifyOutcome {
    let files: Vec<(String, String)> = changes
        .iter()
        .map(|c| (c.path.clone(), c.after.clone()))
        .collect();
    let v = cfg.verify.clone();
    let timeout = Duration::from_secs(cfg.gate.staged_timeout_secs);
    let started = Instant::now();
    let checks =
        tokio::task::spawn_blocking(move || verify::staged_syntax_checks(&v, &files, timeout))
            .await
            .unwrap_or_default();
    VerifyOutcome::from_checks("narrow", &checks, started.elapsed().as_millis())
}

/// 全量验证：复用 `verify::run`（语法 + 单测，隔离出口一处决定）+ lint（规约）。
///
/// **只在写入/自主模式跑**：确认模式下改动还在暂存区、项目磁盘没变，在那里跑出来的
/// "通过"是假结论 —— 宁可显式跳过（带原因），也不给一个假的通过。
pub(crate) async fn full_verify(
    cfg: &AppConfig,
    proj: &Path,
    policy: WritePolicy,
    cancel: &CancelFlag,
) -> VerifyOutcome {
    if policy == WritePolicy::Stage {
        return VerifyOutcome::skip(
            "full",
            "确认模式下改动只在暂存区（项目磁盘未动），跑不了全量验证；本次只做了语法层。\
             切到写入/自主模式才会跑全量"
                .into(),
        );
    }
    let started = Instant::now();
    let (root, cfg_v, flag) = (proj.to_path_buf(), cfg.clone(), cancel.clone());
    let report =
        match tokio::task::spawn_blocking(move || verify::run(&cfg_v, "agent-gate", &root, &flag))
            .await
        {
            Ok(r) => r,
            Err(e) => return VerifyOutcome::skip("full", format!("验证线程没能跑起来：{e}")),
        };

    let mut items: Vec<CheckItem> = report.checks.iter().map(CheckItem::from).collect();
    let hint = report
        .checks
        .iter()
        .filter(|c| c.status == "failed")
        .map(|c| {
            format!(
                "- {}（{}）：{}\n  {}",
                c.target,
                c.language,
                c.reason,
                verify::hint_for(c)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    if cfg.lint.enabled {
        let (root2, cfg_l) = (proj.to_path_buf(), cfg.clone());
        match tokio::task::spawn_blocking(move || lint::run_lint(&root2, &cfg_l)).await {
            Ok(out) => {
                if let Some(item) = lint_item(&out) {
                    items.push(item);
                }
            }
            Err(e) => items.push(CheckItem {
                kind: "lint".into(),
                language: "-".into(),
                target: "规约检查".into(),
                status: "skipped".into(),
                reason: format!("lint 线程没能跑起来：{e}"),
            }),
        }
    }

    let mut out = VerifyOutcome::summarize("full", &items, started.elapsed().as_millis(), None);
    out.hint = hint;
    out
}

/// lint 结果 → 一项检查。口径与前端 `check-style` 一致：**error 才阻断**，warning 只记录。
pub(crate) fn lint_item(o: &lint::LintOutcome) -> Option<CheckItem> {
    let item = |status: &str, reason: String| CheckItem {
        kind: "lint".into(),
        language: "-".into(),
        target: "规约检查".into(),
        status: status.into(),
        reason,
    };
    if !o.ran {
        return Some(item(
            "skipped",
            o.parse_error
                .clone()
                .unwrap_or_else(|| "lint 没有运行".into()),
        ));
    }
    let Some(rep) = &o.report else {
        return Some(item(
            "skipped",
            o.parse_error
                .clone()
                .unwrap_or_else(|| "lint 没有产出报告".into()),
        ));
    };
    let warnings = rep.counts.get("warning").copied().unwrap_or(0);
    let errors = rep.counts.get("error").copied().unwrap_or(0);
    if errors == 0 {
        return Some(item(
            "passed",
            format!("error 0 · warning {warnings}（warning 只记录，不阻断）"),
        ));
    }
    let first = rep
        .diagnostics
        .iter()
        .find(|d| d.level == "error")
        .map(|d| format!("{} {}:{}", d.rule, d.file(), d.line()))
        .unwrap_or_else(|| format!("{errors} 条 error"));
    Some(item("failed", first))
}

/// 交付前对账：本项目里**这次 run 结束时会被收掉**的托管进程（未声明 `keep_alive`）。
///
/// 为什么非说不可：模型用后台模式起了服务、看到就绪，于是 final 写「服务已启动」—— 而 run 一结束
/// 引擎就把它收掉，用户 netstat 一看是空的。实测 run `agent-20260921-0950`（cloud-shop-admin）：
/// `p1.log` 里 09:51:18 `Tomcat started on port 8087`、09:51:21 还真接了模型那次 curl，
/// 日志随后**戛然而止**（没有优雅关闭 = 被杀的），而模型已经在 final 里写
/// 「服务已在本机 8087 端口成功启动并验证可访问」。引擎手里有这份事实，就该在放行答复之前说一次，
/// 让模型自己选：重启成 keep_alive，或者把话说准。
pub(crate) fn reap_warning(live: &[crate::proc::ProcInfo]) -> Option<String> {
    let doomed: Vec<&crate::proc::ProcInfo> = live.iter().filter(|p| !p.keep_alive).collect();
    if doomed.is_empty() {
        return None;
    }
    let list = doomed
        .iter()
        .map(|p| format!("{}（pid {}，{}）", p.handle, p.pid, clip(&p.cmd, 60)))
        .collect::<Vec<_>>()
        .join("、");
    Some(format!(
        "[交付前对账·托管进程] 本项目还有 {} 个后台进程**没声明 keep_alive**，本次 run 一结束引擎就会收掉：{list}。\
         如果答复里写「服务已启动 / 正在运行」，那句话在用户看到时就是假的（netstat 是空的）。二选一：\
         ① 用户要的是「服务跑着」 → 先 execute {{\"op\":\"stop\",\"handle\":\"<上面的 handle>\"}} 停掉，再用 {{\"cmd\":\"<原命令>\",\"background\":true,\"ready_cmd\":\"<原判据>\",\"keep_alive\":true}} 重启，然后 final；\
         ② 只是验证一下 → 明说「本次验证完已自动收掉」，别写成「正在运行」。",
        doomed.len()
    ))
}
/// 交付门禁：**有改动 → 全量验证；然后 → 反思**。返回 `Some(观察)` 表示打回让模型继续修。
///
/// 三条不变量：
/// - 验证/复核都失败时不判"任务失败"，只按预算放行并在答复里写明（诚实 > 好看）；
/// - 复核不引入第二个写手：它的结论回到主循环，改代码的永远是主循环；
/// - 用户取消时不启动新的验证/复核（长动作必须有取消路径）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn gate_before_final(
    cfg: &AppConfig,
    proj: &Path,
    ctx: &Ctx<'_>,
    task: &str,
    read_paths: &[String],
    answer: &str,
    gate: &mut GateState,
    out: &mut AgentOutcome,
    cancel: &CancelFlag,
    sink: &dyn Sink,
) -> Option<String> {
    // ⓪ 交付前对账：会被本次 run 结束收掉的托管进程，先说一次（只提醒一次，不循环）
    if !gate.proc_warned {
        let live = crate::proc::listing_for(proj);
        if let Some(board) = reap_warning(&live) {
            gate.proc_warned = true;
            sink.log(
                "warn",
                format!("[agent] 交付被打回（托管进程对账）：{}", clip(&board, 240)),
            );
            return Some(board);
        }
    }

    let has_changes = !ctx.changes.is_empty();

    // ① 机械验证（全量）：只在"有改动且改动还没被验证过"时跑
    if has_changes && cfg.gate.full && gate.dirty && !is_cancelled(cancel) {
        if gate.full_failures >= cfg.gate.max_full_attempts {
            if !gate.notes.iter().any(|n| n.contains("按预算放行")) {
                gate.notes.push(format!(
                    "> ⚠️ 全量验证连续失败 {} 次，按预算放行 —— **结论：未通过**（详见验证记录）",
                    gate.full_failures
                ));
            }
            gate.dirty = false; // 不再重复走这个分支
        } else {
            let v = full_verify(cfg, proj, ctx.policy, cancel).await;
            let status = v.status.clone();
            let skipped_reason = v.skipped_reason.clone();
            let observation = v.to_observation();
            sink.verify(&v);
            out.verifications.push(v);
            match status.as_str() {
                // 未通过：拒绝交付，把报告回灌；改动仍是"脏"的
                "failed" => {
                    gate.full_failures += 1;
                    gate.dirty = true;
                    return Some(observation);
                }
                // 跳过（确认模式 / 沙箱拒绝）：不是失败，放行但必须写明原因
                "skipped" => {
                    gate.dirty = false;
                    if let Some(r) = skipped_reason {
                        gate.notes.push(format!("> ⚠️ 本次未跑全量验证：{r}"));
                    }
                }
                _ => gate.dirty = false,
            }
        }
    }

    // ② 反思（干净上下文复核）：有改动审产物，没改动核依据
    if cfg.reflect.enabled && gate.reflect_rounds < cfg.reflect.max_rounds && !is_cancelled(cancel)
    {
        let rubric = reflect::select_rubric(has_changes);
        // 复核也要看得到本 run 自己认下的事实（纯问答场景的对齐靠它，见 ReflectInput::findings）
        let findings_lines: Vec<String> = ctx
            .progress()
            .in_prompt()
            .iter()
            .map(|f| f.line())
            .collect();
        let input = reflect::ReflectInput {
            task,
            rubric,
            project_root: proj,
            changes: &ctx.changes,
            read_paths,
            probes: ctx.probes(),
            answer: if has_changes { None } else { Some(answer) },
            findings: &findings_lines,
            verifications: &out.verifications,
            gate_note: gate.notes.last().cloned(),
        };
        let outcome = reflect::run(cfg, &input, cancel, sink).await;
        out.usage.add(&outcome.usage);
        let reflection = outcome.reflection;
        sink.reflect(&reflection);
        let observation = reflection.to_observation();
        let suspect = reflection.suspect();
        if let Some(n) = &reflection.note {
            gate.notes.push(format!("> 复核未完成：{n}"));
        }
        out.reflections.push(reflection);
        if suspect {
            gate.reflect_rounds += 1;
            return Some(observation);
        }
    }

    None
}
