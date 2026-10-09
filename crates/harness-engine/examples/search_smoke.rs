//! 分层检索（v1.5）的**真模型**冒烟 —— 验的不是机制（那有 14 条单测），是"模型会不会用它"。
//!
//! 用法（要真 Key；`--config` 指向宿主那份分节 `ai.toml`）：
//!
//! ```ignore
//! cargo run -q -p harness-engine --example search_smoke -- --config target/debug/global/ai.toml
//! ```
//!
//! ## 判据（退出码）
//!
//! - **0**：模型用了检索形状、找对了文件、而且**没**把跳过的生成物目录当结果；
//! - **2**：任务做对了但**形状没被接住**（模型走 read 树 + 逐个读文件那条老路）——
//!   那说明声明面/提示词还需再推一把，是**要看的读数**，不是"通过了"；
//! - **1**：答案错了（漏文件 / 报了生成物）；
//! - **3**：环境不齐（没有 ai.toml 或没有 key）—— 大声跳过，**不假装通过**。
//!
//! 为什么值得有它：声明面（`TOOL_DECLS`）与提示词改动的**唯一真判据**是"真模型照着做"，
//! 单测只能证明"解析器认这个形状"，证明不了"模型会发它"。

use harness_engine::agent::{self, HistoryMsg, WritePolicy};
use harness_engine::config::AppConfig;
use harness_engine::pipeline::Sink;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 收日志的 sink：跑真循环时把事件攒下来（判据就看这些行）
#[derive(Default)]
struct Rec {
    lines: Mutex<Vec<String>>,
}

impl Sink for Rec {
    fn log(&self, level: &str, msg: String) {
        if let Ok(mut g) = self.lines.lock() {
            g.push(format!("[{level}] {msg}"));
        }
    }
}

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1).cloned())
}

/// 宿主的分节 `ai.toml`（`[ai]` 下扁平键）→ 引擎的嵌套 `AppConfig`。
/// 键名也不同：宿主的 `api_url` 在引擎里叫 `base_url`。
fn apply_ai_toml(cfg: &mut AppConfig, path: &Path) -> Result<(), String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
    let v: toml::Value =
        toml::from_str(&text).map_err(|e| format!("{} 解析失败：{e}", path.display()))?;
    let sec = v.get("ai").unwrap_or(&v);
    let get = |k: &str| sec.get(k).and_then(|x| x.as_str()).map(|s| s.to_string());
    if let Some(k) = get("api_key") {
        cfg.llm.api_key = k;
    }
    if let Some(u) = get("api_url").or_else(|| get("base_url")) {
        cfg.llm.base_url = u;
    }
    if let Some(m) = get("model") {
        cfg.llm.model = m;
    }
    if let Some(f) = get("api_format") {
        cfg.llm.api_format = f;
    }
    Ok(())
}

/// 造一个最小的项目：**两份真命中**（一份在子目录里）+ **一份在生成物目录里**（默认不搜）。
fn build_project(root: &Path) -> std::io::Result<()> {
    let w = |rel: &str, body: &str| -> std::io::Result<()> {
        let p = root.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, body)
    };
    w(
        "src/core.rs",
        "// 重试策略\npub const RETRY_POLICY: u32 = 3;\n",
    )?;
    w("docs/notes.md", "# 说明\n- RETRY_POLICY 由部署配置覆盖\n")?;
    w(
        "src/util.rs",
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )?;
    // 生成物目录：被默认跳过 ⇒ **不该**出现在答案里
    w("target/gen.rs", "// RETRY_POLICY 的编译产物副本\n")?;
    Ok(())
}

fn main() {
    let cfg_path = arg("--config").unwrap_or_else(|| "target/debug/global/ai.toml".into());
    let cfg_path = PathBuf::from(&cfg_path);
    let mut cfg = AppConfig::default();
    if apply_ai_toml(&mut cfg, &cfg_path).is_err() || cfg.llm.api_key.trim().is_empty() {
        eprintln!(
            "[skip] 没有可用的 LLM 配置（{}）—— 这条冒烟不跑，也不假装通过",
            cfg_path.display()
        );
        std::process::exit(3);
    }
    cfg.agent.ctx.dedup = true;
    cfg.agent.ctx.capsule = true;

    let dir = std::env::temp_dir().join(format!("ruyix-search-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时项目");
    build_project(&dir).expect("写夹具");

    // 刻意**不**点名工具：要的就是"模型自己会不会想到检索"。
    // 只是把任务说得像人话（要在多份文件里找东西）。
    let task = "项目里哪些文件提到 RETRY_POLICY？给出文件与行号。用最省事的方式找 —— \
                别把生成物目录里的东西算进来。最后只用 final 列结果。";
    let sink = Rec::default();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio");
    let out = rt.block_on(agent::run(
        &cfg,
        &dir,
        task,
        &[HistoryMsg {
            role: "user".into(),
            text: task.to_string(),
            run_id: None,
        }],
        WritePolicy::Apply,
        &agent::NoConnector,
        &harness_engine::exec::new_cancel_flag(),
        &sink,
    ));

    let logs = sink.lines.lock().map(|g| g.join("\n")).unwrap_or_default();
    println!("=== 模型：{} · 端点 {}", cfg.llm.model, cfg.llm.base_url);
    println!("=== 工具循环（引擎日志）\n{logs}");

    let out = match out {
        Ok(o) => o,
        Err(e) => {
            println!("run 失败：{e}");
            std::process::exit(1);
        }
    };
    println!(
        "=== 交付答案（前 600 字）\n{}",
        out.answer.chars().take(600).collect::<String>()
    );
    println!(
        "=== 用量：{} token（缓存命中 {}）· {} 轮",
        out.usage.total_tokens,
        out.usage
            .cache_read()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "不回报".into()),
        logs.lines().filter(|l| l.contains(" 轮 ")).count()
    );

    let used_search = logs.contains("read search ");
    let mentions_core = out.answer.contains("core.rs");
    let mentions_docs = out.answer.contains("notes.md");
    // "误报"与"解释为什么不算它"要分开看（第一版仪器把后者当成了前者 —— 真跑抓到的：
    // 模型的答案里那句是「target/gen.rs 确实写着这个词，但它在被跳过的目录里」，那是**对的**）。
    // 判据：提到它的那一行必须同时带一个"排除"的说法；否则才叫把生成物当成了结果。
    let excl = [
        "跳过",
        "skipped",
        "生成物",
        "排除",
        "不算",
        "不列",
        "忽略",
        "not counted",
    ];
    let leaked_target = out
        .answer
        .lines()
        .filter(|l| l.contains("target/gen.rs"))
        .any(|l| !excl.iter().any(|k| l.contains(k)));

    println!(
        "\n=== 判读：形状被接住 = {used_search} · 提到 src/core.rs = {mentions_core} · \
         提到 docs/notes.md = {mentions_docs} · 误报生成物 = {leaked_target}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    if leaked_target || !mentions_core || !mentions_docs {
        eprintln!("✗ 答案不对（漏文件或把生成物算进来了）");
        std::process::exit(1);
    }
    if !used_search {
        eprintln!(
            "⚠ 任务做对了，但模型**没用检索形状**（走的是 read 树 + 逐个读文件）—— \
             声明面/提示词没被接住，这是要看的读数，不算通过"
        );
        std::process::exit(2);
    }
    println!("✓ 通过：模型用检索形状找到了两份真命中，且没把跳过的生成物算进来");
}
