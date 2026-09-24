//! 配置：`AppConfig` 是引擎的全部可调参数，**由调用方注入**。
//!
//! 这里**没有配置文件入口** —— 融合前引擎自己读 `%APPDATA%\darkhorse-harness\config.toml`，
//! 那正是"两份真相源"的病根：IDE 里用户改的是 ruyix 配置，引擎却读另一个文件，
//! 于是同一个键要配两次、日志脱敏还拿错了钥匙（详见 `schema()` 上方的说明）。
//! IDE 场景下配置的唯一来源是 ruyix 的三作用域配置（`src-tauri/src/agent/config_bridge.rs`）。
//!
//! 环境变量覆盖保留（`DEEPSEEK_*`）：脚本 / CI 里塞 key 就能跑，不用改 GUI 配置。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn d_base_url() -> String {
    "https://api.deepseek.com".to_string()
}
fn d_model() -> String {
    "deepseek-v4-pro".to_string()
}
fn d_temperature() -> f32 {
    0.2
}
fn d_max_tokens() -> u32 {
    8192
}
fn d_llm_timeout() -> u64 {
    300
}
fn d_web_search() -> String {
    "auto".to_string()
}
fn d_bin(name: &str) -> String {
    name.to_string()
}
fn d_cmd_timeout() -> u64 {
    120
}
fn d_test_timeout() -> u64 {
    300
}
/// 运行目录的默认值 = **空**。
///
/// 引擎**没有自己的家**：宿主（IDE）在 `config_bridge` 里注入 `<便携根>/global/runs`。
/// 旧默认值是融合前的老血统（`~/.darkhorse/...`）—— 那等于绕过 IDE 往用户主目录写，
/// 与"删文件夹即净"直接冲突（v1.0.0 R9 / A7 就是钉这一条）。
fn d_workspace() -> String {
    String::new()
}
fn d_max_context() -> usize {
    24_000
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LlmConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "d_base_url")]
    pub base_url: String,
    #[serde(default = "d_model")]
    pub model: String,
    #[serde(default = "d_temperature")]
    pub temperature: f32,
    #[serde(default = "d_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "d_llm_timeout")]
    pub timeout_secs: u64,
    /// 服务端联网搜索：`off` / `auto` / `on`（见 `llm::web_search_on`）。
    ///
    /// 联网搜索是**服务端能力**：请求里带上 `tools:[{"type":"web_search"}]`，由服务端
    /// 自己检索、把结果灌进上下文，引擎看不到标题与链接（只在响应里拿得到查询词）。
    /// 它**只在 `/responses` 端点上成立** —— 往 `/chat/completions` 塞会被拒
    /// （实测 422 `unknown variant \`web_search\`, expected \`function\``）。
    /// 而且它是**逐模型**的（见 `llm::model_caps`）：`auto` 档要端点与模型能力
    /// 两个条件都满足才开。
    #[serde(default = "d_web_search")]
    pub web_search: String,
    /// 接口协议格式：`openai`（默认，兼容 DeepSeek / OpenAI / 大多数网关）
    /// 或 `anthropic`（Anthropic Messages 协议，`/v1/messages` + `x-api-key`）。
    ///
    /// 故障切换时主用与备用**各自带自己的格式** —— 允许"DeepSeek(OpenAI 格式) 挂了
    /// 切到 Claude(anthropic 格式)"这种异构组合。`chat()` 据此分支建请求与解析响应。
    #[serde(default = "d_api_format")]
    pub api_format: String,
    /// **标准工具协议**（v0.0.6，默认开）：请求里声明 `tools`，让模型把动作发进
    /// `tool_calls`，而不是写在 content 的 JSON 里。
    ///
    /// 为什么必须开：引擎原先从不声明 `tools`，而 DeepSeek 这类模型被**原生工具调用语法**
    /// 训练过 —— 它把调用写成 DSML 标记吐进 content，服务端没收到 `tools` 就不会解析进
    /// `tool_calls`，于是"明明算对了的命令"变成一整轮作废（实测真跑 **5/16 轮**）。
    /// 4 臂 × 8 轮对照：声明 `tools` 的两臂**零泄露、6/8 走标准 `tool_calls`**，且与
    /// `response_format=json_object` 不冲突（见 `doc/v0.x/问题-DSML标记泄露.md`）。
    ///
    /// 关掉它 = 逐字回到老行为（一行回滚），代价是泄露率与"白烧一轮"一起回来。
    /// 只覆盖 `/chat/completions`：`/responses` 与 anthropic 两条路的工具形态不同，
    /// 这次不映射（详见 `llm::extract_responses` / `extract_anthropic` 里的说明）。
    #[serde(default = "d_tool_protocol")]
    pub tool_protocol: bool,
}

fn d_tool_protocol() -> bool {
    true
}

fn d_api_format() -> String {
    "openai".to_string()
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: d_base_url(),
            model: d_model(),
            temperature: d_temperature(),
            max_tokens: d_max_tokens(),
            timeout_secs: d_llm_timeout(),
            web_search: d_web_search(),
            api_format: d_api_format(),
            tool_protocol: d_tool_protocol(),
        }
    }
}

fn d_sb_mode() -> String {
    "require".into()
}
fn d_sb_engine() -> String {
    "docker".into()
}
fn d_sb_image() -> String {
    "python:3.12-slim".into()
}
fn d_sb_network() -> String {
    "none".into()
}
fn d_sb_memory() -> String {
    "1g".into()
}
fn d_sb_cpus() -> String {
    "1.0".into()
}
fn d_sb_pids() -> u32 {
    256
}
fn d_sb_tmpfs() -> String {
    "128m".into()
}
fn d_sb_user() -> String {
    "1000:1000".into()
}
fn d_sb_timeout() -> u64 {
    300
}
fn d_sb_probe_timeout() -> u64 {
    20
}

