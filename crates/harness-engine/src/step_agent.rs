//! 计划步骤的执行体（子 agent）：**一个步骤 = 一个上下文干净的 agent 循环**（v0.4）。
//!
//! 为什么新开上下文：主循环的 messages **只增不减**，每次 read 回灌最多 8K 字符、
//! execute 4K，全 run 没有任何裁剪 —— 累积量随轮数近似平方增长，96 轮时它不是"有点大"，
//! 而是主要成本。把"一步的执行"放进自己的上下文，父上下文里从此不再出现那些文件内容。
//!
//! 三条纪律：
//! 1. **上下文隔离**：子上下文只有 `STEP_SYSTEM` + 本步输入包（[`build_user_prompt`]）。
//!    主循环的历史、计划原文、它读过的文件，一条都不进来；跨步骤唯一能流动的东西是
//!    引擎写的 [`StepSummary`] 一行（模型自述不算事实）。
//! 2. **覆盖层不分裂**：子 agent 不持有自己的 overlay/changes —— 那会让
//!    `emit_step_progress` / `gate_before_final` / `flush_stage` 全部失明。它借的是主循环
//!    **同一份** `&mut Ctx`，写完文件后父的进度判据（声明文件是否落地）自动成立。
//! 3. **失败不撒谎**：超预算 / 取消一律落 `status=error` 并把原因写清楚；已经产出的文件
//!    仍留在覆盖层里（与 `settle_steps` 的"文件全落地就算数"一致）。只有"致命模型错误"
//!    上抛 `Err`，由父循环决定是中断整个 run 还是记一条失败 —— 这与主循环今天的语义相同。
//!
//! 本模块**不发 `sink.step`**：`ui/session.js` 用一维 index（`s._plan[index-1]`）认步骤，
//! 子步骤自报事件会撞掉父的 index。步骤进度由父循环在派发前/返回后统一发。

use crate::agent::{
    Ctx, FileChange, LLM_FAIL_LIMIT, StepAction, VerifyOutcome, narrow_verify,
    parse_failure_feedback, parse_step_action, policy_system_note, staged_execute_note,
    tool_execute,
};
use crate::config::AppConfig;
use crate::exec::{CancelFlag, clip, is_cancelled};
use crate::llm::{self, ChatMessage, Usage};
use crate::pipeline::Sink;
use crate::plan::PlanStep;
use std::path::Path;
use std::time::{Duration, Instant};

/// 本步交付说明的留存上限。它只是给父循环打日志用的，**不是事实来源**。
const SELF_REPORT_CLIP: usize = 1_200;
/// 轨迹一行最长多少字符
const TRACE_CLIP: usize = 160;
/// 前置步骤摘要最多列几条 / 总共多少字符 —— 跨步骤通道必须有界，否则它自己就是新的膨胀源
const PRIOR_MAX: usize = 12;
const PRIOR_CLIP: usize = 1_500;

/// 步骤执行体的系统提示词。
///
/// 刻意**不复用** `AGENT_SYSTEM`：那 3KB 里大半是 connect 清单、历史规则、plan 说明，
/// 执行体一条都用不上，而每步都要重发一次。照 `REFLECT_SYSTEM` 的做法独立成常量。
pub const STEP_SYSTEM: &str = r#"你是 ruyix 的步骤执行体：只负责**一个**计划步骤，做完把结果交回去。你看不到主循环的对话历史 —— 这是刻意的，你的上下文里只有本步需要的东西。

三种原子能力：
- read    读项目：{"tool":"read","args":{"path":"src/ 或 src/main.rs"}} —— 目录给结构树，文件给内容。
- write   写文件：{"tool":"write","args":{"path":"相对路径","content":"完整文件内容"}} —— 改已有文件前先 read 拿到现状，交回的必须是整份内容，不许用省略号或"其余不变"敷衍。
- execute 跑命令：{"tool":"execute","args":{"cmd":"命令","timeout_secs":30}} —— 工作目录是项目根，超时上限 120 秒；编译、测试、格式化都走它。

规则：
1. 每轮只输出一个 JSON 对象（一次能力调用，或本步的交付说明），不要解释文字、不要 markdown 代码块包裹。
2. 只做本步。发现计划与实际不符（要改的文件不存在、步骤拆得不对、范围明显比本步大）时不要自作主张扩大范围：做你能做的部分，并在交付说明里写清哪里对不上。
3. 改代码：先 read 全文，再 write 交回整份新内容（哪怕只改一行）；没把握的地方原样保留，绝不丢内容。
4. 能验证就验证：execute 跑本项目自己的编译/测试命令，失败就继续修。
5. 本步做完输出：{"final":"本步做了什么、产出哪些文件、跑了什么验证、结果如何；对不上的地方写在这里"}"#;

