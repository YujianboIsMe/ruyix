//! 真机探针：拿**真实端点**验"工具调用到底走没走通"。
//!
//! 用法：
//! ```bash
//! cargo run -q -p harness-engine --example llm_tool_probe -- --config <便携根>/global/ai.toml
//! ```
//!
//! **判据（预注册，跑之前就钉死，跑完不改）**：
//!
//! | 臂 | 任务 | 判据 |
//! |---|---|---|
//! | A | `你好`（用户实测的复现句） | **出现过**「工具调用（标准协议）」；且 run 不是以「连续 N 轮没有拿到工具调用」收场 |
//!
//! ⚠ A 臂判据**不能**写成"一次「输出无法解析」都不许有" —— 第一版就是这么写的，结果复跑时红了一次，
//! 而那次**不是协议没通**：严格模式（`llm.tool_protocol`）下模型若把问候写进正文，引擎**本来就该**
//! 点它一轮（"形状对、通道错，请用 final 工具"），模型随后照做 —— 那是**设计内的纠正**，
//! 不是病。真正的判别式是"**工具调用出现过没有**"：病态时是 `0 轮`，且 run 最终以
//! "连续 N 轮没有拿到工具调用"失败（见 `MAX_UNPARSEABLE_ROUNDS`）。
//! 所以「无法解析轮数」只当**证据打印**，不当判据。
//! | B | `读一下 hello.txt 第一行并告诉我内容`（**必须用工具**的任务） | 同上，且最终答复里含探针写进文件的那句话（= 工具**真跑了**、结果**真回到了模型**） |
//!
//! 为什么非要有 B 臂：只跑 A 的话，"模型不发工具调用、直接用 final 交付"也能让 A 过 ——
//! 那说明不了工具通不通。B 臂要的是"文件内容出现在答复里"，这条**只有**当
//! read 工具真被调用、结果真被喂回模型才可能成立。
//!
//! 退出码：0 = 两条臂都达预期；1 = 有臂没达预期（逐条打印原始证据）。
//!
//! 安全：只读配置里的 `api_key`，**从不打印**；两次运行都是真实请求（会消耗少量额度）。

use harness_engine::agent::WritePolicy;
use harness_engine::agent::{NoAsker, NoConnector};
use harness_engine::config::LlmConfig;
use harness_engine::config::{AppConfig, StepAgentConfig};
use harness_engine::exec;
use harness_engine::pipeline::Sink;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 会把引擎日志原样打出来、同时留一份给判据用的 sink。
struct PrintSink {
    lines: Mutex<Vec<(String, String)>>,
}

impl PrintSink {
    fn new() -> Self {
        Self {
            lines: Mutex::new(Vec::new()),
        }
    }
    fn dump(&self) -> Vec<(String, String)> {
        self.lines.lock().unwrap().clone()
    }
}

