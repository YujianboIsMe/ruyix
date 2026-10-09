//! 历史折叠与进展记忆（对应 `findings.rs` + 折叠逻辑）：摘要保留"结论与做过什么"、
//! 停滞只数**没有进展**的轮次、findings 超限带指针溢出、重复读同一个区间要被点出来。

use super::*;

#[test]
fn tail_history_filters_and_caps() {
    let mk = |role: &str, text: &str| HistoryMsg {
        role: role.into(),
        text: text.into(),
    };
    let h = vec![
        mk("system", "x"),
        mk("user", "a"),
        mk("assistant", ""),
        mk("assistant", "b"),
        mk("user", "c"),
    ];
    let t = tail_history(&h, 2);
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].text, "b");
    assert_eq!(t[1].text, "c");
}

/// 历史折叠的窗口语义：最近的 keep 轮一字不动，更老的正文换成一行事实；重复折叠无副作用
#[test]
fn fold_history_keeps_the_tail_and_shrinks_the_head() {
    let mut msgs = vec![ChatMessage::system("S"), ChatMessage::user("任务")];
    let mut slots = Vec::new();
    for i in 1..=4 {
        let assistant = msgs.len();
        msgs.push(ChatMessage::assistant(format!(
            r#"{{"tool":"read","args":{{"path":"f{i}"}}}}"#
        )));
        let result = msgs.len();
        msgs.push(ChatMessage::user(format!("正文{i}{}", "x".repeat(500))));
        slots.push(RoundSlot {
            assistant,
            result,
            call_digest: format!("CALL{i}"),
            outcome_digest: format!("OUT{i}"),
        });
    }

    let (folded, saved) = fold_history(&mut msgs, &slots, 2);
    assert_eq!(folded, 2, "4 轮保留 2 轮 → 该折 2 轮");
    assert!(saved > 1_000, "省下的正是那两段正文：{saved}");
    assert_eq!(msgs[slots[0].assistant].content, "CALL1");
    assert_eq!(msgs[slots[0].result].content, "OUT1");
    assert_eq!(msgs[slots[1].result].content, "OUT2", "窗口外的都要折");
    assert!(
        msgs[slots[2].result].content.contains("正文3"),
        "窗口内的轮次一字不动：{}",
        msgs[slots[2].result].content
    );
    assert!(msgs[slots[3].result].content.contains("正文4"));
    // 幂等：再折一次既不该"又折了几轮"，也不该再省字节
    assert_eq!(fold_history(&mut msgs, &slots, 2), (0, 0));
    // system 与任务消息不属于任何轮次，永远不动
    assert_eq!(msgs[0].content, "S");
    assert_eq!(msgs[1].content, "任务");
    // 一轮都不折的场合
    assert_eq!(fold_history(&mut msgs, &slots, 9), (0, 0));
}

