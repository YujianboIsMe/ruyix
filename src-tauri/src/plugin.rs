//! 语法高亮的**插件化**（v1.0.0）：把"高亮从哪来"变成可插拔。
//!
//! 三句话讲清形状：
//!
//! 1. **一个插件 = 一个目录**：`<插件根>/highlight/<id>/`，里面是 `plugin.toml`
//!    （语言表 / 扩展名 / 图标 / grammar 来源 / query 覆盖 / token 映射）与 `theme.css`
//!    （**只允许 `.tok-*` 规则**）。
//! 2. **两处来源**：全局 `<便携根>/plugins/highlight/`，项目级
//!    `<便携根>/projects/<项目 key>/plugins/highlight/` —— 项目级按 id 覆盖全局
//!    （沿用 `capability.rs` 的"全局 + 项目、按名合并"惯例）。
//! 3. **载荷契约一个字没改**：插件只影响"片段从哪来 / 叫什么名字 / 什么颜色"，
//!    前端渲染、虚拟化、UTF-16 偏移、既有契约全部照旧 —— 这是插件化能便宜的前提
//!    （见 `doc/v0.x/需求-高亮插件化-v0.14.md` §四）。
//!
//! 信任边界（本版本实现的与**刻意没实现**的）：
//!
//! - **CSS 只收 `.tok-*` 选择器**：逐条校验，混着别的选择器（`.a, .tok-b`）整条丢弃并留痕 ——
//!   否则插件能 `.tab-bar{display:none}` 把 IDE 藏了。
//! - **插件引入的新 token 名必须自带 CSS**：否则那条映射整个丢掉并给出理由（静默无色比报错更坏）。
//! - **动态库（法子 4）与外部服务（法子 5）本版本不开**：它们是"能力最强 + 风险最高"的两条路
//!   （本机代码 = 进程权限；外部服务 = 常驻进程 + 往返）。写进清单也会被**明确拒绝**并留痕，
//!   不留"写了没反应"的哑巴状态。开关与留痕的形状照 `env install` 那条纪律，等真要做时接。
//! - **路径封闭**：`theme` / `highlights` 只允许插件目录内的相对路径，`..` 与绝对路径一律拒绝。

use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// 插件根下的子目录名（全局 `plugins/highlight/` 与项目级 `<桶>/plugins/highlight/` 同名）。
pub const PLUGIN_SUBDIR: &str = "highlight";

/// 插件的清单（`plugin.toml`）。
#[derive(Deserialize, Debug, Clone)]
pub struct PluginManifest {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// 主题文件（相对插件目录）。省略 = 这个插件不提供颜色。
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub lang: Vec<LangSpec>,
}

/// 清单里的一条语言。
#[derive(Deserialize, Debug, Clone)]
pub struct LangSpec {
    pub id: String,
    #[serde(default)]
    pub ext: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// `builtin`（编译进来的解析器）｜`dll:<相对路径>`（未启用）｜`service:<MCP 服务名>`（未启用）
    #[serde(default = "default_grammar")]
    pub grammar: String,
    /// query 覆盖（法子 2）：给了就用它替掉编译进来的 `highlights.scm`
    #[serde(default)]
    pub highlights: Option<String>,
    /// capture 名 → 我们的 token 名（例：`{"function.call" = "function"}`）
    #[serde(default)]
    pub token_map: BTreeMap<String, String>,
}

fn default_grammar() -> String {
    "builtin".into()
}

/// 语言用哪种解析器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrammarKind {
    Builtin,
    /// 未启用的两条路：解析成枚举是为了给出**明确理由**，而不是假装不存在
    Dll(String),
    Service(String),
    /// 清单里写了个不认识的词
    Unknown(String),
}

/// 解析后的语言条目。
#[derive(Debug, Clone)]
pub struct PluginLang {
    pub plugin: String,
    pub id: String,
    pub exts: Vec<String>,
    pub icon: Option<String>,
    pub grammar: GrammarKind,
    /// query 覆盖的**内容**（读进来时就把路径校验掉；文件不存在算加载失败）
    pub highlights: Option<String>,
    pub token_map: BTreeMap<String, String>,
}

