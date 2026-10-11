//! Connect 原语的宿主实现：把 IDE 已登记的**外部能力**接到 Agent 工具循环上。
//!
//! 引擎（零 tauri）只定 `agent::Connector` 契约，这里用三类真实连接落地：
//! - **MCP**（`mcp.toml`）：没连上先握手启动（与 `mcp_start` 同一条路径），再 `tools/call`；
//! - **A2A**（`a2a.toml`）：`message/send` 委托 + 轮询到终态，进度经 `a2a://status` 事件推给 UI
//!   （与 `a2a_send` 命令同一条事件，a2a 面板照旧能看到这次委托）；
//! - **env**（`kind = "env"`，见 [`super::env_setup`]）：缺失的命令行工具按需安装。它**不是**
//!   第五种原语，也不占新的 connect 形态 —— 引擎侧零改动，全靠清单里多摆一个目标 + 这里认它。
//!
//! 为什么留在 ruyix 侧：连接是宿主环境的事（子进程、HTTP、三 scope 配置）。引擎搬到别的宿主
//! 不该被 MCP 客户端与 a2a 表绑住 —— 换一个 `Connector` 实现即可。
//!
//! 纪律：清单只报**已登记且启用**的能力（不扫盘、不猜名字）；失败分两层 —— 目标不存在 /
//! 起不来是 `Err`（提示词那侧点名让模型改路子），外部系统自己报的业务错误是
//! `ConnectOutcome::is_error`（信息，交回模型判断下一步）。

use crate::a2a;
use crate::agent::env_setup;
use crate::mcp::{self, McpManager};
use engine::agent::{ConnectFuture, ConnectOutcome, ConnectRequest, ConnectTarget, Connector};
use harness_engine as engine;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Emitter;

/// 宿主连接器：ruyix 的外部能力 = MCP 服务器 + A2A 远端 Agent + 环境准备（env）
pub struct RuyixConnector<'a> {
    /// 事件通道。生产路径给句柄（A2A 委托进度要播给面板）；无 UI 时纯连接不给播报
    app: Option<AppHandle>,
    mcp: &'a McpManager,
    project_root: Option<&'a str>,
    /// 宿主裁量：是否允许按需安装缺失的命令行工具（`kind = "env"` 的连接）。
    /// 关掉后清单里不再出现 env 目标 —— 模型看不到就不会请求，引擎侧无需再判。
    env_install: bool,
}

impl<'a> RuyixConnector<'a> {
    /// 生产路径：带句柄，A2A 进度经 `a2a://status` 播给 UI
    pub fn new(
        app: &AppHandle,
        mcp: &'a McpManager,
        project_root: Option<&'a str>,
        env_install: bool,
    ) -> Self {
        Self {
            app: Some(app.clone()),
            mcp,
            project_root,
            env_install,
        }
    }

    /// 无 UI 路径：连接照做，只是没有事件可播。
    /// `#[cfg(test)]`：只有 live 测试需要绕开 Tauri 句柄，生产路径永远从 `new` 进。
    #[cfg(test)]
    pub fn headless(mcp: &'a McpManager, project_root: Option<&'a str>, env_install: bool) -> Self {
        Self {
            app: None,
            mcp,
            project_root,
            env_install,
        }
    }

    /// A2A 委托进度（与 `a2a_send` 命令同一条事件、同一份 payload）
    fn emit_status(&self, name: &str, state: &str, poll: usize, max: usize) {
        let Some(app) = &self.app else {
            return;
        };
        let _ = app.emit(
            "a2a://status",
            serde_json::json!({"name": name, "state": state, "poll": poll, "max": max}),
        );
    }

    /// MCP 一次调用：登记表里找到服务器 → 没连上先握手 → tools/call
    async fn call_mcp(&self, req: &ConnectRequest) -> Result<ConnectOutcome, String> {
        let cfg = mcp::load_servers(self.project_root)
            .into_iter()
            .find(|s| s.name == req.server)
            .ok_or_else(|| {
                format!(
                    "没有登记名为 {} 的 MCP 服务器（用 connect 的 list 看可连的服务器）",
                    req.server
                )
            })?;
        if !cfg.enabled {
            return Err(format!("MCP 服务器已停用: {}", cfg.name));
        }
        // 幂等：连不上（未启动 / 已崩溃）就先走一遍握手
        if self.mcp.tools(&req.server).await.is_err() {
            self.mcp.start(&cfg).await?;
        }
        let args = if req.arguments.is_null() {
            "{}".to_string()
        } else {
            serde_json::to_string(&req.arguments).map_err(|e| format!("参数序列化失败: {e}"))?
        };
        let out = self.mcp.call_tool(&req.server, &req.tool, &args).await?;
        Ok(ConnectOutcome {
            text: out.text,
            is_error: out.is_error,
        })
    }

