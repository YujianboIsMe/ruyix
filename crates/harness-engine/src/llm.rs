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
    /// 工具协议：assistant 消息里**原样回显**的 tool_calls。
    ///
    /// 本引擎用一种**混合**形态（详见 `doc/v0.x/需求-工具协议改造-v0.0.6.md`）：请求里声明 `tools`
    /// 让模型把动作发进 `tool_calls`（这是治 DSML 泄露的关键 —— 实测声明后 4 臂对照里
    /// 零泄露、6/8 走标准调用），但**自己不在这条消息链上做 tool 记账**：下一轮把动作以
    /// 我们自己的 JSON 形状回显（模型看得懂，且与老协议的历史写法一致），观察结果照旧走
    /// user 消息。这样既拿到"模型的原生调用有地方可去"，又不用把观察结果全部改成 tool
    /// 角色 + 逐条对 id（那会牵动历史折叠、批次与所有观察落点）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 工具协议：`role = tool` 的结果消息指向它答复的那次调用（混合形态下暂不用，
    /// 留着这条通路是为了将来真要切"纯标准协议"时不用再动结构）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// 模型**发出来**的一次工具调用（响应里 `message.tool_calls` 的一项）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    pub function: ToolCallFn,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCallFn {
    #[serde(default)]
    pub name: String,
    /// 参数是**一个 JSON 字符串**（不是对象）—— 实测 DeepSeek 也会给不合法/截断的串，
    /// 所以解析放在调用方，且失败只影响那一条调用（不整轮作废）。
    #[serde(default)]
    pub arguments: String,
}

impl ChatMessage {
    pub fn system(c: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: c.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: c.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant(c: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: c.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    /// 工具结果消息（`role = tool`）—— 混合形态下暂未使用，见 [`ChatMessage::tool_calls`]。
    pub fn tool_result(id: impl Into<String>, c: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: c.into(),
            tool_calls: None,
            tool_call_id: Some(id.into()),
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
    /// 标准工具协议返回的动作（模型按请求里声明的 `tools` 发回来的 `tool_calls`）。
    ///
    /// 与 `content` **并列**：`final` 之前的每一轮，模型要么给工具调用、要么（老习惯）把
    /// 动作写在 content 的 JSON 里 —— 两条路都要认，因为实测声明 `tools` 之后仍有 2/8 轮
    /// 走老形状（见 `doc/v0.x/问题-DSML标记泄露.md` 的 4 臂对照）。
    pub tool_calls: Vec<ToolCall>,
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
    /// `type == "function_call"` 才有：这次调用的 id（回灌结果时要带回去）。
    #[serde(default)]
    call_id: Option<String>,
    /// `type == "function_call"` 才有：工具名。
    #[serde(default)]
    name: Option<String>,
    /// `type == "function_call"` 才有：参数（**JSON 字符串**，与 chat/completions 一致）。
    #[serde(default)]
    arguments: Option<String>,
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
    /// 标准工具协议的调用清单（模型按请求里声明的 `tools` 发回来的动作）
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
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
    let (cleaned, _) = strip_model_markup(raw);
    escape_raw_controls(&extract_json_object_inner(&cleaned))
}

/// 剥掉**模型自带协议**的标记（DSML = DeepSeek Markup Language 这类原生工具调用语法）。
///
/// 本引擎**会声明 `tools`**（v0.0.6 起：OpenAI 兼容走 `tool_calls`，anthropic 走 `tool_use`）——
/// 但被原生工具调用语法
/// 训练过的模型会时不时改用**它自己那一套**：参数写对了（比如 `{"cmd": "…"}`），外面却裹着
/// 它的尖括号标记 —— 服务端没收到 `tools`，就不会把它解析进 `tool_calls`，整段原样落进
/// content。实测（2026-09-21，第 5 轮）：content 是那行命令 JSON + 三行闭合标记，
/// 模型其实已经算出了正确命令，却因为标记挂在 JSON 后面被当成"输出无法解析"，白烧一轮。
///
/// 判定只有一条：**尖括号里成对竖线出现两处**的，都是模型自带标记。竖线有全角
/// （`｜`，U+FF5C，DeepSeek 模板原样）与半角（`|`，有的网关会换）两种，都认。
/// 两处的形状是实测出来的：DSML 的标签是"竖线对 + `DSML` + 竖线对 + 标签名（可带属性）"，
/// 模板自带的句首/句尾标记是"竖线对 + 少量文字 + 竖线对"。
/// 要求**两处**而不是一处，是不误伤的根据 —— JSON 里的竖线是单根管道
/// （`netstat -ano | findstr 8083` 这种命令），构不成"尖括号里两对竖线"。
///
/// 分两条正则，是因为两种标签的**可信度不同**：
/// ① 带 `DSML` 字样的：连属性里的引号一起放行（`name="execute"` 就在标签里），
///    有那个字样的几乎不可能是别的东西；
/// ② 模板自带的短标记：不许出现引号，长度也卡死 —— 引号是 JSON 字符串的常见内容，
///    不卡它会拿"字符串值里恰好长成那样的一小段"去剪，那是在改模型的正文。
///
/// 返回 `(清理后的文本, 剥掉了几处)`：计数给错误文本用（见 `agent::markup_note`）——
/// **只剥不说是治不好的**，标记被静默剪掉后模型看到的只是"无法解析"，它会原样重发。
pub fn strip_model_markup(raw: &str) -> (String, usize) {
    let mut text = raw.to_string();
    let mut n = 0usize;
    for re in markup_regexes() {
        let hits = re.find_iter(&text).count();
        if hits == 0 {
            continue;
        }
        n += hits;
        text = re.replace_all(&text, "").into_owned();
    }
    (text, n)
}

/// 剥标记用的两条常量正则（编译一次，进程内复用）。
fn markup_regexes() -> &'static [regex::Regex; 2] {
    static RE: std::sync::OnceLock<[regex::Regex; 2]> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        [
            // ① 带 DSML 字样的标签：`<` / `</` 之后就是竖线对，然后是 DSML、再一对竖线，后面到 `>` 为止
            regex::Regex::new("(?i)</?[|｜]{2}\\s*DSML[|｜]{2}[^>]{0,80}>")
                .expect("DSML 标签正则写错了（常量正则，构建期就该发现）"),
            // ② 模板自带的短标记：形状同上，但**不许有引号/换行**，长度也卡死
            regex::Regex::new("</?[|｜]{2}[^>\"\\n]{0,32}[|｜]{2}[^>\"\\n]{0,32}>")
                .expect("模板标记正则写错了（常量正则，构建期就该发现）"),
        ]
    })
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
    /// 标准工具协议返回的动作（没声明 `tools`、或模型这轮没用工具时为空）
    tool_calls: Vec<ToolCall>,
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
///
/// **联网检索还要再分一维：协议。** 它是**端点提供的服务端工具**，两条路的工具名不同，
/// 于是"同一个模型在一条路上能搜、在另一条路上搜不了"是常态而非异常：
///
/// | 模型 | anthropic（`web_search_20250305`） | `/responses`（`{type:web_search}`） |
/// |---|---|---|
/// | `deepseek-v4-pro` | ✅ | ✅ |
/// | `deepseek-flash` / `deepseek-v4-flash` | ✅ | ❌ |
/// | `deepseek-v4-flash-vision-exp` | ✅ | ❌ |
///
/// （2026-09-25 四个模型 × 两条协议各打一遍实测。右列与旧的单维表一致 ——
/// flash 之所以曾被判"搜不了"，是因为当时只在 `/responses` 上量过。）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelCaps {
    /// 服务端联网检索 · OpenAI 兼容路（`/responses` + `tools:[{type:web_search}]`）
    pub web_search: bool,
    /// 服务端联网检索 · anthropic 路（`/v1/messages` + `tools:[{type:web_search_20250305}]`）
    pub web_search_anthropic: bool,
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
            web_search_anthropic: true,
            multimodal: false,
        },
    ),
    // 注意 `deepseek-flash` 是官方推荐名，`deepseek-v4-flash` 是仍被接受的旧名
    // —— 两个名字指向同一个模型，能力要一致，否则改名就等于悄悄丢能力。
    (
        "deepseek-flash",
        ModelCaps {
            web_search: false,
            web_search_anthropic: true,
            multimodal: true,
        },
    ),
    (
        "deepseek-v4-flash",
        ModelCaps {
            web_search: false,
            web_search_anthropic: true,
            multimodal: true,
        },
    ),
    (
        "deepseek-v4-flash-vision-exp",
        ModelCaps {
            web_search: false,
            web_search_anthropic: true,
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
            web_search_anthropic: false,
            multimodal: false,
        })
}

/// 本端点上**全部已知模型**及其能力（给宿主做下拉框与能力提示）。
pub fn known_models() -> &'static [(&'static str, ModelCaps)] {
    MODEL_CAPS
}

/// 某模型在**某协议**下能不能服务端联网检索 —— 能力表那一维的取值口径，
/// 只此一处（`auto` 档与 UI 的开关可见性都从它来，避免两处各判一次）。
pub fn web_search_capable(model: &str, api_format: &str) -> bool {
    let caps = model_caps(model);
    if api_format.trim().eq_ignore_ascii_case("anthropic") {
        caps.web_search_anthropic
    } else {
        caps.web_search
    }
}

/// 本次调用要不要挂服务端联网搜索（配置键 `llm.web_search`）。
///
/// - `off`：永不开（联网出问题时**这就是回滚开关**，一行配置即退回老链路）；
/// - `auto`（默认）：DeepSeek 官方端点 **且** 该模型在**本协议**下真有联网能力 ——
///   缺一不可，否则就是换了协议却搜不了；
/// - `on`：强行开（自建兼容端点自己认这个工具时用，**不看能力表**）。
///
/// **2026-09-25 修**：以前这里对 anthropic **直接早返回 false**，即"anthropic 一律不支持
/// 服务端联网"。实测是错的：DeepSeek 的 `/anthropic` 认 anthropic 原生的服务端检索工具
/// （`web_search_20250305`），四个模型全都能搜。更糟的是那个早返回把用户设的 `on` 也一起吃掉
/// —— 配置说开着、请求里没有、界面也不报错（用户报的正是"为什么没法用 web 搜索"）。
/// 判断依据回到能力表那两维，`on` 恢复"强行开"的本义。
pub fn web_search_on(cfg: &LlmConfig) -> bool {
    match cfg.web_search.trim().to_ascii_lowercase().as_str() {
        "on" => true,
        "auto" => is_deepseek_endpoint(cfg) && web_search_capable(&cfg.model, &cfg.api_format),
        _ => false,
    }
}

/// anthropic 的服务端联网工具是**带版本的类型名**，不是 `web_search`。
///
/// 实测（2026-09-25）：发 `{"type":"web_search"}` 会被 422 打回，且服务端在报错里
/// 自己给了正确取值 —— `unknown variant \`web_search\`, expected \`web_search_20250305\`
/// or \`web_search_20260209\``。取旧的那个（兼容面更宽）。
pub const WEB_SEARCH_ANTHROPIC_TYPE: &str = "web_search_20250305";

/// 一轮里最多让服务端检索几次 —— 给模型一个上限，别把一轮烧成十次检索。
pub const WEB_SEARCH_MAX_USES: u32 = 5;

/// 全部工具名（**必须与 [`TOOL_DECLS`] 一行不改地对齐** —— 契约单测同时断言两边）。
///
/// 拆出它是因为 `tools` 现在按**调用方需要的子集**声明：主循环给全套，复核员只给 `read`
/// （它只能读，给了 `write` 等于把只读约束交给模型自觉）。
pub const TOOL_NAMES_ALL: &[&str] = &[
    "read",
    "write",
    "execute",
    "connect",
    "plan",
    "ask_user",
    "record_findings",
    "final",
];