/// 一个插件。
#[derive(Debug, Clone)]
pub struct HighlightPlugin {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub dir: PathBuf,
    pub langs: Vec<PluginLang>,
    /// 校验过的主题 CSS（只含 `.tok-*` 规则）
    pub css: String,
}

/// 加载结果：插件表 + 过程里所有"被拒绝/被丢弃"的理由（都会进 JSONL 留痕）。
#[derive(Debug, Default)]
pub struct Registry {
    pub plugins: Vec<HighlightPlugin>,
    pub notes: Vec<String>,
    /// 本程序有没有内置解析器（`preinstalled` 特性）——纯净模式为 false
    pub builtin_available: bool,
}

impl Registry {
    pub fn empty(builtin_available: bool) -> Self {
        Self {
            plugins: Vec::new(),
            notes: Vec::new(),
            builtin_available,
        }
    }

    /// 扫两个插件根并加载。`project` 里的同名插件覆盖全局的（整份覆盖，不合并字段 ——
    /// 字段级合并的规则没人能记住，项目想改一份就拷过去改）。
    pub fn load(
        global_root: &Path,
        project_root: Option<&Path>,
        builtin_available: bool,
    ) -> Registry {
        let mut reg = Registry::empty(builtin_available);
        let mut by_id: HashMap<String, HighlightPlugin> = HashMap::new();
        for (root, tag) in [(Some(global_root), "global"), (project_root, "project")] {
            let Some(root) = root else { continue };
            for dir in list_plugin_dirs(root, &mut reg.notes) {
                match load_one(&dir, &mut reg.notes, builtin_available) {
                    Some(p) => {
                        reg.notes.push(format!(
                            "[{tag}] 载入插件 {}（{} 门语言，{} 字节主题）",
                            p.id,
                            p.langs.len(),
                            p.css.len()
                        ));
                        by_id.insert(p.id.clone(), p);
                    }
                    None => continue,
                }
            }
        }
        let mut plugins: Vec<HighlightPlugin> = by_id.into_values().collect();
        plugins.sort_by(|a, b| a.id.cmp(&b.id));
        reg.plugins = plugins;
        reg
    }

    /// 扩展名 → 语言条目（注册表优先；调用方在没命中时再退回内置探测）。
    pub fn lang_for_ext(&self, ext: &str) -> Option<&PluginLang> {
        let e = ext.to_ascii_lowercase();
        self.plugins
            .iter()
            .flat_map(|p| p.langs.iter())
            .find(|l| l.exts.iter().any(|x| x.eq_ignore_ascii_case(&e)))
    }

    /// 插件是否覆盖了这门语言的 query。
    pub fn highlights_override(&self, lang: &str) -> Option<(&str, &str)> {
        self.plugins
            .iter()
            .flat_map(|p| p.langs.iter())
            .find(|l| l.id.eq_ignore_ascii_case(lang) && l.highlights.is_some())
            .map(|l| (l.plugin.as_str(), l.highlights.as_deref().unwrap_or("")))
    }

    /// capture 名 → token 名（这门语言声明的映射）。
    pub fn token_map_for(&self, lang: &str) -> Option<BTreeMap<String, String>> {
        self.plugins
            .iter()
            .flat_map(|p| p.langs.iter())
            .find(|l| l.id.eq_ignore_ascii_case(lang) && !l.token_map.is_empty())
            .map(|l| l.token_map.clone())
    }

