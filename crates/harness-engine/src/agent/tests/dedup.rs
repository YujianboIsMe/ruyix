//! 去重账本与 capsule 侧存（v1.2 P2/P3，对应 `ledger.rs` / `keys.rs` / `capsule.rs`）：
//! 纯调用重复执行会被拦下、自己写完再读必须重执行、有副作用的调用永不参与、
//! 内存上限把最老的整条挤掉后要真重跑、命中召回走磁盘且**不重跑工具**。

use super::*;

/// **P2 的主判据**（同一份脚本、两臂）：重复调用在对照臂里**真的执行了**，
/// 在实验臂里**被拦下**。这也是"重复工具调用次数下降 ≥50%"那条验收在引擎里的形状 ——
/// 两臂用的是**同一份仪器**（`metrics` 都开），处理变量只有 `dedup`。
#[test]
fn dedup_blocks_the_repeat_that_the_control_arm_really_executes() {
    let script = || {
        vec![
            r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
            r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
            r#"{"final":"读了两遍"}"#.to_string(),
        ]
    };
    // 臂一（对照）：先只开仪器 —— 重复的那次**照旧执行**，账上记为"仍重复执行"
    let d1 = TempDir::new("dedup-off");
    d1.write("a.txt", "内容-MARK-1");
    let mut cfg_off = quiet_cfg();
    cfg_off.agent.ctx.metrics = true;
    let (llm_off, _, log_off) = block_on(run_logging(&cfg_off, &d1.0, script()));

    // 臂二（实验）：再开去重 —— 同样的重复被拦下
    let d2 = TempDir::new("dedup-on");
    d2.write("a.txt", "内容-MARK-1");
    let mut cfg_on = cfg_off.clone();
    cfg_on.agent.ctx.dedup = true;
    let (llm_on, _, log_on) = block_on(run_logging(&cfg_on, &d2.0, script()));

    assert_eq!(llm_off.count(), 3, "两次读 → 交付，共三次请求");
    assert_eq!(
        llm_on.count(),
        3,
        "去重不省轮次：省的是**执行**，不是模型往返"
    );

    // 对照臂：没有 dedup 行，也没有复用标注；账上"仍重复执行 1"
    assert!(
        !log_off.contains("dedup read"),
        "对照臂不该有 dedup 行：{log_off}"
    );
    assert!(
        !llm_off.request(2).contains("未重跑"),
        "对照臂不该有复用标注"
    );
    assert!(
        log_off.contains("拦截重复 0") && log_off.contains("仍重复执行 1"),
        "对照臂的账该如实数出那次浪费：{log_off}"
    );

    // 实验臂：拦下了、标注了、结果逐字节还是上次那份
    assert!(
        log_on.contains("dedup read a.txt"),
        "实验臂的 trace 必须有 `dedup <tool> <norm>` 这一行：{log_on}"
    );
    let last = llm_on.request(2);
    assert!(
        last.contains("内容-MARK-1") && last.contains("本次直接复用，未重跑"),
        "命中要把**上次的原文**还回去并附标注：{last}"
    );
    assert!(
        log_on.contains("拦截重复 1") && log_on.contains("仍重复执行 0"),
        "实验臂的账该显示那次重复被拦下了：{log_on}"
    );
}

/// **不该省的一条：版本变了必须重执行**（自己写过它 = 版本失效）。
/// 这条是"静默给出错答案"的唯一防线 —— 反向错了最难查。
#[test]
fn dedup_reruns_after_our_own_write_invalidates_the_read() {
    let d = TempDir::new("dedup-stale");
    d.write("a.txt", "旧内容-OLD");
    let script = vec![
        r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
        format!(
            r#"{{"tool":"write","args":{{"path":"a.txt","content":{}}}}}"#,
            serde_json::to_string("新内容-NEW").unwrap()
        ),
        r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
        r#"{"final":"改完再看一遍"}"#.to_string(),
    ];
    let mut cfg = quiet_cfg();
    cfg.agent.ctx.metrics = true;
    cfg.agent.ctx.dedup = true;
    let (llm, _, log) = block_on(run_logging(&cfg, &d.0, script));

    // request(3) = 第 3 条动作（重读）的结果回灌出去的那一次请求
    let last_read = llm.request(3);
    assert!(
        last_read.contains("新内容-NEW"),
        "写完之后再读，必须是**新内容**（复用旧内容就是静默给错答案）：{last_read}"
    );
    assert!(
        !last_read.contains("未重跑"),
        "版本变了不许标注为复用：{last_read}"
    );
    assert!(
        log.contains("版本失效重执行 1"),
        "账上该如实记一次「版本失效重执行」：{log}"
    );
}

