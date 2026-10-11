//! plan-only 端到端冒烟（融合计划 P4 验收工具，也是 P5「引擎 CLI」的雏形）。
//!
//! 真调 LLM 跑完整规划阶段：Key 走环境变量 `DEEPSEEK_API_KEY`（引擎纪律：
//! 环境变量优先、不落盘）；run 产物落在引擎默认运行目录（`%TEMP%/ruyix/runs` ——
//! 引擎不再摸用户主目录，要固定位置就设 `harness.workspace_root`）。刻意不读
//! `%APPDATA%` 下的实验室遗留配置 —— 冒烟要的是确定性。
//!
//! 用法：
//! ```text
//! DEEPSEEK_API_KEY=sk-... cargo run -p harness-engine --example plan_only -- "任务描述"
//! ```

use harness_engine::{config, pipeline};

/// 最小 Sink：事件直接打 stdout（GUI 场景由 ruyix 侧 AgentSink 接管）。
struct PrintSink;

impl pipeline::Sink for PrintSink {
    fn log(&self, level: &str, msg: String) {
        println!("[{level}] {msg}");
    }

    fn stage(&self, name: &str, status: &str, detail: String) {
        if detail.is_empty() {
            println!("== {name} · {status}");
        } else {
            println!("== {name} · {status} — {detail}");
        }
    }
}

fn main() {
    let task = std::env::args().nth(1).unwrap_or_else(|| {
        "实现一个 word_count 函数：统计英文文本中每个单词的出现次数，并补齐单元测试".into()
    });

    // 默认值（DeepSeek 端点 + 运行目录落临时目录）+ 纯环境变量覆盖，不读任何配置文件
    let mut cfg = config::AppConfig::default();
    config::apply_env_overrides(&mut cfg);

    if cfg.llm.api_key.trim().is_empty() {
        eprintln!("未配置 LLM Key：请设置环境变量 DEEPSEEK_API_KEY（不落盘）");
        std::process::exit(2);
    }
    println!(
        "model={} · base_url={} · runs_root={}",
        cfg.llm.model,
        cfg.llm.base_url,
        config::runs_root(&cfg).display()
    );

    // 引擎只依赖 tokio 的基础 rt（单线程即可，LLM 调用是纯 IO）
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    match rt.block_on(pipeline::plan_stage(&PrintSink, &cfg, &task, None)) {
        Ok(rec) => {
            let plan = rec.plan.as_ref().expect("plan_stage 必产出计划");
            println!("\n=== plan-only 验收摘要 ===");
            println!("run_id   : {}", rec.run_id);
            println!("status   : {}", rec.status);
            println!("language : {}", plan.language);
            println!("steps    : {}", plan.steps.len());
            for (i, s) in plan.steps.iter().enumerate() {
                println!("  {}. {}", i + 1, s.title);
            }
            println!(
                "usage    : {} / {} tokens",
                rec.usage.prompt_tokens, rec.usage.completion_tokens
            );
            println!("dir      : {}", rec.dir);
        }
        Err(e) => {
            eprintln!("plan-only 失败：{e}");
            std::process::exit(1);
        }
    }
}
