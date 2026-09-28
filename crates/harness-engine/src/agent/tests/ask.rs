//! 提问通道 `ask_user`：伪造的答案要被拒、没答到一律 fail-closed、次数有上限、
//! 关闭时引擎自己拒绝（不许指望模型自律）。

use super::*;

/// 解析：形状认全、别名认、四种坏输入当面拒（含**不许自问自答**）
#[test]
fn ask_user_parses_and_rejects_forgeries() {
    let a = parse_one(&serde_json::json!({
        "tool": "ask_user",
        "args": {"question": "登哪台机器？", "why": "凭据放哪一侧",
                 "options": ["甲", "乙"], "default_index": 1}
    }))
    .expect("标准形状该能解析");
    match a {
        Action::Ask(s) => {
            assert_eq!(s.question, "登哪台机器？");
            assert_eq!(s.why, "凭据放哪一侧");
            assert_eq!(s.options, vec!["甲", "乙"]);
            assert_eq!(s.default_index, Some(1));
            // 超时由引擎按配置填：模型给的会被忽略（"等多久"是委托人的权力）
            assert_eq!(s.timeout_secs, 0);
        }
        other => panic!("该是 Ask：{other:?}"),
    }
    // 别名 ask 也认
    assert!(matches!(
        parse_one(&serde_json::json!({"tool": "ask", "args": {"question": "q", "why": "w"}}))
            .unwrap(),
        Action::Ask(_)
    ));
    // 缺 why：可以问，但必须说清"这个答案会决定什么"（UI 要原样展示给用户）
    let e =
        parse_one(&serde_json::json!({"tool": "ask_user", "args": {"question": "q"}})).unwrap_err();
    assert!(e.contains("why"), "{e}");
    // 自问自答：模型不许自带答案（答案只能从宿主的用户通道进来）
    let e = parse_one(&serde_json::json!({
        "tool": "ask_user",
        "args": {"question": "q", "why": "w", "answer": "当然可以"}
    }))
    .unwrap_err();
    assert!(e.contains("不许有 answer"), "{e}");
    // 选项太多 / default 越界
    let e = parse_one(&serde_json::json!({
        "tool": "ask_user",
        "args": {"question": "q", "why": "w", "options": ["1", "2", "3", "4", "5", "6"]}
    }))
    .unwrap_err();
    assert!(e.contains("最多 5 个"), "{e}");
    let e = parse_one(&serde_json::json!({
        "tool": "ask_user",
        "args": {"question": "q", "why": "w", "options": ["1"], "default_index": 3}
    }))
    .unwrap_err();
    assert!(e.contains("越界"), "{e}");
}

/// 控制动作的纪律：**不许进批**、**子步骤没有交互权**
#[test]
fn ask_user_never_enters_a_batch_nor_a_step() {
    let raw = r#"{"actions":[{"tool":"read","args":{"path":"a.txt"}},{"tool":"ask_user","args":{"question":"q","why":"w"}}]}"#;
    let e = parse_actions(raw, 8, true).unwrap_err();
    assert!(e.contains("ask_user 不能放进批里"), "{e}");
    // 子步骤：收窄成 Unsupported（回一条明确拒绝，不让整步失败）
    let steps = parse_step_actions(
        r#"{"tool":"ask_user","args":{"question":"q","why":"w"}}"#,
        8,
        true,
    )
    .unwrap();
    assert!(matches!(steps[0], StepAction::Unsupported("ask_user")));
    // 提示词也要各自自洽：主循环提、子步骤不提（提了它就会去找一个自己没有的能力）
    assert!(batch_hint(8, true).contains("ask_user"));
    assert!(!batch_hint(8, false).contains("ask_user"));
    assert!(ask_hint(&AppConfig::default()).contains("ask_user"));
}