/// 交给下一步的那一行（引擎写的事实，只有它允许跨步骤流动）
#[derive(Clone, Debug, Default)]
pub struct StepSummary {
    pub id: u32,
    pub title: String,
    /// done | error | skipped
    pub status: String,
    /// 本步实际写入的文件（相对路径）
    pub files: Vec<String>,
    /// 引擎写的补充事实：产出 / 未产出、语法检查结论、失败原因
    pub note: String,
}

impl StepSummary {
    /// 这一行事实的正文：`format_prior` 给它加列表符号，父循环的日志直接用同一份
    pub fn line(&self) -> String {
        let mut s = format!("步骤 {}「{}」：{}", self.id, self.title.trim(), self.status);
        if !self.files.is_empty() {
            s.push_str(&format!("｜产出 {}", self.files.join("、")));
        }
        if !self.note.trim().is_empty() {
            s.push_str(&format!("｜{}", self.note.trim()));
        }
        s
    }
}

/// 本步的输入包 —— 子上下文里**唯一**允许出现的东西
#[derive(Clone, Debug)]
pub struct StepInput<'a> {
    pub project_root: &'a Path,
    /// 任务原文（模型的 final 里常有"整体交付说明"，本步只做自己那一段）
    pub task: &'a str,
    pub step: &'a PlanStep,
    /// 第几步（1-based）与总步数 —— 模型需要知道自己在哪一步，才不会重做前面的活
    pub index: usize,
    pub total: usize,
    /// 已完成的前置步骤（引擎累积的事实）
    pub done: &'a [StepSummary],
}

/// 一次步骤执行的结果
#[derive(Clone, Debug, Default)]
pub struct StepReport {
    /// done | error
    pub status: String,
    /// 本步实际写入的文件（按写入顺序）
    pub files: Vec<String>,
    /// 本步读过哪些文件/目录。父循环要拿它喂复核员（"依据核对"的证据集合）——
    /// 子步骤的 read 不进父上下文，这份清单是**唯一**能让父知道"它查过什么"的通道。
    pub read_paths: Vec<String>,
    /// 引擎写的本步事实（产出 + 语法检查结论）—— 交回父循环与下一步用的就是它
    pub note: String,
    /// 模型自己写的交付说明。**仅供参考**（它可能自述"已完成"而文件根本没写），
    /// 父循环要判事实就看 `files` / `note` / `verify`。
    pub self_report: String,
    /// 失败原因（`status == error` 时必有）
    pub error: Option<String>,
    /// 本步最后一次语法检查的结论（`gate.narrow` 关闭或本步没写文件时为 None）
    pub verify: Option<VerifyOutcome>,
    /// 实际用掉的轮次
    pub rounds: usize,
    pub usage: Usage,
    pub elapsed_ms: u128,
    /// 工具轨迹（一行一次调用，父循环可以直接打日志）
    pub trace: Vec<String>,
}

impl StepReport {
    pub fn ok(&self) -> bool {
        self.status == "done"
    }

    /// 给父循环打日志的一行：与交给下一步的那一行**同一个事实来源**，不许两套说法
    pub fn headline(&self, step: &PlanStep) -> String {
        self.as_summary(step).line()
    }

    /// → 下一步输入包里的那一行
    pub fn as_summary(&self, step: &PlanStep) -> StepSummary {
        StepSummary {
            id: step.id,
            title: step.title.clone(),
            status: self.status.clone(),
            files: self.files.clone(),
            note: match &self.error {
                Some(e) => format!("{}｜失败：{}", self.note, clip(e, 160)),
                None => self.note.clone(),
            },
        }
    }
}

/// 组装本步的用户消息（纯函数，可单测）
pub fn build_user_prompt(inp: &StepInput<'_>) -> String {
    let mut s = String::new();
    s.push_str(&format!("项目根目录：{}\n", inp.project_root.display()));
    s.push_str(&format!("\n【任务原文】\n{}\n", inp.task.trim()));
    s.push_str(&format!(
        "\n【计划进度】第 {} / {} 步 —— 你只做这一步，不要重做前面的步骤，也不要顺手做后面的。\n本步标题：{}\n",
        inp.index,
        inp.total,
        inp.step.title.trim()
    ));
    if !inp.step.detail.trim().is_empty() {
        s.push_str(&format!("本步说明：{}\n", inp.step.detail.trim()));
    }
    if inp.step.files.is_empty() {
        s.push_str("本步应产出：（计划没声明具体文件）\n");
    } else {
        s.push_str(&format!("本步应产出：{}\n", inp.step.files.join("、")));
    }
    s.push_str(&format!(
        "\n【已完成的前置步骤】\n{}\n",
        format_prior(inp.done)
    ));
    s.push_str(&format!(
        "\n请开始第 {} 步；做完输出 {{\"final\":\"…\"}}。",
        inp.index
    ));
    s
}

