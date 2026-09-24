//! 会话（session）：与 agent 的一段对话（融合后续迭代 — 多会话对话模型）。
//!
//! 会话是 IDE 侧概念：一条会话 = 中央编辑区的一个聊天 tab，消息流里可选挂
//! `run_id`（该消息触发的引擎任务，事件流在会话内渲染，产物可从 runs 目录回看）。
//! 持久化在**便携根**里的项目桶：`<根>/projects/<项目 key>/sessions/<id>.json`
//! （v1.0.0 起不写用户仓库 —— 会话是 IDE 的状态，不是项目的源码）。
//! （agent 只在项目内可用，会话与项目同生命周期）。

use harness_engine::agent::VerifyOutcome;
use harness_engine::plan::PlanStep;
use harness_engine::reflect::Reflection;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 一轮任务计划的持久化快照。
///
/// 步骤**定义**直接复用引擎的 [`PlanStep`]（id / title / detail / files / kind），
/// 不在这里另抄一份字段；`states` 只补 UI 侧那份引擎不管的东西 —— 每步的终态。
/// 分工与 `RunRecord.plan` + `RunRecord.generation.steps` 一致。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PlanSnap {
    #[serde(default)]
    pub steps: Vec<PlanStep>,
    #[serde(default)]
    pub states: Vec<PlanStepState>,
}

/// 计划步骤的终态（UI 侧事实）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlanStepState {
    /// 对应 [`PlanStep::id`]
    pub id: u32,
    /// pending | running | done | error | skipped
    #[serde(default)]
    pub st: String,
    /// 终态说明（skipped 缺什么 / error 为什么），进 tooltip
    #[serde(default)]
    pub note: String,
}

/// 一次 `ask_user` 提问的存档快照（v0.8）：问题 / 为什么问 / 选项 / 答案 / 状态。
///
/// 跟 `plan` / `verify` / `reflect` 同样的理由：会话跑的是工具循环、**不落 RunRecord**，
/// 不跟着消息存下来，重开会话就再也看不到"这一轮问过什么、用户怎么回答的"。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AskSnap {
    pub id: String,
    pub question: String,
    /// 这个答案会决定接下来的什么动作（UI 展示给用户的理由）
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub options: Vec<String>,
    /// 答到了才是 Some；超时 / 取消 / 无通道 / 开关关闭都为空
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// answered | timeout | no_asker | canceled | failed
    pub state: String,
    #[serde(default)]
    pub ts: String,
}

/// agent 工具循环的**执行轨迹**一行（v0.0.5 UI）：一次思考 / 一次调用 / 一条告警。
///
/// 为什么跟着消息存：工具循环不落 RunRecord（`run_id` 恒空，理由见 [`SessionMsg::plan`]），
/// 轨迹只活在内存里的话，重开会话又是"一条光秃秃的答复 + 不知道它干了什么"。
/// `kind` 只影响图标与配色（think | do | fail | done），`text` 是一行原文 ——
/// 行宽由前端用省略号收尾，所以这里**不做截断**：数据要留全文，显示才按窗口裁。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TraceSnap {
    /// think（引擎/模型的判定）| do（一次调用）| fail（告警/失败）| done（收尾）
    pub kind: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionMsg {
    /// "user" | "assistant" | "system"
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub ts: String,
    /// 该消息触发的引擎 run（assistant 消息回填 run_id 便于回看产物）
    #[serde(default)]
    pub run_id: Option<String>,
    /// run 终态：planned/generated/verified/failed/canceled（非 run 消息为空）
    #[serde(default)]
    pub status: Option<String>,
    /// 该轮的任务计划快照（步骤定义 + 各步终态）。
    ///
    /// 为什么计划要跟着消息走而不是靠 `run_id` 回读：会话跑的是工具循环
    /// （`agent_reply`），它**不落 RunRecord**，`run_id` 天然为空 ——
    /// 于是重启 app 打开旧会话时 `agent_run_load` 无迹可寻，大纲区的任务列表
    /// 整个消失（不是沙漏，是压根没有）。老会话没有这个字段，照旧解析。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanSnap>,
    /// 本轮的机械验证结论（v0.3：窄层 + 全量层）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verify: Vec<VerifyOutcome>,
    /// 本轮的复核结论（干净上下文反思）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reflect: Vec<Reflection>,
    /// 本轮的提问（v0.8：需求歧义问了什么、用户怎么答的）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ask: Vec<AskSnap>,
    /// 本轮的执行轨迹（v0.0.5 UI：跑的时候实时刷、跑完跟着消息存档）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<TraceSnap>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    pub messages: Vec<SessionMsg>,
}

