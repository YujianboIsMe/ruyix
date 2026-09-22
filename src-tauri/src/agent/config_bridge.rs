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
const FROM_AI_NAMESPACE: &[&str] = &["llm.base_url", "llm.api_key", "llm.model"];

/// 这几个整数键上的 0 不是"关"，而是**把能力静默关死**（批调用会拒掉所有批、
/// 提问次数为 0 等于关掉提问），所以按非法值处理、保持默认。
///
/// 反例（0 有明确语义，不在此列）：`ask.timeout_secs = 0` 是"无限等"、
/// `agent.max_elapsed_secs = 0` 是"不限时"、`discover.ttl_secs = 0` 是"不缓存"。
const ZERO_MEANS_BROKEN: &[&str] = &["agent.batch_max", "ask.max_per_run"];

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
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".ruyix")
        .join("code")
        .join("agent")
        .join("runs")
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
    !(ZERO_MEANS_BROKEN.contains(&spec.path.as_str()) && value == "0")
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
    // D5：运行目录落在 ruyix 名下，不与旧 harness 共享
    if !configured("workspace_root") {
        cfg.workspace_root = ruyix_workspace_root();
    }
}

/// LLM 三键（宿主所有权，D8）。值缺失时保持引擎默认 —— 面板据此走"未配置"引导态。
fn apply_ai_keys(
    cfg: &mut engine::config::AppConfig,
    url: Option<&str>,
    key: Option<&str>,
    model: Option<&str>,
) {
    if let Some(url) = url {
        // 与 ai.rs 同款清理：用户可能粘贴完整 endpoint，引擎自己会拼 /chat/completions
        cfg.llm.base_url = url
            .trim_end_matches('/')
            .strip_suffix("/chat/completions")
            .unwrap_or(url.trim_end_matches('/'))
            .to_string();
    }
    if let Some(key) = key {
        cfg.llm.api_key = key.to_string();
    }
    if let Some(model) = model {
        cfg.llm.model = model.to_string();
    }
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
        if FROM_AI_NAMESPACE.contains(&spec.path.as_str()) {
            continue;
        }
        let key = format!("{HARNESS_PREFIX}{}", spec.path);
        if let Some(v) = read(mgr, &key, project_root) {
            pairs.push((spec.path.clone(), v));
        }
    }
    apply_engine_keys(&mut cfg, &pairs);

    // 2) LLM 端点 / 密钥 / 模型来自 `ruyix.code.ai.*`（D8：配置单源）
    let ai_url = read(mgr, "ruyix.code.ai.api_url", project_root);
    let ai_key = read(mgr, "ruyix.code.ai.api_key", project_root);
    let ai_model = read(mgr, "ruyix.code.ai.model", project_root);
    apply_ai_keys(
        &mut cfg,
        ai_url.as_deref(),
        ai_key.as_deref(),
        ai_model.as_deref(),
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

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ruyix_bridge_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn empty_values_yield_ruyix_defaults_not_engine_defaults() {
        let cfg = bridge(&[]);
        // D3：IDE 要开箱即用 → prefer（引擎实验室默认是 require）
        assert_eq!(cfg.sandbox.mode, "prefer");
        // D5：运行目录落在 ruyix 名下，不与旧 harness 共享
        // （PathBuf::join 在 Windows 上是反斜杠，先归一再断言）
        let normalized_root = cfg.workspace_root.replace('\\', "/");
        assert!(normalized_root.contains(".ruyix/code/agent/runs"));
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
            .filter(|s| !FROM_AI_NAMESPACE.contains(&s.path.as_str()))
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
