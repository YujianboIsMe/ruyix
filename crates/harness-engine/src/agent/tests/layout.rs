//! 上下文布局与重基线调度（v1.2 P1/P4，对应 `context.rs` / `scheduler.rs`）：
//! 开关关时逐轮重建 system（对照组）、开时根段逐字节不变 + 易变尾在最后 + 公共前缀占比上升；
//! 以及"什么时候压"由目标函数决定（预算越界是**强制**压、视野未知走 ski-rental 兜底）。

use super::*;

/// **运行时 I1**（P1 的第一条判据）：`layout` 打开时，相邻两轮的 S 段**逐字节相同**，
/// 且它就是 `agent_system_prompt()` 那条常量本身（不是"常量 + 点别的"）。
///
/// 关掉时这条必然不成立（每轮重建 system）—— 见下一条测试，那正是本版要治的病。
#[test]
fn layout_on_keeps_the_root_byte_identical_across_rounds() {
    let (d, llm) = fold_fixture("ctx-layout-i1");
    let mut cfg = ask_cfg(&llm);
    cfg.agent.ctx.layout = true;

    block_on(run_five_reads(&cfg, &d.0));
    assert_eq!(llm.count(), 6, "5 轮读 + 1 轮交付");

    let want = agent_system_prompt();
    for i in 0..6 {
        let msgs = req_msgs(&llm, i);
        assert_eq!(
            msgs[0].content,
            want,
            "第 {} 轮：S 段必须等于系统提示词常量本身",
            i + 1
        );
        // 任务头（第一条 user 消息）也在稳定区：它从头到尾不该动
        assert!(
            msgs[1].content.contains("项目根目录"),
            "第 {} 轮：第一条 user 消息该是任务头",
            i + 1
        );
    }
    let roots: Vec<String> = (0..6)
        .map(|i| req_msgs(&llm, i)[0].content.clone())
        .collect();
    let distinct = roots.iter().collect::<std::collections::HashSet<_>>().len();
    assert_eq!(distinct, 1, "六次请求的 S 段只该有**一个**指纹");
}

/// **I1 的反面**（同一份靶子、开关关掉）：块拼在 system 尾部 ⇒ 每轮重建 system，
/// 相邻两轮的 S 段必然不同。这条钉住"关掉时与 v1.1 一字不变"，
/// 同时它也是 P1 要消灭的那个事实的**在场证明**（不是我们嘴上说它不好）。
#[test]
fn layout_off_still_rebuilds_the_system_message_every_round() {
    let (d, llm) = fold_fixture("ctx-layout-off");
    let cfg = ask_cfg(&llm); // 默认关

    block_on(run_five_reads(&cfg, &d.0));
    let first = req_msgs(&llm, 0)[0].content.clone();
    let second = req_msgs(&llm, 1)[0].content.clone();
    assert!(
        first.starts_with(&agent_system_prompt()),
        "老行为：system = 提示词常量 + 块"
    );
    assert!(
        first.contains("已确认的事实"),
        "老行为：findings/账本块在 system 里"
    );
    assert_ne!(
        first, second,
        "关掉布局时 system 每轮都该变（账本多了一行）—— 这就是每轮砸缓存的现状"
    );
    // 尾布局里那条"末尾多一条 user 块"在关闭时**不该**出现
    let last_off = req_msgs(&llm, 5).pop().unwrap();
    assert_eq!(last_off.role, "user");
    assert!(
        !last_off.content.contains("## 已确认的事实"),
        "关闭时最后一条消息该是工具结果，不该是账块"
    );
}