/// 声明给模型的工具（表驱动）。
///
/// ## 为什么非要有它（这次的病根）
///
/// 引擎原先从不声明 `tools`，动作全靠"content 里的 JSON"这条约定。但 DeepSeek 这类模型被
/// **原生工具调用语法**训练过：它会把自己的调用写成 DSML 标记吐进 content，而服务端没收到
/// `tools` 就**不会**把它解析进 `tool_calls` —— 于是"模型明明算对了的命令"变成一整轮作废
/// （实测真跑 **5/16 轮**，见 `doc/v0.x/问题-DSML标记泄露.md`）。4 臂 × 8 轮对照里，声明 `tools`
/// 的两臂**零泄露、6/8 走标准 `tool_calls`**，且与 `response_format=json_object` 不冲突。
///
/// ## 与 [`crate::agent::parse_one`] 的契约（改一处必须看另一处）
///
/// `name` 与 `parameters` 的形状必须与 `parse_one` 认的**逐字一致**（`parameters` 那一层
/// 就是它眼里的 `args`）：名字对不上 = "未知能力"，参数键对不上 = "参数不合法"。
/// 单测 `tools_cover_every_parser_capability` 钉住"工具名与能力名一一对应"。
///
/// `final` 不是"第五个能力"：它是**交付动作**（老协议里是 `{"final":"…"}` 这个字段），
/// 所以这里跟在四种能力后面单独说明。
pub fn tool_decls() -> Vec<serde_json::Value> {
    tool_decls_for(TOOL_NAMES_ALL)
}

/// 只声明**指定的**那几个工具（顺序按表里的顺序，认不出的名字直接忽略）。
///
/// 子集不是可选的锦上添花：复核员只许 `read`、步骤执行体没有 `connect` —— 声明面就是权限面，
/// 给谁多声明一个工具，等于把一条能力交到它的自觉上。
pub fn tool_decls_for(names: &[&str]) -> Vec<serde_json::Value> {
    TOOL_DECLS
        .iter()
        .filter(|(n, _, _)| names.iter().any(|w| w.eq_ignore_ascii_case(n)))
        .map(|(name, desc, params)| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": desc,
                    // 表里写的是 JSON 源串：常量表不能直接放 Value（const 里没有堆分配），
                    // 解析失败属于写错常量，构建期就该炸 —— 单测会逐条解析一遍。
                    "parameters": serde_json::from_str::<serde_json::Value>(params)
                        .expect("工具参数表写错了（常量 JSON，单测会先抓到）"),
                }
            })
        })
        .collect()
}

/// 工具的（名字 / 说明 / 参数 JSON 源串）表。**契约见 [`tool_decls`] 的文档。**
const TOOL_DECLS: &[(&str, &str, &str)] = &[
    (
        "read",
        "读项目内的文件（或目录列表）。大文件用 offset/limit 分段读；连续读多个互不依赖的文件请在一轮里发多个 read。",
        r#"{"type":"object","properties":{"path":{"type":"string","description":"项目内相对路径，用 / 分隔"},"offset":{"type":"integer","description":"从第几行开始读（1 起）"},"limit":{"type":"integer","description":"最多读多少行"}},"required":["path"]}"#,
    ),
    (
        "write",
        "写项目内的文件（整文件）。改已有文件优先用 edits 只传改动；新建文件或整篇重排才用 content 交回整份。两个都给会被拒。",
        r#"{"type":"object","properties":{"path":{"type":"string","description":"项目内相对路径"},"content":{"type":"string","description":"整份新内容（新建/整篇重排）"},"edits":{"type":"array","description":"锚点替换（改已有文件的首选）","items":{"type":"object","properties":{"find":{"type":"string","description":"要被替换的原文（必须唯一）"},"replace":{"type":"string","description":"换成什么"}},"required":["find","replace"]}}},"required":["path"]}"#,
    ),
    (
        "execute",
        "在项目根执行命令。前台有界：{cmd, timeout_secs}（编译/测试/git）。起常驻服务：{cmd, background:true, ready_cmd, ready_timeout_secs, keep_alive}，ready_cmd 退出码 0 即算就绪，返回 handle。句柄操作：{op:\"status\"|\"log\"|\"stop\", handle}。",
        r#"{"type":"object","properties":{"cmd":{"type":"string","description":"要执行的命令行"},"timeout_secs":{"type":"integer","description":"前台最长等多久"},"background":{"type":"boolean","description":"true = 托管常驻进程，起完就返"},"ready_cmd":{"type":"string","description":"后台就绪判据：退出码 0 即就绪"},"ready_timeout_secs":{"type":"integer","description":"等就绪的上限"},"keep_alive":{"type":"boolean","description":"run 结束是否保留这个进程（服务默认要留）"},"op":{"type":"string","description":"句柄操作：status / log / stop"},"handle":{"type":"string","description":"后台进程的句柄（op 时必填）"}},"required":[]}"#,
    ),
    (
        "connect",
        "连项目之外的能力：MCP 工具与远端 agent。action=list 看清单；action=call 调 MCP 工具；action=send 给远端 agent 派任务。项目内的读写执行别用它。",
        r#"{"type":"object","properties":{"action":{"type":"string","description":"list / call / send"},"server":{"type":"string","description":"MCP 服务器名（call）"},"tool":{"type":"string","description":"工具名（call）"},"arguments":{"type":"object","description":"工具参数（call）"},"agent":{"type":"string","description":"远端 agent 名（send）"},"text":{"type":"string","description":"派给远端 agent 的话（send）"}},"required":["action"]}"#,
    ),
    (
        "plan",
        "任务清单：要动多个文件时先发，用户会在大纲区看到进度，引擎也按它逐步派发。files 只列**这一步真的会写**的文件（拿它对账，列多了会让做完的步骤看起来没做完）。",
        r#"{"type":"object","properties":{"steps":{"type":"array","items":{"type":"object","properties":{"title":{"type":"string","description":"短标题"},"detail":{"type":"string","description":"这一步做什么（一句话）"},"files":{"type":"array","items":{"type":"string"},"description":"这一步真的会写的文件（相对路径）"},"kind":{"type":"string","description":"code / test / config / doc"}},"required":["title"]}}},"required":["steps"]}"#,
    ),
    (
        "ask_user",
        "需求有歧义、且猜错会白做时，问委托人（独占一轮）。必须给 why：说明这个答案会决定接下来的什么动作。答案不构成任何授权。",
        r#"{"type":"object","properties":{"question":{"type":"string","description":"要问的问题"},"why":{"type":"string","description":"为什么必须问（决定接下来什么动作）"},"options":{"type":"array","items":{"type":"string"},"description":"候选答案（最多 5 个，用户也可自由输入）"},"default_index":{"type":"integer","description":"推荐第几个选项（0 起）"}},"required":["question","why"]}"#,
    ),
    (
        "record_findings",
        "把**你已确认的事实**记进本 run 的进展记忆（永不折叠，下一轮仍可见）。每读出一件会改变后续决策的事实就记一条；claim 一句话，evidence 给 path:line 或 命令+退出码。修正旧结论时用 supersedes 指向旧条目 id（旧条留在账本里、不再出现在你眼前）。可与其它调用同批发出。",
        r#"{"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"claim":{"type":"string"},"evidence":{"type":"string"},"note":{"type":"string"},"supersedes":{"type":"string"}},"required":["claim","evidence"]}}},"required":["items"]}"#,
    ),
    (
        "final",
        "交付：全部做完后调用它结束本轮。answer 是给用户的完整说明（Markdown：结论、改了哪些文件、验证结果），不要粘贴命令原始输出或整段文件内容。",
        r#"{"type":"object","properties":{"answer":{"type":"string","description":"给用户的完整说明（Markdown）"}},"required":["answer"]}"#,
    ),
];

fn chat_parts(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    json_mode: bool,
    tool_names: &[&str],
) -> (String, serde_json::Value) {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "temperature": cfg.temperature,
        "max_tokens": cfg.max_tokens,
        "stream": false,
    });
    apply_reasoning(&mut body, cfg, false);
    if json_mode {
        body["response_format"] = serde_json::json!({ "type": "json_object" });
    }
    // 标准工具协议（`tool_names` 由调用方显式给，见 [`chat_with_tools`]）：**这是治 DSML 泄露
    // 的那一步** —— 声明之后模型的原生调用进了 `tool_calls`，而不是被截头后落进 content。
    if !tool_names.is_empty() {
        body["tools"] = serde_json::Value::Array(tool_decls_for(tool_names));
    }
    (url, body)
}

/// `/responses` 的请求体。OpenAI 兼容路的服务端联网搜索在这条路上（`{type:web_search}`）；
/// anthropic 路的同名能力走 `/v1/messages` + `web_search_20250305`，那条在 [`anthropic_parts`]。
/// Responses 形态的函数工具声明：`{type:"function", name, description, parameters}` —— **扁平**，
/// 不像 `/chat/completions` 那样再裹一层 `function`。发错了的表现是 400，或更糟：
/// 不报错但模型从不调用（声明没被认成工具）。
fn responses_tool_decls(names: &[&str]) -> Vec<serde_json::Value> {
    TOOL_DECLS
        .iter()
        .filter(|(n, _, _)| names.iter().any(|w| w.eq_ignore_ascii_case(n)))
        .map(|(name, desc, params)| {
            serde_json::json!({
                "type": "function",
                "name": name,
                "description": desc,
                "parameters": serde_json::from_str::<serde_json::Value>(params)
                    .expect("工具参数表写错了（常量 JSON，单测会先抓到）"),
            })
        })
        .collect()
}

fn responses_parts(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    json_mode: bool,
    tool_names: &[&str],
) -> (String, serde_json::Value) {
    let url = format!("{}/responses", cfg.base_url.trim_end_matches('/'));
    let input: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| serde_json::json!({ "type": "message", "role": m.role, "content": m.content }))
        .collect();
    let mut tools: Vec<serde_json::Value> = vec![serde_json::json!({ "type": "web_search" })];
    tools.extend(responses_tool_decls(tool_names));
    let mut body = serde_json::json!({
        "model": cfg.model,
        "input": input,
        "temperature": cfg.temperature,
        "max_output_tokens": cfg.max_tokens,
    "stream": false,
        // 服务端联网检索 + **函数工具**共处一个数组（`type` 区分）。
        // 以前这里**只有** `web_search` —— 那条路上模型一个函数工具都拿不到，严格模式（默认开）
        // 下就只能把动作写进正文，于是每轮"没有工具调用"直到烧完预算：与 anthropic 那条路
        // 是**同一个病**（用户实测报的"发一句你好就死循环"）。
        "tools": tools,
    });
    if json_mode {
        body["text"] = serde_json::json!({ "format": { "type": "json_object" } });
    }
    (url, body)
}

/// 鉴权方式：OpenAI 兼容用 `Bearer`；Anthropic 用 `x-api-key` + `anthropic-version`。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthScheme {
    Bearer,
    Anthropic,
}

/// 一种协议的全部差异：端点、请求体、把响应解析成 [`RawReply`] 的函数，以及鉴权方式。
/// 抽成别名不只是为了可读性 —— 内联这个元组会被 clippy 判 `very_complex_type`。
type Protocol = (
    String,
    serde_json::Value,
    fn(&str) -> Result<RawReply, String>,
    AuthScheme,
);

// ---- Anthropic Messages 协议（`/v1/messages`） ----
// 与 OpenAI 兼容协议的主要差异：鉴权用 `x-api-key` + `anthropic-version` 头；system 是
// 顶层字段（不在 messages 里）；没有 `response_format`（JSON 靠提示词保证）；
// 联网检索走**它自己的**服务端工具 `web_search_20250305`（见 `WEB_SEARCH_ANTHROPIC_TYPE`），
// 与 OpenAI 那条路的 `{type:web_search}` 是两套名字，混用会被 422 打回。

