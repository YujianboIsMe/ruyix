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

/// 后台模式的**词汇**（background / ready_cmd / handle / 就绪）以工具声明为家 ——
/// 声明面是三个受众共享的，模型在调用时刻看的就是它。不写这些词，模型就还用
/// start / Start-Process 那套花招，而那正是这轮的病根（输出拿不到 + 进程脱离掌控 + 桌面弹窗）。
#[test]
fn the_execute_declaration_documents_the_background_mode() {
    let decl = crate::llm::tool_decls()
        .into_iter()
        .find(|t| t["function"]["name"] == "execute")
        .expect("工具表必须有 execute");
    let text = serde_json::to_string(&decl).unwrap();
    for k in ["background", "ready_cmd", "handle", "就绪", "keep_alive"] {
        assert!(text.contains(k), "execute 声明缺 {k}：{text}");
    }
    assert!(text.contains("Start-Process"), "得把 start 花招点掉：{text}");
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

/// 提示词成对写：**策略**判据（该用 / 别用）留在 AGENT_SYSTEM，**形状与机械事实**
/// （edits / offset / 上限 / 失败语义）的家是工具声明 —— 子步骤与复核员共享那份声明面，
/// 形状到不到场看 `tools` 数组，不看谁家的系统提示词里抄没抄。
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
    // 机械事实的家：TOOL_DECLS（read 的窗口上限、write 的 find 唯一性与整批作废、
    // execute 的超时上限 —— 这些曾经只在 AGENT_SYSTEM 里，两处必漂移）
    let decls = serde_json::to_string(&crate::llm::tool_decls()).unwrap();
    for k in [
        "edits",
        "offset",
        "上限 400",
        "恰好出现一次",
        "整批作废",
        "上限 120",
    ] {
        assert!(decls.contains(k), "工具声明缺 {k}：{decls}");
    }
    assert!(
        !decls.contains("交回的必须是整份内容"),
        "老判据不该在声明面复现"
    );
}

/// 多模态：任务消息带图 ⇒ **第一条请求**里就有图块（OpenAI 兼容路的形状）。
///
/// 为什么要在循环这一层量（而不是只测 `llm.rs` 里的序列化）：图是随**第一条 user 消息**
/// 出去的，而那条消息是引擎自己拼的（`head` + `用户消息：` 前缀）—— 贴错位置（贴进历史、
/// 或者干脆没贴）在序列化单测里看不出来。
///
/// 只量 openai 路：假 LLM 桩只服务 `/chat/completions` 这一条路由，anthropic 那条的
/// **块形状**在 `llm.rs::protocol_tests::anthropic_route_puts_images_in_base64_blocks`
/// 里量（同一件事不必两处都测 —— 两处都测等于两处都要维护）。
#[test]
fn the_task_image_goes_out_with_the_first_request() {
    let d = TempDir::new("vision-first-request");
    let llm = crate::testllm::fake_llm(vec![r#"{"final":"看到了：红色方块"}"#.into()]);
    let mut cfg = ask_cfg(&llm);
    // 必须是**能读图**的模型，否则先被能力闸拦下（那是另一条用例的事）
    cfg.llm.model = "deepseek-flash".into();
    cfg.llm.api_format = "openai".into();
    let img = ImagePart {
        mime: "image/png".into(),
        data_base64: "QUJD".into(),
    };
    let out = block_on(run_with_ask(
        &cfg,
        &d.0,
        "这张截图里是什么？",
        &[],
        &[img],
        WritePolicy::Apply,
        &NoConnector,
        &NoAsker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .unwrap();
    assert_eq!(out.answer, "看到了：红色方块");
    let body = llm.request(0);
    assert!(body.contains("image_url"), "缺图块：{body}");
    assert!(body.contains("QUJD"), "图的字节没发出去：{body}");
    assert!(body.contains("这张截图里是什么"), "文字部分还得在：{body}");
}

/// 读图能力闸在**循环这一层**也成立：盲模型 + 图 ⇒ 拒绝，
/// 而且拒绝发生在**发请求之前**（一张图都不许漏出去 —— 把图丢掉再当纯文本发，
/// 等于拿一个看起来正常的答复冒充"看图说话"）。
#[test]
fn a_blind_model_is_refused_before_anything_is_sent() {
    let d = TempDir::new("vision-blind");
    let llm = crate::testllm::fake_llm(vec![r#"{"final":"没看到图"}"#.into()]);
    let mut cfg = ask_cfg(&llm);
    cfg.llm.model = "deepseek-v4-pro".into(); // 表里写着读不了图
    let img = ImagePart {
        mime: "image/png".into(),
        data_base64: "QUJD".into(),
    };
    let err = block_on(run_with_ask(
        &cfg,
        &d.0,
        "看图",
        &[],
        &[img],
        WritePolicy::Apply,
        &NoConnector,
        &NoAsker,
        &crate::exec::new_cancel_flag(),
        &QuietSink,
    ))
    .expect_err("盲模型带图必须被拒");
    assert!(err.contains("能读图"), "{err}");
    assert_eq!(llm.count(), 0, "请求不该发出去");
}
