//! 分层检索（v1.5「五服」）—— 判据与 `doc/v1.5/需求-分层搜索-五服-v1.5.md` §6 一一对应。
//!
//! 夹具（`TempDir` / `parse_action` …）在父模块 `tests.rs`，这里 `use super::*` 取。

use super::*;

fn spec(scope: SearchScope, q: &str) -> SearchSpec {
    SearchSpec {
        scope,
        q: q.to_string(),
        path: None,
        max_hits: None,
    }
}

/// 整棵树的指纹（路径 + 类型 + 字节数）。"一个字节都不写"这条判据靠它 ——
/// 比 stderr/日志可靠：**盘上没变**才是没写。
fn tree_fingerprint(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            match std::fs::metadata(&p) {
                Ok(m) if m.is_dir() => {
                    out.push(format!("d {rel}"));
                    stack.push(p);
                }
                Ok(m) => out.push(format!("f {rel} {}", m.len())),
                Err(_) => {}
            }
        }
    }
    out.sort();
    out
}

/// **形状**：`read` 的两副面孔收口在一处 —— 检索解析成 `Search`，老形状一字不动。
#[test]
fn read_parses_both_shapes() {
    assert!(matches!(
        parse_action(r#"{"tool":"read","args":{"scope":"files","q":"needle"}}"#),
        Ok(Action::Search(s)) if s.scope == SearchScope::Files && s.q == "needle" && s.path.is_none()
    ));
    // files 层允许限定子树（过 jail）
    assert!(matches!(
        parse_action(r#"{"tool":"read","args":{"scope":"files","q":"n","path":"src/","max_hits":5}}"#),
        Ok(Action::Search(s)) if s.path.as_deref() == Some("src/") && s.max_hits == Some(5)
    ));
    // 记忆层不接受 path
    let e = parse_action(r#"{"tool":"read","args":{"scope":"project_mem","q":"n","path":"src/"}}"#)
        .unwrap_err();
    assert!(e.contains("不接受 path"), "{e}");
    // 老形状（文件窗口读）逐字不变
    assert!(matches!(
        parse_action(r#"{"tool":"read","args":{"path":"a.rs","offset":3,"limit":9}}"#),
        Ok(Action::Read(s)) if s.path == "a.rs" && s.offset == Some(3) && s.limit == Some(9)
    ));
}

/// **判据 3**：每种拒绝都指名理由（缺 scope / 缺 q / 未知层 / 与窗口参数混用）。
#[test]
fn search_rejections_name_the_reason() {
    let cases = [
        (
            r#"{"tool":"read","args":{"q":"n"}}"#,
            "没有 scope",
            "缺 scope",
        ),
        (
            r#"{"tool":"read","args":{"scope":"files"}}"#,
            "没有配 q",
            "缺 q",
        ),
        (
            r#"{"tool":"read","args":{"scope":"os","q":"n"}}"#,
            "未知 scope",
            "未知层（含「全盘搜索」这种没开的口）",
        ),
        (
            r#"{"tool":"read","args":{"scope":"files","q":"n","limit":10}}"#,
            "不接受 offset/limit",
            "与窗口读参数混用",
        ),
    ];
    for (raw, want, why) in cases {
        let e = parse_action(raw).unwrap_err();
        assert!(e.contains(want), "{why}：{e}");
    }
    // 未知层要把**开了哪四层**说清楚（不然模型只能再猜一次）
    let e = parse_action(r#"{"tool":"read","args":{"scope":"kb","q":"n"}}"#).unwrap_err();
    assert!(
        e.contains("session")
            && e.contains("project_mem")
            && e.contains("global_mem")
            && e.contains("files"),
        "未知层要给可用层清单：{e}"
    );
}

/// **判据 2 + 判据 4 的正例**：文件层检索真的找到东西（带行号），
/// 而且**一个字节都不写** —— 项目树指纹跑前跑后一致。
#[test]
fn files_search_finds_hits_and_never_writes() {
    let d = TempDir::new("search-files");
    d.write("src/a.rs", "fn main() {\n    let needle = 1;\n}\n");
    d.write("README.md", "这里也有 needle 一次\n");
    d.write("target/gen.rs", "needle（生成物，默认不搜）\n");
    let before = tree_fingerprint(&d.0);

    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply);
    let out = crate::agent::search::run(&mut ctx, &spec(SearchScope::Files, "needle")).unwrap();

    assert!(out.contains("**项目文件**"), "{out}");
    assert!(out.contains("src/a.rs:2:"), "命中要带 path:line：{out}");
    assert!(out.contains("README.md:1:"), "{out}");
    assert!(!out.contains("target/gen.rs"), "生成物目录默认不搜：{out}");
    assert!(out.contains("已跳过目录"), "跳过了什么必须写出来：{out}");
    assert!(
        out.contains("磁盘上现在写的"),
        "文件层的权威说明要在：{out}"
    );

    assert_eq!(
        before,
        tree_fingerprint(&d.0),
        "检索不许写盘（一个字节都不许）"
    );
    assert!(
        !ctx.state_root().join("stage").exists() && !ctx.state_root().join("backups").exists(),
        "检索不该产生暂存或备份"
    );
}

/// **判据 6**：没命中是**合法答案**，且必须说清"在哪一层、用什么词找的、什么范围"。
#[test]
fn a_miss_is_a_legitimate_answer() {
    let d = TempDir::new("search-miss");
    d.write("a.txt", "内容\n");
    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply);
    let out = crate::agent::search::run(&mut ctx, &spec(SearchScope::Files, "不存在的词")).unwrap();
    assert!(out.contains("没有出现这个词"), "{out}");
    assert!(out.contains("命中 0 条"), "{out}");
}

/// **判据 4**：准入在**入口**就拦住 —— `..` / 绝对路径 / `.git` 全拒，不给"多试一次"。
#[test]
fn the_files_scope_subpath_is_jailed() {
    let d = TempDir::new("search-jail");
    d.write("a.txt", "x\n");
    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply);
    for bad in ["../..", "..", "D:/Windows", "/etc", ".git"] {
        let mut s = spec(SearchScope::Files, "x");
        s.path = Some(bad.to_string());
        let got = crate::agent::search::run(&mut ctx, &s);
        assert!(got.is_err(), "`{bad}` 必须被拒：{got:?}");
    }
    // 正例：正常子树放行
    d.write("src/b.txt", "x\n");
    let mut s = spec(SearchScope::Files, "x");
    s.path = Some("src".into());
    assert!(crate::agent::search::run(&mut ctx, &s).is_ok());
}

/// **第 2/3 服**：记忆库不可用时**明说**（"库不可用" ≠ "没有记忆"）。
#[test]
fn a_missing_memory_store_is_an_error_not_an_empty_answer() {
    let d = TempDir::new("search-mem");
    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply);
    for scope in [SearchScope::ProjectMem, SearchScope::GlobalMem] {
        let e = crate::agent::search::run(&mut ctx, &spec(scope, "x")).unwrap_err();
        assert!(e.contains("记忆库没安装"), "{scope:?}: {e}");
        assert!(e.contains("没有读数"), "要说清这是没读数而不是没内容：{e}");
    }
}

/// **第 1 服**：没有侧存（headless / eval）时也是明说，不是"空的"。
#[test]
fn the_session_scope_needs_a_capsule() {
    let d = TempDir::new("search-session");
    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply);
    let e = crate::agent::search::run(&mut ctx, &spec(SearchScope::Session, "x")).unwrap_err();
    assert!(e.contains("侧存"), "{e}");
    assert!(e.contains("没有读数"), "{e}");
}

