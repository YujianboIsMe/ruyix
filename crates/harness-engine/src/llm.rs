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
    /// 本引擎用一种**混合**形态（详见 `doc/需求-工具协议改造-v0.0.6.md`）：请求里声明 `tools`
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
    /// 走老形状（见 `doc/问题-DSML标记泄露.md` 的 4 臂对照）。
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
/// 本引擎的协议是"动作写在 content 的 JSON 里"，**从不发 `tools`**。但被原生工具调用语法
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
    // anthropic 协议不支持服务端联网检索（语义不成立），一律关。
    if cfg.api_format.trim().eq_ignore_ascii_case("anthropic") {
        return false;
    }
    match cfg.web_search.trim().to_ascii_lowercase().as_str() {
        "on" => true,
        "auto" => is_deepseek_endpoint(cfg) && model_caps(&cfg.model).web_search,
        _ => false,
    }
}

/// 全部工具名（**必须与 [`TOOL_DECLS`] 一行不改地对齐** —— 契约单测同时断言两边）。
///
/// 拆出它是因为 `tools` 现在按**调用方需要的子集**声明：主循环给全套，复核员只给 `read`
/// （它只能读，给了 `write` 等于把只读约束交给模型自觉）。
pub const TOOL_NAMES_ALL: &[&str] = &[
    "read", "write", "execute", "connect", "plan", "ask_user", "final",
];

/// 声明给模型的工具（表驱动）。
///
/// ## 为什么非要有它（这次的病根）
///
/// 引擎原先从不声明 `tools`，动作全靠"content 里的 JSON"这条约定。但 DeepSeek 这类模型被
/// **原生工具调用语法**训练过：它会把自己的调用写成 DSML 标记吐进 content，而服务端没收到
/// `tools` 就**不会**把它解析进 `tool_calls` —— 于是"模型明明算对了的命令"变成一整轮作废
/// （实测真跑 **5/16 轮**，见 `doc/问题-DSML标记泄露.md`）。4 臂 × 8 轮对照里，声明 `tools`
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
// 顶层字段（不在 messages 里）；没有 `response_format`（JSON 靠提示词保证）；不支持服务端
// 联网检索（见 `web_search_on` 对 anthropic 直接返回 false）。

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

fn anthropic_parts(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    _json_mode: bool,
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
                msgs.push(serde_json::json!({ "role": "assistant", "content": m.content }))
            }
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
    if !system_text.is_empty() {
        body["system"] = serde_json::json!(system_text);
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
    let finish_reason = match parsed.stop_reason.as_deref() {
        Some("max_tokens") => Some("length".to_string()),
        Some("end_turn") | Some("stop_sequence") => Some("stop".to_string()),
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
        web_queries: Vec::new(),
        // anthropic 的 `tool_use` 块本次不映射：这条路保持老协议（content 里的 JSON），
        // 与改造前逐字一致。
        tool_calls: Vec::new(),
    })
}

/// 按 `api_format` 选择整套协议：anthropic → `/v1/messages`；否则按 `web_search_on`
/// 在 `/responses` 与 `/chat/completions` 间分叉。anthropic 必须最先判（它会跳过联网）。
///
/// `tool_names` 只对 `/chat/completions` 生效：`/responses` 与 anthropic 两条路的工具形态是另一套
/// （`function_call` 项 / `tool_use` 块），本次改造不碰它们（各自 `extract_*` 里的注释说明了
/// 为什么），所以这里不往下传。
fn request_plan(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    json_mode: bool,
    tool_names: &[&str],
) -> Protocol {
    if cfg.api_format.trim().eq_ignore_ascii_case("anthropic") {
        let (u, b) = anthropic_parts(cfg, messages, json_mode);
        (u, b, extract_anthropic, AuthScheme::Anthropic)
    } else if web_search_on(cfg) {
        let (u, b) = responses_parts(cfg, messages, json_mode);
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
        tool_calls: Vec::new(),
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

    let wrapped = format!("DeepSeek 调用失败（已重试 {max_attempts} 次）: {last_err}");
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
/// 才切备用（详见 `doc/需求-LLM网关与多协议-v0.5.md`）。
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

/// 连通性 + 鉴权自检：拿模型列表，比"跑一个真实任务才发现 key 错"友好得多。
///
/// 协议感知：anthropic 走 `/v1/models` + `x-api-key` + `anthropic-version`；否则
/// `/models` + Bearer。响应体都是 `data:[{id}]`，解析共用。
pub async fn probe(cfg: &LlmConfig) -> Result<Vec<String>, String> {
    if cfg.api_key.trim().is_empty() {
        return Err("尚未配置 API Key".into());
    }
    let (url, auth) = if cfg.api_format.trim().eq_ignore_ascii_case("anthropic") {
        (anthropic_models_url(&cfg.base_url), AuthScheme::Anthropic)
    } else {
        (
            format!("{}/models", cfg.base_url.trim_end_matches('/')),
            AuthScheme::Bearer,
        )
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;
    let req = client.get(&url).header("Content-Type", "application/json");
    let req = match auth {
        AuthScheme::Anthropic => req
            .header("x-api-key", cfg.api_key.trim())
            .header("anthropic-version", "2023-06-01"),
        AuthScheme::Bearer => req.header("Authorization", format!("Bearer {}", cfg.api_key.trim())),
    };
    let resp = req.send().await.map_err(|e| format!("网络错误: {e}"))?;
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
            "read", "write", "execute", "connect", "plan", "ask_user", "final",
        ] {
            assert!(
                names.contains(&want.to_string()),
                "工具表缺 {want}：{names:?}"
            );
        }
        assert_eq!(names.len(), 7, "表里多了没被解析器认识的名字：{names:?}");
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
        let (url, body) = anthropic_parts(&cfg, &msgs, true);
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
        let (_, b2) = anthropic_parts(&cfg, &multi, true);
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
        assert!(r.web_queries.is_empty(), "anthropic 协议无服务端联网检索");
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

    #[test]
    fn web_search_is_off_for_anthropic_even_when_auto() {
        let mut cfg = cfg_anthropic();
        cfg.web_search = "auto".into();
        assert!(!web_search_on(&cfg), "anthropic 协议不支持服务端联网检索");
        // openai 格式 + auto 在 DeepSeek 端点上仍应开（回归旧行为）
        let openai = cfg_at("https://api.deepseek.com", "auto");
        assert!(web_search_on(&openai));
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