/// **不该省的第二条：不纯的动作永远不进去重**（白名单外一律执行）。
#[test]
fn dedup_never_blocks_effectful_calls() {
    let d = TempDir::new("dedup-effectful");
    let script = vec![
        r#"{"tool":"execute","args":{"cmd":"echo one"}}"#.to_string(),
        r#"{"tool":"execute","args":{"cmd":"echo one"}}"#.to_string(),
        r#"{"final":"跑了两遍"}"#.to_string(),
    ];
    let mut cfg = quiet_cfg();
    cfg.agent.ctx.metrics = true;
    cfg.agent.ctx.dedup = true;
    let (llm, _, log) = block_on(run_logging(&cfg, &d.0, script));

    assert_eq!(llm.count(), 3);
    assert!(
        log.contains("拦截重复 0") && log.contains("不纯执行 2"),
        "白名单外的命令必须每次都真跑：{log}"
    );
    // 白名单内的同类命令（纯）则会被拦 —— 一正一反，判据才立得住
    let d2 = TempDir::new("dedup-pure-cmd");
    d2.write("b.txt", "hello");
    let script2 = vec![
        r#"{"tool":"execute","args":{"cmd":"type b.txt"}}"#.to_string(),
        r#"{"tool":"execute","args":{"cmd":"type b.txt"}}"#.to_string(),
        r#"{"final":"看完了"}"#.to_string(),
    ];
    let (_, _, log2) = block_on(run_logging(&cfg, &d2.0, script2));
    assert!(
        log2.contains("拦截重复 1") && log2.contains("dedup execute type b.txt"),
        "`type` 是白名单里的只读命令，第二次该被拦下：{log2}"
    );
}

/// **停滞判据的替换**：账本记下一次**真执行**才算进展；只有命中的那一轮什么都不算。
///
/// 它替掉的是 `fresh_cmd` 那条按**命令字面**去重的近似（`git grep x` 与
/// `git grep x 2>nul` 在那个判据下是两条新命令 ⇒ 换个拼法重跑同一件事也能骗过守卫）。
#[test]
fn a_library_hit_is_not_progress_and_a_real_execution_is() {
    let mut p = Progress::new();
    let call = ledger_read("a.rs");
    let v = ledger::VersionVec {
        repo: None,
        paths: vec![("a.rs".to_string(), ledger::Ver::File(1, 1))],
        unknown: false,
    };
    // 第 1 轮：真执行了一次 ⇒ 算进展
    p.dedup_record(call, "内容".into(), v.clone(), 1);
    p.note_round(1, false, false, false);
    assert_eq!(p.stalled(1), 0, "真执行算进展");
    // 第 2 轮：只有命中（复用），没有新结论、没有写文件 ⇒ 算停滞
    p.note_round(2, false, false, false);
    assert_eq!(p.stalled(2), 1, "复用不算进展 —— 那一轮什么都没新看到");
    // 第 3 轮：又真执行了一次（新命令/版本失效的重执行）⇒ 进展恢复
    p.dedup_record(ledger_read("b.rs"), "别的".into(), v, 3);
    p.note_round(3, false, false, false);
    assert_eq!(p.stalled(3), 0);
}

/// 结果侧存的上限：**被挤掉的条目等于没记过**（下次真执行，fail-safe）。
#[test]
fn dedup_cap_evicts_the_oldest_and_then_re_executes() {
    let d = TempDir::new("dedup-cap");
    for i in 0..3 {
        d.write(
            &format!("f{i}.txt"),
            &format!("体量-{i}-{}", "x".repeat(400)),
        );
    }
    let script = vec![
        r#"{"tool":"read","args":{"path":"f0.txt"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"f1.txt"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"f0.txt"}}"#.to_string(),
        r#"{"final":"看完了"}"#.to_string(),
    ];
    let mut cfg = quiet_cfg();
    cfg.agent.ctx.metrics = true;
    cfg.agent.ctx.dedup = true;
    // 上限只够放一条（第二条进来就把 f0 挤掉）
    cfg.agent.ctx.dedup_max_bytes = 500;
    let (llm, _, log) = block_on(run_logging(&cfg, &d.0, script));

    assert!(
        !llm.request(3).contains("未重跑"),
        "被挤掉的条目不许假装还记得：{}",
        llm.request(3)
    );
    assert!(
        log.contains("拦截重复 0"),
        "上限挤掉之后那次读必须真执行：{log}"
    );
    assert!(log.contains("侧存 1 条"), "侧存该被压到上限之内：{log}");
}

// ============================================================================
// v1.2 P3：capsule 侧存（`agent.ctx.capsule`）
// ============================================================================

