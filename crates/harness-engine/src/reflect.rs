//! 反思（复核）agent：**另一个上下文干净的 agent**（v0.3）。
//!
//! 为什么必须新开上下文：主循环的 messages 里有推理轨迹、失败尝试、被推翻的假设 ——
//! 让它在同一个上下文里复核自己，等于让自己给自己背书。这里只喂**客观产物**：
//! 任务原文 + 改动内容（或主循环的答案）+ 机械验证报告 + 它读过的文件清单；
//! 复核员想看别的文件，自己用 `read` 去读。
//!
//! 三条纪律：
//! 1. **只读**：本模块没有任何通向 write / execute / connect 的路径（构造性保证，
//!    不是靠提示词"请不要写"）。
//! 2. **可证伪**：输出是结构化 findings（claim + evidence + verdict），不是自由文本意见；
//!    没证据就写 unknown。空口白话在回灌时会被主循环当成"未给结论"。
//! 3. **不阻断**：复核失败 / 超时 / 输出解析不了，一律降级成带 `note` 的结论，
//!    绝不让整个 run 失败（复核是锦上添花，不是交付前提）。
//!
//! 判据表（rubric）由**事实**选，不用分类器：这一轮改了文件 → 审产物；没改 → 核依据。
//! 这正好对上"问答也会迷航"那条：没依据的断言被逐条标出来。

use crate::agent::{AgentOutcome, FileChange, VerifyOutcome};
use crate::config::AppConfig;
use crate::exec::{CancelFlag, clip, is_cancelled};
use crate::generate::safe_rel_path;
use crate::llm::{self, ChatMessage, Usage};
use crate::pipeline::Sink;
use serde::{Deserialize, Serialize};
use std::path::Path;

const READ_CLIP: usize = 8_000;
const CHANGE_CLIP: usize = 4_000;
const BEFORE_CLIP: usize = 1_500;
/// 提示词里最多逐字列出几个改动文件的完整内容（其余让它自己 read）
const MAX_INLINE_FILES: usize = 6;
/// 【本轮取证】最多列几条命令（复核的上下文也得省着用）
const MAX_PROBES: usize = 8;

pub const REFLECT_SYSTEM: &str = r#"你是 ruyix 的复核员：**独立于**干活的 Agent，只负责挑毛病。你看到的是产物、验证报告与改过的文件，看不到它的推理过程 —— 这是刻意的。

怎么复核（按给你的"本轮判据"选一套）：
- 【产物评审】任务要求是否条条落地？边界与错误路径考虑了吗？会不会破坏既有行为（看调用方）？测试是否真的证明了要证明的行为（而不是自证）？
- 【依据核对】答复里每一条断言，是否都能追到「读过的文件 + **本轮命令取证** + **服务端联网检索** + 机械验证报告」里的证据？追不到的必须标出来。
  关键是别把"证据池里没有"当成"主循环没做"：日期、端口、进程、环境变量这类事实本来就只有命令能给，
  它们已经列在【本轮取证】里 —— 有就核对值对不对，**不许因为你自己不掌握别的来源就要求它再取一次证**。
  联网检索同理：模型开着服务端联网，检索由**服务端**执行、结果直接进上下文，标题与链接不回传给你，
  你只看得到它检索时用的查询词（【本轮取证】里标着"联网检索（服务端执行）"的那条）。
  看到查询词与目标问题对得上，就当作**已取证**，转而核对值本身合不合理；
  **不许因为没有 URL 就判 unsupported，也不许要求它把网页抓下来再答一遍。**

可用工具（只有这一个）：
- read 读项目文件：{"tool":"read","args":{"path":"相对路径"}} —— 判断"是否破坏调用方"这类问题必须自己去读，不许凭空推测。

结论按这个形状直接输出（不要包裹代码块，不要多余文字）：
{"verdict":"ok|suspect","summary":"一句话","findings":[{"severity":"high|medium|low","claim":"问题一句话","evidence":"文件:行 或 命令输出片段；没证据就写 unknown","verdict":"supported|unsupported|unknown","suggest":"建议怎么改"}]}

