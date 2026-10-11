//! **委托人自述的事实**（v1.5 全局通道）—— 判据与 `doc/v1.5/问题-跨项目的用户事实搜不到.md` §7
//! 一一对应。夹具（`TempDir` / `block_on` …）在父模块 `tests.rs`，这里 `use super::*` 取。
//!
//! 这一格治的病：用户 A 项目说「我明天有两场面试」，切到 B 项目问 —— agent 说不知道。
//! 实查结论是**准入表缺一格**（五层里没有一层负责"委托人本人的事实"），不是检索坏了。
//! 所以这里的判据必须成对：**能搜到**（正例）与**不灌噪声**（对照臂）都要钉住 ——
//! 只钉前者的话，"什么都往全局塞"的实现照样绿。

use super::*;
use crate::mem::{self, GLOBAL_SCOPE, Memory};

/// 一条 `record_findings` 入参（判据里只用得到 claim / scope / topic）。
fn spec(scope: &str, topic: &str, claim: &str) -> FindingSpec {
    FindingSpec {
        claim: claim.into(),
        evidence: "用户 21:17 那一轮的原话".into(),
        note: String::new(),
        supersedes: None,
        scope: scope.into(),
        topic: topic.into(),
    }
}

/// 建一个**临时记忆库**（不碰用户的真库；也不动进程级安装点 —— 路由函数把它当参数收）。
fn tmp_mem(tag: &str) -> (TempDir, Memory) {
    let d = TempDir::new(tag);
    let m = Memory::open(d.0.join("mem.db"), vec![]).expect("开临时记忆库");
    (d, m)
}

/// 键前缀就是"这条是不是用户自述"的**唯一判据**（检索打标、注入块换口径都读它）。
#[test]
fn the_user_fact_prefix_is_the_only_marker() {
    assert!(mem::is_user_fact_key("user.面试"));
    assert!(mem::is_user_fact_key("USER.工作时间")); // 大小写不该改变判据
    assert!(!mem::is_user_fact_key("user."), "光有前缀不算一条事实");
    assert!(!mem::is_user_fact_key("finding.abc12345"));
    assert!(!mem::is_user_fact_key("tool.cargo"));
}

/// 主题归一：**只折书写差异**（大小写 / 空格 / 标点），不猜语义。
///
/// 这条特别要紧：同一主题必须落进同一个槽位，否则版本更新失效、两条互相打脸的日程并存。
#[test]
fn topic_normalization_folds_spelling_but_not_meaning() {
    assert_eq!(mem::normalize_topic(" 面试 "), "面试");
    assert_eq!(mem::normalize_topic("面试 安排"), "面试-安排");
    assert_eq!(mem::normalize_topic("Work Hours"), "work-hours");
    assert_eq!(mem::normalize_topic("面试！安排？"), "面试-安排");
    assert_eq!(mem::normalize_topic("用户/偏好"), "用户-偏好");
    // 语义不同的两个主题**不许**被归一到一起（引擎不替人猜"这俩是不是一件事"）
    assert_ne!(
        mem::normalize_topic("面试"),
        mem::normalize_topic("面试安排")
    );
    // 纯标点 ⇒ 空 ⇒ 调用方必须报错，不许编一个键
    assert_eq!(mem::normalize_topic("  ！？  "), "");
}

/// **解析阶段**就定死：`scope=global` 没给主题 ⇒ 当面拒（不是延迟到写入时静默处理）。
#[test]
fn global_without_a_topic_is_refused_at_parse_time() {
    let e = parse_action(
        r#"{"tool":"record_findings","args":{"items":[{"claim":"用户明天两场面试","evidence":"原话","scope":"global"}]}}"#,
    )
    .expect_err("缺 topic 必须拒");
    assert!(e.contains("topic"), "{e}");

    // 认不出的 scope 一律拒（**不猜**：猜错的方向是把项目判断灌进跨项目通道）
    let e = parse_action(
        r#"{"tool":"record_findings","args":{"items":[{"claim":"x","evidence":"y","scope":"wherever","topic":"t"}]}}"#,
    )
    .expect_err("未知 scope 必须拒");
    assert!(e.contains("project") && e.contains("global"), "{e}");

    // 齐全 ⇒ 收下，且归一成 Global
    let acts = parse_action(
        r#"{"tool":"record_findings","args":{"items":[{"claim":"用户明天两场面试","evidence":"原话","scope":"global","topic":" 面试 "}]}}"#,
    )
    .expect("齐全的 global 该被受理");
    match acts {
        Action::Findings(items) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].scope_kind().unwrap(), FindingScope::Global);
            assert_eq!(items[0].topic_key().as_deref(), Some("面试"));
        }
        other => panic!("应当是 Findings，实际 {other:?}"),
    }
}

