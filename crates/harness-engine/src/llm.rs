//! DeepSeek 客户端。
//!
//! 设计要点：
//! 1. `json_object` 响应模式 + 容错抽取 —— 模型偶尔会带上 ```json 围栏或前后废话，
//!    这里统一剥掉，绝不让"格式问题"变成"功能失败"。
//! 2. 重试只针对"可重试"的错误（429 / 5xx / 网络抖动）；401/402 直接报原文，
//!    因为重试再多次也没用，用户需要看到的是"Key 无效/余额不足"。
//! 3. 每次调用都回传 usage，UI 上要能看到 token 消耗 —— 不然用户不知道钱花哪了。

use crate::config::LlmConfig;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Serialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(c: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: c.into(),
        }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: c.into(),
        }
    }
    pub fn assistant(c: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: c.into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Clone, Debug)]
pub struct ChatOutcome {
    pub content: String,
    pub usage: Usage,
    pub model: String,
    pub elapsed_ms: u128,
}

#[derive(Deserialize)]
struct ApiResp {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<MsgBody>,
}

#[derive(Deserialize)]
struct MsgBody {
    #[serde(default)]
    content: String,
}

/// 从模型回复里把一个 JSON 对象抠出来。
/// 处理三种常见脏输出：整体就是 JSON / ```json 围栏 / 前后带解释文字。
pub fn extract_json_object(raw: &str) -> String {
    let mut s = raw.trim();

    // 去掉 markdown 代码围栏
    if s.starts_with("```") {
        if let Some(nl) = s.find('\n') {
            s = &s[nl + 1..];
        }
        if let Some(end) = s.rfind("```") {
            s = &s[..end];
        }
        s = s.trim();
    }

    // 已经是 JSON 就直接用
    if s.starts_with('{')
        && s.ends_with('}')
        && serde_json::from_str::<serde_json::Value>(s).is_ok()
    {
        return s.to_string();
    }

    // 扫描最外层花括号（跳过字符串内的括号）
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        let c = b as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0
                    && let Some(st) = start
                {
                    return s[st..=i].to_string();
                }
            }
            _ => {}
        }
    }
    s.to_string()
}

/// 带重试的一次对话调用。
pub async fn chat(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    json_mode: bool,
) -> Result<ChatOutcome, String> {
    if cfg.api_key.trim().is_empty() {
        return Err(
            "尚未配置 DeepSeek API Key（设置面板里填，或设环境变量 DEEPSEEK_API_KEY）".into(),
        );
    }

    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "temperature": cfg.temperature,
        "max_tokens": cfg.max_tokens,
        "stream": false,
    });
    if json_mode {
        body["response_format"] = serde_json::json!({ "type": "json_object" });
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.timeout_secs))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;

    let started = Instant::now();
    let started_ms = crate::observe::current().map(|t| t.now()).unwrap_or(0);
    let mut last_err = String::new();

    for attempt in 1..=3u32 {
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", cfg.api_key.trim()))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                if status.is_success() {
                    let parsed: ApiResp = serde_json::from_str(&text).map_err(|e| {
                        format!(
                            "解析接口响应失败: {e}；原文片段: {}",
                            crate::exec::clip(&text, 500)
                        )
                    })?;
                    let content = parsed
                        .choices
                        .first()
                        .and_then(|c| c.message.as_ref())
                        .map(|m| m.content.clone())
                        .unwrap_or_default();
                    if content.trim().is_empty() {
                        last_err = "模型返回了空内容".to_string();
                        // 空内容不值得重试
                        return Err(last_err);
                    }
                    let out = ChatOutcome {
                        content,
                        usage: parsed.usage.unwrap_or_default(),
                        model: parsed.model.unwrap_or_else(|| cfg.model.clone()),
                        elapsed_ms: started.elapsed().as_millis(),
                    };
                    // 记一次 LLM 调用：**这是"模型输出错了"唯一能复盘的地方**
                    observe_llm(cfg, messages, &out, attempt, "ok", None, started_ms);
                    return Ok(out);
                }

                let detail =
                    api_error_message(&text).unwrap_or_else(|| crate::exec::clip(&text, 400));
                // 鉴权/余额类错误：重试无意义，直接抛
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    let msg = format!("DeepSeek 鉴权失败(HTTP {status}): {detail}");
                    observe_llm_fail(cfg, messages, attempt, &msg, started_ms);
                    return Err(msg);
                }
                if status.as_u16() == 402 {
                    return Err(format!("DeepSeek 账户余额/额度问题(HTTP 402): {detail}"));
                }
                if status.as_u16() == 400 {
                    return Err(format!("DeepSeek 请求被拒绝(HTTP 400): {detail}"));
                }
                last_err = format!("HTTP {status}: {detail}");
            }
            Err(e) => {
                last_err = format!("网络错误: {e}");
            }
        }

        if attempt < 3 {
            // 用 tokio 的异步 sleep：在 tokio worker 上 std::thread::sleep 会霸占线程
            tokio::time::sleep(Duration::from_millis(800 * attempt as u64)).await;
        }
    }

    Err(format!("DeepSeek 调用失败（已重试 3 次）: {last_err}"))
}