/// 前置步骤摘要 → 有界的一段文本
fn format_prior(done: &[StepSummary]) -> String {
    if done.is_empty() {
        return "（无 —— 这是第一步）".into();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut budget = PRIOR_CLIP;
    for (i, d) in done.iter().enumerate() {
        if i >= PRIOR_MAX {
            lines.push(format!(
                "- …还有 {} 步没列出来（需要细节就自己 read 相关文件）",
                done.len() - PRIOR_MAX
            ));
            break;
        }
        let line = d.line();
        if line.chars().count() > budget {
            lines.push(format!(
                "- 步骤 {}「{}」：{}（摘要已截断，细节自己 read）",
                d.id,
                d.title.trim(),
                d.status
            ));
            break;
        }
        budget = budget.saturating_sub(line.chars().count());
        lines.push(format!("- {line}"));
    }
    lines.join("\n")
}

/// 引擎写的本步事实：产出了什么 + 语法检查结论（没查就明说没查，不许留白当通过）
fn step_note(written: &[String], verify: Option<&VerifyOutcome>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if written.is_empty() {
        parts.push("未产出文件".into());
    }
    parts.push(match verify {
        Some(v) => format!("{}：{}", v.layer_label(), clip(&v.verdict, 80)),
        None => "（本次未做语法检查）".into(),
    });
    parts.join("｜")
}

/// 本步写过的文件对应的变更记录（按步过滤：父循环是"全量改动再查一遍"，这里不必重复）
fn step_changes(cx: &Ctx<'_>, written: &[String]) -> Vec<FileChange> {
    cx.changes()
        .iter()
        .filter(|c| written.iter().any(|w| w == &c.path))
        .cloned()
        .collect()
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// 工具结果 → 回灌消息（与主循环同一形状，模型不必学两套协议）
fn json_result(r: Result<String, String>) -> String {
    match r {
        Ok(v) => format!("{{\"ok\": true, \"result\": {}}}", json_str(&v)),
        Err(e) => format!("{{\"ok\": false, \"error\": {}}}", json_str(&e)),
    }
}

/// 不支持的能力：明确拒绝 + 给出替代动作，别让模型在那儿反复试
fn unsupported_reason(name: &str) -> String {
    format!(
        "步骤执行体只有 read / write / execute 三种能力，没有 {name}：\
         任务清单由主循环维护（本步不要重排计划），外部能力（MCP / 远端 Agent）不在本步范围内。\
         请改用上面三种能力，或直接交回 {{\"final\":\"…\"}}。"
    )
}

#[allow(clippy::too_many_arguments)]
fn error_report(
    reason: String,
    written: Vec<String>,
    read: Vec<String>,
    verify: Option<VerifyOutcome>,
    rounds: usize,
    usage: Usage,
    trace: Vec<String>,
    started: Instant,
) -> StepReport {
    let note = step_note(&written, verify.as_ref());
    StepReport {
        status: "error".into(),
        files: written,
        read_paths: read,
        note,
        self_report: String::new(),
        error: Some(reason),
        verify,
        rounds,
        usage,
        elapsed_ms: started.elapsed().as_millis(),
        trace,
    }
}

/// 执行一个步骤：开自己的 messages，只允许 read / write / execute，直到它交回 `final`。
///
/// `cx` 是主循环那一份（共享覆盖层，见模块文档第 2 条）。写盘/暂存策略由 `cx` 决定。
/// 返回 `Err` 只在"致命模型错误"（Key 无效、余额不足、连续抖动到上限）—— 与主循环
/// 一致，由父循环决定中断还是记一条失败；其余情况一律是 `Ok(status=error)` 的报告。
pub async fn run_step(
    cfg: &AppConfig,
    cx: &mut Ctx<'_>,
    inp: &StepInput<'_>,
    cancel: &CancelFlag,
    // run 级总时长的截止时刻（`None` = 不限）。父循环与子步骤看的是**同一条**闸 ——
    // 只掐父循环不够：一个步骤内部可能连跑几十轮编译/测试，父那边毫无感知。
    deadline: Option<Instant>,
    sink: &dyn Sink,
) -> Result<StepReport, String> {
    let started = Instant::now();
    let mut usage = Usage::default();
    let mut written: Vec<String> = Vec::new();
    let mut read: Vec<String> = Vec::new();
    let mut trace: Vec<String> = Vec::new();
    let mut last_verify: Option<VerifyOutcome> = None;
    let mut llm_failures: u32 = 0;
    let max_steps = cfg.step.max_steps.max(1);

    // 上下文隔离的落点：从零开始，只有系统提示词 + 本步输入包
    let mut user = build_user_prompt(inp);
    // 运行模式与父循环共用同一份说明（同一处真相）：确认模式下本步的 write 只暂存，
    // execute 看不到 —— 不说清这条，子步会像父一样拿 execute 的失败反复"修"一个已经写对的改动
    if let Some(note) = policy_system_note(cx.policy()) {
        user.push_str(&format!("\n\n{note}"));
    }
    let mut msgs = vec![ChatMessage::system(STEP_SYSTEM), ChatMessage::user(user)];

    for round in 1..=max_steps {
        if is_cancelled(cancel) {
            return Ok(error_report(
                "用户取消，本步骤中断".into(),
                written,
                read,
                last_verify,
                round.saturating_sub(1),
                usage,
                trace,
                started,
            ));
        }
        // 总时长闸：与父循环共用同一条 deadline，超了立刻交回（已产出的文件留在覆盖层里）
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            return Ok(error_report(
                "本次 run 的总时长预算已用尽，本步骤中断".into(),
                written,
                read,
                last_verify,
                round.saturating_sub(1),
                usage,
                trace,
                started,
            ));
        }
        sink.log(
            "info",
            format!("[step {}] 第 {round} 轮（预算 {max_steps}）", inp.index),
        );
        let reply = match llm::chat(&cfg.llm, &msgs, true).await {
            Ok(r) => {
                llm_failures = 0;
                r
            }
            Err(e) => {
                llm_failures += 1;
                // 鉴权/余额这类确定性失败重试无意义：直接上抛（与主循环同一判据）
                if llm::is_fatal_error(&e) || llm_failures >= LLM_FAIL_LIMIT {
                    return Err(format!("步骤 {} 的模型调用失败：{e}", inp.index));
                }
                sink.log(
                    "warn",
                    format!(
                        "[step {}] 第 {round} 轮模型调用失败（连续 {llm_failures}/{LLM_FAIL_LIMIT}，退避后重试）：{e}",
                        inp.index
                    ),
                );
                tokio::time::sleep(Duration::from_millis(1200 * llm_failures as u64)).await;
                continue;
            }
        };
        usage.add(&reply.usage);

        let action = match parse_step_action(&reply.content) {
            Ok(a) => a,
            Err(e) => {
                sink.log(
                    "warn",
                    format!("[step {}] 第 {round} 轮输出无法解析：{e}", inp.index),
                );
                msgs.push(ChatMessage::user(parse_failure_feedback(
                    &e,
                    reply.finish_reason.as_deref(),
                )));
                continue;
            }
        };
        msgs.push(ChatMessage::assistant(reply.content.clone()));

        if let StepAction::Final(text) = action {
            let note = step_note(&written, last_verify.as_ref());
            let report = StepReport {
                status: "done".into(),
                files: written,
                read_paths: read,
                note,
                self_report: clip(&text, SELF_REPORT_CLIP),
                error: None,
                verify: last_verify,
                rounds: round,
                usage,
                elapsed_ms: started.elapsed().as_millis(),
                trace,
            };
            sink.log("ok", clip(&report.headline(inp.step), 240));
            return Ok(report);
        }

        let (tool, brief, result): (String, String, Result<String, String>) = match action {
            StepAction::Final(_) => unreachable!("Final 在上面已经返回"),
            StepAction::Read(path) => {
                let brief = format!("read {path}");
                let r = cx.tool_read(&path);
                // 读过什么 = 父循环喂给复核员的证据集合（子步骤的 read 不进父上下文，
                // 这份清单是父唯一能知道"它查过什么"的通道）
                if r.is_ok() && !read.contains(&path) {
                    read.push(path.clone());
                }
                ("read".into(), brief, r)
            }
            StepAction::Write(path, content) => {
                let brief = format!("write {path}（{} 字节）", content.len());
                let r = cx.tool_write(&path, &content);
                if r.is_ok() && !written.contains(&path) {
                    written.push(path.clone());
                }
                ("write".into(), brief, r)
            }
            StepAction::Execute(cmd, t) => {
                let mut r = tool_execute(cx.project_root(), &cmd, t);
                // 确认模式 + 本步已有暂存改动：这条命令看不到本次修改，贴一行说明兜住
                r.push_str(staged_execute_note(cx.policy(), !cx.changes().is_empty()));
                (
                    "execute".into(),
                    format!("execute {}", clip(&cmd, 80)),
                    Ok(r),
                )
            }
            StepAction::Unsupported(name) => (
                name.to_string(),
                format!("{name}（本步不支持）"),
                Err(unsupported_reason(name)),
            ),
        };

        let ok = result.is_ok();
        let icon = if ok { "✓" } else { "✗" };
        trace.push(clip(&format!("{tool} {icon} {brief}"), TRACE_CLIP));
        sink.log(
            if ok { "info" } else { "warn" },
            clip(&format!("[step {}] {tool} {icon} {brief}", inp.index), 400),
        );

        // 机械验证（窄层）：事实触发 —— 本步真的写成了文件。与主循环同一判据、同一实现；
        // 失败当成"观察"回灌，不打断本步（它是即时反馈，交付判据在父循环的门禁里）。
        if tool == "write" && ok && cfg.gate.narrow && !is_cancelled(cancel) {
            let mine = step_changes(cx, &written);
            if !mine.is_empty() {
                let v = narrow_verify(cfg, &mine).await;
                let failed = v.status == "failed";
                sink.log(
                    if failed { "warn" } else { "info" },
                    format!("[step {}] {}", inp.index, clip(&v.verdict, 200)),
                );
                sink.verify(&v);
                if failed {
                    msgs.push(ChatMessage::user(v.to_observation()));
                }
                last_verify = Some(v);
            }
        }

        msgs.push(ChatMessage::user(json_result(result)));
    }

    // 预算用尽：不冒充完成。已产出的文件仍留在覆盖层里，父循环收尾时会照实算。
    Ok(error_report(
        format!("{max_steps} 轮内没有交付本步（预算用尽）"),
        written,
        read,
        last_verify,
        max_steps,
        usage,
        trace,
        started,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::WritePolicy;
    use crate::exec::new_cancel_flag;
    use crate::testllm::{FakeLlm, fake_llm};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    struct Quiet;
    impl Sink for Quiet {}

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn temp_project(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "harness-step-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plan_step(id: u32, title: &str, files: &[&str]) -> PlanStep {
        PlanStep {
            id,
            title: title.into(),
            detail: "把上一步留下的问题修掉".into(),
            files: files.iter().map(|s| s.to_string()).collect(),
            kind: "code".into(),
        }
    }

    /// 单测不依赖本机装没装 python：语法层默认关掉，要验它的用例自己打开
    fn cfg_for(llm: &FakeLlm) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.llm.base_url = llm.base_url.clone();
        cfg.llm.api_key = "smoke".into();
        cfg.llm.model = "fake".into();
        cfg.gate.narrow = false;
        cfg
    }

    fn js(v: &str) -> String {
        serde_json::to_string(v).unwrap()
    }

    fn body(v: &str) -> serde_json::Value {
        serde_json::from_str(v).expect("请求体该是 JSON")
    }

    /// 输入包只装本步：任务、位置、本步标题/说明/声明文件、前置步骤事实
    #[test]
    fn prompt_is_scoped_to_the_one_step() {
        let done = vec![StepSummary {
            id: 1,
            title: "读代码".into(),
            status: "done".into(),
            files: vec!["a.py".into()],
            note: "语法检查：通过：1 项检查全过".into(),
        }];
        let s = plan_step(2, "改 add", &["a.py"]);
        let p = build_user_prompt(&StepInput {
            project_root: Path::new("D:/proj"),
            task: "把 add 改对",
            step: &s,
            index: 2,
            total: 3,
            done: &done,
        });
        assert!(p.contains("第 2 / 3 步"), "{p}");
        assert!(p.contains("把 add 改对"), "{p}");
        assert!(p.contains("本步标题：改 add"), "{p}");
        assert!(p.contains("本步说明：把上一步留下的问题修掉"), "{p}");
        assert!(p.contains("本步应产出：a.py"), "{p}");
        assert!(
            p.contains("- 步骤 1「读代码」：done｜产出 a.py｜语法检查：通过"),
            "{p}"
        );
        assert!(p.contains("D:/proj"), "{p}");
        assert!(p.contains("{\"final\""), "{p}");
    }

    /// 跨步骤通道必须有界 —— 20 步的计划不能把 20 行全塞进下一步
    #[test]
    fn prior_summaries_are_bounded() {
        let done: Vec<StepSummary> = (1..=20)
            .map(|i| StepSummary {
                id: i,
                title: format!("步骤 {i}"),
                status: "done".into(),
                files: Vec::new(),
                note: String::new(),
            })
            .collect();
        let s = plan_step(21, "收尾", &[]);
        let p = build_user_prompt(&StepInput {
            project_root: Path::new("."),
            task: "长任务",
            step: &s,
            index: 21,
            total: 21,
            done: &done,
        });
        assert!(p.contains("步骤 12「步骤 12」：done"), "{p}");
        assert!(p.contains("还有 8 步没列出来"), "{p}");
        assert!(!p.contains("步骤 13"), "超上限的不许列：{p}");
        assert!(p.contains("本步应产出：（计划没声明具体文件）"), "{p}");
    }

    /// 端到端（脚本化假 LLM）：写对文件 + 上下文干净 + 与父共用同一份覆盖层
    #[test]
    fn writes_through_the_shared_overlay_with_a_clean_context() {
        let dir = temp_project("write");
        std::fs::write(dir.join("util.py"), "def add(a, b):\n    return a - b\n").unwrap();
        let good = "def add(a, b):\n    return a + b\n";
        let llm = fake_llm(vec![
            r#"{"tool":"read","args":{"path":"util.py"}}"#.into(),
            format!(
                r#"{{"tool":"write","args":{{"path":"util.py","content":{}}}}}"#,
                js(good)
            ),
            format!(r#"{{"final":{}}}"#, js("把 add 的减号改成了加号。")),
        ]);
        let cfg = cfg_for(&llm);
        let mut cx = Ctx::new(&dir, WritePolicy::Apply);
        let s = plan_step(2, "修正 add", &["util.py"]);
        let done = vec![StepSummary {
            id: 1,
            title: "读代码".into(),
            status: "done".into(),
            files: vec!["util.py".into()],
            note: "语法检查：通过".into(),
        }];
        let inp = StepInput {
            project_root: &dir,
            task: "把 util.py 的 add 改对",
            step: &s,
            index: 2,
            total: 3,
            done: &done,
        };
        let rep = block_on(run_step(
            &cfg,
            &mut cx,
            &inp,
            &new_cancel_flag(),
            None,
            &Quiet,
        ))
        .expect("不该是致命失败");

        assert_eq!(rep.status, "done");
        assert_eq!(rep.files, vec!["util.py".to_string()]);
        assert_eq!(rep.rounds, 3);
        assert!(rep.note.contains("未做语法检查"), "{}", rep.note);
        assert!(
            rep.headline(&s).contains("产出 util.py"),
            "{}",
            rep.headline(&s)
        );
        assert!(
            rep.as_summary(&s)
                .line()
                .contains("步骤 2「修正 add」：done")
        );
        assert_eq!(rep.self_report.trim(), "把 add 的减号改成了加号。");
        assert!(
            std::fs::read_to_string(dir.join("util.py"))
                .unwrap()
                .contains("a + b"),
            "Apply 策略该落到磁盘"
        );
        assert_eq!(llm.count(), 3, "剧本刚好演完，没有多余轮次");

        // ① 上下文隔离：首条请求就是 STEP_SYSTEM + 本步输入包，两条消息，没有父对话
        let first = body(&llm.request(0));
        let msgs = first["messages"].as_array().expect("messages 该是数组");
        assert_eq!(msgs.len(), 2, "子上下文从零开始：{first}");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"].as_str(), Some(STEP_SYSTEM));
        assert_eq!(msgs[1]["role"], "user");
        let user_msg = msgs[1]["content"].as_str().unwrap_or_default();
        assert!(user_msg.contains("第 2 / 3 步"), "{user_msg}");
        assert!(user_msg.contains("已完成的前置步骤"), "{user_msg}");
        // 主循环的系统提示词与它的工具说明，一条都不许出现
        assert!(!user_msg.contains("你是 ruyix IDE 里的编程 Agent"));
        assert!(!user_msg.contains("connect"));
        assert!(
            !user_msg.contains("plan"),
            "本步不该看到计划伪工具：{user_msg}"
        );

        // ② 它自己的上下文只随自己的轮次长：第 2 条请求 = system+user+assistant+user
        let second = body(&llm.request(1));
        let msgs2 = second["messages"].as_array().unwrap();
        assert_eq!(msgs2.len(), 4, "只多了它自己的回灌：{second}");
        assert!(
            msgs2[3]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("return a - b"),
            "读回来的内容在它自己的上下文里"
        );

        // ③ 覆盖层不分裂：父拿到的 changes 里就有这一步写的文件
        assert_eq!(cx.changes().len(), 1);
        assert_eq!(cx.changes()[0].path, "util.py");
        assert!(cx.changes()[0].after.contains("a + b"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 确认模式（Stage）：上下文必须说清"写入只暂存、execute 看到的是改动前的文件" ——
    /// 不说这条，子步会像父循环那样拿 execute 的失败反复"修"一个已经写对的改动
    /// （实测 run `agent-20260920-152312`：65 轮里 45+ 轮就是这种空转）。
    #[test]
    fn confirm_mode_is_declared_in_the_step_context() {
        let dir = temp_project("stage-note");
        std::fs::write(dir.join("util.py"), "def add(a, b):\n    return a - b\n").unwrap();
        let good = "def add(a, b):\n    return a + b\n";
        let llm = fake_llm(vec![
            format!(
                r#"{{"tool":"write","args":{{"path":"util.py","content":{}}}}}"#,
                js(good)
            ),
            // 紧接着跑一次 execute —— 这正是原来会点燃"再修一次"死循环的那一步
            r#"{"tool":"execute","args":{"cmd":"findstr a util.py"}}"#.into(),
            format!(r#"{{"final":{}}}"#, js("改成 a + b，已暂存待确认。")),
        ]);
        let cfg = cfg_for(&llm);
        let mut cx = Ctx::new(&dir, WritePolicy::Stage);
        let s = plan_step(1, "修正 add", &["util.py"]);
        let inp = StepInput {
            project_root: &dir,
            task: "把 util.py 的 add 改对",
            step: &s,
            index: 1,
            total: 1,
            done: &[],
        };
        let rep = block_on(run_step(
            &cfg,
            &mut cx,
            &inp,
            &new_cancel_flag(),
            None,
            &Quiet,
        ))
        .expect("不该是致命失败");
        assert_eq!(rep.status, "done");

        // ① 首条 user 消息（本步输入包）里带着确认模式说明
        let first = body(&llm.request(0));
        let user_msg = first["messages"][1]["content"].as_str().unwrap_or_default();
        assert!(user_msg.contains("确认模式"), "{user_msg}");
        assert!(user_msg.contains("暂存"), "{user_msg}");
        assert!(user_msg.contains("不要用 execute"), "{user_msg}");

        // ② 确认模式不许碰项目磁盘：文件还是旧的
        assert!(
            std::fs::read_to_string(dir.join("util.py"))
                .unwrap()
                .contains("a - b"),
            "Stage 策略不该改磁盘"
        );

        // ③ execute 的结果里贴了"看到的是改动前文件"的说明（兜住模型不读提示的情况）
        let third = body(&llm.request(2));
        let exec_result = third["messages"]
            .as_array()
            .and_then(|m| m.last())
            .and_then(|m| m["content"].as_str())
            .unwrap_or_default();
        assert!(exec_result.contains("改动前"), "{exec_result}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 窄验证绑在"本步真的写成了文件"这条事实上（与主循环同一判据）：
    /// 打开它就必须留下一条结论。本机装没装 python 都能过 —— 装了就真跑，
    /// 没装就是 skipped，但**不许静默无结论**。
    #[test]
    fn a_narrow_check_leaves_a_verdict_when_it_is_on() {
        let dir = temp_project("verify");
        let llm = fake_llm(vec![
            format!(
                r#"{{"tool":"write","args":{{"path":"hello.py","content":{}}}}}"#,
                js("def hi():\n    return \"hi\"\n")
            ),
            format!(r#"{{"final":{}}}"#, js("写好 hello.py。")),
        ]);
        let mut cfg = cfg_for(&llm);
        cfg.gate.narrow = true;
        cfg.gate.staged_timeout_secs = 10;
        let mut cx = Ctx::new(&dir, WritePolicy::Apply);
        let s = plan_step(1, "写 hello.py", &["hello.py"]);
        let inp = StepInput {
            project_root: &dir,
            task: "写一个 hello 模块",
            step: &s,
            index: 1,
            total: 1,
            done: &[],
        };
        let rep = block_on(run_step(
            &cfg,
            &mut cx,
            &inp,
            &new_cancel_flag(),
            None,
            &Quiet,
        ))
        .expect("不该失败");

        assert_eq!(rep.status, "done");
        let v = rep.verify.as_ref().expect("开了窄验证就必须有结论");
        assert_eq!(v.layer, "narrow");
        assert!(rep.note.contains("语法检查："), "{}", rep.note);
        // 验证结论同时交给父循环（UI 的「验证」小节靠它）
        assert_eq!(llm.count(), 2, "验证不该额外多烧一轮模型调用");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// plan / connect 在步骤里被明确拒绝（不整步失败），模型看得到替代动作
    #[test]
    fn plan_and_connect_are_refused_inside_a_step() {
        let dir = temp_project("reject");
        let llm = fake_llm(vec![
            r#"{"tool":"plan","args":{"steps":[{"title":"再拆一步","files":["x.py"]}]}}"#.into(),
            r#"{"tool":"connect","args":{"action":"list"}}"#.into(),
            format!(r#"{{"final":{}}}"#, js("两种能力都用不了，直接交付本步。")),
        ]);
        let cfg = cfg_for(&llm);
        let mut cx = Ctx::new(&dir, WritePolicy::Apply);
        let s = plan_step(1, "看看项目", &[]);
        let inp = StepInput {
            project_root: &dir,
            task: "看看项目",
            step: &s,
            index: 1,
            total: 1,
            done: &[],
        };
        let rep = block_on(run_step(
            &cfg,
            &mut cx,
            &inp,
            &new_cancel_flag(),
            None,
            &Quiet,
        ))
        .expect("不该失败");

        assert_eq!(rep.status, "done");
        assert_eq!(llm.count(), 3);
        let after_plan = llm.request(1);
        assert!(after_plan.contains("没有 plan"), "{after_plan}");
        assert!(
            after_plan.contains("read / write / execute"),
            "{after_plan}"
        );
        let after_connect = llm.request(2);
        assert!(after_connect.contains("没有 connect"), "{after_connect}");
        assert!(
            rep.trace
                .iter()
                .any(|t| t.contains("plan") && t.contains("✗")),
            "{:?}",
            rep.trace
        );
    }

    /// 预算用尽：落 error，绝不冒充 done
    #[test]
    fn budget_exhaustion_is_reported_never_dressed_up_as_done() {
        let dir = temp_project("budget");
        let llm = fake_llm(vec![
            r#"{"tool":"read","args":{"path":"."}}"#.into(),
            r#"{"tool":"read","args":{"path":"."}}"#.into(),
        ]);
        let mut cfg = cfg_for(&llm);
        cfg.step.max_steps = 2;
        let mut cx = Ctx::new(&dir, WritePolicy::Apply);
        let s = plan_step(1, "一直读不写", &["x.py"]);
        let inp = StepInput {
            project_root: &dir,
            task: "做点事",
            step: &s,
            index: 1,
            total: 1,
            done: &[],
        };
        let rep = block_on(run_step(
            &cfg,
            &mut cx,
            &inp,
            &new_cancel_flag(),
            None,
            &Quiet,
        ))
        .expect("不该失败");

        assert_eq!(rep.status, "error");
        assert!(!rep.ok());
        let err = rep.error.as_deref().unwrap_or_default();
        assert!(err.contains("2 轮") && err.contains("预算用尽"), "{err}");
        assert_eq!(rep.rounds, 2);
        assert!(
            rep.as_summary(&s).line().contains("失败："),
            "下一步要知道上一步没成"
        );
    }

    /// 取消位已置：一个请求都不发
    #[test]
    fn cancel_is_honored_before_any_request() {
        let dir = temp_project("cancel");
        let llm = fake_llm(Vec::new());
        let cfg = cfg_for(&llm);
        let mut cx = Ctx::new(&dir, WritePolicy::Apply);
        let s = plan_step(1, "做点事", &[]);
        let inp = StepInput {
            project_root: &dir,
            task: "做点事",
            step: &s,
            index: 1,
            total: 1,
            done: &[],
        };
        let flag = new_cancel_flag();
        flag.store(true, Ordering::Relaxed);
        let rep =
            block_on(run_step(&cfg, &mut cx, &inp, &flag, None, &Quiet)).expect("取消不是致命错误");

        assert_eq!(rep.status, "error");
        assert!(rep.error.as_deref().unwrap_or_default().contains("取消"));
        assert_eq!(llm.count(), 0, "取消后一个请求都不发");
    }

    /// 致命模型错误上抛（不伪装成"这一步失败了"）—— 没配 Key 时 `llm::chat` 立即返回 Err
    #[test]
    fn a_fatal_model_error_propagates() {
        let dir = temp_project("fatal");
        let mut cfg = AppConfig::default();
        cfg.llm.api_key = String::new();
        let mut cx = Ctx::new(&dir, WritePolicy::Apply);
        let s = plan_step(1, "做点事", &[]);
        let inp = StepInput {
            project_root: &dir,
            task: "做点事",
            step: &s,
            index: 1,
            total: 1,
            done: &[],
        };
        let err = block_on(run_step(
            &cfg,
            &mut cx,
            &inp,
            &new_cancel_flag(),
            None,
            &Quiet,
        ))
        .unwrap_err();
        assert!(err.contains("API Key"), "{err}");
        assert!(err.contains("步骤 1"), "{err}");
    }

    /// 父的步骤动作只认三种：可用的原样透传，plan / connect 收窄成不支持
    #[test]
    fn step_action_narrows_the_primary_loop_abilities() {
        use crate::agent::parse_step_action as parse;
        assert!(matches!(
            parse(r#"{"tool":"read","args":{"path":"a.py"}}"#).unwrap(),
            StepAction::Read(p) if p == "a.py"
        ));
        assert!(matches!(
            parse(r#"{"tool":"write","args":{"path":"a.py","content":"x"}}"#).unwrap(),
            StepAction::Write(_, _)
        ));
        assert!(matches!(
            parse(r#"{"tool":"execute","args":{"cmd":"ls"}}"#).unwrap(),
            StepAction::Execute(c, _) if c == "ls"
        ));
        assert!(matches!(
            parse(r#"{"final":"好了"}"#).unwrap(),
            StepAction::Final(t) if t == "好了"
        ));
        assert!(matches!(
            parse(r#"{"tool":"plan","args":{"steps":[{"title":"x"}]}}"#).unwrap(),
            StepAction::Unsupported("plan")
        ));
        assert!(matches!(
            parse(r#"{"tool":"connect","args":{"action":"list"}}"#).unwrap(),
            StepAction::Unsupported("connect")
        ));
        // 格式烂仍然是要重发的错，不该被当成"不支持的能力"
        assert!(parse("随便聊聊").is_err());
    }
}