/// 尾布局的形状：块是请求体的**最后一条消息**（易变尾 A），
/// 而且**每一轮都在**（不是只有第一轮）。
#[test]
fn layout_on_puts_the_volatile_block_last() {
    let (d, llm) = fold_fixture("ctx-layout-tail");
    let mut cfg = ask_cfg(&llm);
    cfg.agent.ctx.layout = true;

    block_on(run_five_reads(&cfg, &d.0));
    for i in 0..6 {
        let msgs = req_msgs(&llm, i);
        let last = msgs.last().expect("请求体不该是空的");
        assert_eq!(last.role, "user", "第 {} 轮：尾该是 user 消息", i + 1);
        assert!(
            last.content.contains("## 已确认的事实"),
            "第 {} 轮：尾必须是账块（findings + 引擎账本）",
            i + 1
        );
        // 块只该出现在末尾一处：S 段里**不许**再有它
        assert!(
            !msgs[0].content.contains("已确认的事实"),
            "第 {} 轮：块不许同时留在 system 里（那就是两份会打架的账）",
            i + 1
        );
    }
}

/// **P1 的主判据**（可证伪）：同一份靶子、同一段脚本，尾布局的加权公共前缀占比
/// 必须**高于**"块塞进 system"那一臂。
///
/// 这条就是需求 §5 的否定判据在单测里的形状：方向反了（占比下降）⇒ 红。
/// 真 run 上还会再量一次（同一判据、真实模型），这里先用确定性脚本钉住方向。
#[test]
fn layout_raises_the_common_prefix_ratio() {
    // 臂一：块塞进 system（v1.1 行为）
    let (d_off, llm_off) = fold_fixture("ctx-prefix-off");
    block_on(run_five_reads(&ask_cfg(&llm_off), &d_off.0));
    // 臂二：块是易变尾
    let (d_on, llm_on) = fold_fixture("ctx-prefix-on");
    let mut cfg_on = ask_cfg(&llm_on);
    cfg_on.agent.ctx.layout = true;
    block_on(run_five_reads(&cfg_on, &d_on.0));

    let (a, b) = (prefix_tally(&llm_off, 6), prefix_tally(&llm_on, 6));
    assert!(
        b.ratio() > a.ratio(),
        "尾布局的加权复用率必须更高：尾 {} vs 塞根 {}",
        b.render(),
        a.render()
    );
    // 首轮按 0 前缀计（它本来就没有可复用的东西）—— 这条也要钉住，别靠"不算首轮"抬分
    assert_eq!(prefix_tally(&llm_on, 1).prefix, 0);
}

/// **折叠的下标纪律**：尾只在请求期间存在，调完就摘 —— `fold_history` 按下标改老轮次
/// 正文，任何跨轮的插/删都会让下标偏一格（折错消息，或直接越界 panic）。
///
/// 这条判据的针对性：把"摘尾"挪到下一轮开头（不修下标）就会红。
#[test]
fn layout_on_does_not_shift_the_fold_bookkeeping() {
    let (d, llm) = fold_fixture("ctx-layout-fold");
    let mut cfg = ask_cfg(&llm);
    cfg.agent.ctx.layout = true;
    cfg.agent.history_keep_rounds = 2;

    let out = block_on(run_five_reads(&cfg, &d.0));
    assert!(out.answer.contains("读完了"), "{}", out.answer);
    let last = llm.request(5);
    assert!(
        last.contains("正文已折叠"),
        "窗口外的老轮次该被折叠：{last}"
    );
    assert!(
        !last.contains("MARK1") && !last.contains("MARK2"),
        "窗口外的正文该被折掉，而窗口内的要留着：{last}"
    );
    assert!(
        last.contains("MARK4") && last.contains("MARK5"),
        "窗口内的正文一字不该动：{last}"
    );
    assert!(
        last.contains("## 已确认的事实"),
        "折叠与尾布局同时开着时，尾该照旧在：{last}"
    );
}