    /// 所有插件主题拼成的一份 CSS（前端注入 `<style>`；空字符串 = 没有插件 ⇒ 纯文本无色）。
    pub fn theme_css(&self) -> String {
        self.plugins
            .iter()
            .filter(|p| !p.css.trim().is_empty())
            .map(|p| format!("/* plugin: {} */\n{}", p.id, p.css.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 插件里出现的**新** token 名（不在内置表里的），转成 `&'static str` 供载荷使用。
    ///
    /// 为什么要 leak：载荷的 `tags` 是 `Vec<&'static str>`（v0.13 为省字节定的形状）。
    /// 插件名在进程生命周期内固定不变、数量有限（每个插件几十个封顶），所以 leak 是**有界**的；
    /// 换 `Vec<String>` 会连带改载荷与 4 条契约，代价远大于收益。这条注释就是那个决定的收据。
    pub fn extra_token_names(&self, builtin: &[&str]) -> Vec<&'static str> {
        let mut seen: Vec<&str> = Vec::new();
        let mut out: Vec<&'static str> = Vec::new();
        for p in &self.plugins {
            for l in &p.langs {
                for name in l.token_map.values() {
                    if builtin.iter().any(|b| b == name) || seen.contains(&name.as_str()) {
                        continue;
                    }
                    seen.push(name.as_str());
                    out.push(Box::leak(name.clone().into_boxed_str()));
                }
            }
        }
        out
    }

    /// 给前端的形状（`highlight_plugins` 命令的返回）。
    pub fn to_json(&self, mode: &str) -> serde_json::Value {
        let langs: Vec<serde_json::Value> = self
            .plugins
            .iter()
            .flat_map(|p| p.langs.iter())
            .map(|l| {
                serde_json::json!({
                    "plugin": l.plugin,
                    "id": l.id,
                    "ext": l.exts,
                    "icon": l.icon,
                    // 语言用的是哪种解析器（前端/面板用它说清"这枚语言从哪来"）
                    "grammar": match &l.grammar {
                        GrammarKind::Builtin => "builtin".to_string(),
                        GrammarKind::Dll(p) => format!("dll:{p}"),
                        GrammarKind::Service(s) => format!("service:{s}"),
                        GrammarKind::Unknown(u) => format!("unknown:{u}"),
                    },
                })
            })
            .collect();
        serde_json::json!({
            "mode": mode,
            "builtin": self.builtin_available,
            "langs": langs,
            "css": self.theme_css(),
            "plugins": self
                .plugins
                .iter()
                .map(|p| serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "version": p.version,
                    "langs": p.langs.len(),
                }))
                .collect::<Vec<_>>(),
            "notes": self.notes,
        })
    }
}

/// 列出插件根下的插件目录（每个 `plugin.toml` 一份）。
fn list_plugin_dirs(root: &Path, notes: &mut Vec<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out; // 目录不存在 = 没有插件（纯净模式的常态，不是错误）
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if p.join("plugin.toml").is_file() {
            out.push(p);
        } else {
            notes.push(format!("{} 下没有 plugin.toml，跳过", p.display()));
        }
    }
    out.sort();
    out
}