/// 运行时隔离（Docker 沙箱）。
///
/// `mode` 是安全策略的核心：
/// - `require`（默认）：沙箱不可用就**拒绝执行**，绝不悄悄退回宿主机；
/// - `prefer`：不可用时降级到宿主机，但结果会显式标记为"未隔离"；
/// - `off`：明确知道自己在宿主机跑（知情选择）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SandboxConfig {
    #[serde(default = "d_sb_mode")]
    pub mode: String,
    #[serde(default = "d_sb_engine")]
    pub engine: String,
    #[serde(default = "d_sb_image")]
    pub image: String,
    /// `none` = 容器无网络（默认）。只有确实需要装依赖时才该改。
    #[serde(default = "d_sb_network")]
    pub network: String,
    #[serde(default = "d_sb_memory")]
    pub memory: String,
    #[serde(default = "d_sb_cpus")]
    pub cpus: String,
    /// 防 fork bomb：PID 上限
    #[serde(default = "d_sb_pids")]
    pub pids_limit: u32,
    #[serde(default = "d_sb_tmpfs")]
    pub tmpfs_size: String,
    #[serde(default = "d_sb_user")]
    pub user: String,
    #[serde(default = "d_sb_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "d_sb_probe_timeout")]
    pub probe_timeout_secs: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: d_sb_mode(),
            engine: d_sb_engine(),
            image: d_sb_image(),
            network: d_sb_network(),
            memory: d_sb_memory(),
            cpus: d_sb_cpus(),
            pids_limit: d_sb_pids(),
            tmpfs_size: d_sb_tmpfs(),
            user: d_sb_user(),
            timeout_secs: d_sb_timeout(),
            probe_timeout_secs: d_sb_probe_timeout(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VerifyConfig {
    #[serde(default = "d_python")]
    pub python_bin: String,
    #[serde(default = "d_node")]
    pub node_bin: String,
    #[serde(default = "d_cargo")]
    pub cargo_bin: String,
    #[serde(default = "d_go")]
    pub go_bin: String,
    #[serde(default = "d_cmd_timeout")]
    pub cmd_timeout_secs: u64,
    #[serde(default = "d_test_timeout")]
    pub test_timeout_secs: u64,
}

fn d_max_full_attempts() -> u32 {
    3
}
fn d_max_reflect_rounds() -> u32 {
    2
}
fn d_staged_timeout() -> u64 {
    60
}
fn d_reflect_max_steps() -> usize {
    8
}

/// Agent 循环的质量门禁（v0.3）：机械验证。
///
/// 触发是**事实驱动**的（这一轮有没有改动），不是任务分类 —— 分类是预测，
/// 会误判；"有没有写文件"不会。见 `doc/v0.x/需求-Agent-验证与反思-v0.3.md`。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GateConfig {
    /// 改动后跑语法层（窄验证）
    #[serde(default = "d_true")]
    pub narrow: bool,
    /// 交付（final）前跑全量验证：语法 + 单测 + 规约
    #[serde(default = "d_true")]
    pub full: bool,
    /// 全量验证连续失败多少次后放弃拦截（防止"验证不过就无限修"）
    #[serde(default = "d_max_full_attempts")]
    pub max_full_attempts: u32,
    /// 暂存内容语法检查的超时（确认模式；全量验证的超时在 `verify.*`）
    #[serde(default = "d_staged_timeout")]
    pub staged_timeout_secs: u64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            narrow: true,
            full: true,
            max_full_attempts: d_max_full_attempts(),
            staged_timeout_secs: d_staged_timeout(),
        }
    }
}

/// 反思（复核 agent）配置。复核必须用**干净上下文**，模型可单独指定。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ReflectConfig {
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// 复核发现问题后最多回灌主循环几轮
    #[serde(default = "d_max_reflect_rounds")]
    pub max_rounds: u32,
    /// 复核 agent 自己的工具轮次上限（只允许 read）
    #[serde(default = "d_reflect_max_steps")]
    pub max_steps: usize,
    /// 复核用哪个模型（空 = 与主循环同模型）
    #[serde(default)]
    pub model: String,
}

impl Default for ReflectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_rounds: d_max_reflect_rounds(),
            max_steps: d_reflect_max_steps(),
            model: String::new(),
        }
    }
}

/// 计划步骤的执行体（子 agent，v0.4）。
///
/// 模型调 `plan` 之后，引擎按序把每个步骤派发给 [`crate::step_agent::run_step`]：
/// 它开自己的 messages（只有 `STEP_SYSTEM` + 本步输入包），读写走主循环**同一份**
/// `Ctx`（覆盖层不分裂），做完交回一行引擎写的事实。父循环的 `MAX_STEPS` 因此
/// 变成"最多多少个步骤/干预轮"——步骤内部的轮次不再计入父预算，这是它独立的预算。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StepAgentConfig {
    /// 单个步骤内部自己的工具轮次上限（与主循环的 `MAX_STEPS` 相互独立；默认 96，
    /// 与父预算对齐的理由见 `d_step_max_steps`）
    #[serde(default = "d_step_max_steps")]
    pub max_steps: usize,
    /// **计划即执行**：模型调 `plan` 之后由引擎按序把每个步骤派发给步骤执行体。
    /// 打开后父循环的一轮 = 一个步骤（或一次失败后的干预轮），`MAX_STEPS` 的含义随之从
    /// "最多多少次模型调用"变成"最多多少个步骤/干预轮"——步骤内部的轮次不再计入父预算。
    ///
    /// 默认**开**（v0.4 起）。原先默认关是为了留一条"关掉即逐字节回到旧行为"的退路；
    /// 但关着的时候大纲的进度只能靠**推断**（拿模型事前列的 `files` 对账），
    /// 实测会大面积误判 —— run `agent-20260920-142405` 8 步判出 6 个"跳过"，
    /// 其中 4 步其实写了一半以上、还有一步声明的文件本来就存在。
    /// **引擎自己知道每步跑没跑完**，这个事实比任何推断都准，所以让它是默认路径；
    /// 退路仍然保留：配 `false` 即完全回到旧行为。
    #[serde(default = "d_step_execute_plan")]
    pub execute_plan: bool,
}

impl Default for StepAgentConfig {
    fn default() -> Self {
        Self {
            max_steps: d_step_max_steps(),
            execute_plan: d_step_execute_plan(),
        }
    }
}

/// 子步骤自己的轮次上限：**96**，与父循环 [`crate::agent::MAX_STEPS`] 对齐（v0.10，原为 24）。
///
/// 24 是 v0.4 定下的值，当时没人算过一笔账。子步执行体写的是**整文件**（引擎没有外科手术式
/// `edit`，Write 就是交出全文），而"改一个页面"这种步骤光前期 `read` 就可能要 3~4 个文件
/// （现有页面 / 路由 / 样式 / 入口），再叠加写完触发窄层语法检查失败后的重写 ——
/// 24 轮会在"本步还没交付"时就用尽，而**父预算 `MAX_STEPS=96` 还剩一大半**。
/// 子预算比父预算更紧，唯一的后果是把一次 run 拆成"失败 → 干预轮 →（可能重规划）"：
/// 步骤没做完这个事实不变，父预算反而多烧一轮。
///
/// 放开之后的上界不靠轮数兜底，靠 `ruyix.code.harness.agent.max_elapsed_secs`
/// （默认 1800s）那道**墙钟**闸 —— 主循环与子步骤都看它，所以真卡住的步骤不会因为
/// 轮数变多而无限跑下去。
fn d_step_max_steps() -> usize {
    96
}

fn d_step_execute_plan() -> bool {
    true
}

/// 工具循环的整体预算（v0.4）。
///
/// 子步骤执行体（[`crate::step_agent`]）能自己跑循环之后，没有**全局**闸它就是个成本
/// 放大器：`MAX_STEPS` 管的是父循环的轮数，而每个父轮底下可能挂着几十轮子调用。
/// 这道闸按墙钟时间收口，主循环与子步骤**都**看它。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AgentConfig {
    /// 单次 run 的总时长上限（秒）。**0 = 不限**。
    /// 到时立刻收手并把原因写进答复 —— 宁可"这次没做完"，也不要让一个 run 无声烧下去。
    #[serde(default = "d_agent_max_elapsed_secs")]
    pub max_elapsed_secs: u64,
    /// 每轮允许模型一次发**一批**互不依赖的调用（`{"actions":[…]}`）。
    /// 关掉即回到"一轮一个调用"的老协议（一行回滚；提示词也不再教这个形状）。
    #[serde(default = "d_agent_batch")]
    pub batch: bool,
    /// 单批最多几个调用。超了不静默截断（那会丢调用），而是把上限告诉模型让它拆批。
    #[serde(default = "d_agent_batch_max")]
    pub batch_max: usize,
    /// 一批里**连续的只读调用**是否并发执行。关掉仍是一批一个上下文往返，
    /// 只是读文件改回排队（用于排查"并发读"本身的问题）。
    #[serde(default = "d_agent_batch_parallel")]
    pub batch_parallel: bool,
    /// 工具循环的**历史折叠**：把"超出保留窗口"的老轮次压成一行事实
    /// （调了什么、成没成、结果多大），不再每轮重发它们的正文。
    ///
    /// 关掉即回到"历史只增不裁"的老行为 —— 排查"模型好像忘了自己读过什么"时的一行回滚。
    #[serde(default = "d_true")]
    pub history_trim: bool,
    /// 最近几轮的正文**不动**（更老的才折叠）。数字越大越安全、上下文越贵。
    #[serde(default = "d_agent_history_keep_rounds")]
    pub history_keep_rounds: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_elapsed_secs: d_agent_max_elapsed_secs(),
            batch: d_agent_batch(),
            batch_max: d_agent_batch_max(),
            batch_parallel: d_agent_batch_parallel(),
            history_trim: d_true(),
            history_keep_rounds: d_agent_history_keep_rounds(),
        }
    }
}

