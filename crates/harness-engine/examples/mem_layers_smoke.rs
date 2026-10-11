//! 侯服（项目记忆）的**真模型**冒烟 —— 验的不是机制（那有单测），是"跑真 run 时它真的记得住"。
//!
//! 用法（要真 Key；`--config` 指向宿主那份分节 `ai.toml`）：
//!
//! ```ignore
//! cargo run -q -p harness-engine --example mem_layers_smoke -- \
//!   --config target/debug/global/ai.toml --model deepseek-v4-flash
//! ```
//!
//! ## 为什么值得有它
//!
//! 2026-10-09 实测过一件难看的事：项目记忆（侯服）在**真实便携根的库里 0 条** ——
//! 单测全绿、接线也在，但**跑起来一条都没进去**。所以这一条必须真跑，而且判据不能是
//! "库里有一行"（那可能是我自己写进去的），得是**下一个 run 只能靠它答得出来**：
//!
//! 1. **第 1 个 run**：项目里两处说法**冲突**（schema 是 varchar(72)，旧文档写 varchar(60)）——
//!    让它以磁盘为准核对、并把确认下来的事实记下来（**不点名工具**，看它自己记不记）；
//! 2. 把两份来源文件**删掉** —— 于是那个事实只存在于项目记忆里；
//! 3. **第 2 个 run**：再问同一个字段有多长。答对了，就只可能是侯服给的。
//!
//! ## 判据（退出码）
//!
//! - **0**：第 1 个 run 记下了结论、第 2 个 run **搜了 project_mem** 并答对；
//! - **2**：任务做对了但**形状没被接住**（没记 findings / 没搜 project_mem）—— 要看的读数，不算通过；
//! - **1**：答案错（第 2 个 run 答不出 varchar(72)）；
//! - **3**：环境不齐（没有 ai.toml 或没有 key）—— 大声跳过，不假装通过。

use harness_engine::agent::{self, HistoryMsg, WritePolicy};
use harness_engine::config::AppConfig;
use harness_engine::mem::{self, Memory};
use harness_engine::pipeline::Sink;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

/// 宿主的分节 `ai.toml`（`[ai]` 下扁平键）→ 引擎的嵌套 `AppConfig`（键名也不同）。
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

/// 夹具：**两处说法冲突**（磁盘 vs 旧文档）—— 真事实在 schema 里。
fn build_project(root: &Path) -> std::io::Result<()> {
    let w = |rel: &str, body: &str| -> std::io::Result<()> {
        let p = root.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, body)
    };
    w(
        "db/schema.sql",
        "-- user 表（**当前**建表语句）\nCREATE TABLE user (\n  id BIGINT PRIMARY KEY,\n  \
         password VARCHAR(72) NOT NULL\n);\n",
    )?;
    w(
        "docs/api.md",
        "# 接口文档（旧）\n- user.password 字段：varchar(60)（这行还没跟着改）\n",
    )?;
    Ok(())
}