/// **判据 1 的机制层**：检索在账本里是**一等纯工具** ——
/// 同 scope 同词（归一后）是同一次调用，且 `read` 的区间包含关系**不适用**于它。
#[test]
fn the_ledger_treats_a_search_as_a_first_class_pure_call() {
    let a = spec(SearchScope::Files, "  Foo   Bar ");
    let b = spec(SearchScope::Files, "foo bar");
    let ca = keys::classify(&Action::Search(a)).expect("检索必须参与去重");
    let cb = keys::classify(&Action::Search(b)).expect("检索必须参与去重");
    assert_eq!(ca.tool, keys::Tool::Search);
    assert_eq!(ca.tool.name(), "search");
    assert_eq!(ca.norm, cb.norm, "空白与大小写归一后是同一次检索");
    assert_eq!(ca.digest, cb.digest);
    assert!(
        ca.span.is_none(),
        "检索之间没有包含关系（搜过 a.rs 不蕴含搜过某个词）"
    );

    // scope 不同 ⇒ 不是同一次调用（层与层的权威级别不同）
    let g = keys::classify(&Action::Search(spec(SearchScope::GlobalMem, "foo bar"))).unwrap();
    assert_ne!(ca.norm, g.norm);

    // 限定路径也归一（`./src` 与 `src` 是同一次）
    let mut p1 = spec(SearchScope::Files, "q");
    p1.path = Some("./src".into());
    let mut p2 = spec(SearchScope::Files, "q");
    p2.path = Some("src".into());
    let c1 = keys::classify(&Action::Search(p1)).unwrap();
    let c2 = keys::classify(&Action::Search(p2)).unwrap();
    assert_eq!(c1.norm, c2.norm);
}

