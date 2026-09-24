use super::*;

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "dh-agent-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn write(&self, rel: &str, content: &str) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `tool_read` 收 `ReadSpec`（窗口读的入口）。整份读在测试里包一层 ——
/// 让每个断言只说自己关心的事，不必每行都写一遍 `ReadSpec::whole`
fn rd(ctx: &Ctx<'_>, path: &str) -> Result<String, String> {
    ctx.tool_read(&ReadSpec::whole(path))
}

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
        WritePolicy::Apply,
        &NoConnector,
        &NoAsker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    // ① 形状纠正：指到"外层缺 tool"，并把模型刚发的那坨参数原样贴回去
    let feedback = llm.request(1);
    assert!(feedback.contains("缺 `tool`"), "{feedback}");
    assert!(
        feedback.contains("echo e2e"),
        "要把它自己发的参数贴回去：{feedback}"
    );
    // ② 协议纠正：告诉它"你那套标记被剥了"，否则它不知道自己错在哪。
    // 断言用"已剥掉"而不是"工具调用标记"—— 后者在系统提示词里也有（规则 1 刚加了同一句话），
    // 拿它当判据会被提示词本身满足，等于没测这条注脚。
    assert!(
        feedback.contains("已剥掉"),
        "必须点明是模型自带的工具调用标记被剥掉：{feedback}"
    );
    // 落在磁盘上的事实：纠正之后动作真的跑了
    assert_eq!(std::fs::read_to_string(d.0.join("nm.txt")).unwrap(), "ok");
    assert!(out.answer.contains("写好了"), "{}", out.answer);
    assert_eq!(llm.count(), 3, "泄露一轮 → 纠正后一轮 → 交付");
}