#[derive(Deserialize, Default)]
struct AnthropicResp {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<AnthropicContent>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize, Default)]
struct AnthropicContent {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
    /// `type == "tool_use"` 才有：这次调用的 id（回灌 tool_result 时要原样带回去）。
    #[serde(default)]
    id: String,
    /// `type == "tool_use"` 才有：工具名。
    #[serde(default)]
    name: String,
    /// `type == "tool_use"` 才有：参数**已经是对象**（不是 JSON 字符串）——
    /// 这是与 OpenAI 那条路最大的差异，见 `extract_anthropic`。
    #[serde(default)]
    input: serde_json::Value,
}

#[derive(Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

fn anthropic_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/messages") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

fn anthropic_models_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

/// anthropic 形态的工具声明：`{name, description, input_schema}`。
///
/// 与 OpenAI 的 `{type:"function", function:{…}}` 是**两套**格式，不能复用同一条数组 ——
/// 用错的表现是 400（`tools.0.type` 之类），或者更糟：不报错但模型从不发 `tool_use`。
fn anthropic_tool_decls(names: &[&str]) -> Vec<serde_json::Value> {
    TOOL_DECLS
        .iter()
        .filter(|(n, _, _)| names.iter().any(|w| w.eq_ignore_ascii_case(n)))
        .map(|(name, desc, params)| {
            serde_json::json!({
                "name": name,
                "description": desc,
                // OpenAI 管它叫 `parameters`，anthropic 管它叫 `input_schema` —— 同一份 JSON Schema
                "input_schema": serde_json::from_str::<serde_json::Value>(params)
                    .expect("工具参数表写错了（常量 JSON，单测会先抓到）"),
            })
        })
        .collect()
}

fn anthropic_parts(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    _json_mode: bool,
    tool_names: &[&str],
) -> (String, serde_json::Value) {
    let url = anthropic_url(&cfg.base_url);
    // 把 system 抽出来放顶层；其余按 user/assistant 入 messages。
    // anthropic 的 messages 只接受这两种角色，出现 system 会被 400 打回。
    let mut system_text = String::new();
    let mut msgs: Vec<serde_json::Value> = Vec::new();
    for m in messages {
        match m.role.as_str() {
            "system" => {
                if !system_text.is_empty() {
                    system_text.push('\n');
                }
                system_text.push_str(&m.content);
            }
            "user" => msgs.push(serde_json::json!({ "role": "user", "content": m.content })),
            "assistant" => {
                // 带工具调用的助手消息要用 anthropic 的 `tool_use` 块回灌（而不是塞进文本）：
                // 我们这条路平时用"调用流水 + 用户观察"的文本回放（`tool_calls_echo`），
                // 但万一某处开始回灌结构化调用，这里就**已经是对的形状**，不会打成 400。
                match &m.tool_calls {
                    Some(calls) if !calls.is_empty() => {
                        let mut blocks: Vec<serde_json::Value> = Vec::new();
                        if !m.content.trim().is_empty() {
                            blocks.push(serde_json::json!({ "type": "text", "text": m.content }));
                        }
                        for c in calls {
                            blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": c.id,
                                "name": c.function.name,
                                // 我们的 arguments 是 JSON 字符串，anthropic 要对象 —— 解不出来就空对象
                                // （宁可让模型看到"参数为空"重发一次，也不要整条请求 400）
                                "input": serde_json::from_str::<serde_json::Value>(&c.function.arguments)
                                    .unwrap_or_else(|_| serde_json::json!({})),
                            }));
                        }
                        msgs.push(serde_json::json!({ "role": "assistant", "content": blocks }));
                    }
                    _ => {
                        msgs.push(serde_json::json!({ "role": "assistant", "content": m.content }))
                    }
                }
            }
            // 工具结果：anthropic **没有** `role = "tool"` —— 结果要包成 user 消息里的
            // `tool_result` 块（且紧跟在对应 tool_use 之后）。照 OpenAI 的形状发会 400。
            "tool" => msgs.push(serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                }],
            })),
            other => msgs.push(serde_json::json!({ "role": other, "content": m.content })),
        }
    }
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": msgs,
        "max_tokens": cfg.max_tokens,
        "temperature": cfg.temperature,
        "stream": false,
    });
    apply_reasoning(&mut body, cfg, true);
    if !system_text.is_empty() {
        body["system"] = serde_json::json!(system_text);
    }
    // **声明工具**（这一步以前漏了）：不声明，模型就只能把动作写进正文 ——
    // 严格模式（`llm.tool_protocol`，默认开）下那一律作废，于是每轮都"没有工具调用"，
    // 直到烧完预算（用户实测："发一句你好，一直无限死循环"）。这是那条 bug 的**真病因**。
    //
    // 联网检索与函数工具**共处同一个数组**（靠 `type` 区分）：
    // 服务端工具项是 `{type, name, max_uses}`，我们的函数工具项是 `{name, description, input_schema}`。
    // 实测这个组合真干活（2026-09-25，`deepseek-flash`）：一轮里先出
    // `server_tool_use(web_search, query="杭州今天天气")` + `web_search_tool_result`，
    // 紧接着就是我们的 `tool_use(final)` —— 检索与交付在同一轮里完成。
    let mut tools: Vec<serde_json::Value> = Vec::new();
    if web_search_on(cfg) {
        tools.push(serde_json::json!({
            "type": WEB_SEARCH_ANTHROPIC_TYPE,
            "name": "web_search",
            "max_uses": WEB_SEARCH_MAX_USES,
        }));
    }
    tools.extend(anthropic_tool_decls(tool_names));
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
    }
    // 注意：anthropic 没有 response_format。JSON 模式靠提示词要求，这里不加任何字段，
    // 否则会被 400 打回（`unknown field 'response_format'`）。
    (url, body)
}

fn extract_anthropic(text: &str) -> Result<RawReply, String> {
    let parsed: AnthropicResp = serde_json::from_str(text).map_err(|e| {
        format!(
            "解析 Anthropic 响应失败: {e}；原文片段: {}",
            crate::exec::clip(text, 500)
        )
    })?;
    let content = parsed
        .content
        .iter()
        .filter(|c| c.kind == "text")
        .map(|c| c.text.as_str())
        .collect::<String>();
    // **anthropic 的工具调用在这里**：`content` 数组里 `type == "tool_use"` 的那些块。
    //
    // 与 OpenAI 那条路的两处关键差异（都不许想当然）：
    // ① 参数在 `input` 里**已经是对象**，而我们的 `ToolCallFn::arguments` 是**JSON 字符串**
    //    （主循环按字符串解析，好处是模型给坏串时只影响那一条调用）—— 所以这里 `to_string()`；
    // ② 调用 id 在 `tool_use.id`，不是 `tool_call_id`。
    //
    // 为什么必须有这一段（用户实测报的就是它）：不解析 `tool_use` 时，即使声明了 tools，
    // 模型的调用也只会落进 content 被当成"没有工具调用"退回，然后**每一轮都重来** ——
    // 现象就是"发一句你好就死循环"。
    let tool_calls: Vec<ToolCall> = parsed
        .content
        .iter()
        .filter(|c| c.kind == "tool_use")
        .map(|c| ToolCall {
            id: c.id.clone(),
            kind: "function".into(),
            function: ToolCallFn {
                name: c.name.clone(),
                arguments: if c.input.is_null() {
                    "{}".to_string()
                } else {
                    c.input.to_string()
                },
            },
        })
        .collect();
    // 服务端联网检索的痕迹：`server_tool_use` 块带着本次的查询词（`input.query`）——
    // 与 `/responses` 那条路的 `web_search_call.action.queries` 同义，落到**同一个**
    // `web_queries` 字段，于是"这一轮查了什么"在两条协议下都从同一处显示。
    // `web_search_tool_result`（检索结果本身）与 `thinking` 既不是正文也不是我们的工具调用，
    // 上面两个 filter（只取 `text` / 只取 `tool_use`）天然跳过它们 —— 这里也不再单独处理。
    let web_queries: Vec<String> = parsed
        .content
        .iter()
        .filter(|c| c.kind == "server_tool_use" && c.name == "web_search")
        .filter_map(|c| c.input.get("query").and_then(|q| q.as_str()))
        .map(|s| s.to_string())
        .collect();
    let finish_reason = match parsed.stop_reason.as_deref() {
        Some("max_tokens") => Some("length".to_string()),
        Some("end_turn") | Some("stop_sequence") => Some("stop".to_string()),
        // `tool_use` 是**正常**的"我还有动作"收尾（不是截断）：映射成中性值，
        // 主循环靠 `tool_calls` 非空来判定，这个值只进日志与调试。
        Some("tool_use") => Some("tool_calls".to_string()),
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
        tool_calls,
    })
}

/// 按 `api_format` 选择整套协议：anthropic → `/v1/messages`；否则按 `web_search_on`
/// 在 `/responses` 与 `/chat/completions` 间分叉。anthropic 必须最先判 ——
/// **但联网不是被它跳过的**：anthropic 有自己的服务端检索工具，由 [`anthropic_parts`] 声明。
///
/// `tool_names` 对 `/chat/completions` 与 anthropic 两条路都生效（各自一套声明与解析形态，
/// 见 [`anthropic_tool_decls`] / [`extract_anthropic`]）。
///
/// **还差一条**：`/responses`（仅 web_search 打开时走）仍是另一套形态（`function_call` 项），
/// 目前不声明也不解析工具 —— 那条路配严格模式会有和 anthropic 以前一样的病（每轮"没有工具调用"）。
/// 它没被一起改的原因是：`/responses` 只服务"要服务端联网检索"的用法，而联网检索与工具循环
/// 同时开着本身还没验证过；等有真实需求再按同一套（声明 + 解析 + 回灌）补齐。
/// 把 `llm.reasoning` 落到请求体上。
///
/// · anthropic：`thinking: {"type":"enabled","budget_tokens":N}` —— N 必须**小于** `max_tokens`
///   （越界厂商直接 400），所以按比例取并给正文留余量；
/// · OpenAI 兼容：`reasoning_effort`（low | medium | high）；
/// · 值不认（写错/空）一律回落 **medium** —— 这是"不许 0 推理强度"的落点。
pub fn reasoning_budget(effort: &str, max_tokens: u32) -> u32 {
    let share = match effort.trim().to_ascii_lowercase().as_str() {
        "low" => 0.25f32,
        "high" => 0.75f32,
        _ => 0.5f32,
    };
    ((max_tokens as f32 * share) as u32).clamp(1024, max_tokens.saturating_sub(1024).max(1024))
}

pub fn apply_reasoning(body: &mut serde_json::Value, cfg: &LlmConfig, anthropic: bool) {
    if anthropic {
        body["thinking"] = serde_json::json!({
            "type": "enabled",
            "budget_tokens": reasoning_budget(&cfg.reasoning, cfg.max_tokens),
        });
    } else {
        body["reasoning_effort"] = serde_json::json!(cfg.reasoning);
    }
}

fn request_plan(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    json_mode: bool,
    tool_names: &[&str],
) -> Protocol {
    if cfg.api_format.trim().eq_ignore_ascii_case("anthropic") {
        let (u, b) = anthropic_parts(cfg, messages, json_mode, tool_names);
        (u, b, extract_anthropic, AuthScheme::Anthropic)
    } else if web_search_on(cfg) {
        let (u, b) = responses_parts(cfg, messages, json_mode, tool_names);
        (u, b, extract_responses, AuthScheme::Bearer)
    } else {
        let (u, b) = chat_parts(cfg, messages, json_mode, tool_names);
        (u, b, extract_chat, AuthScheme::Bearer)
    }
}

