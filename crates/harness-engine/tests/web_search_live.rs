//! 联网搜索的**真 API 自测** —— 默认不跑（要真 key、要联网）。
//!
//! ```bash
//! DEEPSEEK_API_KEY=sk-xxx cargo test -p harness-engine --test web_search_live -- --ignored --nocapture
//! ```
//!
//! 为什么留着它：服务端联网是**黑盒能力**，单测只能用抓回来的样本验解析，验不到
//! "服务端这一刻还认不认这个参数"。DeepSeek 下过一次线（文档一度写成 `Ignored`），
//! 这类外部依赖只有真打一发才知道还在不在。改 `llm.rs` 的协议分支后跑一次就知道。
//!
//! 语料刻意选**模型训练数据里没有**的事实（某个已发生交易日的收盘点位）：
//! 拿模型答得出来的问题做样本，开关开不开都是"对"，测不出东西。

use harness_engine::config::LlmConfig;
use harness_engine::llm::{ChatMessage, chat, web_search_on};

fn key() -> String {
    std::env::var("DEEPSEEK_API_KEY")
        .or_else(|_| std::env::var("DEEPSEEK_APIKEY"))
        .expect("设 DEEPSEEK_API_KEY 再跑这条自测")
}

fn mk_cfg(web: &str) -> LlmConfig {
    LlmConfig {
        api_key: key(),
        web_search: web.to_string(),
        ..LlmConfig::default()
    }
}

fn the_question() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("只输出 JSON，形如 {\"answer\":\"...\"}，不要多余文字。"),
        ChatMessage::user("2026年9月18日 上证指数收盘点位是多少？"),
    ]
}

#[test]
#[ignore = "要真 key 与联网：DEEPSEEK_API_KEY=sk-xxx cargo test -p harness-engine --test web_search_live -- --ignored --nocapture"]
fn live_web_search_really_searches_and_the_queries_come_back() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let cfg = mk_cfg("auto");
        assert!(
            web_search_on(&cfg),
            "默认端点 api.deepseek.com 应判为 DeepSeek"
        );

        let out = chat(&cfg, None, &the_question(), true, false)
            .await
            .unwrap();
        println!("联网查询词 = {:?}", out.web_queries);
        println!("答复       = {}", out.content);
        println!(
            "token      = {}+{}",
            out.usage.prompt_tokens, out.usage.completion_tokens
        );

        // ① 真的检索过 —— 服务端把结果直接灌进上下文，引擎只拿得到查询词
        assert!(!out.web_queries.is_empty(), "服务端没发起检索");
        // ② 查询词里不许混进服务端的内部标记
        assert!(
            out.web_queries
                .iter()
                .all(|q| !q.starts_with("ws_call_id=")),
            "{:?}",
            out.web_queries
        );
        // ③ 答的是检索到的真值，且仍是合法 JSON（json_object 在 /responses 上同样生效）
        assert!(out.content.contains("3911"), "{}", out.content);
        serde_json::from_str::<serde_json::Value>(&out.content).expect("答复应是 JSON");

        // 对照组：关掉联网，同一个问题答不出来 —— 差别不显著就说明开关没真起作用
        let off = mk_cfg("off");
        assert!(!web_search_on(&off));
        let out2 = chat(&off, None, &the_question(), true, false)
            .await
            .unwrap();
        println!("关联网答复 = {}", out2.content);
        assert!(out2.web_queries.is_empty());
        assert!(!out2.content.contains("3911"), "关了联网不该也有真值");
    });
}