/// **缺省 = 项目层**（判据 2 的前提）：不写 scope 的结论不进全局通道。
#[test]
fn scope_defaults_to_project() {
    let s = spec("", "", "这个项目的 pom 里 spring-boot 是 3.2");
    assert_eq!(s.scope_kind().unwrap(), FindingScope::Project);
    assert!(s.topic_key().is_none());
    // 中文写法也认（模型偶尔按中文思考），但仍只认那两种
    assert_eq!(
        spec("全局", "面试", "c").scope_kind().unwrap(),
        FindingScope::Global
    );
    assert_eq!(
        spec("项目", "", "c").scope_kind().unwrap(),
        FindingScope::Project
    );
}

/// **判据 1（跨项目正例）**：委托人自述的事实落在全局通道的 `user.<主题>` 上，
/// 且**项目层一条都不多**（层与层不串 —— 这是"换项目也答得出来"的前提）。
#[test]
fn a_user_fact_lands_in_the_global_channel_under_its_topic() {
    let (_d, m) = tmp_mem("userfact-global");
    let proj = "D-Projects-Rust-ruyix";
    let landed = route_finding_to_memory(
        Some(&m),
        proj,
        &spec("global", "面试", "用户明天有两场面试"),
    );
    assert_eq!(landed.as_deref(), Some("_global user.面试"), "{landed:?}");

    // 全局层：按内容搜得到
    let g = mem::retrieve::now(&m, GLOBAL_SCOPE, "面试", 5, None).unwrap();
    assert_eq!(g.len(), 1, "全局层该有这一条：{g:?}");
    assert_eq!(g[0].key, "user.面试");
    assert!(g[0].value.contains("明天有两场面试"), "{}", g[0].value);

    // 项目层：一条都没有（这条事实不属于任何项目）
    let p = mem::retrieve::now(&m, proj, "面试", 5, None).unwrap();
    assert!(p.is_empty(), "项目层串味了：{p:?}");
}

/// **判据 2（对照臂）**：对项目的判断**不长全局层分毫** —— 只钉"能搜到"的话，
/// 任何"什么都往全局塞"的实现都会绿。
#[test]
fn a_project_judgement_never_grows_the_global_channel() {
    let (_d, m) = tmp_mem("userfact-control");
    let proj = "D-Projects-Rust-ruyix";
    let before = m.stats(GLOBAL_SCOPE).unwrap().beliefs_all;

    let landed = route_finding_to_memory(
        Some(&m),
        proj,
        &spec("", "", "这个项目的 pom 里 spring-boot 是 3.2"),
    );
    assert!(landed.is_none(), "项目结论不该落全局：{landed:?}");

    assert_eq!(
        m.stats(GLOBAL_SCOPE).unwrap().beliefs_all,
        before,
        "全局层的条数不该因为一条项目结论而变"
    );
    let p = mem::retrieve::now(&m, proj, "spring-boot", 5, None).unwrap();
    assert_eq!(p.len(), 1, "项目层该有这一条：{p:?}");
    assert!(p[0].key.starts_with("finding."), "{}", p[0].key);
}

/// **判据 4（更新语义）**：同一个主题先后两个取值 ⇒ **最新值生效、旧值留痕**。
///
/// "留痕"不是"两条并存打架"：`now` 只给最新那条（旧的状态 superseded、被结构性过滤掉），
/// 而按时间点（`as_of`）仍能把当时的旧值取回来 —— 这正是"主题键版本更新"与
/// "矛盾的结论必须并存"（`finding.<指纹>`，同指纹才折）的分界。
#[test]
fn the_same_topic_updates_instead_of_piling_up() {
    let (_d, m) = tmp_mem("userfact-version");
    // 第一条：世界时间取"100 秒前"（好让第二条的"现在"严格晚于它 —— 秒级时间戳，
    // 同一秒内连写两条会落进"同一时刻"，那测不出更新语义）
    let t0 = chrono::Utc::now().timestamp() - 100;
    m.record_obs(
        GLOBAL_SCOPE,
        "user.面试",
        "用户自述：明天有两场面试",
        mem::Origin::Agent,
        &[],
        Some(t0),
    )
    .expect("第一条");
    // 第二条：同一个主题、不同的值（走**用户事实入口**，键由主题归一后拼出来）
    mem::record_user_fact_obs(
        &m,
        "用户自述：改成一场了",
        "用户 22:10 的原话",
        "",
        "面试",
        mem::Origin::Agent,
    )
    .expect("第二条");

    let now_hits = mem::retrieve::now(&m, GLOBAL_SCOPE, "面试", 5, None).unwrap();
    assert_eq!(
        now_hits.len(),
        1,
        "同一主题只许有一条**活着**的取值：{now_hits:?}"
    );
    assert!(
        now_hits[0].value.contains("改成一场"),
        "{}",
        now_hits[0].value
    );

    // 旧值留痕：按时间点查得到（那时候它确实成立）
    let old = m
        .beliefs_as_of(GLOBAL_SCOPE, Some("user.面试"), t0)
        .unwrap();
    assert!(
        old.iter().any(|h| h.value.contains("两场面试")),
        "旧值必须能按时间点取回（历史不丢）：{old:?}"
    );
}

