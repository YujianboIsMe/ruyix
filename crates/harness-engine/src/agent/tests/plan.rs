//! 计划与步骤：大纲的 ✅/⛏️/⚠️/⏹️ 判据（`settle_steps` 必须给每个步骤一个终态、
//! 且不许在没有交付的 run 上标"完成"），以及 `step_agent` 的独立上下文与失败回交（v0.4）。

use super::*;

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