/// 出厂默认就是这套算法，**不写任何配置**也该看见它在工作（2026-09-27 拍板后的判据）。
///
/// 这条判据的存在理由：用户质疑过"到底用没用账本？我怎么看着每轮都在折叠" ——
/// 当时五个开关默认全关，界面上自然一条账本行都没有。现在默认全开，
/// 所以"默认配置 + 一次重复读 ⇒ trace 里必须出现 `dedup` / `上下文计量` / `rebase 决策=`"。
#[test]
fn the_shipped_default_config_shows_the_algorithm_at_work() {
    let d = TempDir::new("shipped-default");
    d.write("f1.txt", "一小段正文\n");
    let mut cfg = AppConfig::default(); // ← **一个 ctx 开关都不碰**
    cfg.gate.narrow = false;
    cfg.gate.full = false;
    cfg.reflect.enabled = false;
    cfg.step.execute_plan = false;
    cfg.discover.enabled = false;
    let script = vec![
        r#"{"tool":"read","args":{"path":"f1.txt"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"f1.txt"}}"#.to_string(),
        r#"{"final":"读完了"}"#.to_string(),
    ];
    let (_, _, log) = block_on(run_logging(&cfg, &d.0, script));
    assert!(log.contains("上下文计量"), "量尺没生效（默认该开）：{log}");
    assert!(
        log.contains("dedup read f1.txt"),
        "去重账本没生效（默认该开）：{log}"
    );
    assert!(
        log.contains("rebase 决策="),
        "重基线调度没生效（默认该开）：{log}"
    );
    assert!(log.contains("拦截重复 1"), "第二次读没有被拦下：{log}");
}