    /// A2A 委托：等远端跑完才回来，轮询进度随时推给面板
    async fn send_a2a(&self, req: &ConnectRequest) -> Result<ConnectOutcome, String> {
        let cfg = a2a::load_agents(self.project_root)
            .into_iter()
            .find(|a| a.name == req.agent)
            .ok_or_else(|| {
                format!(
                    "没有登记名为 {} 的远端 Agent（用 connect 的 list 看可连的 Agent）",
                    req.agent
                )
            })?;
        let name = cfg.name.clone();
        let out = a2a::send_task(&cfg, &req.text, |state, poll, max| {
            self.emit_status(&name, state, poll, max);
        })
        .await?;
        self.emit_status(&out.agent, &out.state, 0, 0);
        Ok(ConnectOutcome {
            text: format!("[{} 状态：{}]\n{}", out.agent, out.state, out.text),
            is_error: out.state != "completed",
        })
    }

    /// env 连接的 `install` 工具：按需装一个缺失的命令行工具。
    ///
    /// 装什么/怎么装全是宿主裁量（[`env_setup`]），这里只做三件事：校验请求、把阻塞安装丢进
    /// `spawn_blocking`（包下载可能要几分钟，别堵异步运行时）、把结果播给 UI 并回给模型。
    async fn install_env(&self, req: &ConnectRequest) -> Result<ConnectOutcome, String> {
        if !self.env_install {
            return Err(
                "宿主未启用环境安装（配置 ruyix.code.harness.env.install_enabled = false）".into(),
            );
        }
        if req.tool != env_setup::TOOL_INSTALL {
            return Err(format!(
                "env 连接只有 {} 一个工具，收到 {:?}（用 connect 的 list 看清单）",
                env_setup::TOOL_INSTALL,
                req.tool
            ));
        }
        let tool = req
            .arguments
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if tool.is_empty() {
            return Err(
                "install 需要参数 tool（要装的命令名，如 {\"tool\":\"mvn\",\"reason\":\"…\"}）"
                    .into(),
            );
        }
        let reason = req
            .arguments
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root = self
            .project_root
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let rec = tokio::task::spawn_blocking(move || env_setup::install(&root, &tool, &reason))
            .await
            .map_err(|e| format!("安装任务失败: {e}"))?;
        if let Some(app) = &self.app {
            let _ = app.emit(
                "env://install",
                serde_json::json!({
                    "tool": rec.tool,
                    "manager": rec.manager,
                    "cmd": rec.cmd,
                    "ok": rec.ok,
                    "exit_code": rec.exit_code,
                }),
            );
        }
        Ok(ConnectOutcome {
            text: rec.text(),
            is_error: !rec.ok,
        })
    }
}

/// env 目标的清单文案：`detail` + `tools`。抽成函数是为了能被环境安装模块的单测断言
/// （模型照 `tools` 里的参数形状拼 `arguments`，写错了整条能力就废了）。
pub(crate) fn env_target_text() -> (String, Vec<String>) {
    let detail = match env_setup::pick_manager() {
        Some(pm) => format!(
            "本机缺失的命令行工具可按需安装（宿主用 {} 装，每次动作留记录）",
            pm.name()
        ),
        None => {
            "本机缺失的命令行工具可按需安装；暂未探测到包管理器，请求会被拒绝并说明原因".to_string()
        }
    };
    let tools = vec![format!(
        "{}(装一个缺失的命令行工具，参数 {{\"tool\":\"mvn\",\"reason\":\"为什么需要\"}}）",
        env_setup::TOOL_INSTALL
    )];
    (detail, tools)
}