/// 端到端：判据是**模型实际看到了什么**（请求体），不是我们自己的账本
#[test]
fn old_tool_results_are_folded_out_of_the_request() {
    let (d, llm) = fold_fixture("fold-e2e");
    let mut cfg = ask_cfg(&llm);
    cfg.agent.history_keep_rounds = 2;

    block_on(run(
        &cfg,
        &d.0,
        "把五个文件都读一遍",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    assert_eq!(llm.count(), 6, "5 轮读 + 1 轮交付，不该有多余轮次");
    // request(0) 是开跑前那次（只有 system + 任务），正文从第 2 次请求起才进上下文
    assert!(
        llm.request(1).contains("MARK1"),
        "第 1 轮读到的正文这一轮当然还在"
    );

    let last = llm.request(5);
    assert!(
        last.contains("MARK4") && last.contains("MARK5"),
        "窗口内的正文不该被动"
    );
    assert!(!last.contains("MARK1"), "第 1 轮的正文必须已经被折叠掉");
    assert!(!last.contains("MARK2") && !last.contains("MARK3"));
    assert!(
        last.contains("第 1 轮结果"),
        "折叠后仍要留下「读过什么」的痕迹：{last}"
    );
    // **措辞是判据的一部分**（ISSUE-8）：旧摘要在字面上邀请模型重读
    // （「正文已从上下文移除，需要时重新 read」），那正是 96 轮 0 结论的生成机制。
    // 现在必须把"要读就精确读"与"得出过结论就先记下来"讲清楚，而不是请它再读一遍。
    assert!(
        last.contains("正文已折叠"),
        "折叠后仍要说清正文去哪了：{last}"
    );
    assert!(
        last.contains("record_findings"),
        "折叠后的摘要必须指向进展记忆（结论先落，再谈重读）：{last}"
    );
    assert!(
        !last.contains("需要时重新 read"),
        "不许再出现「需要时重新 read」这种**邀请重读**的措辞：{last}"
    );
}

/// 一行回滚：关掉 history_trim，老轮次的正文照旧留在上下文里
#[test]
fn history_trim_off_restores_the_always_growing_history() {
    let (d, llm) = fold_fixture("fold-off");
    let mut cfg = ask_cfg(&llm);
    cfg.agent.history_keep_rounds = 2;
    cfg.agent.history_trim = false;

    block_on(run(
        &cfg,
        &d.0,
        "把五个文件都读一遍",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    let last = llm.request(5);
    assert!(
        last.contains("MARK1"),
        "关掉折叠后第 1 轮的正文仍该在（老行为）：开关没生效"
    );
    assert!(!last.contains("正文已从上下文移除"), "不该出现折叠标记");
}

/// 取代**不改历史**：旧条从提示词里退出、但仍留在账本里（`supersede` 的语义边界）。
#[test]
fn superseded_findings_leave_prompt_but_stay_in_ledger() {
    let mut p = prog();
    let a = p
        .record("旧结论：模型名来自硬编码表", "src/a.rs:12", "", None)
        .unwrap();
    let b = p
        .record(
            "新结论：模型名来自 ai_list_models",
            "ui/session.js:303",
            "",
            Some(&a),
        )
        .unwrap();
    assert_eq!((a.as_str(), b.as_str()), ("F1", "F2"));
    let block = p.render();
    assert!(block.contains("新结论"), "新条必须在场：{block}");
    assert!(
        !block.contains("旧结论"),
        "被取代的条不该再出现在提示词里：{block}"
    );
    assert_eq!(p.all().len(), 2, "**不物理删除**：两条都在账本里");
    assert_eq!(p.superseded_count(), 1);
    // 取代一个不存在的 id：必须当面拒（而不是默默新记一条）
    assert!(p.record("x", "y", "", Some("F99")).is_err());
    // 没有证据的断言一律拒 —— findings 的价值全在可核对
    assert!(p.record("只有结论没有证据", "   ", "", None).is_err());
    assert_eq!(p.all().len(), 2, "被拒的条目不该入库");
}

/// 超上限：最老的 active **落盘** + 提示词里留指针。落盘 ≠ 删除。
#[test]
fn findings_over_cap_spill_with_pointer() {
    let dir = std::env::temp_dir().join(format!("ruyix-findings-spill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut p = super::findings::Progress::new();
    p.set_cap(400); // 小上限逼它落盘
    for i in 0..12 {
        p.record(
            &format!("结论 {i}：这是一条足够长的结论用来把上限撑开，长度大约六十个字符左右吧"),
            &format!("src/f{i}.rs:{}", i * 10),
            "",
            None,
        )
        .unwrap();
    }
    let spilled = p.prompt_block(Some(&dir));
    assert!(spilled.is_some(), "超上限必须落盘");
    let block = p.render();
    assert!(block.contains("已落盘"), "提示词里要留指针：{block}");
    assert!(
        p.active_bytes() <= 400,
        "落盘后必须回到上限内：{}",
        p.active_bytes()
    );
    assert_eq!(p.all().len(), 12, "落盘的条目仍在账本里（不删除）");
    let file = dir.join("findings.md");
    let text = std::fs::read_to_string(&file).expect("落盘文件必须写出来");
    // 落盘文件里要能读到那些被移出提示词的条目（否则"指针"就是死链）
    assert!(text.contains("结论 0"), "最早的那条要落到文件里：{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 重复读守卫：同一 `(path, 区间)` 第二次读要**被指名**（区间相交也算）。
#[test]
fn repeated_read_of_same_range_is_called_out() {
    let mut p = prog();
    assert_eq!(
        p.note_read("ui/session.js", 296, 334, 12),
        None,
        "首读不该有意见"
    );
    // 重合区间（330-390 与 296-334 相交）也算"又读了一遍同一块地方"
    assert_eq!(p.note_read("ui/session.js", 330, 390, 40), Some(12));
    // 不同文件不算重复
    assert_eq!(p.note_read("src/main.rs", 1, 60, 41), None);
    // 提示里要说清是第几轮读过、以及当时有没有落结论
    let note = p.repeat_read_note("ui/session.js", 12);
    assert!(note.contains("第 12 轮"), "{note}");
    assert!(
        note.contains("record_findings"),
        "要告诉它该做什么，而不只是「你读过了」：{note}"
    );
    p.record(
        "会话工具栏的模型名来自 wrap._webCaps.model",
        "ui/session.js:303",
        "",
        None,
    )
    .unwrap();
    let note2 = p.repeat_read_note("ui/session.js", 12);
    assert!(
        note2.contains("F1"),
        "有相关结论时要点名（模型据此决定要不要取代）：{note2}"
    );
}

/// 停滞判据：只数"既没有新结论、也没有文件变更"的**连续**轮次。
#[test]
fn stall_counts_only_rounds_without_progress() {
    let mut p = prog();
    for r in 1..=3 {
        p.note_round(r, false, false, false);
    }
    assert_eq!(p.stalled(3), 3);
    // 第 4 轮写了文件 ⇒ 进展清零
    p.note_round(4, true, false, false);
    assert_eq!(p.stalled(4), 0, "有新结论就算进展");
    for r in 5..=9 {
        p.note_round(r, false, false, false);
    }
    assert_eq!(p.stalled(9), 5, "从第 4 轮之后开始数");
}

/// **v1.1 §8-5 的判据**：跑完的 `AgentOutcome.findings` 是**完整账本** —— 被取代的那条也在，
/// 且带 id / claim / evidence。宿主据此写进会话存档，用户重开会话才看得见"模型凭什么那么改"
/// （不进 outcome 就等于跑完即焚 —— 当年拍了「是」却一直没落的那一条）。
#[test]
fn the_outcome_carries_the_findings_ledger_including_superseded_ones() {
    let d = TempDir::new("findings-outcome");
    d.write("a.rs", "let x = 1;\n");
    let cfg = quiet_cfg();
    let (_llm, out, _log) = block_on(run_logging(
        &cfg,
        &d.0,
        vec![
            r#"{"tool":"record_findings","args":{"items":[{"claim":"a.rs 里 x=1","evidence":"a.rs:1"}]}}"#
                .into(),
            r#"{"tool":"record_findings","args":{"items":[{"claim":"已改成 x=2","evidence":"a.rs:1","supersedes":"F1"}]}}"#
                .into(),
            r#"{"final":"记了两条"}"#.into(),
        ],
    ));
    assert_eq!(out.findings.len(), 2, "完整账本：被取代的那条也要在");
    assert_eq!(out.findings[0].id, "F1");
    assert_eq!(out.findings[0].superseded_by.as_deref(), Some("F2"));
    assert_eq!(out.findings[1].claim, "已改成 x=2");
    assert!(
        out.findings.iter().all(|f| !f.evidence.is_empty()),
        "证据是指针，不许空"
    );
}
