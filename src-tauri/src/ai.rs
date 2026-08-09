use crate::config::{ConfigManager, Scope};
use serde::Serialize;
use std::sync::Mutex;

/// 系统提示词：教会 LLM darkhorse-code 的命令系统
const SYSTEM_PROMPT: &str = r#"你是一个命令翻译器。将用户的自然语言输入翻译为 darkhorse-code IDE 的标准命令，每行一条。

# 标准命令

open project <项目文件夹路径>
  打开项目。路径需是绝对路径，如 D:\projects\my-app

open file <文件路径>
  打开文件。如果用户只说文件名，你需要根据上下文推断完整路径

close project
  关闭当前项目

close all
  关闭所有标签页

close <序号>
  关闭指定序号的标签页（从0开始从左到右，负数为从右数，-1=最后一个）

close other
  关闭除当前活动标签页外的所有标签页

close left
  关闭当前标签页左侧的所有标签页

close right
  关闭当前标签页右侧的所有标签页

config add [-g|-p|-r] <key> <value>
  添加配置。key格式: darkhorse.code.<section>.<key>，作用域默认 -r

config get [-g|-p|-r] <key>
  读取配置

config update [-g|-p|-r] <key> <value>
  更新配置

config remove [-g|-p|-r] <key>
  删除配置（同 delete）

# 可用的配置项
darkhorse.code.ai.api_key     AI API密钥
darkhorse.code.ai.api_url     AI API地址
darkhorse.code.ai.model       AI 模型名称
darkhorse.code.ai.alias       AI 别名（不设置则同model）

# 规则
1. 先判断用户意图：是操作请求还是闲聊。
2. 操作请求：只输出标准命令，每行一条，不要用markdown代码块包裹。
3. 如果操作无法实现，输出: 不支持的操作：<原因>
4. 文件路径使用反斜杠或正斜杠均可，保留用户输入的路径。
5. 闲聊：如果回复能控制在20字以内，直接回复（不要加引号）。如果预计超过20字，只回复: 你还是好好工作吧，房贷还清了吗？车贷还清了吗？
6. 闲聊回复字数必须≤20字，不要多写。"#;

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

/// 调用 LLM 将自然语言翻译为标准命令
pub async fn translate(
    config_mgr: &Mutex<ConfigManager>,
    project_root: Option<&str>,
    input: &str,
) -> Result<String, String> {
    let (api_url, api_key, model) = {
        let mgr = config_mgr.lock().map_err(|e| format!("配置锁失败: {}", e))?;

        let api_url = read_ai_config(&mgr, "darkhorse.code.ai.api_url", project_root)
            .unwrap_or_else(|| "https://api.deepseek.com/v1".to_string());
        let api_url = if api_url.ends_with("/chat/completions") {
            api_url
        } else {
            format!("{}/chat/completions", api_url.trim_end_matches('/'))
        };

        let api_key = read_ai_config(&mgr, "darkhorse.code.ai.api_key", project_root)
            .ok_or_else(|| {
                "请先配置 API Key:\n\
                 config add -g darkhorse.code.ai.api_key <你的密钥>"
                    .to_string()
            })?;

        let model = read_ai_config(&mgr, "darkhorse.code.ai.model", project_root)
            .unwrap_or_else(|| "deepseek-chat".to_string());

        (api_url, api_key, model)
    };

    let client = reqwest::Client::new();
    let req_body = ChatRequest {
        model,
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: SYSTEM_PROMPT.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: input.to_string(),
            },
        ],
        temperature: 0.1,
        max_tokens: 300,
    };

    let resp = client
        .post(&api_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&req_body)
        .send()
        .await
        .map_err(|e| format!("网络请求失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("API 返回错误 ({}): {}", status.as_u16(), body));
    }

    let chat_resp: ChatResponse = resp.json().await.map_err(|e| format!("解析响应失败: {}", e))?;

    let content = chat_resp
        .choices
        .first()
        .map(|c| c.message.content.trim().to_string())
        .unwrap_or_else(|| "不支持的操作：AI 未返回有效响应".to_string());

    Ok(content)
}
