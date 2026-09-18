//! 无头 CLI：`darkhorse-harness kb <index|list|search|stats|remove>`。
//!
//! 为什么知识库也需要 CLI：**检索质量是要反复调的**（阈值、预算、每源条数），
//! 而"人肉看命中与分数"在 GUI 里点一次要开窗口、点按钮、等渲染；
//! 命令行里 `kb search "预算裁剪"` 一行就能看。调试检索和调试代码一样，
//! 反馈环越短越好。

use super::{index, retrieve, Engine, KbEntry, Registry};
use std::path::{Path, PathBuf};

pub fn kb_cli(args: &[String]) -> i32 {
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        usage();
        return 2;
    };
    let cfg = match crate::config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("读配置失败：{e}");
            return 2;
        }
    };
    let mut engine = Engine::from_config(&cfg);

    match cmd {
        "index" => cmd_index(&mut engine, &args[1..]),
        "list" => cmd_list(&engine),
        "stats" => cmd_stats(&engine, args.iter().any(|a| a == "--json")),
        "search" => cmd_search(&mut engine, &args[1..]),
        "remove" => cmd_remove(&engine, &args[1..]),
        other => {
            eprintln!("未知子命令：{other}");
            usage();
            2
        }
    }
}

fn usage() {
    eprintln!(
        "用法：\n  \
         darkhorse-harness kb index <path>... [--label <名字>]   建/更新索引（增量）\n  \
         darkhorse-harness kb list                               来源列表\n  \
         darkhorse-harness kb stats [--json]                     索引规模与陈旧情况\n  \
         darkhorse-harness kb search <查询> [--top-k N] [--block] [--json]   人肉看命中\n  \
         darkhorse-harness kb remove <id>                        移除来源（连索引一起删）"
    );
}

fn cmd_index(engine: &mut Engine, args: &[String]) -> i32 {
    let mut paths: Vec<String> = Vec::new();
    let mut label = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--label" => {
                if i + 1 < args.len() {
                    label = args[i + 1].clone();
                    i += 1;
                }
            }
            other => paths.push(other.to_string()),
        }
        i += 1;
    }
    if paths.is_empty() {
        eprintln!("请给至少一个目录：kb index <path>...");
        return 2;
    }
    let mut reg = match Registry::load(&engine.dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let flag: crate::exec::CancelFlag = crate::exec::new_cancel_flag();
    let mut code = 0;
    for raw in paths {
        let p = Path::new(raw.trim().trim_matches('"'));
        if !p.is_dir() {
            eprintln!("跳过（不是目录）：{}", p.display());
            code = 1;
            continue;
        }
        let canon = super::tidy_path(&std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
        let prev = reg.find_by_path(&canon).cloned();
        let entry = KbEntry {
            id: super::entry_id("local", &canon),
            kind: "local".into(),
            path: canon.to_string_lossy().to_string(),
            label: if label.is_empty() {
                prev.as_ref()
                    .map(|e| e.label.clone())
                    .unwrap_or_else(|| {
                        canon
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| canon.to_string_lossy().to_string())
                    })
            } else {
                label.clone()
            },
            added_at: crate::workspace::now_iso(),
            ..Default::default()
        };
        println!("索引 {} …", canon.display());
        let mut last = String::new();
        let mut progress = |p: index::Progress| {
            if p.phase == "scan" {
                println!("  扫描：{}", p.msg);
            } else if p.phase == "done" {
                last = p.msg;
            } else if p.done % 25 == 0 || p.done == p.total {
                println!("  [{}/{}] {}", p.done, p.total, p.msg);
            }
        };
        match index::index_source(&engine.dir, &entry, &engine.cfg, &flag, &mut progress) {
            Ok(st) => {
                for (path, why) in st.skipped_files.iter().take(5) {
                    println!("  跳过 {path}：{why}");
                }
                println!("  {}", if last.is_empty() { st.summary() } else { last });
                let mut e2 = entry.clone();
                e2.doc_count = st.doc_count;
                e2.chunk_count = st.chunk_count;
                e2.indexed_at = st.indexed_at.clone();
                e2.index_files = st.scanned - st.skipped_files.len();
                e2.skipped_files = st.skipped;
                e2.elapsed_ms = st.elapsed_ms;
                if let Err(e) = reg.upsert(e2, &engine.dir) {
                    eprintln!("写注册表失败：{e}");
                    code = 1;
                }
            }
            Err(e) => {
                eprintln!("索引失败：{e}");
                code = 1;
            }
        }
    }
    *engine = Engine::from_config(&crate::config::load().unwrap_or_default());
    code
}

fn cmd_list(engine: &Engine) -> i32 {
    if engine.sources.is_empty() {
        println!("（还没有添加任何知识库来源）");
        return 0;
    }
    for e in &engine.sources {
        let st = index::stats(&engine.dir, &e.id).unwrap_or_default();
        println!(
            "{}  [{}] {} · {} 篇 / {} chunk · 最近索引 {}",
            e.id,
            e.kind,
            e.path,
            st.doc_count,
            st.chunk_count,
            if st.indexed_at.is_empty() {
                "从未".to_string()
            } else {
                st.indexed_at.clone()
            }
        );
    }
    0
}