fn d_agent_max_elapsed_secs() -> u64 {
    1800
}

fn d_agent_batch() -> bool {
    true
}

/// 上限取 8：一次读 5~8 个文件的场景最常见，再多属于"该先规划"而不是"该并发读"。
/// 它同时是提示词里给出的数字，模型据此拆批。
fn d_agent_batch_max() -> usize {
    8
}

fn d_agent_batch_parallel() -> bool {
    true
}

/// 保留窗口 6 轮：一次"读 → 改 → 跑测试 → 再改"大约 4~6 轮，窗口比它小就会把模型
/// 刚看过的现场折掉（它只能重读一遍，反而更贵）。
fn d_agent_history_keep_rounds() -> usize {
    6
}

fn d_python() -> String {
    // Windows 惯例是 `python`；macOS/Linux 系统自带且普遍在 PATH 上的是 `python3`
    if cfg!(target_os = "windows") {
        d_bin("python")
    } else {
        d_bin("python3")
    }
}
fn d_node() -> String {
    d_bin("node")
}
fn d_cargo() -> String {
    d_bin("cargo")
}
fn d_go() -> String {
    d_bin("go")
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            python_bin: d_python(),
            node_bin: d_node(),
            cargo_bin: d_cargo(),
            go_bin: d_go(),
            cmd_timeout_secs: d_cmd_timeout(),
            test_timeout_secs: d_test_timeout(),
        }
    }
}

fn d_true() -> bool {
    true
}
fn d_zero_i32() -> i32 {
    0
}
/// 自我纠正循环的默认轮数上限：验证失败 → stderr 回灌 → 补丁 → 重验。
/// 5 是平衡点：太少救不回缺文件/依赖缺失的 run，太多在顽固问题上烧 token。
fn d_max_repair_rounds() -> u32 {
    5
}
fn d_prompt_budget() -> usize {
    12_000
}

/// lint 子系统的默认安装位置。
///
/// 编译期从 `CARGO_MANIFEST_DIR`（即 ruyix 仓的 `<repo>/crates/harness-engine`）
/// 往上两级推出 `<repo>/tools/lint`（融合计划 D2 落位布局；原 harness 仓为
/// `<repo>/src-tauri` 往上一级）。运行时由 ruyix 的 config_bridge 显式注入，
/// 这个默认值兜底引擎单独构建/测试的场景；换机器/换目录时用
/// `HARNESS_LINT_DIR` 环境变量或配置项覆盖。
fn d_lint_dir() -> String {
    if let Ok(v) = std::env::var("HARNESS_LINT_DIR")
        && !v.trim().is_empty()
    {
        return v.trim().to_string();
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("tools").join("lint"))
        .unwrap_or_else(|| std::path::PathBuf::from("tools/lint"))
        .to_string_lossy()
        .to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LintConfig {
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// harness_lint 包所在目录（作为 PYTHONPATH 注入）
    #[serde(default = "d_lint_dir")]
    pub package_dir: String,
    /// 允许生效的豁免数上限。默认 0：**不给"加 noqa 糊过去"留口子**
    #[serde(default = "d_zero_i32")]
    pub max_suppressions: i32,
    /// warning 是否也算失败
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// 自我纠正循环的最大轮数
    #[serde(default = "d_max_repair_rounds")]
    pub max_repair_rounds: u32,
    /// 诊断包（回灌给模型）的字符预算
    #[serde(default = "d_prompt_budget")]
    pub prompt_budget_chars: usize,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            enabled: d_true(),
            package_dir: d_lint_dir(),
            max_suppressions: d_zero_i32(),
            strict: false,
            extra_args: Vec::new(),
            max_repair_rounds: d_max_repair_rounds(),
            prompt_budget_chars: d_prompt_budget(),
        }
    }
}

fn d_entropy_window() -> u32 {
    3
}
fn d_score_drop() -> f64 {
    3.0
}
fn d_density_up() -> f64 {
    15.0
}
fn d_new_supp() -> i32 {
    2
}
fn d_max_fix() -> usize {
    5
}
fn d_max_files() -> usize {
    3
}
fn d_max_prs() -> usize {
    3
}
fn d_repair_attempts() -> u32 {
    2
}
fn d_branch_prefix() -> String {
    "entropy/".into()
}
fn d_remote() -> String {
    "origin".into()
}

/// 熵管理（跨时间的闭环）配置。
///
/// 阈值放在这里、不放在测量层，是刻意的分层：测量（`harness_lint --entropy`）只负责
/// "测得准"，"涨到多少算退化"这种策略全部集中在控制侧 —— 改策略不用动测量代码。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EntropyConfig {
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// 历史文件（空 = `<repo>/.harness-entropy/history.jsonl`）
    #[serde(default)]
    pub history: String,
    /// 参考基线取最近 N 条的中位数（抗单周噪声）
    #[serde(default = "d_entropy_window")]
    pub window: u32,
    /// 质量分下降达到该值 → 退化
    #[serde(default = "d_score_drop")]
    pub score_drop: f64,
    /// 违规密度上涨达到该百分比 → 退化
    #[serde(default = "d_density_up")]
    pub density_up_pct: f64,
    /// 新增豁免达到该条数 → 退化
    #[serde(default = "d_new_supp")]
    pub new_suppressions: i32,
    /// 一次最多修几条违规（**有界**是定时任务不失控的前提）
    #[serde(default = "d_max_fix")]
    pub max_fix_targets: usize,
    /// 一次最多改几个文件
    #[serde(default = "d_max_files")]
    pub max_changed_files: usize,
    /// 已开着的待审熵 PR 达到该数就跳过本轮（防止把机制刷成噪声）
    #[serde(default = "d_max_prs")]
    pub max_open_prs: usize,
    /// 修复尝试轮数
    #[serde(default = "d_repair_attempts")]
    pub max_repair_attempts: u32,
    #[serde(default = "d_branch_prefix")]
    pub branch_prefix: String,
    #[serde(default = "d_remote")]
    pub remote: String,
    /// 是否真的创建 PR（false = 只产出分支与 PR 正文）
    #[serde(default = "d_true")]
    pub create_pr: bool,
    /// 是否保留 worktree（调试用）
    #[serde(default)]
    pub keep_worktree: bool,
}

