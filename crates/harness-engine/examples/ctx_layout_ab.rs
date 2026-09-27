//! v1.2 P1 的**量尺**：同一份任务、两臂（`agent.ctx.layout` 关 / 开），逐轮打印上下文计量，
//! 最后给出加权合计与方向判读。
//!
//! 为什么要有它：需求 §5 的判据是"真 run 上公共前缀占比**要涨**"，而"涨没涨"必须能**当场量**
//! 出来，不能靠事后讲故事。这条量尺在两臂用的是**同一个**仪器（`agent.ctx.metrics` 都开着，
//! 处理变量只有 `agent.ctx.layout`）—— 仪器只长在实验臂上，那个"上升"就无从比较。
//!
//! 两种跑法：
//! ```text
//! cargo run -q -p harness-engine --example ctx_layout_ab
//!     └─ P1 布局 A/B（脚本化假 LLM，确定性、零成本）
//! cargo run -q -p harness-engine --example ctx_layout_ab -- --dedup
//!     └─ P2 去重 A/B（同一份仪器，处理变量只有 `agent.ctx.dedup`）
//! cargo run -q -p harness-engine --example ctx_layout_ab -- --real [--dedup]
//!     └─ 真模型（要 DEEPSEEK_API_KEY）
//! ```
//! 真跑可另给 `DEEPSEEK_BASE_URL` / `DEEPSEEK_MODEL` / `DEEPSEEK_API_FORMAT`（缺省按 base_url
//! 里有没有 `/anthropic` 认协议）。
//!
//! 判读规则（**预注册**，见需求 §5）：P1 看加权占比"开 > 关"；P2 看**重复执行次数下降**
//! （`仍重复执行` 与 `拦截重复` 是同一件事的两面）。方向反了 ⇒ 否定判据触发，按排期回滚。

use harness_engine::agent::context::{PrefixTally, prefix_stat};
use harness_engine::agent::{self, AgentOutcome, WritePolicy};
use harness_engine::config::{self, AppConfig};
use harness_engine::llm::ChatMessage;
use harness_engine::pipeline::Sink;
use harness_engine::testllm::{FakeLlm, fake_llm};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 只放计量行过闸：两臂各几十行日志会把表冲散，而这张表要的就是那几行。
struct MetricSink {
    lines: Mutex<Vec<String>>,
}

impl MetricSink {
    fn new() -> Self {
        Self {
            lines: Mutex::new(Vec::new()),
        }
    }

    /// 引擎自己写的那条小结里读回加权占比 —— 这一臂的读数**就取引擎的话**，
    /// 不另算一套（真跑时请求体在厂商那边，也拿不到别的数）。
    fn engine_ratio(&self) -> Option<f64> {
        let line = self
            .lines
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| l.contains("上下文计量小结"))?
            .clone();
        let rest = line.split_once("加权公共前缀 ")?.1.to_string();
        let pct = rest.split('%').next()?.trim().parse::<f64>().ok()?;
        Some(pct / 100.0)
    }

    /// 账本小结里取一个数（`拦截重复 N` / `仍重复执行 N` / `唯一执行 N`）
    fn ledger_num(&self, key: &str) -> Option<u64> {
        let line = self
            .lines
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| l.contains("去重账本"))?
            .clone();
        let rest = line.split_once(key)?.1.trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    }

    fn round_lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains("第 ") && l.contains("轮 上下文计量"))
            .cloned()
            .collect()
    }
}

impl Sink for MetricSink {
    fn log(&self, _level: &str, msg: String) {
        // 两套仪器都收：P1 的逐轮计量 + P2 的账本小结（它们本来就是给同一次 run 量的）
        if msg.contains("上下文计量") || msg.contains("去重账本") {
            println!("      {msg}");
            self.lines.lock().unwrap().push(msg);
        }
    }
}

/// 一臂的读数（假 LLM 那边把请求体一起带走，用来重算校验）
struct Arm {
    reqs: Vec<Vec<ChatMessage>>,
    outcome: AgentOutcome,
}