fn api_error_message(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("error")?
        .get("message")?
        .as_str()
        .map(|s| s.to_string())
}

/// 记一次成功的 LLM 调用。
///
/// prompt / response 只记**头部**（脱敏 + 截断后）：完整落盘会把任务原文和代码
/// 大量写进日志，而调试时通常只需要"模型看到了什么开头、回了什么开头"。
fn observe_llm(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    out: &ChatOutcome,
    attempt: u32,
    status: &str,
    extra: Option<&str>,
    started_ms: u128,
) {
    let prompt = messages
        .iter()
        .map(|m| format!("[{}] {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    let mut a = crate::observe::attrs(&[
        ("model", out.model.as_str()),
        ("status", status),
        ("attempt", &attempt.to_string()),
        ("elapsed_ms", &out.elapsed_ms.to_string()),
        (
            "tokens",
            &format!(
                "{}+{}",
                out.usage.prompt_tokens, out.usage.completion_tokens
            ),
        ),
        ("prompt_chars", &prompt.chars().count().to_string()),
        ("prompt_head", &prompt),
        ("response_head", &out.content),
    ]);
    if let Some(e) = extra {
        a.insert("note".into(), e.to_string());
    }
    crate::observe::span_once(
        "llm",
        &format!("{}({})", cfg.model, messages.len()),
        status,
        started_ms,
        a,
    );
}

fn observe_llm_fail(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    attempt: u32,
    err: &str,
    started_ms: u128,
) {
    let mut a = crate::observe::attrs(&[
        ("model", cfg.model.as_str()),
        ("attempt", &attempt.to_string()),
        ("error", err),
    ]);
    a.insert("messages".into(), messages.len().to_string());
    crate::observe::span_once(
        "llm",
        &format!("{}(失败)", cfg.model),
        "error",
        started_ms,
        a,
    );
}

/// 连通性 + 鉴权自检：拿模型列表，比"跑一个真实任务才发现 key 错"友好得多。
pub async fn probe(cfg: &LlmConfig) -> Result<Vec<String>, String> {
    if cfg.api_key.trim().is_empty() {
        return Err("尚未配置 DeepSeek API Key".into());
    }
    let url = format!("{}/models", cfg.base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key.trim()))
        .send()
        .await
        .map_err(|e| format!("网络错误: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail = api_error_message(&text).unwrap_or_else(|| crate::exec::clip(&text, 300));
        return Err(format!("HTTP {status}: {detail}"));
    }
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("解析失败: {e}"))?;
    Ok(v.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_plain_json() {
        assert_eq!(extract_json_object("{\"a\":1}"), "{\"a\":1}");
    }

    #[test]
    fn extract_from_fenced_block() {
        let raw = "好的，这是计划：\n```json\n{\"a\": [1,2]}\n```\n希望有帮助";
        assert_eq!(extract_json_object(raw), "{\"a\": [1,2]}");
    }

    #[test]
    fn extract_ignores_braces_inside_strings() {
        let raw = "前缀 {\"note\": \"含 } 和 { 的文本\", \"n\": 1} 后缀";
        let got = extract_json_object(raw);
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        assert_eq!(v["n"], 1);
        assert!(v["note"].as_str().unwrap().contains("含"));
    }

    #[test]
    fn usage_accumulates() {
        let mut a = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        };
        a.add(&Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        });
        assert_eq!(a.total_tokens, 18);
        assert_eq!(a.completion_tokens, 7);
    }
}