/// **anthropic 路的同一件事**（2026-09-25 新增）。
///
/// 为什么必须有这一臂：引擎此前对 anthropic **一律关掉**联网（`web_search_on` 里直接
/// 早返回 false），连带把用户设的 `on` 也一起吃掉 —— 配置说开着、请求里没有、界面不报错。
/// 实测是错的：DeepSeek 的 `/anthropic` 认 anthropic **原生**的服务端检索工具
/// （`web_search_20250305`，发 `{type:web_search}` 会 422）。这条真机判据就是那个缺陷的门禁：
/// 它在本修复之前**必然红**（查询词恒为空）。
#[test]
#[ignore = "要真 key 与联网：DEEPSEEK_API_KEY=sk-xxx cargo test -p harness-engine --test web_search_live -- --ignored --nocapture"]
fn live_anthropic_web_search_really_searches() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let cfg = mk_anthropic_cfg("auto");
        assert!(
            web_search_on(&cfg),
            "DeepSeek 的 anthropic 端点 + flash 实测能搜，不该判为不支持"
        );

        let out = chat(&cfg, None, &the_question(), true, false)
            .await
            .unwrap();
        println!("[anthropic] 联网查询词 = {:?}", out.web_queries);
        println!("[anthropic] 答复       = {}", out.content);
        assert!(!out.web_queries.is_empty(), "服务端没发起检索");
        assert!(
            out.content.contains("3911"),
            "没答出检索到的真值：{}",
            out.content
        );

        // 对照组：关掉联网，同一个问题答不出来（差别不显著就说明开关没真起作用）
        let off = mk_anthropic_cfg("off");
        assert!(!web_search_on(&off));
        let out2 = chat(&off, None, &the_question(), true, false)
            .await
            .unwrap();
        println!("[anthropic] 关联网答复 = {}", out2.content);
        assert!(out2.web_queries.is_empty());
    });
}

/// **同框**：严格模式（声明函数工具）与 anthropic 服务端检索工具一起发 —— 这是真实运行形状
/// （`llm.tool_protocol = true`，默认开）。实测一轮里先 `server_tool_use` + `web_search_tool_result`，
/// 紧跟着 `tool_use(final)`：检索与交付在同一轮完成，工具协议照旧推进。
#[test]
#[ignore = "要真 key 与联网：DEEPSEEK_API_KEY=sk-xxx cargo test -p harness-engine --test web_search_live -- --ignored --nocapture"]
fn live_anthropic_web_search_coexists_with_the_function_tools() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let cfg = mk_anthropic_cfg("auto");
        // tools = true：与主循环一样声明全套函数工具（`final` 等）
        let out = chat(&cfg, None, &the_question(), false, true)
            .await
            .unwrap();
        println!("[anthropic+tools] 联网查询词 = {:?}", out.web_queries);
        println!("[anthropic+tools] 答复       = {}", out.content);
        for c in &out.tool_calls {
            println!(
                "[anthropic+tools] 工具调用   = {} {}",
                c.function.name, c.function.arguments
            );
        }

        // 判据是"**两件事同框、各不挤掉对方**"（不是"模型必须答出某个数"——
        // 声明了函数工具之后它先调哪个工具是它的自由，单发一次调用没有循环来接着走）：
        // ① 真检索了 —— 服务端工具没被函数工具挤掉；
        assert!(!out.web_queries.is_empty(), "开了联网就必须真检索");
        // ② 函数工具照旧被解析出来 —— 服务端工具块没被误认成我们的调用、也没让我们漏解析；
        assert!(
            !out.tool_calls.is_empty(),
            "声明了函数工具就必须有可解析的调用（同框把工具协议挤掉了）；content={}",
            out.content
        );
        // ③ 声明的工具名与参数都是我们认得的形状（服务端工具块不该混进来）
        for c in &out.tool_calls {
            assert!(
                [
                    "read", "write", "execute", "connect", "plan", "ask_user", "final"
                ]
                .contains(&c.function.name.as_str()),
                "解析出一个不存在的工具名：{}（服务端工具块混进来了？）",
                c.function.name
            );
            assert!(
                serde_json::from_str::<serde_json::Value>(&c.function.arguments).is_ok(),
                "参数不是合法 JSON：{}",
                c.function.arguments
            );
        }
    });
}

/// anthropic 端的配置（真 key + DeepSeek 的 anthropic 入口）。
fn mk_anthropic_cfg(web: &str) -> LlmConfig {
    LlmConfig {
        api_key: key(),
        api_format: "anthropic".to_string(),
        base_url: "https://api.deepseek.com/anthropic".to_string(),
        model: "deepseek-flash".to_string(),
        web_search: web.to_string(),
        ..LlmConfig::default()
    }
}