fn extract_chat(text: &str) -> Result<RawReply, String> {
    let parsed: ApiResp = serde_json::from_str(text).map_err(|e| {
        format!(
            "解析接口响应失败: {e}；原文片段: {}",
            crate::exec::clip(text, 500)
        )
    })?;
    let finish_reason = parsed.choices.first().and_then(|c| c.finish_reason.clone());
    let msg = parsed.choices.first().and_then(|c| c.message.as_ref());
    let content = msg.map(|m| m.content.clone()).unwrap_or_default();
    let tool_calls = msg.map(|m| m.tool_calls.clone()).unwrap_or_default();
    Ok(RawReply {
        content,
        usage: parsed.usage.unwrap_or_default(),
        model: parsed.model,
        finish_reason,
        web_queries: Vec::new(),
        tool_calls,
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
    // **函数调用在这里**：`output` 数组里 `type == "function_call"` 的那些项。
    // 与 `/chat/completions` 相似（`arguments` 也是 **JSON 字符串**），但项目结构与字段名不同
    // （`call_id` 而不是 `id`），所以不能共用一段解析。
    let tool_calls: Vec<ToolCall> = parsed
        .output
        .iter()
        .filter(|i| i.kind == "function_call")
        .map(|i| ToolCall {
            id: i.call_id.clone().unwrap_or_default(),
            kind: "function".into(),
            function: ToolCallFn {
                name: i.name.clone().unwrap_or_default(),
                arguments: i.arguments.clone().unwrap_or_else(|| "{}".into()),
            },
        })
        .collect();
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
        // `/responses` 这条路的工具调用形态与 chat/completions 不同（`function_call` 项），
        // 本次改造只覆盖 chat/completions：这里留空 = 这一路仍走老协议（content 里的 JSON），
        // 与改造前逐字一致。要覆盖它得另写一个映射，别在这里硬塞。
        tool_calls,
    })
}

/// 带重试的一次对话调用（**声明全套工具**）；失败时若配了**备用 LLM** 且错误属于"可用性故障"
/// 则自动切换。只声明子集的场景用 [`chat_with_tools`]。
pub async fn chat(
    cfg: &LlmConfig,
    fallback: Option<&LlmConfig>,
    messages: &[ChatMessage],
    json_mode: bool,
    tools: bool,
) -> Result<ChatOutcome, String> {
    let names: &[&str] = if tools { TOOL_NAMES_ALL } else { &[] };
    chat_with_tools(cfg, fallback, messages, json_mode, names).await
}

/// 与 [`chat`] 相同，但**声明哪几个工具由调用方点名**（空切片 = 不声明工具）。
///
/// `tools = true` 时**这次调用**会声明工具（`chat/completions` 才认；另两条协议忽略它）。
/// 为什么是**每次调用的参数**而不是配置项：工具语义只属于**工具循环**（agent 主循环与
/// 步骤执行体）。plan / generate / repair / reflect / eval 那几处是"单发一次、拿 structured
/// JSON 回来"的生成调用，它们没有"工具"这个语义 —— 给它们声明工具，模型会去调工具、
/// content 变空，而它们的解析器只认 content，等于自己把自己打瘸。所以能力要显式传，
/// 不给隐式默认（`LlmConfig.tool_protocol` 只是 agent 那一侧的开关）。
///
/// 为什么点名而不是一个 bool：**声明面就是权限面** —— 复核员只许 `read`（它的活儿就是读文件），
/// 步骤执行体没有 `connect`。给谁多声明一个工具，等于把一条能力交到它的自觉上。
pub async fn chat_with_tools(
    cfg: &LlmConfig,
    fallback: Option<&LlmConfig>,
    messages: &[ChatMessage],
    json_mode: bool,
    tool_names: &[&str],
) -> Result<ChatOutcome, String> {
    if cfg.api_key.trim().is_empty() {
        return Err(
            "尚未配置 DeepSeek API Key（设置面板里填，或设环境变量 DEEPSEEK_API_KEY）".into(),
        );
    }
    let (out, last_err) =
        attempt_loop(cfg, messages, json_mode, tool_names, fallback.is_some()).await;
    if let Some(o) = out {
        return Ok(o);
    }

    // 失败的第一嫌疑是**模型名不对**（配置里手填过 / 旧配置 / 填错了字段）：厂商对不认识的名字
    // 只回一句 400（真实例：`supported API model names are …, but you passed on`），
    // 而用户拿不到"那我该填什么"。所以这里做一件比报错更有用的事 ——
    // **拿厂商清单的第一个重试一次**（这就是"默认第一个模型"的落点）。
    // 只在异常路径探清单（缓存 10 分钟）：正常调用一次额外请求都不发。
    if let Some(fixed) = model_retry_cfg(cfg, &last_err).await {
        let (out2, err2) =
            attempt_loop(&fixed, messages, json_mode, tool_names, fallback.is_some()).await;
        if let Some(o) = out2 {
            observe_model_corrected(
                &fixed,
                &format!(
                    "模型名「{}」被厂商拒绝 → 已按厂商清单改用「{}」重试成功",
                    cfg.model.trim(),
                    fixed.model
                ),
            );
            return Ok(o);
        }
        let tail = format!(
            "（已按厂商清单把模型名换成「{}」重试过一次，仍失败）",
            fixed.model
        );
        return Err(match model_hint_on_error(&fixed, &err2).await {
            Some(h) => format!("{err2}\n{tail}\n{h}"),
            None => format!("{err2}\n{tail}"),
        });
    }

    // 主用已失败。两条"不切"的先挡住 —— 都在 return 里把**主用的原始错误**原样抛出去，
    // 不包装成"切换失败"，否则用户看到的是一句含糊的话，而不是"key 错了"。
    if !is_switchable_error(&last_err) {
        return Err(last_err);
    }
    let Some(fb) = fallback else {
        return Err(last_err);
    };
    if fb.api_key.trim().is_empty() {
        return Err(format!(
            "主用 LLM 不可用（{last_err}），但备用 LLM 未配置 API Key，无法切换"
        ));
    }

    let (fb_out, fb_err) = attempt_loop(fb, messages, json_mode, tool_names, true).await;
    match fb_out {
        Some(o) => {
            observe_failover(cfg, fb, messages);
            Ok(o)
        }
        None => Err(format!(
            "主用 LLM 不可用已切换备用，但备用也失败：{fb_err}\n（主用错误：{last_err}）"
        )),
    }
}

/// 对单个配置跑带重试的调用循环，返回 `(成功结果, 底层最后错误文案)`。
///
/// `resilient`：弹性模式（主用已配备用、或本就是备用）时收窄 —— 最多 2 次尝试、单次请求
/// 超时封顶 30s。非弹性（无备用）时 3 次、完整超时，**文案与旧版完全一致**
/// （"DeepSeek 调用失败（已重试 3 次）: ..."），保证旧行为不动。
async fn attempt_loop(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    json_mode: bool,
    tool_names: &[&str],
    resilient: bool,
) -> (Option<ChatOutcome>, String) {
    let (url, body, extract, auth) = request_plan(cfg, messages, json_mode, tool_names);
    // `--debug`：先记下这次请求的全貌（端点 / 协议 / 请求体 / 模型实际看到的消息）
    dump_request(cfg, &url, auth, &body, messages, json_mode);
    let max_attempts: u32 = if resilient { 2 } else { 3 };
    // 弹性模式把单次请求超时封顶 30s：主用僵尸挂死时不会干等 timeout_secs 才切备用
    let req_timeout = if resilient {
        cfg.timeout_secs.min(30)
    } else {
        cfg.timeout_secs
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(req_timeout))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (None, format!("构建 HTTP 客户端失败: {e}")),
    };

    let started = Instant::now();
    let started_ms = crate::observe::current().map(|t| t.now()).unwrap_or(0);
    let mut last_err = String::new();

    for attempt in 1..=max_attempts {
        let req = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body);
        let req = match auth {
            AuthScheme::Anthropic => req
                .header("x-api-key", cfg.api_key.trim())
                .header("anthropic-version", "2023-06-01"),
            AuthScheme::Bearer => {
                req.header("Authorization", format!("Bearer {}", cfg.api_key.trim()))
            }
        };
        let resp = req.send().await;

        match resp {
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                // `--debug`：**原始响应体先落盘、再解析**。顺序不能反 —— 解析失败时
                // 这段原文本就是唯一证据，而"解析失败"那条日志必须能往上翻到它。
                crate::debug::note(&format!(
                    "===== LLM 原始响应体 =====\nHTTP      : {status}\n字节      : {}\n{}",
                    text.len(),
                    text
                ));
                if status.is_success() {
                    let raw = match extract(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            crate::debug::note(&format!(
                                "===== LLM 响应解析失败（第 {attempt} 次尝试，原始响应体见上）=====\n{e}"
                            ));
                            last_err = e;
                            observe_llm_fail(cfg, messages, attempt, &last_err, started_ms);
                            continue;
                        }
                    };
                    // 注意：声明了 `tools` 之后，**工具调用轮本来就是空 content**
                    // （动作全在 `tool_calls` 里）—— 只判 content 会把正常的工具轮当成
                    // "模型返回了空内容"重试，等于把整套标准协议废掉。
                    if raw.content.trim().is_empty() && raw.tool_calls.is_empty() {
                        // 空内容这条最需要 `finish_reason`：`length` 是截断、其余是模型波动，
                        // 处置方式不同 —— 所以这条分支也要记解析结果。
                        dump_reply(cfg, &raw, attempt);
                        // 空内容多是模型波动或被 max_tokens 截断：按可重试错误走重试循环
                        last_err = if raw.finish_reason.as_deref() == Some("length") {
                            "模型返回了空内容（finish_reason=length，疑似被 max_tokens 截断，可在设置里调大）".to_string()
                        } else {
                            "模型返回了空内容".to_string()
                        };
                        observe_llm_fail(cfg, messages, attempt, &last_err, started_ms);
                    } else {
                        // `--debug`：解析器**实际拿到**的结构化结果
                        dump_reply(cfg, &raw, attempt);
                        let out = ChatOutcome {
                            content: raw.content,
                            usage: raw.usage,
                            model: raw.model.unwrap_or_else(|| cfg.model.clone()),
                            finish_reason: raw.finish_reason,
                            elapsed_ms: started.elapsed().as_millis(),
                            web_queries: raw.web_queries,
                            tool_calls: raw.tool_calls,
                        };
                        // 记一次 LLM 调用：**这是"模型输出错了"唯一能复盘的地方**
                        observe_llm(cfg, messages, &out, attempt, "ok", None, started_ms);
                        return (Some(out), String::new());
                    }
                } else {
                    let detail =
                        api_error_message(&text).unwrap_or_else(|| crate::exec::clip(&text, 400));
                    // 鉴权/余额类错误：重试无意义，直接抛
                    if status.as_u16() == 401 || status.as_u16() == 403 {
                        let msg = format!("DeepSeek 鉴权失败(HTTP {status}): {detail}");
                        observe_llm_fail(cfg, messages, attempt, &msg, started_ms);
                        return (None, msg);
                    }
                    if status.as_u16() == 402 {
                        return (
                            None,
                            format!("DeepSeek 账户余额/额度问题(HTTP 402): {detail}"),
                        );
                    }
                    // 400：请求格式错；422：请求体能解析但字段不合法（例如往
                    // /chat/completions 塞了 web_search 工具）—— 都是"重试也一样错"
                    if status.as_u16() == 400 || status.as_u16() == 422 {
                        return (
                            None,
                            format!("DeepSeek 请求被拒绝(HTTP {status}): {detail}"),
                        );
                    }
                    last_err = format!("HTTP {status}: {detail}");
                }
            }
            Err(e) => {
                last_err = format!("网络错误: {e}");
            }
        }

        if attempt < max_attempts {
            // 用 tokio 的异步 sleep：在 tokio worker 上 std::thread::sleep 会霸占线程
            tokio::time::sleep(Duration::from_millis(800 * attempt as u64)).await;
        }
    }

    let mut wrapped = format!("DeepSeek 调用失败（已重试 {max_attempts} 次）: {last_err}");
    // 错在模型名的话，顺手把**厂商认的名字**贴进错误里（异常路径才探一次，且用缓存）
    if let Some(hint) = model_hint_on_error(cfg, &wrapped).await {
        wrapped.push('\n');
        wrapped.push_str(&hint);
    }
    (None, wrapped)
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

