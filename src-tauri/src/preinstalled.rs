//! **预装插件**：把仓库里 `plugins/highlight/ruyix-builtin/` 那两份文件嵌进二进制，
//! 首启**物化**到 `<便携根>/plugins/highlight/ruyix-builtin/`。
//!
//! 为什么是"首启物化"而不是"直接用嵌进去的那份"：
//!
//! 1. **可见、可改**：用户想换配色 / 加一门语言的扩展名，直接编辑那份 `theme.css` /
//!    `plugin.toml` 就行（改完重开）—— 这本来就是插件化的目的；
//! 2. **可删**：删掉那个目录，预装的高亮就没了（等价于在这台机器上用纯净模式的体验）；
//! 3. **幂等且不覆盖**：已经存在的文件**一个字都不动**（用户改过的比我们的默认值重要）。
//!
//! 纯净模式（`--no-default-features`）下这个模块**什么都不做**、也不嵌任何文件 ——
//! 二进制里没有内置解析器、没有预装插件，`plugins/` 保持空。

use std::path::Path;

/// 预装插件的 id（与清单里的 `id` 必须一致）。
///
/// 纯净模式下用不到（不物化），但**它属于这份文件的契约** —— 预装插件叫什么名字
/// 不该随编译模式变，所以留在这里，只把"未使用"这个告警按特性关掉。
#[cfg_attr(not(feature = "preinstalled"), allow(dead_code))]
pub const PLUGIN_ID: &str = "ruyix-builtin";

/// 这份二进制是哪种编译模式。
pub fn mode() -> &'static str {
    if cfg!(feature = "preinstalled") {
        "preinstalled"
    } else {
        "pure"
    }
}

/// 有没有内置解析器（纯净模式没有）。
pub fn builtin_available() -> bool {
    cfg!(feature = "preinstalled")
}

#[cfg(feature = "preinstalled")]
const FILES: &[(&str, &str)] = &[
    (
        "plugin.toml",
        include_str!("../../plugins/highlight/ruyix-builtin/plugin.toml"),
    ),
    (
        "theme.css",
        include_str!("../../plugins/highlight/ruyix-builtin/theme.css"),
    ),
];

/// 把预装插件写进 `plugins/highlight/<PLUGIN_ID>/`；返回这一轮**新写**的文件（已存在的跳过）。
///
/// 幂等、不覆盖：`ensure_layout` 的同一套纪律（用户改过的文件比我们的默认值重要）。
#[cfg(feature = "preinstalled")]
pub fn materialize(plugins_root: &Path) -> Vec<String> {
    let dir = plugins_root
        .join(crate::plugin::PLUGIN_SUBDIR)
        .join(PLUGIN_ID);
    let mut wrote = Vec::new();
    for (name, body) in FILES {
        let to = dir.join(name);
        if to.exists() {
            continue;
        }
        if let Some(parent) = to.parent() {
            // 建不出来就让后面的 write 去失败 —— 多一层判断只为了早点 continue，不值得两行缩进
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&to, body).is_ok() {
            wrote.push(to.display().to_string());
        }
    }
    wrote
}

/// 纯净模式：**什么都不物化**（这是两种编译模式的差异所在，不是漏写）。
#[cfg(not(feature = "preinstalled"))]
pub fn materialize(_plugins_root: &Path) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ruyix-preinstalled-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|x| x.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    #[cfg(feature = "preinstalled")]
    fn materialize_writes_the_plugin_once_and_never_overwrites() {
        let root = tmp("once");
        let first = materialize(&root);
        assert_eq!(first.len(), 2, "预装插件应有两个文件：{first:?}");
        let dir = root.join("highlight").join(PLUGIN_ID);
        assert!(dir.join("plugin.toml").is_file());
        assert!(dir.join("theme.css").is_file());

        // 用户改过 → 再物化**不许**覆盖
        let css = dir.join("theme.css");
        std::fs::write(&css, "/* 用户自己改的 */\n.tok-keyword { color: red; }\n").unwrap();
        let second = materialize(&root);
        assert!(second.is_empty(), "幂等：不该再写任何文件 {second:?}");
        assert!(
            std::fs::read_to_string(&css)
                .unwrap()
                .contains("用户自己改的"),
            "用户改过的主题被覆盖了 —— 那是不可接受的"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 物化出来的那份**必须能通过插件加载器的校验**（否则首启就"预装了但没生效"）。
    #[test]
    #[cfg(feature = "preinstalled")]
    fn materialized_plugin_loads_cleanly() {
        let root = tmp("load");
        materialize(&root);
        // load 收的是**插件根**（= plugins/highlight），物化写在它的子目录里
        let reg =
            crate::plugin::Registry::load(&root.join(crate::plugin::PLUGIN_SUBDIR), None, true);
        assert_eq!(reg.plugins.len(), 1, "{:?}", reg.notes);
        assert_eq!(reg.plugins[0].id, PLUGIN_ID);
        assert_eq!(reg.plugins[0].langs.len(), 8, "{:?}", reg.notes);
        assert!(
            reg.notes.iter().all(|n| !n.contains("丢弃")),
            "{:?}",
            reg.notes
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 纯净模式**必须**什么都不做（这条判据把"两种编译模式"钉在测试里）。
    #[test]
    #[cfg(not(feature = "preinstalled"))]
    fn pure_mode_materializes_nothing() {
        let root = tmp("pure");
        let wrote = materialize(&root);
        assert!(wrote.is_empty(), "纯净模式不该写任何插件文件：{wrote:?}");
        assert!(!root.join("highlight").exists(), "纯净模式不该建插件目录");
        assert_eq!(mode(), "pure");
        assert!(!builtin_available());
        let _ = std::fs::remove_dir_all(&root);
    }
}