impl Arm {
    fn tally(&self) -> Option<PrefixTally> {
        if self.reqs.is_empty() {
            return None;
        }
        let mut t = PrefixTally::default();
        for (i, cur) in self.reqs.iter().enumerate() {
            t.add(prefix_stat(
                if i == 0 {
                    None
                } else {
                    Some(&self.reqs[i - 1])
                },
                cur,
            ));
        }
        Some(t)
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(f)
}

fn temp_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ruyix-ctx-ab-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 量尺统一的配置：**只留工具循环本体**（门禁 / 复核 / 计划派发 / 命令发现 / 提问全关）——
/// 它们各自会再发请求，会把"这一轮的请求"换成另一种东西，两臂就没法对着比了。
fn base_cfg(layout: bool, dedup: bool) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.sandbox.mode = "off".into();
    cfg.lint.enabled = false;
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;
    cfg.step.execute_plan = false;
    cfg.discover.enabled = false;
    cfg.ask.enabled = false;
    // **仪器两臂都开**，处理变量只有一个（P1 是 layout，P2 是 dedup）
    cfg.agent.ctx.metrics = true;
    cfg.agent.ctx.layout = layout;
    cfg.agent.ctx.dedup = dedup;
    cfg
}

// ============================================
// 臂一：脚本化假 LLM（确定性、零成本）
// ============================================