/// 主用失败是否值得切备用：仅"可用性故障"，不含鉴权 / 配额 / 参数等确定性失败。
///
/// `is_fatal_error` 命中（未配置 / 鉴权 / 余额 / 额度 / 请求被拒绝 / 客户端构建失败）
/// 一律不算 —— 那些重试也没用，且备用多半同样错。其余网络 / 5xx / 429 / 超时 / 重试耗尽
/// 才切备用（详见 `doc/v0.x/需求-LLM网关与多协议-v0.5.md`）。
pub fn is_switchable_error(e: &str) -> bool {
    if is_fatal_error(e) {
        return false;
    }
    e.contains("网络错误")
        || e.contains("HTTP 5")
        || e.contains("HTTP 429")
        || e.contains("超时")
        || e.contains("重试")
}

fn api_error_message(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("error")?
        .get("message")?
        .as_str()
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------- `--debug` 详细日志
//
// 与上面的 `observe_llm` 分工不同，不是重复：
// - `observe` 写**给机器看**的 jsonl 摘要（头部 + 截断）——长期留档，写全量会把任务
//   原文和代码灌进 trace；
// - `debug` 写**给人看**的完整全貌（完整提示词 + 原始响应体 + 解析结果）——排查
//   "这一轮为什么跑偏"时恰恰只有全量有用，所以默认关、按需 `--debug` 打开。
//
// 请求段与消息段分两次 `note`：拼成一段的话，超长提示词会先吃掉 `DEBUG_CAP` 的额度，
// 把后面的消息与结果段顶掉。

/// 记一次请求的全貌。仅 `--debug` 开启时有效。
fn dump_request(
    cfg: &LlmConfig,
    url: &str,
    auth: AuthScheme,
    body: &serde_json::Value,
    messages: &[ChatMessage],
    json_mode: bool,
) {
    if !crate::debug::enabled() {
        return;
    }
    crate::debug::note(&format!(
        "===== LLM 请求 =====\n端点        : {url}\n模型        : {}\n鉴权        : {}\njson_mode   : {json_mode}\ntemperature : {}   max_tokens: {}   超时: {}s\n消息数      : {}\n----- 请求体（实际发出去的 JSON，含工具声明 / response_format）-----\n{}",
        cfg.model,
        match auth {
            AuthScheme::Anthropic => "anthropic（x-api-key + anthropic-version）",
            AuthScheme::Bearer => "bearer（Authorization）",
        },
        cfg.temperature,
        cfg.max_tokens,
        cfg.timeout_secs,
        messages.len(),
        serde_json::to_string_pretty(body).unwrap_or_else(|e| format!("<请求体序列化失败：{e}>")),
    ));

    let mut s = String::from("----- 消息（模型实际看到的内容）-----");
    for m in messages {
        s.push_str(&format!("\n### [{}]\n{}\n", m.role, m.content));
    }
    crate::debug::note(&s);
}

/// 记解析后的结构化结果。仅 `--debug` 开启时有效。
///
/// `finish_reason` 摆在最显眼处并加了注解：它是**区分"格式烂"与"被 max_tokens 截断"
/// 的唯一线索**，而这两种病的纠偏方向完全相反（改提示词 vs 调大预算）。
fn dump_reply(cfg: &LlmConfig, raw: &RawReply, attempt: u32) {
    if !crate::debug::enabled() {
        return;
    }
    // `tool_calls` 必须单独打出来：声明工具之后**动作不在 content 里**，只打 content
    // 会让人以为"模型什么都没给"（这正是排查 DSML 那次踩过的坑的镜像）。
    let calls = if raw.tool_calls.is_empty() {
        "（无：这轮走 content 里的 JSON 或纯文本）".to_string()
    } else {
        raw.tool_calls
            .iter()
            .map(|c| {
                format!(
                    "{}({}) [id={}]",
                    c.function.name, c.function.arguments, c.id
                )
            })
            .collect::<Vec<_>>()
            .join("\n  ")
    };
    crate::debug::note(&format!(
        "===== LLM 解析结果 =====\n第 {} 次尝试\nmodel        : {}\nfinish_reason: {:?}{}\ntokens       : prompt {} + completion {} = {}\nweb_queries  : {:?}\ntool_calls   : {}\ncontent      : {} 字符\n----- content -----\n{}",
        attempt,
        raw.model.as_deref().unwrap_or(&cfg.model),
        raw.finish_reason,
        if raw.finish_reason.as_deref() == Some("length") {
            "  ← 被 max_tokens 截断，输出没写完（不是格式问题）"
        } else {
            ""
        },
        raw.usage.prompt_tokens,
        raw.usage.completion_tokens,
        raw.usage.total_tokens,
        raw.web_queries,
        raw.tool_calls.len(),
        raw.content.chars().count(),
        raw.content,
    ));
    if !raw.tool_calls.is_empty() {
        crate::debug::note(&format!(
            "----- tool_calls（模型发回来的动作）-----\n  {calls}"
        ));
    }
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

/// 记一次"模型名被校准"：配置里的名字厂商不认，本次按清单第一个跑。
///
/// 为什么必须留痕（而不是悄悄换）：用户看到的模型名与他配的不一样时，
/// 唯一的解释来源就是这里 —— 没有这条记录，"怎么跟我设的不一样"就成了悬案。
fn observe_model_corrected(cfg: &LlmConfig, note: &str) {
    let a = crate::observe::attrs(&[
        ("model", cfg.model.as_str()),
        ("endpoint", cfg.base_url.as_str()),
        ("note", note),
    ]);
    crate::observe::span_once("llm-model-corrected", "used-first-from-vendor", "ok", 0, a);
    crate::debug::note(&format!("----- 模型名校准 -----\n  {note}"));
}

/// 记一次故障切换：主用不可用、已切到备用并成功拿到回复。
fn observe_failover(primary: &LlmConfig, fb: &LlmConfig, messages: &[ChatMessage]) {
    let a = crate::observe::attrs(&[
        ("primary_model", primary.model.as_str()),
        ("fallback_model", fb.model.as_str()),
        ("fallback_endpoint", fb.base_url.as_str()),
        ("messages", &messages.len().to_string()),
    ]);
    crate::observe::span_once("llm-failover", "switched-to-backup", "ok", 0, a);
}

/// 厂商模型列表里的一条（`GET /models` 的 `data[]` 元素）。
///
/// 为什么要整条留下而不是只要 `id`：厂商在这一条里**已经声明了能力**。实测 DeepSeek 的
/// 响应带 `input_modalities: ["text","image"]`（flash）与 `["text"]`（pro）、`name`、
/// `context_window` —— 这是"这个模型收什么输入"的一手证据，比我们那张手工维护的能力表硬。
/// 界面据此决定"要不要出现录音按钮"这类问题（见 `ModelInfo::accepts`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    /// 厂商给的显示名（如 `DeepSeek-V4.1-Flash`）；没有就退回 `id`
    pub name: Option<String>,
    /// 厂商声明的输入模态。**空 = 没声明**（不等于"没有模态"），所以判据一律走 `accepts`
    pub input_modalities: Vec<String>,
    pub context_window: Option<u64>,
}

impl ModelInfo {
    /// 该模型是否声明接受某种输入模态（大小写不敏感）。
    ///
    /// 厂商没声明时一律 `false` —— **不许把"没声明"当"支持"**：发一个服务端不认的
    /// 音频块只会换来 422，而用户看到的是"按了录音却什么也没发生"。
    pub fn accepts(&self, modality: &str) -> bool {
        self.input_modalities
            .iter()
            .any(|m| m.eq_ignore_ascii_case(modality))
    }

    /// 界面显示用的名字（厂商没给就用 id）
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(self.id.as_str())
    }
}

/// 某条 URL 的来源（`scheme://host[:port]`）—— 回落用，切掉路径与查询。
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// 模型列表的**候选 URL + 鉴权**（按顺序试，第一个成功即返回）。
///
/// ① 协议自己的那条：anthropic → `/v1/models` + `x-api-key`；openai → `/models` + Bearer。
/// ② **回落到主机根**：实测 DeepSeek 的 anthropic 入口
///    `https://api.deepseek.com/anthropic/v1/models` 是 **404**，而同一家的
///    `https://api.deepseek.com/models` 是 200 且带完整能力声明。只试 ① 的后果实测过：
///    anthropic 用户的模型下拉框**静默退回文本框**（配置表单那条路就是这样）。
///    回落同时带两种鉴权：DeepSeek 那个入口认 Bearer，真 Anthropic 只认 x-api-key。
fn model_list_candidates(cfg: &LlmConfig) -> Vec<(String, AuthScheme)> {
    let base = cfg.base_url.trim_end_matches('/');
    let anthropic = cfg.api_format.trim().eq_ignore_ascii_case("anthropic");
    let mut out: Vec<(String, AuthScheme)> = Vec::new();
    if anthropic {
        out.push((anthropic_models_url(base), AuthScheme::Anthropic));
    } else {
        out.push((format!("{base}/models"), AuthScheme::Bearer));
    }
    if let Some(origin) = origin_of(base) {
        // 回落一律 Bearer：这类兼容入口（DeepSeek 的 `/models`）就是 Bearer 用法；
        // 真 Anthropic 由上面的主候选（x-api-key + `/v1/models`）覆盖。
        // **同一个 URL 只试一次** —— 换个鉴权再试一遍等于白等一个超时（30s×N 用户等不起）。
        for path in ["/models", "/v1/models"] {
            let url = format!("{origin}{path}");
            if !out.iter().any(|(u, _)| *u == url) {
                out.push((url, AuthScheme::Bearer));
            }
        }
    }
    out
}

