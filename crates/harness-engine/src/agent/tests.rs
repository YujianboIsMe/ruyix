use super::*;

// 测试按域分文件（2026-09-28 拆）：夹具与共享 helper 留在本文件，
// 每个域的测试在 `tests/<域>.rs`（`use super::*` 取本文件的夹具）。
mod ask;
mod connect;
mod dedup;
mod execute;
mod gate;
mod history;
mod layout;
mod parse;
mod plan;
mod prompt;
mod read_write;
mod waves;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "dh-agent-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn write(&self, rel: &str, content: &str) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `tool_read` 收 `ReadSpec`（窗口读的入口）。整份读在测试里包一层 ——
/// 让每个断言只说自己关心的事，不必每行都写一遍 `ReadSpec::whole`
fn rd(ctx: &Ctx<'_>, path: &str) -> Result<String, String> {
    ctx.tool_read(&ReadSpec::whole(path))
}

/// 跑一轮工具循环：无提问通道、无连接器、Apply 策略、静默 sink —— 工具协议那几条用例共用。
fn run_loop(cfg: &crate::config::AppConfig, root: &std::path::Path) -> AgentOutcome {
    block_on(run_with_ask(
        cfg,
        root,
        "任务",
        &[],
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &NoAsker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败")
}

/// 假连接器：记录收到的请求（Connect 的路由与错误语义由它验）
struct Recorder {
    calls: std::sync::Mutex<Vec<ConnectRequest>>,
    fail: bool,
}

impl Connector for Recorder {
    fn list(&self) -> ConnectFuture<'_, Vec<ConnectTarget>> {
        Box::pin(async {
            Ok(vec![ConnectTarget {
                kind: "mcp".into(),
                name: "fs".into(),
                detail: "已连接".into(),
                tools: vec!["read_file(读文件)".into()],
            }])
        })
    }

    fn call(&self, req: ConnectRequest) -> ConnectFuture<'_, ConnectOutcome> {
        Box::pin(async move {
            if self.fail {
                return Err("连不上 fs".into());
            }
            self.calls.lock().unwrap().push(req.clone());
            Ok(ConnectOutcome {
                text: format!("{} 返回", req.action),
                is_error: false,
            })
        })
    }
}

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
            // enable_all 而不是只用 enable_time：整循环级用例要真发 HTTP 请求（假 LLM
            // 是本地 TCP 服务），只开 time driver 会连不上
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
}

/// 门禁单测用的 Sink：不刷屏、不落盘
struct QuietSink;

impl Sink for QuietSink {}

// ============================================
// v0.8：第五个动作 ask_user（需求歧义只能问委托人）
// ============================================

/// 脚本化提问通道：按序给答案（`Err` = 拿不到答案，用来验 fail-closed），并留档问过什么。
/// 剧本演完一律回"没人回答" —— 测试里绝不允许出现"引擎自己猜了个答案"。
struct ScriptAsker {
    answers: std::sync::Mutex<std::collections::VecDeque<Result<String, AskErr>>>,
    seen: std::sync::Mutex<Vec<(String, String, String)>>,
}

impl ScriptAsker {
    fn new(items: Vec<Result<&str, AskErr>>) -> Self {
        Self {
            answers: std::sync::Mutex::new(
                items
                    .into_iter()
                    .map(|r| r.map(|s| s.to_string()))
                    .collect(),
            ),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }
    /// 问过几次、分别问了什么（id / question / why）
    fn asked(&self) -> Vec<(String, String, String)> {
        self.seen.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

impl Asker for ScriptAsker {
    fn ask<'a>(&'a self, id: &'a str, spec: &'a AskSpec) -> AskFut<'a> {
        self.seen
            .lock()
            .unwrap()
            .push((id.to_string(), spec.question.clone(), spec.why.clone()));
        let next = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(AskErr::NoAsker));
        Box::pin(async move {
            next.map(|text| AskAnswer {
                text,
                option_index: None,
                ts: String::new(),
            })
        })
    }
}

/// 提问类测试的公共配置（关掉与提问无关的层，把轮次压到最少）
fn ask_cfg(llm: &crate::testllm::FakeLlm) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;
    cfg.step.execute_plan = false;
    cfg.discover.enabled = false;
    // 同 `quiet_cfg`：**基线不许依赖出厂默认值** —— 要哪一条，用例自己打开。
    cfg.agent.ctx.layout = false;
    cfg.agent.ctx.metrics = false;
    cfg.agent.ctx.dedup = false;
    cfg.agent.ctx.capsule = false;
    cfg.agent.ctx.schedule = false;
    cfg
}