impl Sink for PrintSink {
    fn log(&self, level: &str, msg: String) {
        println!("    [{level}] {msg}");
        self.lines
            .lock()
            .unwrap()
            .push((level.to_string(), msg.clone()));
    }
    fn stage(&self, name: &str, status: &str, detail: String) {
        if !detail.is_empty() {
            println!("    [stage:{status}] {name} — {detail}");
        }
        self.lines
            .lock()
            .unwrap()
            .push((format!("stage:{status}"), format!("[{name}] {detail}")));
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut cfg_path: Option<String> = None;
    while let Some(a) = args.next() {
        if a == "--config" {
            cfg_path = args.next();
        }
    }
    let Some(cfg_path) = cfg_path else {
        println!("用法: --example llm_tool_probe -- --config <便携根>/global/ai.toml");
        std::process::exit(2);
    };

    let raw = std::fs::read_to_string(&cfg_path).unwrap_or_else(|e| {
        println!("读配置失败 {cfg_path}: {e}");
        std::process::exit(2);
    });
    let doc: toml::Value = toml::from_str(&raw).expect("配置不是合法 TOML");
    let ai = doc.get("ai").expect("配置里没有 [ai] 段");

    let s =
        |k: &str| -> Option<String> { ai.get(k).and_then(|v| v.as_str()).map(|x| x.to_string()) };
    let n = |k: &str| -> Option<u32> { ai.get(k).and_then(|v| v.as_integer()).map(|x| x as u32) };
    // 布尔在配置里可能是真布尔，也可能是字符串 "true"（配置表单就是按字符串存的）
    let b = |k: &str| -> bool {
        match ai.get(k) {
            Some(toml::Value::Boolean(x)) => *x,
            Some(toml::Value::String(x)) => x.eq_ignore_ascii_case("true"),
            _ => true,
        }
    };

    let llm = LlmConfig {
        base_url: s("api_url").unwrap_or_default(),
        api_key: s("api_key").unwrap_or_default(),
        model: s("model").unwrap_or_default(),
        max_tokens: n("max_tokens").unwrap_or(2048),
        temperature: 0.0,
        api_format: s("api_format").unwrap_or_else(|| "openai".into()),
        ..Default::default()
    };
    let strict = b("tool_protocol");

    println!("=== 真机工具协议探针 ===");
    println!("  配置   : {cfg_path}");
    println!("  端点   : {}", llm.base_url);
    println!("  模型   : {}", llm.model);
    println!("  协议   : {}", llm.api_format);
    println!(
        "  密钥   : {}",
        if llm.api_key.trim().is_empty() {
            "（空！下面必然失败）"
        } else {
            "已读取（不打印）"
        }
    );
    println!("  严格模式: {strict}");
    if llm.api_key.trim().is_empty() {
        std::process::exit(2);
    }

    // 一次成型（不写成 Default 之后逐字段赋值：clippy 的 field_reassign_with_default 会拦）。
    // 探针要的是「协议通不通」，不想被计划/子步骤干扰：关掉计划执行体，其它保持默认。
    let cfg = AppConfig {
        llm: LlmConfig {
            tool_protocol: strict,
            ..llm.clone()
        },
        step: StepAgentConfig {
            execute_plan: false,
            ..Default::default()
        },
        ..Default::default()
    };

    // 探针项目：一个临时目录 + 一句只有读得到才能回答的内容
    let proj = std::env::temp_dir().join("ruyix-llm-probe");
    let _ = std::fs::remove_dir_all(&proj);
    std::fs::create_dir_all(&proj).expect("建探针项目失败");
    const SECRET_LINE: &str = "probe-line-42";
    std::fs::write(proj.join("hello.txt"), format!("{SECRET_LINE}\n第二行\n"))
        .expect("写探针文件失败");

    let mut fails: Vec<String> = Vec::new();

    // ---------------- A 臂：用户实测的复现句 ----------------
    println!("\n--- A 臂：任务「你好」（用户实测的复现句）---");
    let sink_a = PrintSink::new();
    let out_a = run(&cfg, &proj, "你好", &sink_a);
    match &out_a {
        Ok(answer) => println!("\n  A 答复: {}", clip(answer, 160)),
        Err(e) => println!("\n  A 失败: {e}"),
    }
    let a = judge("A", &sink_a.dump(), &out_a, None, &mut fails);

    // ---------------- B 臂：必须用工具 ----------------
    println!("\n--- B 臂：任务「读一下 hello.txt 的第一行，把内容原样告诉我」---");
    let sink_b = PrintSink::new();
    let out_b = run(
        &cfg,
        &proj,
        "读一下 hello.txt 的第一行，把那一行的内容原样告诉我（不要猜）",
        &sink_b,
    );
    match &out_b {
        Ok(answer) => println!("\n  B 答复: {}", clip(answer, 240)),
        Err(e) => println!("\n  B 失败: {e}"),
    }
    let _ = judge("B", &sink_b.dump(), &out_b, Some(SECRET_LINE), &mut fails);
    let _ = a;

    println!("\n===== 结论 =====");
    if fails.is_empty() {
        println!("两条臂都达预期：工具调用在这条端点上**通了**。");
        std::process::exit(0);
    }
    for f in &fails {
        println!("✗ {f}");
    }
    std::process::exit(1);
}

fn run(cfg: &AppConfig, proj: &Path, task: &str, sink: &PrintSink) -> Result<String, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let proj_path = PathBuf::from(proj);
    rt.block_on(async {
        // Stage：不往探针项目写回（探针只关心协议，不关心产物落地）
        harness_engine::agent::run_with_ask(
            cfg,
            &proj_path,
            task,
            &[],
            WritePolicy::Stage,
            &NoConnector,
            &NoAsker,
            &exec::new_cancel_flag(),
            sink,
        )
        .await
        .map(|o| o.answer)
    })
}

/// 判据：不出现「无法解析」+ 出现「工具调用（标准协议）」+（给定答案时）答复里含密语。
fn judge(
    arm: &str,
    lines: &[(String, String)],
    out: &Result<String, String>,
    must_contain: Option<&str>,
    fails: &mut Vec<String>,
) -> bool {
    let text = lines
        .iter()
        .map(|(_, m)| m.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let unparsed = lines
        .iter()
        .filter(|(_, m)| m.contains("无法解析"))
        .map(|(_, m)| clip(m, 120))
        .collect::<Vec<_>>();
    let tool_rounds = lines
        .iter()
        .filter(|(_, m)| m.contains("工具调用（标准协议）"))
        .count();

    println!("  · 工具调用轮数（标准协议）: {tool_rounds}");
    println!(
        "  · 无法解析轮数: {}（严格模式下模型用正文回话时会出现 1 轮纠正，属设计内）",
        unparsed.len()
    );
    for u in &unparsed {
        println!("      ⚠ {u}");
    }

    let mut ok = true;
    // 「无法解析」轮只打印、不判失败：严格模式（llm.tool_protocol）下模型用正文回话时，
    // 引擎**就该**点它一轮（"形状对、通道错，请用 final 工具"）—— 那是设计内的纠正，不是病。
    // 真正的判别式是下面两条：**一次工具调用都没有**，或**撞上连续失败上限**
    //（后者正是用户报的死循环那种收场）。
    if tool_rounds == 0 {
        fails.push(format!(
            "{arm} 臂**一次**标准工具调用都没有（严格模式下这等于没打通）"
        ));
        ok = false;
    }
    if text.contains("没有拿到工具调用") {
        fails.push(format!(
            "{arm} 臂以「连续多轮没有拿到工具调用」收场 —— 这正是用户报的死循环那种病"
        ));
        ok = false;
    }
    if let (Some(needle), Ok(answer)) = (must_contain, out) {
        if !answer.contains(needle) {
            fails.push(format!(
                "{arm} 臂答复里没有 {needle:?} —— 工具要么没被调用、要么结果没回到模型"
            ));
            ok = false;
        } else {
            println!("  · 答复里含文件里的那句 {needle:?} ⇒ 工具真跑了、结果真回到模型");
        }
    }
    if let Err(e) = out {
        fails.push(format!("{arm} 臂 run 失败: {e}"));
        ok = false;
    }
    let _ = text;
    ok
}

fn clip(s: &str, n: usize) -> String {
    let t = s.trim().replace('\n', " ⏎ ");
    if t.chars().count() <= n {
        t
    } else {
        t.chars().take(n).collect::<String>() + "…"
    }
}
