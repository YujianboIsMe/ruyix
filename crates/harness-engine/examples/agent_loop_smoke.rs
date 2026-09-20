//! 工具循环 + 质量门禁的端到端冒烟：**脚本化假 LLM** 跑真循环、真验证、真 shell。
//!
//! 为什么要有它：单元测试验的是"某一格"（解析、汇总、回灌文案），而这条链路的坑都在
//! 格子之间 —— 连接清单有没有进首条用户消息、写完文件有没有立刻跑语法层、交付前是不是
//! 真的跑了全量验证、验证失败有没有拦住 `final`、复核 agent 的上下文是不是干净的。
//!
//! 三臂（每条都真跑子进程：python 语法检查 + unittest）：
//! - **arm 1 正常路径**：四原语全走一遍 + 交付前验证通过 + 干净上下文复核
//! - **arm 2 验证失败被拦**：写了坏代码 → 全量验证失败 → `final` 被打回 → 改好后放行
//! - **arm 3 预算用尽**：验证连续失败到预算上限 → 放行，但答复必须写明"未通过"
//!
//! 用法：`cargo run -p harness-engine --example agent_loop_smoke`（无需 API Key）
//!
//! 假 LLM 复用 `harness_engine::testllm`（与单测同一份实现，别各写一遍）。

use harness_engine::agent::{
    self, AgentOutcome, ConnectFuture, ConnectOutcome, ConnectRequest, ConnectTarget, Connector,
    HistoryMsg, WritePolicy,
};
use harness_engine::config::AppConfig;
use harness_engine::pipeline::Sink;
use harness_engine::testllm::{FakeLlm, fake_llm};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ============================================
// 断言小工具
// ============================================

#[derive(Default)]
struct Report {
    total: usize,
    failed: Vec<String>,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool) {
        self.total += 1;
        println!("{} {}", if ok { "PASS" } else { "FAIL" }, name);
        if !ok {
            self.failed.push(name.to_string());
        }
    }
}

// ============================================
// 夹具项目 / 连接器 / Sink
// ============================================

struct PrintSink;
impl Sink for PrintSink {
    fn log(&self, level: &str, msg: String) {
        println!("[{level}] {msg}");
    }
}

/// 记录型连接器：宿主侧（MCP/A2A）在 ruyix 有真实现，这里只验引擎怎么用它
struct RecordingConnector {
    calls: Mutex<Vec<ConnectRequest>>,
}

impl Connector for RecordingConnector {
    fn list(&self) -> ConnectFuture<'_, Vec<ConnectTarget>> {
        Box::pin(async {
            Ok(vec![
                ConnectTarget {
                    kind: "mcp".into(),
                    name: "fs".into(),
                    detail: "已连接（py-echo 1.0）".into(),
                    tools: vec!["echo(回显文本)".into()],
                },
                ConnectTarget {
                    kind: "a2a".into(),
                    name: "translator".into(),
                    detail: "http://127.0.0.1:9999".into(),
                    tools: Vec::new(),
                },
            ])
        })
    }

    fn call(&self, req: ConnectRequest) -> ConnectFuture<'_, ConnectOutcome> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(req.clone());
            Ok(ConnectOutcome {
                text: format!("echo: {}", req.arguments["text"].as_str().unwrap_or("")),
                is_error: false,
            })
        })
    }
}