/// 主线：模型问 → 用户答 → run **不中断**，答案以观察回灌，工作继续
#[test]
fn ask_user_gets_the_answer_and_keeps_working() {
    let d = TempDir::new("ask-answer");
    let llm = crate::testllm::fake_llm(vec![
        ask_json(
            "远程登录要连的是哪种机器？",
            "这决定了凭据放哪一侧",
            &["公司托管服务器", "用户自己另一台电脑"],
        ),
        serde_json::json!({
            "tool": "write",
            "args": {"path": "login.md", "content": "方案A：用户自己另一台电脑"}
        })
        .to_string(),
        r#"{"final":"已按你的选择实现"}"#.into(),
    ]);
    let cfg = ask_cfg(&llm);
    let asker = ScriptAsker::new(vec![Ok("用户自己另一台电脑")]);

    let out = block_on(run_with_ask(
        &cfg,
        &d.0,
        "做一个远程登录功能",
        &[],
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &asker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    // 问到的东西：id 从 1 开始、问题与 why 原样带出去
    assert_eq!(asker.count(), 1, "只该问一次");
    let (id, q, why) = &asker.asked()[0];
    assert_eq!(id, "ask-1");
    assert_eq!(q, "远程登录要连的是哪种机器？");
    assert_eq!(why, "这决定了凭据放哪一侧");
    // 留痕进结果（宿主写进会话存档，重开会话仍看得见）
    assert_eq!(out.asks.len(), 1);
    assert_eq!(out.asks[0].state, "answered");
    assert_eq!(out.asks[0].answer.as_deref(), Some("用户自己另一台电脑"));
    // 答案真的进了模型上下文（第 2 轮请求体里能看到），且标注了"不是授权"
    let req2 = llm.request(1);
    assert!(req2.contains("用户自己另一台电脑"), "第 2 轮该看到答案");
    assert!(
        req2.contains("不构成任何门禁的授权"),
        "回灌必须写明它不是授权"
    );
    // 工作继续：写下来了、交付了
    let written = std::fs::read_to_string(d.0.join("login.md")).unwrap();
    assert!(written.contains("用户自己另一台电脑"), "{written}");
    assert!(out.answer.contains("已按你的选择实现"), "{}", out.answer);
    assert_eq!(llm.count(), 3, "问 + 写 + 交付");
}

/// fail-closed：没人回答**不是同意** —— 拒绝依赖它的动作，要求交付并写清假设
#[test]
fn ask_without_an_answer_is_fail_closed() {
    let d = TempDir::new("ask-timeout");
    let llm = crate::testllm::fake_llm(vec![
        r#"{"tool":"ask_user","args":{"question":"登哪台机器？","why":"决定凭据放哪一侧"}}"#.into(),
        r#"{"final":"我按假设实现（未得到你的回答）"}"#.into(),
    ]);
    let cfg = ask_cfg(&llm);
    let asker = ScriptAsker::new(vec![Err(AskErr::Timeout)]);

    let out = block_on(run_with_ask(
        &cfg,
        &d.0,
        "做一个远程登录功能",
        &[],
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &asker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("没人回答不该让整个 run 失败");

    assert_eq!(out.asks[0].state, "timeout");
    assert!(out.asks[0].answer.is_none());
    // 模型看到的是一条"这不是同意"的观察 —— 而不是沉默（沉默会被当成默许）
    let req2 = llm.request(1);
    assert!(
        req2.contains("没有得到回答"),
        "{}",
        &req2[..400.min(req2.len())]
    );
    assert!(req2.contains("不许做"), "fail-closed 必须写明依赖动作被拒");
    assert!(out.answer.contains("未得到你的回答"), "{}", out.answer);
}

/// 无提问通道（headless / eval）：同样 fail-closed，且**不假装有人回答**
#[test]
fn without_an_asker_the_answer_is_fail_closed() {
    let d = TempDir::new("ask-noasker");
    let llm = crate::testllm::fake_llm(vec![
        r#"{"tool":"ask_user","args":{"question":"登哪台机器？","why":"决定凭据放哪一侧"}}"#.into(),
        r#"{"final":"按假设实现"}"#.into(),
    ]);
    let cfg = ask_cfg(&llm);
    // run（不带通道）＝ headless 形态：内部就是 &NoAsker
    let out = block_on(run(
        &cfg,
        &d.0,
        "做一个远程登录功能",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");
    assert_eq!(out.asks[0].state, "no_asker");
    assert!(out.asks[0].answer.is_none(), "绝不假装有人回答");
    assert!(llm.request(1).contains("没有提问通道") || llm.request(1).contains("没有得到回答"));
}

/// 稀缺资源硬闸：一次 run 最多问 N 次，超了直接拒并要求交付 + 声明假设
#[test]
fn ask_count_is_capped_per_run() {
    let d = TempDir::new("ask-cap");
    let llm = crate::testllm::fake_llm(vec![
        r#"{"tool":"ask_user","args":{"question":"问题一？","why":"w1"}}"#.into(),
        r#"{"tool":"ask_user","args":{"question":"问题二？","why":"w2"}}"#.into(),
        r#"{"final":"按假设交付"}"#.into(),
    ]);
    let mut cfg = ask_cfg(&llm);
    cfg.ask.max_per_run = 1;
    let asker = ScriptAsker::new(vec![Ok("答案一")]);

    let out = block_on(run_with_ask(
        &cfg,
        &d.0,
        "任务",
        &[],
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &asker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    assert_eq!(asker.count(), 1, "第二次不许再问出去");
    assert_eq!(out.asks.len(), 1, "被拒的那次不留问答痕迹（没问到人）");
    let req3 = llm.request(2);
    assert!(req3.contains("次数已用完"), "第 3 轮该看到上限提示");
    assert!(out.answer.contains("按假设交付"), "{}", out.answer);
}

/// 开关关掉：引擎一律拒（提示词那句也一字不提，两处同一个开关）
#[test]
fn ask_off_means_the_engine_refuses_it() {
    let d = TempDir::new("ask-off");
    let llm = crate::testllm::fake_llm(vec![
        r#"{"tool":"ask_user","args":{"question":"登哪台机器？","why":"决定凭据放哪一侧"}}"#.into(),
        r#"{"final":"按假设交付"}"#.into(),
    ]);
    let mut cfg = ask_cfg(&llm);
    cfg.ask.enabled = false;
    let asker = ScriptAsker::new(vec![Ok("不该被问到")]);

    let out = block_on(run_with_ask(
        &cfg,
        &d.0,
        "任务",
        &[],
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &asker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    assert_eq!(asker.count(), 0, "关掉后不许把问题递到用户面前");
    assert!(out.asks.is_empty());
    assert!(
        llm.request(1).contains("已关闭"),
        "该回一条「不许问」的观察"
    );
    // 提示词侧：开关关掉时 head 不该出现提问那段
    assert!(
        !llm.request(0).contains("需求歧义："),
        "关掉后提示词一字不提"
    );
}
