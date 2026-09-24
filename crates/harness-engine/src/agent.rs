//! 编程 Agent 的工具循环：Read / Write / Execute / Connect 四大原子能力。
//!
//! 为什么放弃"先分类问答/任务再走两条链路"：边界根本划不清 ——
//! 「帮我理解这个项目」要读代码才能答，「修一下这个报错」要读文件才知道怎么改。
//! 正确形态是一个循环：模型自己决定下一步用哪个能力（看结构 → 读文件 → 写回 → 跑测试），
//! 直到给出最终回答。能力集刻意只有四条原语，不搞一堆专项工具：
//!
//! - **Read**    读项目结构与文件内容（可给 `offset`/`limit` 只取一段；Git 历史经 Execute 的
//!   `git log` / `git show` 达成）
//! - **Write**   写文件，两种参数形状：`content`（整份，新建/大改）或 `edits`（锚点替换，
//!   改既有文件的主力）。**能力只有一条** —— 变的只是"要传多少东西"，见 [`WriteBody`]
//! - **Execute** 跑系统命令（工作目录钉在项目根、超时、输出裁剪；破坏性模式拒绝执行）
//! - **Connect** 连外部能力：MCP 服务器上的工具、A2A 远端 Agent（[`Connector`]）
//!
//! `plan` 不是第五种能力，只是一条给用户看的任务清单通道（经 `sink.plan` 推给大纲区，
//! 步骤文件全部落地后标 ✅、部分落地 ⛏️）。
//!
//! Connect 的落地端在宿主：引擎（零 tauri）只定 [`Connector`] 契约 —— 连什么是宿主环境的事
//! （子进程、HTTP、三 scope 配置），ruyix 侧用 MCP 客户端 + A2A 客户端实现，引擎搬到别的宿主
//! 时接别的实现即可。宿主没接任何外部能力时用 [`NoConnector`]（清单为空 → 提示词里不出现连接段）。
//!
//! 写入策略（[`WritePolicy`]）由调用方定：Stage = 只暂存到**项目状态根**（`<便携根>/projects/<项目 key>/stage/`），
//! UI 确认后才落盘；Apply = 直接写进项目（被覆盖文件先备份到同一状态根的 `backups/`）。
//! 两者都**不写用户仓库**（v1.0.0：IDE 状态属于 IDE，见 `doc/需求-便携形态与零残留-v1.0.0.md`）。

use crate::config::AppConfig;
use crate::discover;
use crate::exec::{self, CancelFlag, clip, is_cancelled};
use crate::generate::{StepOutcome, safe_rel_path};
use crate::lint;
use crate::llm::{self, ChatMessage, Usage};
use crate::pipeline::Sink;
use crate::plan::{Plan, PlanStep};
use crate::reflect::{self, Reflection};
use crate::repair;
use crate::step_agent::{self, StepInput, StepReport, StepSummary};
use crate::verify;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

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
const READ_CLIP: usize = 8_000;
/// 窗口读不传 `limit` 时的默认行数（约"一个屏幕上下"）
const READ_WINDOW_DEFAULT_LINES: usize = 200;
/// 窗口读一次最多几行 —— 防止一个大 `limit` 把窗口读退化成"整份读 + 表头"
const READ_WINDOW_MAX_LINES: usize = 400;
const EXEC_CLIP: usize = 4_000;
const EXEC_DEFAULT_TIMEOUT_SECS: u64 = 30;
const EXEC_MIN_TIMEOUT_SECS: u64 = 5;
const EXEC_MAX_TIMEOUT_SECS: u64 = 120;
/// 连接清单注入提示词时的单目标工具名裁剪（服务器工具可能几十个）
const CONNECT_CLIP: usize = 400;
/// `execute_plan` 下允许模型重排计划几次。重排会把游标归零（新计划从第 1 步重跑），
/// 不设上限的话"失败 → 重排 → 又失败 → 再重排"能烧光整个轮次预算却什么都不产出。
const MAX_PLAN_RESETS: u32 = 2;

/// 会话历史消息（调用方从 session 消息流裁剪后传入；只认 user/assistant）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryMsg {
    pub role: String,
    pub text: String,
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
const PROBE_CMD_CLIP: usize = 160;
const PROBE_OUT_CLIP: usize = 1_200;

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
    pub usage: Usage,
    pub elapsed_ms: u128,
}

// ============================================
// 机械验证结论（v0.3 门禁的对外形状）
// ============================================

/// 逐项检查的精简形状：**不带** stdout/stderr（几 KB 日志进事件流没意义，UI 只需要知道哪项失败）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CheckItem {
    /// syntax | test | lint
    pub kind: String,
    pub language: String,
    pub target: String,
    /// passed | failed | skipped
    pub status: String,
    pub reason: String,
}

impl From<&verify::CheckResult> for CheckItem {
    fn from(c: &verify::CheckResult) -> Self {
        Self {
            kind: c.kind.clone(),
            language: c.language.clone(),
            target: c.target.clone(),
            status: c.status.clone(),
            reason: c.reason.clone(),
        }
    }
}

/// 一次机械验证的结论：`layer` 区分窄层（改动后的语法检查）与全量层（交付前）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct VerifyOutcome {
    /// narrow | full
    pub layer: String,
    /// passed | failed | skipped
    pub status: String,
    /// 一句话结论（UI 直接显示）
    pub verdict: String,
    pub passed_checks: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 回灌给模型的修复提示（失败时非空）
    #[serde(default)]
    pub hint: String,
    /// 跳过原因（跑不了的时候必须有，不许静默）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    pub checks: Vec<CheckItem>,
    pub elapsed_ms: u128,
}

impl VerifyOutcome {
    pub fn layer_label(&self) -> &'static str {
        match self.layer.as_str() {
            "full" => "全量验证",
            _ => "语法检查",
        }
    }

    pub fn passed(&self) -> bool {
        self.status == "passed"
    }

    pub fn skipped(&self) -> bool {
        self.status == "skipped"
    }

    /// 汇总规则（窄/全量共用）：有 failed → failed；否则有 passed → passed；否则 skipped。
    /// "什么都没跑成"绝不写成"通过"。
    fn summarize(
        layer: &str,
        checks: &[CheckItem],
        elapsed_ms: u128,
        skipped_reason: Option<String>,
    ) -> Self {
        let failed_list: Vec<&CheckItem> = checks.iter().filter(|c| c.status == "failed").collect();
        let passed_checks = checks.iter().filter(|c| c.status == "passed").count();
        let skipped = checks.iter().filter(|c| c.status == "skipped").count();
        let failed = failed_list.len();
        let status = if failed > 0 {
            "failed"
        } else if passed_checks > 0 {
            "passed"
        } else {
            "skipped"
        };
        // 跳过的原因优先用调用方给的；没有就借第一条 skipped 检查的理由
        // （沙箱拒绝这类原因就写在那条检查里，不许退化成"没有可跑的检查项"）
        let reason = skipped_reason
            .or_else(|| {
                checks
                    .iter()
                    .find(|c| c.status == "skipped")
                    .map(|c| c.reason.clone())
            })
            .unwrap_or_else(|| "没有可跑的检查项".into());
        let verdict = match status {
            "failed" => format!(
                "未通过：{} 项失败（首个：{} {}）",
                failed,
                failed_list[0].target,
                clip(&failed_list[0].reason, 120)
            ),
            "passed" => format!("通过：{} 项检查全过", passed_checks),
            _ => format!("未执行检查：{reason}"),
        };
        Self {
            layer: layer.into(),
            status: status.into(),
            verdict,
            passed_checks,
            failed,
            skipped,
            hint: String::new(),
            skipped_reason: if status == "skipped" {
                Some(reason)
            } else {
                None
            },
            checks: checks.to_vec(),
            elapsed_ms,
        }
    }

    /// 从真实检查结果建结论（顺带用 `verify::hint_for` 把失败翻译成修复提示）
    fn from_checks(layer: &str, checks: &[verify::CheckResult], elapsed_ms: u128) -> Self {
        let items: Vec<CheckItem> = checks.iter().map(CheckItem::from).collect();
        let hint = checks
            .iter()
            .filter(|c| c.status == "failed")
            .map(|c| {
                format!(
                    "- {}（{}）：{}\n  {}",
                    c.target,
                    c.language,
                    c.reason,
                    verify::hint_for(c)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut out = Self::summarize(layer, &items, elapsed_ms, None);
        out.hint = hint;
        out
    }

    /// 跑不了（确认模式不落盘 / 沙箱拒绝 / 没有可跑命令）—— 必须写明原因
    fn skip(layer: &str, reason: String) -> Self {
        let mut out = Self::summarize(layer, &[], 0, Some(reason));
        out.status = "skipped".into();
        out
    }

    /// 回灌主循环的观察文本（模型看到的是"哪一层的什么检查失败了、怎么修"）
    pub(crate) fn to_observation(&self) -> String {
        let mut s = format!("[机械验证·{}] {}\n", self.layer_label(), self.verdict);
        if let Some(r) = &self.skipped_reason {
            s.push_str(&format!("跳过原因：{r}\n"));
        }
        for c in self.checks.iter().filter(|c| c.status == "failed") {
            s.push_str(&format!("✗ {} {}：{}\n", c.kind, c.target, c.reason));
        }
        if !self.hint.trim().is_empty() {
            s.push_str(&self.hint);
            s.push('\n');
        }
        s.push_str("（修完再交付；验证没通过不算完成。）");
        s
    }
}

// ============================================
// Connect 原语：引擎定契约，宿主接外部系统
// ============================================

/// 宿主提供的"环境准备"连接的 kind 值。
///
/// 它不是第五种原语，也不是新的 connect 形态：宿主只要在 `list()` 里摆出这个 kind 的
/// 目标、并在 `call()` 里认它，引擎侧**零改动**就能用。装什么、用哪个包管理器、装不装 ——
/// 全是宿主的裁量（引擎不知道 choco / apt / brew 的存在，也就不会一格一格补分支）。
///
/// 引擎只关心一件事：**有这么个连接时，缺失工具才允许指向 connect**（见 `discover::render_note`）。
pub const ENV_CONNECTOR_KIND: &str = "env";

/// 一个可连接的外部能力（`connect` 的 list 结果，也是注入提示词的清单项）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ConnectTarget {
    /// `mcp`（MCP 服务器）| `a2a`（远端 Agent）
    pub kind: String,
    pub name: String,
    /// 人读的补充：MCP = 连接状态 / 启动命令，A2A = 端点 URL
    #[serde(default)]
    pub detail: String,
    /// kind = mcp 时的工具名（带括号描述），供模型挑工具
    #[serde(default)]
    pub tools: Vec<String>,
}

/// 一次连接请求（引擎 → 宿主）。`action` 只有两形态：call（MCP 工具）/ send（委托远端 Agent）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ConnectRequest {
    /// `call` | `send`
    pub action: String,
    /// MCP 服务器名（action = call）
    #[serde(default)]
    pub server: String,
    /// MCP 工具名（action = call）
    #[serde(default)]
    pub tool: String,
    /// 工具参数（action = call，原样透传给服务器）
    #[serde(default)]
    pub arguments: serde_json::Value,
    /// 远端 Agent 名（action = send）
    #[serde(default)]
    pub agent: String,
    /// 委托文本（action = send）
    #[serde(default)]
    pub text: String,
}

/// 一次连接的结果
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ConnectOutcome {
    pub text: String,
    /// 外部系统自己报的失败（不是"连不上"）—— 仍然回给模型，让它自己决定下一步
    #[serde(default)]
    pub is_error: bool,
}

/// 连接器返回的 future。手工装箱而不是引入 `async-trait`：引擎的依赖表已经够长，
/// 而这里只需要一个 dyn 兼容的异步签名。
pub type ConnectFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// **Connect 原语的落地端**。引擎不知道 MCP、HTTP 或子进程的存在，
/// 只把"列清单"和"发一次连接"两件事委托给宿主。
pub trait Connector: Send + Sync {
    /// 列出当前可连的外部能力（没有就返回空 —— 提示词里不会出现连接段）
    fn list(&self) -> ConnectFuture<'_, Vec<ConnectTarget>>;
    /// 真正发起一次连接。`Err` = 连不上/目标不存在；外部系统的业务错误走 [`ConnectOutcome::is_error`]
    fn call(&self, req: ConnectRequest) -> ConnectFuture<'_, ConnectOutcome>;
}

/// 未接任何外部能力的宿主实现（引擎单测、评估臂用）
pub struct NoConnector;

impl Connector for NoConnector {
    fn list(&self) -> ConnectFuture<'_, Vec<ConnectTarget>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn call(&self, req: ConnectRequest) -> ConnectFuture<'_, ConnectOutcome> {
        Box::pin(async move {
            Err(format!(
                "这个宿主没有接外部能力，connect 不可用（请求：{} {}）",
                req.action, req.server
            ))
        })
    }
}

/// 连接清单 → 提示词里的一段话（空清单 → None，模型不知道自己没有的能力）
fn connect_note(targets: &[ConnectTarget]) -> Option<String> {
    if targets.is_empty() {
        return None;
    }
    let mut s = String::from("可用外部连接（用 connect 调用）：\n");
    for t in targets {
        s.push_str(&format!("- {} {}：{}", t.kind, t.name, t.detail.trim()));
        if !t.tools.is_empty() {
            s.push_str(&format!(
                "；工具：{}",
                clip(&t.tools.join("、"), CONNECT_CLIP)
            ));
        }
        s.push('\n');
    }
    Some(s.trim_end().to_string())
}

pub const AGENT_SYSTEM: &str = r#"你是 ruyix IDE 里的编程 Agent，通过工具循环完成用户的工作。**动作一律用工具调用表达** —— 引擎已声明 read / write / execute / connect / plan / ask_user / final 七个工具，参数名与说明见工具声明。只有四种原子能力：

- read    读项目：read(path="src/ 或 src/main.rs") —— 目录给结构树，文件给内容；Git 历史用 execute 跑 git log / git show 查。
  文件大、只要一段：read(path="src/big.rs", offset=120, limit=60) —— 从第 120 行起读 60 行（行号从 1 起，一次上限 400 行）。表头上写着「第 a-b 行 / 共 N 行；还有 M 行，接着读用 offset=X」，照着它接着读就能把缺口补齐。
  **该用窗口**：文件好几百行、你只需要其中一段（定位一个函数、看某一处报错）；**别用窗口**：文件不大（一次读完更省事），或者你紧接着要用 content 整份重写它（那必须先看全）。只读了一段就别在 final 里断言"全篇如何如何"—— 没看到的部分就是没看到。
