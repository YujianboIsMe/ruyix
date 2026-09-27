//! 模型厂商对象（宿主 runtime 维护）。
//!
//! 会话工具栏里"当前模型 / 可用模型 / 联网能力"这一组派生状态，过去是**每次配置变化**
//! 都拿配置重新解析一遍。于是拧一下 🌏（写 `harness.llm.web_search`）也会触发重取，
//! 用配置里的默认模型把用户在会话里刚选的那一个画回去 —— **事件回环**
//! （用户实测：toggle 联网就跳回 pro）。
//!
//! 现在把"这是哪一家厂商"抽成一个**对象**，由宿主 runtime 维护：
//!
//! * **身份** = 端点 + 密钥 + 协议（主用 / 备用各一组）。三者任一变化才算**换厂商** ——
//!   "能列出哪些模型、能不能联网"只依赖它；
//! * 换厂商才广播 `model://vendor-changed`，会话面板只认这一个事件；
//! * 对象带一个 `generation`：每换一次 `+1`，前端拿它判断手里的清单是不是这一版。
//!
//! **密钥不进这个对象**：只留一个进程内比较用的摘要（`key_digest`）与"配没配"
//! （`has_key`）。明文既不留在对象上、也绝不序列化给前端或落日志。

use crate::config::ConfigManager;
use harness_engine::config::{AppConfig, LlmConfig};

/// 一组端点身份（主用或备用）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    /// 端点（`config_bridge` 已清理过尾斜杠 / 完整 endpoint 后缀）
    base_url: String,
    /// 协议（openai / anthropic）
    api_format: String,
    /// 密钥配没配（**不外传密钥本身**）
    has_key: bool,
    /// 密钥摘要：只用来在进程内比"换没换钥匙"，碰撞概率可忽略
    key_digest: u64,
}

impl Endpoint {
    fn from_llm(llm: &LlmConfig) -> Self {
        Self {
            base_url: llm.base_url.clone(),
            api_format: llm.api_format.clone(),
            has_key: !llm.api_key.trim().is_empty(),
            key_digest: digest(llm.api_key.trim()),
        }
    }
}

/// 字符串 → u64 摘要（`DefaultHasher`；只作进程内等值比较，不做安全用途）
fn digest(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 宿主 runtime 里维护的模型厂商对象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVendor {
    primary: Endpoint,
    fallback: Option<Endpoint>,
    /// 身份版本：每换一次厂商 `+1`（前端用它丢弃过期的异步回执）
    generation: u64,
}

/// 给前端的厂商快照（**不含密钥**）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VendorSnapshot {
    pub base_url: String,
    pub api_format: String,
    pub has_key: bool,
    pub has_fallback: bool,
    pub generation: u64,
}

impl Default for ModelVendor {
    fn default() -> Self {
        Self::from_app(&AppConfig::default())
    }
}

impl ModelVendor {
    /// 从引擎配置解析厂商身份。
    ///
    /// 用 `AppConfig` 而不是逐个读键：身份必须与**引擎实际要用的端点**同源 ——
    /// 尾斜杠清理、`api_format` 默认值、环境变量覆盖都在 `config_bridge` 里做过，
    /// 这里不抄第二份（抄一份就是"界面说的厂商"与"请求打到的厂商"各说各话）。
    pub fn from_app(cfg: &AppConfig) -> Self {
        Self {
            primary: Endpoint::from_llm(&cfg.llm),
            fallback: cfg.llm_fallback.as_ref().map(Endpoint::from_llm),
            generation: 0,
        }
    }

    /// 从配置管理器读当前身份（读不出配置 = `None`，调用方决定怎么兜底）。
    pub fn read(mgr: &ConfigManager, project_root: Option<&str>) -> Option<Self> {
        crate::agent::config_bridge::build_app_config(mgr, project_root)
            .ok()
            .map(|cfg| Self::from_app(&cfg))
    }

    /// 是不是**同一家厂商**（`generation` 不参与比较）。
    ///
    /// 模型名**不在身份里**：换模型不是换厂商，否则选一次模型就触发厂商事件，回环又回来了。
    pub fn same_vendor(&self, other: &Self) -> bool {
        self.primary == other.primary && self.fallback == other.fallback
    }

    /// 承接新身份：版本 `+1`（`generation` 只在这里涨）。
    pub fn adopt(&mut self, next: Self) {
        let generation = self.generation + 1;
        *self = next;
        self.generation = generation;
    }

