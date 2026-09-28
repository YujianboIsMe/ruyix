//! 动作解析：八种工具 → `Action`，以及解析不出来时的**纠偏**。
//!
//! 覆盖 `action.rs` 的协议面：工具名与参数形状、DSML 标记的剥与不剥、内容通道的关与回滚、
//! 截断 vs 格式错的两种纠偏方向、批量的声明序与控制动作拒绝。
//!
//! 夹具（`run_loop` / `ScriptAsker` / `TempDir` …）在父模块 `tests.rs` 里，这里 `use super::*` 取。

use super::*;

#[test]
fn parse_action_covers_all_tools() {
    assert!(matches!(
        parse_action(r#"{"final":"完成了"}"#),
        Ok(Action::Final(t)) if t == "完成了"
    ));
    assert!(matches!(
        parse_action(r#"{"tool":"read","args":{"path":"src/"}}"#),
        Ok(Action::Read(s)) if s.path == "src/" && !s.is_window()
    ));
    assert!(matches!(
        parse_action(r#"{"tool":"write","args":{"path":"a.py","content":"x=1"}}"#),
        Ok(Action::Write(s)) if s.path == "a.py"
            && matches!(&s.body, WriteBody::Content(c) if c == "x=1")
    ));
    assert!(matches!(
        parse_action(r#"{"tool":"execute","args":{"cmd":"cargo test","timeout_secs":60}}"#),
        Ok(Action::Execute(c, Some(60))) if c == "cargo test"
    ));
    // 别名：老历史里写着 bash 的一轮不该白烧（同一条 Execute 通路）
    assert!(matches!(
        parse_action(r#"{"tool":"bash","args":{"cmd":"ls"}}"#),
        Ok(Action::Execute(c, None)) if c == "ls"
    ));
    assert!(matches!(
        parse_action(r#"{"tool":"plan","args":{"steps":[{"title":"写模块","files":["m.py"]}]}}"#),
        Ok(Action::Plan(s)) if s.len() == 1 && s[0].id == 1
    ));
}

/// 模型自带协议标记（DSML）泄露：标记被剥掉之后，**裹在里面的动作要照样认出来** ——
/// 实测 2026-09-21 第 5 轮，模型把参数 JSON 裹在它自己那套工具调用标记里，引擎只报一句
/// "未知能力"，模型换了个说法重发同一坨，白烧一轮（剥标记见 `llm::strip_model_markup`）。
#[test]
fn dsml_markup_around_an_action_is_stripped_not_reported_as_unknown() {
    let bar = "\u{ff5c}";
    let leak = [
        r#"{"actions":[{"tool":"execute","args":{"cmd":"netstat -ano | findstr \"8083\""}}]}"#,
        &format!("</{0}{0}DSML{0}{0} calls>", bar),
    ]
    .join("\n");
    let acts = parse_actions(&leak, 8, true).expect("剥掉标记后应当解析成动作");
    assert!(matches!(&acts[0], Action::Execute(c, _) if c.contains("findstr")));
}

/// 只发了 args 那一层（`{"cmd": …}`）：报错要指向**外层缺 `tool`**，而不是"未知能力 \"\""——
/// 后者对模型毫无指向性（那次白烧一轮的直接原因）。
#[test]
fn args_only_object_is_told_to_wrap_it_in_a_tool_field() {
    let err = parse_action(r#"{"cmd": "dir"}"#).unwrap_err();
    assert!(err.contains("缺 `tool`"), "{err}");
    assert!(!err.contains("未知能力"), "缺 tool 不该报成未知能力: {err}");
}

/// **bug 5 的回归**（用户实测 2026-09-25）：发一句"你好"，模型每轮都不发工具调用，而引擎只
/// 回一句拒绝就 `continue` —— 兜底是 `MAX_STEPS`（96 轮）与墙钟闸（30 分钟），用户看到的现象
/// 就是**无限死循环**（日志里同一句 warn 一行接一行）。
///
/// 判据三条：
/// ① **三轮之内就停**（不再空烧预算，也不消费第 4 条回复）；
/// ② 停下来是**失败**（`Err`），不是静默的成功 —— 用户必须知道这一轮没跑成；
/// ③ 诊断要指向"接下来怎么办"：点明连续几轮 + 原因（模型/端点侧没走工具调用）+ 现成的回滚开关
///    `ruyix.code.ai.tool_protocol` + 最近一轮的原文片段（否则用户只能对着日志猜模型发了什么）。
#[test]
fn content_only_model_stops_after_the_cap_instead_of_spinning() {
    let d = TempDir::new("unparsed-cap");
    let llm = crate::testllm::fake_llm(vec![
        "你好！有什么可以帮你的？".into(),
        "我可以帮你读文件、改代码。".into(),
        "你想让我做什么？".into(),
        "（第 4 条不该被消费到）".into(),
    ]);
    let cfg = ask_cfg(&llm);

    let err = block_on(run_with_ask(
        &cfg,
        &d.0,
        "你好",
        &[],
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &NoAsker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect_err("三轮都拿不到工具调用 ⇒ 必须失败，而不是继续转");

    assert_eq!(
        llm.count(),
        MAX_UNPARSEABLE_ROUNDS,
        "连续失败到上限就该停，不该再要第 4 条回复"
    );
    assert!(err.contains("连续 3 轮"), "要点明连续几轮：{err}");
    assert!(
        err.contains("tool_protocol"),
        "要给出那条现成的回滚开关（否则用户只能猜）：{err}"
    );
    assert!(
        err.contains("你想让我做什么？"),
        "要带上**最近一轮**的原文片段（不是第一轮的），用户才看得见模型最后发的到底是什么：{err}"
    );
}

/// 端到端复刻那次失败：模型把动作**只发了参数那一层**出来，后面挂着它自己的协议闭合标记
/// （实测 2026-09-21 第 5 轮的原样：`{"cmd": …}` + 三行闭合标记，模型把工具名丢在自己的标记里）。
///
/// 判据放在**回灌给模型的那句话**上 —— 这是本轮唯一能改的东西：旧的报错是"未知能力 \"\""，
/// 模型看不出自己错在哪，只会换个说法重发；现在必须同时给到两条指向：
/// ① 外层缺 `tool`（连带着把它发的参数原样贴回去）② 检测到你发的是自带协议标记、已剥掉。
/// 然后模型照形状重发一轮，动作落地 —— 这才叫"烧一轮换一次纠正"，而不是原地打转。
#[test]
fn a_headless_dsml_call_gets_told_what_is_missing_and_recovers() {
    let d = TempDir::new("dsml-recover");
    let bar = "\u{ff5c}";
    let leaked = [
        r#"{"cmd": "echo e2e"}"#,
        &format!("</{0}{0}DSML{0}{0} parameter>", bar),
        &format!("</{0}{0}DSML{0}{0} invoke>", bar),
        &format!("</{0}{0}DSML{0}{0} calls>", bar),
    ]
    .join("\n");
    let llm = crate::testllm::fake_llm(vec![
        leaked,
        r#"{"tool":"write","args":{"path":"nm.txt","content":"ok"}}"#.into(),
        r#"{"final":"写好了"}"#.into(),
    ]);
    let cfg = ask_cfg(&llm);

    let out = block_on(run_with_ask(
        &cfg,
        &d.0,
        "写一个文件",
        &[],
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &NoAsker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    // ① **认出形状再拒**（严格模式）：它发的是 execute 的参数对象，得点名说出来 ——
    //    真跑那五轮里引擎只回一句"外层缺 tool"，模型根本不知道自己哪一处错了。
    let feedback = llm.request(1);
    assert!(
        feedback.contains("execute 的参数"),
        "要点名它发的是哪个工具的参数：{feedback}"
    );
    assert!(
        feedback.contains("通道错了") || feedback.contains("只认工具调用"),
        "要说清「形状对、通道错」：{feedback}"
    );
    assert!(
        !feedback.contains("不执行") || feedback.contains("content"),
        "要把「content 不执行」讲明白：{feedback}"
    );
    // ② 模型自带的标记被剥掉这件事必须点明，否则它不知道自己错在哪。
    // 断言用"已剥掉"而不是"工具调用标记"—— 后者在系统提示词里也有（规则 1 有同一句话），
    // 拿它当判据会被提示词本身满足，等于没测这条注脚。
    assert!(
        feedback.contains("已剥掉"),
        "必须点明是模型自带的工具调用标记被剥掉：{feedback}"
    );
    // 落在磁盘上的事实：拒掉那一轮之后，正规工具调用让动作真的跑了
    assert_eq!(std::fs::read_to_string(d.0.join("nm.txt")).unwrap(), "ok");
    assert!(out.answer.contains("写好了"), "{}", out.answer);
    assert_eq!(llm.count(), 3, "content 一轮被拒 → 工具调用一轮 → 交付");
}

/// **标准工具协议**（v0.0.6 主路）：一轮 `tool_calls` → 动作照跑；模型下一轮**看得见**自己
/// 发过的那串调用（工具轮 content 天生是空的，回显见 `tool_calls_echo`）。
#[test]
fn tool_calls_round_runs_and_the_model_sees_its_own_call() {
    let d = TempDir::new("tools-basic");
    d.write("a.txt", "AAA");
    let llm = crate::testllm::fake_llm(vec![
        crate::testllm::tool_script(&[("read", serde_json::json!({"path": "a.txt"}))]),
        r#"{"final":"读完了"}"#.into(),
    ]);
    let cfg = ask_cfg(&llm);
    let out = run_loop(&cfg, &d.0);

    assert_eq!(llm.count(), 2, "工具轮只该占一轮（读 + 交付）");
    assert!(out.answer.contains("读完了"), "{}", out.answer);
    // 请求里真的声明了工具（不开这个开关等于没改造）
    let req1 = llm.request(0);
    assert!(req1.contains("\"tools\""), "第 1 轮必须声明 tools");
    assert!(req1.contains("\"read\""), "四个能力都要在声明里: {req1}");
    // 第 2 轮：模型看得到自己发过什么 + 拿得到读到的内容
    let req2 = llm.request(1);
    assert!(
        req2.contains("〔已发出调用〕"),
        "工具轮的 content 是空的，必须把它的调用记回去（流水文字，不是 JSON 信封）: {req2}"
    );
    assert!(
        req2.contains("read(path="),
        "回显要写清调了什么、参数是什么: {req2}"
    );
    assert!(
        !req2.contains("\"tool_calls\""),
        "回显**不能**是 JSON 信封：那是个「能发」的形状，模型会照抄进 content（真跑 73 轮里 2 轮 + \
         新会话第 6 轮就是这么坏的）: {req2}"
    );
    assert!(req2.contains("a.txt"), "观察结果要带上文件内容: {req2}");
}

/// 一轮里发多个工具调用 = **批量调用**（顺序即声明顺序，波次并发照旧）
#[test]
fn several_tool_calls_in_one_round_are_one_batch() {
    let d = TempDir::new("tools-batch");
    d.write("a.txt", "AAA");
    d.write("b.txt", "BBB");
    let llm = crate::testllm::fake_llm(vec![
        crate::testllm::tool_script(&[
            ("read", serde_json::json!({"path": "a.txt"})),
            ("read", serde_json::json!({"path": "b.txt"})),
        ]),
        r#"{"final":"两个都读了"}"#.into(),
    ]);
    let cfg = ask_cfg(&llm);
    let out = run_loop(&cfg, &d.0);

    assert_eq!(llm.count(), 2, "两条 read 一轮发完，仍然只占一轮");
    assert!(out.answer.contains("两个都读了"), "{}", out.answer);
    let req2 = llm.request(1);
    assert!(
        req2.contains("a.txt") && req2.contains("b.txt"),
        "两条观察都要回灌: {req2}"
    );
}

/// 交付：`final` 工具调用（不是"第五种能力"，是收尾）
#[test]
fn final_as_a_tool_call_delivers_in_one_round() {
    let d = TempDir::new("tools-final");
    let llm = crate::testllm::fake_llm(vec![crate::testllm::tool_script(&[(
        "final",
        serde_json::json!({"answer": "做完了"}),
    )])]);
    let cfg = ask_cfg(&llm);
    let out = run_loop(&cfg, &d.0);
    assert_eq!(llm.count(), 1);
    assert!(out.answer.contains("做完了"), "{}", out.answer);
}

/// 批开关关掉：一轮多个工具调用**当面拒**，并给一句模型能照做的纠正
#[test]
fn batch_off_refuses_several_tool_calls() {
    let d = TempDir::new("tools-nobatch");
    let llm = crate::testllm::fake_llm(vec![
        crate::testllm::tool_script(&[
            ("read", serde_json::json!({"path": "a.txt"})),
            ("read", serde_json::json!({"path": "a.txt"})),
        ]),
        r#"{"final":"好"}"#.into(),
    ]);
    let mut cfg = ask_cfg(&llm);
    cfg.agent.batch = false;
    let out = run_loop(&cfg, &d.0);

    assert_eq!(llm.count(), 2, "拒绝也是一轮，之后模型照要求重来");
    assert!(out.answer.contains("好"), "{}", out.answer);
    let req2 = llm.request(1);
    assert!(req2.contains("不允许批量调用"), "{req2}");
}

/// **严格模式**：content 里的动作**不执行** —— 拒掉、点名、给一句能照做的纠正；改走工具调用才跑。
#[test]
fn strict_mode_refuses_content_actions_and_names_them() {
    let d = TempDir::new("strict-refuse");
    let llm = crate::testllm::fake_llm_raw(vec![
        // 第 1 轮：老协议形状的 write（content 通道）→ 目标文件必须**从未**出现
        // （用另一个文件名：后面那一轮工具调用会写 b.txt，同名字就分不清是谁写的了）
        r#"{"tool":"write","args":{"path":"never.txt","content":"BBB"}}"#.into(),
        // 第 2 轮：同一个动作改走工具调用 → 落盘
        crate::testllm::tool_script(&[(
            "write",
            serde_json::json!({"path": "b.txt", "content": "BBB"}),
        )]),
        crate::testllm::tool_script(&[("final", serde_json::json!({"answer": "写好了"}))]),
    ]);
    let cfg = ask_cfg(&llm);
    let out = run_loop(&cfg, &d.0);

    assert!(
        !d.0.join("never.txt").exists(),
        "严格模式下 content 通道的动作一个字节都不许落盘"
    );
    let fb = llm.request(1);
    assert!(fb.contains("write"), "要点名它发的是 write：{fb}");
    assert!(fb.contains("工具调用"), "{fb}");
    assert_eq!(
        std::fs::read_to_string(d.0.join("b.txt")).unwrap(),
        "BBB",
        "改走工具调用之后才落盘"
    );
    assert!(out.answer.contains("写好了"), "{}", out.answer);
    assert_eq!(llm.count(), 3, "content 被拒 → 工具调用 → 交付");
}

/// **抄了回显流水**：模型把历史里那条流水（如今是 `〔已发出调用〕…`；从前是 JSON 信封）
/// 直接写进 content —— 2026-09-24 新会话第 6 轮的原样复刻（`finish_reason=stop`、`tool_calls=0`）。
/// 必须被拒、**不执行**，且纠正话术要**点名这个病因**（泛泛说"没有工具调用"治不了它），
/// 但**不能把那个信封抄回去**（引用等于又教一遍）。
#[test]
fn a_copied_record_is_refused_and_named_without_quoting_it() {
    let raw = r#"{"tool_calls":[{"arguments":"{\"path\": \"a.txt\"}","name":"read"}]}"#;
    let msg = content_channel_error(raw, 1, MAIN_TOOLS_HINT);
    assert!(msg.contains("流水"), "要点名是抄了流水：{msg}");
    assert!(msg.contains("记录"), "{msg}");
    assert!(msg.contains("工具调用"), "{msg}");
    assert!(
        !msg.contains("\"tool_calls\""),
        "不许把信封抄回去（引用等于又教一遍）：{msg}"
    );

    // 端到端：被拒的那一轮**不执行**，改成真工具调用才跑
    let d = TempDir::new("copied-record");
    d.write("a.txt", "AAA");
    let llm = crate::testllm::fake_llm_raw(vec![
        raw.into(),
        crate::testllm::tool_script(&[("read", serde_json::json!({"path": "a.txt"}))]),
        crate::testllm::tool_script(&[("final", serde_json::json!({"answer": "读完了"}))]),
    ]);
    let cfg = ask_cfg(&llm);
    let out = run_loop(&cfg, &d.0);

    let fb = llm.request(1);
    assert!(fb.contains("流水"), "回灌要指出病因：{fb}");
    assert!(out.answer.contains("读完了"), "{}", out.answer);
    assert_eq!(llm.count(), 3, "抄流水一轮被拒 → 真工具调用 → 交付");
}

/// 真跑里那几轮的形状（2026-09-24 cloud-shop 启动前后端）逐一复刻：引擎必须**认出**并
/// **拒掉**，而不是回一句"外层缺 tool"让模型继续换包装瞎猜（那次连烧五轮）。
#[test]
fn the_real_run_shapes_are_each_named_and_refused() {
    let cases: &[(&str, &str)] = &[
        // 第 13 轮：只剩 final 的参数对象
        (r#"{"answer": "前后端都已启动"}"#, "final"),
        // 第 14/15/17 轮：工具名 + 老键名
        (
            r#"{"tool":"final","args":{"answer":"前后端都已启动"}}"#,
            "final",
        ),
        // 第 16 轮：协议的字段名
        (
            r#"{"name":"final","arguments":{"answer":"前后端都已启动"}}"#,
            "final",
        ),
        // 外层漏了名字、只剩 arguments 包装
        (r#"{"arguments":{"answer":"前后端都已启动"}}"#, "final"),
        // json_mode 截头后只剩参数
        (r#"{"cmd":"netstat -ano | findstr :8083"}"#, "execute"),
        (r#"{"path":"cloud-shop-admin-web/vite.config.js"}"#, "read"),
    ];
    for (raw, want) in cases {
        let msg = content_channel_error(raw, 1, MAIN_TOOLS_HINT);
        assert!(msg.contains(want), "「{raw}」该被点名成 {want}：{msg}");
        assert!(msg.contains("工具调用"), "{msg}");
        assert!(msg.contains("不执行"), "{msg}");
        assert!(!msg.contains("外层缺"), "不许再回那句没有指向性的话：{msg}");
    }
    // 第 2 次起只说短话（"提示一次即拒"）
    let short = content_channel_error(r#"{"path":"a.txt"}"#, 2, MAIN_TOOLS_HINT);
    assert!(short.contains("第 2 次"), "{short}");
    assert!(short.len() < 200, "第 2 次要短：{short}");
    // 步骤执行体的名单不许出现它没有的能力（父子提示词各自自洽）
    let step = content_channel_error(r#"{"cmd":"ls"}"#, 1, STEP_TOOLS_HINT);
    assert!(
        !step.contains("connect") && !step.contains("plan"),
        "{step}"
    );
}

/// 一行回滚：`llm.tool_protocol = false` → 请求里不提工具，content 通道恢复执行（老协议）。
#[test]
fn rollback_switch_restores_the_content_channel() {
    let d = TempDir::new("strict-off");
    let llm = crate::testllm::fake_llm_raw(vec![
        r#"{"tool":"write","args":{"path":"b.txt","content":"BBB"}}"#.into(),
        r#"{"final":"老路也能交付"}"#.into(),
    ]);
    let mut cfg = ask_cfg(&llm);
    cfg.llm.tool_protocol = false;
    let out = run_loop(&cfg, &d.0);

    assert!(
        !llm.request(0).contains("\"tools\""),
        "关掉开关就不该声明工具"
    );
    assert_eq!(
        std::fs::read_to_string(d.0.join("b.txt")).unwrap(),
        "BBB",
        "回滚之后 content 通道要照旧执行"
    );
    assert!(out.answer.contains("老路也能交付"), "{}", out.answer);
}

/// connect 的三形态：省略 action = list；call 要 server+tool；send 要 agent+text
#[test]
fn parse_action_covers_connect() {
    assert!(matches!(
        parse_action(r#"{"tool":"connect","args":{"action":"list"}}"#),
        Ok(Action::Connect(ConnectAction::List))
    ));
    assert!(matches!(
        parse_action(r#"{"tool":"connect","args":{}}"#),
        Ok(Action::Connect(ConnectAction::List))
    ));
    assert!(matches!(
        parse_action(
            r#"{"tool":"connect","args":{"action":"call","server":"fs","tool":"read_file","arguments":{"path":"a"}}}"#
        ),
        Ok(Action::Connect(ConnectAction::Call { server, tool, .. })) if server == "fs" && tool == "read_file"
    ));
    assert!(matches!(
        parse_action(r#"{"tool":"connect","args":{"action":"send","agent":"翻译","text":"你好"}}"#),
        Ok(Action::Connect(ConnectAction::Send { agent, text })) if agent == "翻译" && text == "你好"
    ));
    assert!(parse_action(r#"{"tool":"connect","args":{"action":"call","server":"fs"}}"#).is_err());
    assert!(parse_action(r#"{"tool":"connect","args":{"action":"fly"}}"#).is_err());
}

#[test]
fn parse_action_rejects_garbage() {
    assert!(parse_action("随便聊聊").is_err());
    assert!(parse_action(r#"{"tool":"fly"}"#).is_err());
    assert!(parse_action(r#"{"tool":"read","args":{}}"#).is_err());
    assert!(parse_action(r#"{"final":""}"#).is_err());
    // edit 已从能力集移除：模型若照老习惯发 edit，走"未知能力"通道让它改用 write
    assert!(parse_action(r#"{"tool":"edit","args":{"edits":[]}}"#).is_err());
}

/// 解析失败的回灌：截断叫写短、格式烂叫重发；错误文本带引号时
/// 反馈自己仍必须是合法 JSON（老实现裸拼会碎）
#[test]
fn parse_feedback_distinguishes_truncation() {
    let trunc = parse_failure_feedback("EOF while parsing an object", Some("length"), true);
    assert!(trunc.contains("max_tokens 截断"));
    assert!(trunc.contains("精简"));
    assert!(serde_json::from_str::<serde_json::Value>(&trunc).is_ok());

    // 报错文本里带引号和花括号（parse_action 的真实输出形态）
    let messy = parse_failure_feedback(
        "输出不是合法 JSON: ...；片段: {\"tool\":\"plan\",",
        None,
        false,
    );
    assert!(messy.contains("请重新只输出一个 JSON 对象"));
    assert!(serde_json::from_str::<serde_json::Value>(&messy).is_ok());

    // stop 是正常收笔，不算截断
    let stop = parse_failure_feedback("烂格式", Some("stop"), false);
    assert!(stop.contains("请重新只输出一个 JSON 对象"));
    assert!(!stop.contains("max_tokens"));
    assert!(serde_json::from_str::<serde_json::Value>(&stop).is_ok());

    // **严格模式（tools=true）不能说"请重新只输出一个 JSON 对象"** —— 那句话把模型推回
    // content 通道，而严格模式恰恰不执行 content；两条提示打架正是五轮空转的成因。
    let strict = parse_failure_feedback("烂格式", Some("stop"), true);
    assert!(strict.contains("工具调用"), "{strict}");
    assert!(
        !strict.contains("只输出一个 JSON 对象"),
        "严格模式不许再教老协议：{strict}"
    );
    assert!(serde_json::from_str::<serde_json::Value>(&strict).is_ok());
}

/// execute 的三副面孔：前台（原样）/ 后台托管 / 句柄操作。同一个工具，三种生命周期。
#[test]
fn parse_action_accepts_the_three_execute_lifetimes() {
    // ① 前台：一个字节都没变（老历史里的输出必须继续解析得动）
    assert!(matches!(
        parse_action(r#"{"tool":"execute","args":{"cmd":"ls"}}"#).unwrap(),
        Action::Execute(c, None) if c == "ls"
    ));
    // bash 别名仍然是同一个动作
    assert!(matches!(
        parse_action(r#"{"tool":"bash","args":{"cmd":"ls"}}"#).unwrap(),
        Action::Execute(_, _)
    ));

    // ② 后台：cmd + background 加就绪判据
    let raw = r#"{"tool":"execute","args":{"cmd":"mvn spring-boot:run","background":true,"ready_cmd":"netstat -ano | findstr :8083","ready_timeout_secs":90}}"#;
    match parse_action(raw).unwrap() {
        Action::ExecBg(s) => {
            assert_eq!(s.cmd, "mvn spring-boot:run");
            assert_eq!(s.ready_cmd.as_deref(), Some("netstat -ano | findstr :8083"));
            assert_eq!(s.ready_timeout_secs, Some(90));
            assert!(!s.keep_alive, "keep_alive 默认必须是 false");
        }
        other => panic!("应当是后台启动，实际 {other:?}"),
    }
    // 空白判据 = 没给判据（起完即返），不能变成一个永远跑不通的空命令
    match parse_action(
        r#"{"tool":"execute","args":{"cmd":"npm run dev","background":true,"ready_cmd":"   "}}"#,
    )
    .unwrap()
    {
        Action::ExecBg(s) => assert!(s.ready_cmd.is_none()),
        other => panic!("应当是后台启动，实际 {other:?}"),
    }

    // ③ 句柄操作
    assert!(matches!(
        parse_action(r#"{"tool":"execute","args":{"op":"stop","handle":"p1"}}"#).unwrap(),
        Action::Proc(ProcOp::Stop, h) if h == "p1"
    ));
    assert!(matches!(
        parse_action(r#"{"tool":"execute","args":{"op":"logs","handle":"p2"}}"#).unwrap(),
        Action::Proc(ProcOp::Log, _)
    ));
}

/// 请求本身不合法的三种写法都要报错，而不是静默变成"前台跑一条叫 op 的命令"。
#[test]
fn parse_action_rejects_malformed_execute_requests() {
    for bad in [
        r#"{"tool":"execute","args":{"op":"restart","handle":"p1"}}"#,
        r#"{"tool":"execute","args":{"op":"stop"}}"#,
        r#"{"tool":"execute","args":{"background":true}}"#,
        r#"{"tool":"execute","args":{"cmd":"   "}}"#,
    ] {
        assert!(parse_action(bad).is_err(), "该报错：{bad}");
    }
}

/// 多行 final 不该让整轮作废：模型在 JSON 字符串里写真换行是常见写法，
/// 实测 run agent-20260921-0950 第 13 轮就死在这上面（control character ... while parsing a string）。
#[test]
fn a_multiline_final_with_raw_newlines_is_still_a_final() {
    let raw = "{\"final\":\"## 结论\n\n服务已启动\n- 端口 8087\"}";
    match parse_action(raw).expect("真换行不该让整轮作废") {
        Action::Final(t) => {
            assert!(t.contains("服务已启动"), "{t}");
            assert!(t.contains('\n'), "换行要原样保留：{t:?}");
        }
        other => panic!("应当是 final：{other:?}"),
    }
}

/// 批协议：动作按**声明顺序**解析出来；final / plan 混进批里当面拒
#[test]
fn a_batch_parses_in_declared_order_and_rejects_control_actions() {
    let ok = parse_actions(
        concat!(
            r#"{"actions":[{"tool":"read","args":{"path":"a.rs"}},"#,
            r#"{"tool":"read","args":{"path":"b.rs"}},"#,
            r#"{"tool":"write","args":{"path":"c.rs","content":"x"}}]}"#
        ),
        8,
        true,
    )
    .expect("批该被接受");
    assert_eq!(ok.len(), 3);
    assert!(matches!(&ok[0], Action::Read(s) if s.path == "a.rs"));
    assert!(matches!(&ok[1], Action::Read(s) if s.path == "b.rs"));
    assert!(matches!(&ok[2], Action::Write(s) if s.path == "c.rs"));
    // calls 是同一件东西的另一个外衣（模型两种都写过）
    assert!(
        parse_actions(
            r#"{"calls":[{"tool":"read","args":{"path":"a"}}]}"#,
            8,
            true
        )
        .is_ok()
    );
    // 单动作照旧
    let one = parse_actions(r#"{"tool":"read","args":{"path":"a"}}"#, 8, true).unwrap();
    assert!(matches!(one.as_slice(), [Action::Read(_)]));
    // 控制动作不许混进批：谁先谁后没有合理解释，宁可当面拒
    let err = parse_actions(
        r#"{"actions":[{"tool":"read","args":{"path":"a"}},{"final":"完了"}]}"#,
        8,
        true,
    )
    .unwrap_err();
    assert!(err.contains("final"), "{err}");
    let err = parse_actions(
        r#"{"actions":[{"tool":"plan","args":{"steps":[{"title":"x"}]}}]}"#,
        8,
        true,
    )
    .unwrap_err();
    assert!(err.contains("plan"), "{err}");
    // 批里坏的那条要点名是第几个，否则模型不知道该改哪条
    let err = parse_actions(
        r#"{"actions":[{"tool":"read","args":{"path":"a"}},{"tool":"fly"}]}"#,
        8,
        true,
    )
    .unwrap_err();
    assert!(err.contains("第 2 个") && err.contains("fly"), "{err}");
}

/// 批上限：超了**不静默截断**（截断就是丢调用，模型还以为发出去了），把上限报回去
#[test]
fn an_oversized_batch_is_refused_with_the_cap() {
    let three = concat!(
        r#"{"actions":[{"tool":"read","args":{"path":"a"}},"#,
        r#"{"tool":"read","args":{"path":"b"}},"#,
        r#"{"tool":"read","args":{"path":"c"}}]}"#
    );
    assert!(parse_actions(three, 3, true).is_ok(), "刚好到上限该放行");
    let err = parse_actions(three, 2, true).unwrap_err();
    assert!(err.contains("最多 2 个") && err.contains("3 个"), "{err}");
    assert!(
        parse_actions(r#"{"actions":[]}"#, 8, true).is_err(),
        "空批该拒"
    );
}

/// 一行回滚：批关掉后该拒就拒（提示词也不再教这个形状）
#[test]
fn a_batch_is_refused_when_the_switch_is_off() {
    let b = r#"{"actions":[{"tool":"read","args":{"path":"a"}}]}"#;
    let err = parse_actions(b, 8, false).unwrap_err();
    assert!(err.contains("批量"), "{err}");
    assert!(parse_action(b).is_err(), "老入口（单动作语义）同样拒批");
    assert!(
        parse_action(r#"{"tool":"read","args":{"path":"a"}}"#).is_ok(),
        "关掉批不该影响单动作"
    );
}

/// read/write 的参数形状校验：当场拒掉含糊的形状，不去猜模型想干什么
#[test]
fn parameter_shapes_are_validated_at_parse_time() {
    let w = parse_action(
        r#"{"tool":"write","args":{"path":"a.py","edits":[{"find":"x","replace":"y"}]}}"#,
    )
    .unwrap();
    assert!(matches!(&w, Action::Write(s) if s.path == "a.py"
        && matches!(&s.body, WriteBody::Edits(e) if e.len() == 1 && e[0].find == "x")));

    // 两套互相矛盾的意图：猜错就是静默改错文件 → 当面拒
    let both = parse_action(
        r#"{"tool":"write","args":{"path":"a.py","content":"x","edits":[{"find":"a","replace":"b"}]}}"#,
    )
    .unwrap_err();
    assert!(both.contains("只能给一个"), "{both}");
    // 都给不出
    let neither = parse_action(r#"{"tool":"write","args":{"path":"a.py"}}"#).unwrap_err();
    assert!(
        neither.contains("content") && neither.contains("edits"),
        "{neither}"
    );
    assert!(parse_action(r#"{"tool":"write","args":{"path":"a.py","content":""}}"#).is_err());
    assert!(parse_action(r#"{"tool":"write","args":{"path":"a.py","edits":[]}}"#).is_err());
    assert!(
        parse_action(r#"{"tool":"write","args":{"path":"a.py","edits":[{"find":"a"}]}}"#).is_ok(),
        "省略 replace = 删掉这一段（合法，且与「没变化」是两回事）"
    );

    let r =
        parse_action(r#"{"tool":"read","args":{"path":"a.py","offset":10,"limit":5}}"#).unwrap();
    assert!(
        matches!(&r, Action::Read(s) if s.offset == Some(10) && s.limit == Some(5) && s.is_window())
    );
    assert!(matches!(
        parse_action(r#"{"tool":"read","args":{"path":"a.py"}}"#).unwrap(),
        Action::Read(s) if !s.is_window()
    ));
    // 0 / 负数当场拒：静默收下 0 会让模型拿到空结果，还以为文件是空的
    assert!(parse_action(r#"{"tool":"read","args":{"path":"a.py","limit":0}}"#).is_err());
    assert!(parse_action(r#"{"tool":"read","args":{"path":"a.py","offset":-1}}"#).is_err());
}
