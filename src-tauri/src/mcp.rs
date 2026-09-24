//! MCP（Model Context Protocol）客户端 —— stdio 传输。
//!
//! 定位：IDE 作为 MCP **host/client**，连接本地 MCP 服务器子进程（JSON-RPC 2.0，
//! 每行一个 JSON 消息），把服务器的工具暴露给 ruyix（面板查看 / 命令调用）。
//! 协议面只实现工具子集：initialize → tools/list → tools/call（resources/prompts
//! 留待后续）。零新增依赖：子进程 std::process，序列化 serde_json。
//!
//! 配置持久化沿用结构化文件惯例：全局 `<便携根>/global/mcp.toml`，项目级
//! `<便携根>/projects/<key>/mcp.toml`，条目按名字合并（项目覆盖全局同名）。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// MCP 2024-11-05 规范版本（initialize 握手用；服务器不匹配会拒绝）
const PROTOCOL_VERSION: &str = "2024-11-05";
const INIT_TIMEOUT_SECS: u64 = 30;
const REQUEST_TIMEOUT_SECS: u64 = 120;

/// CREATE_NO_WINDOW — 阻止子进程闪黑窗口（同 main.rs 的 PTY/运行语义）
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ============================================
// 配置（mcp.toml：全局 + 项目合并）
// ============================================

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct McpServerCfg {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

fn global_path() -> std::path::PathBuf {
    crate::paths::current().global_dir().join("mcp.toml")
}

fn project_path(project_root: &str) -> std::path::PathBuf {
    crate::paths::current()
        .project_dir(project_root)
        .join("mcp.toml")
}

fn read_file_servers(path: &std::path::Path) -> Vec<McpServerCfg> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        servers: Vec<McpServerCfg>,
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str::<File>(&s).ok())
        .map(|f| f.servers)
        .unwrap_or_default()
}

