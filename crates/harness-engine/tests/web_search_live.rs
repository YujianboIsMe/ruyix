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

        let out = chat(&cfg, &the_question(), true).await.unwrap();
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
        let out2 = chat(&off, &the_question(), true).await.unwrap();
        println!("关联网答复 = {}", out2.content);
        assert!(out2.web_queries.is_empty());
        assert!(!out2.content.contains("3911"), "关了联网不该也有真值");
    });
}