impl Default for EntropyConfig {
    fn default() -> Self {
        Self {
            enabled: d_true(),
            history: String::new(),
            window: d_entropy_window(),
            score_drop: d_score_drop(),
            density_up_pct: d_density_up(),
            new_suppressions: d_new_supp(),
            max_fix_targets: d_max_fix(),
            max_changed_files: d_max_files(),
            max_open_prs: d_max_prs(),
            max_repair_attempts: d_repair_attempts(),
            branch_prefix: d_branch_prefix(),
            remote: d_remote(),
            create_pr: d_true(),
            keep_worktree: false,
        }
    }
}

fn d_kb_top_k() -> usize {
    4
}
fn d_kb_budget() -> usize {
    1200
}
fn d_kb_per_source() -> usize {
    1
}
fn d_kb_min_score() -> f64 {
    0.35
}
fn d_kb_chunk_chars() -> usize {
    1200
}
fn d_kb_max_file_bytes() -> u64 {
    256 * 1024
}
fn d_kb_diversity() -> f64 {
    0.75
}

/// 本地知识库配置（v0.6）。
///
/// 默认 `enabled = false`：**不改变现有行为**，添加了知识库才启用。
/// 预算放在这里而不是散在代码里，理由与熵管理的阈值一样 ——
/// 「注入多少算合适」是策略，策略要能一处改、能被 trace 观察到。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct KbConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 追加来源（GUI 里加的来源落在 `kb.json`，这里给脚本化/无 GUI 的场景）
    #[serde(default)]
    pub roots: Vec<String>,
    /// 知识库根目录（空 = 宿主没注入 → 落临时目录）
    #[serde(default)]
    pub dir: String,
    #[serde(default = "d_kb_top_k")]
    pub top_k: usize,
    /// 注入预算（按字符近似 token）—— 一段 3000 字的文档可能一条就吃光它
    #[serde(default = "d_kb_budget")]
    pub token_budget: usize,
    /// 同一来源文件最多几个 chunk（近重复聚集的第一道闸）
    #[serde(default = "d_kb_per_source")]
    pub per_source_limit: usize,
    /// 低于此分**不注入**：宁可不给，也别给噪声
    #[serde(default = "d_kb_min_score")]
    pub min_score: f64,
    /// chunk 目标长度（字符）
    #[serde(default = "d_kb_chunk_chars")]
    pub chunk_chars: usize,
    /// 单文件索引上限：超过就跳过并记进 stats（大二进制/日志不进库）
    #[serde(default = "d_kb_max_file_bytes")]
    pub max_file_bytes: u64,
    /// 跨来源 MMR 的相似度阈值：超过它就算"近重复"，不再吃预算
    #[serde(default = "d_kb_diversity")]
    pub diversity_max_sim: f64,
}

impl Default for KbConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            roots: Vec::new(),
            dir: String::new(),
            top_k: d_kb_top_k(),
            token_budget: d_kb_budget(),
            per_source_limit: d_kb_per_source(),
            min_score: d_kb_min_score(),
            chunk_chars: d_kb_chunk_chars(),
            max_file_bytes: d_kb_max_file_bytes(),
            diversity_max_sim: d_kb_diversity(),
        }
    }
}

fn d_discover_enabled() -> bool {
    true
}
fn d_discover_ttl() -> u64 {
    300
}
fn d_discover_timeout() -> u64 {
    10
}

/// 命令发现配置（v0.5）。
///
/// 默认**开**：实测 run `agent-20260920-152312` 有 5~6 轮纯粹在试探 `mvn` / `java`
/// 在不在，而引擎早就探过、只是没告诉模型。默认关掉等于把这个空转留着。
///
/// 探测结果**只进上下文**，不改任何判断 —— 所以开开关关都不影响正确性，
/// 只影响模型是"知道"还是"去试"。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DiscoverConfig {
    /// 关掉就不探测（上下文里也不会出现命令段）
    #[serde(default = "d_discover_enabled")]
    pub enabled: bool,
    /// 探测结果缓存多久（秒）。0 = 每次都重新探（换工具链后想立刻生效就用它）
    #[serde(default = "d_discover_ttl")]
    pub ttl_secs: u64,
    /// 单条探测的超时（秒）—— 探测不许把一轮对话卡死
    #[serde(default = "d_discover_timeout")]
    pub timeout_secs: u64,
    /// 追加要探的二进制名（用 `--version`）。表没覆盖的工具走这里，
    /// **不用改代码** —— 这是「能力长在数据里」的落点之一。
    #[serde(default)]
    pub extra: Vec<String>,
}

impl Default for DiscoverConfig {
    fn default() -> Self {
        Self {
            enabled: d_discover_enabled(),
            ttl_secs: d_discover_ttl(),
            timeout_secs: d_discover_timeout(),
            extra: Vec::new(),
        }
    }
}

fn d_env_install_enabled() -> bool {
    true
}

/// 环境准备配置（v0.5）。
///
/// 命令发现（[`DiscoverConfig`]）只解决"模型**知道**缺什么"，补不上。补的能力走 Connect：
/// 宿主在清单里摆一个 `kind = "env"` 的目标，模型 `connect` 请求安装，宿主编排包管理器。
/// 引擎**不碰** choco / apt / brew，只把这个开关透给宿主 —— 关掉后清单里不再出现 env 目标，
/// 模型看不到、也就不会请求安装（引擎侧无需再判一次）。
///
/// 默认**开**：这是自成长闭环的最后一环（探测 → 告知 → 请求 → 安装 → 再探测），
/// 默认关掉等于把闭环断在最后一步。装什么、用哪个包管理器、装不装，全是**宿主裁量**。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EnvConfig {
    /// 关掉后宿主不再把这些能力摆进 connect 清单（模型侧彻底看不见）
    #[serde(default = "d_env_install_enabled")]
    pub install_enabled: bool,
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            install_enabled: d_env_install_enabled(),
        }
    }
}

fn d_proc_enabled() -> bool {
    true
}

fn d_proc_max() -> usize {
    4
}

fn d_proc_ready_timeout() -> u64 {
    60
}

/// 向委托人提问（`ask_user`，v0.8）的配置面。
///
/// 需求歧义（"做一个远程登录功能" —— 登哪台机器？）的答案**不在环境里**：read 读磁盘、
/// execute 跑命令、connect 连机器，三者都只会从"环境"取答案。所以第五个动作是"问人"，
/// 而它的三条纪律都落在这一组开关上：能不能问、等多久算没人回答、一次 run 最多问几次。
///
/// 默认**开**：关掉等于让模型回到"猜"—— 猜错的代价常常是整体返工，而不是多花几轮。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AskConfig {
    /// 关掉后 `ask_user` 一律被拒（提示词也一字不提：不虚报能力）
    #[serde(default = "d_ask_enabled")]
    pub enabled: bool,
    /// 等多久算"没人回答"（秒），0 = 无限等。超时一律 fail-closed：
    /// 引擎会拒绝依赖这个答案的动作，**绝不假设同意**。
    #[serde(default = "d_ask_timeout")]
    pub timeout_secs: u64,
    /// 一次 run 最多问几次（`ask` 是稀缺资源：问多了说明该交付并声明假设）
    #[serde(default = "d_ask_max")]
    pub max_per_run: u32,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            enabled: d_ask_enabled(),
            timeout_secs: d_ask_timeout(),
            max_per_run: d_ask_max(),
        }
    }
}

