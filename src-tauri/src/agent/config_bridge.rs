//! ruyix 三 scope 配置 → 引擎 `AppConfig`（融合计划 D3 / 附录 C）。
//!
//! 引擎不改：仍然吃 `&AppConfig`。这里负责把 ruyix 的配置键翻译过去：
//! - LLM 端点与密钥复用既有键 `ruyix.code.ai.*`（D8：配置单源）
//! - 引擎侧参数用 `ruyix.code.harness.*`（附录 C）
//! - scope 解析沿用 ruyix 惯例：runtime → project → global（同 ai.rs 的读法）
//!
//! 有意决策：ruyix 集成默认 `sandbox.mode = "prefer"`（IDE 要开箱即用；
//! "不允许静默降级"原则不变 —— 未隔离会在报告里显式标注），
//! 而 harness 实验室默认 `require`。

use crate::config::{ConfigManager, Scope};
use harness_engine as engine;

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

/// 从 ruyix 配置里读出的散键集合。独立成结构是为了让
/// [`apply_overrides`] 成为可单测的纯函数（不碰配置 IO）。
#[derive(Debug, Default, Clone)]
pub struct BridgeValues {
    pub api_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub workspace_root: Option<String>,
    pub llm_temperature: Option<String>,
    pub llm_max_tokens: Option<String>,
    pub sandbox_mode: Option<String>,
    pub sandbox_image: Option<String>,
    pub lint_enabled: Option<String>,
    pub lint_package_dir: Option<String>,
    pub lint_max_repair_rounds: Option<String>,
    pub kb_enabled: Option<String>,
    pub kb_top_k: Option<String>,
    // v0.3 质量门禁：机械验证 + 反思
    pub gate_narrow: Option<String>,
    pub gate_full: Option<String>,
    pub gate_max_full_attempts: Option<String>,
    pub gate_staged_timeout: Option<String>,
    pub reflect_enabled: Option<String>,
    pub reflect_max_rounds: Option<String>,
    pub reflect_model: Option<String>,
    // v0.4 计划步骤执行体 + run 级总时长闸
    pub step_execute_plan: Option<String>,
    pub step_max_steps: Option<String>,
    pub agent_max_elapsed_secs: Option<String>,
    // v0.5 命令发现：把"本机有什么命令"实测出来喂进上下文
    pub discover_enabled: Option<String>,
    pub discover_ttl_secs: Option<String>,
    pub discover_extra: Option<String>,
    // v0.5 环境准备：缺失工具的按需安装（走 Connect，宿主裁量 + 留记录）
    pub env_install_enabled: Option<String>,
    // v0.6 托管进程：execute 的第三个生命周期维度（后台起 + 命令判就绪 + 句柄收）
    pub proc_enabled: Option<String>,
    pub proc_max: Option<String>,
    pub proc_ready_timeout: Option<String>,
}

