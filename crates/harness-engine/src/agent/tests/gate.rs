//! 门禁（对应 `gate.rs`）：Stage 模式跳过全量验证**并说明原因**、验证不过拦住交付、
//! 过了就放行、预算用尽带着诚实的脚注放行；复核员要能看到命令取证。

use super::*;

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