/// P3 的判据：挂着 capsule 时，重复的读**从侧存召回**（磁盘读 + sha256 校验），
/// **不重跑工具**，而且侧存里那份与原文**逐字节相同**。
#[test]
fn capsule_serves_the_recall_without_rerunning_the_tool() {
    let d = TempDir::new("capsule-on");
    d.write("a.txt", "正文-MARK-CAPSULE\n第二行\n");
    let state = TempDir::new("capsule-state");

    let mut cfg = quiet_cfg();
    cfg.agent.ctx.dedup = true;
    cfg.agent.ctx.metrics = true;
    cfg.agent.ctx.capsule = true;
    cfg.project_state_root = state.0.to_string_lossy().to_string();

    let script = vec![
        r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
        r#"{"final":"读了两遍"}"#.to_string(),
    ];
    let (llm, _, log) = block_on(run_logging(&cfg, &d.0, script));

    assert_eq!(llm.count(), 3, "去重不省轮次（省的是执行）");
    // ① trace 上要看得出"召回"，且带 sha256 校验标记
    assert!(
        log.contains("capsule 召回，sha256 ✓"),
        "命中的那条 trace 要标出侧存召回：{log}"
    );
    assert!(
        log.contains("拦截重复 1") && log.contains("仍重复执行 0"),
        "账上该是拦下 1 次、零重复执行：{log}"
    );
    // ② 工具只执行了一次：第二次读没有新的 read 结果（正文只在轮 1 出现）
    let reads = log.matches("第 2 轮 read ✓").count();
    assert_eq!(reads, 0, "第二次读不该执行：{log}");
    // ③ 侧存真的落了盘，且**只落在 state_root 下**（绝不写用户仓库）
    let ctx_dir = state.0.join("ctx");
    let runs: Vec<_> = std::fs::read_dir(&ctx_dir)
        .expect("capsule 目录该建出来")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(runs.len(), 1, "一次 run 一个目录");
    let idx = std::fs::read_to_string(runs[0].path().join("index.jsonl")).unwrap();
    assert_eq!(idx.lines().count(), 1, "一条结果一行索引");
    let v: serde_json::Value = serde_json::from_str(idx.lines().next().unwrap()).unwrap();
    let body = std::fs::read_to_string(runs[0].path().join(v["file"].as_str().unwrap())).unwrap();
    assert_eq!(
        crate::agent::capsule::Capsule::sha256(&body),
        v["sha256"].as_str().unwrap(),
        "侧存里的正文与索引里的 sha256 必须一致"
    );
    assert!(body.contains("正文-MARK-CAPSULE"), "{body}");
    // ④ 项目目录里没多出东西（侧存不是写这儿）
    let after: Vec<_> = std::fs::read_dir(&d.0)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        after,
        vec!["a.txt".to_string()],
        "用户仓库里一个字节都不许写"
    );
}

/// `capsule` 关着时行为与 P2 一模一样：结果留在内存里，**不建任何目录**
#[test]
fn capsule_off_writes_nothing_anywhere() {
    let d = TempDir::new("capsule-off");
    d.write("a.txt", "内容-MARK-1");
    let state = TempDir::new("capsule-off-state");
    let mut cfg = quiet_cfg();
    cfg.agent.ctx.metrics = true;
    cfg.agent.ctx.dedup = true;
    cfg.project_state_root = state.0.to_string_lossy().to_string();
    let script = vec![
        r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"a.txt"}}"#.to_string(),
        r#"{"final":"x"}"#.to_string(),
    ];
    let (_, _, log) = block_on(run_logging(&cfg, &d.0, script));
    // 判据盯**行为**，不盯那个词：run 末的"机制 capsule=off"那行本来就该出现
    // （用户要求"关也报一声"），所以这里只禁止侧存/召回**真的发生**。
    assert!(
        !log.contains("capsule 侧存") && !log.contains("capsule 召回"),
        "关着时不该有侧存或召回：{log}"
    );
    // v1.5 起这个目录里多了一条**独立于 capsule** 的东西：本 run 的**留痕**
    // （`agent/transcript.rs`，出厂即用 —— 用户 2026-10-09 的刚需：会话内跨 run 问一件事）。
    // 所以判据从"什么都不许写"改成"**写的是哪一个**"：关着 capsule ⇒ 一个侧存索引都没有，
    // 但留痕照建（它是甸服的正文来源，与 capsule 开关无关）。
    let ctx_dir = state.0.join("ctx");
    let has = |name: &str| {
        std::fs::read_dir(&ctx_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .any(|e| e.path().join(name).is_file())
            })
            .unwrap_or(false)
    };
    assert!(!has("index.jsonl"), "关着 capsule ⇒ 一个侧存索引都不许有");
    assert!(
        has("transcript.jsonl"),
        "留痕是独立于 capsule 的（甸服的正文来源）"
    );
    assert!(log.contains("拦截重复 1"), "P2 的去重照旧：{log}");
}
