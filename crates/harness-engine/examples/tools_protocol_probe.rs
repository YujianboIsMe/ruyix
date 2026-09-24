//! 工具协议真机探针：用**真的** `AGENT_SYSTEM` + 真端点打一次，看模型走不走 `tool_calls`。
//!
//! 为什么要有它：单元测试里的假 LLM 只能证明"我们**会**解析 tool_calls"，证明不了
//! "模型面对这份提示词与这份 `tools` 声明**会**走工具协议"。而这次改造的全部依据就是后者
//! （见 `doc/问题-DSML标记泄露.md` 的 4 臂对照）。
//!
//! 跑法（需要 key，不联网/无 key 时直接报错退出）：
//! ```text
//! cargo run -q -p harness-engine --example tools_protocol_probe
//! ```
//! key 来源：`DEEPSEEK_API_KEY` 环境变量，或 `~/.ruyix/code/ai.toml` 的 `api_key`。
//!
//! **判据（预注册）**：打印的 `tool_calls` 数 > 0 → 主路生效；若为 0 但 content 是
//! `{"tool"/"actions"/"final"…}` → 走的是兼容层（也算通过，但迁移进度为 0）；
//! 若 content 里出现模型自带的工具调用标记 → 说明这份 prompt/声明没压住，得回去看 A/B。
use harness_engine::agent::AGENT_SYSTEM;
use harness_engine::config::LlmConfig;
use harness_engine::llm::{ChatMessage, chat, chat_with_tools};

fn key_from_ai_toml() -> String {
    let p = match std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        Ok(h) => format!("{h}/.ruyix/code/ai.toml"),
        Err(_) => return String::new(),
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return String::new();
    };
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == "api_key"
        {
            return v.trim().trim_matches('"').to_string();
        }
    }
    String::new()
}

fn main() {
    let key = std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .unwrap_or_else(key_from_ai_toml);
    assert!(
        !key.is_empty(),
        "没拿到 key：设 DEEPSEEK_API_KEY 或填 ~/.ruyix/code/ai.toml"
    );

    let cfg = LlmConfig {
        base_url: "https://api.deepseek.com/v1".into(),
        model: "deepseek-v4-flash".into(),
        api_key: key,
        ..LlmConfig::default()
    };

    let msgs = vec![
        ChatMessage::system(AGENT_SYSTEM),
        ChatMessage::user(
            "任务：先看 cloud-shop-admin-web 的 package.json，再确认端口 8083 有没有被占。\
             项目根 D:\\Projects\\Java\\cloud-shop。",
        ),
    ];

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // 第 5 个参数 = tools：true 就是这次改造要验的那条路
    let out = rt
        .block_on(chat(&cfg, None, &msgs, true, true))
        .expect("调用失败");

    println!("model        : {}", out.model);
    println!("finish_reason: {:?}", out.finish_reason);
    println!(
        "tokens       : {} + {} = {}",
        out.usage.prompt_tokens, out.usage.completion_tokens, out.usage.total_tokens
    );
    println!("tool_calls   : {}", out.tool_calls.len());
    for c in &out.tool_calls {
        println!("  · {}({})", c.function.name, c.function.arguments);
    }
    println!("content      : {} 字符", out.content.chars().count());
    println!("----- content -----\n{}", out.content);
    println!(
        "\n判定：tool_calls>0 = 工具协议主路生效；=0 但 content 是动作 JSON = 兼容层；\
         content 里出现自带标记 = 这份 prompt 没压住，回去看 A/B"
    );

    // ---- 场景 2：复核员（**只声明 read**）----
    // 这一路原来是 DSML 泄露的第二个现场（两次真跑各漏一次：content = `{"path": …}` + 三行
    // 自带标记 → 复核解析失败 → 结论 unknown）。这里复验：声明子集之后它走不走工具协议。
    let msgs2 = vec![
        ChatMessage::system(harness_engine::reflect::REFLECT_SYSTEM),
        ChatMessage::user(
            "产物评审：本次改了 cloud-shop-admin-web/vite.config.js（代理目标 8080 → 8083）。\
             请判断是否破坏调用方，必要时读相关文件，然后只输出一个 JSON 结论。",
        ),
    ];
    let out2 = rt
        .block_on(chat_with_tools(&cfg, None, &msgs2, true, &["read"]))
        .expect("调用失败");
    println!("\n== 复核员场景（只声明 read）==");
    println!("finish_reason: {:?}", out2.finish_reason);
    println!("tool_calls   : {}", out2.tool_calls.len());
    for c in &out2.tool_calls {
        println!("  · {}({})", c.function.name, c.function.arguments);
    }
    println!("content      : {} 字符", out2.content.chars().count());
    println!(
        "content 片段   : {}",
        out2.content.chars().take(160).collect::<String>()
    );
    println!(
        "判定：tool_calls>0 且 content 里没有自带标记 = 复核那一路也不再漏；\
         若 content 是 path+参数 + 标记 → 这条路还没修好"
    );
}
