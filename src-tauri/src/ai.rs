use crate::config::{ConfigManager, Scope};
use serde::Serialize;
use std::sync::Mutex;

/// 系统提示词：编译期嵌入 command.md，零运行时开销
const SYSTEM_PROMPT: &str = include_str!("command.md");

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(serde::Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(serde::Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

/// 从配置中读取 AI 设置，依次尝试 runtime → project → global
fn read_ai_config(
    mgr: &ConfigManager,
    key: &str,
    project_root: Option<&str>,
) -> Option<String> {
    for scope in [Scope::Runtime, Scope::Project, Scope::Global] {
        if scope == Scope::Project && project_root.is_none() {
            continue;
        }
        if let Ok(Some(val)) = mgr.config_read(&scope, key, project_root) {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// 读取 AI 配置（api_url, api_key, model）
fn load_ai_config(
    mgr: &ConfigManager,
    project_root: Option<&str>,
) -> Result<(String, String, String), String> {
    let api_url = read_ai_config(mgr, "darkhorse.code.ai.api_url", project_root)
        .unwrap_or_else(|| "https://api.deepseek.com/v1".to_string());
    let api_url = if api_url.ends_with("/chat/completions") {
        api_url
    } else {
        format!("{}/chat/completions", api_url.trim_end_matches('/'))
    };

    let api_key = read_ai_config(mgr, "darkhorse.code.ai.api_key", project_root).ok_or_else(|| {
        "请先配置 API Key:\n\
         config add -g darkhorse.code.ai.api_key <你的密钥>"
            .to_string()
    })?;

    let model = read_ai_config(mgr, "darkhorse.code.ai.model", project_root)
        .unwrap_or_else(|| "deepseek-chat".to_string());

    Ok((api_url, api_key, model))
}

/// 调用 LLM 将自然语言翻译为标准命令
pub async fn translate(
    config_mgr: &Mutex<ConfigManager>,
    project_root: Option<&str>,
    input: &str,
) -> Result<String, String> {
    let (api_url, api_key, model, lang) = {
        let mgr = config_mgr.lock().map_err(|e| format!("配置锁失败: {}", e))?;
        let (api_url, api_key, model) = load_ai_config(&mgr, project_root)?;
        let lang = read_ai_config(&mgr, "darkhorse.code.ui.lang", project_root)
            .unwrap_or_else(|| "zh-CN".to_string());
        (api_url, api_key, model, lang)
    };

    // 打开项目时，将项目根路径作为上下文注入到用户消息中
    let context_prefix = if lang == "en" { "Current project path: " } else { "当前项目路径: " };
    let user_message = match project_root {
        Some(root) => format!("{}{}\n\n{}", context_prefix, root, input),
        None => input.to_string(),
    };

    let client = reqwest::Client::new();
    let req_body = ChatRequest {
        model,
        messages: vec![
            ChatMessage { role: "system".to_string(), content: SYSTEM_PROMPT.to_string() },
            ChatMessage { role: "user".to_string(), content: user_message },
        ],
        temperature: 0.1,
        max_tokens: 300,
    };

    let resp = client.post(&api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&req_body).send().await
        .map_err(|e| format!("网络请求失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API 返回错误 ({}): {}", status.as_u16(), body));
    }

    let chat_resp: ChatResponse = resp.json().await.map_err(|e| format!("解析响应失败: {}", e))?;
    let content = chat_resp.choices.first()
        .map(|c| c.message.content.trim().to_string())
        .unwrap_or_else(|| "不支持的操作：AI 未返回有效响应".to_string());
    Ok(content)
}

/// 询问 LLM：该文件是否可以运行
pub async fn check_executable(
    config_mgr: &Mutex<ConfigManager>,
    path: &str,
) -> Result<String, String> {
    let (api_url, api_key, model) = {
        let mgr = config_mgr.lock().map_err(|e| format!("配置锁失败: {}", e))?;
        load_ai_config(&mgr, None)?
    };

    let p = std::path::Path::new(path);
    let file_name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let prompt = format!(
        "文件路径: {}\n文件名: {}\n扩展名: .{}\n\n这个文件可以运行/执行吗？请只回复一个选项:\n- FILE_YES|<运行命令模板，用 {{file}} 指代文件> — 仅因文件名而可运行的清单/构建文件（如 Cargo.toml、package.json、Makefile）\n- YES|<运行命令模板> — 因其后缀类型而可运行的脚本（如 .py）\n- CONDITIONAL|<条件说明> — 可运行但需要特定条件\n- NO — 不可运行",
        path, file_name, ext
    );

    let client = reqwest::Client::new();
    let req_body = ChatRequest {
        model,
        messages: vec![
            ChatMessage { role: "system".to_string(), content: "你是一个文件执行分析器。判断文件是否可以运行。".to_string() },
            ChatMessage { role: "user".to_string(), content: prompt },
        ],
        temperature: 0.0,
        max_tokens: 150,
    };

    let resp = client.post(&api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&req_body).send().await
        .map_err(|e| format!("网络请求失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API 返回错误 ({}): {}", status.as_u16(), body));
    }

    let chat_resp: ChatResponse = resp.json().await.map_err(|e| format!("解析响应失败: {}", e))?;
    let content = chat_resp.choices.first()
        .map(|c| c.message.content.trim().to_string())
        .unwrap_or_default();
    Ok(content)
}