/// 五轮读 + 一轮交付：每轮读到的正文都进历史（真实 run 里账单的大头就是它）
fn script() -> Vec<String> {
    (1..=5)
        .map(|i| format!(r#"{{"tool":"read","args":{{"path":"mod{i}.rs"}}}}"#))
        .chain(std::iter::once(r#"{"final":"读完了"}"#.to_string()))
        .collect()
}

/// 去重臂的脚本：**故意重复** —— 同一个文件读两遍（真 run 里最常见的浪费就是这个）。
///
/// 只用 `read`：`execute` 的纯命令要真跑成功才会入账（失败的结果**不缓存** ——
/// 那会静默把一次瞬时失败钉死），所以确定性脚本里用读来演示；
/// `execute` 那一半由 `agent/ledger.rs` 的单测与黄金测试盯着（白名单内/外都各有一条）。
fn dedup_script() -> Vec<String> {
    vec![
        r#"{"tool":"read","args":{"path":"mod1.rs"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"mod1.rs"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"mod2.rs"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"mod2.rs"}}"#.to_string(),
        r#"{"final":"看完了"}"#.to_string(),
    ]
}

/// 夹具：五个 ~4KB 的文件（太小的话"历史"不构成账单，量不出布局的价值）
fn write_fixture(dir: &Path) {
    for i in 1..=5 {
        let body = format!(
            "// mod{i}：夹具正文，用来给历史一个真实体量\n{}",
            format!("pub fn f{i}(x: u32) -> u32 {{ x + {i} }}\n").repeat(120)
        );
        std::fs::write(dir.join(format!("mod{i}.rs")), body).unwrap();
    }
}

/// 脚本化臂：**给定脚本**跑一臂（确定性；配对回放也走这里 —— 两臂的实现只有这一份）
fn scripted_arm(
    dir: &Path,
    layout: bool,
    dedup: bool,
    script: Vec<String>,
    sink: &MetricSink,
) -> Arm {
    let llm = fake_llm(script);
    let mut cfg = base_cfg(layout, dedup);
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();

    let outcome = block_on(agent::run(
        &cfg,
        dir,
        "把五个文件都读一遍，然后交付一句话结论",
        &[],
        WritePolicy::Apply,
        &agent::NoConnector,
        &harness_engine::exec::new_cancel_flag(),
        sink,
    ))
    .expect("工具循环不该失败");

    Arm {
        reqs: requests(&llm),
        outcome,
    }
}

/// 假 LLM 收到的每一次请求的 messages（判据看的是**模型实际看到了什么**）
fn requests(llm: &FakeLlm) -> Vec<Vec<ChatMessage>> {
    (0..llm.count())
        .map(|i| {
            let v: serde_json::Value =
                serde_json::from_str(&llm.request(i)).expect("请求体该是 JSON");
            v["messages"]
                .as_array()
                .expect("请求体里该有 messages")
                .iter()
                .map(|m| ChatMessage {
                    role: m["role"].as_str().unwrap_or_default().to_string(),
                    content: m["content"].as_str().unwrap_or_default().to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                })
                .collect()
        })
        .collect()
}

// ============================================
// 臂二：真模型
// ============================================

/// 真跑夹具：几个"得读才能答"的文件（覆盖顺序 / 返回码含义都藏在代码里）
fn write_real_fixture(dir: &Path) {
    std::fs::write(
        dir.join("config_loader.py"),
        r#"import os
import json

DEFAULTS = {"timeout": 30, "retries": 2, "verbose": False}


def load_defaults():
    return dict(DEFAULTS)


def load_file(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def load_env():
    out = {}
    if os.environ.get("APP_TIMEOUT"):
        out["timeout"] = int(os.environ["APP_TIMEOUT"])
    if os.environ.get("APP_VERBOSE"):
        out["verbose"] = os.environ["APP_VERBOSE"] == "1"
    return out


def resolve(path, cli_flags):
    """四层覆盖：默认值 → 文件 → 环境变量 → 命令行。后一层盖前一层。"""
    merged = load_defaults()
    merged.update(load_file(path))
    merged.update(load_env())
    merged.update({k: v for k, v in cli_flags.items() if v is not None})
    return merged
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("verify.py"),
        r#"import subprocess


def verify(project_root):
    """跑一遍检查，返回码的约定写在下面的分派里。"""
    proc = subprocess.run(["python", "-m", "compileall", "-q", project_root])
    if proc.returncode != 0:
        return 1  # 1 = 编译/语法没通过（确定性失败，重跑也没用）
    proc = subprocess.run(["python", "-m", "unittest", "discover", "-q", project_root])
    if proc.returncode != 0:
        return 2  # 2 = 语法过了但测试红了（可能是断言/环境问题）
    return 0  # 0 = 全通过


def describe(code):
    return {0: "通过", 1: "语法不通过", 2: "测试不通过"}.get(code, "未知返回码")
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("README.md"),
        "夹具项目：config_loader.py 管配置覆盖顺序，verify.py 管检查返回码。\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("decoy_utils.py"),
        "def noop():\n    return None\n".repeat(30),
    )
    .unwrap();
}

fn real_arm(dir: &Path, layout: bool, dedup: bool, sink: &MetricSink, task: &str) -> Arm {
    real_arm_try(dir, layout, dedup, sink, task).expect("工具循环不该失败")
}

/// 同 [`real_arm`]，但把失败**交回去**而不是 panic ——
/// 抄轨迹时要能如实说出"这一趟模型没走工具调用"（那是模型的偶发行为，引擎的守卫会报出来）。
fn real_arm_try(
    dir: &Path,
    layout: bool,
    dedup: bool,
    sink: &MetricSink,
    task: &str,
) -> Result<Arm, String> {
    let mut cfg = base_cfg(layout, dedup);
    config::apply_env_overrides(&mut cfg);
    // 协议：显式给了就听显式的，否则按 base_url 认（DeepSeek 的 `/anthropic` 端点就是 anthropic 协议）
    let fmt = std::env::var("DEEPSEEK_API_FORMAT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            if cfg.llm.base_url.contains("/anthropic") {
                "anthropic".into()
            } else {
                "openai".into()
            }
        });
    cfg.llm.api_format = fmt;
    println!(
        "      端点 {} · 模型 {} · 协议 {}",
        cfg.llm.base_url, cfg.llm.model, cfg.llm.api_format
    );

    let outcome = block_on(agent::run(
        &cfg,
        dir,
        task,
        &[],
        WritePolicy::Apply,
        &agent::NoConnector,
        &harness_engine::exec::new_cancel_flag(),
        sink,
    ))
    .map_err(|e| e.to_string())?;

    // 真跑的请求体在厂商那边，拿不到 ⇒ 这一臂的读数就取引擎自己写的那条小结
    Ok(Arm {
        reqs: Vec::new(),
        outcome,
    })
}

// 两臂的定义与判读各只有一处：直接跑法与**配对回放**都必须走同一份，
// 否则"配对"只是个说法（两臂的定义一旦分叉，量出来的差就不是开关的了）。

/// 两种模式各自的**臂定义**：(名字, layout, dedup)
fn arms_for(mode: Mode) -> [(&'static str, bool, bool); 2] {
    match mode {
        Mode::Layout => [
            ("关（块塞进 system，v1.1 行为）", false, false),
            ("开（块是易变尾 A）", true, false),
        ],
        Mode::Dedup => [
            ("关（每次调用都真跑，v1.1 行为）", false, false),
            ("开（纯工具 + 资源版本未变 ⇒ 复用）", false, true),
        ],
    }
}

/// 脚本化臂用哪份确定性脚本
fn entry_script(mode: Mode) -> Vec<String> {
    match mode {
        Mode::Layout => script(),
        Mode::Dedup => dedup_script(),
    }
}

/// 判读（预注册规则）：P1 看加权占比"开 > 关"；P2 看重复执行次数"开 < 关"。
fn verdict(mode: Mode, ratios: &[(&str, Option<f64>)], dups: &[(&str, Option<u64>, Option<u64>)]) {
    match mode {
        Mode::Layout => {
            println!("\n=== 判读（预注册规则：加权占比「开 > 关」才算方向正确）===");
            match (ratios[0].1, ratios[1].1) {
                (Some(a), Some(b)) => {
                    println!("  关：{:.1}%   开：{:.1}%", a * 100.0, b * 100.0);
                    if b > a {
                        println!("  ⇒ 方向正确（提升 {:.1} 个百分点）。", (b - a) * 100.0);
                    } else {
                        println!(
                            "  ⇒ **否定判据触发**：占比下降 = 布局改反了方向 ⇒ 按排期回滚到开关关掉的状态"
                        );
                        std::process::exit(1);
                    }
                }
                _ => println!(
                    "  ⇒ 这一臂没有量到读数（引擎那条小结没出现：检查 metrics 开关与日志出口）"
                ),
            }
        }
        Mode::Dedup => {
            println!("\n=== 判读（预注册规则：重复执行次数「开 < 关」才算方向正确）===");
            match (dups[0].2, dups[1].2) {
                (Some(before), Some(after)) => {
                    println!(
                        "  关：仍重复执行 {before} · 拦截 0      开：仍重复执行 {after} · 拦截 {}",
                        dups[1].1.unwrap_or(0)
                    );
                    if after < before {
                        println!(
                            "  ⇒ 方向正确（重复执行 {} → {}，下降 {:.0}%）。",
                            before,
                            after,
                            (before - after) as f64 * 100.0 / before.max(1) as f64
                        );
                    } else {
                        println!(
                            "  ⇒ **判据未满足**：重复执行次数没有下降 ⇒ 回滚 + 查白名单/版本语义"
                        );
                        std::process::exit(1);
                    }
                }
                _ => println!("  ⇒ 这一臂没有量到读数（检查 metrics 开关与日志出口）"),
            }
        }
    }
    println!(
        "  注：读数只在**两臂跑同一份动作序列**时才成立（直接跑法两臂轨迹由模型自己走，\n\
         那只是观察；配对回放 --pair 带保真校验）。"
    );
}

// ============================================
// 配对回放：**同一条动作序列**过两臂
// ============================================
//
// 为什么必须有它（需求 §8.4 / §9.3）：未配对时两臂的轨迹由模型自己走，处理效应与
// "这一趟怎么干"混在一起 —— 实测两次真 run 因此给出**相反**的结论。
//
// 做法：真 run 跑一次，把每轮动作抄下来（从引擎 `--debug` 日志的
// `----- tool_calls -----` 段，那是**模型原样发回的参数 JSON**），再让两臂跑同一份序列。
// 抄不进 traces 的地方：交付那一轮的回复（它没动作）也一起存，回放才能收尾。
//
// ```text
// cargo run -q -p harness-engine --example ctx_layout_ab -- --real --capture target/trace_p1.json
// cargo run -q -p harness-engine --example ctx_layout_ab -- --pair target/trace_p1.json
// cargo run -q -p harness-engine --example ctx_layout_ab -- --real --dedup --capture target/trace_p2.json
// cargo run -q -p harness-engine --example ctx_layout_ab -- --pair target/trace_p2.json --dedup
// ```

/// 一轮里模型发回的动作（`tool_calls` 段里 `name(args) [id=…]` 的那些）
type Round = Vec<(String, String)>;

/// 从 debug 日志抄出**每轮的动作**（`(工具名, 参数 JSON)`），以及每轮的**回显文本**。
///
/// 回显（assistant 消息）另抄一份是为了**保真校验**：回放之后回显必须逐条相同 ——
/// 那才能说"两臂跑的是同一份动作序列"，而不是"我相信它是"。
fn capture_rounds(log: &Path) -> (Vec<Round>, Vec<String>) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let mut rounds: Vec<Round> = Vec::new();
    let mut echoes: Vec<String> = Vec::new();
    for section in text
        .split("----- tool_calls（模型发回来的动作）-----\n")
        .skip(1)
    {
        // 段内每一行 = 一次调用；行首两个空格，末尾 ` [id=…]`
        let mut round: Round = Vec::new();
        for line in section.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with("-----") || l.starts_with("=====") {
                break;
            }
            let Some(open) = l.find('(') else { continue };
            let name = l[..open].trim().to_string();
            let rest = &l[open + 1..];
            let Some(close) = rest.rfind(") [id=") else {
                continue;
            };
            round.push((name, rest[..close].to_string()));
        }
        if !round.is_empty() {
            rounds.push(round);
        }
    }
    // 回显：取**最后一次请求**里的 assistant 消息（那里含全部历史）
    if let Some(i) = text.rfind("----- 消息（模型实际看到的内容）-----") {
        let rest = &text[i..];
        let end = rest[1..]
            .find("\n---")
            .map(|k| k + 1)
            .into_iter()
            .chain(rest[1..].find("\n===").map(|k| k + 1))
            .min()
            .unwrap_or(rest.len());
        for chunk in rest[..end].split("\n### [").skip(1) {
            let (role, body) = chunk.split_once("]\n").unwrap_or((chunk, ""));
            if role.trim() == "assistant" {
                let b = body.trim();
                if !b.is_empty() {
                    echoes.push(b.to_string());
                }
            }
        }
    }
    (rounds, echoes)
}

/// 把一"轮"动作变回**假 LLM 的脚本条目**（与 `testllm` 约定的形状一致）。
///
/// 单调用 ⇒ `{"tool":..,"args":..}`；一批 ⇒ `{"calls":[…]}`；`final` 单独成条
/// （引擎批里不许有控制动作，而真 run 里 plan/final 本来就是单独一轮）。
fn round_to_script(round: &Round) -> Option<String> {
    if round.is_empty() {
        return None;
    }
    if round.len() == 1 && round[0].0 == "final" {
        let args: serde_json::Value = serde_json::from_str(&round[0].1).ok()?;
        let answer = args.get("answer").cloned().unwrap_or_default();
        return Some(format!(
            r#"{{"final":{}}}"#,
            serde_json::to_string(&answer).ok()?
        ));
    }
    let calls: Vec<String> = round
        .iter()
        .map(|(name, args)| format!(r#"{{"tool":{},"args":{}}}"#, json_str(name), args))
        .collect();
    if calls.len() == 1 {
        Some(calls.into_iter().next().unwrap_or_default())
    } else {
        Some(format!(r#"{{"calls":[{}]}}"#, calls.join(",")))
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// 轨迹文件：动作序列 + 回显（保真校验用）+ 来源说明
#[derive(serde::Serialize, serde::Deserialize)]
struct Trace {
    note: String,
    actions: Vec<String>,
    echoes: Vec<String>,
}

fn save_trace(path: &Path, t: &Trace) -> std::io::Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(t).unwrap_or_default())
}

fn load_trace(path: &Path) -> Option<Trace> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// 配对回放的**保真度判据**：回放之后抄回来的回显必须与轨迹逐条相等。
///
/// 不相等就意味着"两臂跑的不是同一件事"，那么两臂的任何差都不该算到开关头上。
fn verify_pairing(trace: &Trace, log: &Path, name: &str) {
    let (_, echoes) = capture_rounds(log);
    if echoes == trace.echoes {
        println!(
            "     ✓ 配对保真：回放的动作序列与轨迹逐条一致（{} 轮 / {} 条动作）",
            echoes.len(),
            trace.actions.len()
        );
        return;
    }
    let at = echoes
        .iter()
        .zip(trace.echoes.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(echoes.len().min(trace.echoes.len()));
    println!(
        "     ✗ **配对失败（{name}）**：第 {} 轮起不一致（轨迹 {} 轮 / 回放 {} 轮）⇒ 这一臂的读数不可用",
        at + 1,
        trace.echoes.len(),
        echoes.len()
    );
    std::process::exit(1);
}

/// 真 run 一次（基线配置：layout/dedup 都关 = v1.1 行为），把动作序列抄成轨迹文件。
fn capture(out: &Path, task: &str) {
    let dir = temp_project("capture");
    write_real_fixture(&dir);
    let log = std::env::temp_dir().join("ruyix_ctx_capture.log");
    let _ = std::fs::remove_file(&log);
    harness_engine::debug::set_path(log.clone());
    harness_engine::debug::set_enabled(true);

    let sink = MetricSink::new();
    let arm = match real_arm_try(&dir, false, false, &sink, task) {
        Ok(a) => a,
        Err(e) => {
            harness_engine::debug::set_enabled(false);
            let _ = std::fs::remove_dir_all(&dir);
            eprintln!(
                "✗ 抄轨迹失败：{e}\n（这一趟模型没走工具调用 —— 引擎的守卫如实报了；重跑一次或换模型）"
            );
            std::process::exit(2);
        }
    };
    harness_engine::debug::set_enabled(false);
    let _ = std::fs::remove_dir_all(&dir);

    let (rounds, echoes) = capture_rounds(&log);
    let actions: Vec<String> = rounds.iter().filter_map(round_to_script).collect();
    if actions.is_empty() {
        eprintln!("✗ 抄轨迹失败：日志里没抄到任何工具调用（轨迹为空）");
        std::process::exit(2);
    }
    let t = Trace {
        note: format!(
            "真模型轨迹（{} 轮交付：{}）",
            actions.len(),
            first_line(&arm.outcome.answer)
        ),
        actions,
        echoes,
    };
    save_trace(out, &t).expect("轨迹该写得出去");
    println!(
        "✓ 轨迹已存：{}（{} 条动作 / {} 轮回显）",
        out.display(),
        t.actions.len(),
        t.echoes.len()
    );
}

/// 用同一份轨迹跑两臂（`mode` 决定处理变量是 layout 还是 dedup）。
fn pair(path: &Path, mode: Mode) {
    let Some(trace) = load_trace(path) else {
        eprintln!("轨迹文件读不出内容：{}", path.display());
        std::process::exit(2);
    };
    println!(
        "（配对回放：{} 条动作 / {} 轮回显，来自真模型轨迹；两臂跑**同一份**序列）",
        trace.actions.len(),
        trace.echoes.len()
    );

    let mut ratios: Vec<(&str, Option<f64>)> = Vec::new();
    let mut dups: Vec<(&str, Option<u64>, Option<u64>)> = Vec::new();
    for (name, layout, dedup) in arms_for(mode) {
        println!("\n>>> 臂 {name}");
        let sink = MetricSink::new();
        let dir = temp_project("pair");
        write_real_fixture(&dir);
        let log = std::env::temp_dir().join(format!("ruyix_ctx_pair_{}.log", name.len()));
        let _ = std::fs::remove_file(&log);
        harness_engine::debug::set_path(log.clone());
        harness_engine::debug::set_enabled(true);
        let arm = scripted_arm(&dir, layout, dedup, trace.actions.clone(), &sink);
        harness_engine::debug::set_enabled(false);
        let _ = std::fs::remove_dir_all(&dir);

        verify_pairing(&trace, &log, name);

        let rounds = sink.round_lines().len();
        if let Some(t) = arm.tally() {
            println!("     重算校验（由请求体独立算一遍）：{}", t.render());
        }
        println!(
            "     {} 轮 · 交付：{}",
            rounds,
            first_line(&arm.outcome.answer)
        );
        ratios.push((name, sink.engine_ratio()));
        dups.push((
            name,
            sink.ledger_num("拦截重复"),
            sink.ledger_num("仍重复执行"),
        ));
    }
    verdict(mode, &ratios, &dups);
}

// ============================================
// 主流程
// ============================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// P1：布局 A/B（处理变量 = `agent.ctx.layout`）
    Layout,
    /// P2：去重 A/B（处理变量 = `agent.ctx.dedup`）
    Dedup,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let real = args.iter().any(|a| a == "--real");
    let mode = if args.iter().any(|a| a == "--dedup") {
        Mode::Dedup
    } else {
        Mode::Layout
    };
    let (task, title) = match mode {
        Mode::Layout => (
            "读 config_loader.py 与 verify.py，回答两件事：(1) 四层配置覆盖顺序在哪段实现、\
             谁盖谁；(2) verify() 返回 1 和返回 2 分别代表什么。每条结论给出 path:line 证据，\
             然后用 final 交付一句话结论。不要改任何文件。",
            "v1.2 P1 上下文布局 A/B",
        ),
        Mode::Dedup => (
            // **故意请它复读**：第三步是"回同一处原文核对" —— 真 run 里最常见的浪费就是这个。
            // 这不是给判据喂答案：机制的主张本来就是"同一条纯调用不许执行第二次"；
            // 频率那一半由黄金测试与脚本化臂管（这里要验的是机制在真模型轨迹上真发生）。
            "分三步做：(1) 读 config_loader.py，回答四层配置覆盖顺序谁盖谁（给 path:line）；\
             (2) 读 verify.py，回答 verify() 返回 1 与返回 2 分别代表什么（给 path:line）；\
             (3) **回到 config_loader.py 的同一处再读一次原文**，逐字核对你在 (1) 里给的结论。\
             最后用 final 交付三条结论。不要改任何文件。",
            "v1.2 P2 去重账本 A/B",
        ),
    };

    // 配对回放的两条入口：先把真轨迹抄下来，再用同一份序列跑两臂
    if let Some(i) = args.iter().position(|a| a == "--capture") {
        let out = PathBuf::from(
            args.get(i + 1)
                .cloned()
                .unwrap_or_else(|| "ctx_trace.json".into()),
        );
        capture(&out, task);
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--pair") {
        let Some(path) = args.get(i + 1) else {
            eprintln!("--pair 需要一个轨迹文件：--pair <trace.json>");
            std::process::exit(2);
        };
        pair(Path::new(path), mode);
        return;
    }
    let arms = arms_for(mode);

    if real
        && std::env::var("DEEPSEEK_API_KEY")
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        eprintln!("真跑需要 DEEPSEEK_API_KEY（环境变量，不落盘）；只验仪器请去掉 --real");
        std::process::exit(2);
    }

    println!(
        "=== {title}：{} ===",
        if real {
            "真模型"
        } else {
            "脚本化假 LLM"
        }
    );
    println!(
        "（仪器两臂都开：agent.ctx.metrics=true；处理变量 = {}）",
        match mode {
            Mode::Layout => "agent.ctx.layout",
            Mode::Dedup => "agent.ctx.dedup",
        }
    );

    let mut ratios: Vec<(&str, Option<f64>)> = Vec::new();
    let mut dups: Vec<(&str, Option<u64>, Option<u64>)> = Vec::new();
    for (name, layout, dedup) in arms {
        println!("\n>>> 臂 {name}");
        let sink = MetricSink::new();
        let dir = temp_project(if real { "real" } else { "script" });
        if real {
            write_real_fixture(&dir);
        } else {
            write_fixture(&dir);
        }
        let arm = if real {
            real_arm(&dir, layout, dedup, &sink, task)
        } else {
            scripted_arm(&dir, layout, dedup, entry_script(mode), &sink)
        };
        let _ = std::fs::remove_dir_all(&dir);

        let rounds = sink.round_lines().len();
        if let Some(t) = arm.tally() {
            println!("     重算校验（由请求体独立算一遍）：{}", t.render());
        }
        match arm.outcome.usage.cache_read() {
            Some(hit) => println!(
                "     厂商回报命中：{hit} / {} tokens（**真值**；脚本化臂没有这个数才对）",
                arm.outcome.usage.prompt_tokens
            ),
            None => println!("     厂商回报命中：无（这一路没有回报这个数）"),
        }
        println!(
            "     {} 轮 · 交付：{}",
            rounds,
            first_line(&arm.outcome.answer)
        );
        ratios.push((name, sink.engine_ratio()));
        dups.push((
            name,
            sink.ledger_num("拦截重复"),
            sink.ledger_num("仍重复执行"),
        ));
    }

    verdict(mode, &ratios, &dups);
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(80).collect()
}