/// 拿厂商模型列表（连通性 + 鉴权自检，也是模型下拉框的数据源）。
///
/// 失败**不许虚构列表**：让用户以为有得选、选到一个跑不通的模型，比没有下拉框更糟。
/// 每一条候选都试过才报错，报的是**最后一条**的错（通常就是最有信息量的那条）。
pub async fn models(cfg: &LlmConfig) -> Result<Vec<ModelInfo>, String> {
    if cfg.api_key.trim().is_empty() {
        return Err("尚未配置 API Key".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;
    let mut last_err = String::from("没有可用的模型列表地址");
    for (url, auth) in model_list_candidates(cfg) {
        let req = client.get(&url).header("Content-Type", "application/json");
        let req = match auth {
            AuthScheme::Anthropic => req
                .header("x-api-key", cfg.api_key.trim())
                .header("anthropic-version", "2023-06-01"),
            AuthScheme::Bearer => {
                req.header("Authorization", format!("Bearer {}", cfg.api_key.trim()))
            }
        };
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                // 网络层失败（断网 / DNS）：换 URL 也没用，直接说清楚
                return Err(format!("网络错误: {e}"));
            }
        };
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let detail = api_error_message(&text).unwrap_or_else(|| crate::exec::clip(&text, 300));
            last_err = format!("HTTP {status} {url}: {detail}");
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("解析失败: {e}"))?;
        let list: Vec<ModelInfo> = v
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m.get("id")?.as_str()?.to_string();
                        Some(ModelInfo {
                            id,
                            name: m
                                .get("name")
                                .and_then(|n| n.as_str())
                                .map(|s| s.to_string()),
                            input_modalities: m
                                .get("input_modalities")
                                .and_then(|x| x.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            context_window: m.get("context_window").and_then(|c| c.as_u64()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if list.is_empty() {
            last_err = format!("HTTP {status} {url}: 列表为空");
            continue;
        }
        return Ok(list);
    }
    Err(last_err)
}

/// 只要 id 的老口径（配置表单用）—— 从 [`models`] 推，不另发一次请求、不另写一份解析。
pub async fn probe(cfg: &LlmConfig) -> Result<Vec<String>, String> {
    Ok(models(cfg).await?.into_iter().map(|m| m.id).collect())
}

// ============================================
// 模型名校准：**配置里那个名字，厂商未必认**
// ============================================
//
// 真实的 400（用户报的）：配置里 `model = "on"`（一个根本不是模型名的值，像是填错了字段），
// 请求原样发出去，厂商回：
//   "The supported API model names are deepseek-flash, deepseek-v4-pro, but you passed on."
// —— 用户看到的是"我传错了"，但他**问不到"那我该填什么"**（除非自己去翻文档）。
//
// 所以进入请求前先校准一次。三条口径：
//   1. 名字在厂商清单里 → 原样用（绝大多数情况，零额外动作）；
//   2. 名字为空 / 不在清单里 → 用**第一个**，并**照实记一笔**（`llm-model-corrected` 观察点
//      + 一句人话）—— 换掉可以，**静默换掉不行**：用户得知道跑的不是他配的那个；
//   3. **拿不到清单**（断网 / Key 没配 / 端点不支持 `/models`）→ **原样用**。
//      这条是底线：拉不到清单不等于配置错，绝不能因为"我问不到"就擅自换掉用户的模型。
//
// 清单按 `端点 + 协议` 缓存（同一进程里反复调用只探一次）。

/// 清单缓存 TTL。厂商上新模型不会分钟级发生，10 分钟足够；也避免每次调用都打一发 `/models`。
const MODEL_LIST_TTL: Duration = Duration::from_secs(600);

/// 清单缓存的形状：`端点|协议` → （取回时刻，模型 id 列表）。
/// 抽成别名不只是为了过 clippy —— 这种嵌套类型直接写在签名里没人读得下去。
type ModelListCache = std::sync::Mutex<std::collections::HashMap<String, (Instant, Vec<String>)>>;

static MODEL_LISTS: std::sync::OnceLock<ModelListCache> = std::sync::OnceLock::new();

fn model_lists() -> &'static ModelListCache {
    MODEL_LISTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 带缓存的厂商清单（缓存键含协议：同一个端点两条协议能看到的模型可能不同）。
async fn cached_models(cfg: &LlmConfig) -> Result<Vec<String>, String> {
    let key = format!("{}|{}", cfg.base_url.trim(), cfg.api_format.trim());
    if let Ok(g) = model_lists().lock()
        && let Some((at, list)) = g.get(&key)
        && at.elapsed() < MODEL_LIST_TTL
    {
        return Ok(list.clone());
    }
    let list = probe(cfg).await?;
    if let Ok(mut g) = model_lists().lock() {
        g.insert(key, (Instant::now(), list.clone()));
    }
    Ok(list)
}

/// 从厂商清单里挑一个**一定跑得通**的模型名（纯函数 —— 判据要能在没有网络的地方被测）。
///
/// 返回 `(要用的名字, 一句说明)`；说明只在"换过 / 为空"时非空。
pub fn pick_model(current: &str, list: &[String]) -> (String, Option<String>) {
    let cur = current.trim();
    if list.is_empty() {
        return (cur.to_string(), None);
    }
    if !cur.is_empty() && list.iter().any(|m| m == cur) {
        return (cur.to_string(), None);
    }
    let first = list[0].clone();
    let note = if cur.is_empty() {
        format!("未配置模型名 → 本次用厂商列表的第一个：{first}")
    } else {
        format!("配置里的模型名「{cur}」不在厂商列表 {list:?} 里 → 本次改用第一个：{first}")
    };
    (first, Some(note))
}

/// 失败后判断值不值得"换个模型名再试一次"：像模型名的问题 **且** 当前名字确实不在清单里。
///
/// 两道都要过：只看错误文本会误伤（有些 400 的正文里带 "model" 字样但与名字无关）；
/// 只看清单会白试（名字本来就对，换了也一样失败）。所以 `pick_model` 说"需要改"才动手。
async fn model_retry_cfg(cfg: &LlmConfig, err: &str) -> Option<LlmConfig> {
    let e = err.to_ascii_lowercase();
    let modelish =
        e.contains("model") && (e.contains("400") || e.contains("invalid") || e.contains("422"));
    if !modelish {
        return None;
    }
    let list = cached_models(cfg).await.ok()?;
    let (fixed, note) = pick_model(&cfg.model, &list);
    note.map(|_| LlmConfig {
        model: fixed,
        ..cfg.clone()
    })
}

/// 请求被厂商打回时，若错在模型名，**顺手把厂商认的名字贴进错误里**。
///
/// 为什么值得：用户看到的原始 400 只有厂商那句英文（"supported API model names are A, B,
/// but you passed C"），他拿不到"那我该填什么"。这里用**已经缓存的**清单补一句
/// （异常路径才走，成功路径一次都不探）；拿不到清单就什么都不加 —— **不许瞎猜**。
pub async fn model_hint_on_error(cfg: &LlmConfig, err: &str) -> Option<String> {
    let e = err.to_ascii_lowercase();
    if !(e.contains("model") && (e.contains("400") || e.contains("invalid") || e.contains("422"))) {
        return None;
    }
    match cached_models(cfg).await {
        Ok(list) if !list.is_empty() => Some(format!(
            "（厂商认的模型名有：{}；配置里现在是「{}」—— 打开配置表单，从下拉里选一个）",
            list.join("、"),
            cfg.model.trim()
        )),
        _ => None,
    }
}

/// 把配置里的模型名校准成厂商一定认得的名字（拿不到清单就原样返回，见上面的第 3 条）。
pub async fn resolve_model(cfg: &LlmConfig) -> (String, Option<String>) {
    match cached_models(cfg).await {
        Ok(list) => pick_model(&cfg.model, &list),
        Err(_) => (cfg.model.trim().to_string(), None),
    }
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

    /// 校准模型名的三条口径（真机上就是这条把 `model = "on"` 的 400 挡下来的）。
    /// 纯函数、不吃网络 —— 因此可以断言"拿不到清单时原样用"这种**不许擅自换模型**的底线。
    #[test]
    fn pick_model_三条口径() {
        let list: Vec<String> = ["deepseek-flash", "deepseek-v4-pro"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // ① 在清单里 → 原样、无话
        assert_eq!(
            pick_model("deepseek-v4-pro", &list),
            ("deepseek-v4-pro".into(), None)
        );
        // ② 不在清单里（用户报的真实值 "on"）→ 换第一个 + 说清
        let (m, note) = pick_model("on", &list);
        assert_eq!(m, "deepseek-flash");
        let note = note.expect("换过就必须有说明 —— 静默换掉用户的模型是不许的");
        assert!(
            note.contains("on") && note.contains("deepseek-flash"),
            "{note}"
        );
        // ②′ 名字为空 → 也用第一个，且说明里不该出现"配置里的模型名"这种话（它压根没配）
        let (m2, note2) = pick_model("   ", &list);
        assert_eq!(m2, "deepseek-flash");
        assert!(note2.unwrap().contains("未配置"));
        // ③ **拿不到清单 → 原样用**（这条是底线：问不到不等于配置错）
        assert_eq!(pick_model("on", &[]), ("on".into(), None));
        // ③′ 拿不到清单且名字为空 → 也原样（空字符串交给下游按默认处理）
        assert_eq!(pick_model("", &[]), ("".into(), None));
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

        // 联网能力**按协议分**（2026-09-25 四个模型两条路各打一遍实测）：
        // flash 在 `/responses` 上搜不了，但在 anthropic 路上能搜 —— 两列都要钉住，
        // 只钉一列就会重演"整条协议被写死成不支持"。
        assert!(!model_caps("deepseek-v4-flash").web_search);
        assert!(model_caps("deepseek-v4-flash").web_search_anthropic);
        assert!(model_caps("deepseek-v4-pro").web_search);
        assert!(model_caps("deepseek-v4-pro").web_search_anthropic);
        assert!(model_caps("deepseek-v4-flash-vision-exp").web_search_anthropic);

        // 表里没有的模型按"都没能力"处理（宁可不开，也不为不存在的能力换协议）
        assert_eq!(
            model_caps("deepseek-v5-ultra"),
            ModelCaps {
                web_search: false,
                web_search_anthropic: false,
                multimodal: false
            }
        );

        // on = 用户知情后强行开，不看能力表
        let mut forced = cfg_at("https://api.deepseek.com", "on");
        forced.model = "deepseek-v4-flash".into();
        assert!(web_search_on(&forced));

        assert!(!known_models().is_empty());
    }

    /// 模型列表的候选顺序（回落是实测逼出来的：anthropic 入口 `/v1/models` 在 DeepSeek 上 404，
    /// 而主机根 `/models` 200）—— 顺序错了等于这条回落不存在。
    #[test]
    fn model_list_falls_back_to_the_host_root() {
        let mut cfg = cfg_anthropic();
        cfg.base_url = "https://api.deepseek.com/anthropic".into();
        let c = model_list_candidates(&cfg);
        assert_eq!(c[0].0, "https://api.deepseek.com/anthropic/v1/models");
        assert!(matches!(c[0].1, AuthScheme::Anthropic));
        // ② 主机根必须出现在候选里（否则 anthropic 用户的模型下拉框会静默退回文本框）
        assert!(
            c.iter()
                .any(|(u, _)| u == "https://api.deepseek.com/models"),
            "缺少主机根回落: {:?}",
            c.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>()
        );
        // 回落要带 Bearer（DeepSeek 的 /models 认它）
        assert!(c.iter().any(
            |(u, a)| u == "https://api.deepseek.com/models" && matches!(a, AuthScheme::Bearer)
        ));

        // openai 协议：主候选就是 /models；同一个 URL 不许试两遍
        let mut o = cfg_at("https://api.deepseek.com", "auto");
        o.api_format = "openai".into();
        let co = model_list_candidates(&o);
        assert_eq!(co[0].0, "https://api.deepseek.com/models");
        let mut urls: Vec<&String> = co.iter().map(|(u, _)| u).collect();
        let total = urls.len();
        urls.sort();
        urls.dedup();
        assert_eq!(
            urls.len(),
            total,
            "候选里出现了重复 URL（白试一遍 = 白等一个超时）"
        );
        // 而且是**可数个**：URL 数量就是用户要等的最坏请求数
        assert!(
            co.len() <= 3,
            "候选太多（{} 条）：每次失败都是 30s 起",
            co.len()
        );
    }

    /// 厂商声明的输入模态是**能力的一手证据**：没声明 ≠ 支持。
    #[test]
    fn model_info_reads_modalities_and_never_assumes_the_missing_ones() {
        let flash = ModelInfo {
            id: "deepseek-flash".into(),
            name: Some("DeepSeek-V4.1-Flash".into()),
            input_modalities: vec!["text".into(), "image".into()],
            context_window: Some(1048576),
        };
        assert!(flash.accepts("image"));
        assert!(flash.accepts("IMAGE"), "大小写不敏感");
        // 音频：厂商没声明 → 一律 false（发一个不认的音频块只会换来 422）
        assert!(!flash.accepts("audio"));
        assert_eq!(flash.display_name(), "DeepSeek-V4.1-Flash");

        let bare = ModelInfo {
            id: "custom".into(),
            ..Default::default()
        };
        assert!(!bare.accepts("image") && !bare.accepts("audio"));
        assert_eq!(bare.display_name(), "custom", "厂商没给名字就用 id");
    }

    #[test]
    fn the_switch_picks_a_whole_protocol_not_just_one_extra_field() {
        let cfg = cfg_at("https://api.deepseek.com", "auto");
        let msgs = vec![
            ChatMessage::system("你是 A"),
            ChatMessage::user("最近的消息"),
        ];

        let (url, body) = responses_parts(&cfg, &msgs, true, &[]);
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
        let (url2, body2) =
            chat_parts(&cfg_at("https://api.deepseek.com", "off"), &msgs, true, &[]);
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

    /// 模型自带协议标记（DSML）必须**在抠 JSON 之前**被剥掉。
    ///
    /// 实测（2026-09-21 第 5 轮）：content 是「命令 JSON + 三行闭合标记」，标记挂在 JSON 后面
    /// ——抠出来的字符串里混进标记，整轮被判"输出无法解析"，而模型其实已经算对了那条命令。
    /// 竖线用 `\u{ff5c}`（全角，U+FF5C，DeepSeek 模板原样）转义写，源码里不留不可见字符。
    #[test]
    fn leaked_dsml_markup_is_stripped_before_extracting_json() {
        let bar = "\u{ff5c}";
        let leaked = [
            r#"                      {"cmd": "if exist a\\node_modules (echo NM_EXISTS) & netstat -ano | findstr \"8083\""}"#,
            &format!("</{0}{0}DSML{0}{0} parameter>", bar),
            &format!("</{0}{0}DSML{0}{0} invoke>", bar),
            &format!("</{0}{0}DSML{0}{0} calls>", bar),
        ]
        .join("\n");

        let got = extract_json_object(&leaked);
        let v: serde_json::Value = serde_json::from_str(&got).expect("剥掉标记后应当可解析");
        assert!(
            v["cmd"]
                .as_str()
                .unwrap()
                .contains("netstat -ano | findstr"),
            "{got}"
        );
        assert!(!got.contains("DSML"), "标记一枚都不该剩: {got}");
        assert_eq!(strip_model_markup(&leaked).1, 3, "三处闭合标记都要计数");
    }

    /// 半角竖线变体（有的网关把全角换成 `|`）与**整段**裹住动作的形状都要能剥干净。
    #[test]
    fn full_call_envelope_with_ascii_bars_is_stripped_too() {
        let raw = "<||DSML|| tool_calls><||DSML|| invoke name=\"execute\">\
                   {\"cmd\":\"ls\"}</||DSML|| invoke></||DSML|| tool_calls>";
        let (clean, n) = strip_model_markup(raw);
        assert_eq!(n, 4, "开闭标签都要算: {clean}");
        assert_eq!(clean, "{\"cmd\":\"ls\"}");
    }

    /// 反例（误报回归）：JSON 里的竖线是**单根管道**（`netstat -ano | findstr`），字符串里还有
    /// 尖括号 —— 一个字符都不许动。判定只看"尖括号里有没有成对竖线"这个形状。
    #[test]
    fn a_lone_pipe_or_angle_bracket_survives_untouched() {
        let raw = "{\"tool\":\"execute\",\"args\":{\"cmd\":\"netstat -ano | findstr \\\"8083\\\"\"},\"note\":\"a < b > c\"}";
        assert_eq!(strip_model_markup(raw).1, 0, "合法 JSON 里没有标记");
        assert_eq!(extract_json_object(raw), raw);
    }

    /// 工具声明与解析器的**契约**：工具名必须与 `parse_one` 认的能力一一对应 ——
    /// 名字对不上 = 模型一调就"未知能力"；参数键对不上 = "参数不合法"。
    /// 这条是防"改了一边忘了另一边"的唯一手段（加能力 = 改表 + 改解析器 + 改这里）。
    #[test]
    fn tools_cover_every_parser_capability() {
        let decls = tool_decls();
        let names: Vec<String> = decls
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        for want in [
            "read",
            "write",
            "execute",
            "connect",
            "plan",
            "ask_user",
            "record_findings",
            "final",
        ] {
            assert!(
                names.contains(&want.to_string()),
                "工具表缺 {want}：{names:?}"
            );
        }
        assert_eq!(names.len(), 8, "表里多了没被解析器认识的名字：{names:?}");
        // 名字清单与表**必须一一对齐**（子集声明按名字过滤：漏一个就是静默少声明一个工具）
        assert_eq!(
            names,
            TOOL_NAMES_ALL
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            "TOOL_NAMES_ALL 与 TOOL_DECLS 漂移了"
        );
        // 子集声明：复核员只给 read，就**只能**看到 read（声明面 = 权限面）
        let only_read = tool_decls_for(&["read"]);
        assert_eq!(only_read.len(), 1);
        assert_eq!(only_read[0]["function"]["name"], "read");
        // 每条 parameters 都得是合法 JSON Schema（表里存的是 JSON 源串，写错这里就炸）
        for t in &decls {
            let params = &t["function"]["parameters"];
            assert!(params.is_object(), "参数不是对象: {t}");
            assert!(params["properties"].is_object(), "缺 properties: {t}");
        }
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

    // ---- anthropic 协议（F2） ----

    fn cfg_anthropic() -> LlmConfig {
        LlmConfig {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "sk-ant-xxx".to_string(),
            model: "claude-sonnet-4".to_string(),
            api_format: "anthropic".to_string(),
            ..LlmConfig::default()
        }
    }

    #[test]
    fn anthropic_url_normalizes_common_variants() {
        assert_eq!(
            anthropic_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
        // 用户可能直接粘完整 endpoint
        assert_eq!(
            anthropic_url("https://api.anthropic.com/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        // 尾斜杠被清理
        assert_eq!(
            anthropic_url("https://example.com/anthropic/v1/"),
            "https://example.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn anthropic_parts_extracts_system_and_excludes_it_from_messages() {
        let cfg = cfg_anthropic();
        let msgs = vec![
            ChatMessage::system("你是严谨的助手"),
            ChatMessage::user("最近的消息"),
            ChatMessage::assistant("好的"),
        ];
        let (url, body) = anthropic_parts(&cfg, &msgs, true, &[]);
        assert!(url.ends_with("/v1/messages"));
        // system 提到顶层，且不在 messages 里（anthropic 的 messages 只认 user/assistant）
        assert_eq!(body["system"], "你是严谨的助手");
        assert!(body.get("messages").is_some());
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][1]["role"], "assistant");
        // anthropic 没有 response_format，加了会被 400 打回
        assert!(
            body.get("response_format").is_none(),
            "anthropic 不该带 response_format"
        );
        // 多段 system 用换行拼接
        let multi = vec![
            ChatMessage::system("第一段"),
            ChatMessage::system("第二段"),
            ChatMessage::user("问"),
        ];
        let (_, b2) = anthropic_parts(&cfg, &multi, true, &[]);
        assert_eq!(b2["system"], "第一段\n第二段");
    }

    /// 语料取自 Anthropic Messages API 文档示例结构（content 为 [{type:text,...}]）。
    #[test]
    fn extract_anthropic_yields_text_usage_and_finish_reason() {
        let raw = r#"{
          "id":"msg_01","type":"message","role":"assistant","model":"claude-sonnet-4",
          "content":[{"type":"text","text":"{\"answer\":42}"},
                     {"type":"tool_use","name":"x","input":{}}],
          "stop_reason":"end_turn",
          "usage":{"input_tokens":123,"output_tokens":45}
        }"#;
        let r = extract_anthropic(raw).unwrap();
        // 只拼接 text 片段，tool_use 不进正文
        assert_eq!(r.content, "{\"answer\":42}");
        assert_eq!(r.usage.prompt_tokens, 123);
        assert_eq!(r.usage.completion_tokens, 45);
        assert_eq!(r.usage.total_tokens, 168);
        assert_eq!(r.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(r.finish_reason.as_deref(), Some("stop"));
        // 该样本里没有 `server_tool_use` 块 → 查询词为空（有检索块的那种见
        // `anthropic_extract_reports_server_side_queries_without_confusing_tool_use`）
        assert!(r.web_queries.is_empty());
    }

    #[test]
    fn anthropic_max_tokens_maps_to_length_finish_reason() {
        let raw = r#"{"type":"message","stop_reason":"max_tokens",
            "content":[{"type":"text","text":"半截"}],
            "usage":{"input_tokens":1,"output_tokens":2}}"#;
        let r = extract_anthropic(raw).unwrap();
        assert_eq!(r.finish_reason.as_deref(), Some("length"));
        assert_eq!(r.content, "半截");
    }

    /// 联网能力**按协议那一维**判（2026-09-25 实测修正掉"anthropic 一律关"）：
    /// DeepSeek 的 `/anthropic` 认 anthropic 原生的服务端检索工具（`web_search_20250305`），
    /// 实测四个模型全都能搜 —— 而旧代码在这里**直接早返回 false**，连带把用户设的 `on`
    /// 也一起吃掉（配置说开着、请求里没有、界面不报错 = 用户报的"为什么没法用 web 搜索"）。
    #[test]
    fn web_search_is_decided_per_protocol_not_per_api_format_alone() {
        // ① auto + anthropic + DeepSeek + 表里说能搜的模型（flash 也行）→ 开
        let mut ds_anthropic = cfg_anthropic();
        ds_anthropic.base_url = "https://api.deepseek.com/anthropic".into();
        ds_anthropic.model = "deepseek-flash".into();
        ds_anthropic.web_search = "auto".into();
        assert!(
            web_search_on(&ds_anthropic),
            "anthropic 路上 flash 实测能搜，不该再一律关掉"
        );

        // ② auto + anthropic + 表里没有的模型 → 关（不虚报能力：宁可不开，也不发一个搜不了的请求）
        let mut unknown = ds_anthropic.clone();
        unknown.model = "claude-sonnet-4".into();
        assert!(!web_search_on(&unknown));

        // ③ on = 强行开，而且**不再被协议吃掉**（这条正是本次缺陷的回归判据）
        let mut forced = cfg_anthropic();
        forced.model = "claude-sonnet-4".into();
        forced.web_search = "on".into();
        assert!(
            web_search_on(&forced),
            "用户设 on 却被协议静默吞掉 = 配置在撒谎"
        );

        // ④ 非 DeepSeek 端点 + auto → 关（往不认这个工具的端点塞会被 422 打回）
        let mut foreign = cfg_anthropic();
        foreign.web_search = "auto".into();
        assert!(!web_search_on(&foreign));

        // ⑤ 同一模型两条路能力不同 —— 这一条钉的是"表必须留一维给协议"
        assert!(web_search_capable("deepseek-flash", "anthropic"));
        assert!(!web_search_capable("deepseek-flash", "openai"));
        assert!(web_search_capable("deepseek-v4-pro", "openai"));

        // ⑥ openai 格式 + auto 在 DeepSeek 端点上仍应开（回归旧行为）
        let openai = cfg_at("https://api.deepseek.com", "auto");
        assert!(web_search_on(&openai));
    }

    /// anthropic 请求体里必须出现**带版本号**的服务端检索工具，且与函数工具同框。
    /// 发错名字（`{type:web_search}`）会被 422 打回 —— 服务端报错原文就是这个字段的正确取值。
    #[test]
    fn the_anthropic_body_carries_the_versioned_server_web_search_tool() {
        let mut cfg = cfg_anthropic();
        cfg.base_url = "https://api.deepseek.com/anthropic".into();
        cfg.model = "deepseek-flash".into();
        cfg.web_search = "auto".into();
        let msgs = vec![
            ChatMessage::system("你是 A"),
            ChatMessage::user("最近的消息"),
        ];

        let (url, body) = anthropic_parts(&cfg, &msgs, false, &["read", "final"]);
        assert!(url.ends_with("/v1/messages"));
        let tools = body["tools"].as_array().expect("tools 必须是数组");
        assert_eq!(tools[0]["type"], WEB_SEARCH_ANTHROPIC_TYPE);
        assert_eq!(tools[0]["name"], "web_search");
        assert!(tools[0]["max_uses"].as_u64().unwrap_or(0) > 0);
        // 函数工具与它同框（`type` 区分），且必须是 anthropic 形态（`input_schema` 而非 `parameters`）
        assert!(
            tools
                .iter()
                .any(|t| t["name"] == "final" && t["input_schema"].is_object())
        );
        assert!(
            !tools.iter().any(|t| t["type"] == "web_search"),
            "OpenAI 形态的 {{type:web_search}} 会被 anthropic 端点 422 打回"
        );

        // 关掉时那块一个字节都不许出现（`off` 是回滚开关）
        let mut off = cfg.clone();
        off.web_search = "off".into();
        let (_, body) = anthropic_parts(&off, &msgs, false, &["read"]);
        let tools = body["tools"].as_array().expect("tools 必须是数组");
        assert!(tools.iter().all(|t| t["type"] != WEB_SEARCH_ANTHROPIC_TYPE));
        assert!(tools.iter().any(|t| t["name"] == "read"));
    }

    /// 一条**真实响应**的骨架（2026-09-25 实测 DeepSeek `/anthropic`：闪念 → 服务端检索 → 交付）。
    /// 钉两件事：① 检索查询词进 `web_queries`（两条协议同一个字段 → UI 同一处显示）；
    /// ② `server_tool_use` / `web_search_tool_result` **不许**被当成我们的工具调用，
    /// 而同一轮里的 `tool_use(final)` 必须照旧解析出来（严格模式靠它推进）。
    #[test]
    fn anthropic_extract_reports_server_side_queries_without_confusing_tool_use() {
        let raw = r#"{"id":"msg_1","type":"message","role":"assistant","model":"deepseek-flash",
            "stop_reason":"tool_use","content":[
              {"type":"thinking","thinking":"先查一下今天天气"},
              {"type":"server_tool_use","id":"call_00_x","name":"web_search",
               "input":{"query":"杭州今天天气"},"caller":{"type":"direct"}},
              {"type":"web_search_tool_result","tool_use_id":"call_00_x",
               "content":[{"type":"web_search_result","title":"杭州天气","url":"https://x"}]},
              {"type":"tool_use","id":"call_01_y","name":"final","input":{"answer":"多云"}}],
            "usage":{"input_tokens":10,"output_tokens":20}}"#;
        let r = extract_anthropic(raw).unwrap();
        assert_eq!(r.web_queries, vec!["杭州今天天气".to_string()]);
        assert_eq!(r.tool_calls.len(), 1, "服务端工具块不是我们的工具调用");
        assert_eq!(r.tool_calls[0].function.name, "final");
        assert!(r.tool_calls[0].function.arguments.contains("多云"));
        assert_eq!(r.finish_reason.as_deref(), Some("tool_calls"));
        assert!(r.content.is_empty(), "正文只取 text 块");
        assert_eq!(r.usage.prompt_tokens, 10);
    }

    // ---- 故障切换（F1） ----

    #[test]
    fn is_switchable_error_only_matches_availability_failures() {
        // 可用性故障 → 切备用
        assert!(is_switchable_error("网络错误: error trying to connect"));
        assert!(is_switchable_error(
            "DeepSeek 调用失败（已重试 3 次）: HTTP 503: 服务不可用"
        ));
        assert!(is_switchable_error("HTTP 429: 限流"));
        assert!(is_switchable_error("HTTP 500: internal"));
        assert!(is_switchable_error("请求超时"));

        // 确定性失败 → 不切（重试也没用，且备用多半同样错）
        assert!(!is_switchable_error("尚未配置 DeepSeek API Key"));
        assert!(!is_switchable_error(
            "DeepSeek 鉴权失败(HTTP 401): 无效密钥"
        ));
        assert!(!is_switchable_error(
            "DeepSeek 账户余额/额度问题(HTTP 402): 余额不足"
        ));
        assert!(!is_switchable_error(
            "DeepSeek 请求被拒绝(HTTP 400): 参数错误"
        ));
        assert!(!is_switchable_error(
            "DeepSeek 请求被拒绝(HTTP 422): 字段不合法"
        ));
        assert!(!is_switchable_error("构建 HTTP 客户端失败: ..."));
    }

    #[test]
    fn chat_with_no_fallback_is_identical_to_old_behavior() {
        // 无备用时签名多一个参数，但行为与旧版一致：缺 Key 立即报错
        let cfg = LlmConfig::default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt.block_on(chat(&cfg, None, &[ChatMessage::user("hi")], false, false));
        assert!(err.is_err());
        assert!(
            err.unwrap_err().contains("尚未配置"),
            "缺 Key 必须立即报错，不切备用"
        );
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    fn anthropic_cfg() -> LlmConfig {
        LlmConfig {
            base_url: "https://api.anthropic.com".into(),
            api_key: "k".into(),
            model: "claude-x".into(),
            max_tokens: 512,
            temperature: 0.0,
            api_format: "anthropic".into(),
            ..Default::default()
        }
    }

    fn openai_cfg() -> LlmConfig {
        LlmConfig {
            base_url: "https://api.deepseek.com".into(),
            api_key: "k".into(),
            model: "deepseek-x".into(),
            api_format: "openai".into(),
            ..Default::default()
        }
    }

    /// **bug 5 的正面判据（用户实测："发一句你好，一直无限死循环"）**：anthropic 请求
    /// **必须声明 tools**，而且是 anthropic 自己那套形状。不声明，模型只能把动作写进正文，
    /// 严格模式（`llm.tool_protocol`，默认开）下正文里的动作一律作废 —— 每轮都"没有工具调用"，
    /// 直到烧完预算。这就是那条 bug 的**真病因**。
    #[test]
    fn anthropic_request_declares_tools_in_anthropic_shape() {
        let (url, body) = anthropic_parts(
            &anthropic_cfg(),
            &[ChatMessage::user("你好")],
            false,
            &["read", "final"],
        );
        assert!(url.ends_with("/v1/messages"), "{url}");

        let tools = body["tools"]
            .as_array()
            .expect("anthropic 请求必须带 tools");
        assert_eq!(tools.len(), 2, "{tools:?}");
        let read = tools
            .iter()
            .find(|x| x["name"] == "read")
            .expect("read 不在声明里");
        // anthropic 叫 `input_schema`，不是 OpenAI 的 `parameters`
        assert_eq!(read["input_schema"]["required"][0], "path", "{read}");
        assert!(read["description"].as_str().is_some_and(|d| !d.is_empty()));
        // 也不该混进 OpenAI 的 `{type:"function", function:{…}}` 外壳（发过去就是 400）
        assert!(read.get("function").is_none(), "混进了 OpenAI 形态: {read}");
    }

    /// 不声明工具时**别发空 tools 数组**（有的网关对 `tools: []` 直接 400）。
    #[test]
    fn anthropic_request_omits_tools_when_none_declared() {
        let (_, body) = anthropic_parts(&anthropic_cfg(), &[ChatMessage::user("hi")], false, &[]);
        assert!(body.get("tools").is_none(), "{body}");
    }

    /// 响应侧：`content` 里的 `tool_use` 块必须变成可执行的调用。
    #[test]
    fn anthropic_tool_use_block_becomes_a_usable_call() {
        let raw = r#"{
            "model": "claude-x",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "我先读一下"},
                {"type": "tool_use", "id": "toolu_01", "name": "read",
                 "input": {"path": "a.txt", "limit": 20}}
            ],
            "usage": {"input_tokens": 7, "output_tokens": 3}
        }"#;
        let reply = extract_anthropic(raw).expect("解析 anthropic 响应");
        assert_eq!(reply.content, "我先读一下");
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(reply.tool_calls.len(), 1, "{:?}", reply.tool_calls);
        let c = &reply.tool_calls[0];
        assert_eq!(c.id, "toolu_01");
        assert_eq!(c.function.name, "read");
        // 参数在响应里是**对象**，我们的契约是 **JSON 字符串** —— 解出来必须还是那份参数
        let args: serde_json::Value =
            serde_json::from_str(&c.function.arguments).expect("arguments 应是 JSON 字符串");
        assert_eq!(args["path"], "a.txt");
        assert_eq!(args["limit"], 20);
        assert_eq!(reply.usage.prompt_tokens, 7);
    }

    /// 回灌侧：带调用的助手消息、工具结果，都得用 anthropic 的原生块
    /// （anthropic 没有 `role = "tool"`，结果要包成 user 消息里的 `tool_result`）。
    #[test]
    fn anthropic_replay_uses_native_tool_blocks() {
        let mut assistant = ChatMessage::assistant("读一下");
        assistant.tool_calls = Some(vec![ToolCall {
            id: "toolu_01".into(),
            kind: "function".into(),
            function: ToolCallFn {
                name: "read".into(),
                arguments: r#"{"path":"a.txt"}"#.into(),
            },
        }]);
        let msgs = vec![
            ChatMessage::user("读 a.txt"),
            assistant,
            ChatMessage::tool_result("toolu_01", "文件内容"),
        ];
        let (_, body) = anthropic_parts(&anthropic_cfg(), &msgs, false, &["read"]);
        let out = body["messages"].as_array().unwrap();
        let a = out
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant 消息");
        let blocks = a["content"].as_array().expect("assistant 应是块数组");
        assert!(
            blocks
                .iter()
                .any(|b| b["type"] == "text" && b["text"] == "读一下"),
            "{a}"
        );
        let tu = blocks
            .iter()
            .find(|b| b["type"] == "tool_use")
            .expect("缺 tool_use 块");
        assert_eq!(tu["id"], "toolu_01");
        assert_eq!(tu["name"], "read");
        assert_eq!(tu["input"]["path"], "a.txt", "input 必须是**对象**");

        let last = out.last().unwrap();
        assert_eq!(
            last["role"], "user",
            "tool_result 必须挂在 user 消息上: {last}"
        );
        let tr = last["content"].as_array().unwrap();
        assert_eq!(tr[0]["type"], "tool_result");
        assert_eq!(tr[0]["tool_use_id"], "toolu_01");
        assert_eq!(tr[0]["content"], "文件内容");
    }

    /// `/responses`（web_search 那条路）：联网检索与函数工具**共处一个数组**。
    /// 以前只有 `web_search` —— 那条路上模型一个函数工具都拿不到，是同一个病的另一处。
    #[test]
    fn responses_request_declares_function_tools_beside_web_search() {
        let (url, body) = responses_parts(
            &openai_cfg(),
            &[ChatMessage::user("搜一下再改代码")],
            false,
            &["read", "execute"],
        );
        assert!(url.ends_with("/responses"), "{url}");
        let tools = body["tools"].as_array().expect("tools 数组");
        assert!(
            tools.iter().any(|x| x["type"] == "web_search"),
            "联网检索丢了: {tools:?}"
        );
        let read = tools
            .iter()
            .find(|x| x["name"] == "read")
            .expect("read 没被声明");
        assert_eq!(read["type"], "function");
        // 与 /chat/completions 的差别：**扁平**，不裹 `function` 那一层
        assert_eq!(read["parameters"]["required"][0], "path", "{read}");
        assert!(
            read.get("function").is_none(),
            "混进了 chat/completions 形态: {read}"
        );
    }

    /// `/responses` 的响应侧：`output` 里的 `function_call` 项 → 可用调用。
    #[test]
    fn responses_function_call_items_become_usable_calls() {
        let raw = r#"{
            "model": "deepseek-x",
            "status": "completed",
            "output": [
                {"type": "web_search_call", "action": {"queries": ["ws_call_id=1", "ruyix 便携根"]}},
                {"type": "message", "content": [{"type": "output_text", "text": "查到了"}]},
                {"type": "function_call", "call_id": "call_7", "name": "read",
                 "arguments": "{\"path\":\"a.txt\"}"}
            ],
            "usage": {"input_tokens": 5, "output_tokens": 2}
        }"#;
        let reply = extract_responses(raw).expect("解析 /responses 响应");
        assert_eq!(
            reply.web_queries,
            vec!["ruyix 便携根".to_string()],
            "内部标记不该当成查询词"
        );
        assert_eq!(reply.content, "查到了");
        assert_eq!(reply.tool_calls.len(), 1, "{:?}", reply.tool_calls);
        assert_eq!(reply.tool_calls[0].id, "call_7");
        assert_eq!(reply.tool_calls[0].function.name, "read");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&reply.tool_calls[0].function.arguments)
                .unwrap()["path"],
            "a.txt"
        );
    }
}