    pub fn snapshot(&self) -> VendorSnapshot {
        VendorSnapshot {
            base_url: self.primary.base_url.clone(),
            api_format: self.primary.api_format.clone(),
            has_key: self.primary.has_key,
            has_fallback: self.fallback.is_some(),
            generation: self.generation,
        }
    }
}

// ============================================
// 测试
// ============================================

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str, key: &str, fmt: &str) -> AppConfig {
        let mut c = AppConfig::default();
        c.llm.base_url = url.into();
        c.llm.api_key = key.into();
        c.llm.api_format = fmt.into();
        c
    }

    fn vendor(url: &str, key: &str, fmt: &str) -> ModelVendor {
        ModelVendor::from_app(&cfg(url, key, fmt))
    }

    /// 身份只由**端点 / 密钥 / 协议**决定：三者任一变化都算换厂商。
    #[test]
    fn identity_is_endpoint_key_and_protocol() {
        let base = vendor("https://api.deepseek.com", "sk-1", "openai");
        assert!(base.same_vendor(&vendor("https://api.deepseek.com", "sk-1", "openai")));
        assert!(
            !base.same_vendor(&vendor("https://other.example", "sk-1", "openai")),
            "换端点 = 换厂商"
        );
        assert!(
            !base.same_vendor(&vendor("https://api.deepseek.com", "sk-2", "openai")),
            "换密钥 = 换厂商（同一端点换了钥匙，能用的模型可能不同）"
        );
        assert!(
            !base.same_vendor(&vendor("https://api.deepseek.com", "sk-1", "anthropic")),
            "换协议 = 换厂商（联网能力是按协议算的）"
        );
    }

    /// 模型名**不是**身份的一部分 —— 这条是"选模型不触发厂商事件"的判据（回环的根）。
    #[test]
    fn model_name_is_not_part_of_the_identity() {
        let mut a = cfg("https://api.deepseek.com", "sk-1", "openai");
        let mut b = a.clone();
        a.llm.model = "deepseek-v4-pro".into();
        b.llm.model = "deepseek-flash".into();
        assert!(
            ModelVendor::from_app(&a).same_vendor(&ModelVendor::from_app(&b)),
            "换模型不许被当成换厂商（否则会拿配置里的默认把刚选的画回去）"
        );
    }

    /// 配了 / 换了备用端点的三个字段也算一次厂商变化（备用也是厂商配置的一部分）。
    #[test]
    fn fallback_llm_counts_toward_identity() {
        let base = vendor("https://api.deepseek.com", "sk-1", "openai");
        let mut with_fb = cfg("https://api.deepseek.com", "sk-1", "openai");
        with_fb.llm_fallback = Some(LlmConfig {
            base_url: "https://backup.example".into(),
            api_key: "sk-backup".into(),
            api_format: "anthropic".into(),
            ..LlmConfig::default()
        });
        assert!(
            !base.same_vendor(&ModelVendor::from_app(&with_fb)),
            "配了备用端点 = 厂商配置变了"
        );
    }

    /// `adopt` 承接新身份并让版本 +1；快照**不许**带密钥。
    #[test]
    fn adopt_bumps_generation_and_snapshot_hides_the_key() {
        let mut v = vendor("https://api.deepseek.com", "sk-secret", "openai");
        assert_eq!(v.snapshot().generation, 0, "初始版本从 0 起");
        assert!(v.snapshot().has_key);

        v.adopt(vendor("https://api.deepseek.com", "sk-rotated", "openai"));
        assert_eq!(v.snapshot().generation, 1, "换一次厂商版本 +1");

        let json = serde_json::to_string(&v.snapshot()).unwrap();
        assert!(!json.contains("sk-"), "快照序列化不许带密钥：{json}");
        assert!(!json.contains("digest"), "摘要也不外传：{json}");
    }

    /// 空密钥 → `has_key = false`（空白也算没配）。
    #[test]
    fn blank_key_reports_no_key() {
        assert!(!vendor("https://api.deepseek.com", "   ", "openai").snapshot().has_key);
        // 同一份空密钥的两份身份必须相等 —— 摘要对空白取值要稳定
        assert!(
            vendor("https://api.deepseek.com", "", "openai")
                .same_vendor(&vendor("https://api.deepseek.com", "   ", "openai"))
        );
    }
}
