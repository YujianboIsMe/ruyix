//! A2A（Agent-to-Agent）客户端 —— 与远端 A2A agent 协作。
//!
//! 定位：ruyix 作为 A2A **client**。能力面：
//! - 发现：`GET {base}/.well-known/agent-card.json`（0.3+），回退 `agent.json`（0.2.x）
//! - 委托：`message/send` 发送任务，轮询 `tasks/get` 至终态（completed/failed/
//!   canceled/rejected），文本回复从 artifacts / status.message / 直接 message 提取
//! - 流式 tasks/subscribe（SSE）留待后续 —— 第一版用轮询，简单可靠
//!
//! 传输是 JSON-RPC 2.0 over HTTP POST（无新增依赖，复用 reqwest）。
//! 配置：`~/.ruyix/code/a2a.toml`（全局）+ `<root>/.ruyix/code/a2a.toml`（项目，
//! 同名覆盖），与 mcp.toml 同一套结构化文件惯例。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const HTTP_TIMEOUT_SECS: u64 = 60;
/// 轮询节奏与上限：1.5s × 200 ≈ 5 分钟（覆盖典型远端 agent 的长任务）
const POLL_INTERVAL_MS: u64 = 1_500;
const POLL_MAX: usize = 200;

static MSG_SEQ: AtomicU64 = AtomicU64::new(1);

fn new_message_id() -> String {
    format!(
        "ruyix-{}-{}",
        std::process::id(),
        MSG_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

// ============================================
// 配置（a2a.toml）
// ============================================

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct A2aAgentCfg {
    pub name: String,
    pub url: String,
    /// agent card 摘要（discover 时缓存，面板直接展示）
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub skills: Vec<String>,
}

fn global_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".ruyix")
        .join("code")
        .join("a2a.toml")
}

fn project_path(project_root: &str) -> std::path::PathBuf {
    std::path::Path::new(project_root)
        .join(".ruyix")
        .join("code")
        .join("a2a.toml")
}

fn read_file_agents(path: &std::path::Path) -> Vec<A2aAgentCfg> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        agents: Vec<A2aAgentCfg>,
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str::<File>(&s).ok())
        .map(|f| f.agents)
        .unwrap_or_default()
}

fn write_file_agents(path: &std::path::Path, agents: &[A2aAgentCfg]) -> Result<(), String> {
    #[derive(Serialize)]
    struct File<'a> {
        agents: &'a [A2aAgentCfg],
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let text = toml::to_string_pretty(&File { agents })
        .map_err(|e| format!("序列化 a2a.toml 失败: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

pub fn load_agents(project_root: Option<&str>) -> Vec<A2aAgentCfg> {
    let mut merged = read_file_agents(&global_path());
    if let Some(root) = project_root {
        for a in read_file_agents(&project_path(root)) {
            merged.retain(|s| s.name != a.name);
            merged.push(a);
        }
    }
    merged
}

pub fn save_agent(cfg: &A2aAgentCfg) -> Result<(), String> {
    let path = global_path();
    let mut list = read_file_agents(&path);
    list.retain(|a| a.name != cfg.name);
    list.push(cfg.clone());
    write_file_agents(&path, &list)
}

pub fn remove_agent(name: &str, project_root: Option<&str>) -> Result<(), String> {
    let mut touched = 0;
    for path in [Some(global_path()), project_root.map(project_path)]
        .into_iter()
        .flatten()
    {
        let list = read_file_agents(&path);
        if list.iter().any(|a| a.name == name) {
            let kept: Vec<_> = list.into_iter().filter(|a| a.name != name).collect();
            write_file_agents(&path, &kept)?;
            touched += 1;
        }
    }
    if touched == 0 {
        Err(format!("A2A agent 不存在: {name}"))
    } else {
        Ok(())
    }
}

// ============================================
// 发现（agent card）
// ============================================

/// 拉取远端 agent card（先 0.3 的 agent-card.json，回退 0.2 的 agent.json）
pub async fn discover_card(base_url: &str) -> Result<A2aAgentCfg, String> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| e.to_string())?;
    let mut last_err = String::new();
    for well_known in ["agent-card.json", "agent.json"] {
        let url = format!("{base}/.well-known/{well_known}");
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let v: Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("agent card 解析失败（{url}）: {e}"))?;
                return Ok(card_to_cfg(base, &v));
            }
            Ok(resp) => {
                last_err = format!("{} → HTTP {}", well_known, resp.status());
            }
            Err(e) => last_err = format!("{well_known}: {e}"),
        }
    }
    Err(format!("发现失败（{base}）: {last_err}"))
}

/// AgentCard JSON（字段名取 A2A 规范的 camelCase）→ 配置条目
fn card_to_cfg(base: &str, v: &Value) -> A2aAgentCfg {
    let skills = v
        .get("skills")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("name").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    A2aAgentCfg {
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unnamed-agent")
            .to_string(),
        url: base.to_string(),
        description: v
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        version: v
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        skills,
    }
}

// ============================================
// 任务委托（message/send + tasks/get 轮询）
// ============================================

#[derive(Serialize, Clone, Debug)]
pub struct A2aTaskResult {
    pub agent: String,
    pub state: String,
    pub text: String,
}

/// 非终态：submitted / working / input-required（input-required 视作可继续等待）
fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "canceled" | "rejected" | "unknown"
    )
}

async fn rpc(
    client: &reqwest::Client,
    base_url: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let resp = client
        .post(format!("{}/", base_url.trim_end_matches('/')))
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .await
        .map_err(|e| format!("A2A 请求失败: {e}"))?;
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("A2A 响应解析失败: {e}"))?;
    match v.get("error") {
        Some(err) => Err(format!(
            "A2A 错误 {}: {}",
            err.get("code").and_then(Value::as_i64).unwrap_or(0),
            err.get("message").and_then(Value::as_str).unwrap_or("?")
        )),
        None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
    }
}

