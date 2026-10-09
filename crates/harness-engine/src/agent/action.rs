//! 动作解析：**模型输出 → 结构化动作**（以及波次调度）。
//!
//! 这是引擎与模型之间唯一的协议层：八个工具名 → `Action`，参数形状校验（缺字段 / 类型不对都
//! 在这里被翻译成**可读的纠偏**回灌，而不是让循环崩掉），批量的冲突分析与波次切分（同波次并发，
//! 冲突的串行）。解析不出来的那条路径同样在这里：截断（`finish_reason=length`）与格式错
//! 纠偏方向相反，`parse_failure_feedback` 分开说。
//!
//! 从 `agent.rs` 拆出的（2026-09-28，这一段原本 1824 行、是全文件最大的一坨）。

use super::*;

// （原 `agent.rs` 的 banner：动作解析（模型输出 → 结构化动作））

// ============================================
// 动作解析（模型输出 → 结构化动作）
// ============================================

/// read 的参数：路径 + 可选行窗口。
///
/// 不传 offset/limit = 老行为（整份裁剪读，`READ_CLIP` 掐头去尾）。传了 = 只取那一段 ——
/// 这是"缺口可以再接一次"的入口：改一个 3000 行文件中间那 40 行，不必为读它付整份的 token，
/// 也不必为改它把整份吐回来（后者见 [`WriteBody::Edits`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReadSpec {
    pub path: String,
    /// 起始行（1-based，含）
    pub offset: Option<usize>,
    /// 最多几行
    pub limit: Option<usize>,
}

impl ReadSpec {
    /// 整份读（老形状）。生产路径由 [`parse_read`] 直接构造，这条给测试与调用方省噪声。
    #[cfg(test)]
    pub(crate) fn whole(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            offset: None,
            limit: None,
        }
    }

    /// 是不是"窗口读"。全空就是整份读 —— 老形状一字不改地走老路径。
    pub(crate) fn is_window(&self) -> bool {
        self.offset.is_some() || self.limit.is_some()
    }

    /// 台账 / 日志里怎么称呼这次读（窗口读必须写出窗口，否则日志看不出"那次只读了 40 行"）
    pub(crate) fn brief(&self) -> String {
        match (self.offset, self.limit) {
            (None, None) => self.path.clone(),
            (o, l) => format!(
                "{}（{}-{}）",
                self.path,
                o.unwrap_or(1),
                l.map(|l| format!("+{l}"))
                    .unwrap_or_else(|| "末".to_string())
            ),
        }
    }
}

/// 分层检索的**作用域**（v1.5「五服」里的四个可查层；`kb` 是第二刀，见
/// `doc/v1.5/需求-分层搜索-五服-v1.5.md` §5）。
///
/// **这不是四个新工具**：模型侧只有 `read` 一个名字，`scope` 只是它的参数 ——
/// 与 `execute` 的 `background`/`op` 同款（加维度，不加原语）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchScope {
    /// 会话内：本 run **已折掉/复用过的**正文（capsule 侧存）
    Session,
    /// 项目记忆（记忆库里本项目 scope）
    ProjectMem,
    /// 全局记忆（跨项目事实；用户拍板**默认开**，命中必须标来源）
    GlobalMem,
    /// 项目文件（原生实现，**不经过 shell**）
    Files,
}

impl SearchScope {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "session" => Some(Self::Session),
            "project_mem" | "project" => Some(Self::ProjectMem),
            "global_mem" | "global" => Some(Self::GlobalMem),
            "files" => Some(Self::Files),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::ProjectMem => "project_mem",
            Self::GlobalMem => "global_mem",
            Self::Files => "files",
        }
    }
}

/// `read` 的第二种形状：分层检索（v1.5）。**不是第五个原语**（需求 §3）。
#[derive(Clone, Debug)]
pub(crate) struct SearchSpec {
    pub scope: SearchScope,
    /// 搜什么（**字面串，大小写不敏感**；本刀不做正则）
    pub q: String,
    /// 只对 `files` 有意义：限定子树（走 `generate::safe_rel_path` 的 jail）
    pub path: Option<String>,
    /// 要几条（缺省 50，上限同）
    pub max_hits: Option<usize>,
}

impl SearchSpec {
    /// 日志 / trace / 账本里怎么称呼这次检索（**层名必须写出来**：不然"搜过"这件事
    /// 在留痕里看不出搜的是哪一层，而层与层的权威级别不同）
    pub(crate) fn brief(&self) -> String {
        let mut s = format!("{} {:?}", self.scope.as_str(), self.q);
        if let Some(p) = &self.path {
            s.push_str(&format!(" @{p}"));
        }
        s
    }
}

/// write 的一条**锚点改动**：把 `find` 换成 `replace`。
///
/// `find` 必须与原文**逐字一致**（含缩进）且**恰好出现一次** —— 匹配规则不在这里重写，
/// 与修复流水线共用 [`repair::replace_unique`]（它已带行尾归一化：CRLF 文件匹配 LF 的 find，
/// 落盘再还原 CRLF。真机踩过 `core.autocrlf` 让 LF 片段一条都匹配不上）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentEdit {
    pub find: String,
    /// 换成什么。空串 = 删掉这一段（合法，且与"没变化"不同）。
    #[serde(default)]
    pub replace: String,
}

/// write 的两副面孔。**同一个能力，两种参数形状** —— 不新增第五种原子能力：
/// "写文件"的效果没变，变的只是"要传多少东西"。
#[derive(Clone, Debug)]
pub(crate) enum WriteBody {
    /// 整份内容。新建文件、或改动大到锚点划不出来时用（老形态，逐字保留）。
    Content(String),
    /// 锚点替换：只传改动，不重发全文。改既有文件的**首选**。
    Edits(Vec<AgentEdit>),
}

#[derive(Clone, Debug)]
pub(crate) struct WriteSpec {
    pub path: String,
    pub body: WriteBody,
}

impl WriteSpec {
    /// 整份内容形态。生产路径由 [`parse_write`] 直接构造，这条给测试与调用方省噪声。
    #[cfg(test)]
    pub(crate) fn content(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            body: WriteBody::Content(content.into()),
        }
    }

    /// 日志 / 调用摘要里的一句话
    pub(crate) fn brief(&self) -> String {
        match &self.body {
            WriteBody::Content(c) => format!("write {}（{} 字节）", self.path, c.len()),
            WriteBody::Edits(e) => format!("write {}（{} 处锚点替换）", self.path, e.len()),
        }
    }
}