impl Connector for RuyixConnector<'_> {
    fn list(&self) -> ConnectFuture<'_, Vec<ConnectTarget>> {
        Box::pin(async move {
            let mut out: Vec<ConnectTarget> = Vec::new();
            for st in self.mcp.status(self.project_root).await {
                if !st.enabled {
                    continue; // 停用的服务器不进清单：模型照着清单挑，别让它挑到禁用项
                }
                let tools = if st.running {
                    self.mcp
                        .tools(&st.name)
                        .await
                        .map(|ts| ts.iter().map(tool_label).collect())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                out.push(ConnectTarget {
                    kind: "mcp".into(),
                    name: st.name.clone(),
                    detail: if st.running {
                        format!("已连接（{}）", st.server_info.trim())
                    } else {
                        format!("未连接，调用时自动启动（{}）", st.command)
                    },
                    tools,
                });
            }
            for a in a2a::load_agents(self.project_root) {
                out.push(ConnectTarget {
                    kind: "a2a".into(),
                    name: a.name.clone(),
                    detail: a2a_detail(&a),
                    tools: Vec::new(),
                });
            }
            // 环境准备：宿主裁量开不开。开着才摆出来 —— 模型照清单挑，看不见就不会请求。
            if self.env_install {
                let (detail, tools) = env_target_text();
                out.push(ConnectTarget {
                    kind: engine::agent::ENV_CONNECTOR_KIND.into(),
                    name: env_setup::ENV_SERVER.into(),
                    detail,
                    tools,
                });
            }
            Ok(out)
        })
    }

    fn call(&self, req: ConnectRequest) -> ConnectFuture<'_, ConnectOutcome> {
        Box::pin(async move {
            match req.action.as_str() {
                // env 不是 MCP 服务器，先按目标名分流，别落进 call_mcp 的"没登记"分支
                "call" if req.server == env_setup::ENV_SERVER => self.install_env(&req).await,
                "call" => self.call_mcp(&req).await,
                "send" => self.send_a2a(&req).await,
                other => Err(format!(
                    "不支持的 connect.action: {other:?}（只有 call / send；list 走引擎自处理）"
                )),
            }
        })
    }
}

/// 工具名 + 压成一行的描述（清单要塞进提示词，多行 markdown 描述会把清单撑散）
fn tool_label(t: &mcp::ToolInfo) -> String {
    let desc = t
        .description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if desc.is_empty() {
        return t.name.clone();
    }
    // clip 的省略标记本身带换行，裁完再压一次，保证"一条目标一行"
    let clipped = engine::exec::clip(&desc, 80);
    format!(
        "{}({})",
        t.name,
        clipped.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

/// 远端 Agent 的一行摘要：端点 + 描述 + agent card 里声明的技能
fn a2a_detail(a: &a2a::A2aAgentCfg) -> String {
    let mut s = a.url.clone();
    if !a.description.trim().is_empty() {
        s.push_str(&format!(" — {}", a.description.trim()));
    }
    if !a.skills.is_empty() {
        s.push_str(&format!("；技能：{}", a.skills.join("、")));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, description: &str) -> mcp::ToolInfo {
        mcp::ToolInfo {
            name: name.into(),
            description: description.into(),
        }
    }

    /// 最小 MCP 服务器（initialize / tools/list / tools/call 回显），与 mcp.rs 的 live 测试同款
    const PY_ECHO_SERVER: &str = r#"
import json, sys
def send(o): print(json.dumps(o), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m: continue
    method = m.get("method", "")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"py-echo","version":"1.0"}}})
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"tools":[{"name":"echo","description":"回显文本\n 支持中文"}]}})
    elif method == "tools/call":
        text = m["params"]["arguments"].get("text","")
        send({"jsonrpc":"2.0","id":m["id"],"result":{"content":[{"type":"text","text":"echo: "+text}],"isError":False}})