fn temp_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "harness-gate-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Python 夹具：`calc.add` + 一个真跑的 unittest（全量验证会真的执行它）
fn write_py_fixture(dir: &Path, add_body: &str) {
    std::fs::write(
        dir.join("calc.py"),
        format!("def add(a, b):\n    {add_body}\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("test_calc.py"),
        "import unittest\nfrom calc import add\n\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\n\nif __name__ == '__main__':\n    unittest.main()\n",
    )
    .unwrap();
}

/// 跑一臂：起假 LLM → 建配置（冒烟要确定性：不碰 Docker、不依赖 tools/lint）→ 真跑循环
fn run_loop(
    dir: &Path,
    task: &str,
    script: Vec<String>,
    reflect: bool,
    tweak: impl FnOnce(&mut AppConfig),
) -> (FakeLlm, RecordingConnector, AgentOutcome) {
    let llm = fake_llm(script);
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.sandbox.mode = "off".into();
    cfg.lint.enabled = false;
    cfg.reflect.enabled = reflect;
    tweak(&mut cfg);

    let conn = RecordingConnector {
        calls: Mutex::new(Vec::new()),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let out = rt
        .block_on(agent::run(
            &cfg,
            dir,
            task,
            &[HistoryMsg {
                role: "user".into(),
                text: "你好".into(),
            }],
            WritePolicy::Apply,
            &conn,
            &harness_engine::exec::new_cancel_flag(),
            &PrintSink,
        ))
        .expect("工具循环不该失败");
    (llm, conn, out)
}

fn js(v: &str) -> String {
    serde_json::to_string(v).unwrap()
}

fn layers(out: &AgentOutcome) -> Vec<String> {
    out.verifications
        .iter()
        .map(|v| format!("{}:{}", v.layer, v.status))
        .collect()
}

// ============================================
// arm 1：正常路径（四原语 + 门禁 + 干净上下文复核）
// ============================================

fn arm_happy_path(rep: &mut Report) {
    println!("\n=== arm 1：正常路径 ===");
    let dir = temp_project("happy");
    write_py_fixture(&dir, "return a + b");
    let script: Vec<String> = vec![
        r#"{"tool":"read","args":{"path":"."}}"#.into(),
        r#"{"tool":"plan","args":{"steps":[{"title":"改 hello.py","detail":"整份写入","files":["hello.py"]}]}}"#.into(),
        r#"{"tool":"connect","args":{"action":"list"}}"#.into(),
        r#"{"tool":"connect","args":{"action":"call","server":"fs","tool":"echo","arguments":{"text":"hi"}}}"#.into(),
        format!(
            r#"{{"tool":"write","args":{{"path":"hello.py","content":{}}}}}"#,
            js("def hi():\n    return \"hi\"\n")
        ),
        r#"{"tool":"execute","args":{"cmd":"echo smoke-ok","timeout_secs":15}}"#.into(),
        format!(
            r#"{{"final":{}}}"#,
            js("已写入 hello.py，并跑通了验证。")
        ),
        // 交付门禁触发的复核调用，走的是同一条假 LLM 通路
        r#"{"verdict":"ok","summary":"与任务一致","findings":[]}"#.into(),
    ];
    let (llm, conn, out) = run_loop(&dir, "把 hello.py 写上问候语并验证", script, true, |_| {});

    // ① 连接清单进首条用户消息（模型不该猜名字）
    let first = llm.request(0);
    rep.check("首条请求带连接清单", first.contains("mcp fs：已连接"));
    rep.check("首条请求带工具名", first.contains("echo(回显文本)"));
    rep.check(
        "系统提示词只要四个原语",
        ["read", "write", "execute", "connect"]
            .iter()
            .all(|t| first.contains(t)),
    );

    // ② 四原语都真的用上了
    let tools: Vec<&str> = out.steps.iter().map(|s| s.tool.as_str()).collect();
    rep.check(
        "轮次轨迹 = read/plan/connect/connect/write/execute",
        tools == ["read", "plan", "connect", "connect", "write", "execute"],
    );
    let calls = conn.calls.lock().unwrap();
    rep.check(
        "connect call 落到宿主且参数原样透传",
        calls.len() == 1 && calls[0].server == "fs" && calls[0].arguments["text"] == "hi",
    );

    // ③ 机械验证两层都跑了，且全量通过
    rep.check(
        "写完文件立刻跑了语法层，交付前跑了全量验证",
        layers(&out) == ["narrow:passed", "full:passed"],
    );

    // ④ 复核用的是**干净上下文**：只带产物与验证报告，不带主循环的轨迹
    rep.check("复核调用了模型", out.reflections.len() == 1);
    let reflect_req = llm.request(7);
    rep.check("复核请求用的是复核员提示词", reflect_req.contains("复核员"));
    rep.check(
        "复核请求不含主循环的系统提示词",
        !reflect_req.contains("你是 ruyix IDE 里的编程 Agent"),
    );
    rep.check(
        "复核请求不含主循环的工具轨迹（plan/connect 调用原文）",
        !reflect_req.contains(r#""tool":"plan""#) && !reflect_req.contains("echo: hi"),
    );
    rep.check(
        "复核请求带上了机械验证报告",
        reflect_req.contains("机械验证报告") && reflect_req.contains("通过"),
    );

    // ⑤ 验证通过 + 复核通过 → 答复就是 final 原文，没有额外告示
    rep.check(
        "答复即 final 原文（通过时不该有多余告示）",
        out.answer.trim() == "已写入 hello.py，并跑通了验证。",
    );
    rep.check(
        "变更记录在案",
        out.changes.len() == 1 && out.changes[0].path == "hello.py",
    );
    rep.check("假 LLM 的剧本刚好演完（没有多余轮次）", llm.count() == 8);
    let _ = std::fs::remove_dir_all(&dir);
}

// ============================================
// arm 2：验证失败被拦（门禁的核心作用）
// ============================================

fn arm_verify_blocks(rep: &mut Report) {
    println!("\n=== arm 2：验证失败被拦 ===");
    let dir = temp_project("blocked");
    write_py_fixture(&dir, "return a + b");
    let broken = "def add(a, b):\n    return a - b\n";
    let fixed = "def add(a, b):\n    return a + b\n";
    let script: Vec<String> = vec![
        format!(
            r#"{{"tool":"write","args":{{"path":"calc.py","content":{}}}}}"#,
            js(broken)
        ),
        format!(r#"{{"final":{}}}"#, js("我把 add 改好了。")),
        // 被门禁打回后，模型照验证提示真改回来
        format!(
            r#"{{"tool":"write","args":{{"path":"calc.py","content":{}}}}}"#,
            js(fixed)
        ),
        format!(r#"{{"final":{}}}"#, js("重新改好并验证通过。")),
    ];
    // 本臂只验机械验证，关掉复核
    let (llm, _conn, out) = run_loop(&dir, "把 calc.add 改成正确的加法", script, false, |_| {});
    rep.check(
        "轨迹：坏改动 → 全量验证失败 → 改回 → 再验通过",
        layers(&out)
            == [
                "narrow:passed",
                "full:failed",
                "narrow:passed",
                "full:passed",
            ],
    );
    // 打回时模型收到的是"验证报告 + 修法"，不是一句"你错了"
    let after_block = llm.request(2);
    rep.check(
        "打回信息里带验证报告（层名 + 失败项）",
        after_block.contains("[机械验证·全量验证]") && after_block.contains("修完再交付"),
    );
    rep.check(
        "答复是第二次 final 的原文",
        out.answer.trim() == "重新改好并验证通过。",
    );
    rep.check(
        "最终落盘的是修好的内容",
        std::fs::read_to_string(dir.join("calc.py"))
            .unwrap()
            .contains("a + b"),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ============================================
// arm 3：预算用尽 → 放行但必须写明"未通过"
// ============================================

fn arm_budget_exhausted(rep: &mut Report) {
    println!("\n=== arm 3：预算用尽 ===");
    let dir = temp_project("budget");
    write_py_fixture(&dir, "return a + b");
    let broken = "def add(a, b):\n    return a - b\n";
    let script: Vec<String> = vec![
        format!(
            r#"{{"tool":"write","args":{{"path":"calc.py","content":{}}}}}"#,
            js(broken)
        ),
        format!(r#"{{"final":{}}}"#, js("改好了。")),
        // 被拦一次后不再改，直接再交付一次 → 命中预算上限
        format!(r#"{{"final":{}}}"#, js("我认为已经完成。")),
    ];
    // 一次失败就用尽预算；本臂只验门禁的"诚实放行"，关掉复核
    let (_llm, _conn, out) = run_loop(&dir, "把 calc.add 改对", script, false, |c| {
        c.gate.max_full_attempts = 1;
    });

    rep.check(
        "验证失败只跑一次（预算=1）",
        layers(&out) == ["narrow:passed", "full:failed"],
    );
    rep.check(
        "答复里写明按预算放行且结论未通过",
        out.answer.contains("按预算放行") && out.answer.contains("未通过"),
    );
    rep.check(
        "答复仍保留模型原话（不吞掉它的说明）",
        out.answer.contains("我认为已经完成。"),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn main() {
    let mut rep = Report::default();
    arm_happy_path(&mut rep);
    arm_verify_blocks(&mut rep);
    arm_budget_exhausted(&mut rep);

    println!("\n=== 汇总 ===");
    if rep.failed.is_empty() {
        println!("冒烟通过：{} 项断言全绿", rep.total);
    } else {
        eprintln!(
            "冒烟失败 {} / {} 项：{}",
            rep.failed.len(),
            rep.total,
            rep.failed.join(" / ")
        );
        std::process::exit(1);
    }
}