- write   写文件，两种写法**二选一**：
  · 改已有文件里的一处或几处 —— 用 **edits**（首选：只传改动，不重发全文）：
    write(path="src/x.rs", edits=[{"find":"要被替换的原文","replace":"换成什么"}])
    `find` 必须与文件里**逐字符一致**（含缩进），且在文件里**恰好出现一次**；不唯一就把上下文多带两行。匹配不上或撞上多次 → **整批作废、一个字节都不落盘**，所以改之前先 read 确认原文。同一文件的多条 edits 按你给的顺序累积。
  · 新建文件、或改动大到划不出锚点 —— 用 **content**（整份内容）：
    write(path="相对路径", content="完整文件内容")
  **别用 edits**：新建文件（没有原文可锚）、整篇重排、或者你压根没读过这个文件 —— 猜出来的 find 匹配不上，白费一轮。**别用 content**：只改几行的既有文件 —— 把整份吐回来既贵又容易顺手丢原文（曾经真这么删掉过用户的代码）。
  两种形态**不许同时给**（引擎当面拒），content 不许是空串。
- execute 跑命令：execute(cmd="命令", timeout_secs=30) —— 工作目录是项目根，超时上限 120 秒；编译、测试、格式化、git 都走它。
  永不退出的服务（spring-boot:run / java -jar / npm run dev / vite）**必须**用后台模式，不要用 start、Start-Process、往 %TEMP% 写 bat/ps1 那类花招（它们拿不到输出，进程还会脱离掌控）：
  execute(cmd="mvn spring-boot:run", background=true, ready_cmd="netstat -ano | findstr :8083", ready_timeout_secs=90, keep_alive=true)
  background 起完不等它；ready_cmd 是**一条命令**，退出码 0 即就绪（不写就等于不等、起完即返）。返回 handle、pid 与日志**文件路径**，输出全部落在那个文件里（路径由引擎给，别自己写重定向）。
  **keep_alive 决定它活不活得过本次 run**：不写就是 false —— 本次 run 一结束，引擎就把它收掉（连子进程树）。用户要的是“把服务跑起来”（让我访问 / 留着跑 / 等会儿用）→ **必须**写 keep_alive=true：它会留在引擎进程表里，用户在【服务】面板能看到、能按 pid 停掉，IDE 退出时一并收。只是验证它起不起得来、随后就停 → 别写，并在 final 里说明“本次结束已自动收掉”。**报“已启动”时它必须还活着**：不带 keep_alive 却报“服务已启动”，用户 netstat 一看就是空的 —— 那是谎报，不是措辞问题。
  之后用 execute(op="status", handle="p1") 查状态（它会告诉你这个进程有没有声明 keep_alive）、op="log" 读日志尾、op="stop" 停掉（连子进程树一起杀）。重启同一个服务前先 status / stop：端口被上一次的进程占着时，"起不来"是假的。
- connect 连外部能力：connect(action="list") 先看有哪些可连；调 MCP 工具用 connect(action="call", server="服务器名", tool="工具名", arguments={})；把任务委托给远端 Agent 用 connect(action="send", agent="名字", text="任务描述")。可用清单在提示词里给过，没有的就别硬猜名字。
- plan    任务清单（不是第五种能力，只是给用户看进度）：要动多个文件时先 plan(steps=[{"title":"短标题","detail":"做什么","files":["相对路径"]}])，用户会在大纲区看到进度。files 只列**这一步真的会写（新建或整文件重写）**的文件；只是要读一读、参考一下的，或者已经躺在项目里不用改的，都不要列 —— 大纲的进度是拿这份清单对账的，列多了会让做完的步骤看起来没做完。

规则：
1. 每轮用**工具调用**表达动作（可以一轮发多个互不依赖的）。**不要把动作写进 content** —— 本引擎严格只认工具调用，content 里的动作一律**不执行**（只会被退回来重发一轮）；也不要输出解释文字、不要 markdown 代码块包裹，更不要输出任何工具调用标记（DSML 之类）。content 只留给用户的文字，交付也走 final 工具。历史里助手消息中形如 `〔已发出调用〕…` 的是**引擎记的流水**（记录你上几轮发过什么），它**不是**发送格式，别照着它把调用写进 content。
2. 回答关于本项目的问题前，先 read 相关文件/目录 —— 不要凭空猜测项目内容。
3. 改代码：先 read 拿到现状。改已有文件用 write + edits 只传改动；新建文件、或整篇重排才用 content 交回整份。没把握的地方原样保留，绝不丢内容。
4. 改动能验证就验证：execute 跑编译/测试（如 cargo test、python -m pytest、npm test），失败就继续修。
5. 项目之外的东西（数据库、浏览器、远端服务、另一个 Agent）走 connect —— 不要自己写脚本硬凑协议，也不要把外部能力的事当成项目内的改动。
6. 全部完成后**调用 final 工具交付**：final(answer="给用户的完整说明（Markdown：结论、改了哪些文件、验证结果）")
   final 是**给用户的答复**，不是取证记录：不要整段粘贴命令的原始输出、编译/测试日志或文件内容 ——
   那些已经在你自己的调用结果里（用户也能在 Agent 面板逐条看到），抄进交付只会把答复撑爆、
   还容易在转义上翻车，一轮白干。要引用就摘那一行结论（哪个端口、哪个版本号、第几行报错），不要整张贴。"#;

/// 系统提示词 = [`AGENT_SYSTEM`] + 平台特定补充。
/// Windows 上 findstr 的多文件掩码语义不稳（`/c:"串"` 配多个通配符经常漏配），
/// 实测模型要试 5-6 个变体才命中 —— 一行提示换掉这些试错轮。
/// `AGENT_SYSTEM` 保持常量不动（测试直接断言其内容），平台差异在这里拼接。
fn agent_system_prompt() -> String {
    let mut s = AGENT_SYSTEM.to_string();
    #[cfg(target_os = "windows")]
    s.push_str(
        "\n\nWindows 检索提示：内容搜索优先 git grep -i -l <词>（快且稳、跟随 .gitignore，项目不是 git 仓库时不可用）；\
         \nfindstr 的多文件掩码行为不稳（/c:\"串\" 配多个通配符常漏配），需要时单掩码多次跑，或 for /r %f in (*.java) do findstr /m /c:\"词\" \"%f\" 逐个遍历。",
    );
    s
}

/// 确认模式（Stage）下必须显式告诉模型的三件事 —— 不写它，模型会拿 `execute` 的失败
/// 当成"我的改动错了"，反复 findstr/type/dir 对账，烧掉几十轮。
///
/// 实测 run `agent-20260920-152312`（cloud-shop 修一个 Maven 依赖）：写 `pom.xml` 只进暂存区后，
/// `read`（本会话覆盖层）看到**新**内容、`execute`（真实磁盘）看到**旧**内容 —— 两个工具给出
/// **互相矛盾**的证据，模型调和不了，65 轮里 45+ 轮在做无效侦察。根因不是模型能力，是提示词
/// 从未提"写入可能只暂存"，而规则 4 还写着"用 execute 验证改动" —— 确认模式下那条是陷阱，
/// execute 永远看到旧文件、永远失败。
///
/// 写入/自主模式（[`WritePolicy::Apply`]）磁盘真会变，没有这层矛盾 —— 返回 `None`，提示词不虚报。
pub(crate) fn policy_system_note(policy: WritePolicy) -> Option<&'static str> {
    match policy {
        WritePolicy::Stage => Some(
            "当前运行模式：**确认模式**。你的 write 只写进 **IDE 的暂存区**\
             （在 ruyix 程序目录里，**不在你的项目目录里**），\
             **项目磁盘上的文件不会变**，要等用户在界面里确认后才落盘。因此：\n\
             - `read` 看到的是你**暂存后**的内容（本会话覆盖层），这是对的，不是文件真变了；\n\
             - `execute` 跑的是**项目真实磁盘**，看到的是**改动前**的旧文件。两者不一致时以 read 为准，\
             那不是你的改动出错；\n\
             - **不要用 execute 验证本次改动**（编译、测试、findstr、type、dir 都一样）：它看不到你的\
             修改，失败不等于改动错了、成功也不等于生效，反复试只会白烧轮次；\n\
             - 改完直接输出 final，在答复里写明「改动已暂存、待确认」，并给出用户确认后该跑的验证命令。",
        ),
        WritePolicy::Apply => None,
    }
}

/// 确认模式下 `execute` 结果的随附提示：本命令读的是项目磁盘，看到的是**改动前**的文件。
/// 只在"已经有暂存改动"时贴 —— 没写过东西就没有这层歧义，省 token。
pub(crate) fn staged_execute_note(policy: WritePolicy, has_changes: bool) -> &'static str {
    if policy == WritePolicy::Stage && has_changes {
        "\n\n（注意：确认模式下项目磁盘未改动，上面这条命令读到的仍是**改动前**的文件；\
         你本次的写入还在暂存区。别拿它的结果判断本次改动的成败 —— 改完直接 final。）"
    } else {
        ""
    }
}

/// 批量调用：一轮发多个互不依赖的调用，引擎把只读的那些并发跑、其余按声明顺序串行。
///
/// 这是**省轮次的主通道**：一次读 5 个文件原先要 5 轮（每轮一次模型往返，文件内容还要
/// 全文回灌进上下文），现在 1 轮。提示词里必须给出形状 —— 模型不会凭空发明一个没见过的
/// 字段名（同 `keep_alive` 的教训：没写进提示词的能力等于不存在）。
///
/// 放在**首轮 user 消息**而不是 `AGENT_SYSTEM`：与 connect 清单 / 命令发现 / 写入策略同一条
/// 通道（那里已有一处 note 的落点），且 `AGENT_SYSTEM` 保持常量不动（测试直接断言其内容）。
const BATCH_HINT_HEAD: &str = concat!(
    "批量调用（省轮次）：互不依赖的调用可以一轮发一批 —— 也就是同一轮里发**多个工具调用**",
    "（例如同时 read(\"a.rs\") 与 read(\"b.rs\")）",
    "。引擎**把一批里的调用并发跑**（读 / 写 / 执行 / 连接各走各的），结果在同一轮的 results 里按同样顺序一次全给你。",
    "同一批里**只有两种情况**会被自动按你给的顺序排：对同一个文件的写与读、以及托管进程的起停查；",
    "其余依赖（先 build 再 test、抢同一个端口、拿上一条的输出当参数）引擎看不出来 —— 有依赖就分两轮发。"
);

/// 批上限也要写进提示词（数字给模型，它据此拆批）—— 只写"可以批"会让模型发 20 个，
/// 换来一次拒绝，白烧一轮。
///
/// `has_plan` 决定要不要提"清单不能进批"：**步骤子 agent 没有清单工具**（STEP_SYSTEM 只有
/// 三种能力），提了它反倒会去找一个不存在的能力 —— 父子提示词各自自洽这条纪律，与后台模式
/// 那段同源（父那份到不了子上下文，子那份也不许广告父的能力）。
/// 联网检索的提示词 —— 与 [`batch_hint`] 同一条通道（**首轮 user 消息**）。
///
/// 为什么非写不可：服务端联网是"随时可用"的能力，但模型**不知道自己能联网就不会发起检索**
/// （同 `keep_alive` 的教训）。实测对照：同一道"某日上证收盘点位"，提示词里没提联网时
/// 模型答"该日期在未来，我无法获取"；提了之后它自行检索并答出准确数值。
///
/// 判据必须成对写，理由与 `ask_hint` 同源：只教"可以搜"会得到一个什么都搜的助手，
/// 只教"别搜"会得到一个什么都按训练数据猜的助手。
/// 联网取证在【本轮取证】里的标记串。
///
/// **agent 与复核提示词共用这一个常量**：两边各写一份字面量就会漂移 —— 复核员认不出
/// 那条取证是联网检索，于是把"模型知道最近的事"判成 unsupported，死锁换个入口重演。
pub(crate) const WEB_SEARCH_PROBE_LABEL: &str = "联网检索（服务端执行）";

pub(crate) const WEB_SEARCH_HINT: &str = "\
联网检索（本次会话已开启）：你**可以**检索互联网，涉及最新事实的问题该查就查 —— \
服务端替你检索、把结果直接放进上下文，你不需要自己发命令抓网页，也不用声明工具。\n\
- **该查**：知识有截止日期的那些事 —— 现在的版本号 / 行情 / 新闻 / 文档更新 / 报错的最新解法。\
  不查就是猜，猜了就会给出一个自信而过期的值。\n\
- **别查**：项目里读得出来的（read / 命令取证），本地证据比检索结果硬；\
  也不需要为一个纯代码问题去联网。\n\
- 答复里说明哪些结论来自联网（用户有权知道依据），但**不许把检索原文整段贴进 final**。";

pub(crate) fn batch_hint(max: usize, has_plan: bool) -> String {
    let ctrl = if has_plan {
        "final / plan / ask_user 是控制动作，不能放进批里，要单独一轮输出。"
    } else {
        // 子步骤没有清单也没有提问权，不许提它们（父子提示词各自自洽）
        "final 不能放进批里，要单独一轮输出。"
    };
    format!("{BATCH_HINT_HEAD}{ctrl}（一批最多 {max} 个调用）")
}

/// 提问（`ask_user`）的提示词 —— 与 [`batch_hint`] 同一条通道（**首轮 user 消息**），
/// 且**只给主循环**：步骤子 agent 没有交互权（子步的 `ask_user` 是 `Unsupported`），
/// 广告一个子步做不到的能力就是虚报能力（与 plan 那条同源）。
///
/// 判据必须成对写：只教"可以问"会得到一个什么都问的助手，只教"别问"会得到一个什么都猜的助手。
pub(crate) fn ask_hint(cfg: &AppConfig) -> String {
    format!(
        "提问（需求歧义）：**需求本身含糊时用 ask_user 问用户**，不要猜 —— 猜错的代价常常是整体返工。\n\
         形状：{shape}\n\
         什么时候**必须**问：① 目的物 / 环境 / 范围指代不明确，且选错要整体返工（例：\"做一个远程登录功能\" —— 登哪台机器？公司托管的服务器，还是用户自己另一台电脑？）；② 动作不可逆或破坏性，而任务原文没有明确授权。\n\
         什么时候**不许**问：能从项目里读出来的（自己去 read；命令发现表已经告诉你本机有什么）；只有可回退的差异（命名、默认值、目录结构）—— 那时**声明你的假设继续做**，把差异写进 final 的说明里。\n\
         纪律：一次 run 最多问 {max} 次，且 ask_user 要单独一轮（不能进批）；超时或无人回答时引擎会**拒绝**依赖它的动作 —— 所以答案没来之前别把工作压在那个假设上。",
        shape = r#"{"tool":"ask_user","args":{"question":"…","why":"这个答案会决定接下来的什么动作","options":["选项甲","选项乙"],"default_index":0}}"#,
        max = cfg.ask.max_per_run
    )
}

