//! 配置：`%APPDATA%\darkhorse-harness\config.toml`，环境变量可覆盖。
//!
//! 环境变量覆盖的意义：脚本/CI 里塞 `DEEPSEEK_API_KEY` 就能复用，不用改 GUI 配置。

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
fn d_bin(name: &str) -> String {
    name.to_string()
}
fn d_cmd_timeout() -> u64 {
    120
}
fn d_test_timeout() -> u64 {
    300
}
fn d_workspace() -> String {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".darkhorse")
        .join("harness")
        .join("runs")
        .to_string_lossy()
        .to_string()
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
/// 会误判；"有没有写文件"不会。见 `doc/需求-Agent-验证与反思-v0.3.md`。
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
    /// 单个步骤内部自己的工具轮次上限（与主循环的 `MAX_STEPS` 相互独立）
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

fn d_step_max_steps() -> usize {
    24
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
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_elapsed_secs: d_agent_max_elapsed_secs(),
        }
    }
}

fn d_agent_max_elapsed_secs() -> u64 {
    1800
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
    /// 知识库根目录（空 = `<配置目录>/darkhorse-harness/kb`）
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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppConfig {
    #[serde(default)]
    pub llm: LlmConfig,
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
            verify: VerifyConfig::default(),
            gate: GateConfig::default(),
            reflect: ReflectConfig::default(),
            agent: AgentConfig::default(),
            step: StepAgentConfig::default(),
            lint: LintConfig::default(),
            entropy: EntropyConfig::default(),
            sandbox: SandboxConfig::default(),
            kb: KbConfig::default(),
            workspace_root: d_workspace(),
            max_context_chars: d_max_context(),
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("darkhorse-harness").join("config.toml")
}

/// 知识库根目录：注册表 `kb.json` 与每个来源的索引库都放这里。
///
/// 与 `HARNESS_LINT_DIR` 同一套做法：允许环境变量覆盖（**测试靠它隔离**，
/// 不然单测会往用户真实的 `%APPDATA%` 里写东西）。
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
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("darkhorse-harness").join("kb")
}

/// 相对 workspace_root 的展开（配置里可能写 `~` 或相对路径）
pub fn runs_root(cfg: &AppConfig) -> PathBuf {
    let raw = cfg.workspace_root.trim();
    if raw.is_empty() {
        return PathBuf::from(d_workspace());
    }
    let expanded = if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest)
    } else {
        PathBuf::from(raw)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(expanded)
    }
}

/// 读配置。文件不存在返回默认值（不报错），解析失败才报错 —— 用户手改坏了要能看见。
pub fn load() -> Result<AppConfig, String> {
    let path = config_path();
    let mut cfg = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取配置失败 {}: {e}", path.display()))?;
        toml::from_str::<AppConfig>(&text)
            .map_err(|e| format!("解析配置失败 {}: {e}", path.display()))?
    } else {
        AppConfig::default()
    };
    apply_env_overrides(&mut cfg);
    Ok(cfg)
}

/// 环境变量优先于文件：方便脚本化调用 / 不把 key 落盘。
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

pub fn save(cfg: &AppConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let text = toml::to_string_pretty(cfg).map_err(|e| format!("序列化配置失败: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("写入配置失败 {}: {e}", path.display()))?;
    Ok(())
}

/// 读配置文件里某个段下的原始文本值（评估实验要用：临时改 `[kb]` 参数再还原）。
///
/// 为什么按文本读而不是反序列化：**要保住用户手写的注释与顺序**，
/// 只动那一行的值，别把整个 config.toml 重排。
pub fn raw_value(section: &str, key: &str) -> Option<String> {
    let path = config_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let mut cur = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t.to_string();
        } else if cur == format!("[{section}]") {
            if let Some(rest) = t.strip_prefix(&format!("{key} = ")) {
                return Some(rest.trim().to_string());
            }
            if let Some(rest) = t.strip_prefix(&format!("{key}=")) {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

/// 覆盖配置文件里某个段下的某个键（只改那一行，其余原样）。找不到该键返回 false。
pub fn set_raw_value(section: &str, key: &str, value: &str) -> bool {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    let mut cur = String::new();
    let mut hit = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t.to_string();
            out.push(line.to_string());
            continue;
        }
        if cur == format!("[{section}]")
            && (t.starts_with(&format!("{key} = ")) || t.starts_with(&format!("{key}=")))
        {
            out.push(format!("{key} = {value}"));
            hit = true;
            continue;
        }
        out.push(line.to_string());
    }
    if !hit {
        return false;
    }
    let mut body = out.join("\n");
    body.push('\n');
    std::fs::write(&path, body).is_ok()
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
        assert!(!cfg.workspace_root.is_empty());
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
            workspace_root: "~/.darkhorse/harness/runs".into(),
            ..AppConfig::default()
        };
        let p = runs_root(&cfg);
        assert!(p.is_absolute(), "{p:?}");
        assert!(p.ends_with("runs"), "{p:?}");
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
}