规则：
1. verdict=ok 表示"没发现值得打断交付的问题"；findings 必须为空。
2. 只有你能指出**具体位置或具体证据**时才写进 findings；拿不准的写进 summary，别当结论。
3. 不许提出"更进一步优化""加更多测试"这类没有边界的建议 —— 只报与本次任务相关、可指出位置的问题。
4. 你的结论会回到干活的那个 Agent 手里让它自己修，所以每条 findings 都要能落地。"#;

/// 复核的判据表（由事实选：改了东西就审产物，没改就核依据）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rubric {
    /// 有改动 → 审产物
    Artifact,
    /// 无改动 → 核依据
    Evidence,
}

impl Rubric {
    pub fn label(&self) -> &'static str {
        match self {
            Rubric::Artifact => "产物评审",
            Rubric::Evidence => "依据核对",
        }
    }
}

pub fn select_rubric(has_changes: bool) -> Rubric {
    if has_changes {
        Rubric::Artifact
    } else {
        Rubric::Evidence
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Finding {
    /// high | medium | low
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub claim: String,
    /// 证据（文件:行 / 命令输出片段）；没有就写 unknown
    #[serde(default)]
    pub evidence: String,
    /// supported | unsupported | unknown
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub suggest: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Reflection {
    /// ok | suspect | unknown（unknown = 复核没给出可用结论）
    pub verdict: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// 复核没跑成的原因（降级时才有：模型调用失败 / 超时 / 输出解析不了）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Reflection {
    /// 复核判定"有问题"（门禁据此把结论回灌主循环）
    pub fn suspect(&self) -> bool {
        self.verdict == "suspect"
    }

    /// 降级：复核没跑成（不阻断交付，但要在 UI 上说清楚）
    pub fn degraded(&self) -> bool {
        self.note.is_some()
    }

    /// 回灌主循环的观察文本：结构化、可落地、每条都带证据位置
    pub fn to_observation(&self) -> String {
        let mut s = String::from("独立复核（干净上下文）发现以下问题，请自查并修正后重新交付：\n");
        for f in &self.findings {
            let sev = if f.severity.trim().is_empty() {
                "?"
            } else {
                f.severity.trim()
            };
            s.push_str(&format!(
                "- [{}] {}（证据：{}；判定：{}）\n",
                sev,
                f.claim.trim(),
                if f.evidence.trim().is_empty() {
                    "unknown"
                } else {
                    f.evidence.trim()
                },
                if f.verdict.trim().is_empty() {
                    "unknown"
                } else {
                    f.verdict.trim()
                }
            ));
            if !f.suggest.trim().is_empty() {
                s.push_str(&format!("  建议：{}\n", f.suggest.trim()));
            }
        }
        if !self.summary.trim().is_empty() {
            s.push_str(&format!("复核总结：{}\n", self.summary.trim()));
        }
        s.push_str(
            "（若你认为复核判断有误：先 read 相关文件核对，确认无误后再交付，并在最终答复里说明为何不采纳。）\n",
        );
        // 被打回时最自然的反应是"把原始输出整段贴进 final 自证" —— 那正好把 JSON 撑到
        // 手写转义出错，白丢一轮（实测过）。明确说：证据查你的调用记录，答复里只引用。
        s.push_str(
            "别用「把命令输出 / 日志整段抄进 final」来证明自己 —— 那些在你的调用结果里，\
             用户也能在面板看到；贴进去只会把 JSON 撑坏。要举证就摘那一行事实。",
        );
        s
    }
}

/// 复核的输入包 —— 干净上下文里**唯一**允许出现的东西
#[derive(Clone, Debug)]
pub struct ReflectInput<'a> {
    pub task: &'a str,
    pub rubric: Rubric,
    pub project_root: &'a Path,
    pub changes: &'a [FileChange],
    /// 主循环这一轮读过哪些文件（依据核对的"证据集合"）
    pub read_paths: &'a [String],
    /// 主循环这一轮跑过哪些命令、各自输出了什么（**依据核对的另一半证据集合**）
    ///
    /// 没有它，"今天星期几""端口谁占着""进程还在不在"这类**只能靠命令取证**的答复，
    /// 断言会被逐条判"无处可查" —— 而主循环再跑十条命令也仍然喂不进这里，成了死循环。
    pub probes: &'a [crate::agent::Probe],
    /// 主循环准备交付的答复（无改动场景用它做依据核对）
    pub answer: Option<&'a str>,
    pub verifications: &'a [VerifyOutcome],
    /// 门禁那侧的补充说明（例如"确认模式下全量验证没跑"）
    pub gate_note: Option<String>,
}

/// 组装复核用的用户消息（纯函数，可单测）
pub fn build_user_prompt(inp: &ReflectInput<'_>) -> String {
    let mut s = String::new();
    s.push_str(&format!("项目根目录：{}\n", inp.project_root.display()));
    s.push_str(&format!("本轮判据：{}\n\n", inp.rubric.label()));
    s.push_str(&format!("【任务原文】\n{}\n\n", inp.task.trim()));

    match inp.rubric {
        Rubric::Artifact => {
            s.push_str(&format!("【本轮改动】共 {} 个文件\n", inp.changes.len()));
            for (i, c) in inp.changes.iter().enumerate() {
                if i >= MAX_INLINE_FILES {
                    s.push_str(&format!(
                        "- …还有 {} 个文件没列出来（你自己用 read 看）\n",
                        inp.changes.len() - MAX_INLINE_FILES
                    ));
                    break;
                }
                s.push_str(&format!("- {}（{}）\n", c.path, c.kind));
                if let Some(b) = &c.before {
                    s.push_str(&format!("  改动前：\n{}\n", indent(&clip(b, BEFORE_CLIP))));
                } else {
                    s.push_str("  改动前：（新文件）\n");
                }
                s.push_str(&format!(
                    "  改动后：\n{}\n",
                    indent(&clip(&c.after, CHANGE_CLIP))
                ));
            }
            s.push('\n');
        }
        Rubric::Evidence => {
            s.push_str("【本轮答复（待核对依据）】\n");
            s.push_str(inp.answer.unwrap_or("（空）").trim());
            s.push_str("\n\n");
            s.push_str("【它这一轮读过的文件】\n");
            if inp.read_paths.is_empty() {
                s.push_str("（一个都没读）\n");
            } else {
                for p in inp.read_paths {
                    s.push_str(&format!("- {p}\n"));
                }
            }
            s.push('\n');
            // 命令取证：日期 / 端口 / 进程 / 环境变量这类事实只有这条路能给。
            // 漏掉它 = 复核员只能判"证据无处可查"（见 ReflectInput::probes）
            s.push_str("【本轮取证（跑过的命令与实际输出）】\n");
            if inp.probes.is_empty() {
                s.push_str("（一条命令都没跑）\n");
            } else {
                let skip = inp.probes.len().saturating_sub(MAX_PROBES);
                if skip > 0 {
                    s.push_str(&format!("（前面 {skip} 条已略去，只列最近的）\n"));
                }
                for p in inp.probes.iter().skip(skip) {
                    s.push_str(&format!("$ {}\n{}\n\n", p.cmd, indent(&p.output)));
                }
            }
        }
    }

    s.push_str("【机械验证报告】\n");
    if inp.verifications.is_empty() {
        s.push_str("（本轮没有跑验证）\n");
    } else {
        for v in inp.verifications {
            s.push_str(&format!(
                "- {}：{}{}\n",
                v.layer_label(),
                v.verdict,
                v.skipped_reason
                    .as_ref()
                    .map(|r| format!("（跳过原因：{r}）"))
                    .unwrap_or_default()
            ));
            for c in v.checks.iter().filter(|c| c.status == "failed") {
                s.push_str(&format!("  ✗ {} {}：{}\n", c.kind, c.target, c.reason));
            }
        }
    }
    if let Some(n) = &inp.gate_note {
        s.push_str(&format!("补充说明：{n}\n"));
    }
    s.push_str("\n请按上面选定的判据复核，并只输出一个 JSON 结论。");
    s
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析复核结论。容错但**不猜**：verdict 认不出来时按 findings 决定
/// （有 findings = suspect；没有 = unknown，当"没给出结论"处理）。
pub fn parse_reflection(raw: &str) -> Result<Reflection, String> {
    let json = llm::extract_json_object(raw);
    let v: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("不是合法 JSON：{e}"))?;
    let Some(obj) = v.as_object() else {
        return Err("不是一个 JSON 对象".into());
    };
    let mut findings: Vec<Finding> = Vec::new();
    if let Some(arr) = obj.get("findings").and_then(|f| f.as_array()) {
        for item in arr {
            let f: Finding = serde_json::from_value(item.clone())
                .map_err(|e| format!("findings 项解析失败：{e}"))?;
            if !f.claim.trim().is_empty() {
                findings.push(f);
            }
        }
    }
    let raw_verdict = obj
        .get("verdict")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let verdict = match raw_verdict.as_str() {
        "ok" | "pass" | "passed" | "good" => "ok",
        "suspect" | "fail" | "failed" | "issue" | "bad" => "suspect",
        _ => {
            if findings.is_empty() {
                "unknown"
            } else {
                "suspect"
            }
        }
    };
    Ok(Reflection {
        verdict: verdict.into(),
        summary: obj
            .get("summary")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        findings,
        note: None,
    })
}

/// 只读的 path 解析：`{"tool":"read","args":{"path":"…"}}`
/// - Ok(Some(path)) 正常
/// - Ok(None) 不是工具调用（模型可能想给结论但格式不对）
/// - Err 用了别的工具（复核只允许 read）
fn parse_read(raw: &str) -> Result<Option<String>, String> {
    let json = llm::extract_json_object(raw);
    let v: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("不是合法 JSON：{e}"))?;
    let Some(tool) = v.get("tool").and_then(|t| t.as_str()) else {
        return Ok(None);
    };
    if !tool.trim().eq_ignore_ascii_case("read") {
        return Err(format!("复核只允许 read，收到 {tool:?}"));
    }
    let path = v
        .get("args")
        .and_then(|a| a.get("path"))
        .and_then(|p| p.as_str())
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "read 缺少 args.path".to_string())?;
    Ok(Some(path))
}

