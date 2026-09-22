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
    /// 服务端联网检索**发起过**的查询词（没开联网或模型没检索时为空）。
    ///
    /// 这是引擎能从"黑盒注入"里捞到的唯一痕迹：服务端把检索结果直接灌进上下文，
    /// 标题与链接都不回传。但**有查询词就够了** —— 复核员据此知道模型真去查过，
    /// 不会再把"证据池里没有"当成"主循环没做"（那是上一个死锁的成因）。
    pub web_queries: Vec<String>,
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

// ---- Responses 协议（`/responses`）：服务端联网搜索只在它上面成立 ----

#[derive(Deserialize, Default)]
struct RespApi {
    #[serde(default)]
    output: Vec<RespItem>,
    #[serde(default)]
    usage: Option<RespUsage>,
    #[serde(default)]
    model: Option<String>,
    /// `completed` / `incomplete` / `failed`
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize, Default)]
struct RespItem {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    content: Vec<RespContent>,
    /// 仅 `web_search_call` 有：本次检索的查询词
    #[serde(default)]
    action: Option<RespAction>,
}

#[derive(Deserialize, Default)]
struct RespContent {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize, Default)]
struct RespAction {
    #[serde(default)]
    queries: Vec<String>,
}

#[derive(Deserialize)]
struct RespUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
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

/// 一次对话的解析结果 —— 两种协议（`/chat/completions` 与 `/responses`）的形状在这里抹平，
/// 重试循环因此只有一份。
struct RawReply {
    content: String,
    usage: Usage,
    model: Option<String>,
    finish_reason: Option<String>,
    web_queries: Vec<String>,
}

/// 端点是不是 DeepSeek 官方（`api.deepseek.com` 及其子域）。
///
/// 联网搜索是**服务端能力**，只有 DeepSeek 自家端点认。往 OpenAI / 其它兼容端点塞
/// `tools:[{"type":"web_search"}]` 会被打回 —— 实测 422
/// `unknown variant \`web_search\`, expected \`function\``。
pub fn is_deepseek_endpoint(cfg: &LlmConfig) -> bool {
    let after_scheme = cfg
        .base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(cfg.base_url.as_str());
    let host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    host == "api.deepseek.com" || host.ends_with(".deepseek.com")
}

/// 一个模型具备哪几种"服务端能力"。
///
/// 为什么要有这张表：能力是**逐模型**的，而厂商 `/models` 只给 id、不声明能力。
/// 实测联网检索就是这样 —— `deepseek-v4-pro` 在 `/responses` 上每次都真检索，
/// 而 `deepseek-v4-flash` 一次都不检索（模型自己明说"我没有可用的联网检索工具"）。
/// 不查能力就换协议 = 白白走一条新路径却什么也没多拿到。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelCaps {
    /// 服务端联网检索（`/responses` + `tools:[{type:web_search}]`）
    pub web_search: bool,
    /// 能读图（视觉输入）
    pub multimodal: bool,
}

/// 能力矩阵。**暂时硬编码**（厂商没有能力接口），随模型换代手工维护。
///
/// 表里没有的模型一律按"都没这能力"处理 —— 宁可不开，也不要为一条不存在的
/// 能力换协议。用户仍可用 `llm.web_search = "on"` 强行开。
const MODEL_CAPS: &[(&str, ModelCaps)] = &[
    (
        "deepseek-v4-pro",
        ModelCaps {
            web_search: true,
            multimodal: false,
        },
    ),
    // 注意 `deepseek-flash` 是官方推荐名，`deepseek-v4-flash` 是仍被接受的旧名
    // —— 两个名字指向同一个模型，能力要一致，否则改名就等于悄悄丢能力。
    (
        "deepseek-flash",
        ModelCaps {
            web_search: false,
            multimodal: true,
        },
    ),
    (
        "deepseek-v4-flash",
        ModelCaps {
            web_search: false,
            multimodal: true,
        },
    ),
    (
        "deepseek-v4-flash-vision-exp",
        ModelCaps {
            web_search: false,
            multimodal: true,
        },
    ),
];

/// 查模型能力（大小写不敏感）；表里没有的返回"都没有"。
pub fn model_caps(model: &str) -> ModelCaps {
    let m = model.trim().to_ascii_lowercase();
    MODEL_CAPS
        .iter()
        .find(|(k, _)| *k == m)
        .map(|(_, c)| *c)
        .unwrap_or(ModelCaps {
            web_search: false,
            multimodal: false,
        })
}

