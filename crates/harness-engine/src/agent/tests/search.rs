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
    // 「没搜」必须写出来：这里是"不是 git 仓库（忽略规则不适用）+ 跳过的固定目录"两件事。
    // 2026-10-09 起清单来源改成"git 优先、目录树兜底"，措辞跟着事实走。
    assert!(
        out.contains("不是 git 仓库") && out.contains("跳过这些目录"),
        "「没搜」与「没有」必须可分：{out}"
    );
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

/// **第 1 服**：没有留痕（headless / eval / 建不起来）时也是明说，不是"空的"。
#[test]
fn the_session_scope_needs_a_transcript() {
    let d = TempDir::new("search-session");
    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply);
    let e = crate::agent::search::run(&mut ctx, &spec(SearchScope::Session, "x")).unwrap_err();
    assert!(e.contains("留痕"), "{e}");
    assert!(e.contains("没有读数"), "{e}");
}

/// **用户 2026-10-09 的场景**：一个会话跑了 3 个 run，问"第 1 个和第 3 个关于【user 表 password
/// 字段长度】有没有冲突" —— 这一条钉的就是甸服的正文来源与跨 run 能力。
///
/// 为什么此前一条都搜不到（记住这个反例，别再退回那个形状）：正文只存在于 capsule（只留
/// "被折掉 / 可复用"的结果、一 run 一目录），而**提问与回答压根没存过**、工具调用只留了那一小块。
/// 留痕（`transcript.jsonl`）才是这一层的正文：本 run + 历史里那几个 run 的提问/调用/回答。
#[test]
fn the_session_layer_searches_this_run_and_the_previous_ones() {
    let d = TempDir::new("search-xrun-proj"); // 项目（检索绝不写它）
    let state = TempDir::new("search-xrun-state"); // 状态根（留痕落这儿）
    let before = tree_fingerprint(&d.0);

    // 造两个"前面几个 run"的留痕 —— 就像那两个 run 当时真跑过
    let ctx_root = state.0.join("ctx");
    let mk = |id: &str, prompt: &str, result: &str| {
        let mut t = crate::agent::transcript::Transcript::create(&ctx_root.join(id)).unwrap();
        t.prompt(prompt);
        t.call("read", "db/schema.sql", true, result);
        t.final_text("已记下 password 字段的长度。");
    };
    mk(
        "agent-20261001-100000",
        "建 user 表：password 给 varchar(60)",
        "password varchar(60)",
    );
    mk(
        "agent-20261002-100000",
        "需求变了：password 改成 varchar(72)",
        "password varchar(72)",
    );

    // 本 run（第 3 个）：自己的留痕 + 历史里那两个 run 的 id（新 → 旧，宿主就是这么给的）
    let mut ctx = ctx_with(&d.0, vec![], WritePolicy::Stage)
        .with_state_root(state.0.clone())
        .with_session_runs(vec![
            "agent-20261002-100000".into(),
            "agent-20261001-100000".into(),
        ]);
    let mut t =
        crate::agent::transcript::Transcript::create(&ctx_root.join("agent-20261003-100000"))
            .unwrap();
    t.prompt("现在 password 字段到底多长？和最早的建表有没有冲突？");
    t.call(
        "execute",
        "grep -n password db/schema.sql",
        true,
        "password varchar(72)",
    );
    ctx.progress_mut().attach_transcript(t);

    let out = crate::agent::search::run(&mut ctx, &spec(SearchScope::Session, "password")).unwrap();
    assert!(
        out.contains("本 run"),
        "本 run 的提问/调用要在结果里：{out}"
    );
    assert!(
        out.contains("agent-20261001-100000"),
        "第 1 个 run 要被搜到：{out}"
    );
    assert!(
        out.contains("agent-20261002-100000"),
        "第 2 个 run 要被搜到：{out}"
    );
    assert!(
        out.contains("varchar(60)") && out.contains("varchar(72)"),
        "两个 run 的**不同说法都要在**（冲突要看得出来，引擎不许替它挑一个）：{out}"
    );
    let (me, prev) = (
        out.find("本 run").unwrap(),
        out.find("agent-20261002").unwrap(),
    );
    assert!(me < prev, "顺序是 本 run → 新的历史 → 旧的历史：{out}");
    assert_eq!(before, tree_fingerprint(&d.0), "检索一个字节都不许写项目");
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

/// **接线**：真跑一遍工具循环 —— 留痕**真落盘**、run id **真交回**。
///
/// 模块级测试只能证明"给一份留痕我能搜到"；这一条证明"跑起来之后那份留痕真在"。
/// 少这一条，改写留痕的三个位置（提问 / 调用 / final）任何一个断了都不会有人发现。
#[test]
fn a_real_run_writes_the_transcript_and_hands_the_run_id_back() {
    let d = TempDir::new("tr-loop");
    let state = TempDir::new("tr-loop-state");
    d.write("a.txt", "内容-MARK-TR");
    let mut cfg = quiet_cfg();
    cfg.project_state_root = state.0.to_string_lossy().to_string();
    let script = vec![
        r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
        r#"{"final":"读完了：里面有 MARK-TR。"}"#.to_string(),
    ];
    let (_, out, _) = block_on(run_logging(&cfg, &d.0, script));

    let id = out
        .run_id
        .clone()
        .expect("引擎必须把本 run 的留痕 id 交回（跨 run 的钥匙）");
    let dir = state.0.join("ctx").join(&id);
    let text = std::fs::read_to_string(dir.join(crate::agent::transcript::FILE))
        .unwrap_or_else(|e| panic!("留痕没落盘（{}）：{e}", dir.display()));
    assert!(text.contains(r#""kind":"prompt""#), "提问要记：{text}");
    assert!(
        text.contains(r#""kind":"call""#) && text.contains("a.txt"),
        "工具调用与它的 target 要记：{text}"
    );
    assert!(
        text.contains(r#""kind":"final""#),
        "交付的 final 要记：{text}"
    );
    // 同一份留痕**当场就能被会话层搜到**（正文来自工具结果）
    let hits = crate::agent::transcript::search_in(&dir, "MARK-TR", 5);
    assert!(
        hits.iter().any(|h| h.text.contains("MARK-TR")),
        "工具结果要能被搜到：{hits:?}"
    );
    // 也绝不写进用户项目
    assert!(
        !d.0.join("transcript.jsonl").exists() && !d.0.join("ctx").exists(),
        "留痕不许落进用户仓库"
    );
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
    assert!(
        v2.unknown,
        "留痕没挂 ⇒ 版本不可判（宁可多跑一次，不拿旧结果冒充新的）"
    );

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

/// **要服判据**：git 仓库里**认 `.gitignore`**（2026-10-09 审计补上；老路 `git grep` 就是认的）。
///
/// 两个方向都钉（只钉"排除了"的话，任何"什么都搜不到"的实现都会绿）：
/// ① 有 `.gitignore` ⇒ 被忽略的目录**搜不到**；② 没有 `.gitignore` ⇒ 同样的文件**必须搜到**。
/// 第二条是**对照臂**：证明排除来自忽略规则，而不是"因为文件名/目录名眼熟被跳了"。
#[test]
fn the_files_scope_honors_gitignore_with_a_control_arm() {
    let needle = "NEEDLE-IGNORE-CASE";

    // 臂 A：仓库 + `.gitignore` 忽略 `ignored/`
    let a = TempDir::new("search-gi-a");
    if let Err(e) = crate::gitops::snapshot_init(&a.0, "init") {
        // 这台机器没有可用的 git ⇒ **大声跳过**（静默跳过等于没判据）
        eprintln!("跳过 the_files_scope_honors_gitignore_with_a_control_arm：建不了仓库（{e}）");
        return;
    }
    a.write("keep.txt", needle);
    a.write("ignored/secret.txt", needle);
    a.write(".gitignore", "ignored/\n");
    let mut ctx = ctx_with(&a.0, vec![], WritePolicy::Apply);
    let out = crate::agent::search::run(&mut ctx, &spec(SearchScope::Files, needle)).unwrap();
    assert!(out.contains("keep.txt"), "该搜到的文件要搜到：{out}");
    assert!(
        !out.contains("secret.txt"),
        "被 `.gitignore` 忽略的文件不许出现在结果里：{out}"
    );
    assert!(
        out.contains(".gitignore"),
        "结果里要写明是按 git 的忽略规则取的清单：{out}"
    );

    // 臂 B（对照）：同样的树、**没有** `.gitignore` ⇒ 那个文件必须在结果里
    let b = TempDir::new("search-gi-b");
    crate::gitops::snapshot_init(&b.0, "init").expect("臂 A 建得起来，臂 B 也该建得起来");
    b.write("keep.txt", needle);
    b.write("ignored/secret.txt", needle);
    let mut ctx2 = ctx_with(&b.0, vec![], WritePolicy::Apply);
    let out2 = crate::agent::search::run(&mut ctx2, &spec(SearchScope::Files, needle)).unwrap();
    assert!(
        out2.contains("secret.txt"),
        "没有忽略规则时，同一个文件必须搜得到（否则排除的不是忽略规则，是别的什么）：{out2}"
    );
}