fn d_ask_enabled() -> bool {
    true
}

fn d_ask_timeout() -> u64 {
    300
}

fn d_ask_max() -> u32 {
    4
}

/// 托管进程（永不退出的服务）配置（v0.6）。
///
/// `execute` 原来是"跑完为止"，对 `mvn spring-boot:run` / `java -jar` 这类**永不退出**的服务
/// 是错的：模型没有表达"常驻"的词汇，只能靠 `start` / `Start-Process` / 往 %TEMP% 写 bat 去绕，
/// 绕出来的进程还脱离了引擎的进程树（输出拿不到、日志路径漂、杀都杀不到）。这一组开关给
/// "常驻"一个正当出口，是 [`crate::proc`] 的配置面。
///
/// 默认**开**：关掉等于把模型推回歪招 —— 实测 cloud-shop-admin 因此空转了 17 轮，而服务其实
/// 早就起来了（日志里写着 `Started AdminApplication`，进程还占着 8083 端口）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProcConfig {
    /// 关掉后后台启动一律被拒（回一条说明，不执行）
    #[serde(default = "d_proc_enabled")]
    pub enabled: bool,
    /// 同时托管的进程数上限（含 `keep_alive` 留下的），1~16
    #[serde(default = "d_proc_max")]
    pub max: usize,
    /// 没显式给 `ready_timeout_secs` 时的默认就绪窗口（秒），1~600
    #[serde(default = "d_proc_ready_timeout")]
    pub ready_timeout_secs: u64,
}

impl Default for ProcConfig {
    fn default() -> Self {
        Self {
            enabled: d_proc_enabled(),
            max: d_proc_max(),
            ready_timeout_secs: d_proc_ready_timeout(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppConfig {
    #[serde(default)]
    pub llm: LlmConfig,
    /// 备用 LLM（故障切换网关）。`None` = 不配置备用，行为等同旧版（主用不可用时直接报错）。
    ///
    /// 主用不可用时（网络 / 5xx / 429 / 超时，见 `llm::is_switchable_error`）`chat()` 自动
    /// 切到它。主用是鉴权/配额/参数错误时不切换 —— 那些重试也没用。
    /// 备用可以**是** anthropic 协议（异构切换），也可以和主用同协议。
    #[serde(default)]
    pub llm_fallback: Option<LlmConfig>,
    #[serde(default)]
    pub verify: VerifyConfig,
    /// 工具循环的质量门禁：机械验证（v0.3）
    #[serde(default)]
    pub gate: GateConfig,
    /// 工具循环的反思（复核 agent，v0.3）
    #[serde(default)]
    pub reflect: ReflectConfig,
    /// 工具循环的整体预算（v0.4：总时长闸）
    #[serde(default)]
    pub agent: AgentConfig,
    /// 计划步骤的执行体（子 agent，v0.4）
    #[serde(default)]
    pub step: StepAgentConfig,
    #[serde(default)]
    pub lint: LintConfig,
    #[serde(default)]
    pub entropy: EntropyConfig,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub kb: KbConfig,
    /// 命令发现（把"本机有什么命令"实测出来喂进模型上下文，v0.5）
    #[serde(default)]
    pub discover: DiscoverConfig,
    /// 环境准备（缺失工具的按需安装，走 Connect，v0.5）
    #[serde(default)]
    pub env: EnvConfig,
    /// 托管进程（永不退出的服务：后台启动 + 就绪判据 + 句柄，v0.6）
    #[serde(default)]
    pub proc: ProcConfig,
    /// 向委托人提问（需求歧义只能问人，v0.8）
    #[serde(default)]
    pub ask: AskConfig,
    #[serde(default = "d_workspace")]
    pub workspace_root: String,
    #[serde(default = "d_max_context")]
    pub max_context_chars: usize,
}

impl Default for AppConfig {
    /// 手写而不是 derive：`#[serde(default = "...")]` 只在反序列化时生效，
    /// derive 出来的 Default 会把 workspace_root 变成空串（被单测抓到过）。
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            llm_fallback: None,
            verify: VerifyConfig::default(),
            gate: GateConfig::default(),
            reflect: ReflectConfig::default(),
            agent: AgentConfig::default(),
            step: StepAgentConfig::default(),
            lint: LintConfig::default(),
            entropy: EntropyConfig::default(),
            sandbox: SandboxConfig::default(),
            kb: KbConfig::default(),
            discover: DiscoverConfig::default(),
            env: EnvConfig::default(),
            proc: ProcConfig::default(),
            ask: AskConfig::default(),
            workspace_root: d_workspace(),
            max_context_chars: d_max_context(),
        }
    }
}

/// 知识库根目录：注册表 `kb.json` 与每个来源的索引库都放这里。
///
/// 与 `HARNESS_LINT_DIR` 同一套做法：允许环境变量覆盖（**测试靠它隔离**）。
/// 宿主没注入时的兜底落临时目录 —— 引擎不再探测 `%APPDATA%` / 家目录（v1.0.0 R9）。
pub fn kb_dir(cfg: &KbConfig) -> PathBuf {
    if let Ok(v) = std::env::var("HARNESS_KB_DIR")
        && !v.trim().is_empty()
    {
        return PathBuf::from(v.trim());
    }
    let raw = cfg.dir.trim();
    if !raw.is_empty() {
        return PathBuf::from(raw);
    }
    std::env::temp_dir().join("ruyix").join("kb")
}

/// 运行目录（agent 沙箱产物）。三种形态，**默认不再摸用户主目录**：
///
/// - 空（默认）→ `%TEMP%/ruyix/runs`：宿主没注入时的兜底，引擎单独跑（单测 / eval）够用，
///   而且落临时目录、跑完不留东西；
/// - `~/...` → 展开到用户家目录：**这是用户自己写的**，照做（全仓唯一展开 `~` 的地方）；
/// - 相对路径 → 同样落临时根。绝不静默落到家目录 —— "悄悄写到别处"正是这一版要消灭的行为。
pub fn runs_root(cfg: &AppConfig) -> PathBuf {
    let raw = cfg.workspace_root.trim();
    if raw.is_empty() {
        return fallback_runs_root();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return expand_home(rest);
    }
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        fallback_runs_root().join(p)
    }
}

/// 宿主没注入（或只给了相对路径）时的兜底：**临时目录**。
fn fallback_runs_root() -> PathBuf {
    std::env::temp_dir().join("ruyix").join("runs")
}

/// `~/` 的展开（唯一实现）。读环境变量而不是引入 `dirs`：引擎不该有能力"自己找系统位置"，
/// 那条能力只有宿主的 `paths.rs` 该有（宿主侧由静态门禁钉住）。
fn expand_home(rest: &str) -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(rest)
}

/// 环境变量优先于配置：方便脚本化调用 / 不把 key 落盘。
pub fn apply_env_overrides(cfg: &mut AppConfig) {
    if let Ok(v) = std::env::var("DEEPSEEK_API_KEY")
        && !v.trim().is_empty()
    {
        cfg.llm.api_key = v.trim().to_string();
    }
    if let Ok(v) = std::env::var("DEEPSEEK_BASE_URL")
        && !v.trim().is_empty()
    {
        cfg.llm.base_url = v.trim().trim_end_matches('/').to_string();
    }
    if let Ok(v) = std::env::var("DEEPSEEK_MODEL")
        && !v.trim().is_empty()
    {
        cfg.llm.model = v.trim().to_string();
    }
}