/// 跑一轮工具循环：无提问通道、无连接器、Apply 策略、静默 sink —— 工具协议那几条用例共用。
fn run_loop(cfg: &crate::config::AppConfig, root: &std::path::Path) -> AgentOutcome {
    block_on(run_with_ask(
        cfg,
        root,
        "任务",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &NoAsker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败")
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
        req2.contains("tool_calls"),
        "工具轮的 content 是空的，必须把它的调用回显回去: {req2}"
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

/// 一行回滚：`llm.tool_protocol = false` → 请求里**一个字都不提**工具，老协议照旧能跑
#[test]
fn tool_protocol_switch_off_sends_no_tools_field() {
    let d = TempDir::new("tools-off");
    let llm = crate::testllm::fake_llm(vec![r#"{"final":"老路也能交付"}"#.into()]);
    let mut cfg = ask_cfg(&llm);
    cfg.llm.tool_protocol = false;
    let out = run_loop(&cfg, &d.0);

    assert!(
        !llm.request(0).contains("\"tools\""),
        "关掉开关就不该声明工具"
    );
    assert!(out.answer.contains("老路也能交付"), "{}", out.answer);
}

/// 混合态（实测 6/8 工具协议 + 2/8 老协议）：同一趟里两种形状都要能接住
#[test]
fn tool_round_and_legacy_json_round_both_work() {
    let d = TempDir::new("tools-mixed");
    d.write("a.txt", "AAA");
    let llm = crate::testllm::fake_llm(vec![
        // 第 1 轮：标准工具协议
        crate::testllm::tool_script(&[("read", serde_json::json!({"path": "a.txt"}))]),
        // 第 2 轮：老协议（content 里的 JSON），模型偶尔还会这么发
        r#"{"tool":"write","args":{"path":"b.txt","content":"BBB"}}"#.into(),
        r#"{"final":"都做完了"}"#.into(),
    ]);
    let cfg = ask_cfg(&llm);
    let out = run_loop(&cfg, &d.0);

    assert_eq!(llm.count(), 3, "工具轮 + 老协议轮 + 交付");
    assert_eq!(std::fs::read_to_string(d.0.join("b.txt")).unwrap(), "BBB");
    assert!(out.answer.contains("都做完了"), "{}", out.answer);
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

/// 提示词契约：四大原语必须都在，问答场景被明确要求先读再说，edit 不再出现
#[test]
fn system_prompt_defines_four_tools() {
    for t in ["read", "write", "execute", "connect"] {
        assert!(AGENT_SYSTEM.contains(t), "缺能力 {t}");
    }
    assert!(AGENT_SYSTEM.contains("final"), "必须有终止协议");
    assert!(AGENT_SYSTEM.contains("不要凭空猜测"), "问答必须先读项目");
    assert!(
        !AGENT_SYSTEM.contains("\"edit\""),
        "edit 不是一个独立能力：锚点编辑是 write 的 edits 形态，提示词里不该出现裸的 edit 工具名"
    );
}

/// 平台补充只该出现在对应平台的提示词里，且主体（AGENT_SYSTEM）不被改写
#[test]
fn platform_hint_matches_target_os() {
    let p = agent_system_prompt();
    assert!(p.starts_with(AGENT_SYSTEM), "主体必须原样开头");
    if cfg!(windows) {
        assert!(p.contains("git grep"), "Windows 上应带检索提示");
        assert!(p.len() > AGENT_SYSTEM.len());
    } else {
        assert_eq!(p, AGENT_SYSTEM, "非 Windows 不拼任何平台补充");
    }
}

/// 运行模式说明只在确认模式下出现，且必须点名"暂存 / 磁盘未变 / 别用 execute 验证" ——
/// 少了任何一条，模型就会拿 execute 的失败当成"改动错了"去反复修（65 轮空转那次）
#[test]
fn confirm_mode_note_is_conditional_and_complete() {
    let stage = policy_system_note(WritePolicy::Stage).expect("确认模式必须有说明");
    for k in ["确认模式", "暂存", "磁盘", "不要用 execute"] {
        assert!(stage.contains(k), "缺关键点 {k}：{stage}");
    }
    assert!(
        policy_system_note(WritePolicy::Apply).is_none(),
        "写入/自主模式磁盘真会变，没有矛盾就不该加提示（不虚报）"
    );
}

/// execute 的随附说明：只在"确认模式 + 已有暂存改动"时贴 —— 没写过东西就没有歧义，省 token
#[test]
fn staged_execute_note_gated_by_mode_and_changes() {
    let on = staged_execute_note(WritePolicy::Stage, true);
    assert!(on.contains("改动前"), "{on}");
    assert_eq!(
        staged_execute_note(WritePolicy::Stage, false),
        "",
        "没有暂存改动时不该贴"
    );
    assert_eq!(
        staged_execute_note(WritePolicy::Apply, true),
        "",
        "写入/自主模式磁盘已变，没有这层矛盾"
    );
}

/// 解析失败的回灌：截断叫写短、格式烂叫重发；错误文本带引号时
/// 反馈自己仍必须是合法 JSON（老实现裸拼会碎）
#[test]
fn parse_feedback_distinguishes_truncation() {
    let trunc = parse_failure_feedback("EOF while parsing an object", Some("length"));
    assert!(trunc.contains("max_tokens 截断"));
    assert!(trunc.contains("精简"));
    assert!(serde_json::from_str::<serde_json::Value>(&trunc).is_ok());

    // 报错文本里带引号和花括号（parse_action 的真实输出形态）
    let messy = parse_failure_feedback("输出不是合法 JSON: ...；片段: {\"tool\":\"plan\",", None);
    assert!(messy.contains("请重新只输出一个 JSON 对象"));
    assert!(serde_json::from_str::<serde_json::Value>(&messy).is_ok());

    // stop 是正常收笔，不算截断
    let stop = parse_failure_feedback("烂格式", Some("stop"));
    assert!(stop.contains("请重新只输出一个 JSON 对象"));
    assert!(!stop.contains("max_tokens"));
    assert!(serde_json::from_str::<serde_json::Value>(&stop).is_ok());
}

#[test]
fn execute_denies_destructive_patterns() {
    for bad in [
        "rm -rf /",
        "sudo shutdown now",
        "format c: /q",
        "MKFS /dev/sda1",
    ] {
        assert!(execute_allowed(bad).is_err(), "应拒绝：{bad}");
    }
    for ok in ["cargo test", "git log --oneline", "python -m pytest -q"] {
        assert!(execute_allowed(ok).is_ok(), "不该拒绝：{ok}");
    }
}

/// 闸门第一类：路径式垃圾与"把执行交给外壳"的启动器。
/// 用户截图里的 `\` 与 `\admin-run\` 就是前者 —— 真控制台里它们会被
/// ShellExecute 变成桌面弹窗，输出完全拿不到。
#[test]
fn preflight_refuses_path_garbage_and_launchers() {
    let d = TempDir::new("gate-bad");
    for bad in [
        r"\",
        r"\admin-run\",
        r"D:\nope\whatever.exe",
        "start mvn -v",
        r"cmd /C start \admin-run\",
        "explorer .",
        "powershell -NoProfile -Command Start-Process notepad",
        "",
    ] {
        assert!(preflight_execute(&d.0, bad).is_err(), "应拒绝：{bad:?}");
    }
}

/// 闸门要"教"：光说没有、不给清单，模型还会接着瞎试。
#[test]
fn preflight_tells_the_model_what_is_installed() {
    let d = TempDir::new("gate-hint");
    let e = preflight_execute(&d.0, "zzz-no-such-tool-zzz --version").unwrap_err();
    assert!(e.contains("本机没有"), "{e}");
    assert!(e.contains("本机可用"), "拒绝时要附可用清单：{e}");
}

/// 保守原则：常见写法一条都不许误杀（shell 内置、前置赋值、前置操作符、常驻工具）。
#[test]
fn preflight_lets_the_common_cases_through() {
    let d = TempDir::new("gate-ok");
    for ok in [
        "git --version",
        "cd . && git status",
        "echo hi",
        "& git --version",
        "set FOO=1 && git --version",
    ] {
        assert!(preflight_execute(&d.0, ok).is_ok(), "不该拒绝：{ok}");
    }
}

/// 引号必须保住带空格的路径，否则会被切成两半、误判成"路径不存在"。
#[test]
fn preflight_keeps_a_quoted_path_in_one_piece() {
    let toks = tokens(r#""C:\Program Files\Git\bin\bash.exe" -lc "echo hi""#);
    assert_eq!(toks[0], r"C:\Program Files\Git\bin\bash.exe");
    assert_eq!(toks[1], "-lc");
}

/// 项目内的包装脚本（`mvnw` / `gradlew`）既不在 PATH 里、也可能不带扩展名，不能误杀。
#[test]
fn preflight_allows_a_project_local_script_that_exists() {
    let d = TempDir::new("gate-script");
    d.write("mvnw.cmd", "@echo off\n");
    assert!(preflight_execute(&d.0, r".\mvnw.cmd -v").is_ok());
    assert!(preflight_execute(&d.0, "mvnw -v").is_ok());
    assert!(preflight_execute(&d.0, r".\nope.cmd").is_err());
}

/// 端到端：被闸门挡住时不该留下"执行痕迹"（模型会以为跑过了）。
#[test]
fn tool_execute_refuses_garbage_before_touching_the_shell() {
    let d = TempDir::new("gate-exec");
    let r = tool_execute(&d.0, r"\admin-run\", None);
    assert!(r.starts_with('❌'), "{r}");
    assert!(!r.contains("exit="), "拒绝时不该有执行结果：{r}");
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

/// 后台启动同样过闸门与开关 —— 跑多久不改变"它是不是一条命令"。
#[test]
fn background_start_goes_through_the_same_gate_and_switch() {
    let d = TempDir::new("bg-gate");
    let spec = |cmd: &str, ready: Option<&str>| crate::proc::StartSpec {
        cmd: cmd.into(),
        ready_cmd: ready.map(str::to_string),
        ready_timeout_secs: Some(5),
        keep_alive: false,
    };

    // 开关关着：直接拒，并给出可操作的下一步
    let mut off = AppConfig::default();
    off.proc.enabled = false;
    let e = tool_exec_bg(&d.0, &off, &spec("cargo run", None)).expect_err("关掉就该拒");
    assert!(e.contains("proc.enabled"), "{e}");

    // 闸门（启动器 / 垃圾命令）对后台同样生效
    let on = AppConfig::default();
    assert!(
        tool_exec_bg(&d.0, &on, &spec("start foo", None)).is_err(),
        "start 那类外包给外壳的写法不该被后台模式放行"
    );
    assert!(tool_exec_bg(&d.0, &on, &spec(r"\admin-run\", None)).is_err());

    // 就绪判据也要过闸门 —— 它是引擎去跑的命令，不能是个未知命令
    let e = tool_exec_bg(
        &d.0,
        &on,
        &spec("cargo build", Some("zzz-no-such-tool-zzz")),
    )
    .expect_err("判据不可执行就该拒");
    assert!(e.contains("判据"), "{e}");
}

/// 句柄不存在 = 请求不合法（与解析报错同类），不该记成一次成功的工具调用。
#[test]
fn handle_ops_treat_an_unknown_handle_as_a_request_error() {
    assert!(tool_proc(ProcOp::Status, "p999999").is_err());
    assert!(tool_proc(ProcOp::Log, "p999999").is_err());
    assert!(tool_proc(ProcOp::Stop, "p999999").is_err());
}

/// 提示词必须把后台模式说清 —— 不说，模型就还用 start / Start-Process 那套花招，
/// 而那正是这轮的病根（输出拿不到 + 进程脱离掌控 + 桌面弹窗）。
#[test]
fn system_prompt_documents_the_background_mode() {
    for k in ["background", "ready_cmd", "handle", "就绪"] {
        assert!(
            AGENT_SYSTEM.contains(k),
            "提示词缺 {k}：模型没有表达常驻的词汇"
        );
    }
}

/// read：文件给内容、目录给树、overlay 优先、路径封闭
#[test]
fn tool_read_file_dir_overlay_and_jail() {
    let d = TempDir::new("read");
    d.write("src/main.rs", "fn main() {}");
    let mut ctx = Ctx {
        proj: &d.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Stage,
        backup_dir: None,
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
        proj: &d.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Stage,
        backup_dir: None,
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
        proj: &d2.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Apply,
        backup_dir: None,
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

/// 假连接器：记录收到的请求（Connect 的路由与错误语义由它验）
struct Recorder {
    calls: std::sync::Mutex<Vec<ConnectRequest>>,
    fail: bool,
}

impl Connector for Recorder {
    fn list(&self) -> ConnectFuture<'_, Vec<ConnectTarget>> {
        Box::pin(async {
            Ok(vec![ConnectTarget {
                kind: "mcp".into(),
                name: "fs".into(),
                detail: "已连接".into(),
                tools: vec!["read_file(读文件)".into()],
            }])
        })
    }

    fn call(&self, req: ConnectRequest) -> ConnectFuture<'_, ConnectOutcome> {
        Box::pin(async move {
            if self.fail {
                return Err("连不上 fs".into());
            }
            self.calls.lock().unwrap().push(req.clone());
            Ok(ConnectOutcome {
                text: format!("{} 返回", req.action),
                is_error: false,
            })
        })
    }
}

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
            // enable_all 而不是只用 enable_time：整循环级用例要真发 HTTP 请求（假 LLM
            // 是本地 TCP 服务），只开 time driver 会连不上
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
}

/// 门禁单测用的 Sink：不刷屏、不落盘
struct QuietSink;
impl Sink for QuietSink {}

// ============================================
// v0.8：第五个动作 ask_user（需求歧义只能问委托人）
// ============================================

/// 脚本化提问通道：按序给答案（`Err` = 拿不到答案，用来验 fail-closed），并留档问过什么。
/// 剧本演完一律回"没人回答" —— 测试里绝不允许出现"引擎自己猜了个答案"。
struct ScriptAsker {
    answers: std::sync::Mutex<std::collections::VecDeque<Result<String, AskErr>>>,
    seen: std::sync::Mutex<Vec<(String, String, String)>>,
}

impl ScriptAsker {
    fn new(items: Vec<Result<&str, AskErr>>) -> Self {
        Self {
            answers: std::sync::Mutex::new(
                items
                    .into_iter()
                    .map(|r| r.map(|s| s.to_string()))
                    .collect(),
            ),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }
    /// 问过几次、分别问了什么（id / question / why）
    fn asked(&self) -> Vec<(String, String, String)> {
        self.seen.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

impl Asker for ScriptAsker {
    fn ask<'a>(&'a self, id: &'a str, spec: &'a AskSpec) -> AskFut<'a> {
        self.seen
            .lock()
            .unwrap()
            .push((id.to_string(), spec.question.clone(), spec.why.clone()));
        let next = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(AskErr::NoAsker));
        Box::pin(async move {
            next.map(|text| AskAnswer {
                text,
                option_index: None,
                ts: String::new(),
            })
        })
    }
}

/// 提问类测试的公共配置（关掉与提问无关的层，把轮次压到最少）
fn ask_cfg(llm: &crate::testllm::FakeLlm) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;
    cfg.step.execute_plan = false;
    cfg.discover.enabled = false;
    cfg
}

/// 造一条 `ask_user` 剧本（免去长行 + raw string 转义：这类脚本两个坑都踩过）
fn ask_json(q: &str, why: &str, opts: &[&str]) -> String {
    serde_json::json!({
        "tool": "ask_user",
        "args": {"question": q, "why": why, "options": opts}
    })
    .to_string()
}

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

/// 门禁单测用的配置：不依赖 Docker、不绑 tools/lint、不调模型（复核另测）
fn apply_cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.sandbox.mode = "off".into();
    cfg.lint.enabled = false;
    cfg.reflect.enabled = false;
    cfg
}

fn one_change(path: &str) -> Vec<FileChange> {
    vec![FileChange {
        path: path.into(),
        kind: "modify".into(),
        before: Some("旧内容\n".into()),
        after: "新内容\n".into(),
    }]
}

fn ctx_with<'a>(proj: &'a Path, changes: Vec<FileChange>, policy: WritePolicy) -> Ctx<'a> {
    Ctx {
        proj,
        overlay: BTreeMap::new(),
        changes,
        probes: Vec::new(),
        policy,
        backup_dir: None,
    }
}

/// 取证通道的正门：主循环跑过一条命令 → 复核员必须能在**自己那份输入**里看到它的
/// 实际输出。这条一断，"今天星期几"就退回"跑六轮还是被判证据不足"（See Probe）。
#[test]
fn executed_command_output_reaches_the_reviewer() {
    struct Quiet;
    impl crate::pipeline::Sink for Quiet {}

    let marker = "PROBE-MARK-2026";
    let llm = crate::testllm::fake_llm(vec![
        format!(r#"{{"tool":"execute","args":{{"cmd":"echo {marker}"}}}}"#),
        r#"{"final":"拿到了"}"#.into(),
        // 第三个请求 = 复核（没有改动 → 依据核对那套判据）
        r#"{"verdict":"ok","summary":"可核对","findings":[]}"#.into(),
    ]);
    let dir = TempDir::new("probe-e2e");
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    // 与本用例无关的重活全关：窄/全量验证与额外复核轮会把"第几个请求"的断言搅乱
    cfg.gate.narrow = false;
    cfg.gate.full = false;

    let out = block_on(run(
        &cfg,
        &dir.0,
        "查个值给我",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &Quiet,
    ))
    .expect("run 不该失败");

    assert_eq!(out.answer, "拿到了");
    assert_eq!(
        out.reflections.len(),
        1,
        "该复核了一次：{:?}",
        out.reflections
    );
    assert!(
        !out.reflections.iter().any(|r| r.suspect()),
        "有取证可核对时不该被打回：{:?}",
        out.reflections
    );
    // 复核员打开的那份请求（最后一个）必须同时看得到命令与它的输出
    let review = llm.request(llm.count() - 1);
    assert!(review.contains("echo"), "复核员没看到命令：{review}");
    assert!(review.contains(marker), "复核员没看到输出：{review}");
}

/// 确认模式：项目磁盘没动 → 全量验证**显式跳过**（带原因）且放行，不许假装通过
#[test]
fn gate_skips_full_verify_in_stage_mode_and_says_why() {
    let d = TempDir::new("gate-stage");
    let ctx = ctx_with(&d.0, one_change("a.py"), WritePolicy::Stage);
    let cfg = apply_cfg();
    let mut gate = GateState {
        dirty: true,
        ..Default::default()
    };
    let mut out = AgentOutcome::default();
    let blocked = block_on(gate_before_final(
        &cfg,
        &d.0,
        &ctx,
        "改点东西",
        &[],
        "已改好",
        &mut gate,
        &mut out,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ));
    assert!(blocked.is_none(), "跳过不是失败，不该拦交付");
    assert_eq!(out.verifications.len(), 1);
    let v = &out.verifications[0];
    assert_eq!(v.layer, "full");
    assert_eq!(v.status, "skipped");
    assert!(
        v.skipped_reason
            .as_deref()
            .unwrap_or("")
            .contains("确认模式"),
        "{:?}",
        v.skipped_reason
    );
    assert!(
        gate.notes.iter().any(|n| n.contains("未跑全量验证")),
        "跳过必须写进答复：{:?}",
        gate.notes
    );
}

/// 写入模式 + 测试真失败 → 拒绝交付，把验证报告回灌（门禁的核心作用）
#[test]
fn gate_blocks_delivery_when_verification_fails() {
    let d = TempDir::new("gate-fail");
    d.write("calc.py", "def add(a, b):\n    return a - b\n");
    d.write(
            "test_calc.py",
            "import unittest\nfrom calc import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == '__main__':\n    unittest.main()\n",
        );
    let mut ctx = ctx_with(&d.0, one_change("calc.py"), WritePolicy::Apply);
    ctx.overlay.insert(
        "calc.py".into(),
        "def add(a, b):\n    return a - b\n".into(),
    );
    let cfg = apply_cfg();
    let mut gate = GateState {
        dirty: true,
        ..Default::default()
    };
    let mut out = AgentOutcome::default();
    let blocked = block_on(gate_before_final(
        &cfg,
        &d.0,
        &ctx,
        "修好 add",
        &[],
        "我修好了",
        &mut gate,
        &mut out,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("测试真的失败时必须打回");
    assert!(blocked.contains("[机械验证·全量验证]"), "{blocked}");
    assert_eq!(out.verifications.last().unwrap().status, "failed");
    assert_eq!(gate.full_failures, 1);
    assert!(gate.dirty, "没通过就还是脏的，不能放行");
}

/// 验证真通过 → 放行，且改动不再是"脏"的（下次不用重复跑一遍编译）
#[test]
fn gate_releases_once_verification_passes() {
    let d = TempDir::new("gate-pass");
    d.write("calc.py", "def add(a, b):\n    return a + b\n");
    d.write(
            "test_calc.py",
            "import unittest\nfrom calc import add\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == '__main__':\n    unittest.main()\n",
        );
    let ctx = ctx_with(&d.0, one_change("calc.py"), WritePolicy::Apply);
    let cfg = apply_cfg();
    let mut gate = GateState {
        dirty: true,
        ..Default::default()
    };
    let mut out = AgentOutcome::default();
    let blocked = block_on(gate_before_final(
        &cfg,
        &d.0,
        &ctx,
        "补个 add",
        &[],
        "写好了",
        &mut gate,
        &mut out,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ));
    assert!(blocked.is_none(), "{blocked:?}");
    assert_eq!(out.verifications.last().unwrap().status, "passed");
    assert!(!gate.dirty, "通过了就不该再重复编译一遍");
    assert!(gate.notes.is_empty(), "通过不该留任何告示");
}

/// 预算用尽：不再拦，但必须写明"结论：未通过"（诚实优先于好看）
#[test]
fn gate_lets_through_after_budget_with_an_honest_note() {
    let d = TempDir::new("gate-budget");
    let ctx = ctx_with(&d.0, one_change("a.py"), WritePolicy::Apply);
    let cfg = apply_cfg();
    let mut gate = GateState {
        dirty: true,
        full_failures: cfg.gate.max_full_attempts,
        ..Default::default()
    };
    let mut out = AgentOutcome::default();
    let blocked = block_on(gate_before_final(
        &cfg,
        &d.0,
        &ctx,
        "改点东西",
        &[],
        "改好了",
        &mut gate,
        &mut out,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ));
    assert!(blocked.is_none(), "预算用尽后要放行，否则模型会卡死在这里");
    assert!(
        gate.notes.iter().any(|n| n.contains("未通过")),
        "{:?}",
        gate.notes
    );
    assert!(out.verifications.is_empty(), "放行那轮不该再跑一次");
}

/// 门禁汇总：什么都没跑成 → skipped（绝不写"通过"）；失败必须带修复提示
#[test]
fn verify_outcome_never_reports_a_fake_pass() {
    let skip = VerifyOutcome::skip("full", "沙箱不可用".into());
    assert_eq!(skip.status, "skipped");
    assert!(skip.verdict.contains("沙箱不可用"), "{}", skip.verdict);

    let items = vec![
        CheckItem {
            kind: "syntax".into(),
            language: "python".into(),
            target: "a.py".into(),
            status: "skipped".into(),
            reason: "没有 Python 文件".into(),
        },
        CheckItem {
            kind: "lint".into(),
            language: "-".into(),
            target: "规约检查".into(),
            status: "passed".into(),
            reason: "error 0".into(),
        },
    ];
    let mixed = VerifyOutcome::summarize("full", &items, 5, None);
    assert_eq!(mixed.status, "passed", "有通过项就不算跳过");
    assert_eq!(mixed.passed_checks, 1);
    assert_eq!(mixed.skipped, 1);

    let all_skipped = VerifyOutcome::summarize("full", &items[..1], 5, None);
    assert_eq!(all_skipped.status, "skipped");
    assert!(
        all_skipped
            .skipped_reason
            .as_deref()
            .unwrap()
            .contains("Python"),
        "跳过原因要借第一条 skipped 检查，别退化成套话"
    );
}

/// lint → 检查项：error 才算失败，warning 只记录（与前端 check-style 同口径）
#[test]
fn lint_item_maps_errors_only() {
    let clean = lint::LintOutcome {
        ran: true,
        exit_code: Some(0),
        cmd: "harness_lint".into(),
        report: Some(lint::LintReport {
            root: ".".into(),
            files_scanned: 3,
            files_skipped: 0,
            test_count: 0,
            counts: [("warning".to_string(), 7u32)].into_iter().collect(),
            by_rule: Default::default(),
            diagnostics: Vec::new(),
            suppressions: serde_json::Value::Null,
            ok: true,
        }),
        parse_error: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        duration_ms: 3,
    };
    let item = lint_item(&clean).unwrap();
    assert_eq!(item.status, "passed");
    assert!(item.reason.contains("warning 7"), "{}", item.reason);

    let not_run = lint::LintOutcome {
        ran: false,
        parse_error: Some("解释器没找到".into()),
        ..clean.clone()
    };
    assert_eq!(lint_item(&not_run).unwrap().status, "skipped");
}

/// connect：list / call / send 三形态各自把请求原样送达宿主，工具名与简报可读
#[test]
fn connect_routes_three_forms_to_host() {
    let rec = Recorder {
        calls: std::sync::Mutex::new(Vec::new()),
        fail: false,
    };
    let (tool, brief, r) = block_on(connect_step(&rec, ConnectAction::List));
    assert_eq!(tool, "connect");
    assert_eq!(brief, "connect list");
    assert!(r.unwrap().contains("mcp fs：已连接"), "清单要带连接状态");

    let (_, brief, r) = block_on(connect_step(
        &rec,
        ConnectAction::Call {
            server: "fs".into(),
            tool: "read_file".into(),
            arguments: serde_json::json!({"path": "a"}),
        },
    ));
    assert_eq!(brief, "connect fs/read_file");
    assert_eq!(r.unwrap(), "call 返回");

    let (_, brief, _) = block_on(connect_step(
        &rec,
        ConnectAction::Send {
            agent: "翻译".into(),
            text: "你好".into(),
        },
    ));
    assert_eq!(brief, "connect 翻译（委托 2 字）");

    let got = rec.calls.lock().unwrap();
    assert_eq!(got.len(), 2, "list 不该走 call");
    assert_eq!(got[0].action, "call");
    assert_eq!(got[0].server, "fs");
    assert_eq!(got[0].tool, "read_file");
    assert_eq!(got[0].arguments["path"], "a");
    assert_eq!(got[1].action, "send");
    assert_eq!(got[1].agent, "翻译");
    assert_eq!(got[1].text, "你好");
}

/// 连不上 = Err（提示词那侧记失败轮）；没接外部能力的宿主清单为空
#[test]
fn connect_failure_and_no_connector() {
    let rec = Recorder {
        calls: std::sync::Mutex::new(Vec::new()),
        fail: true,
    };
    let (_, _, r) = block_on(connect_step(
        &rec,
        ConnectAction::Call {
            server: "fs".into(),
            tool: "x".into(),
            arguments: serde_json::Value::Null,
        },
    ));
    assert!(r.unwrap_err().contains("连不上 fs"));

    let none = NoConnector;
    let (_, _, r) = block_on(connect_step(&none, ConnectAction::List));
    assert_eq!(r.unwrap(), "（当前没有可连接的外部能力）");
    let (_, _, r) = block_on(connect_step(
        &none,
        ConnectAction::Send {
            agent: "翻译".into(),
            text: "你好".into(),
        },
    ));
    assert!(r.is_err(), "没有连接器时 send 必须失败，不能假装成功");
}

/// 清单注入提示词：空清单不出现，非空逐行列出种类/名字/工具
#[test]
fn connect_note_renders_targets() {
    assert!(connect_note(&[]).is_none());
    let note = connect_note(&[
        ConnectTarget {
            kind: "mcp".into(),
            name: "fs".into(),
            detail: "已连接".into(),
            tools: vec!["read_file(读文件)".into()],
        },
        ConnectTarget {
            kind: "a2a".into(),
            name: "translator".into(),
            detail: "http://127.0.0.1:9999".into(),
            tools: vec![],
        },
    ])
    .unwrap();
    assert!(
        note.contains("mcp fs：已连接；工具：read_file(读文件)"),
        "{note}"
    );
    assert!(
        note.contains("a2a translator：http://127.0.0.1:9999"),
        "{note}"
    );
}

/// 记录 step 事件的 Sink —— 断言 UI 真会收到什么，而不是把谓词再抄一遍
#[derive(Default)]
struct StepSink {
    seen: std::sync::Mutex<Vec<(usize, usize, String, String)>>,
}
impl Sink for StepSink {
    fn step(&self, i: usize, total: usize, st: &StepOutcome) {
        self.seen
            .lock()
            .unwrap()
            .push((i, total, st.status.clone(), st.notes.clone()));
    }
}
impl StepSink {
    /// (index, total, status, notes)
    fn events(&self) -> Vec<(usize, usize, String, String)> {
        self.seen.lock().unwrap().clone()
    }
}

fn plan_step(id: u32, title: &str, files: &[&str], kind: &str) -> PlanStep {
    PlanStep {
        id,
        title: title.into(),
        detail: String::new(),
        files: files.iter().map(|f| (*f).to_string()).collect(),
        kind: kind.into(),
    }
}

/// 进行中：文件全部落地 → done；部分 → running；一步没动 → 不发事件（留给 settle_steps 收尾）
#[test]
fn plan_progress_marks_steps_by_files() {
    let mut overlay = BTreeMap::new();
    overlay.insert("m.py".to_string(), "x".to_string());
    let steps = [
        plan_step(1, "写模块", &["m.py"], "code"),
        plan_step(2, "写模块+测试", &["m.py", "t.py"], "test"),
        plan_step(3, "同步文档", &["docs/readme.md"], "docs"),
    ];
    let sink = StepSink::default();
    emit_step_progress(&sink, &steps, &overlay);
    let ev = sink.events();
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!((ev[0].0, ev[0].2.as_str()), (1, "done"));
    assert_eq!((ev[1].0, ev[1].2.as_str()), (2, "running")); // 1/2
}

/// 收尾不留沙漏：没声明文件的步骤交付即完成；声明了却没产出的落成"本次未完成"并写明缺什么
#[test]
fn settle_steps_closes_every_hourglass() {
    let d = TempDir::new("settle");
    let mut overlay = BTreeMap::new();
    overlay.insert("pom.xml".to_string(), "x".to_string());
    // 复刻实测那一次 run（agent-20260920-091436）：只暂存了 pom.xml，
    // 第 3 步「同步文档」声明的文档文件零产出 —— 修前界面永远 2/3 + ⌛
    let steps = [
        plan_step(1, "加 Spring Security", &["pom.xml"], "code"),
        plan_step(2, "编译验证依赖解析", &[], "verify"),
        plan_step(3, "同步文档", &["CLAUDE.md"], "docs"),
    ];
    let sink = StepSink::default();
    settle_steps(&sink, &steps, &overlay, &d.0, true, &[]);
    let ev = sink.events();
    assert_eq!(ev.len(), 3, "每个步骤都要落定，否则界面留沙漏：{ev:?}");
    assert_eq!(ev[0].2, "done");
    assert_eq!(ev[1].2, "done", "没有文件级判据的步骤，交付即算完成");
    assert_eq!(ev[2].2, "skipped");
    assert!(ev[2].3.contains("CLAUDE.md"), "缺什么要写清楚：{}", ev[2].3);
    assert_eq!(ev[2].1, 3, "total 要带上，UI 才能算 x/y");
}

/// 不是交付收场（轮次上限 / 取消 / 致命错误）：没做完的不算完成，
/// 但**已经落地全部声明文件的步骤不能被降级**（取消前它就做完了）
#[test]
fn settle_steps_never_claims_done_without_delivery() {
    let d = TempDir::new("settle-nodeliver");
    let mut overlay = BTreeMap::new();
    overlay.insert("m.py".to_string(), "x".to_string());
    let steps = [
        plan_step(1, "写模块", &["m.py"], "code"),
        plan_step(2, "同步文档", &[], "docs"),
    ];
    let sink = StepSink::default();
    settle_steps(&sink, &steps, &overlay, &d.0, false, &[]);
    let ev = sink.events();
    assert_eq!(ev.len(), 2);
    assert_eq!(ev[0].2, "done", "文件已全部落地，取消不该把它降级");
    assert_eq!(ev[1].2, "skipped");
    assert!(ev[1].3.contains("未完成"), "{}", ev[1].3);
}

/// 复刻实测 run `agent-20260920-142405`（用户看着 2 ✅ / 6 ⏹ 说"质量差"那次）：
/// 8 步里 6 步被判 `skipped`，可其中 4 步其实写了一半以上。两个口径病：
///
/// ① **本来就存在于项目里的声明文件不算缺**。第 7 步声明 `vite.config.js`，
///    那是前一天就有的文件、本步根本不需要重写 —— 修前界面指着一个躺在那儿的
///    文件说"缺它"。
/// ② **有产出但对不上声明 → `partial`（未对齐），不是 `skipped`**。`skipped` 是
///    "这步一件没做"；用它描述"做了 5/9"比事实更重，和沙漏是同一种骗人。
#[test]
fn settle_steps_distinguishes_unaligned_from_skipped() {
    let d = TempDir::new("settle-partial");
    // 项目里本来就有的文件（本 run 没有任何一步写过它）
    d.write("web/vite.config.js", "export default {}");

    let mut overlay = BTreeMap::new();
    overlay.insert("web/src/api/http.js".to_string(), "x".to_string());
    overlay.insert("web/src/styles.css".to_string(), "x".to_string());

    let steps = [
        // 声明 3 个，只写了 1 个 → 有产出但对不上：partial
        plan_step(
            1,
            "前端页面骨架",
            &[
                "web/src/api/products.js",
                "web/src/api/http.js",
                "web/src/styles.css",
            ],
            "code",
        ),
        // 声明的是**已经存在**的文件，本 run 没写它 → 不是缺 → done
        plan_step(2, "dev 代理", &["web/vite.config.js"], "code"),
        // 声明 2 个，一个都没写、磁盘上也没有 → 这才是 skipped
        plan_step(3, "前端测试", &["web/test/form.test.js"], "test"),
    ];

    let sink = StepSink::default();
    settle_steps(&sink, &steps, &overlay, &d.0, true, &[]);
    let ev = sink.events();
    assert_eq!(ev.len(), 3, "每个步骤都要落定：{ev:?}");
    assert_eq!(
        ev[0].2, "partial",
        "有产出但对不上声明，不该说成'跳过'：{:?}",
        ev[0]
    );
    assert!(ev[0].3.contains("http.js"), "要写明已写哪些：{}", ev[0].3);
    assert!(
        ev[0].3.contains("products.js"),
        "要写明还缺哪些：{}",
        ev[0].3
    );
    assert_eq!(
        ev[1].2, "done",
        "声明文件本来就在项目里（无需重写），不该判缺失：{:?}",
        ev[1]
    );
    assert_eq!(ev[2].2, "skipped", "一件没写才是跳过：{:?}", ev[2]);
    assert!(ev[2].3.contains("form.test.js"), "{}", ev[2].3);
}

/// 命令发现：本机有什么命令必须**实测后告诉模型**，而不是让模型自己试。
///
/// 这条断言的是"真的进了首条 user 消息"，不是"函数返回了非空" —— 后者在纯函数单测里
/// 早就绿了，却完全可能因为拼装点写错而根本到不了模型面前（那个 65 轮空转就是这么来的：
/// 引擎探过，只是没写进上下文）。
#[test]
fn first_user_message_carries_the_discovered_command_list() {
    let llm = crate::testllm::fake_llm(vec![r#"{"final":"好"}"#.into()]);
    let dir = TempDir::new("discover-note");
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;

    let sink = StepSink::default();
    block_on(run(
        &cfg,
        &dir.0,
        "随便问一句",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &sink,
    ))
    .expect("run 不该失败");

    let first = llm.request(0);
    assert!(first.contains("本机命令"), "命令清单没进首条消息：{first}");
    assert!(first.contains("git"), "常驻项 git 该在清单里：{first}");
}

/// 关掉开关，整段就该消失 —— 提示词不许虚报："没探"和"探了但没有"是两件事。
#[test]
fn discovered_command_list_disappears_when_disabled() {
    let llm = crate::testllm::fake_llm(vec![r#"{"final":"好"}"#.into()]);
    let dir = TempDir::new("discover-off");
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.discover.enabled = false;
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;

    let sink = StepSink::default();
    block_on(run(
        &cfg,
        &dir.0,
        "随便问一句",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &sink,
    ))
    .expect("run 不该失败");

    let first = llm.request(0);
    assert!(!first.contains("本机命令"), "{first}");
}

// ============================================
// execute_plan：父循环的一轮 = 一个计划步骤
// ============================================

/// 打开 `execute_plan` 时的完整闭环：模型给计划 → 引擎按序派发两个步骤 → 模型交付。
///
/// 这条用例盯三件事（都不是断言语义，而是断言**实际发生了什么**）：
/// ① 调度权在引擎手里：模型从头到尾没说"做第几步"，两步仍按 1→2 跑完；
/// ② 上下文真的隔离：子步骤写进文件的内容（MARKER）绝不能出现在父的请求体里；
/// ③ UI 收到的是 running→done 两拍，而不是从"文件是否落地"倒推。
#[test]
fn execute_plan_runs_each_step_in_its_own_context() {
    const MARKER: &str = "step-file-secret-a41f";
    let llm = crate::testllm::fake_llm(vec![
        // 父第 1 轮：一份两步计划
        r#"{"tool":"plan","args":{"steps":[
                {"title":"写模块","detail":"写 m.py","files":["m.py"]},
                {"title":"写测试","detail":"写 t.py","files":["t.py"]}]}}"#
            .into(),
        // 子步骤 1：写文件 → 交回
        format!(r#"{{"tool":"write","args":{{"path":"m.py","content":"{MARKER}"}}}}"#),
        r#"{"final":"m.py 写好了"}"#.into(),
        // 子步骤 2：写文件 → 交回
        r#"{"tool":"write","args":{"path":"t.py","content":"t = 1"}}"#.into(),
        r#"{"final":"t.py 写好了"}"#.into(),
        // 父最后的交付
        r#"{"final":"两步都完成了"}"#.into(),
    ]);

    let dir = TempDir::new("plan-exec");
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.step.execute_plan = true;
    // 与本用例无关的重活全部关掉：窄验证要起 python/node，全量验证要沙箱，复核要再
    // 烧一轮模型调用 —— 任一开着都会把"请求第几条"的断言搅乱
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;

    let sink = StepSink::default();
    let out = block_on(run(
        &cfg,
        &dir.0,
        "把 m.py 和 t.py 写出来",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &sink,
    ))
    .expect("run 不该失败");

    assert_eq!(out.answer, "两步都完成了");
    assert_eq!(
        out.changes.len(),
        2,
        "两个子步骤各写了一个文件：{:?}",
        out.changes
    );

    // ① 派发顺序 + UI 事件：running→done，两步依次
    let ev = sink.events();
    let head: Vec<(usize, String)> = ev.iter().take(4).map(|e| (e.0, e.2.clone())).collect();
    assert_eq!(
        head,
        vec![
            (1, "running".to_string()),
            (1, "done".to_string()),
            (2, "running".to_string()),
            (2, "done".to_string()),
        ],
        "全部事件：{ev:?}"
    );
    assert_eq!(ev.len(), 6, "收尾时每个步骤再落一次终态：{ev:?}");

    // ② 隔离的真判据：父轮次 = 1(plan) + 2(两个步骤) + 1(final) = 4，最后一个请求
    //    就是父在交付前看到的东西。子步骤写进文件的内容**不该**出现在里面。
    assert_eq!(llm.count(), 6, "父 4 轮 + 子步骤各 2 轮");
    let last = llm.request(5);
    assert!(last.contains("plan"), "父该记得那份计划：{last}");
    assert!(
        !last.contains(MARKER),
        "子步骤写进文件的内容漏进了父上下文 —— 隔离没生效：{last}"
    );
}

/// 步骤失败：落 ❌ 并**停下交回模型一轮**（而不是闷头跑下一步）。
/// `error` 这个步骤状态在 agent 路径上原先不可达，这里是它第一个正当来源。
#[test]
fn execute_plan_hands_a_failed_step_back_to_the_model() {
    let llm = crate::testllm::fake_llm(vec![
        r#"{"tool":"plan","args":{"steps":[
                {"title":"第一步","files":["a.py"]},
                {"title":"第二步","files":["b.py"]}]}}"#
            .into(),
        // 子步骤 1 只写了文件、没交回 —— `step.max_steps = 1` 让它立刻预算用尽
        r#"{"tool":"write","args":{"path":"a.py","content":"a = 1"}}"#.into(),
        // 父的干预轮：模型选择直接交付
        r#"{"final":"只做完第一步，第二步没做"}"#.into(),
    ]);

    let dir = TempDir::new("plan-fail");
    let mut cfg = AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "smoke".into();
    cfg.llm.model = "fake".into();
    cfg.step.execute_plan = true;
    cfg.step.max_steps = 1;
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;

    let sink = StepSink::default();
    let out = block_on(run(
        &cfg,
        &dir.0,
        "写两个文件",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &sink,
    ))
    .expect("run 不该失败");

    assert_eq!(out.answer, "只做完第一步，第二步没做");

    let st: Vec<(usize, String)> = sink.events().iter().map(|e| (e.0, e.2.clone())).collect();
    assert_eq!(
        st,
        vec![
            (1, "running".to_string()),
            (1, "error".to_string()),
            // 收尾再落一次终态（settle_steps 的契约）
            (1, "error".to_string()),
            // 第二步从没被派发过（游标停在失败那一步），收尾如实标"未完成"
            (2, "skipped".to_string()),
        ],
        "失败步骤必须留痕，未执行的步骤也不能留沙漏"
    );

    // 干预轮真的把失败事实递给了模型
    assert_eq!(llm.count(), 3, "父 3 轮：plan / 干预 / final");
    let intervene = llm.request(2);
    assert!(intervene.contains("执行失败"), "{intervene}");
    assert!(intervene.contains("第一步"), "{intervene}");
}

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

// ---- 托管进程的交付对账（用户报的那个「服务没启动起来」）----

/// 交付前对账：只有**会被本次 run 收掉**的托管进程才该被点名。
#[test]
fn reap_warning_flags_only_the_processes_that_will_die() {
    let p = |handle: &str, keep: bool| crate::proc::ProcInfo {
        handle: handle.into(),
        pid: 34192,
        cmd: "mvn -pl cloud-shop-admin spring-boot:run".into(),
        log: "p1.log".into(),
        ready_cmd: None,
        state: "ready".into(),
        keep_alive: keep,
        elapsed_ms: 1234,
        started_at_ms: 1_700_000_000_000,
    };
    assert!(
        reap_warning(&[p("p1", true)]).is_none(),
        "声明了 keep_alive 的不会被收，没什么可对账的"
    );
    let w = reap_warning(&[p("p1", true), p("p2", false)]).expect("有一个会被收掉就该说话");
    assert!(w.contains("p2"), "{w}");
    assert!(w.contains("keep_alive"), "{w}");
    assert!(w.contains("34192"), "证据要带 pid：{w}");
    assert!(!w.contains("p1（pid"), "不会被收的那个不该被点名：{w}");
}

/// 提示词必须教会模型 keep_alive：run agent-20260921-0950 的病灶就是模型照提示词起服务、
/// 看到就绪就报「已启动」，而提示词里根本没有 keep_alive 这个词，run 一结束服务就被收掉了。
#[test]
fn the_prompt_teaches_keep_alive_and_its_consequence() {
    for k in ["keep_alive", "活不活得过本次 run", "谎报"] {
        assert!(
            AGENT_SYSTEM.contains(k),
            "提示词缺 {k} —— 模型没有表达「活过本次 run」的词汇"
        );
    }
    assert!(
        AGENT_SYSTEM.contains("keep_alive=true"),
        "示例里必须带 keep_alive=true —— 那是模型最常抄的一行"
    );
}

/// start 的返回告示：**未声明**时要把后果说透（不能只说「不会留孤儿」这种好话），
/// 声明了才说「留着了，面板可停」。
#[test]
fn the_start_note_says_what_happens_at_run_end() {
    // 真起后台进程 → 拿住进程表的测试期锁（表的全局性见 proc::table_lock）
    let _g = crate::proc::table_lock();
    let cmd = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let cfg = AppConfig::default();
    let d = TempDir::new("start-note");
    let spec = |keep: bool| crate::proc::StartSpec {
        cmd: cmd.into(),
        ready_cmd: None,
        ready_timeout_secs: None,
        keep_alive: keep,
    };

    let plain = tool_exec_bg(&d.0, &cfg, &spec(false)).expect("应当起得来");
    assert!(plain.contains("未声明 keep_alive"), "{plain}");
    assert!(plain.contains("收掉"), "必须说清 run 结束会收掉它：{plain}");

    let kept = tool_exec_bg(&d.0, &cfg, &spec(true)).expect("应当起得来");
    assert!(kept.contains("已声明 keep_alive"), "{kept}");
    assert!(
        kept.contains("服务】面板"),
        "要告诉模型用户在哪能看见它：{kept}"
    );

    let (stopped, _) = crate::proc::shutdown_for(&d.0, false);
    assert_eq!(stopped.len(), 2, "收尾：两个都收掉，测试不留孤儿");
}

/// **端到端回归**（用户报的 bug 原样复刻）：模型后台起了服务、没声明 keep_alive，
/// 看到就绪就 final 报「服务已启动」—— 而 run 一结束引擎就把它收掉，用户 netstat 一看是空的。
/// 现在：交付前对账打回一次；模型带 keep_alive 重启后，那个服务必须**活过 run 结束**。
#[test]
fn a_service_reported_as_running_must_survive_the_run() {
    // 这条要真起服务、还要它活过 run 结束 —— 全程拿住进程表的测试期锁，
    // 否则并跑的 proc 测试一个 clear_table() 就把条目抹了（进程还在，断言却空了）
    let _g = crate::proc::table_lock();
    let d = TempDir::new("reap-e2e");
    let sleeper = if cfg!(windows) {
        "ping -n 60 127.0.0.1"
    } else {
        "sleep 60"
    };
    let bg = |keep: bool| {
        let extra = if keep { r#","keep_alive":true"# } else { "" };
        let mut s = String::from(r#"{"tool":"execute","args":{"cmd":"#);
        s.push_str(&serde_json::to_string(sleeper).unwrap());
        s.push_str(r#","background":true"#);
        s.push_str(extra);
        s.push_str("}}");
        s
    };
    let llm = crate::testllm::fake_llm(vec![
        bg(false),
        r#"{"final":"服务已启动，端口 8087"}"#.into(),
        bg(true),
        r#"{"final":"服务已启动（已声明 keep_alive，可在【服务】面板停掉）"}"#.into(),
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
        "启动后台服务",
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect("run 不该失败");

    assert_eq!(llm.count(), 4, "起服务 / 报已启动 / 被打回后重启 / 再报");
    assert!(
        llm.request(2).contains("keep_alive"),
        "交付前对账必须把话递给模型：{}",
        llm.request(2)
    );
    assert!(out.answer.contains("keep_alive"), "{}", out.answer);

    let live = crate::proc::listing_for(&d.0);
    assert_eq!(
        live.len(),
        1,
        "声明 keep_alive 的服务必须活过 run 结束：{live:?}"
    );
    assert!(live[0].keep_alive, "留下的那个就是声明过 keep_alive 的");

    let (stopped, _) = crate::proc::shutdown_for(&d.0, false);
    assert_eq!(stopped.len(), 1, "测试自己收尾，不留孤儿");
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
        proj: &d.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Stage,
        backup_dir: None,
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

/// 联网是"随时可用"的能力，但**没写进提示词就等于不存在** —— 实测对照：同一道
/// "某日上证收盘点位"，提示词里没提联网时模型答"该日期在未来，我无法获取"；
/// 提了之后它自行检索并答出准确数值。判据必须成对（该查 / 别查），否则不是
/// 什么都搜就是什么都按训练数据猜。
#[test]
fn the_web_search_hint_states_both_when_to_search_and_when_not() {
    let h = WEB_SEARCH_HINT;
    assert!(
        h.contains("该查"),
        "不教'该查'就得到一个什么都猜的助手：{h}"
    );
    assert!(
        h.contains("别查"),
        "不教'别查'就得到一个什么都搜的助手：{h}"
    );
    assert!(
        h.contains("不许把检索原文整段贴进 final"),
        "与规则 6 一致：final 是给用户看的，贴原文会把手写 JSON 撑到转义出错：{h}"
    );
}

/// 批协议教不教，与引擎收不收由**同一个开关**决定（不虚报能力）
#[test]
fn the_batch_hint_follows_the_switch() {
    let hint = batch_hint(8, true);
    // 子步骤那份**不许出现 plan**：STEP_SYSTEM 里没有这个能力，提了模型会去找它
    let step_hint = batch_hint(8, false);
    assert!(hint.contains("plan"), "主循环要把清单不能进批说清：{hint}");
    assert!(
        !step_hint.contains("plan"),
        "子步骤那份不该提 plan：{step_hint}"
    );
    assert!(
        hint.contains("工具调用") && hint.contains("最多 8"),
        "批提示要说清「一轮发多个工具调用」与上限：{hint}"
    );
    assert!(hint.contains("并发"), "得说清只读会并发、其余按序：{hint}");

    let d = TempDir::new("batch-hint");
    for batch in [true, false] {
        let llm = crate::testllm::fake_llm(vec![r#"{"final":"好的"}"#.into()]);
        let mut cfg = AppConfig::default();
        cfg.llm.base_url = llm.base_url.clone();
        cfg.llm.api_key = "smoke".into();
        cfg.llm.model = "fake".into();
        cfg.agent.batch = batch;
        cfg.gate.full = false;
        cfg.reflect.enabled = false;
        cfg.step.execute_plan = false;
        block_on(run(
            &cfg,
            &d.0,
            "随便说点什么",
            &[],
            WritePolicy::Apply,
            &NoConnector,
            &crate::exec::new_cancel_flag(),
            &QuietSink,
        ))
        .expect("run 不该失败");
        let first = llm.request(0);
        assert_eq!(
            first.contains("批量调用"),
            batch,
            "batch={batch} 时提示词提不提批协议：{first}"
        );
    }
}

// ============================================
// 原子能力的参数形状：read 的窗口 / write 的锚点 / 工具循环的历史折叠
// ============================================

/// 建一个只读上下文（`Ctx` 的字段列表只在这里出现一次：加字段时只改这一处）
fn ctx_for(d: &TempDir) -> Ctx<'_> {
    Ctx {
        proj: &d.0,
        probes: Vec::new(),
        overlay: BTreeMap::new(),
        changes: Vec::new(),
        policy: WritePolicy::Stage,
        backup_dir: None,
    }
}

fn ed(find: &str, replace: &str) -> AgentEdit {
    AgentEdit {
        find: find.to_string(),
        replace: replace.to_string(),
    }
}

/// 500 行的样例文件（窗口读的靶子）
fn lines_fixture(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("line {i}")).collect()
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

/// 五轮"读一个互相认得出来的文件" + 交付：折叠测试的靶子
fn fold_fixture(tag: &str) -> (TempDir, crate::testllm::FakeLlm) {
    let d = TempDir::new(tag);
    for i in 1..=5 {
        d.write(
            &format!("f{i}.txt"),
            &format!("MARK{i}-{}", "A".repeat(400)),
        );
    }
    let script: Vec<String> = (1..=5)
        .map(|i| format!(r#"{{"tool":"read","args":{{"path":"f{i}.txt"}}}}"#))
        .chain(std::iter::once(r#"{"final":"读完了"}"#.to_string()))
        .collect();
    (d, crate::testllm::fake_llm(script))
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
    assert!(
        last.contains("正文已从上下文移除"),
        "还要说清正文去哪了、要看就重新 read"
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

/// 提示词成对写：两种形状都得说清"什么时候用 / 什么时候别用"。
/// 老那句"必须是整份内容"必须消失 —— 留着它，模型看见新形状也不会用。
#[test]
fn prompt_advertises_both_write_shapes_and_the_read_window() {
    assert!(AGENT_SYSTEM.contains("edits"), "不写模型就不知道有锚点形态");
    assert!(AGENT_SYSTEM.contains("offset"), "窗口读也得写进去");
    assert!(
        AGENT_SYSTEM.contains("该用窗口") && AGENT_SYSTEM.contains("别用窗口"),
        "窗口的判据要成对"
    );
    assert!(
        AGENT_SYSTEM.contains("别用 edits") && AGENT_SYSTEM.contains("别用 content"),
        "两种写法的判据要成对"
    );
    assert!(
        !AGENT_SYSTEM.contains("交回的必须是整份内容"),
        "老判据会盖掉新形状"
    );
    assert!(
        !AGENT_SYSTEM.contains("再用 write 交回整份新内容"),
        "规则 3 也得跟着改"
    );
    // 子步骤提示词各自自洽：它没有 connect，但 read/write 的新形状必须有
    assert!(crate::step_agent::STEP_SYSTEM.contains("edits"));
    assert!(crate::step_agent::STEP_SYSTEM.contains("offset"));
    assert!(!crate::step_agent::STEP_SYSTEM.contains("交回的必须是整份内容"));
}
