//! 脚本化假 LLM：一个最小的 OpenAI 兼容端点，第 i 个请求回 `script[i]`。
//!
//! 为什么放在 lib 里而不是 `#[cfg(test)]`：端到端冒烟（`examples/*.rs`）编译时
//! **看不到** lib 的 test 配置项，而"真跑一遍循环"这种断言两边都要用同一份假 LLM
//! —— 实现一旦分叉，单测与冒烟验的就不是同一件事了（仓库对 pipeline/评估也用的是
//! 同一套"别各写一遍"原则，见 `pipeline::seed_files`）。
//!
//! 用法：
//! ```ignore
//! let llm = testllm::fake_llm(vec![r#"{"tool":"read","args":{"path":"."}}"#.into()]);
//! cfg.llm.base_url = llm.base_url.clone();
//! cfg.llm.api_key = "smoke".into();
//! // 断言"模型实际看到了什么" —— 请求体原文（含 messages）都在这里
//! assert!(llm.request(0).contains("项目根目录"));
//! ```
//!
//! 只依赖 std + serde_json：它要能在单测与示例里同样可用。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// 假 LLM 的句柄：`base_url` 给配置，`request`/`count` 给断言。
pub struct FakeLlm {
    /// 填进 `cfg.llm.base_url`
    pub base_url: String,
    /// 每个请求的原始 body（按到达顺序）
    seen: Arc<Mutex<Vec<String>>>,
    script_len: usize,
}

impl FakeLlm {
    /// 第 i 个请求的原始 body（含 messages）——"模型看到了什么"只有这一个判据。
    /// 越界返回空串（说明循环比剧本多跑了一轮，配合 `count()` 一起断言）。
    pub fn request(&self, i: usize) -> String {
        self.seen
            .lock()
            .unwrap()
            .get(i)
            .cloned()
            .unwrap_or_default()
    }

    /// 实际被调了几次 —— 用来断言"没有多余的轮次"。
    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// 剧本长度（预置了几条回复）
    pub fn script_len(&self) -> usize {
        self.script_len
    }
}

/// 起一个最小 OpenAI 兼容服务：第 i 个请求回 `script[i]`，并把请求体留档给断言看。
/// 剧本演完就收工 —— 之后多出来的请求会拿到连接失败，让"循环多跑了一轮"暴露成
/// 测试失败，而不是被静默吞掉。
///
/// **动作类剧本按工具协议发出**：条目若是老协议形状的动作（`{"tool":…}` /
/// `{"actions":[…]}` / `{"calls":[…]}` / `{"final":…}`），会先翻译成一条 `tool_calls` 响应。
/// 原因：严格模式下 content 通道**不执行**（`llm.tool_protocol` 默认开），测试要验的是主路；
/// 翻译让上百条既有脚本一行不改就继续有效。
///
/// 想"就发这个 content"（兼容层、散文、畸形 JSON、DSML 泄露样本那些用例）用 [`fake_llm_raw`]。
pub fn fake_llm(script: Vec<String>) -> FakeLlm {
    fake_llm_raw(
        script
            .iter()
            .map(|e| legacy_entry_as_tool_call(e))
            .collect(),
    )
}

/// 老协议形状的剧本条目 → `tool_calls` 响应条目（认不出就原样返回）。
fn legacy_entry_as_tool_call(entry: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(entry.trim()) else {
        return entry.to_string();
    };
    let items: Vec<&serde_json::Value> = match &v {
        serde_json::Value::Array(a) => a.iter().collect(),
        serde_json::Value::Object(o) => match o.get("actions").or_else(|| o.get("calls")) {
            Some(serde_json::Value::Array(a)) => a.iter().collect(),
            _ => vec![&v],
        },
        _ => return entry.to_string(),
    };
    let mut list: Vec<serde_json::Value> = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        let Some((name, args)) = tool_name_and_args(it) else {
            return entry.to_string(); // 有一条认不出：整条原样发，别悄悄改剧本
        };
        list.push(serde_json::json!({
            "id": format!("call_{}", i + 1),
            "type": "function",
            "function": { "name": name, "arguments": serde_json::to_string(&args).unwrap() }
        }));
    }
    if list.is_empty() {
        return entry.to_string();
    }
    serde_json::json!({ "tool_calls": list }).to_string()
}

