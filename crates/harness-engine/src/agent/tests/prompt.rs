//! 提示词与注入：模型这一轮**看到什么**（对应 `prompt.rs`）。
//!
//! 四类断言：根提示词（四原语 / 平台检索提示词）、写入策略注释（确认模式那三条事实）、
//! 每轮注入的开关跟随（批量 / 联网 / 命令清单）、交付与托管进程的后果说明。

use super::*;

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