fn cmd_stats(engine: &Engine, json: bool) -> i32 {
    let status = engine.status(true);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into())
        );
        return 0;
    }
    println!(
        "知识库目录：{}（{}）",
        status.dir,
        if status.enabled { "已启用" } else { "未启用" }
    );
    if !status.reason.is_empty() {
        println!("状态：{}", status.reason);
    }
    println!(
        "预算 {} 字符 · top_k {} · 每源最多 {} 条 · 最低分 {:.2}",
        status.token_budget, status.top_k, status.per_source_limit, status.min_score
    );
    if status.sources.is_empty() {
        println!("（还没有添加任何知识库来源）");
        return 0;
    }
    for s in &status.sources {
        println!(
            "- [{}] {} · {} 篇 / {} chunk · 最近索引 {} · {}",
            s.kind,
            s.path,
            s.doc_count,
            s.chunk_count,
            if s.indexed_at.is_empty() {
                "从未".to_string()
            } else {
                s.indexed_at.clone()
            },
            s.stale.summary()
        );
        for d in &s.stale.detail {
            println!("    · {d}");
        }
        // 磁盘上"应该有几个文件"：索引规模对不对，先看这个数
        let e = KbEntry {
            id: s.id.clone(),
            kind: s.kind.clone(),
            path: s.path.clone(),
            label: s.label.clone(),
            ..Default::default()
        };
        match index::candidate_count(&e, &engine.cfg) {
            Ok(n) => println!("    · 磁盘上可选文件 {n} 个（索引里 {} 篇）", s.doc_count),
            Err(err) => println!("    · 无法扫描来源：{err}"),
        }
    }
    0
}

fn cmd_search(engine: &mut Engine, args: &[String]) -> i32 {
    let mut query = String::new();
    let mut top_k: Option<usize> = None;
    let mut block = false;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--top-k" => {
                if i + 1 < args.len() {
                    top_k = args[i + 1].parse().ok();
                    i += 1;
                }
            }
            "--block" => block = true,
            "--json" => json = true,
            other => query = other.to_string(),
        }
        i += 1;
    }
    if query.trim().is_empty() {
        eprintln!("用法：kb search <查询> [--top-k N] [--block] [--json]");
        return 2;
    }
    // 调试模式：开关关着也照查（否则"检索效果怎么样"没法在不改配置的情况下验）
    if !engine.enabled {
        eprintln!("（注意：config 里 [kb] enabled = false —— 本次按调试模式检索，真实运行不会注入）");
        engine.enabled = true;
        // 调试模式下把"未启用"这条原因清掉，否则命中列表与说明自相矛盾
        engine.reason.clear();
    }
    if engine.sources.is_empty() {
        eprintln!("还没有添加任何知识库来源（kb index <path> 或 GUI 里添加）");
        return 1;
    }
    if let Some(k) = top_k {
        engine.cfg.top_k = k.max(1);
    }
    let inj = retrieve::retrieve(engine, "search", &query, &retrieve::Workspace::default());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&inj).unwrap_or_else(|_| "{}".into())
        );
        return 0;
    }
    println!("查询：{}", query);
    println!(
        "策略 {} · 命中 {} 条 · 注入 {} / {} 字符",
        inj.strategy,
        inj.hits.len(),
        inj.injected_chars,
        inj.budget_chars
    );
    if !inj.reason.is_empty() {
        println!("说明：{}", inj.reason);
    }
    for (i, h) in inj.hits.iter().enumerate() {
        println!(
            "\n{}. [{trust}] {path}#{ord} · 分数 {score:.2}（覆盖 {cov:.2}）· {label} · 索引 {at}",
            i + 1,
            trust = h.trust,
            path = h.path,
            ord = h.ordinal,
            score = h.score,
            cov = h.coverage,
            label = h.source_label,
            at = if h.indexed_at.is_empty() { "未知" } else { &h.indexed_at }
        );
        println!("   {}", h.title);
    }
    if !inj.dropped.is_empty() {
        println!("\n被裁掉 {} 条：", inj.dropped.len());
        for d in inj.dropped.iter().take(20) {
            println!("  - {}：{}（{}）", d.path, d.reason, d.detail);
        }
    }
    for n in &inj.notes {
        println!("  ! {n}");
    }
    if block {
        println!("\n===== 注入块原文 =====\n{}", retrieve::render_block(&inj).unwrap_or_default());
    }
    0
}

fn cmd_remove(engine: &Engine, args: &[String]) -> i32 {
    let Some(id) = args.first() else {
        eprintln!("用法：kb remove <id>（id 用 kb list 查）");
        return 2;
    };
    let mut reg = match Registry::load(&engine.dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match reg.remove(id, &engine.dir) {
        Ok(Some(_)) => {
            let db: PathBuf = index::db_path(&engine.dir, id);
            if db.exists() {
                if let Err(e) = std::fs::remove_file(&db) {
                    eprintln!("已从列表移除，但索引文件删除失败：{e}（{}）", db.display());
                    return 1;
                }
            }
            println!("已移除 {id}");
            0
        }
        Ok(None) => {
            eprintln!("找不到来源：{id}");
            1
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