// ============================================
// 配置形态：由调用方注入 + 引擎自描述
// ============================================
//
// 这里**没有配置文件入口**。融合前引擎自己读写 `%APPDATA%\darkhorse-harness\config.toml`，
// 那就是"第二真相源"：IDE 里用户改的是 ruyix 配置，引擎读的是另一个文件。
// 实测代价有两笔 —— 同一个键要配两次；`workspace::save` 做日志脱敏时
// 从那个文件取 api_key，而 IDE 跑的是 `~/.ruyix/code/ai.toml` 里的 key，
// 等于**拿错了钥匙、密钥根本没被脱敏**（已随本次改造修掉）。
//
// 现在配置一律由调用方构造（IDE 见 `src-tauri/src/agent/config_bridge.rs`），
// 引擎只提供两样东西：
//   1. `schema()` —— "我有哪些键、什么类型、默认多少"，供宿主桥接与配置表单使用；
//   2. `apply_flat()` —— 把宿主的扁平键值对按类型灌进来（非法值保持默认）。
// 加一个字段只需加字段 + 默认值，宿主、表单、桥全部自动跟上。

/// 配置键的元信息。**路径 / 类型 / 默认值全部从 `AppConfig::default()` 推导**，
/// 不在这里手写 —— 那是"同一个键名写四遍"的老病根。
#[derive(Debug, Clone, Serialize)]
pub struct KeySpec {
    /// 相对配置树根的路径，如 `llm.temperature`（与 serde 路径同构）
    pub path: String,
    /// `bool` / `int` / `float` / `text` / `list`
    pub kind: &'static str,
    /// 默认值的字符串形态（表单用它做空值提示）
    pub default: String,
    /// 是否建议摆进 IDE 配置表单（见 `FORM_HIDDEN`）
    pub ui: bool,
    /// 取值有穷时的合法值（空 = 任意值）。见 `ENUM_KEYS`。
    pub options: Vec<String>,
}

/// 取值有穷的键（键 → 合法值）。**声明在引擎里**是因为只有引擎知道
/// `sandbox.mode` 有哪三档、写错会静默落到哪一档。宿主桥的校验与表单的下拉
/// 都从 `schema()` 读它，所以全局只有这一份。
const ENUM_KEYS: &[(&str, &[&str])] = &[
    ("sandbox.mode", &["require", "prefer", "off"]),
    // `auto` = 只对认这个参数的端点开（DeepSeek 官方），配了别的端点也不会被 422 打回。
    ("llm.web_search", &["off", "auto", "on"]),
    // `api_format` 决定 `chat()` 走哪套协议：OpenAI 兼容 还是 Anthropic Messages。
    ("llm.api_format", &["openai", "anthropic"]),
];

/// 不进配置表单的键（**前缀匹配**：写 `entropy` 就盖住整段）。
///
/// 为什么是"隐藏表"而不是"白名单"：白名单会让**新增字段默认不可见**
/// （忘了加一行 = 用户配不到），而隐藏表失效的方向只是"多露出一个键"，无害。
const FORM_HIDDEN: &[&str] = &[
    // LLM 端点 / 密钥 / 模型由宿主的 `ai` 段管（D8：配置单源），不在这里重复暴露 ——
    // 同一样东西摆两处正是这次要消灭的问题。
    "llm.base_url",
    "llm.api_key",
    "llm.model",
    // `api_format` 在宿主 `ai` / `ai_fallback` 段以 select 呈现（D8：配置单源），
    // 不在 harness 段重复出现，否则用户看到两份互相打架的协议开关。
    "llm.api_format",
    // 备用 LLM 整段由宿主 `ai_fallback` 段管（同 D8），harness 段不重复渲染。
    "llm_fallback",
    // 沙箱引擎（docker / bubblewrap）是平台实现细节，不是用户旋钮；
    // `sandbox.image` 保留可见（换镜像是真需求）。
    "sandbox.engine",
    // 熵管理（自动开 PR 那套）是实验室能力，IDE 场景用不到。
    "entropy",
];

fn is_hidden(path: &str) -> bool {
    FORM_HIDDEN
        .iter()
        .any(|h| path == *h || path.starts_with(&format!("{h}.")))
}

/// 列出全部配置键（含类型与默认值）。宿主拿它做键桥接与表单 schema，
/// 于是**不存在"引擎有键、宿主不知道"这种漂移** —— 这正是以前加一个键要改六处的成因。
pub fn schema() -> Vec<KeySpec> {
    let root = toml::Value::try_from(AppConfig::default())
        .unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let mut out = Vec::new();
    flatten(&root, String::new(), &mut out);
    out
}

fn flatten(v: &toml::Value, prefix: String, out: &mut Vec<KeySpec>) {
    match v {
        toml::Value::Table(t) => {
            for (k, child) in t {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(child, path, out);
            }
        }
        leaf => {
            let (kind, default) = leaf_of(leaf);
            out.push(KeySpec {
                ui: !is_hidden(&prefix),
                options: options_of(&prefix),
                path: prefix,
                kind,
                default,
            });
        }
    }
}

fn options_of(path: &str) -> Vec<String> {
    ENUM_KEYS
        .iter()
        .find(|(k, _)| *k == path)
        .map(|(_, v)| v.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

fn leaf_of(v: &toml::Value) -> (&'static str, String) {
    match v {
        toml::Value::Boolean(b) => ("bool", b.to_string()),
        toml::Value::Integer(i) => ("int", i.to_string()),
        toml::Value::Float(f) => ("float", f.to_string()),
        toml::Value::String(s) => ("text", s.clone()),
        toml::Value::Array(a) => (
            "list",
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(","),
        ),
        _ => ("text", String::new()),
    }
}

/// 把扁平的字符串键值对灌进配置：`[("llm.temperature", "0.5"), …]`。
///
/// 返回**被采纳的键数**。规则：
/// - 键不在 schema 里 → 忽略（拼错不该静默造出一个幽灵字段）；
/// - 值按目标位置的类型 coerce；**转不过去就丢弃该键、保持原值**
///   （用户敲错一个数字不该让整个配置或整批改动一起失效）；
/// - 空串按"未设置"处理（保持默认），与宿主"空值 = 删键"的语义一致。
///
/// 实现刻意不含任何键名：拿配置的序列化结果当"类型表"，逐个打补丁再反序列化回来，
/// 所以**加字段不用改这里**。逐键提交（而非整批）也是刻意的 —— 一个坏值不该连坐。
pub fn apply_flat(cfg: &mut AppConfig, pairs: &[(String, String)]) -> usize {
    let mut taken = 0;
    for (path, raw) in pairs {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let Ok(mut tree) = toml::Value::try_from(&*cfg) else {
            break;
        };
        let Some(slot) = lookup_mut(&mut tree, path) else {
            continue;
        };
        let Some(value) = coerce(slot, raw) else {
            continue;
        };
        *slot = value;
        match tree.try_into::<AppConfig>() {
            Ok(next) => {
                *cfg = next;
                taken += 1;
            }
            Err(_) => continue,
        }
    }
    taken
}

/// 按路径定位到树上的叶子（`a.b.c`）。中间层不存在就 None。
fn lookup_mut<'a>(tree: &'a mut toml::Value, path: &str) -> Option<&'a mut toml::Value> {
    let mut cur = tree;
    for seg in path.split('.') {
        cur = cur.as_table_mut()?.get_mut(seg)?;
    }
    Some(cur)
}