/// 从 message / task 里提取文本（parts[].kind == "text"）
fn extract_text(v: &Value) -> String {
    let parts = v
        .get("parts")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|p| p.get("kind").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    // artifacts[].parts[]（规范里 artifacts 的条目也可能带 parts）
    let artifacts = v
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let t = extract_text(a);
                    if t.is_empty() { None } else { Some(t) }
                })
                .collect::<Vec<_>>()
                .join("\n---\n")
        })
        .unwrap_or_default();
    let status_msg = v
        .get("status")
        .and_then(|s| s.get("message"))
        .map(extract_text)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    // 优先 artifacts，其次 status.message，最后 message 自身 parts
    if !artifacts.is_empty() {
        artifacts
    } else if !status_msg.is_empty() {
        status_msg
    } else {
        parts
    }
}

/// 发送任务并等待终态。每轮轮询回调一次进度（GUI 经事件转发）。
pub async fn send_task<F>(
    cfg: &A2aAgentCfg,
    text: &str,
    mut on_poll: F,
) -> Result<A2aTaskResult, String>
where
    F: FnMut(&str, usize, usize),
{
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| e.to_string())?;
    let message_id = new_message_id();
    let result = rpc(
        &client,
        &cfg.url,
        "message/send",
        json!({
            "message": {
                "role": "user",
                "parts": [{"kind": "text", "text": text}],
                "messageId": message_id,
            }
        }),
    )
    .await?;

    // 响应三形态：直接 message（同步回复）/ task / 空
    if result.get("task").is_none() && result.get("parts").is_some() {
        return Ok(A2aTaskResult {
            agent: cfg.name.clone(),
            state: "completed".into(),
            text: extract_text(&result),
        });
    }
    let mut task = result
        .get("task")
        .cloned()
        .ok_or_else(|| "A2A 响应既非 task 也非 message".to_string())?;

    let mut task_id = task
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut state = state_of(&task);
    let mut polls = 0usize;
    while !is_terminal(&state) && polls < POLL_MAX {
        polls += 1;
        on_poll(&state, polls, POLL_MAX);
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        task = rpc(&client, &cfg.url, "tasks/get", json!({"taskId": task_id})).await?;
        if let Some(new_id) = task.get("id").and_then(Value::as_str) {
            task_id = new_id.to_string();
        }
        state = state_of(&task);
    }
    Ok(A2aTaskResult {
        agent: cfg.name.clone(),
        state,
        text: extract_text(&task),
    })
}

fn state_of(task: &Value) -> String {
    task.get("status")
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_to_cfg_maps_fields() {
        let v = json!({
            "name": "weather",
            "description": "天气查询",
            "version": "2.1",
            "skills": [{"id": "s1", "name": "forecast"}, {"id": "s2", "name": "alert"}],
        });
        let cfg = card_to_cfg("https://a.example.com", &v);
        assert_eq!(cfg.name, "weather");
        assert_eq!(cfg.url, "https://a.example.com");
        assert_eq!(cfg.version, "2.1");
        assert_eq!(cfg.skills, vec!["forecast", "alert"]);
    }

    #[test]
    fn extract_text_prefers_artifacts_then_status() {
        let v = json!({
            "parts": [{"kind": "text", "text": "m"}],
            "artifacts": [{"parts": [{"kind": "text", "text": "产物"}]}],
            "status": {"state": "completed", "message": {"parts": [{"kind": "text", "text": "状态消息"}]}}
        });
        assert_eq!(extract_text(&v), "产物");
        let only_status = json!({
            "status": {"state": "failed", "message": {"parts": [{"kind": "text", "text": "坏了"}]}}
        });
        assert_eq!(extract_text(&only_status), "坏了");
        let only_parts = json!({"parts": [{"kind": "text", "text": "直答"}]});
        assert_eq!(extract_text(&only_parts), "直答");
    }

    #[test]
    fn terminal_states_match_spec() {
        for s in ["completed", "failed", "canceled", "rejected", "unknown"] {
            assert!(is_terminal(s), "{s} 应为终态");
        }
        for s in ["submitted", "working", "input-required"] {
            assert!(!is_terminal(s), "{s} 应为中间态");
        }
    }

    /// 本地 TcpListener 起一个最小 A2A 服务端（纯 std，无依赖），真走 HTTP 往返。
    #[test]
    fn send_task_against_local_mock_server() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap(); // POST 行
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if h.to_ascii_lowercase().starts_with("content-length")
                    && let Some(v) = h.split(':').nth(1)
                {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            std::io::Read::read_exact(&mut reader, &mut body).unwrap();
            let v: Value = serde_json::from_slice(&body).unwrap();
            let resp_body = match v["method"].as_str().unwrap() {
                "message/send" => json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {"task": {"id": "t-1", "status": {"state": "completed"},
                        "artifacts": [{"parts": [{"kind": "text", "text": "远端结果"}]}]}}
                }),
                _ => json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
            };
            let text = resp_body.to_string();
            write!(
                writer,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                text.len(),
                text
            )
            .unwrap();
        });

        let cfg = A2aAgentCfg {
            name: "mock".into(),
            url: format!("http://127.0.0.1:{port}"),
            description: String::new(),
            version: String::new(),
            skills: vec![],
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(send_task(&cfg, "你好", |_, _, _| {})).unwrap();
        assert_eq!(res.state, "completed");
        assert_eq!(res.text, "远端结果");
    }
}
