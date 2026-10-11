//! Read / Write 两条原语（对应 `tools.rs`）：窗口读、覆盖层视图、路径封闭，
//! 以及写入策略（Stage 只进暂存区 / Apply 落盘）与锚点替换的原子性（含 CRLF 文件）。

use super::*;

/// read：文件给内容、目录给树、overlay 优先、路径封闭
#[test]
fn tool_read_file_dir_overlay_and_jail() {
    let d = TempDir::new("read");
    d.write("src/main.rs", "fn main() {}");
    let mut ctx = Ctx {
        progress: Progress::new(),
        proj: &d.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Stage,
        backup_dir: None,
        state_root: None,
        write_allow: Vec::new(),
        session_runs: Vec::new(),
        kb: None,
    };
    assert!(rd(&ctx, "src/main.rs").unwrap().contains("fn main()"));
    assert!(rd(&ctx, "src").unwrap().contains("main.rs"));
    assert!(rd(&ctx, "ghost.py").is_err());
    assert!(rd(&ctx, "../../etc/passwd").is_err());

    // overlay 优先：write 之后 read 能看到修改（Stage 策略磁盘未动）
    ctx.tool_write("src/main.rs", "fn main() { println!(1); }")
        .unwrap();
    let r = rd(&ctx, "src/main.rs").unwrap();
    assert!(r.contains("已暂存的修改"), "{r}");
    assert!(r.contains("println"));
    assert_eq!(
        std::fs::read_to_string(d.0.join("src/main.rs")).unwrap(),
        "fn main() {}",
        "Stage 策略不许动磁盘"
    );
}

