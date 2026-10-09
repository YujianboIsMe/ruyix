//! 知识库：宿主侧"把索引建起来"（v1.5 荒服）。
//!
//! ## 为什么这一步必须有
//!
//! 引擎负责**检索**（`read {scope:"kb"}`），但"语料从哪来、索引什么时候建"是宿主的事：
//! 用户只在配置里写了 `kb.roots`（一个目录列表），而检索要求**索引已经建好**
//! （没有索引时引擎会如实报"这一层没有读数"，而不是装作"知识库里没有"）。
//!
//! ruyix 里没有知识库面板、也没有独立的 CLI 可跑 —— 少这一步，功能就只活在代码里
//! （甸服的教训：**实现了 ≠ 用得上**：用户加了目录却永远搜不到，还没人告诉他能怎么办）。
//! 所以宿主负责：**启动时 + 配置改完**，把"没有索引 / 已经陈旧"的来源在后台建一遍
//! （增量：内容 hash 没变的文件跳过）。
//!
//! ## 三条纪律
//!
//! - **后台线程**：索引要扫语料、开 sqlite，不该让 IDE 启动卡一下；
//! - **失败不打扰**：坏掉的来源留一行日志、跳过，别影响其他来源与整机启动；
//! - **只写索引，不写注册表**：注册表（`kb.json`）是"用户声明了哪些语料"的账，
//!   而 `kb.roots` 来的来源是**配置里声明的**（宿主算 id、不落盘）。索引重建只该
//!   改写 `.db` —— 顺手往 kb.json 里塞条目会让"来源来自哪儿"变得说不清。

use engine::kb::{self, index};
use harness_engine as engine;

/// 后台把索引建起来（同步函数本体；调用方负责放进线程）。
///
/// 返回一行人读的结论（启动日志 / trace 用）。**任何失败都不 panic**：这只是辅助能力。
pub fn ensure_indexes(cfg: &engine::config::AppConfig) -> String {
    if !cfg.kb.enabled {
        return "知识库未启用（harness.kb.enabled = false）—— 跳过建索引".into();
    }
    let eng = kb::Engine::from_config(cfg);
    if eng.sources.is_empty() {
        return "知识库没有来源（配置 harness.kb.roots 可加语料目录）—— 跳过建索引".into();
    }

    let dir = eng.dir.clone();
    let cancel = engine::exec::new_cancel_flag();
    let (mut built, mut fresh) = (0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();

    for src in &eng.sources {
        // 有索引且与语料一致 ⇒ 不碰它（"启动一次就全量重建"是没必要的税）
        let has_db = index::db_path(&dir, &src.id).exists();
        if has_db && !index::quick_stale(&dir, src, &eng.cfg).is_stale() {
            fresh += 1;
            continue;
        }
        match index::index_source(&dir, src, &eng.cfg, &cancel, &mut |_p| {}) {
            Ok(_) => built += 1,
            Err(e) => failures.push(format!("{}（{}）：{e}", src.label, src.path)),
        }
    }

    let mut out = format!(
        "索引 {} 个来源 · {} 个已是最新 · 目录 {}",
        built,
        fresh,
        dir.display()
    );
    if !failures.is_empty() {
        out.push_str(&format!(
            " · 失败 {} 个：{}",
            failures.len(),
            failures.join("；")
        ));
    }
    out
}

/// 放进后台线程的入口（宿主启动 / 配置改完调它）。**不阻塞调用方**。
pub fn spawn_ensure_indexes(cfg: engine::config::AppConfig) {
    std::thread::spawn(move || {
        let msg = ensure_indexes(&cfg);
        // 落两处：stdout（开发时看得见）与引擎的观测流（UI / 日志里查得到）
        println!("[kb] {msg}");
        engine::observe::log("info", "kb", format!("[kb] {msg}"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 关着、没来源、以及"真建一次索引"三条路：都**不许 panic**，且结论要说得清。
    ///
    /// 这条同时是"宿主真会去建索引"的判据 —— 少了它，"用户加了目录却搜不到"这个洞
    /// 没有任何东西会红。
    #[test]
    fn ensure_indexes_builds_a_missing_index_and_is_quiet_otherwise() {
        let root = std::env::temp_dir().join(format!(
            "ruyix-hostkb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let corpus = root.join("corpus");
        let store = root.join("kb");
        std::fs::create_dir_all(&corpus).unwrap();
        std::fs::write(corpus.join("约定.md"), "缓存上限 8G\n").unwrap();

        let mut cfg = engine::config::AppConfig::default();
        cfg.kb.dir = store.to_string_lossy().to_string();
        cfg.kb.roots = vec![corpus.to_string_lossy().to_string()];

        // ① 没来源之外的两种"什么都不做"也要有话说
        let mut off = cfg.clone();
        off.kb.enabled = false;
        assert!(ensure_indexes(&off).contains("未启用"));

        let mut none = cfg.clone();
        none.kb.roots.clear();
        none.kb.dir = root.join("empty-kb").to_string_lossy().to_string();
        assert!(ensure_indexes(&none).contains("没有来源"));

        // ② 真建：第一次建出索引，第二次因为"已是最新"跳过（增量而不是每次重建）
        let first = ensure_indexes(&cfg);
        assert!(
            first.contains("索引 1 个来源"),
            "第一次要把索引建起来：{first}"
        );
        assert!(
            !store.join("kb.json").exists(),
            "宿主建索引**不许**往注册表里塞条目（来源来自配置，账目要分得清）"
        );
        let second = ensure_indexes(&cfg);
        assert!(
            second.contains("1 个已是最新") && second.contains("索引 0 个来源"),
            "索引已经在且不陈旧时不该重建：{second}"
        );

        // ③ 语料变了 ⇒ 陈旧 ⇒ 重建（增量索引会把新文件收进去）
        std::fs::write(corpus.join("新约定.md"), "换出阈值 6G\n").unwrap();
        let third = ensure_indexes(&cfg);
        assert!(third.contains("索引 1 个来源"), "语料变了要重建：{third}");

        let _ = std::fs::remove_dir_all(&root);
    }
}
