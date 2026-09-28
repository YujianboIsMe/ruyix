//! Connect 原语：**引擎定契约，宿主接外部系统**。
//!
//! 引擎（零 tauri）只定义形状与纪律 —— `Connector::list/call` 两个方法 + 四个结构体；
//! 连什么是宿主环境的事（子进程、HTTP、三 scope 配置）。宿主没接任何外部能力时用
//! [`NoConnector`]（清单为空 ⇒ 提示词里不出现连接段）。
//!
//! ⚠️ 这段 banner 以前叫「Connect 原语」，但底下还塞着**整套提示词**（`AGENT_SYSTEM` /
//! `batch_hint` / `ask_hint` …）。2026-09-28 拆 `agent.rs` 时把提示词分出去成了 `prompt.rs`
//! —— 名字跟着内容走，一处 banner 不再覆盖两个域。

use super::*;

// （原 `agent.rs` 的 banner：Connect 原语：引擎定契约，宿主接外部系统）

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
pub(crate) fn connect_note(targets: &[ConnectTarget]) -> Option<String> {
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