/// 本端点上**全部已知模型**及其能力（给宿主做下拉框与能力提示）。
pub fn known_models() -> &'static [(&'static str, ModelCaps)] {
    MODEL_CAPS
}

/// 本次调用要不要挂服务端联网搜索（配置键 `llm.web_search`）。
///
/// - `off`：永不开（联网出问题时**这就是回滚开关**，一行配置即退回老链路）；
/// - `auto`（默认）：DeepSeek 官方端点 **且** 该模型真有联网能力 —— 两个条件缺一不可，
///   否则就是换了协议却搜不了（flash 实测如此）；
/// - `on`：强行开（自建兼容端点自己认这个参数时用，**不看能力表**）。
pub fn web_search_on(cfg: &LlmConfig) -> bool {
    match cfg.web_search.trim().to_ascii_lowercase().as_str() {
        "on" => true,
        "auto" => is_deepseek_endpoint(cfg) && model_caps(&cfg.model).web_search,
        _ => false,
    }
}

fn chat_parts(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    json_mode: bool,
) -> (String, serde_json::Value) {
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
    (url, body)
}

/// `/responses` 的请求体。**只有这条路上服务端联网搜索成立**。
fn responses_parts(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    json_mode: bool,
) -> (String, serde_json::Value) {
    let url = format!("{}/responses", cfg.base_url.trim_end_matches('/'));
    let input: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| serde_json::json!({ "type": "message", "role": m.role, "content": m.content }))
        .collect();
    let mut body = serde_json::json!({
        "model": cfg.model,
        "input": input,
        "temperature": cfg.temperature,
        "max_output_tokens": cfg.max_tokens,
        "stream": false,
        "tools": [{ "type": "web_search" }],
    });
    if json_mode {
        body["text"] = serde_json::json!({ "format": { "type": "json_object" } });
    }
    (url, body)
}

/// 一种协议的全部差异：端点、请求体、以及把响应解析成 [`RawReply`] 的函数。
/// 抽成别名不只是为了可读性 —— 内联这个元组会被 clippy 判 `very_complex_type`。
type Protocol = (
    String,
    serde_json::Value,
    fn(&str) -> Result<RawReply, String>,
);

fn extract_chat(text: &str) -> Result<RawReply, String> {
    let parsed: ApiResp = serde_json::from_str(text).map_err(|e| {
        format!(
            "解析接口响应失败: {e}；原文片段: {}",
            crate::exec::clip(text, 500)
        )
    })?;
    let finish_reason = parsed.choices.first().and_then(|c| c.finish_reason.clone());
    let content = parsed
        .choices
        .first()
        .and_then(|c| c.message.as_ref())
        .map(|m| m.content.clone())
        .unwrap_or_default();
    Ok(RawReply {
        content,
        usage: parsed.usage.unwrap_or_default(),
        model: parsed.model,
        finish_reason,
        web_queries: Vec::new(),
    })
}

fn extract_responses(text: &str) -> Result<RawReply, String> {
    let parsed: RespApi = serde_json::from_str(text).map_err(|e| {
        format!(
            "解析接口响应失败: {e}；原文片段: {}",
            crate::exec::clip(text, 500)
        )
    })?;
    let mut web_queries = Vec::new();
    for it in &parsed.output {
        if it.kind != "web_search_call" {
            continue;
        }
        if let Some(a) = &it.action {
            // 服务端会在查询词里塞一条 `ws_call_id=...` 的内部标记，那不是查询词
            web_queries.extend(
                a.queries
                    .iter()
                    .filter(|q| !q.starts_with("ws_call_id="))
                    .cloned(),
            );
        }
    }
    // 正文只在 `message` 项里（`reasoning` 项的 reasoning_text 是模型的思考，不是回答）
    let content = parsed
        .output
        .iter()
        .filter(|i| i.kind == "message")
        .flat_map(|i| i.content.iter())
        .filter(|c| c.kind != "reasoning_text")
        .map(|c| c.text.as_str())
        .collect::<String>();
    let finish_reason = match parsed.status.as_deref() {
        Some("completed") => Some("stop".to_string()),
        Some("incomplete") => Some("length".to_string()),
        Some("failed") => Some("failed".to_string()),
        _ => None,
    };
    let usage = parsed
        .usage
        .map(|u| Usage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
        })
        .unwrap_or_default();
    Ok(RawReply {
        content,
        usage,
        model: parsed.model,
        finish_reason,
        web_queries,
    })
}