/// `record_findings` 的一条入参：**结论 + 证据指针**。
///
/// `evidence` 必填（空则在引擎侧被拒）：findings 的价值全在"可核对"，
/// 收下一句没有证据的断言，只是把幻觉抬进提示词。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FindingSpec {
    pub claim: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub note: String,
    /// 要取代的旧条目 id（`F3`）—— 从提示词块或工具结果里拿
    #[serde(default)]
    pub supersedes: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Final(String),
    Plan(Vec<PlanStep>),
    Read(ReadSpec),
    /// **`read` 的第二种形状**（v1.5 分层检索）—— 不是第五种能力：它不写盘、不改状态，
    /// 结果由资源当前状态决定，所以它是**读**。单独一个变体是为了让它在**账本里是一等公民**
    /// （`Tool::Search`：同 scope 同词同版本 ⇒ 不重复执行）—— 否则"藏在 execute 里"那条老路
    /// 依然不进账本（管道一加就判不纯），而重复检索正是 v1.2 账本要治的病。
    Search(SearchSpec),
    Write(WriteSpec),
    Execute(String, Option<u64>),
    /// 后台启动：`execute` 的第三种生命周期（有界 / 无界托管 / 句柄操作）。
    /// 模型侧仍是同一个 `execute` 工具，只是多了 `background` 这一维。
    ExecBg(crate::proc::StartSpec),
    /// 托管进程的句柄操作（status / log / stop）
    Proc(ProcOp, String),
    Connect(ConnectAction),
    /// **第五个动作**：向委托人提问。
    ///
    /// 需求歧义（"做一个远程登录功能" —— 登哪台机器？托管的服务器还是用户另一台电脑？）
    /// 的答案**不在环境里**，只存在于委托人脑子里：read 读磁盘、execute 跑命令、connect 连机器，
    /// 三者都只会从"环境"取答案，任何组合都取不到它。它同时是**控制动作**（停下来让出方向盘）
    /// 与**效果**（取信息），也是唯一"另一端是人"的动作 —— 答案可能永远不来，且能改变任务本身。
    Ask(AskSpec),
    /// **进展记忆**（v1.1）：模型把自己确认的事实记下来 —— 引擎永不折叠它。
    ///
    /// 它不是第五种能力（对外部世界没有副作用），也不是控制动作（不停下来）：
    /// 它是**记忆**。允许进批（`plan`/`final` 那类控制动作不许），
    /// 因为搭车记录不该多花一轮。
    Findings(Vec<FindingSpec>),
}

/// 一次提问的规格（模型侧 `ask_user` 的 args）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AskSpec {
    /// 问什么。指代必须明确（"你想要什么"这种废问题会被用户当噪声）
    pub question: String,
    /// **这个答案会决定接下来的什么动作** —— UI 原样展示给用户。
    /// 审计与反社会工程学的硬要求：模型不许把一个危险动作包装成一个无害的问题。
    pub why: String,
    /// 2~5 个候选；空 = 自由文本
    #[serde(default)]
    pub options: Vec<String>,
    /// 用户不答 / 超时时采用的默认（**没有默认 = fail-closed 拒绝**依赖它的动作）
    #[serde(default)]
    pub default_index: Option<usize>,
    /// 等多久算"没人回答"（0 = 无限等）。**由引擎按配置填，模型给的会被忽略** ——
    /// 能不能决定超时，是委托人的权力，不是模型的。
    #[serde(default)]
    pub timeout_secs: u64,
}

/// 用户（委托人）的回答
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AskAnswer {
    pub text: String,
    #[serde(default)]
    pub option_index: Option<usize>,
    #[serde(default)]
    pub ts: String,
}

/// 为什么没拿到答案。**每一种都必须是 fail-closed**：绝不假设同意。
#[derive(Clone, Debug)]
pub enum AskErr {
    /// 宿主没接提问通道（headless / eval / 冒烟）
    NoAsker,
    Timeout,
    Canceled,
    Failed(String),
}

impl AskErr {
    pub fn reason(&self) -> String {
        match self {
            AskErr::NoAsker => "当前环境没有提问通道（headless 或未接宿主）".into(),
            AskErr::Timeout => "等超时了，用户没有回答".into(),
            AskErr::Canceled => "这次 run 被取消了".into(),
            AskErr::Failed(e) => format!("提问失败：{e}"),
        }
    }
    /// 状态标签（进会话存档，UI 据此显示"未回答 / 超时"）
    pub fn state(&self) -> &'static str {
        match self {
            AskErr::NoAsker => "no_asker",
            AskErr::Timeout => "timeout",
            AskErr::Canceled => "canceled",
            AskErr::Failed(_) => "failed",
        }
    }
}

pub type AskFut<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<AskAnswer, AskErr>> + Send + 'a>>;

/// 提问通道的宿主契约 —— 与 [`Connector`] 同款：引擎只声明能力，宿主兑现。
///
/// **等待与超时由实现负责**（引擎没有计时器），而拿不到答案必须返回 `Err`：
/// 拿一个"猜的答案"回来是这条契约唯一不可接受的事。
pub trait Asker: Send + Sync {
    fn ask<'a>(&'a self, id: &'a str, spec: &'a AskSpec) -> AskFut<'a>;
}

/// 空实现：headless / eval / 冒烟用。一律 fail-closed（与 `NoConnector` 同款纪律）。
pub struct NoAsker;

impl Asker for NoAsker {
    fn ask<'a>(&'a self, _id: &'a str, _spec: &'a AskSpec) -> AskFut<'a> {
        Box::pin(async { Err(AskErr::NoAsker) })
    }
}

/// 一次提问的留痕（进 `AgentOutcome.asks` → 宿主写进会话存档 → 重开会话仍看得见）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AskRecord {
    pub id: String,
    pub question: String,
    pub why: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub answer: Option<String>,
    /// answered | timeout | no_asker | canceled | failed
    pub state: String,
    #[serde(default)]
    pub ts: String,
}

/// `execute` 对托管进程的句柄操作
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcOp {
    Status,
    Log,
    Stop,
}

pub(crate) fn proc_op_name(op: ProcOp) -> &'static str {
    match op {
        ProcOp::Status => "status",
        ProcOp::Log => "log",
        ProcOp::Stop => "stop",
    }
}

/// connect 的三形态：看清单 / 调 MCP 工具 / 委托远端 Agent
#[derive(Clone, Debug)]
pub(crate) enum ConnectAction {
    List,
    Call {
        server: String,
        tool: String,
        arguments: serde_json::Value,
    },
    Send {
        agent: String,
        text: String,
    },
}

pub(crate) fn get_str(v: &serde_json::Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("args 缺少字符串字段 {key}"))
}

/// 单动作入口：**不接受**批（老语义）。生产路径走 [`parse_actions`]，
/// 这条留给测试断言"批关掉时该拒就拒"。（`Action` 只在本模块用，别把内部枚举泄成 pub）
#[cfg(test)]
pub(crate) fn parse_action(raw: &str) -> Result<Action, String> {
    let (v, markup) = json_of(raw)?;
    parse_one(&v).map_err(|e| markup_note(e, markup))
}