"#;

    /// Connect 的 MCP 通路真·往返（live）：登记 → 未连接时自动握手 → tools/call 拿到结果。
    /// 默认忽略：`cargo test -p ruyix live_mcp_connect -- --ignored --nocapture`
    #[test]
    #[ignore = "live 测试：需要本机 python"]
    fn live_mcp_connect_lists_and_calls() {
        let root = std::env::temp_dir().join(format!("ruyix-connect-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root_str = root.to_string_lossy().to_string();
        mcp::save_server(
            &mcp::McpServerCfg {
                name: "live-echo".into(),
                command: "python".into(),
                args: vec!["-c".into(), PY_ECHO_SERVER.into()],
                env: std::collections::HashMap::new(),
                enabled: true,
            },
            Some(&root_str),
        )
        .unwrap();

        let mgr = McpManager::new();
        let conn = RuyixConnector::headless(&mgr, Some(&root_str), false);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // 清单：登记了但还没连 → 报"未连接，调用时自动启动"，且不带工具名
        let targets = rt.block_on(conn.list()).unwrap();
        let echo = targets
            .iter()
            .find(|t| t.name == "live-echo")
            .expect("清单里应有 live-echo");
        assert!(echo.detail.contains("未连接"), "{}", echo.detail);
        assert!(echo.tools.is_empty(), "未连接时报不出工具名");

        // 调用：连接器自己握手启动，再 tools/call —— 真走 stdio JSON-RPC
        let out = rt
            .block_on(conn.call(ConnectRequest {
                action: "call".into(),
                server: "live-echo".into(),
                tool: "echo".into(),
                arguments: serde_json::json!({"text": "你好"}),
                ..Default::default()
            }))
            .unwrap();
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(out.text, "echo: 你好");

        // 连上之后清单该变成"已连接"并带出压成一行的工具名
        let targets = rt.block_on(conn.list()).unwrap();
        let echo = targets.iter().find(|t| t.name == "live-echo").unwrap();
        assert!(echo.detail.contains("已连接"), "{}", echo.detail);
        assert_eq!(echo.tools, vec!["echo(回显文本 支持中文)"]);

        // 连不存在的服务器 = Err（提示词那侧记失败轮，让模型改路子）
        let err = rt
            .block_on(conn.call(ConnectRequest {
                action: "call".into(),
                server: "ghost".into(),
                tool: "x".into(),
                ..Default::default()
            }))
            .unwrap_err();
        assert!(err.contains("没有登记名为 ghost"), "{err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 工具名进提示词：带描述、描述压成一行、超长裁剪（服务器描述常带多行 markdown）
    #[test]
    fn tool_label_is_one_line_and_clipped() {
        assert_eq!(tool_label(&tool("echo", "  ")), "echo");
        assert_eq!(
            tool_label(&tool("read_file", "读文件\n  支持 offset")),
            "read_file(读文件 支持 offset)"
        );
        let long = tool_label(&tool("x", &"长".repeat(200)));
        assert!(long.starts_with("x("), "{long}");
        assert!(!long.contains('\n'), "裁剪标记的换行也要压掉：{long}");
        assert!(
            long.chars().count() <= 90,
            "必须裁剪，实际 {} 字",
            long.chars().count()
        );
    }

    #[test]
    fn a2a_detail_appends_description_and_skills() {
        let mut a = a2a::A2aAgentCfg {
            name: "translator".into(),
            url: "http://127.0.0.1:9999".into(),
            description: String::new(),
            version: String::new(),
            skills: Vec::new(),
        };
        assert_eq!(a2a_detail(&a), "http://127.0.0.1:9999");
        a.description = "  翻译服务 ".into();
        a.skills = vec!["translate".into()];
        assert_eq!(
            a2a_detail(&a),
            "http://127.0.0.1:9999 — 翻译服务；技能：translate"
        );
    }

    /// env 目标：开关决定它出不出现在清单里（关掉 = 模型彻底看不见）。
    #[test]
    fn env_target_is_listed_only_when_enabled() {
        let root = std::env::temp_dir().join(format!("ruyix-conn-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root_str = root.to_string_lossy().to_string();
        let mgr = McpManager::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let off = RuyixConnector::headless(&mgr, Some(&root_str), false);
        let targets = rt.block_on(off.list()).unwrap();
        assert!(
            targets
                .iter()
                .all(|t| t.kind != engine::agent::ENV_CONNECTOR_KIND),
            "关掉后不该出现 env 目标"
        );

        let on = RuyixConnector::headless(&mgr, Some(&root_str), true);
        let targets = rt.block_on(on.list()).unwrap();
        let env = targets
            .iter()
            .find(|t| t.kind == engine::agent::ENV_CONNECTOR_KIND)
            .expect("开着时应列出 env");
        assert_eq!(env.name, env_setup::ENV_SERVER);
        assert!(
            env.tools.iter().any(|t| t.starts_with("install(")),
            "{:?}",
            env.tools
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// env 的 install：三种坏请求各在**动手之前**被挡（没有一个测试真去装软件）。
    #[test]
    fn env_install_rejects_bad_requests() {
        let root = std::env::temp_dir().join(format!("ruyix-conn-env2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root_str = root.to_string_lossy().to_string();
        let mgr = McpManager::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let conn = RuyixConnector::headless(&mgr, Some(&root_str), true);
        let off = RuyixConnector::headless(&mgr, Some(&root_str), false);

        let req = |tool: &str, args: serde_json::Value| ConnectRequest {
            action: "call".into(),
            server: env_setup::ENV_SERVER.into(),
            tool: tool.into(),
            arguments: args,
            ..Default::default()
        };

        // 开关关着 → 直接拒（即使名字对）
        let err = rt
            .block_on(off.call(req(
                env_setup::TOOL_INSTALL,
                serde_json::json!({"tool": "mvn"}),
            )))
            .unwrap_err();
        assert!(err.contains("未启用"), "{err}");

        // 工具名不对
        let err = rt
            .block_on(conn.call(req("nope", serde_json::Value::Null)))
            .unwrap_err();
        assert!(err.contains("只有 install"), "{err}");

        // 少了 tool 参数
        let err = rt
            .block_on(conn.call(req(env_setup::TOOL_INSTALL, serde_json::json!({}))))
            .unwrap_err();
        assert!(err.contains("需要参数 tool"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
