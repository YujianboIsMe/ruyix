//! ruyix 三 scope 配置 → 引擎 `AppConfig`（融合计划 D3 / 附录 C）。
//!
//! 引擎不改：仍然吃 `&AppConfig`。这里负责把 ruyix 的配置键翻译过去：
//! - LLM 端点与密钥复用既有键 `ruyix.code.ai.*`（D8：配置单源）
//! - 引擎侧参数用 `ruyix.code.harness.*`（附录 C）
//! - scope 解析沿用 ruyix 惯例：runtime → project → global（同 ai.rs 的读法）
//!
//! **键清单不在这里写。** 它由引擎的 `config::schema()` 声明 —— 键名、类型、默认值、
//! 枚举取值全部从 `AppConfig` 推导，这里只是"照 schema 去 ruyix 配置里取值"。
//! 改造前这里是 34 个 `Option<String>` 字段 + 34 行读键 + 220 行逐键 `parse` 赋值，
//! 于是加一个键要改六处、还总漏同步（memory 里那条"新键必须同步进 config_bridge"
//! 的铁律就是这个税的化石）。现在加键只需在引擎加字段，宿主与表单自动跟上。
//!
//! 有意决策：ruyix 集成默认 `sandbox.mode = "prefer"`（IDE 要开箱即用；
//! "不允许静默降级"原则不变 —— 未隔离会在报告里显式标注），
//! 而 harness 实验室默认 `require`。

use crate::config::{ConfigManager, Scope};
use harness_engine as engine;

/// 引擎配置在 ruyix 配置里的挂载点（附录 C）
const HARNESS_PREFIX: &str = "ruyix.code.harness.";

/// 这几个引擎键不由 `harness.*` 提供 —— 宿主早就把它们放在 `ai.*` 了（D8：配置单源）。
/// 同一样东西摆两处正是这次要消灭的问题，所以在这里排除，并单独桥接。
///
/// v0.5 起 `llm.api_format` 也归这里：协议开关与端点/密钥/模型同属 `ai` 段，
/// 摆在 harness 段会让用户看到两份互相打架的开关。
const FROM_AI_NAMESPACE: &[&str] = &["llm.base_url", "llm.api_key", "llm.model", "llm.api_format"];

/// 这几个引擎键**必须由宿主按当前项目算出来**，因此不进 `harness.*` 命名空间：
/// 用户在配置里写死一个路径，只会把项目状态写到别的项目头上（`project_state_root` 就是
/// `<便携根>/projects/<项目 key>`，key 由项目路径决定）。
const HOST_INJECTED: &[&str] = &["project_state_root"];

/// 这个引擎键是不是由宿主 `ai` 段供值。
///
/// 备用 LLM（`llm_fallback`）**不在这里**：它是 `Option<LlmConfig>`，默认 `None` 时
/// toml 整段省略，于是根本不进 `schema()`，第 1 步的遍历天然碰不到它 —— 备用段的值
/// 全部由第 2b 步从 `ruyix.code.ai_fallback.*` 直接读（见 `apply_ai_fallback_keys`）。
fn is_host_owned(path: &str) -> bool {
    FROM_AI_NAMESPACE.contains(&path) || HOST_INJECTED.contains(&path)
}

/// 这几个整数键上的 0 不是"关"，而是**把能力静默关死**（批调用会拒掉所有批、
/// 提问次数为 0 等于关掉提问、历史保留 0 轮等于把**当前这一轮**的结果也折掉 ——
/// 模型将永远看不到自己刚拿到的工具结果），所以按非法值处理、保持默认。
///
/// 反例（0 有明确语义，不在此列）：`ask.timeout_secs = 0` 是"无限等"、
/// `agent.max_elapsed_secs = 0` 是"不限时"、`discover.ttl_secs = 0` 是"不缓存"。
const ZERO_MEANS_BROKEN: &[&str] = &[
    "agent.batch_max",
    "agent.history_keep_rounds",
    "ask.max_per_run",
];

/// 从三 scope 读一个键（runtime → project → global，空值跳过）
fn read(mgr: &ConfigManager, key: &str, project_root: Option<&str>) -> Option<String> {
    for scope in [Scope::Runtime, Scope::Project, Scope::Global] {
        if scope == Scope::Project && project_root.is_none() {
            continue;
        }
        if let Ok(Some(val)) = mgr.config_read(&scope, key, project_root)
            && !val.trim().is_empty()
        {
            return Some(val.trim().to_string());
        }
    }
    None
}

/// ruyix 集成的默认值（区别于引擎实验室默认值，见 D3）
pub fn ruyix_workspace_root() -> String {
    crate::paths::current()
        .runs_root()
        .to_string_lossy()
        .to_string()
}