/// 判据 3（权威序在**读侧**的唯一落点）：标了「用户自述」的条目**不套用「以文件为准」**。
///
/// 两段口径（全局层 / 项目层）都必须是纯函数可测的 —— 措辞一漂，模型就会拿项目文件
/// 去"推翻"用户自己说的话，而那种错在界面上完全看不出来。
#[test]
fn the_reader_marks_user_facts_and_keeps_files_first_away_from_them() {
    let g = crate::agent::search::memory_scope_note(true);
    assert!(
        g.contains(mem::USER_FACT_TAG),
        "全局层口径要标出「用户自述」：{g}"
    );
    assert!(
        g.contains("不套用") && g.contains("并列"),
        "全局层必须写明它不参与「以文件为准」、冲突时并列：{g}"
    );
    let p = crate::agent::search::memory_scope_note(false);
    assert!(
        !p.contains(mem::USER_FACT_TAG),
        "项目层不该出现「用户自述」这套口径：{p}"
    );
    assert!(p.contains("以文件为准"), "项目层仍以文件为准：{p}");

    let hit = |key: &str, value: &str| mem::Hit {
        row_key: format!("_global|{key}|1|e1"),
        key: key.into(),
        value: value.into(),
        status: "active".into(),
        valid_from: 1,
        valid_to: None,
        score: 1.0,
        prov: vec!["e1".into()],
        revs: vec![],
        updated_ts: 1,
    };
    let line = crate::agent::search::memory_line(&hit("user.面试", "用户自述：明天两场面试"));
    assert!(line.contains(mem::USER_FACT_TAG), "{line}");
    assert!(line.contains("user.面试"), "{line}");
    // 项目层的一条普通结论：不加标记（前缀是唯一判据，与正文里恰好写了什么无关）
    let plain = crate::agent::search::memory_line(&hit(
        "finding.abc12345",
        "这个项目的 pom 是 spring-boot 3.2",
    ));
    assert!(!plain.contains(mem::USER_FACT_TAG), "{plain}");
}

/// 注入块（第一屏）也要按层换口径 + 打标 —— 它是"换个项目还答得出来"的**实际通路**：
/// 新项目开跑时，全局块跟着首条用户消息一起进去。
#[test]
fn the_injected_block_switches_its_wording_for_the_global_channel() {
    let (_d, m) = tmp_mem("userfact-block");
    m.record_obs(
        GLOBAL_SCOPE,
        "user.面试",
        "用户自述：明天有两场面试",
        mem::Origin::Agent,
        &[],
        None,
    )
    .unwrap();
    m.record_obs(
        GLOBAL_SCOPE,
        "machine.note",
        "GPU: RTX 5060",
        mem::Origin::Probe,
        &[],
        None,
    )
    .unwrap();

    let g = m
        .block_for_prompt(GLOBAL_SCOPE, 4000)
        .unwrap()
        .expect("有信念就有块");
    assert!(g.contains(mem::USER_FACT_TAG), "{g}");
    assert!(g.contains("user.面试"), "{g}");
    assert!(
        g.contains("不要") && g.contains("以仓库为准"),
        "全局块必须**显式**把「以仓库为准」挡在用户自述之外：{g}"
    );

    // 项目块（没有用户事实时）不该冒出这套口径
    m.record_obs(
        "proj-x",
        "finding.abc",
        "日志级别是 info",
        mem::Origin::Agent,
        &[],
        None,
    )
    .unwrap();
    let p = m.block_for_prompt("proj-x", 4000).unwrap().unwrap();
    assert!(p.contains("以仓库为准"), "{p}");
    assert!(!p.contains(mem::USER_FACT_TAG), "{p}");
}

