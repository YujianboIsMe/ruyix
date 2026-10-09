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

/// 只存在于**知识库**里的事实（项目里一个字都没有）—— `--kb` 那一臂的靶子。
///
/// 为什么这么设计：`files` 层那条路能答对，说明不了 `kb` 层被接住 —— 事实必须**只有**那一层有。
const KB_FACT: &str = "日志保留 45 天";

fn build_corpus(root: &Path) -> std::io::Result<()> {
    let p = root.join("规范/部署规范.md");
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(
        p,
        "# 部署规范\n\n- 日志保留 45 天，过期归档到冷存储。\n- 缓存上限 8G，超过就换出。\n",
    )
}

/// `--kb` 那一臂：**事实只在知识库里**，看模型会不会主动问 `kb` 层。
///
/// 判据（退出码）与 files 臂同款：`0` 形状被接住且答案对 · `2` 答对但走的是别的路（要走 `files`
/// 是徒劳 —— 事实不在项目里）· `1` 答案错 · `3` 环境不齐。
fn run_kb_arm(cfg: &AppConfig, model_override: Option<String>) -> i32 {
    let mut cfg = cfg.clone();
    if let Some(m) = model_override {
        cfg.llm.model = m;
    }
    // 知识库：语料目录（用户资料）+ 索引库（`<便携根>/global/kb` 的替身）
    let base = std::env::temp_dir().join(format!("ruyix-kb-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (proj, corpus, store) = (base.join("proj"), base.join("corpus"), base.join("kb"));
    for d in [&proj, &corpus, &store] {
        std::fs::create_dir_all(d).expect("建目录");
    }
    build_project(&proj).expect("写项目夹具");
    build_corpus(&corpus).expect("写语料夹具");

    cfg.kb.enabled = true;
    cfg.kb.dir = store.to_string_lossy().to_string();
    cfg.kb.roots = vec![corpus.to_string_lossy().to_string()];
    cfg.agent.ctx.dedup = true;
    // 索引：**真实形态**是宿主在启动 / 配置改完后建（`src-tauri/src/agent/kb.rs`），
    // 这里替它跑一遍 —— 索引没建好时 `kb` 层会如实报"没有读数"，那也不算通过。
    let entry = harness_engine::kb::ephemeral_entry(&corpus);
    if let Err(e) = harness_engine::kb::index::index_source(
        &store,
        &entry,
        &cfg.kb,
        &harness_engine::exec::new_cancel_flag(),
        &mut |_p| {},
    ) {
        eprintln!("[skip] 索引建不起来（{e}）—— 这条冒烟不跑，也不假装通过");
        return 3;
    }

    let task = "我们团队的**部署规范**里说日志要保留多少天？项目代码里没有这条，去我的知识库里查。\
                最后只用 final 回答（一句话 + 说清依据来自哪儿）。";
    let sink = Rec::default();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio");
    let out = rt.block_on(agent::run(
        &cfg,
        &proj,
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
            let _ = std::fs::remove_dir_all(&base);
            return 1;
        }
    };
    println!(
        "=== 交付答案（前 600 字）\n{}",
        out.answer.chars().take(600).collect::<String>()
    );

    let used_kb = logs.contains("read search kb");
    let answered = out.answer.contains("45");
    println!("\n=== 判读：用了 kb 层 = {used_kb} · 答出「{KB_FACT}」= {answered}");
    let _ = std::fs::remove_dir_all(&base);
    if !answered {
        eprintln!("✗ 答案不对（没答出知识库里那条事实）");
        return 1;
    }
    if !used_kb {
        eprintln!(
            "⚠ 答案对了，但模型**没用 kb 层**（走的是别的路）—— 声明面/提示词没被接住，\
             这是要看的读数，不算通过"
        );
        return 2;
    }
    println!("✓ 通过：模型主动问了知识库，答出了只存在于那儿的事实");
    0
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
    let model = arg("--model");
    if let Some(m) = &model {
        cfg.llm.model = m.clone(); // 真 run 冒烟一律用 flash（pro 太贵，见仓库惯例）
    }
    let kb_arm = std::env::args().any(|a| a == "--kb");
    if kb_arm {
        std::process::exit(run_kb_arm(&cfg, None));
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
