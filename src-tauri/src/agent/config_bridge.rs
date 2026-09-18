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

    // 运行目录：ruyix 默认 ~/.ruyix/code/agent/runs（D5）
    cfg.workspace_root = v
        .workspace_root
        .clone()
        .unwrap_or_else(ruyix_workspace_root);
}

/// 命令层入口：读 ruyix 配置 → 引擎 AppConfig
pub fn build_app_config(
    mgr: &ConfigManager,
    project_root: Option<&str>,
) -> Result<engine::config::AppConfig, String> {
    let mut cfg = engine::config::AppConfig::default();
    apply_overrides(&mut cfg, &BridgeValues::from_config(mgr, project_root));
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
        assert!(cfg.workspace_root.contains(".ruyix/code/agent/runs"));
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
}