/// Stage 策略：变更只进暂存目录 + manifest；Apply 策略：直接落盘且覆盖先备份
#[test]
fn write_policies_stage_vs_apply() {
    let d = TempDir::new("policy-stage");
    d.write("keep.txt", "old");
    let mut ctx = Ctx {
        progress: Progress::new(),
        proj: &d.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Stage,
        backup_dir: None,
        state_root: None,
        write_allow: Vec::new(),
        session_runs: Vec::new(),
        kb: None,
    };
    ctx.tool_write("keep.txt", "new").unwrap();
    ctx.tool_write("created.txt", "hi").unwrap();
    let stage = ctx.flush_stage().unwrap();
    assert_eq!(
        std::fs::read_to_string(stage.join("files/keep.txt")).unwrap(),
        "new"
    );
    let manifest: Vec<FileChange> =
        serde_json::from_str(&std::fs::read_to_string(stage.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest.len(), 2);
    assert!(
        manifest.iter().any(|c| c.path == "keep.txt"
            && c.kind == "modify"
            && c.before.as_deref() == Some("old"))
    );
    assert!(
        manifest
            .iter()
            .any(|c| c.path == "created.txt" && c.kind == "add")
    );

    let d2 = TempDir::new("policy-apply");
    d2.write("keep.txt", "old");
    let mut ctx2 = Ctx {
        progress: Progress::new(),
        proj: &d2.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Apply,
        backup_dir: None,
        state_root: None,
        write_allow: Vec::new(),
        session_runs: Vec::new(),
        kb: None,
    };
    ctx2.tool_write("keep.txt", "new").unwrap();
    assert_eq!(
        std::fs::read_to_string(d2.0.join("keep.txt")).unwrap(),
        "new"
    );
    let backup = ctx2.backup_dir.as_ref().unwrap();
    assert_eq!(
        std::fs::read_to_string(backup.join("keep.txt")).unwrap(),
        "old"
    );
}

/// 窗口读：只取那一段，并在表头里说清"共几行、缺口在哪、怎么接上"
#[test]
fn read_window_returns_the_slice_and_names_the_gap() {
    let d = TempDir::new("read-window");
    d.write("big.txt", &format!("{}\n", lines_fixture(500).join("\n")));
    let ctx = ctx_for(&d);

    // 不传窗口 = 老行为（整份裁读），行号表头不该冒出来
    let all = rd(&ctx, "big.txt").unwrap();
    assert!(all.contains("line 500"), "整份读该看到最后一行");
    assert!(!all.contains("共 500 行"), "整份读不该有窗口表头");

    let w = ctx
        .tool_read(&ReadSpec {
            path: "big.txt".into(),
            offset: Some(120),
            limit: Some(3),
        })
        .unwrap();
    assert!(w.contains("第 120-122 行 / 共 500 行"), "{w}");
    assert!(w.contains("line 120") && w.contains("line 122"), "{w}");
    assert!(!w.contains("line 123"), "limit 之外一行都不许多给：{w}");
    assert!(w.contains("接着读用 offset=123"), "缺口必须能接上：{w}");

    // 读到底：明说已到末尾（而不是让模型自己猜还剩多少）
    let tail = ctx
        .tool_read(&ReadSpec {
            path: "big.txt".into(),
            offset: Some(499),
            limit: None,
        })
        .unwrap();
    assert!(
        tail.contains("line 500") && tail.contains("已到文件末尾"),
        "{tail}"
    );

    // 越界当场说清：静默返回最后一行会让模型以为"中间没内容"
    let over = ctx
        .tool_read(&ReadSpec {
            path: "big.txt".into(),
            offset: Some(501),
            limit: None,
        })
        .unwrap_err();
    assert!(over.contains("只有 500 行"), "{over}");

    // 目录不吃窗口参数（它给的本来就是结构树，报错只会白换一轮往返）
    assert!(
        ctx.tool_read(&ReadSpec {
            path: ".".into(),
            offset: Some(3),
            limit: Some(2),
        })
        .is_ok()
    );
}

/// 锚点编辑：只动那一处；不唯一 / 匹配不上 / 文件不存在都当场拒，且**整批一个字节都不落盘**
#[test]
fn anchor_edits_are_surgical_and_all_or_nothing() {
    let d = TempDir::new("anchor");
    d.write("a.txt", "one\ntwo\nthree\n");
    d.write("dup.txt", "same\nsame\n");
    let mut ctx = ctx_for(&d);

    let r = ctx.tool_edit("a.txt", vec![ed("two", "TWO")]).unwrap();
    assert!(r.contains("1 处锚点替换"), "回灌要说清改了几处：{r}");
    let got = rd(&ctx, "a.txt").unwrap();
    assert!(got.contains("TWO"), "{got}");
    assert!(
        got.contains("one") && got.contains("three"),
        "别处一个字都不许碰：{got}"
    );
    assert_eq!(
        std::fs::read_to_string(d.0.join("a.txt")).unwrap(),
        "one\ntwo\nthree\n",
        "锚点编辑同样走 Stage 通道，磁盘不许动"
    );

    // 同一文件的多条 edits 按声明顺序累积
    ctx.tool_edit("a.txt", vec![ed("one", "1"), ed("three", "3")])
        .unwrap();
    let got = rd(&ctx, "a.txt").unwrap();
    assert!(got.contains("1\nTWO\n3"), "{got}");

    // 不唯一 → 拒（"恰好一次"是防改错地方的核心闸）
    let dup = ctx
        .tool_edit("dup.txt", vec![ed("same", "other")])
        .unwrap_err();
    assert!(dup.contains("出现 2 次"), "{dup}");
    // 整批作废：同批里第 1 条是合法的，也不许生效
    let mixed = ctx
        .tool_edit("a.txt", vec![ed("1", "X"), ed("nope", "Y")])
        .unwrap_err();
    assert!(mixed.contains("第 2 条 edit"), "{mixed}");
    let after = rd(&ctx, "a.txt").unwrap();
    assert!(
        !after.contains('X'),
        "整批作废：第 1 条也不许留下痕迹：{after}"
    );
    // 替换前后一致也是错（那说明模型没搞清自己要改什么）
    let same = ctx.tool_edit("a.txt", vec![ed("1", "1")]).unwrap_err();
    assert!(same.contains("完全一致"), "{same}");
    // 文件不存在 → 锚点无处可锚（新建走 content 形态）
    let ghost = ctx.tool_edit("ghost.txt", vec![ed("a", "b")]).unwrap_err();
    assert!(ghost.contains("不存在"), "{ghost}");
}

/// 行尾风格不算"改动"：LF 的 find 能匹配 CRLF 的文件，落盘后 CRLF 原样保留
#[test]
fn anchor_edits_survive_crlf_files() {
    let d = TempDir::new("anchor-crlf");
    d.write("w.txt", "a\r\nb\r\n");
    let mut ctx = ctx_for(&d);
    ctx.tool_edit("w.txt", vec![ed("b", "B")]).unwrap();
    let got = rd(&ctx, "w.txt").unwrap();
    assert!(got.contains("a\r\nB\r\n"), "行尾风格必须原样保留：{got:?}");
}

/// 端到端：模型用 edits 形态改文件 —— 只传改动，落到磁盘的却是完整正确的文件
#[test]
fn edits_shape_reaches_the_file_through_the_loop() {
    let d = TempDir::new("edits-e2e");
    d.write("a.txt", "alpha\nbeta\ngamma\n");
    let llm = crate::testllm::fake_llm(vec![
        r#"{"tool":"write","args":{"path":"a.txt","edits":[{"find":"beta","replace":"BETA"}]}}"#
            .into(),
        r#"{"final":"改好了"}"#.into(),
    ]);
    let cfg = ask_cfg(&llm);
    block_on(run(
        &cfg,
        &d.0,
        "把 beta 改成 BETA",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    assert_eq!(
        std::fs::read_to_string(d.0.join("a.txt")).unwrap(),
        "alpha\nBETA\ngamma\n",
        "只传了一处改动，落盘的必须是完整文件"
    );
    assert!(
        llm.request(1).contains("1 处锚点替换"),
        "回灌要给模型一句「改了几处」"
    );
    assert_eq!(llm.count(), 2);
}

/// `read` 的唯一例外：**项目桶里的侧存文件可读（只读）**，但逃不出 state_root，
/// 项目外的绝对路径照旧一律拒绝。
#[test]
fn read_may_reach_the_state_root_and_nothing_else() {
    let d = TempDir::new("read-exc");
    d.write("a.txt", "项目内");
    let state = TempDir::new("read-exc-state");
    std::fs::create_dir_all(state.0.join("ctx")).unwrap();
    let inside = state.0.join("ctx").join("0001-read-deadbeef.txt");
    std::fs::write(&inside, "侧存正文").unwrap();
    let outside = state.0.parent().unwrap().join("ruyix_outside_probe.txt");
    std::fs::write(&outside, "不该读到").unwrap();

    let mut cfg = quiet_cfg();
    cfg.project_state_root = state.0.to_string_lossy().to_string();
    let ctx = Ctx::new(&d.0, WritePolicy::Apply)
        .with_state_root(crate::config::project_state_root(&cfg, &d.0));

    // ① 侧存文件：绝对路径可读（走例外）
    let got = ctx.tool_read(&ReadSpec {
        path: inside.to_string_lossy().to_string(),
        offset: None,
        limit: None,
    });
    assert!(
        got.as_deref().unwrap_or("").contains("侧存正文"),
        "侧存文件该读得到：{got:?}"
    );
    // ② 逃出 state_root（`..`）：拒绝
    let escape = state
        .0
        .join("ctx")
        .join("..")
        .join("..")
        .join("ruyix_outside_probe.txt");
    assert!(
        ctx.tool_read(&ReadSpec {
            path: escape.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        })
        .is_err(),
        "`..` 逃出 state_root 必须被拒"
    );
    // ③ 项目文件的绝对路径：照旧拒绝（项目内用相对路径）
    let abs = d.0.join("a.txt");
    assert!(
        ctx.tool_read(&ReadSpec {
            path: abs.to_string_lossy().to_string(),
            offset: None,
            limit: None,
        })
        .is_err(),
        "项目内文件仍只认相对路径"
    );
    let _ = std::fs::remove_file(&outside);
}

// ============================================
// v1.4 P1：写入白名单（自改闭环的边界）
// ============================================
//
// 判据 1（需求 §6）：改白名单外的文件 ⇒ **被拒**，且拒绝理由**指名是哪一类** ——
// 不是提示词礼貌劝阻。这里的断言分三层：匹配规则 / 拒绝发生在落盘之前 / 并发波那条也过闸。

/// 模式语法：段内 `*` / `?`、整段 `**`（含"零段"）、**大小写敏感**（宁严不宽）
#[test]
fn write_allow_patterns_match_documented_forms() {
    let cases = [
        (
            "plugins/tools/*/tools.toml",
            "plugins/tools/demo/tools.toml",
            true,
        ),
        (
            "plugins/tools/*/tools.toml",
            "plugins/tools/demo/nested/tools.toml",
            false,
        ),
        ("plugins/prompts/**", "plugins/prompts/a/b/c.md", true),
        ("plugins/prompts/**", "plugins/prompts", true),
        ("plugins/prompts/**", "plugins/other/a.md", false),
        ("*.py", "calc.py", true),
        ("*.py", "src/calc.py", false),
        ("src/*.py", "src/calc.py", true),
        ("src/?alc.py", "src/calc.py", true),
        ("**", "any/deep/file.txt", true),
        ("", "a.txt", false),
        (
            "plugins/tools/*/tools.toml",
            "plugins/tools/demo/Tools.toml",
            false,
        ),
    ];
    for (pat, rel, want) in cases {
        assert_eq!(allow_matches(pat, rel), want, "模式 {pat:?} 对 {rel:?}");
    }
}

/// 白名单外 ⇒ 拒绝，且**拒绝在落盘之前**（磁盘、备份、覆盖层都没动）；白名单内 ⇒ 照常
#[test]
fn writes_outside_the_whitelist_are_refused_before_anything_is_written() {
    let d = TempDir::new("allow-basic");
    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply)
        .with_write_allow(vec!["plugins/tools/*/tools.toml".into(), "*.md".into()]);

    assert!(
        ctx.tool_write("notes.md", "改这里").is_ok(),
        "白名单内该照常写"
    );
    let e = ctx
        .tool_write("global/ai.toml", "偷改别人家的配置")
        .unwrap_err();
    assert!(e.contains("global/ai.toml"), "要**指名路径**：{e}");
    assert!(
        e.contains("不在本 run 的写入白名单内"),
        "要说明是哪一类：{e}"
    );
    assert!(
        e.contains("plugins/tools/*/tools.toml"),
        "要把白名单原文列出来（模型据此一轮就能改对）：{e}"
    );
    // "拒绝在落盘之前"这句必须真成立 —— 只写在注释里就是一句口号
    assert!(!d.0.join("global/ai.toml").exists(), "白名单外不许落盘");
    assert_eq!(ctx.changes.len(), 1, "只有白名单内那一次进账");
    assert!(
        !ctx.overlay.contains_key("global/ai.toml"),
        "覆盖层也不该有它（否则 read 会看到一次并不存在的改动）"
    );
}

/// 空白名单 = **不启用**（出厂默认；老行为一字不变）
#[test]
fn an_empty_whitelist_changes_nothing() {
    let d = TempDir::new("allow-off");
    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply);
    assert!(ctx.tool_write("anything/at/all.txt", "随便写").is_ok());
    assert!(d.0.join("anything/at/all.txt").is_file());
}

/// **并发波**那条路径也必须过闸 —— 它绕开了 `Ctx::apply_write`，是同一类闸最容易漏的一处
/// （`dispatch` 里自己 `resolve_write` + `flush_write_disk`）。
#[test]
fn the_parallel_wave_is_guarded_too() {
    let d = TempDir::new("allow-wave");
    let mk = |path: &str, body: &str| {
        parse_one(&serde_json::json!({"tool":"write","args":{"path":path,"content":body}}))
            .expect("write 该能解析")
    };
    let mut cfg = quiet_cfg();
    cfg.agent.write_allow = vec!["ok/**".into()];
    let allow = cfg.agent.write_allow.clone();
    let mut ctx = Ctx::new(&d.0, WritePolicy::Apply).with_write_allow(allow);
    let actions = vec![mk("ok/a.txt", "允许"), mk("bad.txt", "越界")];
    let plans = vec![None, None];
    let mut slots: Vec<Option<CallResult>> = vec![None, None];
    block_on(run_wave(
        &cfg,
        &d.0,
        WritePolicy::Apply,
        &NoConnector,
        &mut ctx,
        &actions,
        &plans,
        &[0, 1],
        &mut slots,
    ));
    let ok = slots[0].clone().expect("白名单内该有结果").2;
    let bad = slots[1].clone().expect("越界该有结果").2;
    assert!(ok.is_ok(), "白名单内该写成功：{ok:?}");
    let e = bad.unwrap_err();
    assert!(
        e.contains("bad.txt") && e.contains("白名单"),
        "越界要给具体理由：{e}"
    );
    assert!(d.0.join("ok/a.txt").is_file());
    assert!(!d.0.join("bad.txt").exists(), "越界的文件不许出现在磁盘上");
}