fn write_file_servers(path: &std::path::Path, servers: &[McpServerCfg]) -> Result<(), String> {
    #[derive(Serialize)]
    struct File<'a> {
        servers: &'a [McpServerCfg],
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let text = toml::to_string_pretty(&File { servers })
        .map_err(|e| format!("序列化 mcp.toml 失败: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

/// 全局 + 项目配置合并（项目覆盖同名条目，保持全局顺序在前）
pub fn load_servers(project_root: Option<&str>) -> Vec<McpServerCfg> {
    let global = read_file_servers(&global_path());
    let project = project_root
        .map(|r| read_file_servers(&project_path(r)))
        .unwrap_or_default();
    merge_lists(global, project)
}

fn merge_lists(global: Vec<McpServerCfg>, project: Vec<McpServerCfg>) -> Vec<McpServerCfg> {
    let mut merged = global;
    for p in project {
        merged.retain(|s| s.name != p.name);
        merged.push(p);
    }
    merged
}

/// 写回配置：条目属于哪个文件就写哪个（项目存在该名字 → 项目级，否则全局）
pub fn save_server(cfg: &McpServerCfg, project_root: Option<&str>) -> Result<(), String> {
    let in_project = project_root
        .map(|r| {
            read_file_servers(&project_path(r))
                .iter()
                .any(|s| s.name == cfg.name)
        })
        .unwrap_or(false);
    let (path, mut list) = if in_project {
        let r = project_root.expect("已判定项目级");
        (project_path(r), read_file_servers(&project_path(r)))
    } else {
        (global_path(), read_file_servers(&global_path()))
    };
    list.retain(|s| s.name != cfg.name);
    list.push(cfg.clone());
    write_file_servers(&path, &list)
}

pub fn remove_server(name: &str, project_root: Option<&str>) -> Result<(), String> {
    let targets: Vec<std::path::PathBuf> = {
        let mut v = vec![global_path()];
        if let Some(r) = project_root {
            v.push(project_path(r));
        }
        v
    };
    let mut touched = 0;
    for path in targets {
        let list = read_file_servers(&path);
        if list.iter().any(|s| s.name == name) {
            let kept: Vec<_> = list.into_iter().filter(|s| s.name != name).collect();
            write_file_servers(&path, &kept)?;
            touched += 1;
        }
    }
    if touched == 0 {
        Err(format!("MCP 服务器不存在: {name}"))
    } else {
        Ok(())
    }
}

// ============================================
// 协议结构
// ============================================

#[derive(Serialize, Clone, Debug)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct ServerStatus {
    pub name: String,
    pub command: String,
    pub enabled: bool,
    pub running: bool,
    pub server_info: String,
    pub tools: usize,
}

#[derive(Serialize, Clone, Debug)]
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

/// 请求/响应匹配表：JSON-RPC id → 响应回传通道（读线程消费）
type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

/// 一条已建立的连接：子进程 + 请求/响应匹配表。
/// Drop 时杀掉子进程 —— IDE 退出不能留下孤儿 MCP 服务器。
struct Conn {
    child: Child,
    stdin: std::process::ChildStdin,
    pending: PendingMap,
    next_id: Arc<AtomicI64>,
    server_info: String,
    tools: Vec<ToolInfo>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
        // 唤醒所有挂起的请求（"服务器已停止"）
        if let Ok(mut map) = self.pending.lock() {
            for (_, tx) in map.drain() {
                let _ = tx.send(Err("MCP 服务器已停止".into()));
            }
        }
    }
}

/// 连接表用异步锁：Tauri async 命令里持锁跨 await（guard 必须 Send）。
pub struct McpManager {
    conns: tokio::sync::Mutex<HashMap<String, Conn>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            conns: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    fn spawn(cfg: &McpServerCfg) -> Result<Conn, String> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .envs(&cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 {} 失败: {e}", cfg.name))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "无法获取子进程 stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "无法获取子进程 stdout".to_string())?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        // 读线程：逐行读 JSON-RPC 响应，按 id 匹配 oneshot；通知消息（无 id）忽略
        let reader_pending = Arc::clone(&pending);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue; // 非 JSON 行（服务器日志）忽略
                };
                let Some(id) = v.get("id").and_then(Value::as_i64) else {
                    continue; // notification
                };
                let tx = reader_pending.lock().ok().and_then(|mut m| m.remove(&id));
                if let Some(tx) = tx {
                    let result = match v.get("error") {
                        Some(err) => Err(format!(
                            "MCP 错误 {}: {}",
                            err.get("code").and_then(Value::as_i64).unwrap_or(0),
                            err.get("message").and_then(Value::as_str).unwrap_or("?")
                        )),
                        None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(result);
                }
            }
            // EOF：服务器退出，失败所有挂起请求
            if let Ok(mut map) = reader_pending.lock() {
                for (_, tx) in map.drain() {
                    let _ = tx.send(Err("MCP 服务器已退出".into()));
                }
            }
        });

        Ok(Conn {
            child,
            stdin,
            pending,
            next_id: Arc::new(AtomicI64::new(1)),
            server_info: String::new(),
            tools: Vec::new(),
        })
    }

    /// 发一个 JSON-RPC 请求并等待响应（带超时）
    async fn request(conn: &mut Conn, method: &str, params: Value) -> Result<Value, String> {
        let id = conn.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
        line.push('\n');
        conn.stdin
            .write_all(line.as_bytes())
            .and_then(|_| conn.stdin.flush())
            .map_err(|e| format!("写入 MCP 服务器失败: {e}"))?;

        let (tx, rx) = oneshot::channel();
        conn.pending
            .lock()
            .map_err(|e| e.to_string())?
            .insert(id, tx);
        let timeout = Duration::from_secs(REQUEST_TIMEOUT_SECS);
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| format!("MCP 请求超时（{REQUEST_TIMEOUT_SECS}s）: {method}"))?
            .map_err(|_| "MCP 响应通道关闭".to_string())?
    }

    /// 启动并完成 MCP 握手（initialize → initialized 通知 → tools/list）
    pub async fn start(&self, cfg: &McpServerCfg) -> Result<ServerStatus, String> {
        let mut conns = self.conns.lock().await;
        if conns.contains_key(&cfg.name) {
            conns.remove(&cfg.name); // Drop 杀旧进程，重连
        }
        let mut conn = Self::spawn(cfg)?;

        let init = tokio::time::timeout(
            Duration::from_secs(INIT_TIMEOUT_SECS),
            Self::request(
                &mut conn,
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "ruyix", "version": env!("CARGO_PKG_VERSION")}
                }),
            ),
        )
        .await
        .map_err(|_| format!("initialize 超时（{INIT_TIMEOUT_SECS}s）"))??;

        conn.server_info = init
            .get("serverInfo")
            .map(|s| {
                format!(
                    "{} {}",
                    s.get("name").and_then(Value::as_str).unwrap_or("?"),
                    s.get("version").and_then(Value::as_str).unwrap_or("")
                )
                .trim()
                .to_string()
            })
            .unwrap_or_default();

        // initialized 通知（无 id，不等待响应）
        let notice = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let mut line = serde_json::to_string(&notice).map_err(|e| e.to_string())?;
        line.push('\n');
        let _ = conn
            .stdin
            .write_all(line.as_bytes())
            .and_then(|_| conn.stdin.flush());

        conn.tools = Self::parse_tools(&Self::request(&mut conn, "tools/list", json!({})).await?);

        let status = ServerStatus {
            name: cfg.name.clone(),
            command: format!("{} {}", cfg.command, cfg.args.join(" "))
                .trim()
                .to_string(),
            enabled: cfg.enabled,
            running: true,
            server_info: conn.server_info.clone(),
            tools: conn.tools.len(),
        };
        conns.insert(cfg.name.clone(), conn);
        Ok(status)
    }

    fn parse_tools(result: &Value) -> Vec<ToolInfo> {
        result
            .get("tools")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|t| ToolInfo {
                        name: t
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                        description: t
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn stop(&self, name: &str) -> bool {
        self.conns.lock().await.remove(name).is_some()
    }

    pub async fn status(&self, project_root: Option<&str>) -> Vec<ServerStatus> {
        let conns = self.conns.lock().await;
        load_servers(project_root)
            .into_iter()
            .map(|cfg| {
                let running = conns.get(&cfg.name);
                ServerStatus {
                    name: cfg.name,
                    command: format!("{} {}", cfg.command, cfg.args.join(" "))
                        .trim()
                        .to_string(),
                    enabled: cfg.enabled,
                    running: running.is_some(),
                    server_info: running.map(|c| c.server_info.clone()).unwrap_or_default(),
                    tools: running.map(|c| c.tools.len()).unwrap_or(0),
                }
            })
            .collect()
    }

    pub async fn tools(&self, name: &str) -> Result<Vec<ToolInfo>, String> {
        self.conns
            .lock()
            .await
            .get(name)
            .map(|c| c.tools.clone())
            .ok_or_else(|| format!("MCP 服务器未连接: {name}（先 mcp_start）"))
    }

    /// 调用工具。args_json 允许空串（无参工具）
    pub async fn call_tool(
        &self,
        name: &str,
        tool: &str,
        args_json: &str,
    ) -> Result<CallOutcome, String> {
        let arguments: Value = if args_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(args_json).map_err(|e| format!("参数 JSON 解析失败: {e}"))?
        };
        let mut conns = self.conns.lock().await;
        let conn = conns
            .get_mut(name)
            .ok_or_else(|| format!("MCP 服务器未连接: {name}"))?;
        let result = Self::request(
            conn,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        )
        .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // content[] 里的 text 块拼接为结果文本（其他类型给占位）
        let mut parts: Vec<String> = Vec::new();
        if let Some(arr) = result.get("content").and_then(Value::as_array) {
            for c in arr {
                match c.get("type").and_then(Value::as_str) {
                    Some("text") => parts.push(
                        c.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ),
                    Some("image") => parts.push("[image]".to_string()),
                    Some("resource") => parts.push(
                        c.get("resource")
                            .and_then(|r| r.get("uri"))
                            .and_then(Value::as_str)
                            .unwrap_or("[resource]")
                            .to_string(),
                    ),
                    _ => parts.push("[unsupported]".to_string()),
                }
            }
        }
        Ok(CallOutcome {
            text: parts.join("\n"),
            is_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, command: &str) -> McpServerCfg {
        McpServerCfg {
            name: name.into(),
            command: command.into(),
            args: vec![],
            env: HashMap::new(),
            enabled: true,
        }
    }

    #[test]
    fn project_servers_override_global_by_name() {
        let merged = merge_lists(
            vec![server("fs", "mcp-fs")],
            vec![server("fs", "project-fs"), server("git", "mcp-git")],
        );
        assert_eq!(merged.len(), 2);
        let fs = merged.iter().find(|s| s.name == "fs").unwrap();
        assert_eq!(fs.command, "project-fs", "项目级应覆盖全局同名");
    }

    #[test]
    fn project_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ruyix-mcp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::paths::set_test_root(&dir);
        write_file_servers(
            &project_path(dir.to_str().unwrap()),
            &[server("fs", "mcp-fs"), server("git", "mcp-git")],
        )
        .unwrap();
        let list = read_file_servers(&project_path(dir.to_str().unwrap()));
        assert_eq!(list.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_tools_handles_missing_fields() {
        let v = json!({"tools": [{"name": "echo", "description": "回显"}, {"name": "x"}]});
        let tools = McpManager::parse_tools(&v);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[1].description, "");
    }

    /// 真·stdio 往返（live）：需要 python。默认忽略：
    /// `cargo test -p ruyix mcp_live -- --ignored`
    #[test]
    #[ignore = "live 测试：需要本机 python"]
    fn mcp_live_python_echo_server() {
        // 最小 MCP 服务器：initialize / tools/list / tools/call(echo)
        let py = r#"
import json, sys
def send(o): print(json.dumps(o), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m: continue
    method = m.get("method", "")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"py-echo","version":"1.0"}}})
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"tools":[{"name":"echo","description":"回显文本"}]}})
    elif method == "tools/call":
        text = m["params"]["arguments"].get("text","")
        send({"jsonrpc":"2.0","id":m["id"],"result":{"content":[{"type":"text","text":"echo: "+text}],"isError":False}})
"#;
        let cfg = McpServerCfg {
            name: "live-echo".into(),
            command: "python".into(),
            args: vec!["-c".into(), py.into()],
            env: HashMap::new(),
            enabled: true,
        };
        let mgr = McpManager::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let st = rt.block_on(mgr.start(&cfg)).unwrap();
        assert!(st.running);
        assert_eq!(st.tools, 1);
        assert!(st.server_info.contains("py-echo"));
        let out = rt
            .block_on(mgr.call_tool("live-echo", "echo", r#"{"text":"你好"}"#))
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.text, "echo: 你好");
        assert!(rt.block_on(mgr.stop("live-echo")));
    }
}
