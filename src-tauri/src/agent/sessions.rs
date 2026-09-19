//! 会话（session）：与 agent 的一段对话（融合后续迭代 — 多会话对话模型）。
//!
//! 会话是 IDE 侧概念：一条会话 = 中央编辑区的一个聊天 tab，消息流里可选挂
//! `run_id`（该消息触发的引擎任务，事件流在会话内渲染，产物可从 runs 目录回看）。
//! 持久化在项目内：`<root>/.ruyix/code/agent/sessions/<id>.json`
//! （agent 只在项目内可用，会话与项目同生命周期）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    Path::new(project_root)
        .join(".ruyix")
        .join("code")
        .join("agent")
        .join("sessions")
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
    std::fs::write(&path, text).map_err(|e| format!("写入会话失败: {e}"))
}

pub fn delete(project_root: &str, id: &str) -> Result<(), String> {
    let path = session_path(project_root, id)?;
    if !path.exists() {
        return Err(format!("会话不存在: {id}"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("删除会话失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ruyix-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn roundtrip_and_list_order() {
        let root = dir();
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
                },
                SessionMsg {
                    role: "assistant".into(),
                    text: "3 步计划已生成".into(),
                    ts: "t2".into(),
                    run_id: Some("run-1".into()),
                    status: Some("planned".into()),
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
}
