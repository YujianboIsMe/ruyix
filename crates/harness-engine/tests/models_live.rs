//! 厂商模型列表的**真 API 自测** —— 默认不跑（要真 key、要联网）。
//!
//! ```bash
//! DEEPSEEK_API_KEY=sk-xxx cargo test -p harness-engine --test models_live -- --ignored --nocapture
//! ```
//!
//! 为什么单测不够：**端点长什么样是外部事实**。实测（2026-09-25）DeepSeek 的
//! `https://api.deepseek.com/anthropic/v1/models` 是 **404**，而 `https://api.deepseek.com/models`
//! 是 200 且带完整能力声明 —— 只试协议那条 URL 的话，anthropic 用户的模型下拉框会**静默
//! 退回文本框**（配置表单那条路就是这样）。回落这件事只有真打一发才知道还在不在。

use harness_engine::config::LlmConfig;
use harness_engine::llm::{models, probe};

fn key() -> String {
    std::env::var("DEEPSEEK_API_KEY")
        .or_else(|_| std::env::var("DEEPSEEK_APIKEY"))
        .expect("设 DEEPSEEK_API_KEY 再跑这条自测")
}

fn anthropic_cfg() -> LlmConfig {
    LlmConfig {
        api_key: key(),
        api_format: "anthropic".to_string(),
        base_url: "https://api.deepseek.com/anthropic".to_string(),
        model: "deepseek-flash".to_string(),
        ..LlmConfig::default()
    }
}

#[test]
#[ignore = "要真 key 与联网：DEEPSEEK_API_KEY=sk-xxx cargo test -p harness-engine --test models_live -- --ignored --nocapture"]
fn live_models_fall_back_from_the_anthropic_path_to_the_host_root() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let cfg = anthropic_cfg();
        let list = models(&cfg)
            .await
            .expect("anthropic 入口的 /v1/models 是 404 —— 这条断言的另一头就是主机根回落");
        println!("模型数 = {}", list.len());
        for m in &list {
            println!(
                "  {:<20} name={:<24} in={:?} ctx={:?}",
                m.id,
                m.display_name(),
                m.input_modalities,
                m.context_window
            );
        }
        // ① 回落真的生效（否则这里会是 Err）
        assert!(!list.is_empty(), "厂商列表是空的");
        // ② 每一条都要带厂商声明的输入模态 —— 录音/读图判断的一手证据
        assert!(
            list.iter().any(|m| m.accepts("image")),
            "没有任何模型声明 image：厂商响应结构变了？"
        );
        // ③ 今天 DeepSeek 谁都不收音频（这个断言会随厂商升级而红 —— 那时正是该改代码的信号）
        assert!(
            !list.iter().any(|m| m.accepts("audio")),
            "有模型声明 audio 了！录音那条链该从「只落盘」改成「直发」了：{:?}",
            list.iter()
                .filter(|m| m.accepts("audio"))
                .map(|m| m.id.clone())
                .collect::<Vec<_>>()
        );
        // ④ probe() 与 models() 同源（配置表单与会话下拉框不许各说各话）
        let ids = probe(&cfg).await.unwrap();
        assert_eq!(ids, list.iter().map(|m| m.id.clone()).collect::<Vec<_>>());
    });
}
