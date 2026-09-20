//! 编程 Agent 的工具循环：Read / Write / Execute / Connect 四大原子能力。
//!
//! 为什么放弃"先分类问答/任务再走两条链路"：边界根本划不清 ——
//! 「帮我理解这个项目」要读代码才能答，「修一下这个报错」要读文件才知道怎么改。
//! 正确形态是一个循环：模型自己决定下一步用哪个能力（看结构 → 读文件 → 写回 → 跑测试），
//! 直到给出最终回答。能力集刻意只有四条原语，不搞一堆专项工具：
//!
//! - **Read**    读项目结构与文件内容（Git 历史经 Execute 的 `git log` / `git show` 达成）
//! - **Write**   写整文件（新增或覆盖）—— 改动也走整份内容，不留 find/replace 这类"半截写入"
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
//! 写入策略（[`WritePolicy`]）由调用方定：Stage = 只暂存到 `.ruyix/stage/`，UI 确认后
//! 才落盘；Apply = 直接写进项目（被覆盖文件先备份到 `.ruyix/backups/`）。

use crate::config::AppConfig;
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
use std::time::{Duration, Instant};

/// 循环轮次上限（每轮 = 一次模型调用，产出工具调用或最终答复）
pub const MAX_STEPS: usize = 96;
/// 连续模型调用失败的容忍上限：瞬时空内容/网络抖动退避后重试，连续超限才终止会话
/// 连续模型调用失败几次算致命。步骤执行体（`crate::step_agent`）复用同一条判据 ——
/// "抖两次就放弃"与"抖十次才放弃"是两种产品行为，不该在两个模块里各写一个数。
pub(crate) const LLM_FAIL_LIMIT: u32 = 3;
const READ_CLIP: usize = 8_000;
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
    /// 只暂存（`<项目>/.ruyix/stage/<id>/`），UI 确认后才落盘
    Stage,
    /// 直接写进项目；被覆盖文件先备份到 `<项目>/.ruyix/backups/agent-<ts>/`
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

pub const AGENT_SYSTEM: &str = r#"你是 ruyix IDE 里的编程 Agent，通过工具循环完成用户的工作。只有四种原子能力：

- read    读项目：{"tool":"read","args":{"path":"src/ 或 src/main.rs"}} —— 目录给结构树，文件给内容；Git 历史用 execute 跑 git log / git show 查。
- write   写文件：{"tool":"write","args":{"path":"相对路径","content":"完整文件内容"}} —— 新建或整文件重写。改已有文件前先 read 拿到现状，交回的必须是整份内容，不许用省略号或"其余不变"敷衍。
- execute 跑命令：{"tool":"execute","args":{"cmd":"命令","timeout_secs":30}} —— 工作目录是项目根，超时上限 120 秒；编译、测试、格式化、git 都走它。
- connect 连外部能力：{"tool":"connect","args":{"action":"list"}} 先看有哪些可连；调 MCP 工具用 {"tool":"connect","args":{"action":"call","server":"服务器名","tool":"工具名","arguments":{}}}；把任务委托给远端 Agent 用 {"tool":"connect","args":{"action":"send","agent":"名字","text":"任务描述"}}。可用清单在提示词里给过，没有的就别硬猜名字。
- plan    任务清单（不是第五种能力，只是给用户看进度）：要动多个文件时先 {"tool":"plan","args":{"steps":[{"title":"短标题","detail":"做什么","files":["相对路径"]}]}}，用户会在大纲区看到进度。

规则：
1. 每轮只输出一个 JSON 对象（一次能力调用，或最终答复），不要输出解释文字、不要 markdown 代码块包裹。
2. 回答关于本项目的问题前，先 read 相关文件/目录 —— 不要凭空猜测项目内容。
3. 改代码：先 read 全文，再用 write 交回整份新内容（哪怕只改一行）；没把握的地方原样保留，绝不丢内容。
4. 改动能验证就验证：execute 跑编译/测试（如 cargo test、python -m pytest、npm test），失败就继续修。
5. 项目之外的东西（数据库、浏览器、远端服务、另一个 Agent）走 connect —— 不要自己写脚本硬凑协议，也不要把外部能力的事当成项目内的改动。
6. 全部完成后输出最终答复：{"final":"给用户的完整说明（Markdown：结论、改了哪些文件、验证结果）"}"#;

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

// ============================================
// 动作解析（模型输出 → 结构化动作）
// ============================================