/// 拿到答案的观察：**明确它不是授权** —— 门禁仍走"用户看过改动内容"的暂存确认。
fn ask_answer_note(id: &str, ans: &AskAnswer, spec: &AskSpec) -> String {
    let picked = match ans.option_index.and_then(|i| spec.options.get(i)) {
        Some(o) => format!("\n（他选了：{o}）"),
        None => String::new(),
    };
    format!(
        "用户（**委托人本人**）回答了 {id}：\"{}\"{picked}\n问的是：{}\n\
         注意：这是人的回答，只用于消除歧义，**不构成任何门禁的授权** —— 写盘照旧按你看到的模式与确认流程走。",
        ans.text, spec.question
    )
}

/// 没拿到答案的观察：fail-closed —— 拒绝依赖它的动作，并点名它必须去交付 + 写清假设。
fn ask_failed_note(id: &str, err: &AskErr, _spec: &AskSpec) -> String {
    format!(
        "ask_user 没有得到回答（{id}：{}）—— **这不是\"同意\"，也不是\"没人关心\"**；依赖这个答案的动作现在**不许做**。\
         \n请改为：把不确定的点写进 final 的说明里（你的假设是什么、用户之后要确认什么），或换一条不依赖它的路。",
        err.reason()
    )
}

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

#[derive(Clone, Debug)]
enum Action {
    Final(String),
    Plan(Vec<PlanStep>),
    Read(ReadSpec),
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
enum ConnectAction {
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

fn get_str(v: &serde_json::Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("args 缺少字符串字段 {key}"))
}

/// 单动作入口：**不接受**批（老语义）。生产路径走 [`parse_actions`]，
/// 这条留给测试断言"批关掉时该拒就拒"。（`Action` 只在本模块用，别把内部枚举泄成 pub）
#[cfg(test)]
fn parse_action(raw: &str) -> Result<Action, String> {
    let (v, markup) = json_of(raw)?;
    parse_one(&v).map_err(|e| markup_note(e, markup))
}

/// 单动作与批动作的**公共前段**：剥掉模型自带的协议标记 → 抠 JSON → 解成 `Value`。
///
/// 剥标记要对着 `raw` 做、也只做在这里：`extract_json_object` 是全仓唯一的抠 JSON 收口点，
/// 模型自带的那套标记（[`llm::strip_model_markup`]）在进解析器之前就剪掉，
/// 于是 plan / generate / repair / reflect / eval 那些调用方一起受益，不用各自记得过滤。
fn json_of(raw: &str) -> Result<(serde_json::Value, usize), String> {
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
fn markup_note(err: String, markup: usize) -> String {
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
fn parse_tool_calls(
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
fn parse_actions(raw: &str, max: usize, allow_batch: bool) -> Result<Vec<Action>, String> {
    let (v, markup) = json_of(raw)?;
    parse_actions_value(&v, max, allow_batch).map_err(|e| markup_note(e, markup))
}

/// 批协议本体：`Value` → 动作清单（语义见 [`parse_actions`]）。拆出来只为了让
/// "剥标记 → 抠 JSON → 报错加注脚"那一段各有一处，不被打成两份。
fn parse_actions_value(
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
fn final_answer(args: &serde_json::Value) -> Result<String, String> {
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
fn parse_one(v: &serde_json::Value) -> Result<Action, String> {
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

/// 冲突判定只看三件事：**路径 / 是不是写 / 是不是托管进程操作**。
/// 父子两种动作（`Action` / `StepAction`）都映射到它 —— 判定内核只有一份。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shape<'a> {
    path: Option<&'a str>,
    write: bool,
    proc: bool,
}

fn shape_of(a: &Action) -> Shape<'_> {
    match a {
        Action::Read(s) => Shape {
            path: Some(&s.path),
            write: false,
            proc: false,
        },
        // 冲突只看路径与"是不是写"，不看传的是 content 还是 edits：
        // 同一个文件的两种写法仍然互斥。
        Action::Write(s) => Shape {
            path: Some(&s.path),
            write: true,
            proc: false,
        },
        Action::ExecBg(_) | Action::Proc(..) => Shape {
            path: None,
            write: false,
            proc: true,
        },
        Action::Execute(..)
        | Action::Connect(_)
        | Action::Plan(_)
        | Action::Final(_)
        | Action::Ask(_) => Shape {
            path: None,
            write: false,
            proc: false,
        },
    }
}

pub(crate) fn shape_of_step(a: &StepAction) -> Shape<'_> {
    match a {
        StepAction::Read(s) => Shape {
            path: Some(&s.path),
            write: false,
            proc: false,
        },
        StepAction::Write(s) => Shape {
            path: Some(&s.path),
            write: true,
            proc: false,
        },
        StepAction::ExecBg(_) | StepAction::Proc(..) => Shape {
            path: None,
            write: false,
            proc: true,
        },
        StepAction::Execute(..) | StepAction::Final(_) | StepAction::Unsupported(_) => Shape {
            path: None,
            write: false,
            proc: false,
        },
    }
}

/// 两条调用是否**必须保序**（结构性判定 —— 只看原语与路径，**不猜命令语义**）。
///
/// 只有两种冲突：
/// 1. **同一条路径上的写**：`write` 与同路径的 `read`/`write` 必须保序 —— 覆盖层是"写完立刻
///    读回来"的依据，同文件两次写的先后也是模型表达的意思；
/// 2. **托管进程的生命周期操作**（`background` 起 / `status`·`log`·`stop`）：那张进程表是引擎
///    自己持有的共享状态，句柄又由引擎赋值 —— 同批里"起完再查"必须保序。
///
/// 其余**一律并发**：`execute` 之间共享什么（构建缓存、锁、端口）引擎不知道，凭命令文本猜就是
/// app 知识泄漏 —— 这属于**模型的依赖声明**（提示词已写明"同一批并发跑，有依赖就分两轮发"）。
pub(crate) fn conflicts(a: &Shape<'_>, b: &Shape<'_>) -> bool {
    if a.proc && b.proc {
        return true;
    }
    match (a.path, b.path) {
        (Some(x), Some(y)) => x == y && (a.write || b.write),
        _ => false,
    }
}

/// 一批动作 → **执行波次**：波内互不冲突（并发跑），波与波之间按声明顺序。
///
/// 贪心分层：每条落在"它所有冲突前驱的下一波"。于是 `[read a, read b, write a, read a]`
/// → `[[0,1],[2],[3]]`：两次读并发；写 a 等读 a；最后一个读 a 等写 a（于是读到新内容）。
/// 最常见的批（互不相关的一串读 / 一串命令）**只有一波** —— 那就是"一起并发"。
pub(crate) fn waves_by(shapes: &[Shape<'_>]) -> Vec<Vec<usize>> {
    let mut level: Vec<usize> = Vec::with_capacity(shapes.len());
    for (i, a) in shapes.iter().enumerate() {
        let mut lv = 0;
        for (j, b) in shapes.iter().enumerate().take(i) {
            if conflicts(a, b) {
                lv = lv.max(level[j] + 1);
            }
        }
        level.push(lv);
    }
    let n = level.iter().copied().max().map_or(0, |m| m + 1);
    let mut waves = vec![Vec::new(); n];
    for (i, lv) in level.into_iter().enumerate() {
        waves[lv].push(i);
    }
    waves
}

fn batch_waves(actions: &[Action]) -> Vec<Vec<usize>> {
    let shapes: Vec<Shape<'_>> = actions.iter().map(shape_of).collect();
    waves_by(&shapes)
}

/// 步骤子 agent 的波次（**同一个内核**，父子不分叉）
pub(crate) fn batch_waves_for_step(actions: &[StepAction]) -> Vec<Vec<usize>> {
    let shapes: Vec<Shape<'_>> = actions.iter().map(shape_of_step).collect();
    waves_by(&shapes)
}

/// 并发跑一组只读调用，结果按**声明顺序**落回各自的槽位。
///
/// 顺序错位比慢更糟：模型会拿 B 文件的内容当 A 的依据，而且它没有任何办法察觉。
///
/// 用 `std::thread::scope` 而不是 tokio 任务：`Ctx::tool_read` 是同步文件 I/O，起任务也只是
/// 在同一执行线程上排队；scoped 线程能直接借 `&Ctx`（不必 `'static`、不必克隆覆盖层），
/// 也不要求 runtime 是多线程 —— 单测跑在 `new_current_thread` 上，那里 `block_in_place` 会直接 panic。
pub(crate) fn read_group(
    ctx: &Ctx<'_>,
    specs: &[ReadSpec],
    parallel: bool,
) -> Vec<Result<String, String>> {
    if specs.len() == 1 || !parallel {
        return specs.iter().map(|s| ctx.tool_read(s)).collect();
    }
    let mut slots: Vec<Option<Result<String, String>>> = (0..specs.len()).map(|_| None).collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = slots
            .iter_mut()
            .zip(specs.iter())
            .map(|(slot, spec)| s.spawn(move || *slot = Some(ctx.tool_read(spec))))
            .collect();
        // 全部 join 掉：线程 panic 时它的槽位留 None（下面统一报"线程异常"），
        // 不 join 的话 `scope` 结束时会自己再抛一次 "scoped thread panicked"
        for h in handles {
            let _ = h.join();
        }
    });
    slots
        .into_iter()
        .map(|s| s.unwrap_or_else(|| Err("并发读取未返回（读取线程异常）".into())))
        .collect()
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// 单动作结果回灌：`{"ok":…, "result"|"error":…}`（历史里模型学过的形状，保持不动）
pub(crate) fn json_result(r: Result<String, String>) -> String {
    match r {
        Ok(v) => format!("{{\"ok\": true, \"result\": {}}}", json_str(&v)),
        Err(e) => format!("{{\"ok\": false, \"error\": {}}}", json_str(&e)),
    }
}

/// 把 write 的两种参数形状都归约成"最终整份内容"。**能力没变，变的只是要传多少东西**：
///
/// - [`WriteBody::Content`] —— 原样透传（老路径语义逐字不变；空内容仍然拒）
/// - [`WriteBody::Edits`] —— 拿**当前**内容逐条锚点替换。匹配规则不在这里重写：
///   [`repair::replace_unique`] 负责"恰好出现一次"与行尾归一化（CRLF 文件能匹配 LF 的 `find`，
///   落盘再还原 CRLF —— 真机踩过 `core.autocrlf` 让 LF 片段一条都匹配不上）
///
/// `cur` = 当前内容（覆盖层优先，`None` = 文件不存在）。锚点编辑只能改**已有**文件：
/// 新建文件的锚点无处可锚，那种场合用 `content` 形态。
pub(crate) fn resolve_write(
    rel: &str,
    body: &WriteBody,
    cur: Option<&str>,
) -> Result<String, String> {
    match body {
        WriteBody::Content(c) => {
            if c.is_empty() {
                return Err("content 为空 —— 不允许静默清空文件".into());
            }
            Ok(c.clone())
        }
        WriteBody::Edits(edits) => {
            let Some(text) = cur else {
                return Err(format!(
                    "{rel} 不存在 —— 锚点编辑只能改已有文件；新建文件请用 content 形态（整份内容）"
                ));
            };
            let mut out = text.to_string();
            for (i, e) in edits.iter().enumerate() {
                if e.find.is_empty() {
                    return Err(format!(
                        "第 {} 条 edit：find 为空（{rel}）—— 锚点编辑要给出要被替换的原文；\
                         要给整份内容请用 content 形态",
                        i + 1
                    ));
                }
                // 同一文件的多条改动**按声明顺序累积**（与 repair::apply_edits 同一语义）
                out = repair::replace_unique(&out, &e.find, &e.replace)
                    .map_err(|why| format!("第 {} 条 edit：{why}（{rel}）", i + 1))?;
            }
            Ok(out)
        }
    }
}

/// 锚点写入成功的回灌文案。必须把"改了几处"说出来：只说"已写入 N 字节"，
/// 模型看不出自己那轮传的是 edits 还是 content，下一轮容易再 verify 一遍。
pub(crate) fn write_edits_ok_text(
    rel: &str,
    count: usize,
    len: usize,
    policy: WritePolicy,
) -> String {
    format!(
        "已改 {rel}：{count} 处锚点替换（改后 {len} 字节）{}",
        if policy == WritePolicy::Stage {
            "（已暂存到 IDE 暂存区，项目磁盘未变，用户确认后才生效）"
        } else {
            ""
        }
    )
}

/// 文件内容"怎么给"：整份走老裁剪，窗口走行切片。
fn read_body(rel: &str, content: &str, spec: &ReadSpec) -> Result<String, String> {
    if !spec.is_window() {
        return Ok(clip(content, READ_CLIP));
    }
    window_of(rel, content, spec.offset, spec.limit)
}

/// 行窗口读：从 `offset`（1-based）起最多 `limit` 行。
///
/// 表头不是装饰，它是这个能力的**一半**：模型要靠"共 N 行 / 还有 M 行，接着读用 offset=X"
/// 才知道缺口在哪、怎么接上。没有它，模型只知道自己拿到了 200 行。
fn window_of(
    rel: &str,
    content: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String, String> {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Ok(format!("（{rel} 是空文件）"));
    }
    let start = offset.unwrap_or(1);
    if start > total {
        // 越界当场说清：静默返回最后一行会让模型以为"中间没内容"
        return Err(format!(
            "offset={start} 越界：{rel} 只有 {total} 行（行号从 1 起）"
        ));
    }
    let asked = limit.unwrap_or(READ_WINDOW_DEFAULT_LINES);
    let take = asked.min(READ_WINDOW_MAX_LINES);
    let end = (start - 1 + take).min(total);
    let body = lines[start - 1..end].join("\n");
    let tail = if end < total {
        format!("；还有 {} 行，接着读用 offset={}", total - end, end + 1)
    } else {
        "；已到文件末尾".to_string()
    };
    let clamped = if asked > READ_WINDOW_MAX_LINES {
        format!("（请求 {asked} 行，一次窗口上限 {READ_WINDOW_MAX_LINES} 行）")
    } else {
        String::new()
    };
    Ok(format!(
        "--- {rel}（第 {start}-{end} 行 / 共 {total} 行{tail}）{clamped}---\n{}",
        clip(&body, READ_CLIP)
    ))
}

/// 写入成功的回灌文案。**单动作与批并发两条路径共用**：确认模式必须把"磁盘没变"说清，
/// 只说"已暂存"模型仍会去 execute 里找它的改动。
pub(crate) fn write_ok_text(rel: &str, len: usize, policy: WritePolicy) -> String {
    format!(
        "已写入 {rel}（{len} 字节）{}",
        if policy == WritePolicy::Stage {
            "（已暂存到 IDE 暂存区，项目磁盘未变，用户确认后才生效）"
        } else {
            ""
        }
    )
}

/// 一次写入的**磁盘阶段**：先备份被覆盖的原文件，再写目标。
///
/// 纯磁盘操作、只吃 `&Path`，所以能在并发波里跑；"写入怎么落盘"全仓只允许这一个实现
/// （单动作路径走 [`Ctx::flush_one`]，它委托到这里）。
pub(crate) fn flush_write_disk(
    proj: &Path,
    backup_dir: Option<&Path>,
    rel: &str,
    before: Option<&str>,
    after: &str,
) -> Result<(), String> {
    if let (Some(dir), Some(before)) = (backup_dir, before) {
        let to = dir.join(rel);
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&to, before).map_err(|e| format!("备份 {rel} 失败：{e}（已中止写入）"))?;
    }
    let target = proj.join(rel);
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&target, after).map_err(|e| format!("写入 {rel} 失败：{e}"))
}

/// 最简 `join_all`：在**同一个任务**里并发推进一组 future（引擎不为此引 `futures` 依赖）。
///
/// 为什么不是 `tokio::spawn`：这些 future 借的是 `&dyn Connector`，非 `'static`，spawn 装不下；
/// 也不是 `tokio::join!`：它元数编译期固定，装不下运行时才知道长度的列表。
struct JoinAll<'a, T> {
    futs: Vec<ConnectFuture<'a, T>>,
    out: Vec<Option<Result<T, String>>>,
}

impl<'a, T> JoinAll<'a, T> {
    fn new(futs: Vec<ConnectFuture<'a, T>>) -> Self {
        let out = futs.iter().map(|_| None).collect();
        Self { futs, out }
    }
}

impl<T: Unpin> Future for JoinAll<'_, T> {
    type Output = Vec<Result<T, String>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `Pin<Box<dyn Future>>` 与 `Option<..>` 都是 `Unpin`，所以这里能安全拿 `&mut`
        let this = self.get_mut();
        let mut pending = 0;
        for (i, f) in this.futs.iter_mut().enumerate() {
            if this.out[i].is_some() {
                continue;
            }
            match f.as_mut().poll(cx) {
                Poll::Ready(r) => this.out[i] = Some(r),
                Poll::Pending => pending += 1,
            }
        }
        if pending == 0 {
            Poll::Ready(
                this.out
                    .iter_mut()
                    .map(|o| o.take().unwrap_or_else(|| Err("join 状态丢失".into())))
                    .collect(),
            )
        } else {
            Poll::Pending
        }
    }
}