impl BridgeValues {
    pub fn from_config(mgr: &ConfigManager, project_root: Option<&str>) -> Self {
        Self {
            api_url: read(mgr, "ruyix.code.ai.api_url", project_root),
            api_key: read(mgr, "ruyix.code.ai.api_key", project_root),
            model: read(mgr, "ruyix.code.ai.model", project_root),
            workspace_root: read(mgr, "ruyix.code.harness.workspace_root", project_root),
            llm_temperature: read(mgr, "ruyix.code.harness.llm.temperature", project_root),
            llm_max_tokens: read(mgr, "ruyix.code.harness.llm.max_tokens", project_root),
            sandbox_mode: read(mgr, "ruyix.code.harness.sandbox.mode", project_root),
            sandbox_image: read(mgr, "ruyix.code.harness.sandbox.image", project_root),
            lint_enabled: read(mgr, "ruyix.code.harness.lint.enabled", project_root),
            lint_package_dir: read(mgr, "ruyix.code.harness.lint.package_dir", project_root),
            lint_max_repair_rounds: read(
                mgr,
                "ruyix.code.harness.lint.max_repair_rounds",
                project_root,
            ),
            kb_enabled: read(mgr, "ruyix.code.harness.kb.enabled", project_root),
            kb_top_k: read(mgr, "ruyix.code.harness.kb.top_k", project_root),
            gate_narrow: read(mgr, "ruyix.code.harness.gate.narrow", project_root),
            gate_full: read(mgr, "ruyix.code.harness.gate.full", project_root),
            gate_max_full_attempts: read(
                mgr,
                "ruyix.code.harness.gate.max_full_attempts",
                project_root,
            ),
            gate_staged_timeout: read(
                mgr,
                "ruyix.code.harness.gate.staged_timeout_secs",
                project_root,
            ),
            reflect_enabled: read(mgr, "ruyix.code.harness.reflect.enabled", project_root),
            reflect_max_rounds: read(mgr, "ruyix.code.harness.reflect.max_rounds", project_root),
            reflect_model: read(mgr, "ruyix.code.harness.reflect.model", project_root),
            step_execute_plan: read(mgr, "ruyix.code.harness.step.execute_plan", project_root),
            step_max_steps: read(mgr, "ruyix.code.harness.step.max_steps", project_root),
            agent_max_elapsed_secs: read(
                mgr,
                "ruyix.code.harness.agent.max_elapsed_secs",
                project_root,
            ),
            discover_enabled: read(mgr, "ruyix.code.harness.discover.enabled", project_root),
            discover_ttl_secs: read(mgr, "ruyix.code.harness.discover.ttl_secs", project_root),
            discover_extra: read(mgr, "ruyix.code.harness.discover.extra", project_root),
            env_install_enabled: read(mgr, "ruyix.code.harness.env.install_enabled", project_root),
            proc_enabled: read(mgr, "ruyix.code.harness.proc.enabled", project_root),
            proc_max: read(mgr, "ruyix.code.harness.proc.max", project_root),
            proc_ready_timeout: read(
                mgr,
                "ruyix.code.harness.proc.ready_timeout_secs",
                project_root,
            ),
        }
    }
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

/// 把散键映射进 AppConfig。非法值一律**保持默认并忽略**（配置桥不做硬失败 ——
/// 用户敲错一个数字不该让面板整个不可用；真实值在引擎各自的校验层兜底）。
pub fn apply_overrides(cfg: &mut engine::config::AppConfig, v: &BridgeValues) {
    // LLM：端点/密钥/模型来自 ruyix.code.ai.*（配置单源，D8）
    if let Some(url) = &v.api_url {
        // 与 ai.rs 同款清理：用户可能粘贴完整 endpoint，引擎自己会拼 /chat/completions
        cfg.llm.base_url = url
            .trim_end_matches('/')
            .strip_suffix("/chat/completions")
            .unwrap_or(url.trim_end_matches('/'))
            .to_string();
    }
    if let Some(key) = &v.api_key {
        cfg.llm.api_key = key.clone();
    }
    if let Some(model) = &v.model {
        cfg.llm.model = model.clone();
    }
    if let Some(t) = v
        .llm_temperature
        .as_deref()
        .and_then(|s| s.parse::<f32>().ok())
    {
        cfg.llm.temperature = t;
    }
    if let Some(m) = v
        .llm_max_tokens
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok())
    {
        cfg.llm.max_tokens = m;
    }

    // 沙箱：ruyix 默认 prefer（D3），仅接受三档合法值
    cfg.sandbox.mode = match v.sandbox_mode.as_deref() {
        Some("require") => "require".to_string(),
        Some("off") => "off".to_string(),
        Some("prefer") | None => "prefer".to_string(),
        Some(_) => "prefer".to_string(),
    };
    if let Some(image) = &v.sandbox_image {
        cfg.sandbox.image = image.clone();
    }