#[derive(Clone, Debug)]
enum Action {
    Final(String),
    Plan(Vec<PlanStep>),
    Read(String),
    Write(String, String),
    Execute(String, Option<u64>),
    Connect(ConnectAction),
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

/// 模型输出 → 结构化动作（`Action` 只在本模块用，别把内部枚举泄成 pub）
fn parse_action(raw: &str) -> Result<Action, String> {
    let json = llm::extract_json_object(raw);
    let v: serde_json::Value = serde_json::from_str(&json)
        .map_err(|e| format!("输出不是合法 JSON: {e}；片段: {}", clip(&json, 200)))?;
    if let Some(f) = v.get("final").and_then(|x| x.as_str()) {
        let text = f.trim().to_string();
        if text.is_empty() {
            return Err("final 为空".into());
        }
        return Ok(Action::Final(text));
    }
    let tool = v
        .get("tool")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let args = v.get("args").cloned().unwrap_or(serde_json::Value::Null);
    match tool.as_str() {
        "read" => Ok(Action::Read(get_str(&args, "path")?)),
        "write" => Ok(Action::Write(
            get_str(&args, "path")?,
            v.get("args")
                .and_then(|a| a.get("content"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        )),
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
        "execute" | "bash" => Ok(Action::Execute(
            get_str(&args, "cmd")?,
            args.get("timeout_secs").and_then(|x| x.as_u64()),
        )),
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
        other => Err(format!(
            "未知能力 {other:?}（只有 read / write / execute / connect 四种，外加 plan 清单，或输出 final）"
        )),
    }
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
    Read(String),
    Write(String, String),
    Execute(String, Option<u64>),
    Unsupported(&'static str),
}

/// 主循环动作 → 步骤动作（plan / connect 收窄成「不支持」）
pub(crate) fn parse_step_action(raw: &str) -> Result<StepAction, String> {
    Ok(match parse_action(raw)? {
        Action::Final(t) => StepAction::Final(t),
        Action::Read(p) => StepAction::Read(p),
        Action::Write(p, c) => StepAction::Write(p, c),
        Action::Execute(c, t) => StepAction::Execute(c, t),
        Action::Plan(_) => StepAction::Unsupported("plan"),
        Action::Connect(_) => StepAction::Unsupported("connect"),
    })
}

/// 解析失败的回灌消息（JSON 字符串，直接作为 user 消息）。
/// 两种病两种药：截断（finish_reason=length）叫模型**写短**，格式烂才叫它重发。
/// 错误文本一律经 serde 编码 —— parse_action 的报错带着 JSON 片段，裸拼会把
/// 引号漏进字符串值，让这条反馈自己变成非法 JSON。
pub(crate) fn parse_failure_feedback(err: &str, finish_reason: Option<&str>) -> String {
    let hint = if finish_reason == Some("length") {
        format!(
            "你的上一条输出被 max_tokens 截断了（finish_reason=length），不是格式问题：{err}。\
             请精简后重发：plan 的 detail 每条一句话、必要时减少 steps，不要重复刚才的长输出。"
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
    policy: WritePolicy,
    backup_dir: Option<PathBuf>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(proj: &'a Path, policy: WritePolicy) -> Self {
        Self {
            proj,
            overlay: BTreeMap::new(),
            changes: Vec::new(),
            policy,
            backup_dir: None,
        }
    }

    /// 本会话改了哪些文件（步骤执行体按"本步写过的路径"筛自己那部分）
    pub(crate) fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    /// 项目根（execute 的工作目录）
    pub(crate) fn project_root(&self) -> &Path {
        self.proj
    }

    /// read：目录给结构树（复用 repair 的列表逻辑），文件给内容；"." = 项目根
    pub(crate) fn tool_read(&self, raw: &str) -> Result<String, String> {
        let raw_trim = raw.trim();
        if raw_trim == "." || raw_trim == "./" {
            // 项目根结构：read "." 是模型探索项目的第一步
            let mut listing = String::from("--- 项目根目录结构 ---\n");
            let mut n = 0usize;
            repair::list_dir(self.proj, "", 0, &mut n, &mut listing);
            return Ok(listing);
        }
        let rel = safe_rel_path(raw)?;
        if let Some(c) = self.overlay.get(&rel) {
            return Ok(format!(
                "（{rel} 的当前内容 = 本会话已暂存的修改）\n{}",
                clip(c, READ_CLIP)
            ));
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
            Ok(format!("--- {rel} ---\n{}", clip(&content, READ_CLIP)))
        } else {
            Err(format!(
                "{rel} 不存在（项目根目录用 read \".\" 看整体结构）"
            ))
        }
    }

    /// write：记录变更进覆盖层；Apply 策略立刻落盘（覆盖前备份）
    pub(crate) fn tool_write(&mut self, path: &str, content: &str) -> Result<String, String> {
        let rel = safe_rel_path(path)?;
        if content.is_empty() {
            return Err("content 为空 —— 不允许静默清空文件".into());
        }
        self.commit(rel.clone(), content.to_string())?;
        Ok(format!(
            "已写入 {rel}（{} 字节）{}",
            content.len(),
            if self.policy == WritePolicy::Stage {
                "（已暂存，用户确认后生效）"
            } else {
                ""
            }
        ))
    }

    /// 记录变更 + 更新覆盖层；Apply 策略同步落盘
    fn commit(&mut self, rel: String, after: String) -> Result<(), String> {
        let before = self
            .overlay
            .get(&rel)
            .cloned()
            .or_else(|| std::fs::read_to_string(self.proj.join(&rel)).ok());
        // 同一文件多次写：以首次记录的 before 为准（变更史在轨迹里）
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
                before: before.clone(),
                after: after.clone(),
            });
        }
        if self.policy == WritePolicy::Apply {
            self.flush_one(&rel, before, &after)?;
        }
        self.overlay.insert(rel, after);
        Ok(())
    }

    /// 落盘一个文件；覆盖既有文件前，把原文备份到 .ruyix/backups/agent-<ts>/<rel>
    fn flush_one(&mut self, rel: &str, before: Option<String>, after: &str) -> Result<(), String> {
        let target = self.proj.join(rel);
        if before.is_some() {
            let dir = self.backup_dir.get_or_insert_with(|| {
                let d = self
                    .proj
                    .join(".ruyix")
                    .join("backups")
                    .join(format!("agent-{}", crate::workspace::now_compact()));
                let _ = std::fs::create_dir_all(&d);
                d
            });
            let to = dir.join(rel);
            if let Some(parent) = to.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&to, before.unwrap_or_default())
                .map_err(|e| format!("备份 {rel} 失败：{e}（已中止写入）"))?;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&target, after).map_err(|e| format!("写入 {rel} 失败：{e}"))
    }

    /// Stage 策略收尾：把覆盖层写到暂存目录，返回目录路径
    fn flush_stage(&self) -> Result<PathBuf, String> {
        let dir = self
            .proj
            .join(".ruyix")
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

fn run_shell(proj: &Path, cmd: &str, timeout: Duration) -> exec::CmdOutput {
    #[cfg(target_os = "windows")]
    return exec::run(proj, "cmd", &["/C", cmd], timeout, &[]);
    #[cfg(not(target_os = "windows"))]
    return exec::run(proj, "sh", &["-c", cmd], timeout, &[]);
}

pub(crate) fn tool_execute(proj: &Path, cmd: &str, timeout_secs: Option<u64>) -> String {
    execute_allowed(cmd).map_or_else(
        |e| format!("❌ {e}"),
        |_| {
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
        },
    )
}

/// 执行一次 connect：看清单 / 调 MCP 工具 / 委托远端 Agent。
/// "连不上"（目标不存在、服务器起不来）走 Err → 提示词那侧记为失败轮；
/// 外部系统的业务错误是信息不是故障，包成 Ok 交回模型自己判断。
async fn connect_step(
    conn: &dyn Connector,
    action: ConnectAction,
) -> (String, String, Result<String, String>) {
    let tool = "connect".to_string();
    match action {
        ConnectAction::List => (
            tool,
            "connect list".into(),
            conn.list().await.map(|ts| {
                connect_note(&ts).unwrap_or_else(|| "（当前没有可连接的外部能力）".into())
            }),
        ),
        ConnectAction::Call {
            server,
            tool: name,
            arguments,
        } => (
            tool,
            format!("connect {server}/{name}"),
            conn.call(ConnectRequest {
                action: "call".into(),
                server,
                tool: name,
                arguments,
                ..Default::default()
            })
            .await
            .map(connect_outcome_text),
        ),
        ConnectAction::Send { agent, text } => (
            tool,
            format!("connect {agent}（委托 {} 字）", text.chars().count()),
            conn.call(ConnectRequest {
                action: "send".into(),
                agent,
                text,
                ..Default::default()
            })
            .await
            .map(connect_outcome_text),
        ),
    }
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
fn settle_steps(
    sink: &dyn Sink,
    steps: &[PlanStep],
    overlay: &BTreeMap<String, String>,
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
        let missing: Vec<&str> = s
            .files
            .iter()
            .filter(|f| !overlay.contains_key(f.as_str()))
            .map(String::as_str)
            .collect();
        // 三种情形分开判，别让"没交付"一刀切把已做完的步骤降级：
        //   ① 声明了文件却没产出 → 不冒充完成，落成终态并把缺什么写清楚（UI 放 title 提示）
        //   ② 计划里根本没声明文件（「编译验证」这类没有文件级判据的步骤）→ 只有交付才算完成
        //   ③ 声明的文件全部落地 → done，与交付与否无关（取消前已经做完的步骤不该被降级）
        let (status, notes) = if !missing.is_empty() {
            let why = if delivered {
                "本次未产出声明的文件："
            } else {
                "本次未完成；未产出："
            };
            ("skipped", format!("{why}{}", missing.join("、")))
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
    let task = task.trim();
    if task.is_empty() {
        return Err("消息不能为空".into());
    }
    let started = Instant::now();
    let mut ctx = Ctx::new(proj, policy);
    let mut plan_steps: Vec<PlanStep> = Vec::new();
    // ---- 计划执行（`step.execute_plan` 开启时）----
    // 游标在**引擎**手里：让模型每轮自选"我要做第几步"必然乱序、跳步、重复，而且
    // "选哪一步"本身还要烧一轮。模型的调整能力由"干预轮"补回来 —— 只有子步骤失败时，
    // 才把控制权交回模型一次。
    let mut plan_cursor: usize = 0;
    let mut plan_resets: u32 = 0;
    let mut intervene = false;
    // 已完成步骤的引擎侧事实（跨步骤唯一通道：子步骤输入包里的那一行）
    let mut plan_done: Vec<StepSummary> = Vec::new();
    // 每个步骤的终态（done / error + 说明）。收尾优先用它，而不是"文件是否落地"的推断
    let mut step_states: Vec<Option<(String, String)>> = Vec::new();
    // 总时长闸（0 = 不限）：父循环与每个子步骤共用同一条 deadline
    let deadline = (cfg.agent.max_elapsed_secs > 0)
        .then(|| started + Duration::from_secs(cfg.agent.max_elapsed_secs));
    // 本次是否真的交付了 final（收尾时决定计划步骤能不能算完成，见 settle_steps）
    let mut delivered = false;
    let mut out = AgentOutcome::default();
    let mut gate = GateState::default();
    // 这一轮读过哪些文件（依据核对的证据集合，复核员会看到这份清单）
    let mut read_paths: Vec<String> = Vec::new();
    // 连续模型调用失败计数：成功一轮即清零
    let mut llm_failures: u32 = 0;

    let mut msgs = vec![ChatMessage::system(agent_system_prompt())];
    for m in tail_history(history, 12) {
        msgs.push(if m.role == "assistant" {
            ChatMessage::assistant(m.text)
        } else {
            ChatMessage::user(m.text)
        });
    }
    // Connect：把宿主可连的外部能力一次性摆给模型（零连接时这段不出现，提示词不虚报能力）
    let connectables = conn.list().await.unwrap_or_default();
    let mut head = format!("项目根目录：{}", proj.display());
    if let Some(note) = connect_note(&connectables) {
        head.push_str(&format!("\n\n{note}"));
    }
    msgs.push(ChatMessage::user(format!("{head}\n\n用户消息：\n{task}")));

    sink.stage(
        "agent",
        "start",
        format!("工具循环（最多 {MAX_STEPS} 轮，写入策略：{policy:?}）"),
    );

    for step in 1..=MAX_STEPS {
        // 取消位：长动作（编译/测试/复核）都在这一层之下，先拦住再谈别的
        if is_cancelled(cancel) {
            out.answer = format!(
                "（用户取消：已完成 {} 轮工具调用、{} 个文件变更）",
                out.steps.len(),
                ctx.changes.len()
            );
            sink.log("warn", out.answer.clone());
            break;
        }
        // 总时长闸：到点就收手，把"为什么停"写进答复（子步骤内部看的是同一条 deadline）
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            out.answer = format!(
                "（达到总时长上限 {} 秒，循环停止。已完成 {} 轮工具调用、{} 个文件变更；请基于以上进展继续指示。）",
                cfg.agent.max_elapsed_secs,
                out.steps.len(),
                ctx.changes.len()
            );
            sink.log("warn", out.answer.clone());
            break;
        }

        // 计划即执行：这一轮就做一件事 —— 跑完一个步骤（失败则再给模型一次干预轮）。
        // 步骤由引擎按序派发，模型不参与调度（理由见 plan_cursor 的注释）。
        if cfg.step.execute_plan && !intervene && plan_cursor < plan_steps.len() {
            let idx = plan_cursor;
            let cur = plan_steps[idx].clone();
            let total = plan_steps.len();
            // 派发**之前**就报"进行中"：原先 running 只在 write 之后才发，用户会盯着 ⌛
            // 干等几十秒（一个步骤内部往往要先 read 好几个文件）
            sink.step(
                idx + 1,
                total,
                &StepOutcome {
                    step_id: cur.id,
                    title: cur.title.clone(),
                    status: "running".into(),
                    files: cur.files.clone(),
                    ..Default::default()
                },
            );
            sink.log(
                "info",
                clip(
                    &format!(
                        "[agent] 第 {step} 轮 派发步骤 {}/{}「{}」",
                        idx + 1,
                        total,
                        cur.title.trim()
                    ),
                    240,
                ),
            );
            let inp = StepInput {
                project_root: proj,
                task,
                step: &cur,
                index: idx + 1,
                total,
                done: &plan_done,
            };
            // 致命模型错误上抛（与主循环同一判据）；其余一律是 Ok(status=error) 的报告
            let report = step_agent::run_step(cfg, &mut ctx, &inp, cancel, deadline, sink).await?;
            out.usage.add(&report.usage);
            let ok = report.ok();
            if !report.files.is_empty() {
                gate.dirty = true; // 子步骤写了文件 → final 时该跑全量验证
            }
            // 子步骤的 read 不进父上下文，但它查过什么必须让父知道（复核员的证据集合）
            for p in &report.read_paths {
                if !read_paths.contains(p) {
                    read_paths.push(p.clone());
                }
            }
            let facts = report.as_summary(&cur);
            step_states[idx] = Some((
                report.status.clone(),
                if facts.note.trim().is_empty() {
                    report.error.clone().unwrap_or_default()
                } else {
                    facts.note.clone()
                },
            ));
            if ok {
                plan_done.push(facts);
            }
            sink.step(
                idx + 1,
                total,
                &StepOutcome {
                    step_id: cur.id,
                    title: cur.title.clone(),
                    status: report.status.clone(),
                    notes: report.note.clone(),
                    error: report.error.clone(),
                    files: report.files.clone(),
                    no_files: report.files.is_empty(),
                    ..Default::default()
                },
            );
            let headline = clip(&report.headline(&cur), 400);
            out.steps.push(StepTrace {
                step,
                tool: "step".into(),
                brief: headline.clone(),
                ok,
            });
            sink.log(
                if ok { "info" } else { "warn" },
                format!(
                    "[agent] 第 {step} 轮 step {} {headline}",
                    if ok { "✓" } else { "✗" }
                ),
            );
            plan_cursor += 1;
            if !ok {
                // 停下交回模型一次：第 1 步就崩了，闷头往下跑全是白费
                intervene = true;
                msgs.push(ChatMessage::user(step_failure_feedback(
                    &cur,
                    idx + 1,
                    total,
                    &report,
                )));
            }
            continue;
        }

        let reply = match llm::chat(&cfg.llm, &msgs, true).await {
            Ok(r) => {
                llm_failures = 0;
                r
            }
            Err(e) => {
                llm_failures += 1;
                // 鉴权/余额这类确定性失败重试无意义；其余（空内容/网络抖动）退避后
                // 吃下一轮 —— 轮次预算本身就是防死循环的闸
                if llm::is_fatal_error(&e) || llm_failures >= LLM_FAIL_LIMIT {
                    sink.log("error", format!("[agent] 第 {step} 轮模型调用失败：{e}"));
                    return Err(e);
                }
                sink.log(
                    "warn",
                    format!(
                        "[agent] 第 {step} 轮模型调用失败（连续 {llm_failures}/{LLM_FAIL_LIMIT}，退避后重试）：{e}"
                    ),
                );
                tokio::time::sleep(Duration::from_millis(1200 * llm_failures as u64)).await;
                continue;
            }
        };
        out.usage.add(&reply.usage);
        let action = match parse_action(&reply.content) {
            Ok(a) => a,
            Err(e) => {
                // 解析失败不终止：把错误告诉模型让它重出（消耗轮次预算，防死循环）。
                // 截断（finish_reason=length）与格式烂是两种病：截断必须叫模型写短，
                // 否则它原样重发再截断一次（实测连烧三轮才碰巧写短过关）
                let truncated = reply.finish_reason.as_deref() == Some("length");
                let tag = if truncated {
                    "（finish_reason=length，已要求精简重发）"
                } else {
                    ""
                };
                sink.log(
                    "warn",
                    format!("[agent] 第 {step} 轮输出无法解析{tag}：{e}"),
                );
                msgs.push(ChatMessage::user(parse_failure_feedback(
                    &e,
                    reply.finish_reason.as_deref(),
                )));
                continue;
            }
        };
        msgs.push(ChatMessage::assistant(reply.content.clone()));

        // 交付走门禁：有改动先过全量验证，再过干净上下文的复核
        if let Action::Final(text) = action {
            let blocked = gate_before_final(
                cfg,
                proj,
                &ctx,
                task,
                &read_paths,
                &text,
                &mut gate,
                &mut out,
                cancel,
                sink,
            )
            .await;
            match blocked {
                Some(observation) => {
                    let level = if observation.contains("[机械验证") {
                        "warn"
                    } else {
                        "info"
                    };
                    sink.log(
                        level,
                        format!(
                            "[agent] 第 {step} 轮交付被打回：{}",
                            clip(&observation, 300)
                        ),
                    );
                    msgs.push(ChatMessage::user(observation));
                    continue;
                }
                None => {
                    out.answer = text;
                    delivered = true;
                    sink.log("ok", clip(&format!("[agent] 完成，共 {step} 轮"), 200));
                    break;
                }
            }
        }

        let (tool, brief, result): (String, String, Result<String, String>) = match action {
            // Final 已在上面处理（这里只是让 match 穷尽）
            Action::Final(_) => unreachable!("Final 在门禁分支里已经处理"),
            Action::Plan(steps) => {
                let is_reset = !plan_steps.is_empty();
                if cfg.step.execute_plan && is_reset && plan_resets >= MAX_PLAN_RESETS {
                    // 重排次数用尽：忽略这一次，按现有计划继续 —— 否则
                    // "失败 → 重排 → 又失败 → 再重排"能把整个轮次预算烧光却什么都不产出
                    (
                        "plan".into(),
                        "重排被忽略（已达上限）".into(),
                        Ok(format!(
                            "计划重排次数已达上限（{MAX_PLAN_RESETS} 次），继续按现有计划执行。"
                        )),
                    )
                } else {
                    plan_steps = steps;
                    // 新计划 = 从头执行（游标归零）。已完成的步骤事实留在 plan_done 里，
                    // 子步骤输入包会带上它，模型仍能看到"前面做过什么"。
                    plan_cursor = 0;
                    step_states = vec![None; plan_steps.len()];
                    if is_reset {
                        plan_resets += 1;
                    }
                    let p = plan_to_outline(task, plan_steps.clone());
                    sink.plan(&p);
                    (
                        "plan".into(),
                        format!("{} 个步骤", plan_steps.len()),
                        Ok(if cfg.step.execute_plan {
                            "计划已收到，引擎将按序执行各步骤；全部做完后输出 final。\
                             若要调整计划，重新输出 plan（注意：会从第 1 步重新执行）。"
                                .into()
                        } else {
                            "任务清单已展示给用户（大纲区），按清单继续。".into()
                        }),
                    )
                }
            }
            Action::Read(path) => {
                let brief = format!("read {path}");
                let r = ctx.tool_read(&path);
                // 读过什么 = 依据核对的证据集合（复核员会看到这份清单）
                if r.is_ok() && !read_paths.contains(&path) {
                    read_paths.push(path.clone());
                }
                ("read".into(), brief, r)
            }
            Action::Write(path, content) => {
                let brief = format!("write {path}（{} 字节）", content.len());
                let r = ctx.tool_write(&path, &content);
                ("write".into(), brief, r)
            }
            Action::Execute(cmd, t) => {
                let brief = format!("execute {}", clip(&cmd, 80));
                let r = Ok(tool_execute(proj, &cmd, t));
                ("execute".into(), brief, r)
            }
            Action::Connect(ca) => connect_step(conn, ca).await,
        };

        let ok = result.is_ok();
        out.steps.push(StepTrace {
            step,
            tool: tool.clone(),
            brief: brief.clone(),
            ok,
        });
        let level = if ok { "info" } else { "warn" };
        let status_icon = if ok { "✓" } else { "✗" };
        sink.log(
            level,
            clip(
                &format!("[agent] 第 {step} 轮 {tool} {status_icon} {brief}"),
                400,
            ),
        );

        // execute_plan 下步骤状态由派发逻辑报告（引擎知道"这一步跑完了"这个事实，比
        // "声明文件是否落地"的推断准得多），两条通道混用只会互相打架
        if !cfg.step.execute_plan && !plan_steps.is_empty() && tool == "write" {
            emit_step_progress(sink, &plan_steps, &ctx.overlay);
        }

        // 机械验证（窄层）：事实触发 —— 这一轮真的写成了文件。失败当成"观察"回灌，
        // 不打断这一轮（它是即时反馈，不是交付判据；交付判据在 gate_before_final）。
        if tool == "write" && ok && !ctx.changes.is_empty() {
            gate.dirty = true;
            if cfg.gate.narrow && !is_cancelled(cancel) {
                let v = narrow_verify(cfg, &ctx.changes).await;
                let failed = v.status == "failed";
                let observation = v.to_observation();
                sink.log(
                    if failed { "warn" } else { "info" },
                    format!("[agent] 第 {step} 轮 {}", clip(&v.verdict, 200)),
                );
                sink.verify(&v);
                out.verifications.push(v);
                if failed {
                    msgs.push(ChatMessage::user(observation));
                }
            }
        }

        let result_text = match result {
            Ok(r) => format!(
                "{{\"ok\": true, \"result\": {}}}",
                serde_json::to_string(&r).unwrap_or_else(|_| "\"\"".into())
            ),
            Err(e) => format!(
                "{{\"ok\": false, \"error\": {}}}",
                serde_json::to_string(&e).unwrap_or_else(|_| "\"\"".into())
            ),
        };
        msgs.push(ChatMessage::user(result_text));

        // 模型已经对失败做出过回应（无论它选了哪个动作），把控制权交回引擎继续派发。
        // 解析失败 / 门禁打回那两条 `continue` 不清它 —— 那种场合模型还没给出有效决定。
        intervene = false;

        if step == MAX_STEPS {
            out.answer = format!(
                "（达到 {MAX_STEPS} 轮上限，循环停止。已完成 {} 次工具调用、{} 个文件变更；请基于以上进展继续指示。）",
                out.steps.len(),
                out.changes.len()
            );
            sink.log("warn", out.answer.clone());
        }
    }

    // 计划落定：run 结束了，大纲区不该再留沙漏（⌛ 是"等待执行"，不是"没做成"）
    if !plan_steps.is_empty() {
        settle_steps(sink, &plan_steps, &ctx.overlay, delivered, &step_states);
    }

    // 门禁留下的补充说明（跳过原因 / 预算用尽 / 复核未完成）：
    // 写在答复末尾 —— 用户不该去翻日志才知道"这次其实没验成"
    if !gate.notes.is_empty() {
        out.answer = format!(
            "{}\n\n---\n{}",
            out.answer.trim_end(),
            gate.notes.join("\n")
        );
    }

    if !ctx.changes.is_empty() && policy == WritePolicy::Stage {
        let dir = ctx.flush_stage()?;
        out.stage_dir = Some(dir.to_string_lossy().to_string());
    }
    out.backup_dir = ctx
        .backup_dir
        .as_ref()
        .map(|d| d.to_string_lossy().to_string());
    out.changes = ctx.changes.clone();
    sink.stage(
        "agent",
        "done",
        format!(
            "工具调用 {} 次 · {} 个文件变更 · 验证 {} 次 · 复核 {} 次",
            out.steps.len(),
            out.changes.len(),
            out.verifications.len(),
            out.reflections.len()
        ),
    );
    out.elapsed_ms = started.elapsed().as_millis();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "dh-agent-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn write(&self, rel: &str, content: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parse_action_covers_all_tools() {
        assert!(matches!(
            parse_action(r#"{"final":"完成了"}"#),
            Ok(Action::Final(t)) if t == "完成了"
        ));
        assert!(matches!(
            parse_action(r#"{"tool":"read","args":{"path":"src/"}}"#),
            Ok(Action::Read(p)) if p == "src/"
        ));
        assert!(matches!(
            parse_action(r#"{"tool":"write","args":{"path":"a.py","content":"x=1"}}"#),
            Ok(Action::Write(p, c)) if p == "a.py" && c == "x=1"
        ));
        assert!(matches!(
            parse_action(r#"{"tool":"execute","args":{"cmd":"cargo test","timeout_secs":60}}"#),
            Ok(Action::Execute(c, Some(60))) if c == "cargo test"
        ));
        // 别名：老历史里写着 bash 的一轮不该白烧（同一条 Execute 通路）
        assert!(matches!(
            parse_action(r#"{"tool":"bash","args":{"cmd":"ls"}}"#),
            Ok(Action::Execute(c, None)) if c == "ls"
        ));
        assert!(matches!(
            parse_action(r#"{"tool":"plan","args":{"steps":[{"title":"写模块","files":["m.py"]}]}}"#),
            Ok(Action::Plan(s)) if s.len() == 1 && s[0].id == 1
        ));
    }

    /// connect 的三形态：省略 action = list；call 要 server+tool；send 要 agent+text
    #[test]
    fn parse_action_covers_connect() {
        assert!(matches!(
            parse_action(r#"{"tool":"connect","args":{"action":"list"}}"#),
            Ok(Action::Connect(ConnectAction::List))
        ));
        assert!(matches!(
            parse_action(r#"{"tool":"connect","args":{}}"#),
            Ok(Action::Connect(ConnectAction::List))
        ));
        assert!(matches!(
            parse_action(
                r#"{"tool":"connect","args":{"action":"call","server":"fs","tool":"read_file","arguments":{"path":"a"}}}"#
            ),
            Ok(Action::Connect(ConnectAction::Call { server, tool, .. })) if server == "fs" && tool == "read_file"
        ));
        assert!(matches!(
            parse_action(r#"{"tool":"connect","args":{"action":"send","agent":"翻译","text":"你好"}}"#),
            Ok(Action::Connect(ConnectAction::Send { agent, text })) if agent == "翻译" && text == "你好"
        ));
        assert!(
            parse_action(r#"{"tool":"connect","args":{"action":"call","server":"fs"}}"#).is_err()
        );
        assert!(parse_action(r#"{"tool":"connect","args":{"action":"fly"}}"#).is_err());
    }

    #[test]
    fn parse_action_rejects_garbage() {
        assert!(parse_action("随便聊聊").is_err());
        assert!(parse_action(r#"{"tool":"fly"}"#).is_err());
        assert!(parse_action(r#"{"tool":"read","args":{}}"#).is_err());
        assert!(parse_action(r#"{"final":""}"#).is_err());
        // edit 已从能力集移除：模型若照老习惯发 edit，走"未知能力"通道让它改用 write
        assert!(parse_action(r#"{"tool":"edit","args":{"edits":[]}}"#).is_err());
    }

    /// 提示词契约：四大原语必须都在，问答场景被明确要求先读再说，edit 不再出现
    #[test]
    fn system_prompt_defines_four_tools() {
        for t in ["read", "write", "execute", "connect"] {
            assert!(AGENT_SYSTEM.contains(t), "缺能力 {t}");
        }
        assert!(AGENT_SYSTEM.contains("final"), "必须有终止协议");
        assert!(AGENT_SYSTEM.contains("不要凭空猜测"), "问答必须先读项目");
        assert!(
            !AGENT_SYSTEM.contains("\"edit\""),
            "edit 已并入 write（整份内容），提示词里不该再有 edit"
        );
    }

    /// 平台补充只该出现在对应平台的提示词里，且主体（AGENT_SYSTEM）不被改写
    #[test]
    fn platform_hint_matches_target_os() {
        let p = agent_system_prompt();
        assert!(p.starts_with(AGENT_SYSTEM), "主体必须原样开头");
        if cfg!(windows) {
            assert!(p.contains("git grep"), "Windows 上应带检索提示");
            assert!(p.len() > AGENT_SYSTEM.len());
        } else {
            assert_eq!(p, AGENT_SYSTEM, "非 Windows 不拼任何平台补充");
        }
    }

    /// 解析失败的回灌：截断叫写短、格式烂叫重发；错误文本带引号时
    /// 反馈自己仍必须是合法 JSON（老实现裸拼会碎）
    #[test]
    fn parse_feedback_distinguishes_truncation() {
        let trunc = parse_failure_feedback("EOF while parsing an object", Some("length"));
        assert!(trunc.contains("max_tokens 截断"));
        assert!(trunc.contains("精简"));
        assert!(serde_json::from_str::<serde_json::Value>(&trunc).is_ok());

        // 报错文本里带引号和花括号（parse_action 的真实输出形态）
        let messy =
            parse_failure_feedback("输出不是合法 JSON: ...；片段: {\"tool\":\"plan\",", None);
        assert!(messy.contains("请重新只输出一个 JSON 对象"));
        assert!(serde_json::from_str::<serde_json::Value>(&messy).is_ok());

        // stop 是正常收笔，不算截断
        let stop = parse_failure_feedback("烂格式", Some("stop"));
        assert!(stop.contains("请重新只输出一个 JSON 对象"));
        assert!(!stop.contains("max_tokens"));
        assert!(serde_json::from_str::<serde_json::Value>(&stop).is_ok());
    }

    #[test]
    fn execute_denies_destructive_patterns() {
        for bad in [
            "rm -rf /",
            "sudo shutdown now",
            "format c: /q",
            "MKFS /dev/sda1",
        ] {
            assert!(execute_allowed(bad).is_err(), "应拒绝：{bad}");
        }
        for ok in ["cargo test", "git log --oneline", "python -m pytest -q"] {
            assert!(execute_allowed(ok).is_ok(), "不该拒绝：{ok}");
        }
    }

    /// read：文件给内容、目录给树、overlay 优先、路径封闭
    #[test]
    fn tool_read_file_dir_overlay_and_jail() {
        let d = TempDir::new("read");
        d.write("src/main.rs", "fn main() {}");
        let mut ctx = Ctx {
            proj: &d.0,
            overlay: BTreeMap::new(),
            changes: Vec::new(),
            policy: WritePolicy::Stage,
            backup_dir: None,
        };
        assert!(ctx.tool_read("src/main.rs").unwrap().contains("fn main()"));
        assert!(ctx.tool_read("src").unwrap().contains("main.rs"));
        assert!(ctx.tool_read("ghost.py").is_err());
        assert!(ctx.tool_read("../../etc/passwd").is_err());

        // overlay 优先：write 之后 read 能看到修改（Stage 策略磁盘未动）
        ctx.tool_write("src/main.rs", "fn main() { println!(1); }")
            .unwrap();
        let r = ctx.tool_read("src/main.rs").unwrap();
        assert!(r.contains("已暂存的修改"), "{r}");
        assert!(r.contains("println"));
        assert_eq!(
            std::fs::read_to_string(d.0.join("src/main.rs")).unwrap(),
            "fn main() {}",
            "Stage 策略不许动磁盘"
        );
    }

    /// Stage 策略：变更只进暂存目录 + manifest；Apply 策略：直接落盘且覆盖先备份
    #[test]
    fn write_policies_stage_vs_apply() {
        let d = TempDir::new("policy-stage");
        d.write("keep.txt", "old");
        let mut ctx = Ctx {
            proj: &d.0,
            overlay: BTreeMap::new(),
            changes: Vec::new(),
            policy: WritePolicy::Stage,
            backup_dir: None,
        };
        ctx.tool_write("keep.txt", "new").unwrap();
        ctx.tool_write("created.txt", "hi").unwrap();
        let stage = ctx.flush_stage().unwrap();
        assert_eq!(
            std::fs::read_to_string(stage.join("files/keep.txt")).unwrap(),
            "new"
        );
        let manifest: Vec<FileChange> =
            serde_json::from_str(&std::fs::read_to_string(stage.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.len(), 2);
        assert!(manifest.iter().any(|c| c.path == "keep.txt"
            && c.kind == "modify"
            && c.before.as_deref() == Some("old")));
        assert!(
            manifest
                .iter()
                .any(|c| c.path == "created.txt" && c.kind == "add")
        );

        let d2 = TempDir::new("policy-apply");
        d2.write("keep.txt", "old");
        let mut ctx2 = Ctx {
            proj: &d2.0,
            overlay: BTreeMap::new(),
            changes: Vec::new(),
            policy: WritePolicy::Apply,
            backup_dir: None,
        };
        ctx2.tool_write("keep.txt", "new").unwrap();
        assert_eq!(
            std::fs::read_to_string(d2.0.join("keep.txt")).unwrap(),
            "new"
        );
        let backup = ctx2.backup_dir.as_ref().unwrap();
        assert_eq!(
            std::fs::read_to_string(backup.join("keep.txt")).unwrap(),
            "old"
        );
    }

    /// 假连接器：记录收到的请求（Connect 的路由与错误语义由它验）
    struct Recorder {
        calls: std::sync::Mutex<Vec<ConnectRequest>>,
        fail: bool,
    }

    impl Connector for Recorder {
        fn list(&self) -> ConnectFuture<'_, Vec<ConnectTarget>> {
            Box::pin(async {
                Ok(vec![ConnectTarget {
                    kind: "mcp".into(),
                    name: "fs".into(),
                    detail: "已连接".into(),
                    tools: vec!["read_file(读文件)".into()],
                }])
            })
        }

        fn call(&self, req: ConnectRequest) -> ConnectFuture<'_, ConnectOutcome> {
            Box::pin(async move {
                if self.fail {
                    return Err("连不上 fs".into());
                }
                self.calls.lock().unwrap().push(req.clone());
                Ok(ConnectOutcome {
                    text: format!("{} 返回", req.action),
                    is_error: false,
                })
            })
        }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            // enable_all 而不是只用 enable_time：整循环级用例要真发 HTTP 请求（假 LLM
            // 是本地 TCP 服务），只开 time driver 会连不上
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// 门禁单测用的 Sink：不刷屏、不落盘
    struct QuietSink;
    impl Sink for QuietSink {}

    /// 门禁单测用的配置：不依赖 Docker、不绑 tools/lint、不调模型（复核另测）
    fn apply_cfg() -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.sandbox.mode = "off".into();
        cfg.lint.enabled = false;
        cfg.reflect.enabled = false;
        cfg
    }

    fn one_change(path: &str) -> Vec<FileChange> {
        vec![FileChange {
            path: path.into(),
            kind: "modify".into(),
            before: Some("旧内容\n".into()),
            after: "新内容\n".into(),
        }]
    }

    fn ctx_with<'a>(proj: &'a Path, changes: Vec<FileChange>, policy: WritePolicy) -> Ctx<'a> {
        Ctx {
            proj,
            overlay: BTreeMap::new(),
            changes,
            policy,
            backup_dir: None,
        }
    }

    /// 确认模式：项目磁盘没动 → 全量验证**显式跳过**（带原因）且放行，不许假装通过
    #[test]
    fn gate_skips_full_verify_in_stage_mode_and_says_why() {
        let d = TempDir::new("gate-stage");
        let ctx = ctx_with(&d.0, one_change("a.py"), WritePolicy::Stage);
        let cfg = apply_cfg();
        let mut gate = GateState {
            dirty: true,
            ..Default::default()
        };
        let mut out = AgentOutcome::default();
        let blocked = block_on(gate_before_final(
            &cfg,
            &d.0,
            &ctx,
            "改点东西",
            &[],
            "已改好",
            &mut gate,
            &mut out,
            &crate::exec::new_cancel_flag(),
            &QuietSink,
        ));
        assert!(blocked.is_none(), "跳过不是失败，不该拦交付");
        assert_eq!(out.verifications.len(), 1);
        let v = &out.verifications[0];
        assert_eq!(v.layer, "full");
        assert_eq!(v.status, "skipped");
        assert!(
            v.skipped_reason
                .as_deref()
                .unwrap_or("")
                .contains("确认模式"),
            "{:?}",
            v.skipped_reason
        );
        assert!(
            gate.notes.iter().any(|n| n.contains("未跑全量验证")),
            "跳过必须写进答复：{:?}",
            gate.notes
        );
    }

    /// 写入模式 + 测试真失败 → 拒绝交付，把验证报告回灌（门禁的核心作用）
    #[test]
    fn gate_blocks_delivery_when_verification_fails() {
        let d = TempDir::new("gate-fail");
        d.write("calc.py", "def add(a, b):\n    return a - b\n");
        d.write(
            "test_calc.py",
            "import unittest\nfrom calc import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == '__main__':\n    unittest.main()\n",
        );
        let mut ctx = ctx_with(&d.0, one_change("calc.py"), WritePolicy::Apply);
        ctx.overlay.insert(
            "calc.py".into(),
            "def add(a, b):\n    return a - b\n".into(),
        );
        let cfg = apply_cfg();
        let mut gate = GateState {
            dirty: true,
            ..Default::default()
        };
        let mut out = AgentOutcome::default();
        let blocked = block_on(gate_before_final(
            &cfg,
            &d.0,
            &ctx,
            "修好 add",
            &[],
            "我修好了",
            &mut gate,
            &mut out,
            &crate::exec::new_cancel_flag(),
            &QuietSink,
        ))
        .expect("测试真的失败时必须打回");
        assert!(blocked.contains("[机械验证·全量验证]"), "{blocked}");
        assert_eq!(out.verifications.last().unwrap().status, "failed");
        assert_eq!(gate.full_failures, 1);
        assert!(gate.dirty, "没通过就还是脏的，不能放行");
    }

    /// 验证真通过 → 放行，且改动不再是"脏"的（下次不用重复跑一遍编译）
    #[test]
    fn gate_releases_once_verification_passes() {
        let d = TempDir::new("gate-pass");
        d.write("calc.py", "def add(a, b):\n    return a + b\n");
        d.write(
            "test_calc.py",
            "import unittest\nfrom calc import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == '__main__':\n    unittest.main()\n",
        );
        let ctx = ctx_with(&d.0, one_change("calc.py"), WritePolicy::Apply);
        let cfg = apply_cfg();
        let mut gate = GateState {
            dirty: true,
            ..Default::default()
        };
        let mut out = AgentOutcome::default();
        let blocked = block_on(gate_before_final(
            &cfg,
            &d.0,
            &ctx,
            "补个 add",
            &[],
            "写好了",
            &mut gate,
            &mut out,
            &crate::exec::new_cancel_flag(),
            &QuietSink,
        ));
        assert!(blocked.is_none(), "{blocked:?}");
        assert_eq!(out.verifications.last().unwrap().status, "passed");
        assert!(!gate.dirty, "通过了就不该再重复编译一遍");
        assert!(gate.notes.is_empty(), "通过不该留任何告示");
    }

    /// 预算用尽：不再拦，但必须写明"结论：未通过"（诚实优先于好看）
    #[test]
    fn gate_lets_through_after_budget_with_an_honest_note() {
        let d = TempDir::new("gate-budget");
        let ctx = ctx_with(&d.0, one_change("a.py"), WritePolicy::Apply);
        let cfg = apply_cfg();
        let mut gate = GateState {
            dirty: true,
            full_failures: cfg.gate.max_full_attempts,
            ..Default::default()
        };
        let mut out = AgentOutcome::default();
        let blocked = block_on(gate_before_final(
            &cfg,
            &d.0,
            &ctx,
            "改点东西",
            &[],
            "改好了",
            &mut gate,
            &mut out,
            &crate::exec::new_cancel_flag(),
            &QuietSink,
        ));
        assert!(blocked.is_none(), "预算用尽后要放行，否则模型会卡死在这里");
        assert!(
            gate.notes.iter().any(|n| n.contains("未通过")),
            "{:?}",
            gate.notes
        );
        assert!(out.verifications.is_empty(), "放行那轮不该再跑一次");
    }

    /// 门禁汇总：什么都没跑成 → skipped（绝不写"通过"）；失败必须带修复提示
    #[test]
    fn verify_outcome_never_reports_a_fake_pass() {
        let skip = VerifyOutcome::skip("full", "沙箱不可用".into());
        assert_eq!(skip.status, "skipped");
        assert!(skip.verdict.contains("沙箱不可用"), "{}", skip.verdict);

        let items = vec![
            CheckItem {
                kind: "syntax".into(),
                language: "python".into(),
                target: "a.py".into(),
                status: "skipped".into(),
                reason: "没有 Python 文件".into(),
            },
            CheckItem {
                kind: "lint".into(),
                language: "-".into(),
                target: "规约检查".into(),
                status: "passed".into(),
                reason: "error 0".into(),
            },
        ];
        let mixed = VerifyOutcome::summarize("full", &items, 5, None);
        assert_eq!(mixed.status, "passed", "有通过项就不算跳过");
        assert_eq!(mixed.passed_checks, 1);
        assert_eq!(mixed.skipped, 1);

        let all_skipped = VerifyOutcome::summarize("full", &items[..1], 5, None);
        assert_eq!(all_skipped.status, "skipped");
        assert!(
            all_skipped
                .skipped_reason
                .as_deref()
                .unwrap()
                .contains("Python"),
            "跳过原因要借第一条 skipped 检查，别退化成套话"
        );
    }

    /// lint → 检查项：error 才算失败，warning 只记录（与前端 check-style 同口径）
    #[test]
    fn lint_item_maps_errors_only() {
        let clean = lint::LintOutcome {
            ran: true,
            exit_code: Some(0),
            cmd: "harness_lint".into(),
            report: Some(lint::LintReport {
                root: ".".into(),
                files_scanned: 3,
                files_skipped: 0,
                test_count: 0,
                counts: [("warning".to_string(), 7u32)].into_iter().collect(),
                by_rule: Default::default(),
                diagnostics: Vec::new(),
                suppressions: serde_json::Value::Null,
                ok: true,
            }),
            parse_error: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            duration_ms: 3,
        };
        let item = lint_item(&clean).unwrap();
        assert_eq!(item.status, "passed");
        assert!(item.reason.contains("warning 7"), "{}", item.reason);

        let not_run = lint::LintOutcome {
            ran: false,
            parse_error: Some("解释器没找到".into()),
            ..clean.clone()
        };
        assert_eq!(lint_item(&not_run).unwrap().status, "skipped");
    }

    /// connect：list / call / send 三形态各自把请求原样送达宿主，工具名与简报可读
    #[test]
    fn connect_routes_three_forms_to_host() {
        let rec = Recorder {
            calls: std::sync::Mutex::new(Vec::new()),
            fail: false,
        };
        let (tool, brief, r) = block_on(connect_step(&rec, ConnectAction::List));
        assert_eq!(tool, "connect");
        assert_eq!(brief, "connect list");
        assert!(r.unwrap().contains("mcp fs：已连接"), "清单要带连接状态");

        let (_, brief, r) = block_on(connect_step(
            &rec,
            ConnectAction::Call {
                server: "fs".into(),
                tool: "read_file".into(),
                arguments: serde_json::json!({"path": "a"}),
            },
        ));
        assert_eq!(brief, "connect fs/read_file");
        assert_eq!(r.unwrap(), "call 返回");

        let (_, brief, _) = block_on(connect_step(
            &rec,
            ConnectAction::Send {
                agent: "翻译".into(),
                text: "你好".into(),
            },
        ));
        assert_eq!(brief, "connect 翻译（委托 2 字）");

        let got = rec.calls.lock().unwrap();
        assert_eq!(got.len(), 2, "list 不该走 call");
        assert_eq!(got[0].action, "call");
        assert_eq!(got[0].server, "fs");
        assert_eq!(got[0].tool, "read_file");
        assert_eq!(got[0].arguments["path"], "a");
        assert_eq!(got[1].action, "send");
        assert_eq!(got[1].agent, "翻译");
        assert_eq!(got[1].text, "你好");
    }

    /// 连不上 = Err（提示词那侧记失败轮）；没接外部能力的宿主清单为空
    #[test]
    fn connect_failure_and_no_connector() {
        let rec = Recorder {
            calls: std::sync::Mutex::new(Vec::new()),
            fail: true,
        };
        let (_, _, r) = block_on(connect_step(
            &rec,
            ConnectAction::Call {
                server: "fs".into(),
                tool: "x".into(),
                arguments: serde_json::Value::Null,
            },
        ));
        assert!(r.unwrap_err().contains("连不上 fs"));

        let none = NoConnector;
        let (_, _, r) = block_on(connect_step(&none, ConnectAction::List));
        assert_eq!(r.unwrap(), "（当前没有可连接的外部能力）");
        let (_, _, r) = block_on(connect_step(
            &none,
            ConnectAction::Send {
                agent: "翻译".into(),
                text: "你好".into(),
            },
        ));
        assert!(r.is_err(), "没有连接器时 send 必须失败，不能假装成功");
    }

    /// 清单注入提示词：空清单不出现，非空逐行列出种类/名字/工具
    #[test]
    fn connect_note_renders_targets() {
        assert!(connect_note(&[]).is_none());
        let note = connect_note(&[
            ConnectTarget {
                kind: "mcp".into(),
                name: "fs".into(),
                detail: "已连接".into(),
                tools: vec!["read_file(读文件)".into()],
            },
            ConnectTarget {
                kind: "a2a".into(),
                name: "translator".into(),
                detail: "http://127.0.0.1:9999".into(),
                tools: vec![],
            },
        ])
        .unwrap();
        assert!(
            note.contains("mcp fs：已连接；工具：read_file(读文件)"),
            "{note}"
        );
        assert!(
            note.contains("a2a translator：http://127.0.0.1:9999"),
            "{note}"
        );
    }

    /// 记录 step 事件的 Sink —— 断言 UI 真会收到什么，而不是把谓词再抄一遍
    #[derive(Default)]
    struct StepSink {
        seen: std::sync::Mutex<Vec<(usize, usize, String, String)>>,
    }
    impl Sink for StepSink {
        fn step(&self, i: usize, total: usize, st: &StepOutcome) {
            self.seen
                .lock()
                .unwrap()
                .push((i, total, st.status.clone(), st.notes.clone()));
        }
    }
    impl StepSink {
        /// (index, total, status, notes)
        fn events(&self) -> Vec<(usize, usize, String, String)> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn plan_step(id: u32, title: &str, files: &[&str], kind: &str) -> PlanStep {
        PlanStep {
            id,
            title: title.into(),
            detail: String::new(),
            files: files.iter().map(|f| (*f).to_string()).collect(),
            kind: kind.into(),
        }
    }

    /// 进行中：文件全部落地 → done；部分 → running；一步没动 → 不发事件（留给 settle_steps 收尾）
    #[test]
    fn plan_progress_marks_steps_by_files() {
        let mut overlay = BTreeMap::new();
        overlay.insert("m.py".to_string(), "x".to_string());
        let steps = [
            plan_step(1, "写模块", &["m.py"], "code"),
            plan_step(2, "写模块+测试", &["m.py", "t.py"], "test"),
            plan_step(3, "同步文档", &["docs/readme.md"], "docs"),
        ];
        let sink = StepSink::default();
        emit_step_progress(&sink, &steps, &overlay);
        let ev = sink.events();
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!((ev[0].0, ev[0].2.as_str()), (1, "done"));
        assert_eq!((ev[1].0, ev[1].2.as_str()), (2, "running")); // 1/2
    }

    /// 收尾不留沙漏：没声明文件的步骤交付即完成；声明了却没产出的落成"本次未完成"并写明缺什么
    #[test]
    fn settle_steps_closes_every_hourglass() {
        let mut overlay = BTreeMap::new();
        overlay.insert("pom.xml".to_string(), "x".to_string());
        // 复刻实测那一次 run（agent-20260920-091436）：只暂存了 pom.xml，
        // 第 3 步「同步文档」声明的文档文件零产出 —— 修前界面永远 2/3 + ⌛
        let steps = [
            plan_step(1, "加 Spring Security", &["pom.xml"], "code"),
            plan_step(2, "编译验证依赖解析", &[], "verify"),
            plan_step(3, "同步文档", &["CLAUDE.md"], "docs"),
        ];
        let sink = StepSink::default();
        settle_steps(&sink, &steps, &overlay, true, &[]);
        let ev = sink.events();
        assert_eq!(ev.len(), 3, "每个步骤都要落定，否则界面留沙漏：{ev:?}");
        assert_eq!(ev[0].2, "done");
        assert_eq!(ev[1].2, "done", "没有文件级判据的步骤，交付即算完成");
        assert_eq!(ev[2].2, "skipped");
        assert!(ev[2].3.contains("CLAUDE.md"), "缺什么要写清楚：{}", ev[2].3);
        assert_eq!(ev[2].1, 3, "total 要带上，UI 才能算 x/y");
    }

    /// 不是交付收场（轮次上限 / 取消 / 致命错误）：没做完的不算完成，
    /// 但**已经落地全部声明文件的步骤不能被降级**（取消前它就做完了）
    #[test]
    fn settle_steps_never_claims_done_without_delivery() {
        let mut overlay = BTreeMap::new();
        overlay.insert("m.py".to_string(), "x".to_string());
        let steps = [
            plan_step(1, "写模块", &["m.py"], "code"),
            plan_step(2, "同步文档", &[], "docs"),
        ];
        let sink = StepSink::default();
        settle_steps(&sink, &steps, &overlay, false, &[]);
        let ev = sink.events();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].2, "done", "文件已全部落地，取消不该把它降级");
        assert_eq!(ev[1].2, "skipped");
        assert!(ev[1].3.contains("未完成"), "{}", ev[1].3);
    }

    // ============================================
    // execute_plan：父循环的一轮 = 一个计划步骤
    // ============================================

    /// 打开 `execute_plan` 时的完整闭环：模型给计划 → 引擎按序派发两个步骤 → 模型交付。
    ///
    /// 这条用例盯三件事（都不是断言语义，而是断言**实际发生了什么**）：
    /// ① 调度权在引擎手里：模型从头到尾没说"做第几步"，两步仍按 1→2 跑完；
    /// ② 上下文真的隔离：子步骤写进文件的内容（MARKER）绝不能出现在父的请求体里；
    /// ③ UI 收到的是 running→done 两拍，而不是从"文件是否落地"倒推。
    #[test]
    fn execute_plan_runs_each_step_in_its_own_context() {
        const MARKER: &str = "step-file-secret-a41f";
        let llm = crate::testllm::fake_llm(vec![
            // 父第 1 轮：一份两步计划
            r#"{"tool":"plan","args":{"steps":[
                {"title":"写模块","detail":"写 m.py","files":["m.py"]},
                {"title":"写测试","detail":"写 t.py","files":["t.py"]}]}}"#
                .into(),
            // 子步骤 1：写文件 → 交回
            format!(r#"{{"tool":"write","args":{{"path":"m.py","content":"{MARKER}"}}}}"#),
            r#"{"final":"m.py 写好了"}"#.into(),
            // 子步骤 2：写文件 → 交回
            r#"{"tool":"write","args":{"path":"t.py","content":"t = 1"}}"#.into(),
            r#"{"final":"t.py 写好了"}"#.into(),
            // 父最后的交付
            r#"{"final":"两步都完成了"}"#.into(),
        ]);

        let dir = TempDir::new("plan-exec");
        let mut cfg = AppConfig::default();
        cfg.llm.base_url = llm.base_url.clone();
        cfg.llm.api_key = "smoke".into();
        cfg.llm.model = "fake".into();
        cfg.step.execute_plan = true;
        // 与本用例无关的重活全部关掉：窄验证要起 python/node，全量验证要沙箱，复核要再
        // 烧一轮模型调用 —— 任一开着都会把"请求第几条"的断言搅乱
        cfg.gate.narrow = false;
        cfg.gate.full = false;
        cfg.reflect.enabled = false;

        let sink = StepSink::default();
        let out = block_on(run(
            &cfg,
            &dir.0,
            "把 m.py 和 t.py 写出来",
            &[],
            WritePolicy::Apply,
            &NoConnector,
            &crate::exec::new_cancel_flag(),
            &sink,
        ))
        .expect("run 不该失败");

        assert_eq!(out.answer, "两步都完成了");
        assert_eq!(
            out.changes.len(),
            2,
            "两个子步骤各写了一个文件：{:?}",
            out.changes
        );

        // ① 派发顺序 + UI 事件：running→done，两步依次
        let ev = sink.events();
        let head: Vec<(usize, String)> = ev.iter().take(4).map(|e| (e.0, e.2.clone())).collect();
        assert_eq!(
            head,
            vec![
                (1, "running".to_string()),
                (1, "done".to_string()),
                (2, "running".to_string()),
                (2, "done".to_string()),
            ],
            "全部事件：{ev:?}"
        );
        assert_eq!(ev.len(), 6, "收尾时每个步骤再落一次终态：{ev:?}");

        // ② 隔离的真判据：父轮次 = 1(plan) + 2(两个步骤) + 1(final) = 4，最后一个请求
        //    就是父在交付前看到的东西。子步骤写进文件的内容**不该**出现在里面。
        assert_eq!(llm.count(), 6, "父 4 轮 + 子步骤各 2 轮");
        let last = llm.request(5);
        assert!(last.contains("plan"), "父该记得那份计划：{last}");
        assert!(
            !last.contains(MARKER),
            "子步骤写进文件的内容漏进了父上下文 —— 隔离没生效：{last}"
        );
    }

    /// 步骤失败：落 ❌ 并**停下交回模型一轮**（而不是闷头跑下一步）。
    /// `error` 这个步骤状态在 agent 路径上原先不可达，这里是它第一个正当来源。
    #[test]
    fn execute_plan_hands_a_failed_step_back_to_the_model() {
        let llm = crate::testllm::fake_llm(vec![
            r#"{"tool":"plan","args":{"steps":[
                {"title":"第一步","files":["a.py"]},
                {"title":"第二步","files":["b.py"]}]}}"#
                .into(),
            // 子步骤 1 只写了文件、没交回 —— `step.max_steps = 1` 让它立刻预算用尽
            r#"{"tool":"write","args":{"path":"a.py","content":"a = 1"}}"#.into(),
            // 父的干预轮：模型选择直接交付
            r#"{"final":"只做完第一步，第二步没做"}"#.into(),
        ]);

        let dir = TempDir::new("plan-fail");
        let mut cfg = AppConfig::default();
        cfg.llm.base_url = llm.base_url.clone();
        cfg.llm.api_key = "smoke".into();
        cfg.llm.model = "fake".into();
        cfg.step.execute_plan = true;
        cfg.step.max_steps = 1;
        cfg.gate.narrow = false;
        cfg.gate.full = false;
        cfg.reflect.enabled = false;

        let sink = StepSink::default();
        let out = block_on(run(
            &cfg,
            &dir.0,
            "写两个文件",
            &[],
            WritePolicy::Apply,
            &NoConnector,
            &crate::exec::new_cancel_flag(),
            &sink,
        ))
        .expect("run 不该失败");

        assert_eq!(out.answer, "只做完第一步，第二步没做");

        let st: Vec<(usize, String)> = sink.events().iter().map(|e| (e.0, e.2.clone())).collect();
        assert_eq!(
            st,
            vec![
                (1, "running".to_string()),
                (1, "error".to_string()),
                // 收尾再落一次终态（settle_steps 的契约）
                (1, "error".to_string()),
                // 第二步从没被派发过（游标停在失败那一步），收尾如实标"未完成"
                (2, "skipped".to_string()),
            ],
            "失败步骤必须留痕，未执行的步骤也不能留沙漏"
        );

        // 干预轮真的把失败事实递给了模型
        assert_eq!(llm.count(), 3, "父 3 轮：plan / 干预 / final");
        let intervene = llm.request(2);
        assert!(intervene.contains("执行失败"), "{intervene}");
        assert!(intervene.contains("第一步"), "{intervene}");
    }

    #[test]
    fn tail_history_filters_and_caps() {
        let mk = |role: &str, text: &str| HistoryMsg {
            role: role.into(),
            text: text.into(),
        };
        let h = vec![
            mk("system", "x"),
            mk("user", "a"),
            mk("assistant", ""),
            mk("assistant", "b"),
            mk("user", "c"),
        ];
        let t = tail_history(&h, 2);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].text, "b");
        assert_eq!(t[1].text, "c");
    }
}