/// 带重试的一次对话调用。
///
/// 走哪套协议由 `web_search_on` 决定：开了联网就走 `/responses`（服务端检索），
/// 否则走原来的 `/chat/completions`。**两条路共用同一套重试与错误分类** ——
/// 分叉只在请求体与解析函数上，不在重试语义上。
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

    let (url, body, extract): Protocol = if web_search_on(cfg) {
        let (u, b) = responses_parts(cfg, messages, json_mode);
        (u, b, extract_responses)
    } else {
        let (u, b) = chat_parts(cfg, messages, json_mode);
        (u, b, extract_chat)
    };

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
                    let raw = extract(&text)?;
                    if raw.content.trim().is_empty() {
                        // 空内容多是模型波动或被 max_tokens 截断：按可重试错误走重试循环
                        last_err = if raw.finish_reason.as_deref() == Some("length") {
                            "模型返回了空内容（finish_reason=length，疑似被 max_tokens 截断，可在设置里调大）".to_string()
                        } else {
                            "模型返回了空内容".to_string()
                        };
                        observe_llm_fail(cfg, messages, attempt, &last_err, started_ms);
                    } else {
                        let out = ChatOutcome {
                            content: raw.content,
                            usage: raw.usage,
                            model: raw.model.unwrap_or_else(|| cfg.model.clone()),
                            finish_reason: raw.finish_reason,
                            elapsed_ms: started.elapsed().as_millis(),
                            web_queries: raw.web_queries,
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
                    // 400：请求格式错；422：请求体能解析但字段不合法（例如往
                    // /chat/completions 塞了 web_search 工具）—— 都是"重试也一样错"
                    if status.as_u16() == 400 || status.as_u16() == 422 {
                        return Err(format!("DeepSeek 请求被拒绝(HTTP {status}): {detail}"));
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

    fn cfg_at(url: &str, web_search: &str) -> LlmConfig {
        LlmConfig {
            base_url: url.to_string(),
            web_search: web_search.to_string(),
            ..LlmConfig::default()
        }
    }

    #[test]
    fn web_search_defaults_to_auto_and_only_fires_on_deepseek_endpoints() {
        let cfg = LlmConfig::default();
        assert_eq!(cfg.web_search, "auto", "默认要开，否则联网等于没做");
        assert!(is_deepseek_endpoint(&cfg));
        assert!(web_search_on(&cfg));
        // 带路径与结尾斜杠的写法也要认（用户常配成 https://api.deepseek.com/v1/）
        assert!(is_deepseek_endpoint(&cfg_at(
            "https://api.deepseek.com/v1/",
            "auto"
        )));

        // 换了端点必须自动关掉：往非 DeepSeek 端点塞 web_search 会被 422 打回
        // （实测 `unknown variant \`web_search\`, expected \`function\``）
        assert!(!is_deepseek_endpoint(&cfg_at(
            "https://api.openai.com/v1",
            "auto"
        )));
        assert!(!web_search_on(&cfg_at("https://api.openai.com/v1", "auto")));
        assert!(!is_deepseek_endpoint(&cfg_at(
            "http://127.0.0.1:11434/v1",
            "auto"
        )));

        // off 是回滚开关；on 是用户知情后强行开
        assert!(!web_search_on(&cfg_at("https://api.deepseek.com", "off")));
        assert!(web_search_on(&cfg_at("https://api.openai.com/v1", "on")));
        // 大小写不敏感（配置值宽容无害）："Auto" 与 "auto" 同义
        assert!(web_search_on(&cfg_at("https://api.deepseek.com", "Auto")));
        // 认不出的档位按 off 处理：宁可不联网，也不要发一个服务端不认的请求
        assert!(!web_search_on(&cfg_at(
            "https://api.deepseek.com",
            "sometimes"
        )));
        assert!(!web_search_on(&cfg_at("https://api.deepseek.com", "")));
    }

    /// 能力是**逐模型**的：flash 实测一次都不检索（模型自己说没有这个工具）。
    /// 不查能力就换协议，等于白走一条新路径却什么也没多拿到 —— 这条钉的正是那道闸。
    #[test]
    fn auto_also_requires_the_model_to_actually_have_web_search() {
        let pro = cfg_at("https://api.deepseek.com", "auto");
        assert_eq!(pro.model, "deepseek-v4-pro");
        assert!(model_caps("deepseek-v4-pro").web_search);
        assert!(web_search_on(&pro));

        // 同样在 DeepSeek 端点上，换 flash 就必须自动关掉
        let mut flash = cfg_at("https://api.deepseek.com", "auto");
        flash.model = "deepseek-v4-flash".into();
        assert!(!model_caps("deepseek-v4-flash").web_search);
        assert!(!web_search_on(&flash), "flash 搜不了，不该为它换协议");

        // 官方推荐名与旧名必须同能力（改名不该悄悄丢能力）
        assert_eq!(
            model_caps("deepseek-flash"),
            model_caps("deepseek-v4-flash")
        );
        assert!(model_caps("deepseek-v4-flash").multimodal);
        assert!(!model_caps("deepseek-v4-pro").multimodal);

        // 表里没有的模型按"都没能力"处理（宁可不开，也不为不存在的能力换协议）
        assert_eq!(
            model_caps("deepseek-v5-ultra"),
            ModelCaps {
                web_search: false,
                multimodal: false
            }
        );

        // on = 用户知情后强行开，不看能力表
        let mut forced = cfg_at("https://api.deepseek.com", "on");
        forced.model = "deepseek-v4-flash".into();
        assert!(web_search_on(&forced));

        assert!(!known_models().is_empty());
    }

    #[test]
    fn the_switch_picks_a_whole_protocol_not_just_one_extra_field() {
        let cfg = cfg_at("https://api.deepseek.com", "auto");
        let msgs = vec![
            ChatMessage::system("你是 A"),
            ChatMessage::user("最近的消息"),
        ];

        let (url, body) = responses_parts(&cfg, &msgs, true);
        assert!(url.ends_with("/responses"));
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert_eq!(body["text"]["format"]["type"], "json_object");
        assert_eq!(body["input"][0]["role"], "system");
        assert_eq!(body["input"][1]["content"], "最近的消息");
        assert!(
            body.get("messages").is_none(),
            "Responses 协议不带 messages，混着发服务端不认"
        );

        // 关掉就退回原链路：路径、字段、json 模式全都回到 /chat/completions 那一套
        let (url2, body2) = chat_parts(&cfg_at("https://api.deepseek.com", "off"), &msgs, true);
        assert!(url2.ends_with("/chat/completions"));
        assert!(body2["messages"].is_array());
        assert_eq!(body2["response_format"]["type"], "json_object");
        assert!(body2.get("tools").is_none(), "没开联网就不许带 tools");
    }

    /// 语料是**真抓回来**的一次 `/responses` 应答（开联网问上证收盘点位那次），不是编的。
    #[test]
    fn a_real_responses_reply_yields_text_usage_and_the_queries_it_searched() {
        let raw = r#"{
          "id":"ef6960fc","object":"response","status":"completed","model":"deepseek-v4-pro",
          "output":[
            {"type":"reasoning","id":"r1","content":[{"type":"reasoning_text","text":"用户想知道2026年9月18日上证指数的收盘点位。"}]},
            {"type":"web_search_call","id":"call_00_x","status":"completed",
             "action":{"type":"search","queries":["2026年9月18日 上证指数 收盘点位","ws_call_id=call_00_x"]}},
            {"type":"message","id":"m1","status":"completed",
             "content":[{"type":"output_text","annotations":[],"text":"{\"answer\":\"3911.87\"}"}]}
          ],
          "usage":{"input_tokens":3069,"output_tokens":176,"total_tokens":3245}
        }"#;
        let r = extract_responses(raw).unwrap();
        assert_eq!(r.content, "{\"answer\":\"3911.87\"}");
        assert!(
            !r.content.contains("用户想知道"),
            "reasoning 是模型的思考，不是回答 —— 混进正文会被当成模型说的话"
        );
        assert_eq!(
            r.web_queries,
            vec!["2026年9月18日 上证指数 收盘点位".to_string()],
            "`ws_call_id=...` 是服务端的内部标记，不是查询词"
        );
        assert_eq!(r.usage.prompt_tokens, 3069);
        assert_eq!(r.usage.completion_tokens, 176);
        assert_eq!(r.usage.total_tokens, 3245);
        assert_eq!(r.finish_reason.as_deref(), Some("stop"));
        assert_eq!(r.model.as_deref(), Some("deepseek-v4-pro"));
    }

    #[test]
    fn an_incomplete_responses_reply_looks_like_truncation() {
        let raw = r#"{"status":"incomplete","model":"m",
            "output":[{"type":"message","content":[{"type":"output_text","text":""}]}],
            "usage":{"input_tokens":10,"output_tokens":800}}"#;
        let r = extract_responses(raw).unwrap();
        assert_eq!(
            r.finish_reason.as_deref(),
            Some("length"),
            "incomplete 要映射成 length，否则主循环分不清'格式烂'和'没写完'"
        );
        assert!(r.content.is_empty());
        assert!(r.web_queries.is_empty());
    }

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