/// 单动作与批动作的**公共前段**：剥掉模型自带的协议标记 → 抠 JSON → 解成 `Value`。
///
/// 剥标记要对着 `raw` 做、也只做在这里：`extract_json_object` 是全仓唯一的抠 JSON 收口点，
/// 模型自带的那套标记（[`llm::strip_model_markup`]）在进解析器之前就剪掉，
/// 于是 plan / generate / repair / reflect / eval 那些调用方一起受益，不用各自记得过滤。
pub(crate) fn json_of(raw: &str) -> Result<(serde_json::Value, usize), String> {
    let (clean, markup) = llm::strip_model_markup(raw);
    let json = llm::extract_json_object(&clean);
    serde_json::from_str::<serde_json::Value>(&json)
        .map(|v| (v, markup))
        .map_err(|e| {
            markup_note(
                format!("输出不是合法 JSON: {e}；片段: {}", clip(&json, 200)),
                markup,
            )
        })
}

/// 连续解析失败到上限时的收尾诊断（**纯函数，可单测**）。
///
/// 它要回答的不是"错了"，而是"**接下来怎么办**"——因为这个失败几乎总不是模型"笨"，
/// 而是"这条通道在你的模型上走不通"。三件事必须给全：
/// ① 现象（连续几轮、最近一轮的原文长什么样，截一小段，够看出是 JSON 还是标记）；
/// ② 最可能的原因（网关把 `tools` 丢了 / 模型不支持 function calling / 模型坚持用它自己那套标记）；
/// ③ 两条出路（换模型端点；或一行回滚到老协议 `tool_protocol=false`）。
///
/// 不给③的话，用户只能看着日志猜 —— 而这条回滚开关是现成的（`ruyix.code.ai.tool_protocol`）。
pub(crate) fn unparseable_diagnosis(streak: usize, last_raw: &str) -> String {
    let (clean, markup) = crate::llm::strip_model_markup(last_raw);
    let snippet = clip(clean.trim(), 240);
    let snippet = if snippet.trim().is_empty() {
        "（这一轮 content 是空的）".to_string()
    } else {
        snippet
    };
    let markup_note = if markup > 0 {
        format!(
            "\n其中检测到 {markup} 处模型自带的工具调用标记（已剥掉），说明它**算出了动作**，\
                 只是用了自己那套协议 —— 而这一轮的请求里服务端没能把它解析成 `tool_calls`。"
        )
    } else {
        String::new()
    };
    format!(
        "连续 {streak} 轮没有拿到工具调用，已停止本轮（不再空烧预算）。\n\n\
         最近一轮的 content 片段：{snippet}{markup_note}\n\n\
         最可能的原因：这条请求在你的模型 / 端点上**没走工具调用**（网关把 `tools` 丢了、\
         模型不支持 function calling、或它坚持用自己那套标记）。\n\n\
         两条出路：\n\
         ① 换一个支持工具调用（tools / function calling）的模型或端点；\n\
         ② 一行回滚到老协议：把 `ruyix.code.ai.tool_protocol` 设成 false —— 动作改走 content 里的 \
         JSON（代价是模型自带标记更容易泄露，所以只作为兜底）。"
    )
}

/// 解析错误 + "你发的是自带协议标记"这句。
///
/// **只剥不说是治不好的**：标记被静默剪掉之后，模型看到的只是"无法解析"，而它自己认为
/// 明明发出来了（在它那套协议里是合法的），于是换个说法重发同一坨 —— 实测就是这么烧掉一轮的。
/// 把"剥了几处"写进错误文本，它才知道要换形状。
pub(crate) fn markup_note(err: String, markup: usize) -> String {
    if markup == 0 {
        return err;
    }
    format!(
        "{err}（本次输出里检测到 {markup} 处模型自带的工具调用标记，已剥掉 —— 动作请**直接用工具调用**\
         表达（本轮已声明 read / write / execute / connect / plan / ask_user / final），不要把动作、\
         更不要把这类标记写进 content）"
    )
}

/// `tool_calls` → 动作清单（**标准工具协议**，v0.0.6 起的主路）。
///
/// 一条 `tool_call` = 一个动作；一轮多条 = 我们的**批量调用**（顺序即声明顺序，与老协议
/// `{"actions":[…]}` 同义，后面的波次并发/冲突保序照旧生效）。
///
/// 三条纪律：
///
/// 1. **参数解析失败只作废那一条**，不整批抛出 —— 模型算对了另外三条时不该一起烧掉；
/// 2. **校验复用 `parse_one`**：把 `{tool: <名字>, args: <参数>}` 交给它，于是"未知能力 /
///    参数不合法"两种判定父子两条协议只有一份实现（工具名与能力名一一对应由
///    `llm::tool_decls` 的契约保证）；
/// 3. **控制动作不许进批**（`plan` / `ask_user` / `final`）：与老协议同一条纪律，理由也一样 ——
///    同一批里两件事谁先谁后没有合理解释，替模型猜一个顺序不如当面拒。
pub(crate) fn parse_tool_calls(
    calls: &[llm::ToolCall],
    max: usize,
    allow_batch: bool,
) -> Result<Vec<Action>, String> {
    if calls.len() > 1 && !allow_batch {
        return Err(format!(
            "本轮不允许批量调用：一次发了 {} 个工具调用。请每轮只发一个",
            calls.len()
        ));
    }
    if calls.len() > max {
        return Err(format!(
            "一批最多 {max} 个调用，这次给了 {} 个 —— 请拆成多轮，或按依赖关系分成几批",
            calls.len()
        ));
    }
    let mut out = Vec::with_capacity(calls.len());
    for (i, c) in calls.iter().enumerate() {
        let name = c.function.name.trim().to_ascii_lowercase();
        // 工具名对不上是模型自己的锅，报清楚它发了什么（不带"未知能力"那套老词，
        // 免得它以为要改用 content 里的 JSON）
        let args: serde_json::Value = if c.function.arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&c.function.arguments).map_err(|e| {
                format!(
                    "第 {} 个工具调用 `{name}` 的参数不是合法 JSON: {e}；片段: {}",
                    i + 1,
                    clip(&c.function.arguments, 200)
                )
            })?
        };
        // 交付动作走 `parse_one` 的统一入口：`final` 现在是一条正式的工具名，
        // 键名 `answer` / `final` 与裸串都认（见 [`final_answer`]）。
        let v = serde_json::json!({ "tool": name, "args": args });
        let a = parse_one(&v).map_err(|e| format!("第 {} 个工具调用：{e}", i + 1))?;
        match &a {
            Action::Plan(_) if calls.len() > 1 => {
                return Err("plan 不能和别的调用放进同一轮：清单要单独发一轮".into());
            }
            Action::Ask(_) if calls.len() > 1 => {
                return Err("ask_user 不能和别的调用放进同一轮：提问要单独一轮".into());
            }
            Action::Final(_) if calls.len() > 1 => {
                return Err("final 不能和别的调用放进同一轮：完成时单独发一轮".into());
            }
            _ => out.push(a),
        }
    }
    Ok(out)
}