/// 项目状态根（v1.0.0 P3）：`<便携根>/projects/<项目 key>` —— 暂存 / 备份 / 进程日志 /
/// 验证产物都写这儿。**引擎不 `discover`**（它是库，不许自己找 exe），所以只能由宿主注入；
/// key 规则也只在 `paths` 里算一次，引擎侧拿到的已经是算好的目录。
pub fn ruyix_project_state_root(project_root: &str) -> String {
    crate::paths::current()
        .project_dir(project_root)
        .to_string_lossy()
        .to_string()
}

/// 这个值在该键上可用吗？两类例外：
///
/// - **枚举键**（`schema().options` 非空，目前只有 `sandbox.mode`）：只认白名单里的值。
///   写错不能静默换档 —— `yolo` 若被当成"未知档"回落，用户会以为隔离开着。
///   白名单本身声明在引擎里（`config::ENUM_KEYS`），这里只是执行它。
/// - **0 会把能力关死的键**（见 `ZERO_MEANS_BROKEN`）：按非法值处理。
fn acceptable(spec: &engine::config::KeySpec, value: &str) -> bool {
    if !spec.options.is_empty() {
        return spec.options.iter().any(|o| o == value);
    }
    // **类型要对得上**（`KeySpec.kind` 只有 `bool` / `int` / `float` / `text` / `list`）。
    //
    // 为什么要在**读**的时候也拦：真事故（2026-09-25）里 23 个键被写成了字符串 `"on"`
    // —— 一次错位的表单提交把 checkbox 的 DOM 默认值写进了 `proc.max` / `sandbox.image` /
    // `verify.python_bin` / `workspace_root` 这些键（`verify.python_bin="on"` 会让环境探针
    // 老老实实报告「python 未找到 on」）。写侧已修（按 `data-row` 寻址 + 提交前类型闸），
    // 但读侧必须有兜底：**一份已经坏掉的配置不该把引擎带偏** —— 说不通的值一律当没配，回落默认。
    // 这就是"坏文件不继续制造坏行为"。
    let v = value.trim();
    // 0 在这些键上不是"关"，而是**把能力静默关死**（提问次数 0 = 拒掉所有提问、
    // 历史保留 0 轮 = 连当前这轮也折掉）—— 这条对**所有**类型都成立，别只写在 text 分支里。
    if ZERO_MEANS_BROKEN.contains(&spec.path.as_str()) && v == "0" {
        return false;
    }
    match spec.kind {
        "bool" => matches!(
            v.to_ascii_lowercase().as_str(),
            "true" | "false" | "1" | "0" | "on" | "off" | "yes" | "no"
        ),
        "int" => v.parse::<i64>().is_ok(),
        "float" => v.parse::<f64>().is_ok(),
        _ => true,
    }
}