/// **判据 1 的版本层**：拿不到版本 ⇒ **每次真执行**（fail-safe）。
/// 记忆库不在 / 侧存没挂 ⇒ `unknown`；文件层不额外盖章（走仓库版本）。
#[test]
fn a_search_without_a_version_source_always_executes() {
    let d = TempDir::new("search-stamp");
    let ctx = Ctx::new(&d.0, WritePolicy::Apply);

    let mut v = ledger::VersionVec::default();
    crate::agent::search::stamp_version(&mut v, &spec(SearchScope::ProjectMem, "x"), &ctx);
    assert!(v.unknown, "记忆库不在 ⇒ 版本不可判 ⇒ 宁可多跑一次");

    let mut v2 = ledger::VersionVec::default();
    crate::agent::search::stamp_version(&mut v2, &spec(SearchScope::Session, "x"), &ctx);
    assert!(v2.unknown, "侧存没挂 ⇒ 版本不可判");

    // 文件层：不盖状态版本，交给 `snapshot` 的仓库那一路（非 git ⇒ snapshot 自己判 unknown）
    let mut v3 = ledger::VersionVec::default();
    crate::agent::search::stamp_version(&mut v3, &spec(SearchScope::Files, "x"), &ctx);
    assert!(!v3.unknown, "文件层不该在这里下结论（snapshot 管）");
}

/// **子步**：检索是只读 ⇒ 允许（不是 `Unsupported`）；plan / connect 仍然打回。
#[test]
fn a_step_may_search_but_not_plan_or_connect() {
    assert!(matches!(
        to_step_action(Action::Search(spec(SearchScope::Files, "x"))),
        StepAction::Search(_)
    ));
    assert!(matches!(
        to_step_action(Action::Plan(vec![])),
        StepAction::Unsupported("plan")
    ));
}

/// 工具名必须是 **read**：批上限、审计计数、"读类动作"的账全按工具名对账。
#[test]
fn a_search_counts_as_the_read_tool() {
    assert_eq!(
        action_name(&Action::Search(spec(SearchScope::Files, "x"))),
        "read"
    );
    let label = call_shape(&[Action::Search(spec(SearchScope::ProjectMem, "约定"))]);
    assert!(label.contains("read search project_mem"), "{label}");
}