/// 工具轮**回显给模型**的那条 assistant 消息（**流水文字，不是 JSON 信封**）。
///
/// 工具轮的 `content` 天生是空的（动作全在 `tool_calls` 里）。若原样回显空消息，模型下一轮
/// 就看不见自己刚发过什么 —— 所以要把它自己发的那串调用记回来（`arguments` 一字不改，
/// 截断/畸形也照原样，让模型自己看出问题）。
///
/// ⚠ **这里绝对不能写成 JSON 信封**（曾经的写法是 `{"tool_calls":[{"name":…,"arguments":…}]}`）。
/// 那是一个"看起来可以发送"的形状，而模型真的会照抄：
/// - 真跑 `agent-…` 那次 73 轮里 **2 轮**被它带偏（`finish_reason=stop`、`tool_calls=0`、
///   content 就是那个信封）；
/// - 2026-09-24 新会话第 6 轮**又中一次**（用户原话"我看模型返回的没有问题啊"——
///   动作意图和参数都对，只是发在了 content 通道）。
///
/// 病因在回显自己：我们在历史里反复摆一个"能发的 JSON"，模型就学着用它发。所以回显只给
/// **流水文字**（带 `〔已发出调用〕` 前缀 + `name(键=值)`），一眼看得出是"引擎记的账"：
/// 抄了也没用 —— 那是散文，同样不执行，但不会再被误当成协议的一部分。
pub(crate) fn tool_calls_echo(calls: &[llm::ToolCall]) -> String {
    let mut s = String::from("〔已发出调用〕");
    for (i, c) in calls.iter().enumerate() {
        if i > 0 {
            s.push('；');
        }
        let raw = c.function.arguments.trim();
        // 参数摊平成 `键=值`（与系统提示词里教工具时的写法同族），
        // 而不是把 JSON 原样贴回来 —— 少一个可被复制的信封。
        let shown = match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(serde_json::Value::Object(o)) => o
                .iter()
                .map(|(k, v)| match v {
                    serde_json::Value::String(t) => format!("{k}=\"{}\"", clip(t, 120)),
                    other => format!("{k}={}", clip(&other.to_string(), 120)),
                })
                .collect::<Vec<_>>()
                .join(", "),
            _ => clip(raw, 160),
        };
        s.push_str(&format!("{}({shown})", c.function.name));
    }
    s
}

/// 一轮模型输出 → 动作清单（1 个或多个）。
///
/// 多个动作 = **批量调用**：模型把互不依赖的调用一次发出来，引擎并发执行只读的那些、
/// 按声明顺序执行有副作用的那些，再把结果按同一顺序一起回灌（见 [`group_batch`]）。
///
/// `max` 是单批上限：超了**不静默截断**（截断就是丢调用，模型还以为发出去了），
/// 而是把上限报回去让它拆批。`allow_batch = false` 时批协议整体关闭 —— 一行回滚，
/// 与提示词里教不教这个形状由同一个开关决定（不虚报能力）。
pub(crate) fn parse_actions(
    raw: &str,
    max: usize,
    allow_batch: bool,
) -> Result<Vec<Action>, String> {
    let (v, markup) = json_of(raw)?;
    parse_actions_value(&v, max, allow_batch).map_err(|e| markup_note(e, markup))
}