/// 造一条 `ask_user` 剧本（免去长行 + raw string 转义：这类脚本两个坑都踩过）
fn ask_json(q: &str, why: &str, opts: &[&str]) -> String {
    serde_json::json!({
        "tool": "ask_user",
        "args": {"question": q, "why": why, "options": opts}
    })
    .to_string()
}

/// 门禁单测用的配置：不依赖 Docker、不绑 tools/lint、不调模型（复核另测）
fn apply_cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.sandbox.mode = "off".into();
    cfg.lint.enabled = false;
    cfg.reflect.enabled = false;
    cfg
}

fn one_change(path: &str) -> Vec<FileChange> {
    vec![FileChange {
        path: path.into(),
        kind: "modify".into(),
        before: Some("旧内容\n".into()),
        after: "新内容\n".into(),
    }]
}

fn ctx_with<'a>(proj: &'a Path, changes: Vec<FileChange>, policy: WritePolicy) -> Ctx<'a> {
    Ctx {
        proj,
        overlay: BTreeMap::new(),
        changes,
        probes: Vec::new(),
        progress: Progress::new(),
        policy,
        backup_dir: None,
        state_root: None,
    }
}

/// 记录 step 事件的 Sink —— 断言 UI 真会收到什么，而不是把谓词再抄一遍
#[derive(Default)]
struct StepSink {
    seen: std::sync::Mutex<Vec<(usize, usize, String, String)>>,
}

impl Sink for StepSink {
    fn step(&self, i: usize, total: usize, st: &StepOutcome) {
        self.seen
            .lock()
            .unwrap()
            .push((i, total, st.status.clone(), st.notes.clone()));
    }
}

impl StepSink {
    /// (index, total, status, notes)
    fn events(&self) -> Vec<(usize, usize, String, String)> {
        self.seen.lock().unwrap().clone()
    }
}

fn plan_step(id: u32, title: &str, files: &[&str], kind: &str) -> PlanStep {
    PlanStep {
        id,
        title: title.into(),
        detail: String::new(),
        files: files.iter().map(|f| (*f).to_string()).collect(),
        kind: kind.into(),
    }
}

// ============================================
// 原子能力的参数形状：read 的窗口 / write 的锚点 / 工具循环的历史折叠
// ============================================

/// 建一个只读上下文（`Ctx` 的字段列表只在这里出现一次：加字段时只改这一处）
fn ctx_for(d: &TempDir) -> Ctx<'_> {
    Ctx {
        progress: Progress::new(),
        proj: &d.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Stage,
        backup_dir: None,
        state_root: None,
    }
}

fn ed(find: &str, replace: &str) -> AgentEdit {
    AgentEdit {
        find: find.to_string(),
        replace: replace.to_string(),
    }
}

/// 500 行的样例文件（窗口读的靶子）
fn lines_fixture(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("line {i}")).collect()
}