fn read_project_file(proj: &Path, raw: &str) -> Result<String, String> {
    let rel = safe_rel_path(raw)?;
    let p = proj.join(&rel);
    if p.is_dir() {
        let mut listing = String::from("--- 目录结构 ---\n");
        let mut n = 0usize;
        crate::repair::list_dir(proj, &rel, 0, &mut n, &mut listing);
        return Ok(listing);
    }
    if !p.is_file() {
        return Err(format!("{rel} 不存在"));
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("读不到 {rel}：{e}"))?;
    Ok(format!("--- {rel} ---\n{}", clip(&text, READ_CLIP)))
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

#[derive(Clone, Debug, Default)]
pub struct ReflectOutcome {
    pub reflection: Reflection,
    pub usage: Usage,
}

fn degraded(reason: String) -> Reflection {
    Reflection {
        verdict: "unknown".into(),
        summary: String::new(),
        findings: Vec::new(),
        note: Some(reason),
    }
}

/// 跑一次复核：新开 messages（只有 system + 输入包），只允许 read，直到给出结论。
pub async fn run(
    cfg: &AppConfig,
    inp: &ReflectInput<'_>,
    cancel: &CancelFlag,
    sink: &dyn Sink,
) -> ReflectOutcome {
    let mut usage = Usage::default();
    // P6：复核可以单独指定模型（空 = 与主循环同模型）
    let mut llm_cfg = cfg.llm.clone();
    let model_override = cfg.reflect.model.trim();
    if !model_override.is_empty() {
        llm_cfg.model = model_override.to_string();
    }
    let mut msgs = vec![
        ChatMessage::system(REFLECT_SYSTEM),
        ChatMessage::user(build_user_prompt(inp)),
    ];
    let max_steps = cfg.reflect.max_steps.max(1);

    for step in 1..=max_steps {
        if is_cancelled(cancel) {
            return ReflectOutcome {
                reflection: degraded("用户取消，复核中断".into()),
                usage,
            };
        }
        sink.log("info", format!("[reflect] 第 {step} 轮复核"));
        let reply = match llm::chat(&llm_cfg, &msgs, true).await {
            Ok(r) => r,
            Err(e) => {
                return ReflectOutcome {
                    reflection: degraded(format!("复核模型调用失败：{e}")),
                    usage,
                };
            }
        };
        usage.add(&reply.usage);

        // ① 结论优先（模型可以直接给 verdict，不必先 read）
        if let Ok(r) = parse_reflection(&reply.content) {
            sink.log(
                "ok",
                format!(
                    "[reflect] 结论：{}（{} 条 findings）",
                    r.verdict,
                    r.findings.len()
                ),
            );
            return ReflectOutcome {
                reflection: r,
                usage,
            };
        }

        // ② 只读工具：read；别的工具一律打回
        msgs.push(ChatMessage::assistant(reply.content.clone()));
        let nudge = match parse_read(&reply.content) {
            Ok(Some(path)) => match read_project_file(inp.project_root, &path) {
                Ok(text) => {
                    sink.log("info", format!("[reflect] read {path}"));
                    format!("{{\"ok\":true,\"result\":{}}}", json_str(&text))
                }
                Err(e) => format!("{{\"ok\":false,\"error\":{}}}", json_str(&e)),
            },
            Ok(None) => "{\"ok\":false,\"error\":\"请只输出两种情况之一：{\\\"tool\\\":\\\"read\\\",\\\"args\\\":{\\\"path\\\":\\\"相对路径\\\"}} 去看文件，或直接给出 {\\\"verdict\\\":\\\"ok|suspect\\\",…} 结论。\"}".to_string(),
            Err(e) => format!("{{\"ok\":false,\"error\":{}}}", json_str(&e)),
        };
        msgs.push(ChatMessage::user(nudge));
    }

    ReflectOutcome {
        reflection: degraded(format!("复核在 {max_steps} 轮内没有给出结论")),
        usage,
    }
}

/// 复核结论 → AgentOutcome 里的汇总（给 UI 的一句话）
pub fn summarize(out: &AgentOutcome) -> Option<String> {
    out.reflections.last().map(|r| {
        if let Some(n) = &r.note {
            format!("复核未完成：{n}")
        } else if r.suspect() {
            format!("复核发现 {} 个问题", r.findings.len())
        } else {
            format!(
                "复核通过（{}）",
                if r.summary.is_empty() {
                    "ok"
                } else {
                    &r.summary
                }
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rubric_follows_the_facts_not_a_classifier() {
        assert_eq!(select_rubric(true), Rubric::Artifact);
        assert_eq!(select_rubric(false), Rubric::Evidence);
        assert_eq!(Rubric::Artifact.label(), "产物评审");
        assert_eq!(Rubric::Evidence.label(), "依据核对");
    }

    /// 容错但不猜：ok 认得出；异体词按 findings 归位；垃圾输出报错
    #[test]
    fn parse_reflection_is_tolerant_but_not_guessing() {
        let ok = parse_reflection(r#"{"verdict":"ok","summary":"没毛病","findings":[]}"#).unwrap();
        assert_eq!(ok.verdict, "ok");
        assert!(!ok.suspect());

        // 异体词 + 有 findings → suspect（宁可拦一下，也不放过）
        let weird = parse_reflection(
            r#"{"verdict":"nope","findings":[{"claim":"少了参数校验","evidence":"a.py:12","verdict":"unsupported"}]}"#,
        )
        .unwrap();
        assert!(weird.suspect());
        assert_eq!(weird.findings.len(), 1);

        // 异体词 + 没 findings → unknown（当"没给结论"处理，不拦）
        let unknown = parse_reflection(r#"{"verdict":"dunno","findings":[]}"#).unwrap();
        assert_eq!(unknown.verdict, "unknown");
        assert!(!unknown.suspect());

        // 代码块围栏也要能剥
        let fenced = parse_reflection(
            "```json\n{\"verdict\":\"suspect\",\"summary\":\"x\",\"findings\":[]}\n```",
        )
        .unwrap();
        assert!(fenced.suspect());

        assert!(parse_reflection("随便聊聊").is_err());
        assert!(parse_reflection("[1,2,3]").is_err());
        // claim 为空的 findings 直接丢掉（不许拿空条目凑数）
        let empty =
            parse_reflection(r#"{"verdict":"ok","findings":[{"claim":"","severity":"high"}]}"#)
                .unwrap();
        assert!(empty.findings.is_empty());
    }

    #[test]
    fn read_parser_only_allows_read() {
        assert_eq!(
            parse_read(r#"{"tool":"read","args":{"path":"src/main.rs"}}"#).unwrap(),
            Some("src/main.rs".into())
        );
        assert!(parse_read(r#"{"verdict":"ok"}"#).unwrap().is_none());
        assert!(parse_read(r#"{"tool":"write","args":{"path":"a"}}"#).is_err());
        assert!(parse_read(r#"{"tool":"read","args":{}}"#).is_err());
        assert!(
            parse_read(r#"{"tool":"read","args":{"path":"a"}}"#)
                .unwrap()
                .is_some()
        );
        assert!(parse_read("不是 JSON").is_err());
    }

    /// 回灌文本必须带证据位置与建议 —— 主循环照着它就能定位
    #[test]
    fn observation_carries_evidence_and_suggestion() {
        let r = Reflection {
            verdict: "suspect".into(),
            summary: "边界没处理".into(),
            findings: vec![Finding {
                severity: "high".into(),
                claim: "除零没有保护".into(),
                evidence: "calc.py:9".into(),
                verdict: "unsupported".into(),
                suggest: "加 if 判断".into(),
            }],
            note: None,
        };
        let text = r.to_observation();
        assert!(text.contains("calc.py:9"), "{text}");
        assert!(text.contains("加 if 判断"), "{text}");
        assert!(text.contains("边界没处理"), "{text}");
    }

    /// 输入包：产物评审列改动内容；依据核对列答复 + 读过的文件
    #[test]
    fn prompt_carries_the_right_evidence_per_rubric() {
        let proj = Path::new("D:/proj");
        let changes = vec![FileChange {
            path: "a.py".into(),
            kind: "modify".into(),
            before: Some("x = 1\n".into()),
            after: "x = 2\n".into(),
        }];
        let ver = vec![VerifyOutcome {
            layer: "full".into(),
            status: "failed".into(),
            verdict: "未通过：1 项失败".into(),
            failed: 1,
            passed_checks: 0,
            skipped: 0,
            hint: "test_calc 失败".into(),
            skipped_reason: None,
            checks: Vec::new(),
            elapsed_ms: 12,
        }];
        let artifact = build_user_prompt(&ReflectInput {
            task: "把 x 改成 2",
            rubric: Rubric::Artifact,
            project_root: proj,
            changes: &changes,
            read_paths: &[],
            probes: &[],
            answer: None,
            verifications: &ver,
            gate_note: None,
        });
        assert!(artifact.contains("本轮判据：产物评审"), "{artifact}");
        assert!(artifact.contains("x = 2"), "{artifact}");
        assert!(artifact.contains("未通过：1 项失败"), "{artifact}");

        let reads = vec!["src/main.rs".to_string()];
        let probes = vec![probe("date /t", "2026/09/22 周二")];
        let evidence = build_user_prompt(&ReflectInput {
            task: "这个项目用什么框架",
            rubric: Rubric::Evidence,
            project_root: proj,
            changes: &[],
            read_paths: &reads,
            probes: &probes,
            answer: Some("用的是 Axum"),
            verifications: &[],
            gate_note: None,
        });
        assert!(evidence.contains("本轮判据：依据核对"), "{evidence}");
        assert!(evidence.contains("用的是 Axum"), "{evidence}");
        assert!(evidence.contains("src/main.rs"), "{evidence}");
        assert!(evidence.contains("（本轮没有跑验证）"), "{evidence}");
        // 取证必须看得见 —— 这正是之前死锁的那条通道
        assert!(evidence.contains("【本轮取证"), "{evidence}");
        assert!(evidence.contains("$ date /t"), "{evidence}");
        assert!(evidence.contains("2026/09/22 周二"), "{evidence}");
    }

    /// 测试用的 runtime。`enable_all`：复核要连假 LLM（`testllm` 是台真 HTTP 服务器）
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// 纪律 1（只读）在代码层的钉子：复核路径只认 read，别的工具一律拒绝。
    /// 这不是靠提示词"请不要写"—— 这里根本没有通向 write / execute / connect 的代码路径。
    #[test]
    fn reflect_has_no_write_path() {
        for bad in ["write", "execute", "connect", "plan"] {
            let raw = format!(r#"{{"tool":"{bad}","args":{{"path":"a.py","content":"x"}}}}"#);
            assert!(parse_read(&raw).is_err(), "复核不该允许 {bad}");
        }
        assert!(REFLECT_SYSTEM.contains("{\"tool\":\"read\""));
        assert!(REFLECT_SYSTEM.contains("只有这一个"));
    }

    /// 纪律 3（不阻断）：复核跑不成只降级，绝不把整个 run 判死。
    /// 没配 API Key 时 `llm::chat` 立即返回 Err，所以这条不联网。
    #[test]
    fn reflect_degrades_instead_of_killing_the_run() {
        struct Quiet;
        impl crate::pipeline::Sink for Quiet {}

        let mut cfg = AppConfig::default();
        cfg.llm.api_key = String::new();
        let inp = ReflectInput {
            task: "做点事",
            rubric: Rubric::Artifact,
            project_root: Path::new("."),
            changes: &[],
            read_paths: &[],
            probes: &[],
            answer: None,
            verifications: &[],
            gate_note: None,
        };
        let out = block_on(run(&cfg, &inp, &crate::exec::new_cancel_flag(), &Quiet));
        assert!(out.reflection.degraded(), "必须降级而不是报错");
        assert!(!out.reflection.suspect(), "降级不能被当成「有问题」");
        assert_eq!(out.usage.total_tokens, 0);
    }

    /// 取消位：进循环前就置位 → 直接降级为"用户取消"，一个请求都不发
    #[test]
    fn reflect_honors_cancel_flag() {
        struct Quiet;
        impl crate::pipeline::Sink for Quiet {}

        let mut cfg = AppConfig::default();
        cfg.llm.api_key = "should-not-be-used".into();
        let flag = crate::exec::new_cancel_flag();
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let inp = ReflectInput {
            task: "做点事",
            rubric: Rubric::Evidence,
            project_root: Path::new("."),
            changes: &[],
            read_paths: &[],
            probes: &[],
            answer: Some("答案"),
            verifications: &[],
            gate_note: None,
        };
        let out = block_on(run(&cfg, &inp, &flag, &Quiet));
        assert!(out.reflection.degraded());
        assert!(
            out.reflection.note.as_deref().unwrap().contains("取消"),
            "{:?}",
            out.reflection.note
        );
    }

    fn probe(cmd: &str, output: &str) -> crate::agent::Probe {
        crate::agent::Probe {
            cmd: cmd.into(),
            output: output.into(),
        }
    }

    /// **那条死锁的正门**（原来是必须先修的 bug）：靠命令取证的任务（今天星期几 / 端口 /
    /// 进程在不在），答复里的断言以前必然被判"无处可查" —— 复核员的证据池里只有读过的
    /// 文件，命令输出用完即弃，主循环再跑十条命令也喂不进来。
    ///
    /// 这里断言两件事：① 复核员**真收到**了命令输出（不是代码里存着就算）；
    /// ② 有输出可核对时它判 ok，不再打回。
    #[test]
    fn evidence_rubric_accepts_server_side_web_search() {
        // 服务端联网是**黑盒注入**：结果直接进上下文，标题与链接都不回传，复核员只看得到
        // 查询词。它若不知道有这条路，就会把"模型知道最近的事"判成 unsupported，
        // 于是主循环被反复打回 —— 这就是刚修好的那个死锁换个入口重演。
        // 钉的是**契约串本身**：agent.rs 记取证时用的就是这个标记，提示词里少一个字
        // 复核员就认不出那条是联网（只写"联网检索"四个字会被别处的同一词组骗过去）
        assert!(
            REFLECT_SYSTEM.contains(crate::agent::WEB_SEARCH_PROBE_LABEL),
            "提示词里必须出现取证用的那个标记串 {:?}，否则复核员认不出联网那条",
            crate::agent::WEB_SEARCH_PROBE_LABEL
        );
        assert!(
            REFLECT_SYSTEM.contains("不许因为没有 URL 就判 unsupported"),
            "光列出来不够：必须明说查询词对得上就算已取证"
        );
        // 联网取证走的是同一条 probes 通道，主循环把查询词当一条取证记进来即可
        let probes = vec![probe(
            "联网检索（服务端执行）",
            "- 2026年9月18日 上证指数 收盘点位",
        )];
        let inp = ReflectInput {
            task: "2026年9月18日上证指数收盘点位是多少",
            rubric: Rubric::Evidence,
            project_root: Path::new("."),
            changes: &[],
            read_paths: &[],
            probes: &probes,
            answer: Some("3911.87 点"),
            verifications: &[],
            gate_note: None,
        };
        let ev = build_user_prompt(&inp);
        assert!(
            ev.contains(crate::agent::WEB_SEARCH_PROBE_LABEL),
            "取证通道与提示词共用同一个标记串，这里对不上就是漂移：{ev}"
        );
        assert!(ev.contains("上证指数"), "{ev}");
    }

    #[test]
    fn evidence_carries_command_probes_to_the_reviewer() {
        struct Quiet;
        impl crate::pipeline::Sink for Quiet {}

        let llm = crate::testllm::fake_llm(vec![
            r#"{"verdict":"ok","summary":"输出可核对","findings":[]}"#.into(),
        ]);
        let mut cfg = AppConfig::default();
        cfg.llm.base_url = llm.base_url.clone();
        cfg.llm.api_key = "smoke".into();
        cfg.llm.model = "fake".into();
        cfg.reflect.max_steps = 1;

        let probes = vec![probe("date /t", "2026/09/22 周二")];
        let inp = ReflectInput {
            task: "今天星期几",
            rubric: Rubric::Evidence,
            project_root: Path::new("."),
            changes: &[],
            read_paths: &[],
            probes: &probes,
            answer: Some("今天是星期二（2026-09-22）"),
            verifications: &[],
            gate_note: None,
        };
        let out = block_on(run(&cfg, &inp, &crate::exec::new_cancel_flag(), &Quiet));

        // ① 复核员打开的那份输入里必须有这条命令的实际输出
        let req = llm.request(0);
        assert!(req.contains("$ date /t"), "复核员没看到命令：{req}");
        assert!(req.contains("2026/09/22 周二"), "复核员没看到输出：{req}");
        // ② 看得到就该放行，不再打回
        assert!(!out.reflection.suspect(), "{:?}", out.reflection);
        assert_eq!(llm.count(), 1, "一次复核只该问一次");
    }

    /// 反证：证据池里**没有**命令输出时，"输出无处可查"是复核员唯一诚实的结论
    /// （这条存在的意义是提醒：别把上面的通道再删掉）
    #[test]
    fn without_probes_the_answer_looks_unsupported() {
        struct Quiet;
        impl crate::pipeline::Sink for Quiet {}

        let llm = crate::testllm::fake_llm(vec![
            r#"{"verdict":"suspect","summary":"查不到","findings":[{"severity":"medium","claim":"2026/09/22 周二 在本次记录中无处可查","evidence":"unknown","verdict":"unsupported","suggest":"再取证"}]}"#
                .into(),
        ]);
        let mut cfg = AppConfig::default();
        cfg.llm.base_url = llm.base_url.clone();
        cfg.llm.api_key = "smoke".into();
        cfg.llm.model = "fake".into();
        cfg.reflect.max_steps = 1;

        let inp = ReflectInput {
            task: "今天星期几",
            rubric: Rubric::Evidence,
            project_root: Path::new("."),
            changes: &[],
            read_paths: &[],
            probes: &[],
            answer: Some("今天是星期二（2026-09-22）"),
            verifications: &[],
            gate_note: None,
        };
        let out = block_on(run(&cfg, &inp, &crate::exec::new_cancel_flag(), &Quiet));
        assert!(!llm.request(0).contains("$ date"), "没跑命令就不该有取证段");
        assert!(
            out.reflection.suspect(),
            "证据池里没有就该报疑，这才是诚实的"
        );
    }
}