fn sessions_dir(project_root: &str) -> PathBuf {
    crate::paths::current().project_bucket(project_root, "sessions")
}

fn session_path(project_root: &str, id: &str) -> Result<PathBuf, String> {
    // id 只允许 [A-Za-z0-9_-]，防路径逃逸
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("非法会话 id: {id}"));
    }
    Ok(sessions_dir(project_root).join(format!("{id}.json")))
}

pub fn new_session_id() -> String {
    // rfc3339（含 T : . + 时区偏移）→ 只留字母数字与 '-'，保证过上面的 id 白名单
    let safe: String = harness_engine::workspace::now_iso()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    format!("sess_{safe}")
}

/// 列出项目的会话（按更新时间新→旧）
pub fn list(project_root: &str) -> Vec<Session> {
    let dir = sessions_dir(project_root);
    let mut out: Vec<Session> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| {
                    let id = e.path().file_stem()?.to_string_lossy().to_string();
                    load(project_root, &id).ok()
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

pub fn load(project_root: &str, id: &str) -> Result<Session, String> {
    let path = session_path(project_root, id)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取会话失败 {}: {e}", path.display()))?;
    let mut s: Session = serde_json::from_str(&text)
        .map_err(|e| format!("会话 JSON 解析失败（{}）: {e}", path.display()))?;
    // 文件名即 id 的唯一事实来源（防止手改错位）
    s.id = id.to_string();
    Ok(s)
}

pub fn save(session: &Session, project_root: &str) -> Result<(), String> {
    let path = session_path(project_root, &session.id)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建会话目录失败: {e}"))?;
    }
    let text = serde_json::to_string_pretty(session).map_err(|e| format!("序列化会话失败: {e}"))?;
    // **原子写**：先写同目录临时文件、再 rename 覆盖。
    //
    // 会话是用户的对话历史，而 `fs::write` 是截断式写入 —— 在写一半时被打断（关窗口 / 断电 /
    // 进程被杀）会留下半截 JSON，而 `list()` 会跳过解析失败的条目：**那一段对话就整段消失了**，
    // 且用户没有任何提示。rename 在同一卷上是原子的：读到的要么是旧全文、要么是新全文。
    // （临时文件刻意用 `.tmp` 后缀：`list()` 只认 `*.json`，所以残留的临时文件不会被当成会话。）
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)
        .map_err(|e| format!("写入会话临时文件失败 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换会话文件失败 {}: {e}", path.display())
    })
}