    // 规约
    if let Some(e) = v.lint_enabled.as_deref() {
        cfg.lint.enabled = e == "true" || e == "1";
    }
    if let Some(dir) = &v.lint_package_dir {
        cfg.lint.package_dir = dir.clone();
    } else if let Ok(env_dir) = std::env::var("HARNESS_LINT_DIR")
        && !env_dir.trim().is_empty()
    {
        cfg.lint.package_dir = env_dir.trim().to_string();
    }
    if let Some(r) = v
        .lint_max_repair_rounds
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok())
    {
        cfg.lint.max_repair_rounds = r;
    }

    // 知识库：默认关闭（不改变现有行为）
    if let Some(e) = v.kb_enabled.as_deref() {
        cfg.kb.enabled = e == "true" || e == "1";
    }
    if let Some(k) = v.kb_top_k.as_deref().and_then(|s| s.parse::<usize>().ok()) {
        cfg.kb.top_k = k;
    }

    // 质量门禁（v0.3）：机械验证 + 反思。非法值保持默认（配置桥不做硬失败）
    if let Some(e) = v.gate_narrow.as_deref() {
        cfg.gate.narrow = e == "true" || e == "1";
    }
    if let Some(e) = v.gate_full.as_deref() {
        cfg.gate.full = e == "true" || e == "1";
    }
    if let Some(n) = v
        .gate_max_full_attempts
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok())
    {
        cfg.gate.max_full_attempts = n;
    }
    if let Some(t) = v
        .gate_staged_timeout
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
    {
        cfg.gate.staged_timeout_secs = t;
    }
    if let Some(e) = v.reflect_enabled.as_deref() {
        cfg.reflect.enabled = e == "true" || e == "1";
    }
    if let Some(n) = v
        .reflect_max_rounds
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok())
    {
        cfg.reflect.max_rounds = n;
    }
    if let Some(m) = &v.reflect_model {
        cfg.reflect.model = m.clone();
    }

    // v0.4 计划步骤执行体：默认**开**（引擎自己知道每步跑没跑完，比"声明文件是否落地"
    // 的推断准；推断那条路实测会大面积误判）。父循环一轮 = 一个步骤。
    // 退路仍在：配 `false` 即完全回到旧行为。
    if let Some(e) = v.step_execute_plan.as_deref() {
        cfg.step.execute_plan = e == "true" || e == "1";
    }
    if let Some(n) = v
        .step_max_steps
        .as_deref()
        .and_then(|s| s.parse::<usize>().ok())
    {
        cfg.step.max_steps = n;
    }
    // run 级总时长闸（秒，0 = 不限）
    if let Some(n) = v
        .agent_max_elapsed_secs
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
    {
        cfg.agent.max_elapsed_secs = n;
    }

    // v0.5 命令发现：默认**开**。实测那次 65 轮空转里有 5~6 轮纯粹在试探 `mvn` / `java`
    // 在不在，而引擎早就探过、只是没告诉模型 —— 默认关掉等于把这段空转留着。
    // 探测结果只进上下文、不参与任何判断，所以开关不影响正确性，只影响模型是"知道"还是"去试"。
    if let Some(e) = v.discover_enabled.as_deref() {
        cfg.discover.enabled = e == "true" || e == "1";
    }
    if let Some(n) = v
        .discover_ttl_secs
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
    {
        cfg.discover.ttl_secs = n;
    }
    // `extra` 是「能力长在数据里」的落点：工具表没覆盖的写在这儿，**不用改代码**。
    // 逗号 / 分号 / 空白都能当分隔符 —— 手写配置的人不该被格式绊住。
    if let Some(x) = v.discover_extra.as_deref() {
        cfg.discover.extra = x
            .split([',', ';', '\n', ' ', '\t'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
    }

    // v0.5 环境准备：默认**开**（自成长闭环的最后一环）。关掉后宿主连清单都不摆 env 目标，
    // 模型侧彻底看不见 —— 所以这里只是一行可回退的开关，不涉及任何判定逻辑。
    if let Some(e) = v.env_install_enabled.as_deref() {
        cfg.env.install_enabled = e == "true" || e == "1";
    }

    // v0.6 托管进程：默认**开**。这不是新原语 —— 是 execute 的第三个生命周期维度
    // （前台 / 后台起 + 命令判就绪 / 句柄操作）。关掉后 `background:true` 一律被拒并回说明，
    // 模型会被推回 `start` / `Start-Process` 那套歪招（实测就死在这儿），所以默认开。
    // 三个键都是可解析才生效、非法值保持默认 —— 与上面 discover / env 同款纪律。
    if let Some(e) = v.proc_enabled.as_deref() {
        cfg.proc.enabled = e == "true" || e == "1";
    }
    if let Some(n) = v.proc_max.as_deref().and_then(|s| s.parse::<usize>().ok()) {
        // 上限是**容量**，不是安全边界：真实夹取在 proc::start（1~16），这里只做可回退的旋钮。
        cfg.proc.max = n;
    }
    if let Some(n) = v
        .proc_ready_timeout
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
    {
        cfg.proc.ready_timeout_secs = n;
    }

    // 运行目录：ruyix 默认 ~/.ruyix/code/agent/runs（D5）
    cfg.workspace_root = v
        .workspace_root
        .clone()
        .unwrap_or_else(ruyix_workspace_root);
}

/// 命令层入口：读 ruyix 配置 → 引擎 AppConfig。
/// 末尾沿用引擎的环境变量纪律（`DEEPSEEK_API_KEY` 等优先于文件，key 不落盘，
/// 见 engine config.rs）——脚本/CI 与 GUI 读同一套来源。
pub fn build_app_config(
    mgr: &ConfigManager,
    project_root: Option<&str>,
) -> Result<engine::config::AppConfig, String> {
    let mut cfg = engine::config::AppConfig::default();
    apply_overrides(&mut cfg, &BridgeValues::from_config(mgr, project_root));
    engine::config::apply_env_overrides(&mut cfg);
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> engine::config::AppConfig {
        engine::config::AppConfig::default()
    }

    #[test]
    fn empty_values_yield_ruyix_defaults_not_engine_defaults() {
        let mut cfg = base();
        apply_overrides(&mut cfg, &BridgeValues::default());
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

    #[test]
    fn llm_keys_bridge_from_ai_namespace() {
        let mut cfg = base();
        apply_overrides(
            &mut cfg,
            &BridgeValues {
                api_url: Some("https://api.example.com/v1/chat/completions/".into()),
                api_key: Some("sk-test".into()),
                model: Some("deepseek-chat".into()),
                ..Default::default()
            },
        );
        // 尾斜杠与完整 endpoint 粘贴都被清理（引擎自己拼 /chat/completions）
        assert_eq!(cfg.llm.base_url, "https://api.example.com/v1");
        assert_eq!(cfg.llm.api_key, "sk-test");
        assert_eq!(cfg.llm.model, "deepseek-chat");
    }

    #[test]
    fn numeric_garbage_falls_back_to_defaults() {
        let mut cfg = base();
        let before = (cfg.llm.temperature, cfg.llm.max_tokens, cfg.kb.top_k);
        apply_overrides(
            &mut cfg,
            &BridgeValues {
                llm_temperature: Some("hot".into()),
                llm_max_tokens: Some("-3".into()),
                kb_top_k: Some("many".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            before,
            (cfg.llm.temperature, cfg.llm.max_tokens, cfg.kb.top_k)
        );
    }

    /// v0.4：计划执行开关与总时长闸。默认**开**（引擎事实优先），但 `false` 必须能关回去
    /// —— 这两条断言合起来才是"可回退"的凭据
    #[test]
    fn step_execute_plan_and_elapsed_budget_bridge() {
        let mut cfg = base();
        apply_overrides(&mut cfg, &BridgeValues::default());
        assert!(
            cfg.step.execute_plan,
            "默认必须开：关着的时候进度只能靠推断，实测会大面积误判"
        );
        assert_eq!(cfg.step.max_steps, 24);
        assert_eq!(cfg.agent.max_elapsed_secs, 1800);

        // 一行回退：配 false 就回到"plan 只给用户看"的旧行为
        let mut off = base();
        apply_overrides(
            &mut off,
            &BridgeValues {
                step_execute_plan: Some("false".into()),
                ..Default::default()
            },
        );
        assert!(!off.step.execute_plan, "退路必须真的能关掉");

        apply_overrides(
            &mut cfg,
            &BridgeValues {
                step_execute_plan: Some("true".into()),
                step_max_steps: Some("12".into()),
                agent_max_elapsed_secs: Some("0".into()),
                ..Default::default()
            },
        );
        assert!(cfg.step.execute_plan);
        assert_eq!(cfg.step.max_steps, 12);
        assert_eq!(cfg.agent.max_elapsed_secs, 0, "0 = 不限，是合法值");

        // 非法值保持默认（配置桥不做硬失败）
        let mut cfg2 = base();
        apply_overrides(
            &mut cfg2,
            &BridgeValues {
                step_max_steps: Some("很多".into()),
                agent_max_elapsed_secs: Some("-5".into()),
                ..Default::default()
            },
        );
        assert_eq!(cfg2.step.max_steps, 24);
        assert_eq!(cfg2.agent.max_elapsed_secs, 1800);
    }

    /// v0.5：命令发现默认开，`false` 能关回去；`extra` 的手写格式要容错
    #[test]
    fn discover_bridge_defaults_on_and_parses_the_extra_list() {
        let mut cfg = base();
        apply_overrides(&mut cfg, &BridgeValues::default());
        assert!(
            cfg.discover.enabled,
            "默认必须开：关着的时候模型只能自己试命令在不在，实测会烧掉好几轮"
        );
        assert_eq!(cfg.discover.ttl_secs, 300);
        assert!(cfg.discover.extra.is_empty());

        let mut off = base();
        apply_overrides(
            &mut off,
            &BridgeValues {
                discover_enabled: Some("false".into()),
                ..Default::default()
            },
        );
        assert!(!off.discover.enabled, "退路必须真的能关掉");

        // 逗号 / 分号 / 空白混着写都要能拆开，空项忽略
        let mut cfg2 = base();
        apply_overrides(
            &mut cfg2,
            &BridgeValues {
                discover_ttl_secs: Some("0".into()),
                discover_extra: Some("helm, curl; jq\n terraform  ".into()),
                ..Default::default()
            },
        );
        assert_eq!(cfg2.discover.ttl_secs, 0, "0 = 不缓存，是合法值");
        assert_eq!(
            cfg2.discover.extra,
            vec!["helm", "curl", "jq", "terraform"],
            "分隔符与空白都要容错"
        );

        // 非法值保持默认（配置桥不做硬失败）
        let mut cfg3 = base();
        apply_overrides(
            &mut cfg3,
            &BridgeValues {
                discover_ttl_secs: Some("很久".into()),
                ..Default::default()
            },
        );
        assert_eq!(cfg3.discover.ttl_secs, 300);
    }

    /// v0.5：环境准备（按需安装）默认开，`false` 能关回去
    #[test]
    fn env_install_bridge_defaults_on_and_can_be_turned_off() {
        let mut cfg = base();
        apply_overrides(&mut cfg, &BridgeValues::default());
        assert!(cfg.env.install_enabled, "默认开：这是自成长闭环的最后一环");

        let mut off = base();
        apply_overrides(
            &mut off,
            &BridgeValues {
                env_install_enabled: Some("false".into()),
                ..Default::default()
            },
        );
        assert!(!off.env.install_enabled, "必须能一行回退");

        // "1" 也算开
        let mut on = base();
        apply_overrides(
            &mut on,
            &BridgeValues {
                env_install_enabled: Some("1".into()),
                ..Default::default()
            },
        );
        assert!(on.env.install_enabled);
    }

    /// v0.6：托管进程默认开（关掉会把模型推回 start/Start-Process 歪招），三个键都可回退
    #[test]
    fn proc_bridge_defaults_on_and_each_knob_can_be_overridden() {
        let mut cfg = base();
        apply_overrides(&mut cfg, &BridgeValues::default());
        assert!(cfg.proc.enabled, "默认开：这是永不退出服务模式的唯一出口");
        assert_eq!(cfg.proc.max, 4);
        assert_eq!(cfg.proc.ready_timeout_secs, 60);

        let mut off = base();
        apply_overrides(
            &mut off,
            &BridgeValues {
                proc_enabled: Some("false".into()),
                proc_max: Some("8".into()),
                proc_ready_timeout: Some("120".into()),
                ..Default::default()
            },
        );
        assert!(!off.proc.enabled, "必须能一行回退");
        assert_eq!(off.proc.max, 8);
        assert_eq!(off.proc.ready_timeout_secs, 120);

        // 非法值保持默认，不让一个手滑的数字把面板弄瘫
        let mut bad = base();
        apply_overrides(
            &mut bad,
            &BridgeValues {
                proc_max: Some("很多".into()),
                proc_ready_timeout: Some("-1".into()),
                ..Default::default()
            },
        );
        assert_eq!(bad.proc.max, 4);
        assert_eq!(bad.proc.ready_timeout_secs, 60);
    }

    /// 桥的键名一旦写错，三 scope 都会读空 —— 这里钉住它确实读的是 proc.* 三键
    #[test]
    fn proc_bridge_reads_its_own_keys_from_any_scope() {
        let dir = std::env::temp_dir().join(format!("ruyix_bridge_proc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut mgr = ConfigManager::new_with_dir(dir.clone());

        for (k, v) in [
            ("ruyix.code.harness.proc.enabled", "false"),
            ("ruyix.code.harness.proc.max", "7"),
            ("ruyix.code.harness.proc.ready_timeout_secs", "180"),
        ] {
            mgr.config_write(&Scope::Runtime, k, v, None).unwrap();
        }

        let values = BridgeValues::from_config(&mgr, None);
        let mut cfg = base();
        apply_overrides(&mut cfg, &values);
        assert!(!cfg.proc.enabled, "runtime scope 的 false 必须被读到");
        assert_eq!(cfg.proc.max, 7);
        assert_eq!(cfg.proc.ready_timeout_secs, 180);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sandbox_mode_whitelist_and_lint_rounds() {
        let mut cfg = base();
        apply_overrides(
            &mut cfg,
            &BridgeValues {
                sandbox_mode: Some("require".into()),
                lint_max_repair_rounds: Some("5".into()),
                ..Default::default()
            },
        );
        assert_eq!(cfg.sandbox.mode, "require");
        assert_eq!(cfg.lint.max_repair_rounds, 5);
        apply_overrides(
            &mut cfg,
            &BridgeValues {
                sandbox_mode: Some("yolo".into()),
                ..Default::default()
            },
        );
        assert_eq!(cfg.sandbox.mode, "prefer");
    }

    #[test]
    fn gate_and_reflect_keys_bridge_with_defaults_on_garbage() {
        let mut cfg = base();
        // 默认：门禁与复核都开（v0.3 的默认行为就是"改了就验、交付前复核"）
        assert!(cfg.gate.narrow && cfg.gate.full && cfg.reflect.enabled);
        apply_overrides(
            &mut cfg,
            &BridgeValues {
                gate_narrow: Some("false".into()),
                gate_max_full_attempts: Some("1".into()),
                reflect_max_rounds: Some("3".into()),
                reflect_model: Some("deepseek-v4-pro".into()),
                ..Default::default()
            },
        );
        assert!(!cfg.gate.narrow);
        assert!(cfg.gate.full, "没传的键保持默认");
        assert_eq!(cfg.gate.max_full_attempts, 1);
        assert_eq!(cfg.reflect.max_rounds, 3);
        assert_eq!(cfg.reflect.model, "deepseek-v4-pro");

        apply_overrides(
            &mut cfg,
            &BridgeValues {
                gate_max_full_attempts: Some("many".into()),
                ..Default::default()
            },
        );
        assert_eq!(cfg.gate.max_full_attempts, 1, "非法值保持原值，不硬失败");
    }

    #[test]
    fn build_app_config_honors_env_key_over_files() {
        let dir = std::env::temp_dir().join(format!("ruyix-agent-bridge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = crate::config::ConfigManager::new_with_dir(dir.clone());

        // 无环境变量：未配置态（面板据此走引导视图而非报错）
        // Safety：本测试是本进程内唯一读写该环境变量的测试
        unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
        let cfg = build_app_config(&mgr, None).unwrap();
        assert!(cfg.llm.api_key.is_empty());

        // 环境变量优先于配置文件且不落盘（引擎纪律）
        unsafe { std::env::set_var("DEEPSEEK_API_KEY", "sk-env-bridge") };
        let cfg = build_app_config(&mgr, None).unwrap();
        assert_eq!(cfg.llm.api_key, "sk-env-bridge");
        unsafe { std::env::remove_var("DEEPSEEK_API_KEY") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
