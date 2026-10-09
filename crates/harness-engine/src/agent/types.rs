//! 引擎 Agent 的**数据形状**：常量上限 + 进出工具循环的结构体。
//!
//! 这些类型是 `agent` 的词汇表：`HistoryMsg` 是上一轮对话（`run` 收它、`AgentOutcome` 回它），
//! `WritePolicy` 决定写落在暂存区还是磁盘，`FileChange` 是改动台账（窄验证 / 回写 / 备份都认它），
//! `StepTrace` / `Probe` 是给 UI 与发现的观测形状。
//!
//! 从 `agent.rs` 拆出的（2026-09-28，文件当时 3975 行）；对外路径不变（`agent::WritePolicy` 等
//! 仍由父模块 `pub use` 再导一次）。

use super::*;

/// 循环轮次上限（每轮 = 一次模型调用，产出工具调用或最终答复）
pub const MAX_STEPS: usize = 96;

/// **连续**多少轮解析不出动作就停止本轮。
///
/// 为什么必须有这条：解析失败本身**不终止**循环（只回一句纠正再 `continue`），唯一的兜底是
/// `MAX_STEPS`（96 轮）与墙钟闸（默认 30 分钟）——于是"模型坚决不发工具调用"这种病会把整轮
/// 预算烧光，用户看到的就是**无限死循环**（实测：发一句"你好"，每轮收到同一句拒绝，一直转）。
/// 三轮足够说明"这条路走不通"：第 1 轮讲清道理、第 2 轮只说短话、第 3 轮还不行就是
/// 模型/端点侧的问题，再烧 93 轮只是把同一句话重复 93 遍。
///
/// **成功一轮就清零** —— 中间偶发一次格式烂不该累计成"病"。
pub const MAX_UNPARSEABLE_ROUNDS: usize = 3;
/// 连续模型调用失败的容忍上限：瞬时空内容/网络抖动退避后重试，连续超限才终止会话
/// 连续模型调用失败几次算致命。步骤执行体（`crate::step_agent`）复用同一条判据 ——
/// "抖两次就放弃"与"抖十次才放弃"是两种产品行为，不该在两个模块里各写一个数。
pub(crate) const LLM_FAIL_LIMIT: u32 = 3;
pub(crate) const READ_CLIP: usize = 8_000;
/// 窗口读不传 `limit` 时的默认行数（约"一个屏幕上下"）
pub(crate) const READ_WINDOW_DEFAULT_LINES: usize = 200;
/// 窗口读一次最多几行 —— 防止一个大 `limit` 把窗口读退化成"整份读 + 表头"
pub(crate) const READ_WINDOW_MAX_LINES: usize = 400;
pub(crate) const EXEC_CLIP: usize = 4_000;
pub(crate) const EXEC_DEFAULT_TIMEOUT_SECS: u64 = 30;
pub(crate) const EXEC_MIN_TIMEOUT_SECS: u64 = 5;
pub(crate) const EXEC_MAX_TIMEOUT_SECS: u64 = 120;
/// 连接清单注入提示词时的单目标工具名裁剪（服务器工具可能几十个）
pub(crate) const CONNECT_CLIP: usize = 400;
/// `execute_plan` 下允许模型重排计划几次。重排会把游标归零（新计划从第 1 步重跑），
/// 不设上限的话"失败 → 重排 → 又失败 → 再重排"能烧光整个轮次预算却什么都不产出。
pub(crate) const MAX_PLAN_RESETS: u32 = 2;

/// 一张图在字节代理里的固定配额（≈1200 token，见 `msgs_bytes`）。
/// 小到不会虚报规模、大到能让"阶梯大小"这个数把图算进去。
pub(crate) const IMAGE_PROXY_BYTES: usize = 4 * 1200;

/// 会话历史消息（调用方从 session 消息流裁剪后传入；只认 user/assistant）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryMsg {
    pub role: String,
    pub text: String,
    /// 这条消息当时那个 run 的 id（v1.5 会话层跨 run 检索的**钥匙**）。
    ///
    /// 为什么搭在 `history` 上而不是加参数：宿主本来就把整段会话发过来，`SessionMsg.run_id`
    /// 也已经落盘 —— 跨 run 检索要的"这个会话有哪几个 run"就在这段历史里，不必再开一条链路。
    /// 老会话没这个字段 ⇒ `None` ⇒ 那段历史不进检索（如实少搜，不假装搜过）。
    #[serde(default)]
    pub run_id: Option<String>,
}

