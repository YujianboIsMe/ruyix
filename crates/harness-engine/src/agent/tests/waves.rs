//! 波次调度与并发（对应 `dispatch.rs`）。
//!
//! 无冲突 ⇒ 并发、有冲突 ⇒ 下一波；并发跑完**结果顺序仍按声明序**（模型靠下标对齐自己的动作）。

use super::*;

/// 分组：只有**连续只读**成组并发；写入/执行/连接一律按序各成一组
#[test]
fn waves_parallelize_everything_except_conflicts() {
    // 多给一个 content：write 现在必须带 content 或 edits（形状含糊当场拒），
    // 其余能力忽略多余字段 —— 这个测试只关心分组，不关心参数形状
    let a = |tool: &str| {
        parse_one(
            &serde_json::json!({"tool": tool, "args": {"path": "x", "cmd": "c", "content": "v"}}),
        )
        .unwrap_or_else(|e| panic!("{tool} 该能解析：{e}"))
    };
    let mk = |tool: &str, path: &str| {
        parse_one(
            &serde_json::json!({"tool": tool, "args": {"path": path, "cmd": "c", "content": "x"}}),
        )
        .unwrap_or_else(|e| panic!("{tool} 该能解析：{e}"))
    };
    // 口径：**要并发就一起并发** —— 读/写/执行/连接只要互不冲突就都在同一波
    assert_eq!(
        batch_waves(&[
            a("read"),
            a("read"),
            a("execute"),
            a("connect"),
            a("execute")
        ]),
        vec![vec![0, 1, 2, 3, 4]]
    );
    // 不同路径的两次写：也可以并发
    assert_eq!(
        batch_waves(&[mk("write", "a.rs"), mk("write", "b.rs"), mk("read", "c.rs")]),
        vec![vec![0, 1, 2]]
    );
    // 唯一的保序之一：同一条路径上的写与读（覆盖层是"写完立刻读回来"的依据）。
    // 注意 execute 与它们都不冲突，所以并进第一波；写 x 独占第二波；写之后的读 x 在第三波。
    assert_eq!(
        batch_waves(&[a("read"), a("write"), a("read"), a("execute")]),
        vec![vec![0, 3], vec![1], vec![2]]
    );
    // 唯一之二的保序：托管进程的生命周期操作（引擎自己持有那张表，句柄又由引擎赋值）
    let bg = parse_one(&serde_json::json!({
        "tool": "execute",
        "args": {"cmd": "srv", "background": true}
    }))
    .expect("bg 该能解析");
    let st = parse_one(&serde_json::json!({
        "tool": "execute",
        "args": {"op": "status", "handle": "p1"}
    }))
    .expect("status 该能解析");
    assert_eq!(
        batch_waves(&[bg.clone(), st.clone()]),
        vec![vec![0], vec![1]]
    );
    // 单条也是一波（单波单条 = 老路径）
    assert_eq!(batch_waves(&[a("read")]), vec![vec![0]]);
}

/// 并发读：结果按声明顺序落位，一条失败不挪动别人的位置
#[test]
fn parallel_reads_land_in_declared_order() {
    let d = TempDir::new("batch-read");
    d.write("a.txt", "AAA");
    d.write("b.txt", "BBB");
    d.write("c.txt", "CCC");
    let ctx = Ctx {
        progress: Progress::new(),
        proj: &d.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Stage,
        backup_dir: None,
        state_root: None,
        write_allow: Vec::new(),
    };
    let paths: Vec<ReadSpec> = ["a.txt", "ghost.txt", "c.txt", "b.txt"]
        .iter()
        .map(|s| ReadSpec::whole(*s))
        .collect();
    for parallel in [true, false] {
        let rs = read_group(&ctx, &paths, parallel);
        assert_eq!(rs.len(), 4);
        assert!(
            rs[0].as_ref().unwrap().contains("AAA"),
            "槽位错位：{:?}",
            rs[0]
        );
        assert!(rs[1].is_err(), "不存在的文件该在自己那一格报错");
        assert!(
            rs[2].as_ref().unwrap().contains("CCC"),
            "槽位错位：{:?}",
            rs[2]
        );
        assert!(
            rs[3].as_ref().unwrap().contains("BBB"),
            "槽位错位：{:?}",
            rs[3]
        );
    }
}