/// 一批结果回灌：`results` 数组**按声明顺序**，逐条带调用摘要与自己的 ok。
///
/// 顶层 `ok` 只是"全成"的汇总 —— 模型拿它当"这一轮有没有事"会漏掉其中一条失败，
/// 所以每条都带 `ok`，并在 note 里点明顺序与执行方式（并发/串行）的关系。
pub(crate) fn batch_json_result(items: &[CallResult]) -> String {
    let all_ok = items.iter().all(|(_, _, r)| r.is_ok());
    let results: Vec<serde_json::Value> = items
        .iter()
        .enumerate()
        .map(|(i, (tool, brief, r))| {
            let mut o = serde_json::json!({
                "index": i + 1,
                "tool": tool,
                "call": brief,
                "ok": r.is_ok(),
            });
            match r {
                Ok(v) => o["result"] = serde_json::Value::String(v.clone()),
                Err(e) => o["error"] = serde_json::Value::String(e.clone()),
            }
            o
        })
        .collect();
    serde_json::json!({
        "ok": all_ok,
        "results": results,
        "note": "以上是同一轮里的多个调用（一批并发跑；只有同一条路径上的写与读、以及托管进程的起停查会按你给的顺序排），results 按你声明的顺序排列。",
    })
    .to_string()
}

/// 一个动作 → `(tool, brief, result)`。批与单动作走**同一份**实现：
/// 两条派发路径必然分叉（老代码里解析与执行就分在两处 `match`），这里只留一条。
async fn exec_one(
    cfg: &AppConfig,
    proj: &Path,
    policy: WritePolicy,
    conn: &dyn Connector,
    ctx: &mut Ctx<'_>,
    action: Action,
) -> (String, String, Result<String, String>) {
    match action {
        Action::Read(spec) => {
            let brief = format!("read {}", spec.brief());
            ("read".into(), brief, ctx.tool_read(&spec))
        }
        Action::Write(spec) => {
            let brief = spec.brief();
            let r = ctx.apply_write(&spec);
            ("write".into(), brief, r)
        }
        Action::Execute(cmd, t) => {
            let brief = format!("execute {}", clip(&cmd, 80));
            let mut r = tool_execute(proj, &cmd, t);
            // 确认模式 + 已有暂存改动：这条命令必然看不到本次修改，贴一行说明兜住
            r.push_str(staged_execute_note(policy, !ctx.changes.is_empty()));
            ("execute".into(), brief, Ok(r))
        }
        Action::ExecBg(spec) => {
            let brief = format!("execute bg {}", clip(&spec.cmd, 70));
            ("execute".into(), brief, tool_exec_bg(proj, cfg, &spec))
        }
        Action::Proc(op, handle) => {
            let brief = format!("execute {} {handle}", proc_op_name(op));
            ("execute".into(), brief, tool_proc(op, &handle))
        }
        Action::Connect(ca) => connect_step(conn, ca).await,
        // 控制动作进不了批（parse_actions 已当面拒）；这条分支只为让 match 穷尽
        Action::Plan(_) | Action::Final(_) | Action::Ask(_) => (
            "control".into(),
            "控制动作".into(),
            Err("plan / final / ask_user 不能与调用同批执行".into()),
        ),
    }
}

/// 一波（波内互不冲突）的**并发执行**，结果按索引写回 `slots`。
///
/// 落地方式按原语分：
/// - 只读：`read_group`（scoped 线程借 `&Ctx`，同步文件 I/O，不占 runtime）
/// - 写入：**磁盘部分**在 scoped 线程里并行（冲突规则保证同波不同路径），记账回主线程按声明
///   顺序补 —— `Ctx` 的覆盖层与变更表只有一份内存状态，不能并发改
/// - 执行 / 托管进程：每个线程自己起进程（`tool_execute` / `tool_exec_bg` / `tool_proc` 都只吃
///   `&Path` 与 `&AppConfig`，与 `Ctx` 无关）
/// - 连接：同一任务里 [`JoinAll`]（future 借 `&dyn Connector`，非 `'static`，spawn 装不下）
///
/// 波内没有依赖，所以"谁先谁后"无所谓：只读先跑完再起写/执行，纯粹是为了复用同一份并发读实现。
/// 参数多：这是"把一批调用按波并发跑"的执行口，它本来就要同时知道配置、项目、策略、连接、
/// 覆盖层与结果槽位（与本模块其它执行函数同款，见 `#[allow(too_many_arguments)]` 的既有用法）。
#[allow(clippy::too_many_arguments)]
async fn run_wave(
    cfg: &AppConfig,
    proj: &Path,
    policy: WritePolicy,
    conn: &dyn Connector,
    ctx: &mut Ctx<'_>,
    actions: &[Action],
    wave: &[usize],
    slots: &mut [Option<CallResult>],
) {
    // 一行回退：波内也不并发（仍是一批一次往返，只是按声明顺序串行）
    if !cfg.agent.batch_parallel {
        for &i in wave {
            slots[i] = Some(exec_one(cfg, proj, policy, conn, &mut *ctx, actions[i].clone()).await);
        }
        return;
    }

    let mut reads: Vec<(usize, ReadSpec)> = Vec::new();
    let mut writes: Vec<(usize, String, Option<String>, String)> = Vec::new();
    let mut execs: Vec<(usize, String, Option<u64>)> = Vec::new();
    let mut bgs: Vec<(usize, crate::proc::StartSpec)> = Vec::new();
    let mut procs: Vec<(usize, ProcOp, String)> = Vec::new();
    let mut conn_briefs: Vec<(usize, String)> = Vec::new();
    let mut conn_futs: Vec<ConnectFuture<'_, String>> = Vec::new();

    for &i in wave {
        match &actions[i] {
            Action::Read(spec) => reads.push((i, spec.clone())),
            Action::Write(spec) => match safe_rel_path(&spec.path) {
                // `before` 与"归约成整份内容"都在主线程做（要碰覆盖层：edits 得先读到当前内容）
                // —— 线程里只做磁盘
                Ok(rel) => {
                    let before = ctx.before_of(&rel);
                    match resolve_write(&rel, &spec.body, before.as_deref()) {
                        Ok(after) => writes.push((i, rel, before, after)),
                        Err(e) => slots[i] = Some(("write".into(), spec.brief(), Err(e))),
                    }
                }
                Err(e) => slots[i] = Some(("write".into(), spec.brief(), Err(e))),
            },
            Action::Execute(cmd, t) => execs.push((i, cmd.clone(), *t)),
            Action::ExecBg(spec) => bgs.push((i, spec.clone())),
            Action::Proc(op, h) => procs.push((i, *op, h.clone())),
            Action::Connect(ca) => {
                conn_briefs.push((i, connect_brief(ca)));
                conn_futs.push(connect_future(conn, ca.clone()));
            }
            // 控制动作进不了多人波（parse_actions 已拒批里的 plan / final）
            Action::Plan(_) | Action::Final(_) | Action::Ask(_) => {
                slots[i] = Some((
                    "control".into(),
                    "控制动作".into(),
                    Err("plan / final / ask_user 不能与调用同波执行".into()),
                ));
            }
        }
    }

    // 只读：复用同一份并发读（顺序按声明落位）
    if !reads.is_empty() {
        let specs: Vec<ReadSpec> = reads.iter().map(|(_, s)| s.clone()).collect();
        for ((i, spec), r) in reads.iter().zip(read_group(ctx, &specs, true)) {
            slots[*i] = Some(("read".to_string(), format!("read {}", spec.brief()), r));
        }
    }

    // Apply 模式要覆盖已有文件 → 备份目录在主线程备好（线程里不改 `Ctx`）
    let bdir = if policy == WritePolicy::Apply && writes.iter().any(|(_, _, b, _)| b.is_some()) {
        ctx.ensure_backup_dir(true)
    } else {
        ctx.backup_dir.clone()
    };
    // 贴"暂存改动"横幅用的判据（线程里读 `Ctx` 不方便，提前取）
    let text_changes = !ctx.changes.is_empty();

    let mut write_out: Vec<(usize, Result<(), String>)> = Vec::new();
    let mut exec_out: Vec<(usize, Result<String, String>)> = Vec::new();
    let mut bg_out: Vec<(usize, Result<String, String>)> = Vec::new();
    let mut proc_out: Vec<(usize, Result<String, String>)> = Vec::new();
    std::thread::scope(|s| {
        let hw: Vec<_> = writes
            .iter()
            .map(|(i, rel, before, after)| {
                let (i, rel, before, after) = (*i, rel.clone(), before.clone(), after.clone());
                let bdir = bdir.clone();
                (
                    i,
                    s.spawn(move || {
                        flush_write_disk(proj, bdir.as_deref(), &rel, before.as_deref(), &after)
                    }),
                )
            })
            .collect();
        let he: Vec<_> = execs
            .iter()
            .map(|(i, cmd, t)| {
                let (i, cmd, t) = (*i, cmd.clone(), *t);
                (i, s.spawn(move || tool_execute(proj, &cmd, t)))
            })
            .collect();
        let hb: Vec<_> = bgs
            .iter()
            .map(|(i, spec)| {
                let (i, spec) = (*i, spec.clone());
                (i, s.spawn(move || tool_exec_bg(proj, cfg, &spec)))
            })
            .collect();
        let hp: Vec<_> = procs
            .iter()
            .map(|(i, op, handle)| {
                let (i, op, handle) = (*i, *op, handle.clone());
                (i, s.spawn(move || tool_proc(op, &handle)))
            })
            .collect();
        for (i, h) in hw {
            write_out.push((i, h.join().unwrap_or_else(|_| Err("写入线程异常".into()))));
        }
        for (i, h) in he {
            // `tool_execute` 的失败信息是**嵌在文本里**的（不是 Err）：模型需要读到命令的
            // 实际输出才能改；只有"线程都没回来"才算这一条没结果
            exec_out.push((
                i,
                Ok(h.join().unwrap_or_else(|_| "执行线程异常（未返回）".into())),
            ));
        }
        for (i, h) in hb {
            bg_out.push((i, h.join().unwrap_or_else(|_| Err("启动线程异常".into()))));
        }
        for (i, h) in hp {
            proc_out.push((i, h.join().unwrap_or_else(|_| Err("句柄线程异常".into()))));
        }
    });

    // 写入：磁盘已落 → 这里按**声明顺序**记账（内存状态只有一份）
    for ((i, rel, before, after), (_, r)) in writes.iter().zip(write_out) {
        // 摘要用**调用方声明的形状**（content 报字节、edits 报几处替换）——
        // 只说"写成了 N 字节"会让模型看不出自己那轮传的是哪种形态
        let brief = match &actions[*i] {
            Action::Write(spec) => spec.brief(),
            _ => format!("write {rel}（{} 字节）", after.len()),
        };
        let res = match r {
            Ok(()) => {
                ctx.record(rel.clone(), before.clone(), after.clone());
                Ok(write_ok_text(rel, after.len(), policy))
            }
            Err(e) => Err(e),
        };
        slots[*i] = Some(("write".into(), brief, res));
    }
    for ((i, cmd, _), (_, r)) in execs.iter().zip(exec_out) {
        // 确认模式 + 已有暂存改动：这条命令看不到本次修改，逐条贴一行说明
        let r = r.map(|txt| format!("{txt}{}", staged_execute_note(policy, text_changes)));
        slots[*i] = Some(("execute".into(), format!("execute {}", clip(cmd, 80)), r));
    }
    for ((i, spec), (_, r)) in bgs.iter().zip(bg_out) {
        let brief = format!("execute bg {}", clip(&spec.cmd, 70));
        slots[*i] = Some(("execute".into(), brief, r));
    }
    for ((i, op, handle), (_, r)) in procs.iter().zip(proc_out) {
        let brief = format!("execute {} {handle}", proc_op_name(*op));
        slots[*i] = Some(("execute".into(), brief, r));
    }
    // 连接：同一任务里并发推进
    if !conn_futs.is_empty() {
        for ((i, brief), r) in conn_briefs.iter().zip(JoinAll::new(conn_futs).await) {
            slots[*i] = Some(("connect".into(), brief.clone(), r));
        }
    }
}