/// 把 `harness.*` 下读到的键灌进配置，再补上 ruyix 与本实验室不同的两处默认。
///
/// 独立成纯函数（不碰配置 IO）是为了可单测 —— 它承载 D3 / D5 两个决策。
fn apply_engine_keys(cfg: &mut engine::config::AppConfig, raw: &[(String, String)]) {
    let specs = engine::config::schema();
    let pairs: Vec<(String, String)> = raw
        .iter()
        .filter(|(k, v)| {
            specs
                .iter()
                .find(|s| &s.path == k)
                .map(|s| acceptable(s, v))
                // 不在 schema 里的键放行，交由 apply_flat 统一忽略（那里是唯一判据）
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    engine::config::apply_flat(cfg, &pairs);

    let configured = |p: &str| pairs.iter().any(|(k, _)| k == p);
    // D3：引擎实验室默认 `require`（不许静默降级），IDE 要开箱即用 → 没配就是 prefer
    if !configured("sandbox.mode") {
        cfg.sandbox.mode = "prefer".to_string();
    }
    // D5：运行目录落进**便携根**（`<exe 同目录>/global/runs`），不与旧 harness 共享
    if !configured("workspace_root") {
        cfg.workspace_root = ruyix_workspace_root();
    }
}

/// 项目状态根的注入（**只在有项目时**）：没有项目就没有桶，此时引擎侧走兜底临时目录，
/// 而绝不会回落到某个项目里（见引擎 `config::project_state_root`）。
fn apply_project_state_root(cfg: &mut engine::config::AppConfig, project_root: Option<&str>) {
    if let Some(proj) = project_root {
        cfg.project_state_root = ruyix_project_state_root(proj);
    }
}

/// 清理用户粘贴的 LLM 端点：尾斜杠 + 完整 `/chat/completions` 后缀都剥掉
/// （引擎自己会拼路径；anthropic 端点不含该后缀，原样保留）。
fn clean_base_url(url: &str) -> String {
    url.trim_end_matches('/')
        .strip_suffix("/chat/completions")
        .unwrap_or_else(|| url.trim_end_matches('/'))
        .to_string()
}

/// LLM 三键 + 协议格式（宿主所有权，D8）。值缺失时保持引擎默认 —— 面板据此走"未配置"引导态。
fn apply_ai_keys(
    cfg: &mut engine::config::AppConfig,
    url: Option<&str>,
    key: Option<&str>,
    model: Option<&str>,
    api_format: Option<&str>,
) {
    if let Some(url) = url {
        // 与 ai.rs 同款清理：用户可能粘贴完整 endpoint，引擎自己会拼 /chat/completions
        cfg.llm.base_url = clean_base_url(url);
    }
    if let Some(key) = key {
        cfg.llm.api_key = key.to_string();
    }
    if let Some(model) = model {
        cfg.llm.model = model.to_string();
    }
    if let Some(fmt) = api_format {
        // 空值保持默认 openai；非 openai/anthropic 当 openai 兜底（不让坏值静默破坏调用）
        let f = fmt.trim();
        if !f.is_empty() {
            cfg.llm.api_format = f.to_string();
        }
    }
}

/// 备用 LLM（故障切换网关）。任一字段被配置即启用 `cfg.llm_fallback`；全空则保持 `None`
/// （不切换，行为等同旧版）。`api_format` 缺省回退 openai；其余生成参数用引擎默认。
fn apply_ai_fallback_keys(
    cfg: &mut engine::config::AppConfig,
    url: Option<&str>,
    key: Option<&str>,
    model: Option<&str>,
    api_format: Option<&str>,
) {
    if url.is_none() && key.is_none() && model.is_none() {
        return; // 没配备用 = 不切换
    }
    let fmt = api_format
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("openai");
    cfg.llm_fallback = Some(engine::config::LlmConfig {
        base_url: url.map(clean_base_url).unwrap_or_default(),
        api_key: key.unwrap_or_default().to_string(),
        model: model.unwrap_or_default().to_string(),
        api_format: fmt.to_string(),
        ..engine::config::LlmConfig::default()
    });
}

/// 命令层入口：读 ruyix 配置 → 引擎 AppConfig。
/// 末尾沿用引擎的环境变量纪律（`DEEPSEEK_API_KEY` 等优先于配置，key 不落盘）——
/// 脚本 / CI 与 GUI 读同一套来源。
pub fn build_app_config(
    mgr: &ConfigManager,
    project_root: Option<&str>,
) -> Result<engine::config::AppConfig, String> {
    let mut cfg = engine::config::AppConfig::default();

    // 1) 照引擎的 schema 读 `harness.*`：引擎有键这里就自动跟上，不会漏同步
    let mut pairs: Vec<(String, String)> = Vec::new();
    for spec in engine::config::schema() {
        if is_host_owned(&spec.path) {
            continue;
        }
        let key = format!("{HARNESS_PREFIX}{}", spec.path);
        if let Some(v) = read(mgr, &key, project_root) {
            pairs.push((spec.path.clone(), v));
        }
    }
    apply_engine_keys(&mut cfg, &pairs);

    // 1a) 项目状态根（v1.0.0 P3）：宿主注入 —— 项目侧那几类写入（暂存/备份/进程日志/
    //     验证产物）全靠它搬进便携根，一个字节都不进用户仓库。
    apply_project_state_root(&mut cfg, project_root);

    // 2) LLM 端点 / 密钥 / 模型 / 协议格式来自 `ruyix.code.ai.*`（D8：配置单源）
    let ai_url = read(mgr, "ruyix.code.ai.api_url", project_root);
    let ai_key = read(mgr, "ruyix.code.ai.api_key", project_root);
    let ai_model = read(mgr, "ruyix.code.ai.model", project_root);
    let ai_fmt = read(mgr, "ruyix.code.ai.api_format", project_root);
    apply_ai_keys(
        &mut cfg,
        ai_url.as_deref(),
        ai_key.as_deref(),
        ai_model.as_deref(),
        ai_fmt.as_deref(),
    );

    // 2a) 工具协议开关（v0.0.6）来自 `ruyix.code.ai.tool_protocol`：缺省保持引擎默认（**开**）。
    // 只在 agent 工具循环一侧生效（见 `engine::llm::chat` 的文档）：填 false = 一行回滚到
    // "动作写在 content 的 JSON 里"的老协议，代价是 DSML 泄露与白烧轮次一起回来。
    if let Some(v) = read(mgr, "ruyix.code.ai.tool_protocol", project_root) {
        cfg.llm.tool_protocol = !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no"
        );
    }

    // 2b) 备用 LLM（故障切换）来自 `ruyix.code.ai_fallback.*`
    let fb_url = read(mgr, "ruyix.code.ai_fallback.api_url", project_root);
    let fb_key = read(mgr, "ruyix.code.ai_fallback.api_key", project_root);
    let fb_model = read(mgr, "ruyix.code.ai_fallback.model", project_root);
    let fb_fmt = read(mgr, "ruyix.code.ai_fallback.api_format", project_root);
    apply_ai_fallback_keys(
        &mut cfg,
        fb_url.as_deref(),
        fb_key.as_deref(),
        fb_model.as_deref(),
        fb_fmt.as_deref(),
    );

    // 3) 规约包的目录回退：`HARNESS_LINT_DIR` 是引擎那套"让测试隔离真实 HOME"
    //    的环境变量之一，配置里没写时它说了算。
    if cfg.lint.package_dir.trim().is_empty()
        && let Ok(dir) = std::env::var("HARNESS_LINT_DIR")
        && !dir.trim().is_empty()
    {
        cfg.lint.package_dir = dir.trim().to_string();
    }

    // 4) 环境变量覆盖（脚本 / CI 与 GUI 同一套来源）
    engine::config::apply_env_overrides(&mut cfg);
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量是进程级全局状态，本模块里碰它的测试必须串行 ——
    /// 否则一个测试 set 的 key 会窜进另一个测试的断言
    /// （实测：`sk-env-bridge` 冒进了 ai 命名空间的用例）。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 用 `harness.*` 键跑一遍桥的核心（不走配置 IO 的那半）。
    fn bridge(pairs: &[(&str, &str)]) -> engine::config::AppConfig {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut cfg = engine::config::AppConfig::default();
        apply_engine_keys(&mut cfg, &owned);
        cfg
    }

    /// 说不通的值必须**当没配**（回落到默认）—— 真事故的回归判据。
    ///
    /// 现场：23 个键被写成字符串 `"on"`（错位的表单提交把 checkbox 的 DOM 默认值写了进去），
    /// 于是 `verify.python_bin="on"`、`sandbox.image="on"`、`proc.max="on"`、`workspace_root="on"`。
    /// 一份已经坏掉的配置不该把引擎带偏：`on` 在 int 键上不是数字 ⇒ 忽略、保持默认。
    #[test]
    fn 说不通的值当没配_坏配置不会把引擎带偏() {
        let cfg = bridge(&[
            ("verify.python_bin", "on"), // text 键：语法上合法，拦不住（写侧才是防线）
            ("proc.max", "on"),          // int 键：说得通吗？不 ⇒ 当没配
            ("sandbox.pids_limit", "on"), // int 同上（注意 `sandbox.cpus` 是 **text**：
            // 语法上拦不住 —— 所以写侧的 `data-row` 寻址才是那道真防线）
            ("agent.batch_max", "0"), // 0 = 会把能力静默关死 ⇒ 当没配
            ("sandbox.mode", "yolo"), // 枚举外 ⇒ 当没配（不许静默换档）
            ("proc.ready_timeout_secs", "60"), // 合法 ⇒ 要生效
        ]);
        let default = engine::config::AppConfig::default();
        assert_eq!(
            cfg.proc.max, default.proc.max,
            "int 键上的 \"on\" 必须被忽略（否则引擎拿着 \"on\" 去 parse）"
        );
        assert_eq!(cfg.sandbox.pids_limit, default.sandbox.pids_limit, "同上");
        assert_eq!(
            cfg.agent.batch_max, default.agent.batch_max,
            "0 会关死能力，按非法处理"
        );
        assert_eq!(
            cfg.sandbox.mode, "prefer",
            "枚举外的值不许生效（D3 的宿舍默认）"
        );
        assert_eq!(
            cfg.proc.ready_timeout_secs, 60,
            "合法值必须真的生效 —— 这道闸不能把正常配置一起拦下"
        );
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ruyix_bridge_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 项目状态根必须落到便携根里的项目桶（A3/A4 的单元级判据：写出去的东西不在项目里）
    #[test]
    fn project_state_root_is_host_injected_under_the_portable_root() {
        crate::paths::set_test_root(r"D:\tmp-ruyix-root2");
        let cfg = bridge(&[]);
        assert!(
            cfg.project_state_root.is_empty(),
            "桥本身不管这个键：它在【宿主注入】名单里"
        );
        let mut cfg2 = engine::config::AppConfig::default();
        apply_project_state_root(&mut cfg2, Some(r"D:\Projects\Rust\ruyix"));
        assert_eq!(
            cfg2.project_state_root.replace('\\', "/"),
            "D:/tmp-ruyix-root2/projects/D-Projects-Rust-ruyix"
        );
        let mut cfg3 = engine::config::AppConfig::default();
        apply_project_state_root(&mut cfg3, None);
        assert!(
            cfg3.project_state_root.is_empty(),
            "没项目就不注入，引擎侧走兜底"
        );
    }

    #[test]
    fn empty_values_yield_ruyix_defaults_not_engine_defaults() {
        crate::paths::set_test_root(r"D:\tmp-ruyix-root");
        let cfg = bridge(&[]);
        // D3：IDE 要开箱即用 → prefer（引擎实验室默认是 require）
        assert_eq!(cfg.sandbox.mode, "prefer");
        // D5：运行目录落进**便携根**（`<exe 同目录>/global/runs`），不与旧 harness 共享
        // （PathBuf::join 在 Windows 上是反斜杠，先归一再断言）
        let normalized_root = cfg.workspace_root.replace('\\', "/");
        assert_eq!(normalized_root, "D:/tmp-ruyix-root/global/runs");
        // kb 默认关闭（附录 C）
        assert!(!cfg.kb.enabled);
        // 不配 key 时保持空串（命令层据此走"未配置"状态而非报错）
        assert!(cfg.llm.api_key.is_empty());
    }

    /// **键名一致性的端到端门禁**：schema 里每一个键都能从任一 scope 读到、
    /// 且真能改到配置上。
    ///
    /// 为什么需要它：以前键名手写在桥里，写错一个字符 = 三个 scope 全读空、
    /// 界面里配了不生效，于是 proc / agent.batch 各写了一条专项测试来防这个。
    /// 现在一条测试盖住**全部**键，而且引擎以后新增的键自动被覆盖 ——
    /// 这正是"键表单源化"要买到的东西。
    ///
    /// **两个 scope 都要过**：runtime 走内存；global **真落盘**（`write_toml_file`
    /// → `read_toml_file`）。只测 runtime 会漏掉一条真实存在的断层 ——
    /// 含点键（`kb.top_k`）在 TOML 里必须被写成**带引号的键**（`"kb.top_k" = "4"`），
    /// 不引号就成嵌套表 `[harness.kb]`，读回来键名不再是 `kb.top_k`，
    /// 于是"文件里配了、界面里不生效"。这条只有走一遍真文件才能钉住。
    #[test]
    fn every_schema_key_is_readable_from_any_scope() {
        // 造一个"类型合法、但与默认值不同"的探针值
        fn probe(spec: &engine::config::KeySpec) -> String {
            match spec.kind {
                "bool" => if spec.default == "true" {
                    "false"
                } else {
                    "true"
                }
                .into(),
                "int" => (spec.default.parse::<i64>().unwrap_or(0) + 1).to_string(),
                "float" => (spec.default.parse::<f64>().unwrap_or(0.0) + 1.5).to_string(),
                "list" => "probe-one,probe-two".into(),
                _ => format!("{}-probe", spec.default),
            }
        }

        let specs: Vec<engine::config::KeySpec> = engine::config::schema()
            .into_iter()
            // 宿主所有的键（ai / ai_fallback 段）不走 harness 命名空间，另有专项测试覆盖
            .filter(|s| !is_host_owned(&s.path))
            // 枚举键的探针必然在白名单外（会被正确拒掉），另有一条测试专测它
            .filter(|s| s.options.is_empty())
            .collect();
        assert!(specs.len() > 30, "schema 键太少（{}）", specs.len());

        for scope in [Scope::Runtime, Scope::Global] {
            let dir = temp_dir("allkeys");
            let mut mgr = ConfigManager::new_with_dir(dir.clone());

            for s in &specs {
                let key = format!("{HARNESS_PREFIX}{}", s.path);
                let val = probe(s);
                mgr.config_write(&scope, &key, &val, None)
                    .unwrap_or_else(|e| panic!("写 {key} 失败：{e}"));
            }

            let cfg = build_app_config(&mgr, None).unwrap();
            for s in &specs {
                let want = probe(s);
                let got = engine::config::flat_value(&cfg, &s.path)
                    .unwrap_or_else(|| panic!("{} 在配置上读不到", s.path));
                if s.kind == "float" {
                    let a: f64 = got.parse().unwrap_or(f64::NAN);
                    let b: f64 = want.parse().unwrap_or(f64::INFINITY);
                    assert!((a - b).abs() < 1e-6, "{}：读到 {got}，期望 {want}", s.path);
                } else {
                    assert_eq!(
                        got,
                        want,
                        "{} 没读到 {} scope 里写的值",
                        s.path,
                        scope.name()
                    );
                }
            }

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn llm_keys_bridge_from_ai_namespace() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Safety：本测试持有 ENV_LOCK，是此刻唯一读写该环境变量的测试
        unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
        let dir = temp_dir("ai");
        let mut mgr = ConfigManager::new_with_dir(dir.clone());
        for (k, v) in [
            (
                "ruyix.code.ai.api_url",
                "https://api.example.com/v1/chat/completions/",
            ),
            ("ruyix.code.ai.api_key", "sk-test"),
            ("ruyix.code.ai.model", "deepseek-chat"),
        ] {
            mgr.config_write(&Scope::Runtime, k, v, None).unwrap();
        }
        let cfg = build_app_config(&mgr, None).unwrap();
        // 尾斜杠与完整 endpoint 粘贴都被清理（引擎自己拼 /chat/completions）
        assert_eq!(cfg.llm.base_url, "https://api.example.com/v1");
        assert_eq!(cfg.llm.api_key, "sk-test");
        assert_eq!(cfg.llm.model, "deepseek-chat");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v0.5：协议格式（宿主 `ai` 段）与备用 LLM（`ai_fallback` 段）的端到端桥接。
    ///
    /// 为什么必须是一条**真配置管理器**的测试：这两个功能的新键都在宿主命名空间里，
    /// 桥的第 1 步（按 schema 遍历 `harness.*`）**看不见它们** —— 只测 schema 或只测
    /// `apply_*` 函数都验不到"界面配了真进引擎"。翻车形态是：表单渲染正常、保存报成功、
    /// 引擎却仍用旧协议 —— 界面完全看不出来。
    #[test]
    fn api_format_and_fallback_llm_bridge_from_the_host_namespaces() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Safety：本测试持有 ENV_LOCK，是此刻唯一读写该环境变量的测试
        unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };

        // ① 什么都没配：主用默认 openai（旧行为），备用保持 None（不切换）
        let dir_none = temp_dir("fb_none");
        let mgr = ConfigManager::new_with_dir(dir_none.clone());
        let cfg = build_app_config(&mgr, None).unwrap();
        assert_eq!(cfg.llm.api_format, "openai", "默认协议必须是 openai");
        assert!(
            cfg.llm_fallback.is_none(),
            "没配备用 = 不切换，行为等同旧版"
        );
        let _ = std::fs::remove_dir_all(&dir_none);

        // ② 配了：主用协议生效；备用独立成一个 LlmConfig，且**允许与主用异构**
        let dir_set = temp_dir("fb_set");
        let mut mgr = ConfigManager::new_with_dir(dir_set.clone());
        for (k, v) in [
            ("ruyix.code.ai.api_format", "anthropic"),
            // 备用端点粘贴完整 URL + 尾斜杠，同样要被清理
            (
                "ruyix.code.ai_fallback.api_url",
                "https://backup.example.com/v1/",
            ),
            ("ruyix.code.ai_fallback.api_key", "sk-backup"),
            ("ruyix.code.ai_fallback.model", "claude-sonnet-4"),
            ("ruyix.code.ai_fallback.api_format", "anthropic"),
        ] {
            mgr.config_write(&Scope::Runtime, k, v, None).unwrap();
        }
        let cfg = build_app_config(&mgr, None).unwrap();
        assert_eq!(cfg.llm.api_format, "anthropic", "主用协议要真进引擎");
        let fb = cfg
            .llm_fallback
            .expect("配了 ai_fallback.* 就该启用备用 LLM");
        assert_eq!(fb.base_url, "https://backup.example.com/v1", "尾斜杠要清理");
        assert_eq!(fb.api_key, "sk-backup");
        assert_eq!(fb.model, "claude-sonnet-4");
        assert_eq!(fb.api_format, "anthropic", "备用协议与主用互相独立");
        // 未配的生成参数继承引擎默认（不是空值）
        assert_eq!(
            fb.max_tokens,
            engine::config::AppConfig::default().llm.max_tokens
        );
        let _ = std::fs::remove_dir_all(&dir_set);
    }

    /// 备用 LLM 是**增量**开关：只要填了端点/密钥/模型里的任一个就启用；
    /// 全空（表单留白）必须保持 None，否则"没配"和"配了个空端点"分不开。
    #[test]
    fn fallback_llm_is_enabled_by_any_single_field() {
        let mut cfg = engine::config::AppConfig::default();
        apply_ai_fallback_keys(&mut cfg, None, None, None, None);
        assert!(cfg.llm_fallback.is_none(), "全空 = 不切换");

        let mut cfg = engine::config::AppConfig::default();
        apply_ai_fallback_keys(&mut cfg, None, Some("sk-only"), None, None);
        let fb = cfg.llm_fallback.expect("只配了密钥也算配了备用");
        assert_eq!(fb.api_key, "sk-only");
        assert_eq!(fb.api_format, "openai", "协议缺省回落 openai");
    }

    /// 数字 / 布尔键上的垃圾值一律回落默认，不硬失败。
    #[test]
    fn numeric_garbage_falls_back_to_defaults() {
        let d = engine::config::AppConfig::default();
        let cfg = bridge(&[
            ("llm.temperature", "hot"),
            ("llm.max_tokens", "-3"),
            ("kb.top_k", "many"),
            ("gate.max_full_attempts", "many"),
        ]);
        assert_eq!(cfg.llm.temperature, d.llm.temperature);
        assert_eq!(cfg.llm.max_tokens, d.llm.max_tokens);
        assert_eq!(cfg.kb.top_k, d.kb.top_k);
        assert_eq!(cfg.gate.max_full_attempts, d.gate.max_full_attempts);
    }

    /// v0.4：计划执行开关与总时长闸。默认**开**（引擎事实优先），但 `false` 必须能关回去
    /// —— 这两条断言合起来才是"可回退"的凭据
    #[test]
    fn step_execute_plan_and_elapsed_budget_bridge() {
        let d = bridge(&[]);
        assert!(
            d.step.execute_plan,
            "默认必须开：关着的时候进度只能靠推断，实测会大面积误判"
        );
        assert_eq!(d.agent.max_elapsed_secs, 1800);

        let off = bridge(&[("step.execute_plan", "false")]);
        assert!(!off.step.execute_plan, "退路必须真的能关掉");

        let on = bridge(&[
            ("step.execute_plan", "true"),
            ("step.max_steps", "12"),
            ("agent.max_elapsed_secs", "0"),
        ]);
        assert!(on.step.execute_plan);
        assert_eq!(on.step.max_steps, 12);
        assert_eq!(on.agent.max_elapsed_secs, 0, "0 = 不限，是合法值");

        let bad = bridge(&[("step.max_steps", "很多"), ("agent.max_elapsed_secs", "-5")]);
        assert_eq!(bad.step.max_steps, d.step.max_steps, "非法值回落默认");
        assert_eq!(bad.agent.max_elapsed_secs, 1800);
    }

    /// v0.5：命令发现默认开，`false` 能关回去；`extra` 的手写格式要容错
    #[test]
    fn discover_bridge_defaults_on_and_parses_the_extra_list() {
        let d = bridge(&[]);
        assert!(
            d.discover.enabled,
            "默认必须开：关着的时候模型只能自己试命令在不在，实测会烧掉好几轮"
        );
        assert!(d.discover.extra.is_empty());

        let off = bridge(&[("discover.enabled", "false")]);
        assert!(!off.discover.enabled, "退路必须真的能关掉");

        // 逗号 / 分号 / 空白混着写都要能拆开，空项忽略
        let loose = bridge(&[
            ("discover.ttl_secs", "0"),
            ("discover.extra", "helm, curl; jq\n terraform  "),
        ]);
        assert_eq!(loose.discover.ttl_secs, 0, "0 = 不缓存，是合法值");
        assert_eq!(
            loose.discover.extra,
            vec!["helm", "curl", "jq", "terraform"],
            "分隔符与空白都要容错"
        );

        let bad = bridge(&[("discover.ttl_secs", "很久")]);
        assert_eq!(bad.discover.ttl_secs, d.discover.ttl_secs);
    }

    /// v0.8：提问（ask_user）默认开；关掉能回退；非法上限保持默认
    #[test]
    fn ask_bridge_defaults_on_and_parses_three_keys() {
        let d = bridge(&[]);
        assert!(d.ask.enabled, "提问默认必须是开的");
        assert_eq!(d.ask.max_per_run, 4);

        let cfg = bridge(&[
            ("ask.enabled", "false"),
            ("ask.timeout_secs", "0"),
            ("ask.max_per_run", "9"),
        ]);
        assert!(!cfg.ask.enabled, "false 必须能关回去");
        assert_eq!(cfg.ask.timeout_secs, 0, "0 = 无限等，是合法值");
        assert_eq!(cfg.ask.max_per_run, 9);

        let bad = bridge(&[("ask.timeout_secs", "很久")]);
        assert_eq!(bad.ask.timeout_secs, d.ask.timeout_secs);
    }

    /// 0 会把能力静默关死的键：按非法值处理，保持默认。
    #[test]
    fn zero_would_silently_kill_these_features() {
        let d = engine::config::AppConfig::default();
        let cfg = bridge(&[("ask.max_per_run", "0"), ("agent.batch_max", "0")]);
        assert_eq!(
            cfg.ask.max_per_run, d.ask.max_per_run,
            "0 会把所有提问都拒掉 = 悄悄关死能力"
        );
        assert_eq!(
            cfg.agent.batch_max, d.agent.batch_max,
            "0 会把所有批都拒掉 = 把功能关死"
        );
        // 负数 / 非数字同样保持默认
        let worse = bridge(&[("ask.max_per_run", "-2"), ("agent.batch_max", "很多")]);
        assert_eq!(worse.ask.max_per_run, d.ask.max_per_run);
        assert_eq!(worse.agent.batch_max, d.agent.batch_max);
    }

    /// v0.11 历史折叠：默认开（保留最近 12 轮），两个键都能一行回退；
    /// `history_keep_rounds = 0` 按非法值处理 —— 0 轮等于把当前这轮的结果也折掉
    #[test]
    fn agent_history_fold_bridge_defaults_on_and_can_be_turned_off() {
        let d = bridge(&[]);
        assert!(d.agent.history_trim, "默认开：老轮次的正文不该每轮重发");
        // 断言的是**下限**不是具体数字：窗口小到 6 会让模型"刚读过的东西六轮后被折掉、
        // 只能重读同一批文件"（实测两次跑死在这条线上）。以后往上调不触发假红，
        // 但调回 6 这种量级必须红。
        assert!(
            d.agent.history_keep_rounds >= 10,
            "保留窗口太小 ⇒ 模型刚读过就失忆（实测定在 12）"
        );

        let off = bridge(&[
            ("agent.history_trim", "false"),
            ("agent.history_keep_rounds", "3"),
        ]);
        assert!(!off.agent.history_trim, "必须能一行回退到只增不裁");
        assert_eq!(off.agent.history_keep_rounds, 3);

        let zero = bridge(&[("agent.history_keep_rounds", "0")]);
        assert_eq!(
            zero.agent.history_keep_rounds, d.agent.history_keep_rounds,
            "0 轮 = 连当前这轮的结果都折掉，模型将看不到自己刚拿到的东西"
        );
    }

    /// v0.5：环境准备（按需安装）默认开，`false` 能关回去
    #[test]
    fn env_install_bridge_defaults_on_and_can_be_turned_off() {
        assert!(
            bridge(&[]).env.install_enabled,
            "默认开：自成长闭环的最后一环"
        );
        let off = bridge(&[("env.install_enabled", "false")]);
        assert!(!off.env.install_enabled, "必须能一行回退");
        // "1" 也算开
        assert!(bridge(&[("env.install_enabled", "1")]).env.install_enabled);
    }

    /// v0.6：托管进程默认开（关掉会把模型推回 start/Start-Process 歪招），三个键都可回退
    #[test]
    fn proc_bridge_defaults_on_and_each_knob_can_be_overridden() {
        let d = bridge(&[]);
        assert!(d.proc.enabled, "默认开：这是永不退出服务模式的唯一出口");

        let cfg = bridge(&[
            ("proc.enabled", "false"),
            ("proc.max", "8"),
            ("proc.ready_timeout_secs", "120"),
        ]);
        assert!(!cfg.proc.enabled, "必须能一行回退");
        assert_eq!(cfg.proc.max, 8);
        assert_eq!(cfg.proc.ready_timeout_secs, 120);

        let bad = bridge(&[("proc.max", "很多"), ("proc.ready_timeout_secs", "-1")]);
        assert_eq!(bad.proc.max, d.proc.max);
        assert_eq!(bad.proc.ready_timeout_secs, d.proc.ready_timeout_secs);
    }

    /// v0.7 批调用：默认全开（省轮次的主通道），三个键都能一行回退
    #[test]
    fn agent_batch_bridge_defaults_on_and_each_knob_can_be_overridden() {
        let d = bridge(&[]);
        assert!(d.agent.batch, "默认开：这是省轮次的主通道");
        assert!(d.agent.batch_parallel, "默认并发跑只读");

        let off = bridge(&[
            ("agent.batch", "false"),
            ("agent.batch_max", "4"),
            ("agent.batch_parallel", "false"),
        ]);
        assert!(!off.agent.batch, "必须能一行回退到一轮一个调用");
        assert_eq!(off.agent.batch_max, 4);
        assert!(!off.agent.batch_parallel);
    }

    /// 沙箱档位是白名单（取值声明在引擎里）：三档之外的值一律回落 ruyix 默认
    /// （prefer），绝不"就着那个字符串往下走"。
    #[test]
    fn sandbox_mode_is_whitelisted() {
        assert_eq!(
            bridge(&[("sandbox.mode", "require")]).sandbox.mode,
            "require"
        );
        assert_eq!(bridge(&[("sandbox.mode", "off")]).sandbox.mode, "off");
        assert_eq!(
            bridge(&[("sandbox.mode", "yolo")]).sandbox.mode,
            "prefer",
            "白名单外的值必须回落 prefer，不能静默换档"
        );
        assert_eq!(
            bridge(&[("lint.max_repair_rounds", "5")])
                .lint
                .max_repair_rounds,
            5
        );
    }

    /// v0.3 门禁与复核：默认都开，没传的键保持默认，非法值不硬失败
    #[test]
    fn gate_and_reflect_keys_bridge_with_defaults_on_garbage() {
        let d = bridge(&[]);
        assert!(d.gate.narrow && d.gate.full && d.reflect.enabled);

        let cfg = bridge(&[
            ("gate.narrow", "false"),
            ("gate.max_full_attempts", "1"),
            ("reflect.max_rounds", "3"),
            ("reflect.model", "deepseek-v4-pro"),
        ]);
        assert!(!cfg.gate.narrow);
        assert!(cfg.gate.full, "没传的键保持默认");
        assert_eq!(cfg.gate.max_full_attempts, 1);
        assert_eq!(cfg.reflect.max_rounds, 3);
        assert_eq!(cfg.reflect.model, "deepseek-v4-pro");
    }

    #[test]
    fn build_app_config_honors_env_key_over_files() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = temp_dir("env");
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = crate::config::ConfigManager::new_with_dir(dir.clone());

        // 无环境变量：未配置态（面板据此走引导视图而非报错）
        // Safety：本测试持有 ENV_LOCK，是此刻唯一读写该环境变量的测试
        unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
        let cfg = build_app_config(&mgr, None).unwrap();
        assert!(cfg.llm.api_key.is_empty());

        // 环境变量优先于配置且不落盘（引擎纪律）
        unsafe { std::env::set_var("DEEPSEEK_API_KEY", "sk-env-bridge") };
        let cfg = build_app_config(&mgr, None).unwrap();
        assert_eq!(cfg.llm.api_key, "sk-env-bridge");
        unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