/// **省轮次的核心断言**：一批 3 个 read 只花 1 轮（改前是 3 轮），结果一次全回给模型
#[test]
fn a_read_batch_costs_one_round_and_returns_every_result() {
    let d = TempDir::new("batch-e2e");
    d.write("a.txt", "AAA");
    d.write("b.txt", "BBB");
    d.write("c.txt", "CCC");
    let llm = crate::testllm::fake_llm(vec![
        concat!(
            r#"{"actions":[{"tool":"read","args":{"path":"a.txt"}},"#,
            r#"{"tool":"read","args":{"path":"b.txt"}},"#,
            r#"{"tool":"read","args":{"path":"c.txt"}}]}"#
        )
        .into(),
        r#"{"final":"三个文件都看过了"}"#.into(),
    ]);
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;
    cfg.step.execute_plan = false;

    let out = block_on(run(
        &cfg,
        &d.0,
        "看看这三个文件",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    assert_eq!(llm.count(), 2, "一批 3 个 read = 1 轮（外加 final 那轮）");
    let second = llm.request(1);
    for content in ["AAA", "BBB", "CCC"] {
        assert!(
            second.contains(content),
            "第二轮该看到全部结果（缺 {content}）"
        );
    }
    assert!(
        second.contains("results"),
        "批结果走 results 数组：{second}"
    );
    // 三次调用都进轨迹，且都属于**同一轮**
    assert_eq!(out.steps.len(), 3);
    assert!(
        out.steps
            .iter()
            .all(|s| s.step == 1 && s.tool == "read" && s.ok),
        "{:?}",
        out.steps
    );
    assert_eq!(out.answer, "三个文件都看过了");
}

/// 同一批里的顺序语义：write 之后紧跟的 read 必须看到**刚写的内容**（覆盖层），
/// 所以写与读之间不许并发 —— 分组按"连续的只读区间"切，正是为了这条。
#[test]
fn a_write_followed_by_a_read_in_one_batch_keeps_order() {
    let d = TempDir::new("batch-order");
    d.write("a.txt", "OLD");
    let llm = crate::testllm::fake_llm(vec![
        concat!(
            r#"{"actions":[{"tool":"write","args":{"path":"a.txt","content":"NEW"}},"#,
            r#"{"tool":"read","args":{"path":"a.txt"}}]}"#
        )
        .into(),
        r#"{"final":"改完了"}"#.into(),
    ]);
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;
    cfg.step.execute_plan = false;

    let out = block_on(run(
        &cfg,
        &d.0,
        "把 a.txt 改掉",
        &[],
        WritePolicy::Stage,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    assert_eq!(llm.count(), 2, "写 + 读同批，只花 1 轮");
    let second = llm.request(1);
    assert!(
        second.contains("本会话已暂存的修改"),
        "同批里的 read 必须看到刚写的暂存内容：{second}"
    );
    assert!(
        out.steps.iter().any(|s| s.tool == "write" && s.ok),
        "{:?}",
        out.steps
    );
    // Stage 策略：项目磁盘不动（读到的新内容来自覆盖层）
    assert_eq!(std::fs::read_to_string(d.0.join("a.txt")).unwrap(), "OLD");
}

/// **并发是真的**（不是"看起来像"）：同样两条 2 秒命令，一轮发一条（串行）vs 一批发两条
/// （并发）。用**对照臂比时间**而不是拿绝对秒数赌机器负载 —— 相对判据在慢机器、以及全套测试
/// 并跑抢 CPU 时同样成立（两条臂被一起拉长）。谁把 execute 挪回串行，这条立刻变红。
#[test]
fn two_commands_in_one_batch_really_run_in_parallel() {
    let d = TempDir::new("batch-par");
    // Windows 没有 sleep，用 ping 的次数间隔当"2 秒的活"
    let nap = if cfg!(windows) {
        "ping -n 3 127.0.0.1 > nul"
    } else {
        "sleep 2"
    };
    let one = format!(r#"{{"tool":"execute","args":{{"cmd":"{nap}"}}}}"#);
    let two = format!(r#"{{"actions":[{one},{one}]}}"#);

    let timed = |script: Vec<String>| -> (Duration, usize) {
        let llm = crate::testllm::fake_llm(script);
        let mut cfg = AppConfig::default();
        cfg.llm.base_url = llm.base_url.clone();
        cfg.llm.api_key = "smoke".into();
        cfg.llm.model = "fake".into();
        cfg.gate.narrow = false;
        cfg.gate.full = false;
        cfg.reflect.enabled = false;
        cfg.step.execute_plan = false;
        // 命令发现要探一遍本机工具（git/python 各一次调用），会把计时搅浑 —— 这条不测它
        cfg.discover.enabled = false;
        let t0 = Instant::now();
        let out = block_on(run(
            &cfg,
            &d.0,
            "跑两条命令",
            &[],
            WritePolicy::Apply,
            &NoConnector,
            &crate::exec::new_cancel_flag(),
            &QuietSink,
        ))
        .expect("run 不该失败");
        let el = t0.elapsed();
        assert_eq!(out.steps.len(), 2, "{:?}", out.steps);
        (el, llm.count())
    };

    let (serial, serial_calls) = timed(vec![one.clone(), one, r#"{"final":"跑完了"}"#.into()]);
    let (parallel, parallel_calls) = timed(vec![two, r#"{"final":"跑完了"}"#.into()]);
    assert_eq!(serial_calls, 3, "串行臂：两条命令各占一轮 + final");
    assert_eq!(parallel_calls, 2, "并发臂：两条命令同批一轮 + final");
    // 串行 ≈ 4 秒、并发 ≈ 2 秒；给足余量（3×并发 < 2×串行 ≈ 并发 < 0.67×串行）
    assert!(
        parallel * 3 < serial * 2,
        "同一条批里的两条命令必须并发跑：并发 {parallel:?} vs 串行 {serial:?}"
    );
}

/// `record_findings` 能搭车进批（对外部世界没有副作用、没有顺序风险），
/// 而不是像 plan/final 那样被当面拒。
#[test]
fn record_findings_rides_along_in_a_batch() {
    let raw = r#"{"actions":[{"tool":"read","args":{"path":"a.rs"}},{"tool":"record_findings","args":{"items":[{"claim":"a.rs 里有 X","evidence":"a.rs:3"}]}}]}"#;
    let acts = parse_actions(raw, 8, true).expect("批里带 record_findings 应当被受理");
    assert_eq!(acts.len(), 2);
    assert!(matches!(acts[1], Action::Findings(ref v) if v.len() == 1));
    // 单个条目直接给（不套 items）也要认 —— 少写一层包装不该白费一轮
    let one =
        parse_action(r#"{"tool":"record_findings","args":{"claim":"c","evidence":"e"}}"#).unwrap();
    assert!(matches!(one, Action::Findings(ref v) if v.len() == 1));
    // 空 items 一律拒（空调用是无意义的一轮，要变成一句明确的纠正）
    assert!(parse_action(r#"{"tool":"record_findings","args":{"items":[]}}"#).is_err());
}