/// 正整数字段（`offset` / `limit` 这类）。0 与负数都当场拒 ——
/// 静默收下 0 会让模型拿到空结果，然后以为"文件是空的"，比报错难查得多。
fn get_pos_usize(args: &serde_json::Value, key: &str) -> Result<Option<usize>, String> {
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
fn parse_read(args: &serde_json::Value) -> Result<Action, String> {
    let path = get_str(args, "path")?;
    let offset = get_pos_usize(args, "offset")?;
    let limit = get_pos_usize(args, "limit")?;
    Ok(Action::Read(ReadSpec {
        path,
        offset,
        limit,
    }))
}

/// `write` 的两副面孔收口在这一处：`content`（整份）或 `edits`（锚点）。
///
/// **两个都给 = 当面拒**，不猜哪个优先：模型给两套互相矛盾的意图时，猜错就是静默改错文件。
fn parse_write(args: &serde_json::Value) -> Result<Action, String> {
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
fn parse_execute(args: &serde_json::Value) -> Result<Action, String> {
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
    Write(WriteSpec),
    Execute(String, Option<u64>),
    ExecBg(crate::proc::StartSpec),
    Proc(ProcOp, String),
    Unsupported(&'static str),
}

/// 主循环动作 → 步骤动作（plan / connect 收窄成「不支持」）
fn to_step_action(a: Action) -> StepAction {
    match a {
        Action::Final(t) => StepAction::Final(t),
        Action::Read(s) => StepAction::Read(s),
        Action::Write(s) => StepAction::Write(s),
        Action::Execute(c, t) => StepAction::Execute(c, t),
        Action::ExecBg(s) => StepAction::ExecBg(s),
        Action::Proc(op, h) => StepAction::Proc(op, h),
        Action::Plan(_) => StepAction::Unsupported("plan"),
        Action::Connect(_) => StepAction::Unsupported("connect"),
        // 子步没有交互权：它的 messages 是干净上下文，一问就破了"一轮 = 一步"的派发语义
        Action::Ask(_) => StepAction::Unsupported("ask_user"),
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
fn action_name(a: &Action) -> &'static str {
    match a {
        Action::Final(_) => "final",
        Action::Plan(_) => "plan",
        Action::Read(_) => "read",
        Action::Write(_) => "write",
        Action::Execute(..) | Action::ExecBg(_) | Action::Proc(..) => "execute",
        Action::Connect(_) => "connect",
        Action::Ask(_) => "ask_user",
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
fn recognize_tool(v: &serde_json::Value) -> Option<String> {
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
fn args_shape_tool(v: &serde_json::Value) -> Option<&'static str> {
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

// ============================================
// 工具执行（项目目录 + 覆盖层；路径全部封闭在项目内）
// ============================================

/// 覆盖层：本会话已写/改的内容。read 优先读它（模型改完能读回自己的修改），
/// Stage 策略下磁盘未动也能保持一致的视图。
///
/// 步骤执行体（`crate::step_agent`）借的是**同一份** `&mut Ctx`，不是自己的副本：
/// `emit_step_progress` / `gate_before_final` / `flush_stage` 全依赖这一份 overlay 与
/// changes，各持一份就得写 merge 与冲突处理，而收益只有"父能看到中间态"。
///
/// 它是 `pub` 只因为出现在 `step_agent::run_step` 的签名里；字段与工具方法仍然是
/// crate 内可见 —— 外部拿不到一份可用的上下文，工具循环的唯一入口是 [`run`]。
pub struct Ctx<'a> {
    proj: &'a Path,
    overlay: BTreeMap<String, String>,
    changes: Vec<FileChange>,
    /// 本轮**命令取证**的留痕（依据核对的证据集合之一；复核员会看到它）
    probes: Vec<Probe>,
    policy: WritePolicy,
    backup_dir: Option<PathBuf>,
    /// **项目状态根**（宿主注入；暂存与备份落这里，绝不落进项目）。
    /// `None` = 走兜底（临时目录）—— 引擎单测与"宿主没注入"的情况都走它。
    state_root: Option<PathBuf>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(proj: &'a Path, policy: WritePolicy) -> Self {
        Self {
            proj,
            overlay: BTreeMap::new(),
            changes: Vec::new(),
            probes: Vec::new(),
            policy,
            backup_dir: None,
            state_root: None,
        }
    }

    /// 项目状态根：注入优先，否则兜底临时目录（**绝不回落进项目**）。
    pub(crate) fn state_root(&self) -> PathBuf {
        self.state_root
            .clone()
            .unwrap_or_else(|| crate::config::fallback_state_root(self.proj))
    }

    /// 宿主注入项目状态根（`<便携根>/projects/<项目 key>`）：暂存与备份都写到那儿。
    pub(crate) fn with_state_root(mut self, dir: PathBuf) -> Self {
        self.state_root = Some(dir);
        self
    }

    /// 本会话改了哪些文件（步骤执行体按"本步写过的路径"筛自己那部分）
    pub(crate) fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    /// 本会话跑过哪些命令、各自输出了什么（依据核对的证据集合）
    pub(crate) fn probes(&self) -> &[Probe] {
        &self.probes
    }

    /// 留一次取证。两端都要裁剪：命令可能特长、输出可能是一份几千行的编译日志，
    /// 而它是要进复核员上下文的。
    pub(crate) fn note_probe(&mut self, cmd: &str, output: &str) {
        self.probes.push(Probe {
            cmd: clip(cmd, PROBE_CMD_CLIP),
            output: clip(output, PROBE_OUT_CLIP),
        });
    }

    /// 项目根（execute 的工作目录）
    pub(crate) fn project_root(&self) -> &Path {
        self.proj
    }

    /// 本会话的写入策略。子步骤执行体（`crate::step_agent`）借同一份 `Ctx`，
    /// 要靠它决定要不要把"确认模式：execute 看不到你的改动"贴进自己的上下文。
    pub(crate) fn policy(&self) -> WritePolicy {
        self.policy
    }

    /// read：目录给结构树（复用 repair 的列表逻辑），文件给内容；"." = 项目根。
    ///
    /// `spec` 带 `offset`/`limit` 时只取那一段行窗口（[`ReadSpec::is_window`]）——
    /// 让模型不必为读 3000 行文件里中间那 40 行而付整份 token。
    /// 不带窗口时与老行为**逐字一致**（同一次 `clip`）。
    pub(crate) fn tool_read(&self, spec: &ReadSpec) -> Result<String, String> {
        let raw = spec.path.as_str();
        let raw_trim = raw.trim();
        if raw_trim == "." || raw_trim == "./" {
            // 项目根结构：read "." 是模型探索项目的第一步
            // （目录给的本来就是结构树，窗口参数在这里无意义 —— 忽略，不报错换一轮白跑）
            let mut listing = String::from("--- 项目根目录结构 ---\n");
            let mut n = 0usize;
            repair::list_dir(self.proj, "", 0, &mut n, &mut listing);
            return Ok(listing);
        }
        let rel = safe_rel_path(raw)?;
        if let Some(c) = self.overlay.get(&rel) {
            let body = read_body(&rel, c, spec)?;
            return Ok(format!("（{rel} 的当前内容 = 本会话已暂存的修改）\n{body}"));
        }
        let p = self.proj.join(&rel);
        if p.is_dir() {
            Ok(repair::gather_context(
                self.proj,
                std::slice::from_ref(&rel),
                READ_CLIP,
            ))
        } else if p.is_file() {
            let content =
                std::fs::read_to_string(&p).unwrap_or_else(|e| format!("<读取失败：{e}>"));
            let body = read_body(&rel, &content, spec)?;
            Ok(format!("--- {rel} ---\n{body}"))
        } else {
            Err(format!(
                "{rel} 不存在（项目根目录用 read \".\" 看整体结构）"
            ))
        }
    }

    /// write 的整份形态（老形状，逐字保留）。生产路径走 [`Ctx::apply_write`]，
    /// 这条是测试与调用方直连的入口。
    #[cfg(test)]
    pub(crate) fn tool_write(&mut self, path: &str, content: &str) -> Result<String, String> {
        self.apply_write(&WriteSpec::content(path, content))
    }

    /// write 的锚点形态：只传改动，不重发全文
    #[cfg(test)]
    pub(crate) fn tool_edit(
        &mut self,
        path: &str,
        edits: Vec<AgentEdit>,
    ) -> Result<String, String> {
        self.apply_write(&WriteSpec {
            path: path.to_string(),
            body: WriteBody::Edits(edits),
        })
    }

    /// write 的**唯一入口**：两种参数形状都在 [`resolve_write`] 里归约成"最终整份内容"，
    /// 再走同一条记账/落盘通道。
    ///
    /// 为什么不直接调 [`repair::apply_edits`]：那个函数**自己写盘**，会绕过覆盖层与
    /// 备份/暂存记账 —— 确认模式下磁盘本来就不该动（那是"read 看到新内容、execute 看到旧内容"
    /// 那场 65 轮误侦察的根）。共用的只是**匹配规则**（[`repair::replace_unique`]），
    /// 落盘通道仍然只有 [`Ctx::commit_with`] 一条。
    pub(crate) fn apply_write(&mut self, spec: &WriteSpec) -> Result<String, String> {
        let rel = safe_rel_path(&spec.path)?;
        let before = self.before_of(&rel);
        let after = resolve_write(&rel, &spec.body, before.as_deref())?;
        let len = after.len();
        let reply = match &spec.body {
            WriteBody::Content(_) => write_ok_text(&rel, len, self.policy),
            WriteBody::Edits(edits) => write_edits_ok_text(&rel, edits.len(), len, self.policy),
        };
        self.commit_with(rel, before, after)?;
        Ok(reply)
    }

    /// 记录变更 + 更新覆盖层；Apply 策略同步落盘。
    /// `before` 由调用方给：它往往已经为了别的目的读过一遍当前内容，这里再读一次是白付一次 I/O。
    fn commit_with(
        &mut self,
        rel: String,
        before: Option<String>,
        after: String,
    ) -> Result<(), String> {
        if self.policy == WritePolicy::Apply {
            let bdir = self.ensure_backup_dir(before.is_some());
            flush_write_disk(self.proj, bdir.as_deref(), &rel, before.as_deref(), &after)?;
        }
        self.record(rel, before, after);
        Ok(())
    }

    /// 取被覆盖前的原内容（覆盖层优先：同 run 内的二次写以首次记录为准）。
    /// 只读 `&self` —— 所以并发波的线程里也能取。
    pub(crate) fn before_of(&self, rel: &str) -> Option<String> {
        self.overlay
            .get(rel)
            .cloned()
            .or_else(|| std::fs::read_to_string(self.proj.join(rel)).ok())
    }

    /// 备份目录（Apply 模式覆盖已有文件时才需要）。`need` 为假则返回当前值。
    /// 必须在**主线程**调（它改 `Ctx`），线程里只读 [`Ctx::backup_dir`] 的克隆。
    pub(crate) fn ensure_backup_dir(&mut self, need: bool) -> Option<PathBuf> {
        if need {
            // 备份落在**项目状态根**里（v1.0.0 起不在用户仓库里）
            let d = self
                .state_root()
                .join("backups")
                .join(format!("agent-{}", crate::workspace::now_compact()));
            let _ = std::fs::create_dir_all(&d);
            self.backup_dir.get_or_insert(d);
        }
        self.backup_dir.clone()
    }

    /// 当前备份目录的只读访问（并发波里要用它落备份，但线程不许改 `Ctx`）
    pub(crate) fn backup_dir_path(&self) -> Option<PathBuf> {
        self.backup_dir.clone()
    }

    /// 写入的**记账阶段**：只动内存（overlay / changes），磁盘已由 [`flush_write_disk`] 落过了。
    /// 所以并发波里写完磁盘后，可以在主线程按声明顺序补这一步。
    pub(crate) fn record(&mut self, rel: String, before: Option<String>, after: String) {
        if let Some(existing) = self.changes.iter_mut().find(|c| c.path == rel) {
            existing.after = after.clone();
        } else {
            self.changes.push(FileChange {
                path: rel.clone(),
                kind: if before.is_some() {
                    "modify".into()
                } else {
                    "add".into()
                },
                before,
                after: after.clone(),
            });
        }
        self.overlay.insert(rel, after);
    }

    /// Stage 策略收尾：把覆盖层写到暂存目录，返回目录路径
    fn flush_stage(&self) -> Result<PathBuf, String> {
        // 暂存落在**项目状态根**里（v1.0.0 起不在用户仓库里）
        let dir = self
            .state_root()
            .join("stage")
            .join(format!("agent-{}", crate::workspace::now_compact()));
        for c in &self.changes {
            let to = dir.join("files").join(&c.path);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建暂存目录失败：{e}"))?;
            }
            std::fs::write(&to, &c.after).map_err(|e| format!("暂存 {} 失败：{e}", c.path))?;
        }
        let manifest = serde_json::to_string_pretty(&self.changes)
            .map_err(|e| format!("序列化暂存清单失败：{e}"))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建暂存目录失败：{e}"))?;
        std::fs::write(dir.join("manifest.json"), manifest)
            .map_err(|e| format!("写暂存清单失败：{e}"))?;
        Ok(dir)
    }
}

/// execute 的破坏性模式拒绝清单。这是绊线不是沙箱 —— 真正的隔离在 verify 的 docker 沙箱；
/// Agent 的 execute 面向用户自己的项目目录，只拦"不可逆的系统级破坏"。
pub(crate) fn execute_allowed(cmd: &str) -> Result<(), String> {
    let c = cmd.to_ascii_lowercase();
    for pat in [
        "rm -rf /",
        "rm -rf /*",
        "rm -rf ~",
        "mkfs",
        "shutdown",
        "reboot",
        "del /s /q",
        "rd /s /q",
        "format c:",
    ] {
        if c.contains(pat) {
            return Err(format!(
                "命令被拒绝（破坏性模式 {pat:?}）。execute 的作用域是当前项目，不允许不可逆的系统级操作。"
            ));
        }
    }
    Ok(())
}

/// shell **内置**命令：`where` / `command -v` 查不到，但绝不是"命令不存在"。
/// 表而不是分支 —— 与 `discover::TOOLS` 同一哲学：无界的那一类靠数据长，不靠加 if。
const SHELL_BUILTINS: &[&str] = &[
    // cmd.exe
    "cd", "chdir", "dir", "echo", "set", "type", "copy", "move", "del", "erase", "md", "mkdir",
    "rd", "rmdir", "cls", "title", "ver", "date", "time", "exit", "call", "shift", "if", "for",
    "goto", "pause", "pushd", "popd", "assoc", "ftype", "color", "prompt", "rem",
    // sh / bash
    "export", "source", "test", "true", "false", "pwd", "unset", "alias", "read", "umask", "local",
    "return", "printf", "ulimit", "trap", "exec", "eval", "wait", "kill", "jobs", "bg", "fg", ".",
];

/// "把执行交给外壳"的启动器：**输出脱离捕获**（stdout / stderr 一律拿不到），
/// 真控制台里还会把"找不到"变成桌面弹窗。
const LAUNCHERS: &[&str] = &["start", "explorer", "rundll32", "open", "xdg-open"];

/// 命令文本里出现这些就拒：PowerShell 的外壳转发同样脱离捕获。
const SHELL_ESCAPES: &[&str] = &["start-process", "invoke-item"];

/// execute 的**执行前闸门**：把"确定是垃圾"的命令挡在 shell 之外，并回一条可读的纠正。
///
/// 为什么要有：execute 是模型唯一的写入口，一次瞎试就是一轮。实测 run
/// `agent-20260920-152312` 里模型为"mvn / java 到底存不存在"空转 5~6 轮；而 `\`、
/// `\admin-run\` 这类**根本不是命令**的字符串也会被原样交给 cmd —— 模型只看到一行它
/// 读不懂的报错（GBK 乱码，见 [`exec::decode_output`]），于是换个更离谱的继续试。
///
/// **保守原则**：只拦三类，拿不准一律放行。闸门误杀一条合法命令的代价，
/// 远大于放过去一条垃圾命令。
///
/// 与 discover 的关系：同一张工具表，两种用法 —— 探测结果既喂上下文
/// （[`discover::render_note`]），也在这里当**验证器**。
pub(crate) fn preflight_execute(proj: &Path, cmd: &str) -> Result<(), String> {
    // ① 启动器 / 外壳转发：必然拿不到输出，真控制台还会弹窗
    if let Some(l) = launcher_of(cmd) {
        return Err(format!(
            "❌ 这条命令没有执行：`{l}` 会把执行交给外壳 —— stdout / stderr 一律拿不到，\
             出错还会在桌面上弹窗。请直接运行程序本身（例：`mvn -v`、`java -jar app.jar`）；\
             确实需要新窗口时，把这条命令写进最终答复让用户手动跑。"
        ));
    }
    let Some(tok) = first_token(cmd) else {
        return Err("❌ 空命令。".into());
    };
    // ② 路径式写法，但本机与项目里都没有这个文件
    if path_like(&tok) {
        if path_exists(proj, &tok) {
            return Ok(());
        }
        return Err(format!(
            "❌ 这条命令没有执行：`{tok}` 是路径写法，但本机与项目里都没有这个文件。\n\
             项目根：{}\n\
             要跑程序就写命令名（例：`mvn -v`）；要跑项目里的脚本就写**存在的**相对路径。\n{}",
            proj.display(),
            available_hint(proj)
        ));
    }
    // ③ 本机没有这个命令（先排除 shell 内置与项目内脚本）
    if !SHELL_BUILTINS.contains(&tok.as_str())
        && !path_exists(proj, &tok)
        && !discover::is_available(&tok)
    {
        return Err(format!(
            "❌ 这条命令没有执行：本机没有 `{tok}`。别猜工具名，先看这份实测清单。\n{}",
            available_hint(proj)
        ));
    }
    Ok(())
}

/// 极简命令行切分：按空白切，引号内的空白不算分隔符。
/// 够闸门用（不做转义与变量展开）—— 但**必须**认引号，否则
/// `"C:\Program Files\Git\bin\bash.exe"` 会被切成两半而误判。
fn tokens(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q: Option<char> = None;
    for ch in cmd.chars() {
        match q {
            Some(qc) => {
                if ch == qc {
                    q = None;
                } else {
                    cur.push(ch);
                }
            }
            None if ch == '"' || ch == '\'' => q = Some(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 首个**有意义的** token：跳过前置环境赋值（`FOO=bar prog`）与纯操作符（`& prog`）。
fn first_token(cmd: &str) -> Option<String> {
    tokens(cmd).into_iter().find(|t| {
        !matches!(t.as_str(), "&" | "&&" | "|" | "||" | "(" | ")")
            && !(t.contains('=') && !t.starts_with('-') && !path_like(t))
    })
}

/// 带路径分隔符 = 模型在写路径（而不是命令名）
fn path_like(tok: &str) -> bool {
    tok.contains('\\') || tok.contains('/')
}

/// 这个路径（项目相对 / 绝对）真的指向一个可执行文件吗。
/// 覆盖三件事：绝对路径、项目内相对路径、以及项目内 `gradlew` / `mvnw`
/// 这类**不带扩展名**、靠 `.cmd` / `.bat` 落地的包装脚本。
fn path_exists(proj: &Path, tok: &str) -> bool {
    let p = Path::new(tok);
    if p.file_name().is_none() {
        return false; // 纯分隔符 / 盘根：不是可执行的东西
    }
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        proj.join(p)
    };
    if abs.is_file() {
        return true;
    }
    if p.extension().is_none() {
        for ext in [".cmd", ".bat", ".exe", ".ps1", ".sh"] {
            if proj.join(format!("{tok}{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// 首 token（含 `cmd /C start …` 这种包一层的情况）是启动器就返回它。
fn launcher_of(cmd: &str) -> Option<String> {
    let low = cmd.to_ascii_lowercase();
    if let Some(e) = SHELL_ESCAPES.iter().find(|e| low.contains(**e)) {
        return Some((*e).to_string());
    }
    let toks = tokens(cmd);
    let mut i = 0;
    if toks.len() >= 2
        && matches!(
            toks[0].to_ascii_lowercase().as_str(),
            "cmd" | "cmd.exe" | "sh" | "bash" | "bash.exe"
        )
        && matches!(toks[1].to_ascii_lowercase().as_str(), "/c" | "-c")
    {
        i = 2;
    }
    let t = toks.get(i)?;
    let base = t.rsplit(['\\', '/']).next().unwrap_or(t.as_str());
    let name = base.to_ascii_lowercase();
    let name = name
        .strip_suffix(".exe")
        .unwrap_or(name.as_str())
        .to_string();
    LAUNCHERS.contains(&name.as_str()).then_some(name)
}

/// 拒绝命令时附上"那有什么" —— 光说"这条不存在"治不了空转。
fn available_hint(proj: &Path) -> String {
    let names = discover::available_names(proj);
    if names.is_empty() {
        return "（本机没探到可用命令；先看看上下文里的『本机命令』段）".to_string();
    }
    format!("本机可用：{}", names.join(" / "))
}

fn run_shell(proj: &Path, cmd: &str, timeout: Duration) -> exec::CmdOutput {
    #[cfg(target_os = "windows")]
    return exec::run(proj, "cmd", &["/C", cmd], timeout, &[]);
    #[cfg(not(target_os = "windows"))]
    return exec::run(proj, "sh", &["-c", cmd], timeout, &[]);
}

pub(crate) fn tool_execute(proj: &Path, cmd: &str, timeout_secs: Option<u64>) -> String {
    // 闸门在破坏性模式之前：先判"这条命令有没有意义"，再判"它危不危险"。
    if let Err(e) = preflight_execute(proj, cmd) {
        return e;
    }
    if let Err(e) = execute_allowed(cmd) {
        return format!("❌ {e}");
    }
    let t = Duration::from_secs(
        timeout_secs
            .unwrap_or(EXEC_DEFAULT_TIMEOUT_SECS)
            .clamp(EXEC_MIN_TIMEOUT_SECS, EXEC_MAX_TIMEOUT_SECS),
    );
    let out = run_shell(proj, cmd, t);
    let mut s = format!(
        "exit={} 耗时 {}ms{}",
        out.exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into()),
        out.duration_ms,
        if out.timed_out {
            "（超时被杀）"
        } else {
            ""
        }
    );
    if let Some(e) = &out.spawn_error {
        s.push_str(&format!("\n启动失败: {e}"));
    }
    if !out.stdout.trim().is_empty() {
        s.push_str(&format!("\nstdout:\n{}", clip(&out.stdout, EXEC_CLIP)));
    }
    if !out.stderr.trim().is_empty() {
        s.push_str(&format!("\nstderr:\n{}", clip(&out.stderr, EXEC_CLIP)));
    }
    s
}

/// 把一次后台启动的结果渲染给模型。**每个出口都带证据**：判据命中的那行 / 退出码 +
/// 日志尾 + 判据最后结果 / 判据未命中 + 日志尾。
///
/// 另外两件必须说清的事：① 日志在哪、后续怎么操作（不然模型还得猜）；
/// ② **如实记账** —— 同一就绪判据已经有别的进程在跑，就直说。实测那 17 轮空转里最毒的一环
/// 就是"上一轮的进程还占着端口，于是每次重启拿到的都是假信号"。引擎不该猜模型的意图，
/// 但把事实摆出来，模型自己就会先 stop 再起。
fn render_start(proj: &Path, out: &crate::proc::StartOutcome) -> String {
    use crate::proc::StartKind;
    let i = &out.info;
    let secs = out.waited_ms as f64 / 1000.0;
    let (mut s, tail) = match &out.kind {
        StartKind::Ready { evidence } => (
            format!(
                "✓ 后台已就绪 handle={} pid={} 用时 {secs:.1}s\n就绪判据命中：{evidence}",
                i.handle, i.pid
            ),
            None,
        ),
        StartKind::Exited {
            code,
            tail,
            last_probe,
        } => (
            format!(
                "✗ 进程已退出（没等到就绪）handle={} pid={} 用时 {secs:.1}s exit={}\n\
                 就绪判据最后一次：{}",
                i.handle,
                i.pid,
                code.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
                last_probe.clone().unwrap_or_else(|| "（没跑过）".into())
            ),
            Some(tail.clone()),
        ),
        StartKind::NotReady {
            evidence,
            tail,
            hint,
        } => (
            format!(
                "⚠ 进程还活着，但就绪判据在窗口内没命中 handle={} pid={} 用时 {secs:.1}s\n\
                 判据最后一次：{evidence}\n\
                 （判据没命中 ≠ 启动失败：慢启动很常见。看下面的日志尾，分辨它在启动还是卡住了。）{}",
                i.handle,
                i.pid,
                match hint {
                    Some(h) => format!("\n{h}"),
                    None => String::new(),
                }
            ),
            Some(tail.clone()),
        ),
        StartKind::Started => (
            format!(
                "✓ 后台已启动（没给就绪判据，不等它）handle={} pid={}",
                i.handle, i.pid
            ),
            None,
        ),
    };
    if let Some(t) = tail
        && !t.trim().is_empty()
    {
        s.push_str(&format!("\n日志尾部：\n{t}"));
    }
    s.push_str(&format!(
        "\n日志文件（可直接 read，或用 execute {{\"op\":\"log\",\"handle\":\"{}\"}}）：{}",
        i.handle, i.log
    ));
    s.push_str(&format!(
        "\n查状态 / 读日志 / 停掉：execute {{\"op\":\"status\"|\"log\"|\"stop\",\"handle\":\"{}\"}}",
        i.handle
    ));
    if i.keep_alive {
        s.push_str(
            "\n（已声明 keep_alive：本次 run 结束**不**收它 —— 用户在【服务】面板能看到、能按 pid 停掉；IDE 退出时一并收。）",
        );
    } else {
        s.push_str(&format!(
            "\n（**未声明 keep_alive**：本次 run 一结束引擎就会把它收掉。用户要的是“服务跑着”、而不是“验证一下”时，\
             现在就 execute {{\"op\":\"stop\",\"handle\":\"{}\"}} 停掉、再带 \"keep_alive\":true 重启；\
             否则你 final 里的“已启动”在用户看到时已经是空的。）",
            i.handle
        ));
    }

    let others: Vec<crate::proc::ProcInfo> = crate::proc::listing_for(proj)
        .into_iter()
        .filter(|p| !crate::proc::is_dead(&p.state) && p.handle != i.handle)
        .collect();
    if !others.is_empty() {
        s.push_str(&format!(
            "\n其他托管进程：{}",
            crate::proc::render_listing(&others)
        ));
        if let Some(rc) = &i.ready_cmd {
            let same = others
                .iter()
                .filter(|p| p.ready_cmd.as_deref() == Some(rc.as_str()))
                .count();
            if same > 0 {
                s.push_str(&format!(
                    "\n⚠ 上面有 {same} 个用的是**同一个就绪判据**。如果你是在反复重启同一个服务：\
                     先 stop 掉旧的再起 —— 重复起一个已经跑着的服务通常只会撞端口占用，\
                     而那个失败是假的。"
                ));
            }
        }
    }
    s
}

/// 后台启动的入口。
///
/// **`Err` = "这次请求本身没能执行"**（开关关着 / 闸门拒了 / 判据在启动前就已命中 /
/// 到并发上限 / 起不来），`Ok` = "真的跑起来了，结果好坏都写在文本里"。
/// 与 [`parse_action`] 的分界一致：句柄不存在等同于参数不合法，不该伪装成一次成功的工具调用。
pub(crate) fn tool_exec_bg(
    proj: &Path,
    cfg: &AppConfig,
    spec: &crate::proc::StartSpec,
) -> Result<String, String> {
    if !cfg.proc.enabled {
        return Err(
            "后台启动已关闭（ruyix.code.harness.proc.enabled = false）。请改用前台 execute，\
                    或在设置里打开 —— 关着的时候去用 start / Start-Process 那类花招只会更糟：\
                    输出拿不到、进程还脱离掌控。"
                .into(),
        );
    }
    // 闸门对后台同样生效：跑多久不改变"它是不是一条命令"
    preflight_execute(proj, &spec.cmd)?;
    execute_allowed(&spec.cmd)?;
    if let Some(rc) = &spec.ready_cmd {
        preflight_execute(proj, rc).map_err(|e| format!("就绪判据没通过闸门：\n{e}"))?;
        execute_allowed(rc).map_err(|e| format!("就绪判据被拒绝：{e}"))?;
    }
    let out = crate::proc::start(
        proj,
        spec,
        cfg.proc.max,
        cfg.proc.ready_timeout_secs,
        &crate::config::project_state_root(cfg, proj),
    )?;
    Ok(render_start(proj, &out))
}

/// 托管进程的句柄操作：查状态 / 读日志尾 / 停掉（连子进程树）。
pub(crate) fn tool_proc(op: ProcOp, handle: &str) -> Result<String, String> {
    match op {
        ProcOp::Status => {
            let i = crate::proc::status(handle)?;
            let ready = i.ready_cmd.clone().unwrap_or_else(|| "（无）".into());
            let mut s = format!(
                "handle={} {} pid={} 已跑 {:.1}s\n命令：{}\n就绪判据：{ready}\n日志：{}",
                i.handle,
                i.state,
                i.pid,
                i.elapsed_ms as f64 / 1000.0,
                clip(&i.cmd, 160),
                i.log
            );
            if crate::proc::is_dead(&i.state) {
                s.push_str(&format!(
                    "\n进程已退出 —— 退出码在上面 state 里；日志尾部用 \
                     execute {{\"op\":\"log\",\"handle\":\"{}\"}} 看。",
                    i.handle
                ));
            } else {
                s.push_str(&format!(
                    "\n还在跑。读日志尾：execute {{\"op\":\"log\",\"handle\":\"{}\"}}；\
                     停掉：execute {{\"op\":\"stop\",\"handle\":\"{}\"}}",
                    i.handle, i.handle
                ));
            }
            Ok(s)
        }
        ProcOp::Log => {
            let i = crate::proc::status(handle)?;
            let tail = crate::proc::log_tail(handle, crate::proc::LOG_TAIL_LINES)?;
            let what = if tail.trim().is_empty() {
                "（空 —— 进程可能还没吐东西）"
            } else {
                "（末尾若干行）"
            };
            Ok(format!(
                "handle={} {} pid={} 日志{what}：\n{tail}\n—— 全文在 {}",
                i.handle, i.state, i.pid, i.log
            ))
        }
        ProcOp::Stop => {
            let i = crate::proc::stop(handle)?;
            Ok(format!(
                "✓ 已停止 handle={} pid={}（连子进程树一起杀，端口会立刻释放）\n日志留着：{}",
                i.handle, i.pid, i.log
            ))
        }
    }
}

/// 执行一次 connect：看清单 / 调 MCP 工具 / 委托远端 Agent。
/// "连不上"（目标不存在、服务器起不来）走 Err → 提示词那侧记为失败轮；
/// 外部系统的业务错误是信息不是故障，包成 Ok 交回模型自己判断。
/// connect 的**调用摘要**（不含执行）：并发路径要先把摘要定好，再去 join 那些 future。
fn connect_brief(a: &ConnectAction) -> String {
    match a {
        ConnectAction::List => "connect list".into(),
        ConnectAction::Call { server, tool, .. } => format!("connect {server}/{tool}"),
        ConnectAction::Send { agent, text } => {
            format!("connect {agent}（委托 {} 字）", text.chars().count())
        }
    }
}

/// connect 的**执行**（不含摘要）：单动作与批并发两条路径共用同一个实现。
fn connect_future<'a>(conn: &'a dyn Connector, action: ConnectAction) -> ConnectFuture<'a, String> {
    Box::pin(connect_run(conn, action))
}

async fn connect_run(conn: &dyn Connector, action: ConnectAction) -> Result<String, String> {
    match action {
        ConnectAction::List => conn
            .list()
            .await
            .map(|ts| connect_note(&ts).unwrap_or_else(|| "（当前没有可连接的外部能力）".into())),
        ConnectAction::Call {
            server,
            tool: name,
            arguments,
        } => conn
            .call(ConnectRequest {
                action: "call".into(),
                server,
                tool: name,
                arguments,
                ..Default::default()
            })
            .await
            .map(connect_outcome_text),
        ConnectAction::Send { agent, text } => conn
            .call(ConnectRequest {
                action: "send".into(),
                agent,
                text,
                ..Default::default()
            })
            .await
            .map(connect_outcome_text),
    }
}

/// 单动作路径：摘要 + 执行（与并发路径同一份实现，不许各写一遍）
async fn connect_step(
    conn: &dyn Connector,
    action: ConnectAction,
) -> (String, String, Result<String, String>) {
    let brief = connect_brief(&action);
    (
        "connect".to_string(),
        brief,
        connect_future(conn, action).await,
    )
}

fn connect_outcome_text(outcome: ConnectOutcome) -> String {
    if outcome.is_error {
        format!("（外部系统返回错误）{}", outcome.text)
    } else {
        outcome.text
    }
}

/// 历史裁剪：只留 user/assistant、非空文本，最多 n 条（最近的），保持原顺序。
pub fn tail_history(history: &[HistoryMsg], n: usize) -> Vec<HistoryMsg> {
    let filtered: Vec<HistoryMsg> = history
        .iter()
        .filter(|m| (m.role == "user" || m.role == "assistant") && !m.text.trim().is_empty())
        .cloned()
        .collect();
    if filtered.len() <= n {
        filtered
    } else {
        filtered[filtered.len() - n..].to_vec()
    }
}

/// 一轮工具调用在 `msgs` 里的两个落点，以及它折叠后各自替换成什么。
///
/// 为什么记下标、而不给消息加个"这是第几轮"的字段：`ChatMessage` 是 `{role, content}` 且
/// 全仓共用（`llm.rs`），为工具循环的记账给它加字段会污染每一个调用方。
/// 下标 + **幂等**的摘要文本就够：同一轮反复折叠写进去的是同一串，不会漂移。
struct RoundSlot {
    /// 模型那半（assistant，含它发出的调用 JSON）在 msgs 里的下标
    assistant: usize,
    /// 结果那半（user）在 msgs 里的下标
    result: usize,
    /// 折叠后 assistant 那半替换成什么（只留"调了什么"，不留 content 正文）
    call_digest: String,
    /// 折叠后 result 那半替换成什么（一行事实）
    outcome_digest: String,
}

/// 一轮调用的形状摘要（**不带正文**）：折叠后模型仍认得出自己那一轮调了什么。
fn call_shape(actions: &[Action]) -> String {
    let one = |a: &Action| match a {
        Action::Read(s) => format!("read {}", s.brief()),
        Action::Write(s) => s.brief(),
        Action::Execute(c, _) => format!("execute {}", clip(c, 60)),
        Action::ExecBg(s) => format!("execute bg {}", clip(&s.cmd, 60)),
        Action::Proc(op, h) => format!("execute {} {h}", proc_op_name(*op)),
        Action::Connect(_) => "connect".to_string(),
        Action::Plan(p) => format!("plan {} 步", p.len()),
        Action::Ask(_) => "ask_user".to_string(),
        Action::Final(_) => "final".to_string(),
    };
    if actions.len() == 1 {
        one(&actions[0])
    } else {
        actions.iter().map(one).collect::<Vec<_>>().join("；")
    }
}

/// 一条工具结果折成一行：调了什么、成没成、头一行是什么。
///
/// 取头一行对四种能力刚好都是最有信息量的那行：read 的表头（`--- src/a.rs ---` 或
/// 窗口表头）、write 的"已写入/已改"、execute 输出的第一行（往往就是那行 error）。
fn outcome_line(tool: &str, brief: &str, res: &Result<String, String>) -> String {
    match res {
        Ok(t) => {
            let head = t.lines().next().unwrap_or("").trim();
            format!("{tool} {brief} ✓ {}", clip(head, 100))
        }
        Err(e) => format!("{tool} {brief} ✗ {}", clip(e, 100)),
    }
}

/// 把"超出保留窗口"的老轮次折叠成一行事实，返回 `(折叠了几轮, 省下多少字节)`。
///
/// 幂等：已折叠的轮次再折一次写进去的是同一串文本，所以每轮都调它没有副作用。
/// 它管的是**单次 run 内**工具循环的历史；跨会话的输入历史归 [`tail_history`]，两回事。
fn fold_history(msgs: &mut [ChatMessage], rounds: &[RoundSlot], keep: usize) -> (usize, usize) {
    if rounds.len() <= keep {
        return (0, 0);
    }
    let (mut folded, mut saved) = (0usize, 0usize);
    for r in &rounds[..rounds.len() - keep] {
        let mut hit = false;
        for (idx, digest) in [(r.assistant, &r.call_digest), (r.result, &r.outcome_digest)] {
            if msgs[idx].content != *digest {
                saved += msgs[idx].content.len().saturating_sub(digest.len());
                msgs[idx].content = digest.clone();
                hit = true;
            }
        }
        if hit {
            folded += 1;
        }
    }
    (folded, saved)
}

/// plan 步骤 → 大纲区的任务列表
fn plan_to_outline(task: &str, steps: Vec<PlanStep>) -> Plan {
    Plan {
        project_name: "agent".into(),
        language: String::new(),
        summary: clip(task, 120),
        entry: String::new(),
        test_command: String::new(),
        steps,
    }
}

/// 步骤声明的文件里，有多少已经在本 run 落地（overlay = 本 run 写过的相对路径 → 内容）
fn produced_files(s: &PlanStep, overlay: &BTreeMap<String, String>) -> usize {
    s.files
        .iter()
        .filter(|f| overlay.contains_key(f.as_str()))
        .count()
}

/// 步骤完成度 → 推给 UI（全部文件落地 = done ✅；部分 = running ⛏️）
///
/// **只在 `execute_plan` 关着的时候用**（开着时状态由派发逻辑报，两条通道同时跑只会互相矛盾）。
/// 它刻意不查"文件是否本来就存在"：run 中途我们没法知道模型的意图，能诚实说的只有
/// "本 run 写出来几个"。所以它只会把状态**往前推**（never downgrade）——
/// 拿"声明文件本来就存在"当满足是收尾（[`settle_steps`]）才做的判断。
fn emit_step_progress(sink: &dyn Sink, steps: &[PlanStep], overlay: &BTreeMap<String, String>) {
    let total = steps.len();
    for (i, s) in steps.iter().enumerate() {
        if s.files.is_empty() {
            continue;
        }
        let produced = produced_files(s, overlay);
        let st = if produced == s.files.len() {
            "done"
        } else if produced > 0 {
            "running"
        } else {
            continue; // 还没动过，不用发事件
        };
        sink.step(
            i + 1,
            total,
            &StepOutcome {
                step_id: s.id,
                title: s.title.clone(),
                status: st.into(),
                files: s.files.clone(),
                ..Default::default()
            },
        );
    }
}

/// 子步骤失败后交回模型的那一轮 —— 这是 `execute_plan` 下模型唯一的干预通道。
///
/// 为什么不闷头继续跑下一步：第 1 步就崩了的话后面全白跑。把"哪一步、为什么、已经产出
/// 什么"如实回灌，让模型自己选：重排计划 / 自己动手补齐 / 直接交付（并说明为何没做）。
fn step_failure_feedback(
    step: &PlanStep,
    index: usize,
    total: usize,
    report: &StepReport,
) -> String {
    let reason = report.error.clone().unwrap_or_else(|| "未说明原因".into());
    let produced = if report.files.is_empty() {
        "无".to_string()
    } else {
        report.files.join("、")
    };
    let msg = format!(
        "步骤 {index}/{total}「{}」执行失败：{reason}\n\
         本步已产出的文件：{produced}（已留在覆盖层里，不回滚）。\n\
         请决定下一步：① 调整计划后继续（重新输出 plan —— 注意新计划会从第 1 步重新执行）\
         ② 你自己动手补齐（read / write / execute）\
         ③ 直接交付（final，并说明这一步为何没做）。",
        step.title.trim()
    );
    format!(
        "{{\"ok\": false, \"error\": {}}}",
        serde_json::to_string(&msg).unwrap_or_else(|_| "\"\"".into())
    )
}

/// run 收尾：把计划里每个步骤都落到终态。
///
/// 沙漏（⌛）的语义是**等待执行** —— run 已经结束还显示等待，就是在骗人。
/// 步骤状态原先只由 [`emit_step_progress`] 在 write 轮里按「声明文件是否落地」推，
/// 于是两类步骤会永远停在初始态：① 计划里没声明文件的步骤（「编译验证」这类
/// 没有文件级判据的步骤）；② 声明了文件、这次却没写的步骤 —— 实测 run
/// `agent-20260920-091436` 只暂存了 `cloud-shop-admin/pom.xml`，而计划第 3 步
/// 「同步文档」声明的文档文件零产出，界面永远停在 2/3 + ⌛。
///
/// `delivered` = 本次交付了 final（按契约，模型在 final 里断言"全部完成后"），
/// 否则是轮次上限 / 用户取消 / 致命错误收场 —— 那种场合一步都不能算完成。
///
/// **这个推断分支只在没有引擎事实可用时才走**（`execute_plan` 关着，或步骤还没轮到派发）。
/// 它的判据是 `PlanStep::files` —— 模型**动手前**自己写的产出清单，会漂移，所以口径要宽：
///
/// * `missing` 只认「本 run 没写 **且** 磁盘上也没有」。声明了却本来就存在、无需重写的文件
///   （实测 run `agent-20260920-142405` 第 7 步声明 `vite.config.js`，那是前一天就有的文件）
///   不算缺 —— 否则界面会指着一个静静躺在那儿的文件说"缺它"。
/// * 「有产出但对不上声明」落 `partial`（未对齐），不再冒充 `skipped`。
///   `skipped`（跳过）的意思是"这步一件没做"，而实测里 6 个 `skipped` 有 4 个其实做了
///   一半以上、只是改名或少写 —— 用一个比事实更重的词去描述，和沙漏是同一种骗人。
fn settle_steps(
    sink: &dyn Sink,
    steps: &[PlanStep],
    overlay: &BTreeMap<String, String>,
    // 项目根：用来把"声明了、本 run 没写"再分成「真缺」和「本来就在」
    proj: &Path,
    delivered: bool,
    // 每个步骤**已执行过**的终态（`execute_plan` 下由派发逻辑写）。有它的步骤直接照抄：
    // 引擎知道"这一步跑完了 / 失败了"这个事实，比"声明文件是否落地"的推断准得多。
    executed: &[Option<(String, String)>],
) {
    let total = steps.len();
    for (i, s) in steps.iter().enumerate() {
        if let Some(Some((status, notes))) = executed.get(i) {
            sink.step(
                i + 1,
                total,
                &StepOutcome {
                    step_id: s.id,
                    title: s.title.clone(),
                    status: status.clone(),
                    notes: notes.clone(),
                    files: s.files.clone(),
                    ..Default::default()
                },
            );
            continue;
        }
        // 声明了、本 run 却没写的文件（两种成因，别混成一句"缺失"）：
        let unwritten: Vec<&str> = s
            .files
            .iter()
            .filter(|f| !overlay.contains_key(f.as_str()))
            .map(String::as_str)
            .collect();
        // 真缺 = 没写、磁盘上也没有。已经躺在项目里的文件不算缺（那只是"无需重写"）。
        let missing: Vec<&str> = unwritten
            .iter()
            .copied()
            .filter(|f| !proj.join(f).exists())
            .collect();
        // 本 run 真写出来的声明文件（partial 的说明里要列清楚"已经写了哪些"）
        let produced: Vec<&str> = s
            .files
            .iter()
            .filter(|f| overlay.contains_key(f.as_str()))
            .map(String::as_str)
            .collect();
        // 四种情形分开判，别让"没交付"一刀切把已做完的步骤降级：
        //   ① 真缺、且一件没写 → skipped，把缺什么写清楚（这是"跳过"的正当用法）
        //   ② 真缺、但有产出 → partial「未对齐」：做了，只是产出与它自己事前列的清单不一致
        //   ③ 计划里根本没声明文件（「编译验证」这类没有文件级判据的步骤）→ 只有交付才算完成
        //   ④ 声明的文件全部满足（写过 or 本来就在）→ done，与交付与否无关
        //      （取消前已经做完的步骤不该被降级）
        // 注意 ④ 不留 note：声明文件本来就在是**正常**的，给它挂个 ⚠ 只会让用户学会忽略提示。
        let (status, notes) = if !missing.is_empty() && produced.is_empty() {
            // 一件没写：这才是"跳过"
            let why = if delivered {
                "本次未产出声明的文件："
            } else {
                "本次未完成；未产出："
            };
            ("skipped", format!("{why}{}", missing.join("、")))
        } else if !missing.is_empty() {
            // 有产出但没对齐：做了事，只是产出与它自己事前列的清单不一致
            (
                "partial",
                format!(
                    "产出与声明不一致（已写 {}；还缺 {}）",
                    produced.join("、"),
                    missing.join("、")
                ),
            )
        } else if s.files.is_empty() {
            if delivered {
                ("done", String::new())
            } else {
                ("skipped", "本次未完成".into())
            }
        } else {
            ("done", String::new())
        };
        sink.step(
            i + 1,
            total,
            &StepOutcome {
                step_id: s.id,
                title: s.title.clone(),
                status: status.into(),
                notes,
                files: s.files.clone(),
                ..Default::default()
            },
        );
    }
}

// ============================================
// 门禁：机械验证 + 反思（v0.3）
// ============================================

/// 一次 run 内的门禁状态
#[derive(Default)]
struct GateState {
    /// 存在"改动后还没通过全量验证"的内容
    dirty: bool,
    /// 全量验证连续失败次数（防"验证不过就无限修"）
    full_failures: u32,
    /// 复核结论回灌主循环的次数
    reflect_rounds: u32,
    /// 要写进最终答复的补充说明（跳过原因 / 预算用尽 / 复核未完成）
    notes: Vec<String>,
    /// 交付前对账（未声明 keep_alive 的托管进程）提醒过了没有 —— 只提醒一次，不循环
    proc_warned: bool,
}

/// 窄验证：对**这一轮的改动内容**做单文件语法检查。
///
/// 作用在覆盖层内容上（不是磁盘），所以确认模式"磁盘未动"也安全。
/// 代价小是刻意的：它每写一次就跑一次，是给模型的即时反馈，不是交付判据。
pub(crate) async fn narrow_verify(cfg: &AppConfig, changes: &[FileChange]) -> VerifyOutcome {
    let files: Vec<(String, String)> = changes
        .iter()
        .map(|c| (c.path.clone(), c.after.clone()))
        .collect();
    let v = cfg.verify.clone();
    let timeout = Duration::from_secs(cfg.gate.staged_timeout_secs);
    let started = Instant::now();
    let checks =
        tokio::task::spawn_blocking(move || verify::staged_syntax_checks(&v, &files, timeout))
            .await
            .unwrap_or_default();
    VerifyOutcome::from_checks("narrow", &checks, started.elapsed().as_millis())
}

/// 全量验证：复用 `verify::run`（语法 + 单测，隔离出口一处决定）+ lint（规约）。
///
/// **只在写入/自主模式跑**：确认模式下改动还在暂存区、项目磁盘没变，在那里跑出来的
/// "通过"是假结论 —— 宁可显式跳过（带原因），也不给一个假的通过。
async fn full_verify(
    cfg: &AppConfig,
    proj: &Path,
    policy: WritePolicy,
    cancel: &CancelFlag,
) -> VerifyOutcome {
    if policy == WritePolicy::Stage {
        return VerifyOutcome::skip(
            "full",
            "确认模式下改动只在暂存区（项目磁盘未动），跑不了全量验证；本次只做了语法层。\
             切到写入/自主模式才会跑全量"
                .into(),
        );
    }
    let started = Instant::now();
    let (root, cfg_v, flag) = (proj.to_path_buf(), cfg.clone(), cancel.clone());
    let report =
        match tokio::task::spawn_blocking(move || verify::run(&cfg_v, "agent-gate", &root, &flag))
            .await
        {
            Ok(r) => r,
            Err(e) => return VerifyOutcome::skip("full", format!("验证线程没能跑起来：{e}")),
        };

    let mut items: Vec<CheckItem> = report.checks.iter().map(CheckItem::from).collect();
    let hint = report
        .checks
        .iter()
        .filter(|c| c.status == "failed")
        .map(|c| {
            format!(
                "- {}（{}）：{}\n  {}",
                c.target,
                c.language,
                c.reason,
                verify::hint_for(c)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    if cfg.lint.enabled {
        let (root2, cfg_l) = (proj.to_path_buf(), cfg.clone());
        match tokio::task::spawn_blocking(move || lint::run_lint(&root2, &cfg_l)).await {
            Ok(out) => {
                if let Some(item) = lint_item(&out) {
                    items.push(item);
                }
            }
            Err(e) => items.push(CheckItem {
                kind: "lint".into(),
                language: "-".into(),
                target: "规约检查".into(),
                status: "skipped".into(),
                reason: format!("lint 线程没能跑起来：{e}"),
            }),
        }
    }

    let mut out = VerifyOutcome::summarize("full", &items, started.elapsed().as_millis(), None);
    out.hint = hint;
    out
}

/// lint 结果 → 一项检查。口径与前端 `check-style` 一致：**error 才阻断**，warning 只记录。
fn lint_item(o: &lint::LintOutcome) -> Option<CheckItem> {
    let item = |status: &str, reason: String| CheckItem {
        kind: "lint".into(),
        language: "-".into(),
        target: "规约检查".into(),
        status: status.into(),
        reason,
    };
    if !o.ran {
        return Some(item(
            "skipped",
            o.parse_error
                .clone()
                .unwrap_or_else(|| "lint 没有运行".into()),
        ));
    }
    let Some(rep) = &o.report else {
        return Some(item(
            "skipped",
            o.parse_error
                .clone()
                .unwrap_or_else(|| "lint 没有产出报告".into()),
        ));
    };
    let warnings = rep.counts.get("warning").copied().unwrap_or(0);
    let errors = rep.counts.get("error").copied().unwrap_or(0);
    if errors == 0 {
        return Some(item(
            "passed",
            format!("error 0 · warning {warnings}（warning 只记录，不阻断）"),
        ));
    }
    let first = rep
        .diagnostics
        .iter()
        .find(|d| d.level == "error")
        .map(|d| format!("{} {}:{}", d.rule, d.file(), d.line()))
        .unwrap_or_else(|| format!("{errors} 条 error"));
    Some(item("failed", first))
}

/// 交付前对账：本项目里**这次 run 结束时会被收掉**的托管进程（未声明 `keep_alive`）。
///
/// 为什么非说不可：模型用后台模式起了服务、看到就绪，于是 final 写「服务已启动」—— 而 run 一结束
/// 引擎就把它收掉，用户 netstat 一看是空的。实测 run `agent-20260921-0950`（cloud-shop-admin）：
/// `p1.log` 里 09:51:18 `Tomcat started on port 8087`、09:51:21 还真接了模型那次 curl，
/// 日志随后**戛然而止**（没有优雅关闭 = 被杀的），而模型已经在 final 里写
/// 「服务已在本机 8087 端口成功启动并验证可访问」。引擎手里有这份事实，就该在放行答复之前说一次，
/// 让模型自己选：重启成 keep_alive，或者把话说准。
fn reap_warning(live: &[crate::proc::ProcInfo]) -> Option<String> {
    let doomed: Vec<&crate::proc::ProcInfo> = live.iter().filter(|p| !p.keep_alive).collect();
    if doomed.is_empty() {
        return None;
    }
    let list = doomed
        .iter()
        .map(|p| format!("{}（pid {}，{}）", p.handle, p.pid, clip(&p.cmd, 60)))
        .collect::<Vec<_>>()
        .join("、");
    Some(format!(
        "[交付前对账·托管进程] 本项目还有 {} 个后台进程**没声明 keep_alive**，本次 run 一结束引擎就会收掉：{list}。\
         如果答复里写「服务已启动 / 正在运行」，那句话在用户看到时就是假的（netstat 是空的）。二选一：\
         ① 用户要的是「服务跑着」 → 先 execute {{\"op\":\"stop\",\"handle\":\"<上面的 handle>\"}} 停掉，再用 {{\"cmd\":\"<原命令>\",\"background\":true,\"ready_cmd\":\"<原判据>\",\"keep_alive\":true}} 重启，然后 final；\
         ② 只是验证一下 → 明说「本次验证完已自动收掉」，别写成「正在运行」。",
        doomed.len()
    ))
}
/// 交付门禁：**有改动 → 全量验证；然后 → 反思**。返回 `Some(观察)` 表示打回让模型继续修。
///
/// 三条不变量：
/// - 验证/复核都失败时不判"任务失败"，只按预算放行并在答复里写明（诚实 > 好看）；
/// - 复核不引入第二个写手：它的结论回到主循环，改代码的永远是主循环；
/// - 用户取消时不启动新的验证/复核（长动作必须有取消路径）。
#[allow(clippy::too_many_arguments)]
async fn gate_before_final(
    cfg: &AppConfig,
    proj: &Path,
    ctx: &Ctx<'_>,
    task: &str,
    read_paths: &[String],
    answer: &str,
    gate: &mut GateState,
    out: &mut AgentOutcome,
    cancel: &CancelFlag,
    sink: &dyn Sink,
) -> Option<String> {
    // ⓪ 交付前对账：会被本次 run 结束收掉的托管进程，先说一次（只提醒一次，不循环）
    if !gate.proc_warned {
        let live = crate::proc::listing_for(proj);
        if let Some(board) = reap_warning(&live) {
            gate.proc_warned = true;
            sink.log(
                "warn",
                format!("[agent] 交付被打回（托管进程对账）：{}", clip(&board, 240)),
            );
            return Some(board);
        }
    }

    let has_changes = !ctx.changes.is_empty();

    // ① 机械验证（全量）：只在"有改动且改动还没被验证过"时跑
    if has_changes && cfg.gate.full && gate.dirty && !is_cancelled(cancel) {
        if gate.full_failures >= cfg.gate.max_full_attempts {
            if !gate.notes.iter().any(|n| n.contains("按预算放行")) {
                gate.notes.push(format!(
                    "> ⚠️ 全量验证连续失败 {} 次，按预算放行 —— **结论：未通过**（详见验证记录）",
                    gate.full_failures
                ));
            }
            gate.dirty = false; // 不再重复走这个分支
        } else {
            let v = full_verify(cfg, proj, ctx.policy, cancel).await;
            let status = v.status.clone();
            let skipped_reason = v.skipped_reason.clone();
            let observation = v.to_observation();
            sink.verify(&v);
            out.verifications.push(v);
            match status.as_str() {
                // 未通过：拒绝交付，把报告回灌；改动仍是"脏"的
                "failed" => {
                    gate.full_failures += 1;
                    gate.dirty = true;
                    return Some(observation);
                }
                // 跳过（确认模式 / 沙箱拒绝）：不是失败，放行但必须写明原因
                "skipped" => {
                    gate.dirty = false;
                    if let Some(r) = skipped_reason {
                        gate.notes.push(format!("> ⚠️ 本次未跑全量验证：{r}"));
                    }
                }
                _ => gate.dirty = false,
            }
        }
    }

    // ② 反思（干净上下文复核）：有改动审产物，没改动核依据
    if cfg.reflect.enabled && gate.reflect_rounds < cfg.reflect.max_rounds && !is_cancelled(cancel)
    {
        let rubric = reflect::select_rubric(has_changes);
        let input = reflect::ReflectInput {
            task,
            rubric,
            project_root: proj,
            changes: &ctx.changes,
            read_paths,
            probes: ctx.probes(),
            answer: if has_changes { None } else { Some(answer) },
            verifications: &out.verifications,
            gate_note: gate.notes.last().cloned(),
        };
        let outcome = reflect::run(cfg, &input, cancel, sink).await;
        out.usage.add(&outcome.usage);
        let reflection = outcome.reflection;
        sink.reflect(&reflection);
        let observation = reflection.to_observation();
        let suspect = reflection.suspect();
        if let Some(n) = &reflection.note {
            gate.notes.push(format!("> 复核未完成：{n}"));
        }
        out.reflections.push(reflection);
        if suspect {
            gate.reflect_rounds += 1;
            return Some(observation);
        }
    }

    None
}

// ============================================
// 主循环
// ============================================

/// 跑一次 Agent 工具循环。
/// `conn` 接 Connect 原语（宿主实现 MCP / A2A 等外部能力），没接就用 [`NoConnector`]。
/// `cancel` 是所有长动作（编译 / 测试 / 复核）的取消路径。
// 八项都是调用方已就绪的事实（配置、项目、任务、历史、策略、连接器、取消位、事件出口）；
// 打包成结构体只是把参数换个地方列一遍，同一先例见 `verify` 的 `run_check`。
#[allow(clippy::too_many_arguments)]
/// 工具循环（**没有提问通道**的形态）：headless / eval / 冒烟 / 管线用。
///
/// 没有 `Asker` 时 `ask_user` 一律 fail-closed（拒绝依赖它的动作），**绝不假装有人回答** ——
/// 与 `NoConnector` 同款纪律。宿主（会话）走 [`run_with_ask`]。
pub async fn run(
    cfg: &AppConfig,
    proj: &Path,
    task: &str,
    history: &[HistoryMsg],
    policy: WritePolicy,
    conn: &dyn Connector,
    cancel: &CancelFlag,
    sink: &dyn Sink,
) -> Result<AgentOutcome, String> {
    run_with_ask(
        cfg, proj, task, history, policy, conn, &NoAsker, cancel, sink,
    )
    .await
}

// 循环本体在 `agent/tool_loop.rs` —— 那 780 行搬出去之后，这个文件回到 3.5k 量级。
// 子模块用 `use super::*` 就能看到这里的一切（含私有项），所以是文件切分、不是可见性改造。
mod tool_loop;
pub use tool_loop::run_with_ask;

#[cfg(test)]
mod tests;