/// 五轮"读一个互相认得出来的文件" + 交付：折叠测试的靶子
fn fold_fixture(tag: &str) -> (TempDir, crate::testllm::FakeLlm) {
    let d = TempDir::new(tag);
    for i in 1..=5 {
        d.write(
            &format!("f{i}.txt"),
            &format!("MARK{i}-{}", "A".repeat(400)),
        );
    }
    let script: Vec<String> = (1..=5)
        .map(|i| format!(r#"{{"tool":"read","args":{{"path":"f{i}.txt"}}}}"#))
        .chain(std::iter::once(r#"{"final":"读完了"}"#.to_string()))
        .collect();
    (d, crate::testllm::fake_llm(script))
}

// ============================================================================
// 进展记忆与循环守卫（v1.1）
// 见 `doc/v1.1/需求-Agent-进展记忆与循环守卫-v1.1.md` §6.3
// ============================================================================

fn prog() -> super::findings::Progress {
    let mut p = super::findings::Progress::new();
    p.set_cap(8192);
    p
}

// ============================================================================
// v1.2 P1：上下文布局契约 [S][E][A]（`agent.ctx.layout` / `agent.ctx.metrics`）
//
// 见 `doc/v1.2/需求-Agent-上下文布局与去重账本-v1.2.md` §1 与
// `doc/v1.2/架构-上下文账本与重基线调度-v1.2.md` §1。
// 判据全部落在**模型实际看到的东西**上（请求体），不落在我们自己的账本上。
// ============================================================================

/// 假 LLM 收到的第 i 次请求的 messages —— 判据的唯一事实来源。
fn req_msgs(llm: &crate::testllm::FakeLlm, i: usize) -> Vec<ChatMessage> {
    let body = llm.request(i);
    let v: serde_json::Value = serde_json::from_str(&body).expect("请求体该是 JSON");
    v["messages"]
        .as_array()
        .expect("请求体里该有 messages 数组")
        .iter()
        .map(|m| ChatMessage {
            role: m["role"].as_str().unwrap_or_default().to_string(),
            content: m["content"].as_str().unwrap_or_default().to_string(),
            tool_calls: None,
            tool_call_id: None,
            images: Vec::new(),
        })
        .collect()
}

/// 从请求体序列算出全程**加权**复用率（与引擎里那个仪器同一份实现）
fn prefix_tally(llm: &crate::testllm::FakeLlm, rounds: usize) -> context::PrefixTally {
    let reqs: Vec<Vec<ChatMessage>> = (0..rounds).map(|i| req_msgs(llm, i)).collect();
    let mut t = context::PrefixTally::default();
    for (i, cur) in reqs.iter().enumerate() {
        t.add(context::prefix_stat(
            if i == 0 { None } else { Some(&reqs[i - 1]) },
            cur,
        ));
    }
    t
}

/// 跑一轮「读五个文件 → 交付」（六次模型调用；`fold_fixture` 的同一份靶子）。
async fn run_five_reads(cfg: &AppConfig, root: &std::path::Path) -> AgentOutcome {
    run(
        cfg,
        root,
        "把五个文件都读一遍",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    )
    .await
    .expect("run 不该失败")
}

// ============================================================================
// v1.2 P2：去重账本（`agent.ctx.dedup` + 仪器 `agent.ctx.metrics`）
//
// 判据的取向：**宁可多执行一次，也不许"看起来一样就跳过"**。所以每条断言都问两件事 ——
// ① 该省的时候省了吗 ② 不该省的时候（不纯 / 版本变了）有没有老实执行。
// ============================================================================

/// 留档型 Sink：把引擎的日志行收下来。P2 的两条读数（`dedup` 行、账本小结）
/// 只有从这里看得见 —— 它们是**给模型与运维看的**，不是我们自己的内部计数。
#[derive(Default)]
struct LogSink {
    lines: std::sync::Mutex<Vec<String>>,
}

impl LogSink {
    fn joined(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }
}

impl Sink for LogSink {
    fn log(&self, _level: &str, msg: String) {
        self.lines.lock().unwrap().push(msg);
    }
}

/// 跑一轮并把日志收下来（P2 用）
async fn run_logging(
    cfg: &AppConfig,
    root: &std::path::Path,
    script: Vec<String>,
) -> (crate::testllm::FakeLlm, AgentOutcome, String) {
    let llm = crate::testllm::fake_llm(script);
    let mut cfg = cfg.clone();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    let sink = LogSink::default();
    let out = run(
        &cfg,
        root,
        "把同一个文件读两遍",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &sink,
    )
    .await
    .expect("run 不该失败");
    (llm, out, sink.joined())
}

/// 与 `ask_cfg` 同款，但**不需要先有一个假 LLM**（P2 那几条自己起假 LLM）
fn quiet_cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;
    cfg.step.execute_plan = false;
    cfg.discover.enabled = false;
    // **基线不许依赖出厂默认值**（2026-09-27 的教训）：那一刻起出厂默认从"五个全关"
    // 翻成"五个全开"（用户拍板：不要配置，run 内直接用 LSC 算法），当天有 7 条老用例
    // 因为"默认值变了"整片变红 —— 它们要的其实是 **v1.1 基线**，不是"默认值"。
    // 现在把五个开关**逐条钉在这里**：要哪一条，用例自己打开。
    cfg.agent.ctx.layout = false;
    cfg.agent.ctx.metrics = false;
    cfg.agent.ctx.dedup = false;
    cfg.agent.ctx.capsule = false;
    cfg.agent.ctx.schedule = false;
    cfg
}

/// 一条读动作的账本键（测试里手搓用）
fn ledger_read(path: &str) -> keys::LedgerCall {
    keys::classify(&Action::Read(ReadSpec {
        path: path.to_string(),
        offset: None,
        limit: None,
    }))
    .expect("read 永远纯")
}

// ============================================================================
// v1.2 P4：重基线调度（`agent.ctx.schedule`）
// ============================================================================

fn sched_script() -> Vec<String> {
    let mut v: Vec<String> = (1..=5)
        .map(|i| format!(r#"{{"tool":"read","args":{{"path":"f{i}.txt"}}}}"#))
        .collect();
    v.push(r#"{"final":"读完了"}"#.to_string());
    v
}