/// 删除会话。**幂等**：文件已经不在（从未落盘的空会话被删、或上个进程被强杀）也算成功 ——
/// 用户点 ✕ 的意图是"这条没了"，此时报错只会让他以为删失败了。
pub fn delete(project_root: &str, id: &str) -> Result<(), String> {
    let path = session_path(project_root, id)?;
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(&path).map_err(|e| format!("删除会话失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个用例一个独立临时目录：同进程内的用例是并行跑的，共用目录会让
    /// 彼此的 `remove_dir_all` 互删（曾让 `roundtrip_and_list_order` 看到 1 条消息）。
    fn dir(tag: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("ruyix-session-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// 造一个会话（消息条数可控）
    fn sess(id: &str, title: &str, n: usize) -> Session {
        Session {
            id: id.into(),
            title: title.into(),
            created_at: "2026-09-19T08:00:00".into(),
            updated_at: "2026-09-19T08:00:01".into(),
            messages: (0..n)
                .map(|i| SessionMsg {
                    role: if i % 2 == 0 {
                        "user".into()
                    } else {
                        "assistant".into()
                    },
                    text: format!("第 {i} 条"),
                    ts: "t".into(),
                    run_id: None,
                    status: None,
                    plan: None,
                    verify: vec![],
                    reflect: vec![],
                    ask: vec![],
                    trace: vec![],
                })
                .collect(),
        }
    }

    /// **原子写**：写完之后不留临时文件；覆盖是"换整份"而不是"截断再写"。
    ///
    /// 为什么这条算不变量：`fs::write` 截断写入一旦被中断（关窗口 / 断电 / 进程被杀），
    /// 留下半截 JSON；而 `list()` 会跳过解析失败的条目 —— 那一段对话就整段消失了。
    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let root = dir("atomic");
        let root = root.to_str().unwrap().to_string();
        let mut s = sess("sess_atomic", "第一版标题", 2);
        save(&s, &root).unwrap();
        s.title = "改过的标题".into();
        s.messages.push(SessionMsg {
            role: "user".into(),
            text: "追加一条".into(),
            ts: "t2".into(),
            run_id: None,
            status: None,
            plan: None,
            verify: vec![],
            reflect: vec![],
            ask: vec![],
            trace: vec![],
        });
        save(&s, &root).unwrap();

        let back = load(&root, "sess_atomic").unwrap();
        assert_eq!(back.title, "改过的标题");
        assert_eq!(back.messages.len(), 3, "覆盖要落成新全文");

        let files: Vec<String> = std::fs::read_dir(sessions_dir(&root))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            files,
            vec!["sess_atomic.json".to_string()],
            "目录里不该有临时文件残留: {files:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **删除幂等**：文件不在也算成功 —— 空会话从未落盘、或上一个进程被强杀过。
    /// 用户点 ✕ 的意图是"这条没了"，此时报错只会让他以为删失败。
    #[test]
    fn delete_is_idempotent() {
        let root = dir("del");
        let root = root.to_str().unwrap().to_string();
        assert!(
            delete(&root, "sess_missing").is_ok(),
            "删不存在的会话不该报错"
        );
        save(&sess("sess_del", "要删的", 1), &root).unwrap();
        delete(&root, "sess_del").unwrap();
        assert!(load(&root, "sess_del").is_err(), "删完就该读不到");
        assert!(delete(&root, "sess_del").is_ok(), "再删一次也不报错");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **坏文件只影响它自己**：一条解析失败的会话不许把整个列表打空。
    ///
    /// 这是"历史全丢"的另一条可能路径：如果 `list()` 用 `collect::<Result<Vec<_>,_>>()`，
    /// 一条坏文件就能让用户看到"一条会话都没有"（而盘上其实有几十条）。临时文件同理 ——
    /// `list()` 只认 `*.json`，`.json.tmp` 不能被当成会话。
    #[test]
    fn a_corrupt_file_only_drops_itself_from_the_list() {
        let root = dir("corrupt");
        let root = root.to_str().unwrap().to_string();
        save(&sess("sess_good", "好的那条", 2), &root).unwrap();
        std::fs::write(sessions_dir(&root).join("sess_broken.json"), "{ 半截 JSON").unwrap();

        let listed = list(&root);
        assert_eq!(
            listed.len(),
            1,
            "好的那条必须还在：{:?}",
            listed.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
        assert_eq!(listed[0].id, "sess_good");
        assert_eq!(listed[0].messages.len(), 2);

        std::fs::write(sessions_dir(&root).join("sess_good.json.tmp"), "{}").unwrap();
        assert_eq!(list(&root).len(), 1, "临时文件不能被当成会话");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn roundtrip_and_list_order() {
        let root = dir("roundtrip");
        let root = root.to_str().unwrap().to_string();
        let s1 = Session {
            id: "sess_a".into(),
            title: "第一段".into(),
            created_at: "2026-09-19T08:00:00".into(),
            updated_at: "2026-09-19T08:00:01".into(),
            messages: vec![
                SessionMsg {
                    role: "user".into(),
                    text: "实现 word_count".into(),
                    ts: "t1".into(),
                    run_id: None,
                    status: None,
                    plan: None,
                    verify: vec![],
                    reflect: vec![],
                    ask: vec![],
                    trace: vec![],
                },
                SessionMsg {
                    role: "assistant".into(),
                    text: "3 步计划已生成".into(),
                    ts: "t2".into(),
                    run_id: Some("run-1".into()),
                    status: Some("planned".into()),
                    plan: None,
                    verify: vec![],
                    reflect: vec![],
                    ask: vec![],
                    trace: vec![],
                },
            ],
        };
        let s2 = Session {
            id: "sess_b".into(),
            title: "第二段".into(),
            created_at: "2026-09-19T09:00:00".into(),
            updated_at: "2026-09-19T09:30:00".into(),
            messages: vec![],
        };
        save(&s1, &root).unwrap();
        save(&s2, &root).unwrap();

        let back = load(&root, "sess_a").unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].run_id.as_deref(), Some("run-1"));

        let listed = list(&root);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "sess_b", "新→旧排序");

        delete(&root, "sess_a").unwrap();
        assert!(load(&root, "sess_a").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_id_rejects_path_escape() {
        assert!(session_path("x", "../evil").is_err());
        assert!(session_path("x", "ok_id-1").is_ok());
    }

    /// 防回归：生成的 id 必须能通过自身白名单（rfc3339 的 + 时区偏移曾导致"非法会话 id"）
    #[test]
    fn generated_id_passes_own_whitelist() {
        for _ in 0..3 {
            let id = new_session_id();
            assert!(session_path("x", &id).is_ok(), "id 未过白名单: {id}");
            assert!(id.starts_with("sess_2"), "id 缺少可排序时间前缀: {id}");
        }
    }

    /// 计划快照必须能往返，**尤其终态**：重启后大纲区若把 done/skipped 读回成 pending，
    /// 就等于把"run 已经结束"重新显示成"⌛ 等待执行" —— 与沙漏 bug 同一种骗人。
    #[test]
    fn plan_snapshot_roundtrip_keeps_terminal_states() {
        let root = dir("plan");
        let root = root.to_str().unwrap().to_string();
        let snap = PlanSnap {
            steps: vec![
                PlanStep {
                    id: 1,
                    title: "实现 word_count".into(),
                    detail: "核心函数".into(),
                    files: vec!["wc.py".into()],
                    kind: "code".into(),
                },
                PlanStep {
                    id: 2,
                    title: "写单测".into(),
                    detail: String::new(),
                    files: vec!["test_wc.py".into()],
                    kind: "test".into(),
                },
            ],
            states: vec![
                PlanStepState {
                    id: 1,
                    st: "done".into(),
                    note: String::new(),
                },
                PlanStepState {
                    id: 2,
                    st: "skipped".into(),
                    note: "缺 test_wc.py".into(),
                },
            ],
        };
        let s = Session {
            id: "sess_plan".into(),
            title: "带计划".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            messages: vec![SessionMsg {
                role: "assistant".into(),
                text: "做完了".into(),
                ts: "t3".into(),
                run_id: None,
                status: None,
                plan: Some(snap),
                verify: vec![VerifyOutcome {
                    layer: "full".into(),
                    status: "failed".into(),
                    verdict: "1 项没通过".into(),
                    failed: 1,
                    ..Default::default()
                }],
                reflect: vec![Reflection {
                    verdict: "suspect".into(),
                    summary: "断言与实现不符".into(),
                    ..Default::default()
                }],
                ask: vec![AskSnap {
                    id: "ask-1".into(),
                    question: "登哪台机器？".into(),
                    why: "凭据放哪一侧".into(),
                    options: vec!["托管的服务器".into(), "用户另一台电脑".into()],
                    answer: Some("用户另一台电脑".into()),
                    state: "answered".into(),
                    ts: "t4".into(),
                }],
                trace: vec![
                    TraceSnap {
                        kind: "think".into(),
                        text: "工具循环（最多 40 轮，写入策略：Confirm）".into(),
                    },
                    TraceSnap {
                        kind: "do".into(),
                        text: "第 1 轮 read ✓ ui/session.js（905-1145）".into(),
                    },
                    TraceSnap {
                        kind: "fail".into(),
                        text: "第 2 轮 write ✗ 锚点在文件里出现 2 次".into(),
                    },
                ],
            }],
        };
        save(&s, &root).unwrap();

        let back = load(&root, "sess_plan").unwrap();
        let m = &back.messages[0];
        let p = m.plan.as_ref().expect("计划快照丢了");
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[1].kind, "test");
        assert_eq!(p.steps[1].files, vec!["test_wc.py".to_string()]);
        assert_eq!(p.states.len(), 2);
        assert_eq!(p.states[0].st, "done");
        assert_eq!(p.states[1].st, "skipped", "终态被读回成别的状态");
        assert_eq!(p.states[1].note, "缺 test_wc.py");
        // 同一类缺陷的另两个字段：UI 把结论挂在消息上，Rust 结构体不声明就会被
        // agent_session_save 的往返顺手抹掉（UI 拿返回值覆盖自己的 messages）
        assert_eq!(m.verify.len(), 1, "验证结论丢了");
        assert_eq!(m.verify[0].layer, "full");
        assert_eq!(m.verify[0].verdict, "1 项没通过");
        assert_eq!(m.reflect.len(), 1, "复核结论丢了");
        assert_eq!(m.reflect[0].verdict, "suspect");
        // v0.8：提问留痕也必须活过往返（同一个坑：结构体不声明就被 serde 抹掉）
        assert_eq!(m.ask.len(), 1, "提问留痕丢了");
        assert_eq!(m.ask[0].question, "登哪台机器？");
        assert_eq!(m.ask[0].state, "answered");
        assert_eq!(m.ask[0].answer.as_deref(), Some("用户另一台电脑"));
        assert_eq!(m.ask[0].options.len(), 2, "选项要留着（UI 要能复现问题卡）");
        // v0.0.5：执行轨迹同一条纪律 —— 结构体不声明就被 serde 抹掉，
        // 重开会话看到的就只剩一条答复，用户想看"它当时在干什么"再也回不来
        assert_eq!(m.trace.len(), 3, "执行轨迹丢了");
        assert_eq!(m.trace[0].kind, "think");
        assert_eq!(m.trace[1].kind, "do");
        assert_eq!(m.trace[1].text, "第 1 轮 read ✓ ui/session.js（905-1145）");
        assert_eq!(m.trace[2].kind, "fail");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 老会话（这些字段出现之前落的盘）必须照旧能读 —— 少一个字段不该让整段对话打不开。
    #[test]
    fn legacy_message_without_plan_still_loads() {
        let msg: SessionMsg =
            serde_json::from_str(r#"{"role":"assistant","text":"旧消息","ts":"t"}"#)
                .expect("老消息解析失败");
        assert!(msg.plan.is_none());
        assert!(msg.run_id.is_none());
        assert!(msg.verify.is_empty());
        assert!(msg.reflect.is_empty());
        assert!(msg.trace.is_empty(), "老消息的轨迹该是空的，不是解析失败");
        // 老会话序列化回去不许凭空长出字段（否则每个旧文件都被改写一遍）
        let back = serde_json::to_string(&msg).unwrap();
        assert!(!back.contains("plan"), "空计划不该写进 JSON: {back}");
        assert!(!back.contains("verify"), "空验证结论不该写进 JSON: {back}");
    }
}