/// **P4 的验收判据**：调度器开着时，压缩的**时机由调度器判定**、不再由"过了几轮"决定。
///
/// 判据取的是**两个机制会给出不同答案**的那个场合：`keep_rounds = 6` 但**视野只剩 1-2 轮**
/// （`horizon = 8`）。老规则会说"已经过了 6 轮 ⇒ 折"，而代价模型会算出一句反话 ——
/// "付 0.9L 只省 1 轮，纯亏" ⇒ 一次都不折。trace 里三种理由都看得见。
///
/// 注：**长视野**下 DP 通常会同意"把阶梯压小"（carry 0.1 vs rebuild 0.9，压得勤反而便宜），
/// 所以这一期的价值主要在**上界（预算强制压）**与**收尾（快结束时别重建）**这两处，
/// 不是"比老规则折得更少"。这一点写在这里，免得后来人拿"折得少不少"当判据。
#[test]
fn the_scheduler_decides_when_to_rebase_not_the_round_count() {
    let script = || {
        let mut v: Vec<String> = (1..=10)
            .map(|i| {
                format!(
                    r#"{{"tool":"read","args":{{"path":"f{}.txt"}}}}"#,
                    (i % 3) + 1
                )
            })
            .collect();
        v.push(r#"{"final":"读完了"}"#.to_string());
        v
    };
    let mk = || {
        let d = TempDir::new("sched-vs-rounds");
        for i in 1..=3 {
            d.write(
                &format!("f{i}.txt"),
                "一小段正文
",
            );
        }
        d
    };
    let mut cfg_on = quiet_cfg();
    cfg_on.agent.history_trim = true;
    cfg_on.agent.history_keep_rounds = 6; // 老触发器：过 6 轮就该折
    cfg_on.agent.ctx.metrics = true;
    cfg_on.agent.ctx.schedule = true;
    cfg_on.agent.ctx.horizon = 8; // 视野只剩 1-2 轮 ⇒ 重建摊不回来
    let d1 = mk();
    let (_, _, log_on) = block_on(run_logging(&cfg_on, &d1.0, script()));

    let mut cfg_off = cfg_on.clone();
    cfg_off.agent.ctx.schedule = false;
    let d2 = mk();
    let (_, _, log_off) = block_on(run_logging(&cfg_off, &d2.0, script()));

    // 关着：老规则照旧（轮数够了就折）—— 对照组必须成立，否则"不同"说明不了什么
    assert!(
        log_off.contains("历史折叠"),
        "关着调度器时，老触发条件必须一字不变：{log_off}"
    );
    // 开着：同一个 keep_rounds，调度器说不值得重建 ⇒ 一次都不折
    assert!(
        !log_on.contains("历史折叠"),
        "调度器开着时，'过了几轮'不再是触发器：{log_on}"
    );
    // 三种理由在 trace 里都要看得见：下限挡着 / DP 判定 / 兜底
    assert!(
        log_on.contains("理由=下限"),
        "前 6 轮该由下限挡着：{log_on}"
    );
    assert!(
        log_on.contains("理由=DP") || log_on.contains("理由=ski-rental"),
        "轮数够了之后必须由调度器判定：{log_on}"
    );
    assert!(
        log_on.contains("字节代理计量"),
        "trace 必须写明是**字节代理**计量（架构 §6）：{log_on}"
    );
}

/// 预算越界 ⇒ **必须**压（G2 是硬约束）：trace 里理由必须写"预算"
#[test]
fn a_budget_overflow_forces_the_fold() {
    let d = TempDir::new("sched-budget");
    for i in 1..=5 {
        d.write(&format!("f{i}.txt"), "一小段正文\n");
    }
    let mut cfg = quiet_cfg();
    cfg.agent.history_trim = true;
    cfg.agent.history_keep_rounds = 6; // 触发器够不着（5 轮就结束了）
    cfg.agent.ctx.metrics = true;
    cfg.agent.ctx.schedule = true;
    cfg.agent.ctx.budget_tokens = 1; // 任何阶梯都越界 ⇒ 每轮都该压
    let (_, _, log) = block_on(run_logging(&cfg, &d.0, sched_script()));
    assert!(log.contains("理由=预算"), "越过预算就该由预算强制压：{log}");
    assert!(log.contains("历史折叠"), "预算压了就必须真折：{log}");
}

/// 视野跑完（未知 horizon）时走 ski-rental 兜底，理由必须写出来
#[test]
fn an_exhausted_horizon_falls_back_to_ski_rental() {
    let d = TempDir::new("sched-ski");
    for i in 1..=3 {
        d.write(&format!("f{i}.txt"), "一小段正文\n");
    }
    let mut cfg = quiet_cfg();
    cfg.agent.history_trim = true;
    cfg.agent.history_keep_rounds = 1;
    cfg.agent.ctx.metrics = true;
    cfg.agent.ctx.schedule = true;
    // 把视野设成 1：跑两步就"未知 horizon"了 ⇒ 该看到 ski-rental 的理由
    cfg.agent.ctx.horizon = 1;
    let script = vec![
        r#"{"tool":"read","args":{"path":"f1.txt"}}"#.to_string(),
        r#"{"tool":"read","args":{"path":"f2.txt"}}"#.to_string(),
        r#"{"final":"x"}"#.to_string(),
    ];
    let (_, _, log) = block_on(run_logging(&cfg, &d.0, script));
    assert!(
        log.contains("理由=ski-rental"),
        "视野用完 ⇒ 走 ski-rental 兜底：{log}"
    );
}

/// 字节代理：图算**固定配额**，不按 base64 字面量算。
///
/// 这不是精度洁癖：调度器拿这个数判"超没超预算"，而超预算在 P4 里是**强制压**历史。
/// 一张 1600px 截图的 base64 有 30 万字节 —— 按字面量算就是 7.5 万 token，于是每一轮
/// 都判"该压"，历史被反复折叠成摘要；而厂商实际按**像素**计价，这张图只有 1k 出头 token。
#[test]
fn the_byte_proxy_counts_an_image_as_a_fixed_allowance_not_as_base64() {
    let one = |n: usize| {
        ChatMessage::user("hi").with_images(vec![ImagePart {
            mime: "image/png".into(),
            data_base64: "A".repeat(n),
        }])
    };
    assert_eq!(
        msgs_bytes(&[ChatMessage::user("hi")]),
        2,
        "无图 = 纯文本字节"
    );
    let small = msgs_bytes(&[one(1)]);
    let huge = msgs_bytes(&[one(300_000)]);
    assert_eq!(small, huge, "30 万字节的 base64 不许让阶梯读数虚涨");
    assert_eq!(huge, 2 + IMAGE_PROXY_BYTES, "一张图 = 一个固定配额");
    assert_eq!(
        msgs_bytes(&[one(1), ChatMessage::user("hi")]),
        huge + 2,
        "多消息照样累加"
    );
}
