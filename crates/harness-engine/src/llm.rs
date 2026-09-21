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
    /// 本次补全的结束原因（"stop" / "length" / ...）。length = 内容被 max_tokens
    /// 截断（推理模型的思考 token 也计入预算，长输出最容易撞线）—— 调用方据此
    /// 区分"格式烂"和"没写完"，两种病的纠偏指令完全不同。
    pub finish_reason: Option<String>,
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
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct MsgBody {
    #[serde(default)]
    content: String,
}

/// 从模型回复里把一个 JSON 对象抠出来，**并修掉字符串里的裸控制字符**。
///
/// 两件事分开：抠（`extract_json_object_inner`）+ 修（`escape_raw_controls`）。
/// 为什么必须修：模型被要求在 `{"final":"…Markdown…"}` 里塞多行说明，而它经常直接写真换行、
/// 而不是转义写法（一个反斜杠加 n）—— 于是 serde_json 报
/// `control character found while parsing a string`，**整轮输出作废**
/// （实测 run `agent-20260921-0950`：第 13 轮的 final 就死在这儿，白烧一轮，模型只能重发一遍）。
/// 这不是把解析放松成猜：合法 JSON 的字符串里不可能出现裸控制字符，所以这个转换只可能把
/// 「不合法」修成「模型想说的」，动不了任何已经合法的输入。
pub fn extract_json_object(raw: &str) -> String {
    escape_raw_controls(&extract_json_object_inner(raw))
}

/// 把**字符串字面量内部**的裸控制字符转义掉（换行 / 回车 / 制表符，其余 < 0x20 走 `u00XX` 形式）。
/// 字符串外一个字符都不动 —— 那里的空白本来就是 JSON 的合法分隔符。
pub fn escape_raw_controls(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        if !in_str {
            if c == '"' {
                in_str = true;
            }
            out.push(c);
            continue;
        }
        if esc {
            esc = false;
            out.push(c);
            continue;
        }
        match c {
            '\\' => {
                esc = true;
                out.push(c);
            }
            '"' => {
                in_str = false;
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 从模型回复里把一个 JSON 对象抠出来（不做控制字符修复，见 [`extract_json_object`]）。
/// 处理三种常见脏输出：整体就是 JSON / 围栏 / 前后带解释文字。
fn extract_json_object_inner(raw: &str) -> String {
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
                    let finish_reason =
                        parsed.choices.first().and_then(|c| c.finish_reason.clone());
                    let content = parsed
                        .choices
                        .first()
                        .and_then(|c| c.message.as_ref())
                        .map(|m| m.content.clone())
                        .unwrap_or_default();
                    if content.trim().is_empty() {
                        // 空内容多是模型波动或被 max_tokens 截断：按可重试错误走重试循环
                        last_err = if finish_reason.as_deref() == Some("length") {
                            "模型返回了空内容（finish_reason=length，疑似被 max_tokens 截断，可在设置里调大）".to_string()
                        } else {
                            "模型返回了空内容".to_string()
                        };
                        observe_llm_fail(cfg, messages, attempt, &last_err, started_ms);
                    } else {
                        let out = ChatOutcome {
                            content,
                            usage: parsed.usage.unwrap_or_default(),
                            model: parsed.model.unwrap_or_else(|| cfg.model.clone()),
                            finish_reason,
                            elapsed_ms: started.elapsed().as_millis(),
                        };
                        // 记一次 LLM 调用：**这是"模型输出错了"唯一能复盘的地方**
                        observe_llm(cfg, messages, &out, attempt, "ok", None, started_ms);
                        return Ok(out);
                    }
                } else {
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

/// 哪些 `chat` 错误是"重试也不会好"的确定性失败（缺 Key / 鉴权 / 余额 / 参数被拒）。
/// 调用方据此区分"退避后再试"和"立刻放弃"，不必再猜错误字符串的含义。
pub fn is_fatal_error(e: &str) -> bool {
    [
        "尚未配置",
        "鉴权失败",
        "余额",
        "额度",
        "请求被拒绝",
        "构建 HTTP 客户端失败",
    ]
    .iter()
    .any(|m| e.contains(m))
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

    /// 模型把 Markdown 的多行说明直接塞进 JSON 字符串（写真换行，而不是转义写法）时，
    /// 抠出来的必须是**能解析**的 JSON，而不是把整轮输出作废 ——
    /// 实测 run agent-20260921-0950 第 13 轮就死在这儿。
    #[test]
    fn raw_newlines_inside_a_model_string_are_repaired() {
        let raw = "prefix {\"final\":\"## 结论\n\n服务已启动\n- 端口 8087\"} suffix";
        let json = extract_json_object(raw);
        let v: serde_json::Value = serde_json::from_str(&json).expect("修好之后应当可解析");
        let f = v["final"].as_str().expect("final 应当是字符串");
        assert!(f.contains("服务已启动"), "{f:?}");
        assert_eq!(f.matches('\n').count(), 3, "换行要原样保留：{f:?}");
    }

    /// 修复器只动字符串内部的裸控制字符：合法的 JSON 一个字都不改，字符串外的空白也不许动。
    #[test]
    fn control_repair_leaves_valid_json_alone() {
        // 合法 JSON：字符串外是真空白，字符串里是「反斜杠+n」这种**转义写法**
        let good = "{\n  \"a\": \"b\\n\\n\"\n}";
        assert_eq!(
            escape_raw_controls(good),
            good,
            "合法 JSON 一个字都不该被改"
        );
        let spaced = "{  \"a\": 1 }";
        assert_eq!(
            escape_raw_controls(spaced),
            spaced,
            "字符串外的空白是分隔符，不许动"
        );
    }
}