/// 提示词与工具声明（**模型真正读到的两处**）必须都写上这两格，否则模型根本不会用。
#[test]
fn the_prompt_and_the_tool_declaration_both_teach_the_global_scope() {
    assert!(
        AGENT_SYSTEM.contains("scope:\"global\"") && AGENT_SYSTEM.contains("topic"),
        "系统提示词里没有委托人事实那条规则"
    );
    assert!(
        AGENT_SYSTEM.contains(mem::USER_FACT_TAG),
        "系统提示词里要写出「用户自述」这个标记（读侧按它绕过以文件为准）"
    );
    let decl = crate::llm::tool_decls()
        .into_iter()
        .find(|d| d["function"]["name"] == "record_findings")
        .expect("record_findings 必须有声明");
    let props = &decl["function"]["parameters"]["properties"]["items"]["items"]["properties"];
    assert!(props.get("scope").is_some(), "声明里缺 scope：{decl}");
    assert!(props.get("topic").is_some(), "声明里缺 topic：{decl}");
}

/// **判据 5（写盘 = 零）**：委托人事实的落点是**记忆库**（调用方给的路径），
/// 项目树**一个字节都不写** —— 换项目能答得出来，靠的不是往别人仓库里塞文件。
///
/// 判据成对：项目树指纹不变 **且** 记忆库里确实多了东西 ——
/// 只钉前半条的话，把这段代码整段删掉也能"绿"（什么都没做也是一种"没写"）。
#[test]
fn writing_a_user_fact_touches_no_project_file() {
    let proj = TempDir::new("userfact-nowrite-proj");
    let db = TempDir::new("userfact-nowrite-db");
    let m = Memory::open(db.0.join("mem.db"), vec![]).expect("开库");
    let before = super::search::tree_fingerprint(&proj.0);

    let _ = route_finding_to_memory(
        Some(&m),
        "proj-key",
        &spec("global", "面试", "用户明天有两场面试"),
    );
    let _ = route_finding_to_memory(
        Some(&m),
        "proj-key",
        &spec("", "", "这个项目的日志级别是 info"),
    );

    assert_eq!(
        super::search::tree_fingerprint(&proj.0),
        before,
        "项目树被写了（委托人事实与项目结论都不许落进用户仓库）"
    );
    assert!(
        m.stats(GLOBAL_SCOPE).unwrap().beliefs_all > 0,
        "记忆库里也该真的多了东西 —— 否则『没写』可能只是『什么都没做』"
    );
}

/// **第二个缺陷（跨 run 的钥匙）**：留痕 id 必须在 **run 一开始**就到宿主。
///
/// 造法：脚本一条都不给（假 LLM 剧本演完就收工 ⇒ 第一轮就致命失败）。
/// 判据三件：返回值是 `Err`、sink **已经**收到了 id、留痕**确实在盘上** ——
/// 这正是"键传不出去、盘上那份却已经存在"的那条路。
#[test]
fn the_run_id_reaches_the_host_even_when_the_run_fails() {
    let d = TempDir::new("runid-always");
    let llm = crate::testllm::fake_llm(vec![]);
    let mut cfg = crate::config::AppConfig::default();
    cfg.llm.base_url = llm.base_url.clone();
    cfg.llm.api_key = "t".into();
    cfg.llm.model = "fake".into();
    cfg.project_state_root = d.0.to_string_lossy().to_string();

    let sink = RunIdSink::default();
    let r = block_on(run_with_ask(
        &cfg,
        &d.0,
        "任务",
        &[],
        &[],
        WritePolicy::Apply,
        &NoConnector,
        &NoAsker,
        &crate::exec::new_cancel_flag(),
        &sink,
    ));
    assert!(r.is_err(), "空剧本应当致命失败（这一用例要的就是异常路径）");

    let ids = sink.runs.lock().unwrap().clone();
    assert_eq!(ids.len(), 1, "run 一开始就该报一次 id，实际 {ids:?}");
    let dir = d.0.join("ctx").join(&ids[0]);
    assert!(
        dir.join("transcript.jsonl").is_file(),
        "留痕该在盘上（{}）—— 不早报 id，这一份就永远接不回会话",
        dir.display()
    );
}