/// 这段历史里出现过的 run id（**新 → 旧**，去重，去掉空值）。
///
/// 会话层检索要按时间倒序找：新的更可能有用。顺序由 `history` 里的先后决定
/// （宿主按时间顺序发），所以这里整体反转。
pub fn session_run_ids(history: &[HistoryMsg]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in history.iter().rev() {
        if let Some(id) = m.run_id.as_deref() {
            let id = id.trim();
            if !id.is_empty() && !out.iter().any(|x| x == id) {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// 写入策略：确认模式 → Stage；写入/自主模式 → Apply
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritePolicy {
    /// 只暂存（`<状态根>/stage/<id>/`，在 ruyix 程序目录里），UI 确认后才落盘
    Stage,
    /// 直接写进项目；被覆盖文件先备份到 `<状态根>/backups/agent-<ts>/`
    Apply,
}

/// 一个文件的变更记录（暂存清单 / 确认面板的差异来源）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileChange {
    pub path: String,
    /// add | modify
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    pub after: String,
}

/// 一轮工具调用的轨迹（UI 展示"Agent 都做了什么"）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StepTrace {
    pub step: usize,
    pub tool: String,
    pub brief: String,
    pub ok: bool,
}

/// 一次取证留痕的两端裁剪上限（它要进复核员的上下文）
pub(crate) const PROBE_CMD_CLIP: usize = 160;
pub(crate) const PROBE_OUT_CLIP: usize = 1_200;

/// 一次取证的留痕：跑过的命令 + 它实际的输出。
///
/// 复核员的依据核对原本只看得见「读过的文件」—— 命令输出用完即弃，只活在 `msgs` 里。
/// 于是凡**靠命令取证**的任务（今天星期几、端口占用、进程在不在、环境变量），答复里的
/// 断言必然被判"无处可查"，主循环再跑十条命令也喂不进复核员的输入包 —— 一条没有出口
/// 的死循环。这份记录就是把那条路补上。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Probe {
    pub cmd: String,
    pub output: String,
}

/// 一次调用的回灌信息：`(tool, brief, result)`。单动作路径与批里每条都用它
/// （免得同一个元组在五处各写一遍）。
pub(crate) type CallResult = (String, String, Result<String, String>);

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AgentOutcome {
    pub answer: String,
    pub steps: Vec<StepTrace>,
    pub changes: Vec<FileChange>,
    /// Stage 策略：暂存目录（files/ + manifest.json），确认后由调用方落盘
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_dir: Option<String>,
    /// Apply 策略：被覆盖文件的备份目录（没有覆盖则 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
    /// 机械验证的每一次结论（窄层 + 全量层，按发生顺序）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verifications: Vec<VerifyOutcome>,
    /// 反思（干净上下文复核）的每一次结论
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reflections: Vec<Reflection>,
    /// 本轮的提问留痕（问题 / 为什么问 / 答案 / 没答到的原因）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asks: Vec<AskRecord>,
    /// 模型在本 run 里确认过的事实 —— **完整账本**（被取代的也在，`superseded_by` 指出去向）。
    ///
    /// 为什么必须出去（v1.1 §8-5 当年拍了「是」，但一直没落）：findings 是"模型当时**凭什么**
    /// 那么改"的唯一记录，不进 outcome 就是跑完即焚 —— 宿主无从写进会话存档，用户重开会话
    /// 既看不到它确认过什么、也无从复核。**与落盘文件不是一回事**：`findings.md` 是模型在提示词里
    /// 看到的 active 视图，这里是完整账本（含被取代的）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<super::findings::Finding>,
    /// 本 run 的**留痕 id**（`<状态根>/ctx/<run-id>/transcript.jsonl` 那个目录名）。
    ///
    /// 交回宿主是**必须**的：宿主把它写进那条 assistant 消息的 `run_id`，下一个 run 才找得回
    /// 这一次的提问 / 工具调用 / 回答（会话层跨 run 检索的唯一钥匙）。此前聊天 run 不落
    /// RunRecord ⇒ 一直是 `None`，于是"前面几个 run"根本无从枚举。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub usage: Usage,
    pub elapsed_ms: u128,
}