fn run_once(
    cfg: &AppConfig,
    dir: &Path,
    task: &str,
    sink: &Rec,
) -> Result<agent::AgentOutcome, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio: {e}"))?;
    rt.block_on(agent::run(
        cfg,
        dir,
        task,
        &[HistoryMsg {
            role: "user".into(),
            text: task.to_string(),
            run_id: None,
        }],
        WritePolicy::Apply,
        &agent::NoConnector,
        &harness_engine::exec::new_cancel_flag(),
        sink,
    ))
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
    if let Some(m) = arg("--model") {
        cfg.llm.model = m;
    }
    // 与真宿主同款：留痕 + 去重 + 侧存都开（侯服要的是"结论进记忆"，与它们无关，但别少开）
    cfg.agent.ctx.dedup = true;
    cfg.agent.ctx.capsule = true;

    let dir = std::env::temp_dir().join(format!("ruyix-mem-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时项目");
    build_project(&dir).expect("写夹具");

    // 记忆库：**临时库**（不碰用户真实那份），作用域 = 这个临时项目的 key。
    let db = dir.join("mem.db");
    let m = Memory::open(&db, vec![]).expect("开临时记忆库");
    let scope = format!("smoke-{}", std::process::id());
    mem::set_scope(&scope);
    if let Err(e) = mem::install(m) {
        eprintln!("[skip] 装不上记忆库：{e}");
        std::process::exit(3);
    }
    let m = mem::current().expect("装上了就该拿得到");

    let rt_sink = Rec::default();
    println!("=== 模型：{} · 端点 {}", cfg.llm.model, cfg.llm.base_url);
    println!("=== 记忆库：{}（scope = {scope}）\n", db.display());

    // ---------- 第 1 个 run：核对事实 + 记下来 ----------
    let task1 = "本项目 user 表的 password 字段到底是多长？磁盘上的东西比文档硬，自己核对；\
                 确认下来的事实按你的规矩记下来（免得下次还得再查一遍）。最后只用 final 说结论。";
    let out1 = match run_once(&cfg, &dir, task1, &rt_sink) {
        Ok(o) => o,
        Err(e) => {
            println!("第 1 个 run 失败：{e}");
            std::process::exit(1);
        }
    };
    let logs1 = rt_sink
        .lines
        .lock()
        .map(|g| g.join("\n"))
        .unwrap_or_default();
    println!(
        "--- 第 1 个 run：{} 轮 · findings {} 条 · 用量 {} token",
        logs1.lines().filter(|l| l.contains(" 轮 ")).count(),
        out1.findings.len(),
        out1.usage.total_tokens
    );
    println!(
        "--- 第 1 个 run 的答案（前 300 字）\n{}",
        out1.answer.chars().take(300).collect::<String>()
    );

    // **判据 A**：结论真的进了项目记忆（按内容搜得到）
    let in_mem = mem::retrieve::now(m, &scope, "password", 10, None).unwrap_or_default();
    let mem_text = in_mem
        .iter()
        .map(|h| format!("{:?}", h))
        .collect::<Vec<_>>()
        .join("\n");
    println!(
        "\n--- 项目记忆里现在有什么（scope={scope}）：{} 条\n{}",
        in_mem.len(),
        mem_text.chars().take(500).collect::<String>()
    );

    // ---------- 把来源删掉：那个事实此后只存在于记忆里 ----------
    let _ = std::fs::remove_file(dir.join("db/schema.sql"));
    let _ = std::fs::remove_file(dir.join("docs/api.md"));
    println!("\n--- 已删掉 db/schema.sql 与 docs/api.md（事实只剩记忆里那一份）");

    // ---------- 第 2 个 run：只能靠记忆答 ----------
    let sink2 = Rec::default();
    let task2 = "user 表的 password 字段有多长？定义它的那份文件我刚删了 —— \
                 用你**记得**的东西回答，并说清依据来自哪里。";
    let out2 = match run_once(&cfg, &dir, task2, &sink2) {
        Ok(o) => o,
        Err(e) => {
            println!("第 2 个 run 失败：{e}");
            std::process::exit(1);
        }
    };
    let logs2 = sink2.lines.lock().map(|g| g.join("\n")).unwrap_or_default();
    println!(
        "\n--- 第 2 个 run：{} 轮 · 用量 {} token",
        logs2.lines().filter(|l| l.contains(" 轮 ")).count(),
        out2.usage.total_tokens
    );
    println!(
        "--- 第 2 个 run 的答案（前 400 字）\n{}",
        out2.answer.chars().take(400).collect::<String>()
    );

    let used_mem = logs2.contains("project_mem");
    let got_fact = out2.answer.contains("72");
    let global_leak = mem::retrieve::global_now(m, "password", 5)
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    println!(
        "\n=== 判读：第 1 个 run 记下结论 = {} · 项目记忆搜得到 = {} · 第 2 个 run 搜了 project_mem = {} \
         · 答出 72 = {} · 全局层串味 = {}",
        !out1.findings.is_empty(),
        !in_mem.is_empty(),
        used_mem,
        got_fact,
        global_leak
    );
    let _ = std::fs::remove_dir_all(&dir);

    if !got_fact {
        eprintln!("✗ 第 2 个 run 没答对（事实没从记忆里回来）");
        std::process::exit(1);
    }
    if out1.findings.is_empty() || in_mem.is_empty() || !used_mem {
        eprintln!(
            "⚠ 答案对了，但形状没走通（第 1 个 run 没记结论 / 项目记忆里没有 / 第 2 个 run 没搜 project_mem）\
             —— 这是要看的读数，不算通过"
        );
        std::process::exit(2);
    }
    if global_leak {
        eprintln!("✗ 项目事实漏进了全局层（层与层必须不串）");
        std::process::exit(1);
    }
    println!("✓ 通过：结论进了项目记忆，下一个 run 只靠它就答对了");
}