/// 批协议本体：`Value` → 动作清单（语义见 [`parse_actions`]）。拆出来只为了让
/// "剥标记 → 抠 JSON → 报错加注脚"那一段各有一处，不被打成两份。
pub(crate) fn parse_actions_value(
    v: &serde_json::Value,
    max: usize,
    allow_batch: bool,
) -> Result<Vec<Action>, String> {
    // 批的两件外衣：actions / calls（模型两种都写过，认全了省一轮）
    let Some(arr) = v
        .get("actions")
        .or_else(|| v.get("calls"))
        .and_then(|x| x.as_array())
    else {
        return Ok(vec![parse_one(v)?]);
    };
    if !allow_batch {
        return Err("本轮不允许批量调用：请每轮只发一个调用（actions 已关闭）".into());
    }
    if arr.is_empty() {
        return Err("actions 为空".into());
    }
    if arr.len() > max {
        return Err(format!(
            "一批最多 {max} 个调用，这次给了 {} 个 —— 请拆成多轮，或按依赖关系分成几批",
            arr.len()
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let a = parse_one(item).map_err(|e| format!("actions 第 {} 个解析失败：{e}", i + 1))?;
        // final / plan 是控制动作不是调用：和调用混在一批里，"谁先谁后"没有合理解释 →
        // 当面拒掉（模型照提示词改一次就好），而不是替它猜一个顺序
        match a {
            Action::Final(_) => {
                return Err("final 不能放进批里：完成时单独发一轮 final".into());
            }
            Action::Plan(_) => {
                return Err("plan 不能放进批里：清单要单独发一轮".into());
            }
            Action::Ask(_) => {
                return Err(
                    "ask_user 不能放进批里：提问要单独一轮（同一批里两件事谁先谁后没有合理解释）"
                        .into(),
                );
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// 从 final 的参数里取答复：两条通道的键名（`answer` / `final`）与裸串都认。
///
/// 为什么键名都要认：工具协议的 schema 里叫 `answer`，老协议字段叫 `final`，而模型混写时
/// 这两种都可能出现（真跑日志里都有）。
pub(crate) fn final_answer(args: &serde_json::Value) -> Result<String, String> {
    let text = args
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| {
            args.get("answer")
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .or_else(|| args.get("final").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("final 的 answer 为空 —— 交付要写清结论、改了哪些文件、验证结果".into());
    }
    Ok(text)
}

/// 单个动作对象 → `Action`（final 也在这里认）
pub(crate) fn parse_one(v: &serde_json::Value) -> Result<Action, String> {
    // 先取"名字"和"参数"——两条通道的键名不同，这里统一（工具协议：`tool` + `arguments`；
    // 老协议：`tool` + `args`；模型混写时还会出 `name` + `arguments`，真跑里实测出现过）。
    let name = v
        .get("tool")
        .or_else(|| v.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let args = v
        .get("args")
        .or_else(|| v.get("arguments"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    // 交付（final）先认：它在工具协议里是**工具**，在老协议里是**字段**，模型走错通道时还会
    // 换包装写法（一次真跑里出现过 4 种，见 [`content_channel_error`] 的文档）。
    if let Some(f) = v.get("final").and_then(|x| x.as_str()) {
        let text = f.trim().to_string();
        if text.is_empty() {
            return Err("final 为空".into());
        }
        return Ok(Action::Final(text));
    }
    if name == "final" {
        return Ok(Action::Final(final_answer(&args)?));
    }
    // 只剩参数对象（`{"answer": "…"}`）：四种能力里只有 final 认 `answer` 这个键，所以无歧义 ——
    // 认它不是为了执行（严格模式下 content 通道本来就不执行），而是为了**认出形状**、
    // 给模型一句指向明确的纠正，而不是干巴巴的"外层缺 tool"。
    if name.is_empty()
        && args.is_null()
        && v.get("answer").is_some()
        && v.as_object().map(|o| o.len() == 1).unwrap_or(false)
    {
        return Ok(Action::Final(final_answer(v)?));
    }
    let tool = name;
    match tool.as_str() {
        // 进展记忆：允许进批（无副作用、无顺序风险）。也认 `findings` 这个短名 ——
        // 模型少写一个前缀不该白费一轮。
        "record_findings" | "findings" => parse_findings(&args),
        "read" => parse_read(&args),
        // write 两副面孔：content（整份）或 edits（锚点）。形状判断在 parse_write 里一处收口 ——
        // 与 read 的窗口参数同款：参数形状变了，能力没变。
        "write" => parse_write(&args),
        "connect" => {
            let action = args
                .get("action")
                .and_then(|x| x.as_str())
                .unwrap_or("list")
                .trim()
                .to_ascii_lowercase();
            Ok(Action::Connect(match action.as_str() {
                "" | "list" => ConnectAction::List,
                "call" => ConnectAction::Call {
                    server: get_str(&args, "server")?,
                    tool: get_str(&args, "tool")?,
                    arguments: args
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                },
                "send" => ConnectAction::Send {
                    agent: get_str(&args, "agent")?,
                    text: get_str(&args, "text")?,
                },
                other => {
                    return Err(format!(
                        "connect.action 只支持 list / call / send，收到 {other:?}"
                    ));
                }
            }))
        }
        // execute 是原语名；bash 留作别名 —— 老历史里写着 bash 的模型不该白烧一轮
        "execute" | "bash" => parse_execute(&args),
        // 第五个动作：需求歧义只能问委托人。args 里出现 answer / granted 之类一律拒 ——
        // 模型不许自问自答（答案只能从宿主的用户通道进来，与"复核 agent 构造上没有写路径"同款纪律）。
        "ask_user" | "ask" => {
            for forged in ["answer", "granted", "approved", "user_says"] {
                if args.get(forged).is_some() {
                    return Err(format!(
                        "ask_user 的 args 里不许有 {forged}（答案只能由用户给）—— 你要问的是问题，不是答案"
                    ));
                }
            }
            let why = get_str(&args, "why")?;
            let options: Vec<String> = args
                .get("options")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if options.len() > 5 {
                return Err(format!(
                    "ask_user 的选项最多 5 个（收到 {} 个）：选项太多用户反而答不了",
                    options.len()
                ));
            }
            let default_index = args
                .get("default_index")
                .and_then(|x| x.as_u64())
                .map(|v| v as usize);
            let bad_default = default_index.filter(|i| *i >= options.len());
            if let Some(i) = bad_default {
                return Err(format!(
                    "ask_user 的 default_index = {i} 越界（只有 {} 个选项）",
                    options.len()
                ));
            }
            Ok(Action::Ask(AskSpec {
                question: get_str(&args, "question")?,
                why,
                options,
                default_index,
                timeout_secs: 0,
            }))
        }
        "plan" => {
            let mut steps: Vec<PlanStep> =
                serde_json::from_value(args.get("steps").cloned().unwrap_or(serde_json::json!([])))
                    .map_err(|e| format!("plan.steps 解析失败: {e}"))?;
            if steps.is_empty() {
                return Err("plan.steps 为空".into());
            }
            for (i, s) in steps.iter_mut().enumerate() {
                s.id = (i + 1) as u32;
                if s.title.trim().is_empty() {
                    s.title = format!("步骤 {}", i + 1);
                }
            }
            Ok(Action::Plan(steps))
        }
        // 空名字 = 外层没有 `tool`：这不是"能力不认识"，是**形状不对**。两种常见来源：
        // 只发了 args 那一层（`{"cmd": "…"}`），或把动作写进了模型自带的工具调用标记里
        // （后者见 `llm::strip_model_markup` —— 标记已被剥掉，剩下的正是这坨裸 args）。
        // 报"未知能力 \"\"" 对模型毫无指向性，它只会换个说法重发同一坨；这里直接说缺什么。
        "" => Err(format!(
            "这不是动作对象：外层缺 `tool` 字段（收到的是参数对象 {}）。形状要写成 \
             {{\"tool\":\"execute\",\"args\":{{\"cmd\":\"…\"}}}} —— 能力名在外层 tool、参数放 args 里",
            clip(&v.to_string(), 160)
        )),
        other => Err(format!(
            "未知能力 {other:?}（原子能力只有 read / write / execute / connect 四种，外加 plan 清单、ask_user 提问，或输出 final）"
        )),
    }
}

/// 正整数字段（`offset` / `limit` 这类）。0 与负数都当场拒 ——
/// 静默收下 0 会让模型拿到空结果，然后以为"文件是空的"，比报错难查得多。
pub(crate) fn get_pos_usize(args: &serde_json::Value, key: &str) -> Result<Option<usize>, String> {
    let Some(v) = args.get(key) else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    let n = v
        .as_u64()
        .ok_or_else(|| format!("{key} 只能是正整数，收到 {}", clip(&v.to_string(), 40)))?;
    if n == 0 {
        return Err(format!("{key} 不能是 0（{key} 是行数/行号，从 1 起）"));
    }
    Ok(Some(n as usize))
}

/// `read` 的两副面孔：`{"path":"a.rs"}`（整份）与 `{"path":"a.rs","offset":120,"limit":60}`（窗口）。
///
/// 窗口只对**文件**有意义；目录给的本来就是结构树，窗口参数在那边被忽略（不报错：
/// 报错会换来一轮无效往返，而模型"想看看某个目录的一段"的意图本身没有歧义）。
/// `record_findings` 的参数：`{"items":[{"claim":…,"evidence":…,"note":?,"supersedes":?}]}`。
///
/// 也认"单个条目直接给"（`{"claim":…}`）与"items 给了一个对象"—— 少写一层包装不该白费一轮；
/// 但**空 items 一律拒**（空调用是无意义的一轮，要把它变成一句明确的纠正）。
pub(crate) fn parse_findings(args: &serde_json::Value) -> Result<Action, String> {
    let items = match args.get("items") {
        Some(serde_json::Value::Array(a)) => a.clone(),
        Some(other) => vec![other.clone()],
        None => vec![args.clone()],
    };
    if items.is_empty() {
        return Err("record_findings 的 items 为空（要给 claim + evidence）".into());
    }
    let mut out = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let spec: FindingSpec = serde_json::from_value(it.clone())
            .map_err(|e| format!("items 第 {} 条解析失败：{e}", i + 1))?;
        if spec.claim.trim().is_empty() {
            return Err(format!("items 第 {} 条的 claim 为空", i + 1));
        }
        out.push(spec);
    }
    Ok(Action::Findings(out))
}

/// `read` 的**两副面孔**收口在这一处（与 `parse_write` 同款）：`path` 是文件/目录读，
/// `scope` + `q` 是分层检索。**两个形状同时给 = 当面拒**，不猜哪个优先。
pub(crate) fn parse_read(args: &serde_json::Value) -> Result<Action, String> {
    let scope_raw = args
        .get("scope")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let q_raw = args
        .get("q")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let has_path = args
        .get("path")
        .and_then(|x| x.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    match (scope_raw, q_raw) {
        // 老形状一字不改
        (None, None) => {
            let path = get_str(args, "path")?;
            let offset = get_pos_usize(args, "offset")?;
            let limit = get_pos_usize(args, "limit")?;
            Ok(Action::Read(ReadSpec {
                path,
                offset,
                limit,
            }))
        }
        (None, Some(_)) => Err("read 收到了 q 但没有 scope —— 检索必须点名**在哪一层**找：\
             session / project_mem / global_mem / files"
            .into()),
        (Some(s), None) => Err(format!("read 的 scope={s:?} 没有配 q —— 要说清搜什么")),
        (Some(s), Some(q)) => {
            let scope = SearchScope::parse(&s).ok_or_else(|| {
                format!(
                    "未知 scope {s:?}：本版只开了四层 session / project_mem / global_mem / files \
                     （kb 还没开）"
                )
            })?;
            let path = args
                .get("path")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if has_path && !matches!(scope, SearchScope::Files) {
                return Err(format!(
                    "scope={} 不接受 path（那是**文件**层限定子树用的）：记忆类各层按 scope 取，\
                     不给路径",
                    scope.as_str()
                ));
            }
            if args.get("offset").is_some() || args.get("limit").is_some() {
                return Err(
                    "检索不接受 offset/limit（那是文件窗口读的参数）—— 要少要几条就用 max_hits"
                        .into(),
                );
            }
            let max_hits = get_pos_usize(args, "max_hits")?;
            Ok(Action::Search(SearchSpec {
                scope,
                q,
                path,
                max_hits,
            }))
        }
    }
}

/// `write` 的两副面孔收口在这一处：`content`（整份）或 `edits`（锚点）。
///
/// **两个都给 = 当面拒**，不猜哪个优先：模型给两套互相矛盾的意图时，猜错就是静默改错文件。
pub(crate) fn parse_write(args: &serde_json::Value) -> Result<Action, String> {
    let path = get_str(args, "path")?;
    let content = args.get("content").and_then(|x| x.as_str());
    let edits = args.get("edits");
    match (content, edits) {
        (Some(_), Some(_)) => Err(
            "write 的 args 里 content 与 edits 只能给一个：整份重写用 content，\
             只改动几处用 edits"
                .into(),
        ),
        (Some(c), None) => {
            if c.is_empty() {
                return Err("write.content 为空 —— 不允许静默清空文件".into());
            }
            Ok(Action::Write(WriteSpec {
                path,
                body: WriteBody::Content(c.to_string()),
            }))
        }
        (None, Some(v)) => {
            let list: Vec<AgentEdit> = serde_json::from_value(v.clone()).map_err(|e| {
                format!(
                    "write.edits 解析失败（形状是 [{{\"find\":\"要被替换的原文\",\"replace\":\"换成什么\"}}]）：{e}"
                )
            })?;
            if list.is_empty() {
                return Err("write.edits 为空 —— 没有改动就别说要改".into());
            }
            Ok(Action::Write(WriteSpec {
                path,
                body: WriteBody::Edits(list),
            }))
        }
        (None, None) => Err(
            "write 的 args 里必须给 content（整份内容）或 edits（锚点改动）：\
             改已有文件优先用 edits"
                .into(),
        ),
    }
}

/// `execute` 的三副面孔（同一个工具的三个生命周期）：
///
/// - `{cmd, timeout_secs}` —— **有界**：跑完为止（编译 / 测试 / git）
/// - `{cmd, background:true, ready_cmd, ready_timeout_secs, keep_alive}` —— **无界托管**：
///   起完即返，[`crate::proc`] 接管（服务 / 长驻进程）
/// - `{op:status|log|stop, handle}` —— **句柄操作**
///
/// 为什么不加第五个原语：见 [`crate::proc`] 模块文档 —— "常驻"没有引入新的*效果*，
/// 模型不该多一次毫无信息量的选型决策。
pub(crate) fn parse_execute(args: &serde_json::Value) -> Result<Action, String> {
    if let Some(op) = args.get("op").and_then(|x| x.as_str()) {
        let op = match op.trim().to_ascii_lowercase().as_str() {
            "status" => ProcOp::Status,
            "log" | "logs" | "tail" => ProcOp::Log,
            "stop" | "kill" => ProcOp::Stop,
            other => {
                return Err(format!(
                    "execute.op 只支持 status / log / stop，收到 {other:?}"
                ));
            }
        };
        return Ok(Action::Proc(op, get_str(args, "handle")?));
    }
    let cmd = get_str(args, "cmd")?;
    let background = args
        .get("background")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    if !background {
        return Ok(Action::Execute(
            cmd,
            args.get("timeout_secs").and_then(|x| x.as_u64()),
        ));
    }
    Ok(Action::ExecBg(crate::proc::StartSpec {
        cmd,
        ready_cmd: args
            .get("ready_cmd")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        ready_timeout_secs: args.get("ready_timeout_secs").and_then(|x| x.as_u64()),
        keep_alive: args
            .get("keep_alive")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    }))
}

/// 步骤执行体（`crate::step_agent`）能用的动作子集 —— 主循环解析结果的**收窄视图**。
///
/// 为什么不把 `Action` 直接开给子 agent：`plan` 会变成嵌套计划（父的计划谁维护？），
/// `connect` 会把外部能力的上下文塞进子上下文（它与本步无关，代价还是父的 token）。
/// 解析仍然只有 `parse_action` 一处真相，这里只做"哪些能用"的裁剪：
/// 不支持的能力回一条 `Unsupported`，让模型看到明确拒绝后自己收敛，而不是整步失败。
#[derive(Clone, Debug)]
pub(crate) enum StepAction {
    Final(String),
    Read(ReadSpec),
    /// 分层检索（只读 ⇒ 子步允许；它不动文件，所以**不参与**"声明文件已落地"的判据）
    Search(SearchSpec),
    Write(WriteSpec),
    Execute(String, Option<u64>),
    ExecBg(crate::proc::StartSpec),
    Proc(ProcOp, String),
    Unsupported(&'static str),
}

/// 主循环动作 → 步骤动作（plan / connect 收窄成「不支持」）
pub(crate) fn to_step_action(a: Action) -> StepAction {
    match a {
        Action::Final(t) => StepAction::Final(t),
        Action::Read(s) => StepAction::Read(s),
        Action::Search(s) => StepAction::Search(s),
        Action::Write(s) => StepAction::Write(s),
        Action::Execute(c, t) => StepAction::Execute(c, t),
        Action::ExecBg(s) => StepAction::ExecBg(s),
        Action::Proc(op, h) => StepAction::Proc(op, h),
        Action::Plan(_) => StepAction::Unsupported("plan"),
        Action::Connect(_) => StepAction::Unsupported("connect"),
        // 子步没有交互权：它的 messages 是干净上下文，一问就破了"一轮 = 一步"的派发语义
        Action::Ask(_) => StepAction::Unsupported("ask_user"),
        // 子步不记 findings：它读到的事实经**引擎写的步骤摘要**与共享账本回到主干
        // （子步骤借的是同一个 `&mut Ctx`，账本天然共享）。给它开这个口子会让
        // "子步的一轮 = 一步"这条派发语义多出一个不产生交付物的动作。
        Action::Findings(_) => StepAction::Unsupported("record_findings"),
    }
}

/// 单动作步骤入口（**不接受**批）。生产路径已全部走 [`parse_step_actions`]，
/// 这条只剩"单动作语义"的测试在用 —— 留着它，是让"老入口不接受批"这条契约可被断言。
#[cfg(test)]
pub(crate) fn parse_step_action(raw: &str) -> Result<StepAction, String> {
    Ok(to_step_action(parse_action(raw)?))
}

/// 步骤子 agent 的批入口：与主循环**同一套**解析（[`parse_actions`]），只是把动作收窄成
/// 三种能力。两个入口共用解析 —— 批协议不许在父子两处各长一遍。
pub(crate) fn parse_step_actions(
    raw: &str,
    max: usize,
    allow_batch: bool,
) -> Result<Vec<StepAction>, String> {
    Ok(parse_actions(raw, max, allow_batch)?
        .into_iter()
        .map(to_step_action)
        .collect())
}

/// 步骤执行体的**工具调用**入口：与主循环同一套解析（[`parse_tool_calls`]），只把动作收窄成
/// 三种能力 —— 与 [`parse_step_actions`] 的父子分工完全对称（批协议不许父子两处各长一遍）。
pub(crate) fn parse_step_tool_calls(
    calls: &[llm::ToolCall],
    max: usize,
    allow_batch: bool,
) -> Result<Vec<StepAction>, String> {
    Ok(parse_tool_calls(calls, max, allow_batch)?
        .into_iter()
        .map(to_step_action)
        .collect())
}

/// 本轮实际声明的工具名（严格模式的纠正话术里要点名"请用这些工具"）。
///
/// **父子两份名单必须各自自洽**：步骤执行体只有三种能力 + 交付，广告了它没有的工具
/// 等于让它去找一个不存在的能力（与 `batch_hint(.., has_plan=false)` 同一条纪律）。
pub(crate) const MAIN_TOOLS_HINT: &str =
    "read / write / execute / connect / plan / ask_user / final";
pub(crate) const STEP_TOOLS_HINT: &str = "read / write / execute / final";

/// 步骤执行体**声明给模型的工具**（= 它真能执行的那几个）。
///
/// 与 [`STEP_TOOLS_HINT`] 是同一件事的两种形态，必须一字不差地对齐 —— 本单的病就是它俩分家：
/// `STEP_SYSTEM` 里写着"引擎已声明 read / write / execute / final 四个工具"，
/// 而请求走的是 `llm::chat`（**全七个**），模型于是能从 `tools` 字段里看到
/// `connect` / `plan` / `ask_user`，一试就被 [`to_step_action`] 打回，白烧一轮。
/// **声明面就是权限面**：单一真相是 [`to_step_action`] 的那四支，这条 const 由契约测试钉死。
pub(crate) const STEP_TOOL_NAMES: &[&str] = &["read", "write", "execute", "final"];

/// 动作 → 工具名（日志与纠正话术里用；必须与 `llm::TOOL_DECLS` 的名字一致）。
pub(crate) fn action_name(a: &Action) -> &'static str {
    match a {
        Action::Final(_) => "final",
        Action::Plan(_) => "plan",
        // 分层检索是 `read` 的第二种形状 —— 工具名必须是 read（纠正话术、批上限、
        // 审计计数全都按工具名对账，这里写错会让"读类动作"的账对不上）
        Action::Read(_) | Action::Search(_) => "read",
        Action::Write(_) => "write",
        Action::Execute(..) | Action::ExecBg(_) | Action::Proc(..) => "execute",
        Action::Connect(_) => "connect",
        Action::Ask(_) => "ask_user",
        Action::Findings(_) => "record_findings",
    }
}

/// **严格模式的唯一处置**：content 通道里的动作一律**不执行**，换成一句指向明确的拒绝。
///
/// 为什么不能只说"无法解析"：实测真跑（cloud-shop 启动前后端，2026-09-24）第 13–17 轮，模型把
/// 同一个 `final` 答复换了 **4 种包装**反复发 —— 裸 `{"answer":…}`、
/// `{"tool":"final","args":{…}}`（两次）、`{"name":"final","arguments":{…}}` —— 而引擎每轮只回
/// 一句"请重新只输出一个 JSON 对象"，等于**把它按回**它已经在用的那条通道，于是连烧五轮；
/// 那五轮里答复内容一次比一次完整，纯粹死在"形状对、通道错"。所以这里必须**认出它想调哪个
/// 工具**再说话。
///
/// `tools_hint`：本轮实际声明了哪些工具（主循环 7 个、步骤执行体 4 个）—— 广告不存在的工具
/// 正是"父子提示词各自自洽"那条纪律防的事，所以名单由调用方给，不写死。
/// `strikes`：连续第几次走错通道（第 1 次讲清道理，第 2 次起只说一句短话）。
pub(crate) fn content_channel_error(raw: &str, strikes: usize, tools_hint: &str) -> String {
    let (clean, markup) = crate::llm::strip_model_markup(raw);
    let parsed = serde_json::from_str::<serde_json::Value>(clean.trim()).ok();
    let named = parsed.as_ref().and_then(recognize_tool);
    let mark_note = if markup > 0 {
        format!(
            "（另：本次输出里有 {markup} 处模型自带的工具调用标记，已剥掉 —— 那种标记不要再发）"
        )
    } else {
        String::new()
    };
    // 抄了**回显流水**（历史里那种带 tool_calls 键的信封）—— 单独一句话，因为病因在**我们自己的
    // 回显**上：泛泛说"没有工具调用"治不了它（真跑 73 轮里 2 轮被它带偏；2026-09-24 新会话
    // 第 6 轮又中一次，用户原话"我看模型返回的没有问题啊"）。
    // 话术里**不引用那个信封本身** —— 引用等于又教一遍。
    let copied_record = parsed
        .as_ref()
        .map(|v| v.get("tool_calls").is_some() || v.get("toolCalls").is_some())
        .unwrap_or(false);
    if copied_record {
        return format!(
            "你把**历史里那行流水**抄进了 content —— 那是引擎记的账（形如 `〔已发出调用〕…`），\
             是**记录**、不是发送格式{mark_note}。发送动作只有一条路：**直接用工具调用**\
             （{tools_hint}）。例：要 read 一个文件，就调 read 工具，别把它写成 content 里的 JSON。"
        );
    }
    match named {
        Some(what) if strikes <= 1 => format!(
            "你把动作写进了 content（`{what}`）—— **形状是对的，通道错了**：本引擎严格只认工具调用，\
             content 一律不执行{mark_note}。请用同名工具发它（可用：{tools_hint}）；一轮可以发多个\
             互不依赖的调用。content 只留给用户的文字，交付也走 final 工具。"
        ),
        Some(_) => format!(
            "又是 content（第 {strikes} 次）：本引擎只认工具调用，content 不执行{mark_note}。\
             请直接调用 {tools_hint} 之一。"
        ),
        None => format!(
            "content 里没有可执行的工具调用{mark_note}。本引擎严格只认工具调用：动作请调 {tools_hint}；\
             如果你是要交付，请调用 final 工具（不要把答复写成 content 里的 JSON）。"
        ),
    }
}

/// **认出 content 里那坨东西想调哪个工具**（只为把拒绝理由说清，**永不用于执行**）。
///
/// 三层认法，逐层放宽（真跑日志里每一层都出现过）：
/// 1. 按动作解析（`parse_one`）—— `{"tool":"final",…}` / `{"final":…}` / `{"answer":…}`；
/// 2. 按**参数形状**认（[`args_shape_tool`]）—— `{"cmd":…}` / `{"path":…}`（json_mode 把信封头截掉后就剩这些）；
/// 3. 再往里一层 —— `{"arguments":{"answer":…}}`（外层漏了名字）。
pub(crate) fn recognize_tool(v: &serde_json::Value) -> Option<String> {
    if let Some(a) = v.as_array()
        && !a.is_empty()
    {
        return Some(format!("一批 {} 个调用（顶层数组）", a.len()));
    }
    if let Ok(a) = parse_one(v) {
        return Some(action_name(&a).to_string());
    }
    if let Some(t) = args_shape_tool(v) {
        return Some(format!("{t} 的参数"));
    }
    if let Some(inner) = v.get("args").or_else(|| v.get("arguments"))
        && inner.is_object()
    {
        if let Ok(a) = parse_one(inner) {
            return Some(format!("{} 的参数", action_name(&a)));
        }
        if let Some(t) = args_shape_tool(inner) {
            return Some(format!("{t} 的参数"));
        }
    }
    None
}

/// **参数形状 → 工具名**（只用于把拒绝理由说清楚，**永不用于执行**）。
///
/// 为什么需要它：`json_mode` 会把模型自带信封的头部（含工具名）截掉，content 里就只剩一坨参数
/// （实测：`{"cmd": …}`、`{"path": …}`、`{"answer": …}`）。这几种形状在四种能力的协议内是**唯一解**
/// —— `cmd` 只有 execute 认、`path` 单独出现只可能是 read（write 必须有内容）—— 所以拿它来
/// **描述**"你发的是谁"，不是"替模型决定做什么"：说清之后本轮照样拒、由模型自己用工具重发。
pub(crate) fn args_shape_tool(v: &serde_json::Value) -> Option<&'static str> {
    let o = v.as_object()?;
    let has = |k: &str| o.contains_key(k);
    if has("cmd") || has("op") || has("handle") {
        return Some("execute");
    }
    if has("path") && (has("content") || has("edits")) {
        return Some("write");
    }
    if has("path") {
        return Some("read");
    }
    if has("steps") {
        return Some("plan");
    }
    if has("question") {
        return Some("ask_user");
    }
    if has("action") || has("server") || has("agent") {
        return Some("connect");
    }
    None
}

/// 解析失败 → 回灌给模型的一句话（`parse_failure_feedback` 的正文）。
///
/// 两种病两种药：截断（finish_reason=length）叫模型**写短**，格式烂才叫它重发。
/// 错误文本一律经 serde 编码 —— parse_action 的报错带着 JSON 片段，裸拼会把
/// 引号漏进字符串值，让这条反馈自己变成非法 JSON。
///
/// `tools = true`（严格模式）时**不能再说"请重新只输出一个 JSON 对象"**：那句话把模型推回
/// content 通道，而严格模式恰恰不执行 content —— 两条提示互相打架正是那五轮空转的成因。
pub(crate) fn parse_failure_feedback(
    err: &str,
    finish_reason: Option<&str>,
    tools: bool,
) -> String {
    let hint = if finish_reason == Some("length") {
        format!(
            "你的上一条输出被 max_tokens 截断了（finish_reason=length），不是格式问题：{err}。\
             请精简后重发：plan 的 detail 每条一句话、必要时减少 steps，不要重复刚才的长输出。"
        )
    } else if tools {
        format!(
            "你的上一条输出无法解析：{err}。请改用**工具调用**表达动作，不要再把动作写进 content。"
        )
    } else {
        format!("你的上一条输出无法解析：{err}。请重新只输出一个 JSON 对象。")
    };
    format!(
        "{{\"ok\": false, \"error\": {}}}",
        serde_json::to_string(&hint)
            .unwrap_or_else(|_| "\"输出无法解析，请重发一个 JSON 对象\"".into())
    )
}

/// LLM 调用**本身**失败的回灌（区别于 [`parse_failure_feedback`]：那边模型至少输出了
/// 一段可议的文本，这边是**整条输出蒸发** —— 截断落在 thinking / 半截 tool_use 里，
/// 引擎一个字节都没收到）。不回灌的后果实测过：模型对"上一条输出没了"毫不知情，
/// 原样重发同一个超预算的大输出，六轮零进展（2026-09-30 step 第 28-33 轮）。
///
/// **落点是可变尾**：调用方把它 push 成下一条 user 消息（消息序列的末尾 = 易变尾 A 段），
/// 不进 system；老的失败说明随历史折叠自然收走。
///
/// 截断与波动是两种病、两副药：截断必须叫模型改小交付粒度（edits / 分段写 / final 只写
/// 结论），否则它原样重发再截断一次；波动只需要它继续。
/// 截断判定走字符串：`err` 来自 llm.rs 的固定措辞（那边只产出带/不带 `finish_reason=length`
/// 两种"空内容"消息，没有结构化通道）—— 改那两行字面量时这里要跟着看一眼。
pub(crate) fn llm_failure_feedback(err: &str) -> String {
    let hint = if err.contains("finish_reason=length") {
        format!(
            "你的上一条输出被 max_tokens 截断，引擎**整条都没收到**（不是格式问题）：{err}。\
             不要原样重发同样的体量 —— 改小交付粒度：write 改用 edits 只传改动，\
             大文件分多次小段写，final 只写结论不贴长文。"
        )
    } else {
        format!("你的上一轮模型调用失败，引擎已重试仍未成功：{err}。请继续，输出尽量精简。")
    };
    format!(
        "{{\"ok\": false, \"error\": {}}}",
        serde_json::to_string(&hint).unwrap_or_else(|_| "\"模型调用失败，请继续\"".into())
    )
}