/// **端到端（脚本化 LLM，走真循环）**：模型发一次检索 ⇒ 命中进上下文（带行号）⇒ 交付。
/// 这条断的是**接线**：形状解析 → 波次 → 执行 → 工具结果回灌，一处断了都过不去。
#[test]
fn the_loop_runs_a_search_and_hands_the_hits_back() {
    let d = TempDir::new("search-loop");
    d.write("a.txt", "内容-MARK-1\n");
    d.write("b.txt", "无关内容\n");
    let cfg = quiet_cfg();
    let (llm, _outcome, log) = block_on(run_logging(
        &cfg,
        &d.0,
        vec![
            r#"{"tool":"read","args":{"scope":"files","q":"MARK-1"}}"#.into(),
            r#"{"final":"搜到了"}"#.into(),
        ],
    ));
    assert_eq!(llm.count(), 2, "一次检索 + 一次交付");
    let req = llm.request(1);
    assert!(req.contains("a.txt:1:"), "命中要回灌进上下文：{req}");
    assert!(!req.contains("b.txt:1:"), "没命中的文件不该出现：{req}");
    assert!(req.contains("项目文件"), "结果块要点名层：{req}");
    assert!(
        log.contains("read search files"),
        "trace 里要能看出搜的是哪一层、哪个词：{log}"
    );
}

/// **判据 1（端到端，文件层）**：同一层同一个词的第二次检索**不执行** —— 走账本命中。
///
/// 文件层的版本源是**仓库版本**（`git HEAD + 工作树脏否`），所以要先有个仓库；
/// 环境里没有可用的 git 就**大声跳过**（静默"绿"等于没测）。
#[test]
fn a_repeated_files_search_is_served_from_the_ledger() {
    let d = TempDir::new("search-dedup-git");
    d.write("a.txt", "内容-MARK-1\n");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&d.0)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let ready = git(&["init", "-q"])
        && git(&["add", "."])
        && git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ]);
    if !ready {
        eprintln!("[skip] 环境没有可用的 git —— 文件层的版本源就是仓库版本，这条跳过");
        return;
    }

    let mut cfg = quiet_cfg();
    cfg.agent.ctx.dedup = true;
    let (llm, _o, log) = block_on(run_logging(
        &cfg,
        &d.0,
        vec![
            r#"{"tool":"read","args":{"scope":"files","q":"MARK-1"}}"#.into(),
            r#"{"tool":"read","args":{"scope":"files","q":"MARK-1"}}"#.into(),
            r#"{"final":"搜了两遍"}"#.into(),
        ],
    ));
    assert_eq!(llm.count(), 3, "去重省的是执行，不是模型往返");
    assert!(
        llm.request(1).contains("a.txt:1:"),
        "第一次要真搜到：{}",
        llm.request(1)
    );
    assert!(
        llm.request(2).contains("未重跑"),
        "第二次要走账本、把上次原文还回去：{}",
        llm.request(2)
    );
    assert!(
        log.contains("dedup search files|mark-1"),
        "trace 要写出这次复用（层与词都在键里）：{log}"
    );
}

/// **会话层的诚实结论**：它是**自失效**的 —— 检索自己的结果也会进侧存，
/// 而侧存索引就是这一层的版本源 ⇒ 下一次检索的版本必然变了 ⇒ **重新执行**。
///
/// 这不是缺陷，是 fail-safe 的方向：拿旧结果冒充新的比多跑一次坏得多。
/// 判据把这件事**钉住**（谁要是把版本源改成"不算自己写的那几条"，这条会红）。
#[test]
fn the_session_layer_is_self_invalidating_and_that_is_the_safe_direction() {
    let d = TempDir::new("search-session-selfinv");
    d.write("a.txt", "x\n");
    let mut cfg = quiet_cfg();
    cfg.agent.ctx.dedup = true;
    cfg.agent.ctx.capsule = true;
    let (llm, _o, log) = block_on(run_logging(
        &cfg,
        &d.0,
        vec![
            r#"{"tool":"read","args":{"scope":"session","q":"needle"}}"#.into(),
            r#"{"tool":"read","args":{"scope":"session","q":"needle"}}"#.into(),
            r#"{"final":"搜了两遍"}"#.into(),
        ],
    ));
    assert!(
        llm.request(1).contains("会话内"),
        "第一次要成功（侧存挂了的话这里是 ✗，别把失败当命中）：{}",
        llm.request(1)
    );
    assert!(
        !llm.request(2).contains("未重跑"),
        "这一层不该复用自己的结果（它的输出就是它的输入的一部分）：{}",
        llm.request(2)
    );
    assert!(
        log.contains("版本失效重执行") || log.contains("唯一执行"),
        "账本要如实记下这次真执行：{log}"
    );
}