/// 加载一个插件目录；失败返回 `None`（理由已写进 `notes`）。
fn load_one(
    dir: &Path,
    notes: &mut Vec<String>,
    builtin_available: bool,
) -> Option<HighlightPlugin> {
    let manifest_path = dir.join("plugin.toml");
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            notes.push(format!("读不到 {}：{e}", manifest_path.display()));
            return None;
        }
    };
    let m: PluginManifest = match toml::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            notes.push(format!("{} 不是合法清单：{e}", manifest_path.display()));
            return None;
        }
    };

    // 主题：路径封闭 + 只收 `.tok-*`
    let mut css = String::new();
    if let Some(theme) = m.theme.as_deref() {
        match screen_rel_path(dir, theme).and_then(|p| {
            std::fs::read_to_string(&p).map_err(|e| format!("读不到主题 {}：{e}", p.display()))
        }) {
            Ok(raw) => {
                let (kept, dropped) = filter_theme_css(&raw);
                for d in &dropped {
                    notes.push(format!("插件 {} 的主题里有一条被丢弃：{d}", m.id));
                }
                css = kept;
            }
            Err(e) => {
                notes.push(format!("插件 {} 的主题不可用：{e}", m.id));
                return None;
            }
        }
    }

    let mut langs: Vec<PluginLang> = Vec::new();
    for spec in &m.lang {
        let grammar = if let Some(rest) = spec.grammar.strip_prefix("dll:") {
            GrammarKind::Dll(rest.to_string())
        } else if let Some(rest) = spec.grammar.strip_prefix("service:") {
            GrammarKind::Service(rest.to_string())
        } else if spec.grammar.eq_ignore_ascii_case("builtin") {
            GrammarKind::Builtin
        } else {
            GrammarKind::Unknown(spec.grammar.clone())
        };

        // 未启用的两条路 / 纯净模式没有内置解析器：**明确拒绝 + 明确理由**，
        // 不留"清单写了、界面没反应"的哑巴状态。
        match &grammar {
            GrammarKind::Dll(_) | GrammarKind::Service(_) => {
                notes.push(format!(
                    "插件 {} 的语言 {} 用了 `{}`：本版本未启用动态库 / 外部服务（信任边界见 \
                     doc/highlight-plugins.md），该语言已跳过",
                    m.id, spec.id, spec.grammar
                ));
                continue;
            }
            GrammarKind::Unknown(g) => {
                notes.push(format!(
                    "插件 {} 的语言 {} 的 grammar=`{g}` 不认识（只认 builtin / dll: / service:），已跳过",
                    m.id, spec.id
                ));
                continue;
            }
            GrammarKind::Builtin if !builtin_available => {
                notes.push(format!(
                    "插件 {} 的语言 {} 声明 grammar=\"builtin\"，但**这枚程序是纯净模式编译的**\
                     （没有内置解析器），该语言已跳过",
                    m.id, spec.id
                ));
                continue;
            }
            GrammarKind::Builtin => {}
        }

        // query 覆盖（可读性优先：给了就必须读得到，读不到算这条语言失败）
        let highlights = match spec.highlights.as_deref() {
            None => None,
            Some(rel) => match screen_rel_path(dir, rel).and_then(|p| {
                std::fs::read_to_string(&p).map_err(|e| format!("读不到 {}：{e}", p.display()))
            }) {
                Ok(t) => Some(t),
                Err(e) => {
                    notes.push(format!("插件 {} 的语言 {}：{e}", m.id, spec.id));
                    continue;
                }
            },
        };

        // 新 token 名必须自带 CSS（否则渲染出来是"没有颜色的 token"，比报错更难查）
        let new_names: Vec<&String> = spec
            .token_map
            .values()
            .filter(|v| !crate::is_builtin_token(v))
            .collect();
        if let Some(missing) = new_names.iter().find(|v| !css_has_token(&css, v)) {
            notes.push(format!(
                "插件 {} 的语言 {} 引入了新 token 名 `{missing}`，但它的主题里没有 `.tok-{}\
                 ` 规则 —— 该语言已跳过（静默无色比报错更难查）",
                m.id, spec.id, missing
            ));
            continue;
        }

        langs.push(PluginLang {
            plugin: m.id.clone(),
            id: spec.id.clone(),
            exts: spec.ext.iter().map(|e| e.to_ascii_lowercase()).collect(),
            icon: spec.icon.clone(),
            grammar,
            highlights,
            token_map: spec.token_map.clone(),
        });
    }

    Some(HighlightPlugin {
        id: m.id.clone(),
        name: m.name.clone().unwrap_or_else(|| m.id.clone()),
        version: m.version.clone(),
        dir: dir.to_path_buf(),
        langs,
        css,
    })
}

