//! 命令发现的实测工具：对给定项目根跑一遍探测，打印结论、耗时与将要注入模型的提示词。
//!
//! 用法：`cargo run -q -p harness-engine --example cmd_discover -- <项目根>`
//!
//! 存在的意义是**可核对**：探测结论必须能拿这条命令复现，而不是"应该没问题"。
//! 尤其要看两件事 —— ① 冷探测耗时（会不会拖慢一轮对话）② 缓存是否真的命中。

use harness_engine::config::DiscoverConfig;
use harness_engine::discover;
use std::path::PathBuf;

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let cfg = DiscoverConfig::default();

    let t0 = std::time::Instant::now();
    let tools = discover::discover(&root, &cfg, "python", "node");
    let cold = t0.elapsed();

    println!("项目根：{}", root.display());
    println!("冷探测：{cold:?}（{} 项）", tools.len());
    for t in &tools {
        if t.available {
            println!("  ✓ {:<8} {:<42} {}", t.name, t.version, t.path);
        } else {
            println!("  ✗ {:<8} （本机没有）", t.name);
        }
    }

    let t1 = std::time::Instant::now();
    let again = discover::discover(&root, &cfg, "python", "node");
    println!("缓存后：{:?}（{} 项）", t1.elapsed(), again.len());

    println!("\n--- 注入模型的提示词（无环境准备连接）---");
    println!(
        "{}",
        discover::render_note(&tools, false).unwrap_or_else(|| "（空）".into())
    );
    println!("\n--- 注入模型的提示词（宿主接了环境准备连接）---");
    println!(
        "{}",
        discover::render_note(&tools, true).unwrap_or_else(|| "（空）".into())
    );
}