/// 读某个键当前值的字符串形态 —— **与 `apply_flat` 的输入同构，可原样回灌**。
///
/// 存在的意义是"改了要能还原"（评估实验的临时参数覆盖）。列表用逗号连接，
/// 与 `coerce` 的分隔符解析对称。
pub fn flat_value(cfg: &AppConfig, path: &str) -> Option<String> {
    let tree = toml::Value::try_from(cfg).ok()?;
    let mut cur = &tree;
    for seg in path.split('.') {
        cur = cur.as_table()?.get(seg)?;
    }
    Some(match cur {
        toml::Value::String(s) => s.clone(),
        toml::Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    })
}

/// 把字符串按**目标位置的类型**转成 toml 值；转不了返回 None（= 保持默认）。
fn coerce(slot: &toml::Value, raw: &str) -> Option<toml::Value> {
    match slot {
        // 只认明确的真假值。旧桥的写法是"值存在但不是 true/1 就当 false"，
        // 于是 `proc.enabled = "也许"` 会**静默把功能关掉** —— 现在它保持默认。
        toml::Value::Boolean(_) => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(toml::Value::Boolean(true)),
            "false" | "0" | "no" | "off" => Some(toml::Value::Boolean(false)),
            _ => None,
        },
        toml::Value::Integer(_) => raw.parse::<i64>().ok().map(toml::Value::Integer),
        toml::Value::Float(_) => raw.parse::<f64>().ok().map(toml::Value::Float),
        toml::Value::String(_) => Some(toml::Value::String(raw.to_string())),
        // 列表：逗号 / 分号 / 换行 / 空白都当分隔符 —— 手写配置的人不该被格式绊住
        toml::Value::Array(_) => Some(toml::Value::Array(
            raw.split([',', ';', '\n', '\t', ' '])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| toml::Value::String(s.to_string()))
                .collect(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_usable_without_a_config_file() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.llm.base_url, "https://api.deepseek.com");
        assert!(cfg.llm.model.starts_with("deepseek"));
        assert!(cfg.verify.test_timeout_secs > cfg.verify.cmd_timeout_secs);
        assert!(
            cfg.workspace_root.is_empty(),
            "默认必须为空 —— 宿主注入是运行目录的唯一来源"
        );
        let fallback = runs_root(&cfg);
        assert!(
            fallback.is_absolute() && fallback.starts_with(std::env::temp_dir()),
            "兜底必须落临时目录，不许碰家目录：{}",
            fallback.display()
        );
        // 计划即执行默认开：关着的时候大纲进度只能靠"声明文件是否落地"推断，
        // 实测 run agent-20260920-142405 8 步判出 6 个"跳过"（4 步其实做了一半以上）。
        // 引擎自己知道每步的终态，那才该是默认路径。这条断言就是"默认值是它"的凭据。
        assert!(cfg.step.execute_plan, "execute_plan 默认必须开");
        // 用户显式写 false 仍要能关回去（退路）。
        let off: AppConfig = toml::from_str("[step]\nexecute_plan = false\n").unwrap();
        assert!(!off.step.execute_plan, "配 false 必须能回到旧行为");
    }

    #[test]
    fn partial_toml_fills_missing_fields() {
        // 用户只写了 api_key，其它字段必须落回默认值而不是报错
        let cfg: AppConfig = toml::from_str("[llm]\napi_key = \"sk-x\"\n").unwrap();
        assert_eq!(cfg.llm.api_key, "sk-x");
        assert_eq!(cfg.llm.model, "deepseek-v4-pro");
        // python 默认名按平台走：Windows 是 python，Unix 是 python3
        #[cfg(target_os = "windows")]
        assert_eq!(cfg.verify.python_bin, "python");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(cfg.verify.python_bin, "python3");
        assert_eq!(cfg.max_context_chars, 24_000);
    }

    #[test]
    fn tilde_workspace_root_expands_to_home() {
        let cfg = AppConfig {
            workspace_root: "~/ruyix-runs-probe".into(),
            ..AppConfig::default()
        };
        let p = runs_root(&cfg);
        assert!(p.is_absolute(), "{p:?}");
        assert!(p.ends_with("ruyix-runs-probe"), "{p:?}");
        // `~/` 必须展开到家目录（全仓唯一展开 `~` 的地方）
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from);
        if let Some(home) = home {
            assert!(p.starts_with(home), "`~/` 应展开到家目录：{p:?}");
        }
    }

    #[test]
    fn kb_is_off_by_default_and_has_a_budget() {
        // 默认关 = 不改变现有行为；但预算/阈值必须有可用默认值，
        // 否则用户一开开关就拿到"没有上限的注入"。
        let cfg = AppConfig::default();
        assert!(!cfg.kb.enabled);
        assert!(cfg.kb.token_budget > 0);
        assert!(cfg.kb.top_k >= 1);
        assert!(cfg.kb.min_score > 0.0 && cfg.kb.min_score < 1.0);
        assert!(cfg.kb.per_source_limit >= 1);
    }

    #[test]
    fn partial_kb_toml_only_touches_what_it_says() {
        let cfg: AppConfig =
            toml::from_str("[kb]\nenabled = true\nroots = [\"D:/Notes/ai-notes\"]\n").unwrap();
        assert!(cfg.kb.enabled);
        assert_eq!(cfg.kb.roots, vec!["D:/Notes/ai-notes".to_string()]);
        // 没写的字段落默认值（而不是 0 —— 0 预算等于静默关闭注入）
        assert_eq!(cfg.kb.top_k, 4);
        assert_eq!(cfg.kb.token_budget, 1200);
    }

    #[test]
    fn kb_dir_prefers_explicit_config_over_default() {
        // 环境变量优先级更高，这里只验配置项本身能生效
        let mut kb = KbConfig {
            dir: "D:/tmp/kb-test".into(),
            ..KbConfig::default()
        };
        assert_eq!(kb_dir(&kb), PathBuf::from("D:/tmp/kb-test"));
        kb.dir = String::new();
        assert!(kb_dir(&kb).ends_with("kb"), "{:?}", kb_dir(&kb));
    }

    // ============================================
    // 配置 schema / 表驱动注入
    // ============================================

    /// schema 里的每个路径都必须**真的能在配置树上定位到**，且 kind 与实际类型相符。
    /// 这条测试是"键清单与结构体不漂移"的凭据 —— 以前键名在四处各写一遍，
    /// 漏同步只能靠人眼；现在漂移会直接红。
    #[test]
    fn every_schema_key_resolves_on_the_real_config_tree() {
        let specs = schema();
        assert!(
            specs.len() > 40,
            "schema 键太少（{}），多半没走通推导",
            specs.len()
        );

        let tree = toml::Value::try_from(AppConfig::default()).unwrap();
        for s in &specs {
            let mut got = &tree;
            for seg in s.path.split('.') {
                got = got
                    .as_table()
                    .and_then(|t| t.get(seg))
                    .unwrap_or_else(|| panic!("schema 报了一个配置树上不存在的键：{}", s.path));
            }
            let (kind, default) = leaf_of(got);
            assert_eq!(kind, s.kind, "{} 的类型标注与结构体不符", s.path);
            assert_eq!(default, s.default, "{} 的默认值与结构体不符", s.path);
        }
    }

    /// LLM 端点 / 密钥 / 模型归宿主的 `ai` 段，熵管理整段是实验室能力 —— 都不该进表单。
    #[test]
    fn schema_hides_keys_owned_by_other_layers() {
        let specs = schema();
        let ui = |p: &str| specs.iter().find(|s| s.path == p).map(|s| s.ui);
        for p in ["llm.api_key", "llm.base_url", "llm.model", "sandbox.engine"] {
            assert_eq!(ui(p), Some(false), "{p} 不该出现在配置表单里");
        }
        assert!(
            specs
                .iter()
                .filter(|s| s.path.starts_with("entropy."))
                .all(|s| !s.ui),
            "entropy 整段不该进表单"
        );
        // 对照组：真旋钮必须可见，否则隐藏表写宽了没人发现
        for p in [
            "sandbox.image",
            "kb.enabled",
            "ask.max_per_run",
            "verify.test_timeout_secs",
        ] {
            assert_eq!(ui(p), Some(true), "{p} 应该出现在配置表单里");
        }
    }

    /// v0.5：`llm.api_format` 是**取值有穷**的协议开关。合法值声明在引擎 `ENUM_KEYS`
    /// （宿主表单的下拉与桥的校验都从 `schema()` 读，全局只此一份 —— 前端不许自立一份）；
    /// 同时它归宿主 `ai` / `ai_fallback` 段呈现，harness 表单必须藏掉，否则用户会看到
    /// 两份互相打架的协议开关。
    #[test]
    fn api_format_is_a_declared_enum_hidden_from_the_harness_form() {
        let specs = schema();
        let f = specs
            .iter()
            .find(|s| s.path == "llm.api_format")
            .expect("llm.api_format 必须在 schema 里");
        assert_eq!(
            f.options,
            vec!["openai".to_string(), "anthropic".to_string()],
            "协议可选值由引擎声明，别处不许自立一份"
        );
        assert!(
            !f.ui,
            "api_format 由宿主 ai / ai_fallback 段呈现，harness 表单不重复露出"
        );
        assert_eq!(f.default, "openai", "默认协议必须是 openai —— 旧行为不许变");
    }

    /// v0.5：备用 LLM（故障切换）是 `AppConfig` 上的 `Option<LlmConfig>`。默认 `None` 时
    /// toml **整段省略**，于是它根本不出现在 `schema()` 里 —— 这既是"不配备用就等于没这个
    /// 功能"的凭据，也顺带保证宿主表单不会渲染出一堆空白的备用字段。
    ///
    /// 这条一旦变红，说明有人把 `llm_fallback` 从 `Option` 改成了带默认值的必填段：
    /// 那时它就会进 schema，`FORM_HIDDEN` 里那条 `llm_fallback` 前缀规则必须同时还在。
    #[test]
    fn the_fallback_llm_segment_never_leaks_into_the_harness_form() {
        let leaked: Vec<_> = schema()
            .into_iter()
            .filter(|s| s.path.starts_with("llm_fallback"))
            .map(|s| s.path)
            .collect();
        assert!(
            leaked.is_empty(),
            "默认 None 的备用 LLM 不该出现在 schema 里（漏了会白占表单）: {leaked:?}"
        );
        // 兜底：万一将来它进了 schema，隐藏表也要盖住整段
        assert!(
            is_hidden("llm_fallback.base_url"),
            "FORM_HIDDEN 必须用前缀盖住 llm_fallback.*，否则备用段会漏进 harness 表单"
        );
    }

    /// 值按**目标位置的类型**转换：数字串进数字键、真假词进布尔键、分隔符串进列表键。
    #[test]
    fn apply_flat_coerces_by_target_type() {
        let mut cfg = AppConfig::default();
        let pairs = vec![
            ("llm.temperature".to_string(), "0.55".to_string()),
            ("llm.max_tokens".to_string(), "1234".to_string()),
            ("agent.batch".to_string(), "off".to_string()),
            ("proc.enabled".to_string(), "YES".to_string()),
            (
                "discover.extra".to_string(),
                "gradle, mvn;  ./x.sh".to_string(),
            ),
            ("workspace_root".to_string(), "D:/tmp/runs".to_string()),
        ];
        let n = apply_flat(&mut cfg, &pairs);
        assert_eq!(n, pairs.len(), "合法的键应全部被采纳");
        assert!((cfg.llm.temperature - 0.55).abs() < 1e-6);
        assert_eq!(cfg.llm.max_tokens, 1234);
        assert!(!cfg.agent.batch, "off 应解析为 false");
        assert!(cfg.proc.enabled, "YES 应解析为 true");
        assert_eq!(cfg.discover.extra, vec!["gradle", "mvn", "./x.sh"]);
        assert_eq!(cfg.workspace_root, "D:/tmp/runs");
    }

    /// **坏值不许连坐**：敲错的那个键保持默认，同一批里正确的键照常生效。
    #[test]
    fn apply_flat_keeps_defaults_for_garbage_and_never_fails_the_batch() {
        let mut cfg = AppConfig::default();
        let before = cfg.clone();
        let pairs = vec![
            ("llm.temperature".to_string(), "hot".to_string()), // 非数字
            ("llm.max_tokens".to_string(), "4096".to_string()), // 合法
            ("agent.batch".to_string(), "也许".to_string()),    // 非真假词
            ("max_context_chars".to_string(), "-1".to_string()), // 负数给 usize
            ("没有这个键".to_string(), "x".to_string()),        // 不在 schema 里
            ("kb.top_k".to_string(), "".to_string()),           // 空 = 未设置
        ];
        let n = apply_flat(&mut cfg, &pairs);
        assert_eq!(n, 1, "六个里只有 max_tokens 该被采纳");
        assert_eq!(
            cfg.llm.temperature, before.llm.temperature,
            "坏值必须保持默认"
        );
        assert_eq!(
            cfg.agent.batch, before.agent.batch,
            "不认识的真假词不许当 false"
        );
        assert_eq!(
            cfg.max_context_chars, before.max_context_chars,
            "负数不许把 usize 打成 0"
        );
        assert_eq!(cfg.kb.top_k, before.kb.top_k, "空串 = 未设置");
        assert_eq!(cfg.llm.max_tokens, 4096, "同批里的正确键必须生效");
    }

    /// 旧桥把"值存在但不是 true/1"判为 false，于是 `proc.enabled = "也许"`
    /// 会静默把功能关掉。这条钉住新语义：不认识的真假词 = 保持默认。
    #[test]
    fn a_misspelled_boolean_never_silently_disables_a_feature() {
        let mut cfg = AppConfig::default();
        assert!(cfg.proc.enabled, "前置：这个功能默认是开的");
        apply_flat(
            &mut cfg,
            &[("proc.enabled".to_string(), "ture".to_string())],
        );
        assert!(cfg.proc.enabled, "拼错的 true 不该把功能关掉");
    }
}