/// 主题 CSS 的校验：**只留 `.tok-*` 选择器**的规则。
///
/// 返回 `(留下的 CSS, 被丢弃的理由)`。丢弃的理由要留痕 —— 插件作者得知道自己的主题为什么没生效。
pub fn filter_theme_css(text: &str) -> (String, Vec<String>) {
    let mut kept = String::new();
    let mut dropped = Vec::new();
    // 先去掉注释，免得 `/* .tok-x */` 这种说明被当成规则
    let stripped = strip_css_comments(text);
    for rule in stripped.split('}') {
        let Some((sel, body)) = rule.split_once('{') else {
            continue;
        };
        let sel = sel.trim();
        if sel.is_empty() {
            continue;
        }
        let parts: Vec<&str> = sel.split(',').map(|s| s.trim()).collect();
        if parts.iter().all(|s| is_tok_selector(s)) {
            kept.push_str(&format!("{sel} {{{}\n", body.trim_end()));
        } else {
            dropped.push(format!(
                "选择器 `{}` 不是纯 `.tok-*`（插件不许碰 IDE 自己的样式）",
                sel
            ));
        }
    }
    (kept, dropped)
}

fn strip_css_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i..].find("*/") {
            Some(j) => rest = &rest[i + j + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// `.tok-<名称>`（允许 `:hover` 之类的伪类后缀？**不允许** —— 高亮只需要颜色与字重）。
fn is_tok_selector(sel: &str) -> bool {
    let Some(rest) = sel.strip_prefix(".tok-") else {
        return false;
    };
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 主题里有没有这个 token 名的规则。
fn css_has_token(css: &str, name: &str) -> bool {
    css.contains(&format!(".tok-{name} {{")) || css.contains(&format!(".tok-{name}{{"))
}

/// 路径封闭：只允许插件目录内的相对路径（`..` / 绝对路径 / 盘符一律拒绝）。
fn screen_rel_path(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let r = rel.trim().replace('\\', "/");
    if r.is_empty() {
        return Err("路径为空".into());
    }
    if r.starts_with('/') || r.contains(':') {
        return Err(format!("拒绝绝对路径: {rel}"));
    }
    if r.split('/').any(|seg| seg == "..") {
        return Err(format!("拒绝向上越界路径: {rel}"));
    }
    Ok(dir.join(r))
}

/// 把加载过程写一行 JSONL（谁、哪个目录、结果）—— 与 `env install` 同一套留痕纪律。
///
/// 为什么连**成功**也要写：出问题时第一个要回答的是"这台机器上到底加载了哪些插件"，
/// 而"没加载成功"与"没加载"在界面上长得一样。
pub fn log_records(log_dir: &Path, reg: &Registry, mode: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(log_dir)?;
    let path = log_dir.join("plugins.jsonl");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut lines = vec![format!(
        "{{\"ts\":{ts},\"mode\":\"{mode}\",\"event\":\"load\",\"plugins\":{},\"langs\":{},\"builtin\":{}}}",
        reg.plugins.len(),
        reg.plugins.iter().map(|p| p.langs.len()).sum::<usize>(),
        reg.builtin_available
    )];
    for p in &reg.plugins {
        lines.push(format!(
            "{{\"ts\":{ts},\"event\":\"plugin\",\"id\":\"{}\",\"dir\":\"{}\",\"langs\":{}}}",
            p.id,
            p.dir.display().to_string().replace('\\', "/"),
            p.langs.len()
        ));
    }
    for n in &reg.notes {
        lines.push(format!(
            "{{\"ts\":{ts},\"event\":\"note\",\"text\":{}}}",
            serde_json::to_string(n).unwrap_or_else(|_| "\"\"".into())
        ));
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for l in lines {
        writeln!(f, "{l}")?;
    }
    Ok(path)
}

// ============================================
// 单测
// ============================================

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ruyix-plugin-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|x| x.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 造一个插件目录
    fn write_plugin(root: &Path, id: &str, manifest: &str, theme: Option<&str>) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
        if let Some(t) = theme {
            std::fs::write(dir.join("theme.css"), t).unwrap();
        }
        dir
    }

    const OK_MANIFEST: &str = r#"
id = "demo"
name = "Demo"
theme = "theme.css"

[[lang]]
id = "toml"
ext = ["toml"]
icon = "⚙️"
grammar = "builtin"
"#;

    #[test]
    fn manifest_parses_langs_ext_and_icon() {
        let root = tmp("parse");
        write_plugin(
            &root,
            "demo",
            OK_MANIFEST,
            Some(".tok-keyword { color: #0af; }\n"),
        );
        let reg = Registry::load(&root, None, true);
        assert_eq!(reg.plugins.len(), 1, "{:?}", reg.notes);
        let l = reg
            .lang_for_ext("TOML")
            .expect("扩展名应命中（大小写不敏感）");
        assert_eq!(l.id, "toml");
        assert_eq!(l.icon.as_deref(), Some("⚙️"));
        assert_eq!(l.grammar, GrammarKind::Builtin);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn theme_keeps_only_tok_rules() {
        let css = ".tok-keyword { color: #0af; }\n.tab-bar { display: none; }\n.a, .tok-b { color: red; }\n";
        let (kept, dropped) = filter_theme_css(css);
        assert!(kept.contains(".tok-keyword"), "{kept}");
        assert!(!kept.contains("tab-bar"), "{kept}");
        assert!(!kept.contains(".a,"), "{kept}");
        assert_eq!(dropped.len(), 2, "{dropped:?}");
    }

    #[test]
    fn theme_comments_are_not_rules() {
        let (kept, dropped) =
            filter_theme_css("/* .tok-nope { color: red } */\n.tok-x { color: blue; }\n");
        assert!(kept.contains(".tok-x"), "{kept}");
        assert!(!kept.contains(".tok-nope"), "{kept}");
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    #[test]
    fn path_escape_is_refused() {
        let dir = Path::new("/tmp/x");
        assert!(screen_rel_path(dir, "../evil.css").is_err());
        assert!(screen_rel_path(dir, "/etc/passwd").is_err());
        assert!(screen_rel_path(dir, "C:/windows/x.css").is_err());
        assert!(screen_rel_path(dir, "sub/theme.css").is_ok());
    }

    #[test]
    fn new_token_name_requires_its_own_css() {
        let root = tmp("newtok");
        let m = r#"
id = "demo"
theme = "theme.css"
[[lang]]
id = "toml"
ext = ["toml"]
grammar = "builtin"
token_map = { "property" = "table-key" }
"#;
        // 主题里没有 .tok-table-key ⇒ 这门语言整个跳过，并给出理由
        write_plugin(&root, "demo", m, Some(".tok-keyword { color: #0af; }\n"));
        let reg = Registry::load(&root, None, true);
        assert!(
            reg.plugins[0].langs.is_empty(),
            "{:?}",
            reg.plugins[0].langs
        );
        assert!(
            reg.notes.iter().any(|n| n.contains("table-key")),
            "要有明确理由：{:?}",
            reg.notes
        );
        // 补上规则就能用
        write_plugin(
            &root,
            "demo",
            m,
            Some(".tok-keyword { color: #0af; }\n.tok-table-key { color: #8f8; }\n"),
        );
        let reg = Registry::load(&root, None, true);
        assert_eq!(reg.plugins[0].langs.len(), 1, "{:?}", reg.notes);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn builtin_grammar_is_refused_in_pure_mode() {
        let root = tmp("pure");
        write_plugin(
            &root,
            "demo",
            OK_MANIFEST,
            Some(".tok-x { color: #fff; }\n"),
        );
        let reg = Registry::load(&root, None, false); // ← 纯净模式
        assert!(
            reg.plugins[0].langs.is_empty(),
            "{:?}",
            reg.plugins[0].langs
        );
        assert!(
            reg.notes.iter().any(|n| n.contains("纯净模式")),
            "理由必须点明是纯净模式：{:?}",
            reg.notes
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dll_and_service_are_refused_with_a_reason() {
        let root = tmp("dll");
        let m = r#"
id = "demo"
[[lang]]
id = "toml"
ext = ["toml"]
grammar = "dll:grammars/toml.so"
[[lang]]
id = "yaml"
ext = ["yaml"]
grammar = "service:shiki"
[[lang]]
id = "weird"
ext = ["xyz"]
grammar = "wat"
"#;
        write_plugin(&root, "demo", m, None);
        let reg = Registry::load(&root, None, true);
        assert!(
            reg.plugins[0].langs.is_empty(),
            "{:?}",
            reg.plugins[0].langs
        );
        assert!(
            reg.notes.iter().any(|n| n.contains("未启用")),
            "{:?}",
            reg.notes
        );
        assert!(
            reg.notes.iter().any(|n| n.contains("不认识")),
            "{:?}",
            reg.notes
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_plugins_override_global_by_id() {
        let g = tmp("ovr-g");
        let p = tmp("ovr-p");
        write_plugin(&g, "demo", OK_MANIFEST, Some(".tok-a { color: #111; }\n"));
        let pm = r#"
id = "demo"
theme = "theme.css"
[[lang]]
id = "toml"
ext = ["tmpl"]
icon = "🔧"
grammar = "builtin"
"#;
        write_plugin(&p, "demo", pm, Some(".tok-b { color: #222; }\n"));
        let reg = Registry::load(&g, Some(&p), true);
        assert_eq!(reg.plugins.len(), 1, "同名插件只留项目级那份");
        assert!(reg.lang_for_ext("tmpl").is_some(), "项目级的扩展名应生效");
        assert!(reg.lang_for_ext("toml").is_none(), "全局那份已被整份覆盖");
        let _ = std::fs::remove_dir_all(&g);
        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn extra_token_names_are_deduped_and_exclude_builtin() {
        let root = tmp("extra");
        let m = r#"
id = "demo"
theme = "theme.css"
[[lang]]
id = "toml"
ext = ["toml"]
grammar = "builtin"
token_map = { "a" = "table-key", "b" = "keyword", "c" = "table-key" }
"#;
        write_plugin(&root, "demo", m, Some(".tok-table-key { color: #fff; }\n"));
        let reg = Registry::load(&root, None, true);
        let extra = reg.extra_token_names(&["keyword", "string"]);
        assert_eq!(
            extra,
            vec!["table-key"],
            "内置名不重复 leak、同名只 leak 一次"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn jsonl_records_success_and_notes() {
        let root = tmp("jsonl");
        write_plugin(
            &root,
            "demo",
            OK_MANIFEST,
            Some(".tok-a { color: #fff; }\n"),
        );
        let reg = Registry::load(&root, None, true);
        let log_dir = root.join("logs");
        let p = log_records(&log_dir, &reg, "preinstalled").expect("写留痕");
        let text = std::fs::read_to_string(p).unwrap();
        assert!(text.contains("\"event\":\"load\""), "{text}");
        assert!(text.contains("\"id\":\"demo\""), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 出厂那份预装插件的清单与主题**必须自洽**（这是"语法高亮作为插件"的正面判据）：
    /// 清单能解析、8 门语言都在、扩展名能命中、主题里 27 个内置 token 一个不缺。
    #[test]
    fn preinstalled_plugin_is_self_consistent() {
        let root = tmp("preinstalled");
        write_plugin(
            &root,
            "ruyix-builtin",
            include_str!("../../plugins/highlight/ruyix-builtin/plugin.toml"),
            Some(include_str!(
                "../../plugins/highlight/ruyix-builtin/theme.css"
            )),
        );
        let reg = Registry::load(&root, None, true);
        assert_eq!(reg.plugins.len(), 1, "{:?}", reg.notes);
        let p = &reg.plugins[0];
        assert_eq!(p.id, "ruyix-builtin");
        assert_eq!(p.langs.len(), 8, "预装插件应覆盖编译进来的 8 门语言");
        for ext in ["py", "rs", "html", "css", "js", "md", "sql", "java"] {
            assert!(reg.lang_for_ext(ext).is_some(), "{ext} 应该能高亮");
        }
        for tok in crate::builtin_token_names() {
            assert!(
                css_has_token(&p.css, tok),
                "预装主题里缺 .tok-{tok}（渲染成默认色 = 看起来像高亮丢了）"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
