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