/// 老协议形状的动作 → （工具名，参数）
fn tool_name_and_args(v: &serde_json::Value) -> Option<(String, serde_json::Value)> {
    if let Some(name) = v.get("tool").and_then(|x| x.as_str()) {
        let args = v
            .get("args")
            .or_else(|| v.get("arguments"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        return Some((name.to_string(), args));
    }
    if let Some(ans) = v.get("final").and_then(|x| x.as_str()) {
        return Some(("final".into(), serde_json::json!({ "answer": ans })));
    }
    if v.get("answer").is_some() && v.as_object().map(|o| o.len() == 1).unwrap_or(false) {
        return Some(("final".into(), v.clone()));
    }
    None
}

/// 剧本**原样**当 content 回话（不做任何翻译）—— 兼容层/兜底/畸形输出的用例用这个。
pub fn fake_llm_raw(script: Vec<String>) -> FakeLlm {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定假 LLM 端口");
    let addr = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let served = Arc::new(AtomicUsize::new(0));
    let seen_bg = Arc::clone(&seen);
    let script_len = script.len();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let i = served.fetch_add(1, Ordering::SeqCst);
            if i >= script.len() {
                break; // 剧本演完收工：多出来的请求说明循环比预期多跑了一轮
            }
            let body = read_http_body(&mut stream);
            seen_bg.lock().unwrap().push(body);
            // 剧本条目有两种写法（见 [`tool_script`]）：
            //   · `{"tool_calls":[…]}` → 回一个**工具调用轮**（content 空，动作在 tool_calls 里）
            //   · 其它一切 → 照旧当 content 回话（老剧本一行都不用改）
            let message = match tool_calls_of(&script[i]) {
                Some(calls) => serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": calls,
                }),
                None => serde_json::json!({ "role": "assistant", "content": script[i] }),
            };
            let reply = format!(
                r#"{{"model":"fake","choices":[{{"message":{}}}],"usage":{{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}}}"#,
                serde_json::to_string(&message).unwrap()
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    FakeLlm {
        base_url: format!("http://{addr}"),
        seen,
        script_len,
    }
}

/// 造一条**工具调用轮**的剧本条目（标准工具协议）。
///
/// ```ignore
/// let llm = testllm::fake_llm(vec![
///     testllm::tool_script(&[("read", serde_json::json!({"path": "a.txt"}))]),
///     r#"{"final":"读完了"}"#.into(),
/// ]);
/// ```
///
/// 参数会被**序列化成字符串**（协议要求 `function.arguments` 是 JSON 串），id 自动编号。
/// 做成"造剧本的糖"而不是让测试手写转义串 —— 参数串里嵌套引号是真踩过的坑。
pub fn tool_script(calls: &[(&str, serde_json::Value)]) -> String {
    let list: Vec<serde_json::Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            serde_json::json!({
                "id": format!("call_{}", i + 1),
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(args).unwrap(),
                }
            })
        })
        .collect();
    serde_json::json!({ "tool_calls": list }).to_string()
}

/// 剧本条目是不是"工具调用轮"（`{"tool_calls":[…]}`）：是就取出那串，不是就 `None`。
fn tool_calls_of(entry: &str) -> Option<Vec<serde_json::Value>> {
    let v: serde_json::Value = serde_json::from_str(entry).ok()?;
    let arr = v.get("tool_calls")?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    Some(arr.clone())
}

fn read_http_body(stream: &mut std::net::TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim().is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut buf = vec![0u8; len];
    let _ = reader.read_exact(&mut buf);
    String::from_utf8_lossy(&buf).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// 走**真的客户端**（`llm::chat`）验假 LLM：剧本按序回话、请求体可回读、usage 有个数。
    #[test]
    fn serves_the_script_through_the_real_client() {
        let llm = fake_llm(vec!["第一句".into(), "第二句".into()]);
        assert_eq!(llm.script_len(), 2);
        let cfg = crate::config::LlmConfig {
            base_url: llm.base_url.clone(),
            api_key: "smoke".into(),
            model: "fake".into(),
            ..Default::default()
        };
        for (i, expect) in ["第一句", "第二句"].iter().enumerate() {
            let out = block_on(crate::llm::chat(
                &cfg,
                None,
                &[crate::llm::ChatMessage::user("你好")],
                true,
                false,
            ))
            .expect("假 LLM 该回话");
            assert_eq!(&out.content, expect);
            assert_eq!(out.usage.total_tokens, 15);
            assert!(llm.request(i).contains("你好"), "请求体要留档");
        }
        assert_eq!(llm.count(), 2);
        assert_eq!(llm.request(9), "", "越界读回空串");
    }
}
