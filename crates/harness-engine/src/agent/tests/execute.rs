//! Execute 的三条生命周期：前台 / 后台 / 句柄（对应 `tools.rs` 的执行侧）。
//!
//! 预检的四类拒绝（破坏性模式、启动器、路径形状、没装的命令）、就绪判据的证据要求
//! （自相矛盾的判据要在**起之前**被拒）、句柄不许跨项目、run 结束的收尾对账。

use super::*;

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
    // 包装脚本的**本地落地形态**两端不同：Windows 是 `mvnw.cmd`（cmd 只认带扩展名的落地文件），
    // Unix 是无扩展名的 `mvnw`。测试得跟着写对名字 —— 否则量到的是"这个平台没有这种文件"，
    // 而不是闸门本身（macOS/Linux 上 `.\mvnw.cmd` 是**字面文件名**，永远不存在）。
    #[cfg(target_os = "windows")]
    let (name, run, missing) = ("mvnw.cmd", r".\mvnw.cmd -v", r".\nope.cmd");
    #[cfg(not(target_os = "windows"))]
    let (name, run, missing) = ("mvnw", "./mvnw -v", "./nope");
    d.write(name, "@echo off\n");
    assert!(
        preflight_execute(&d.0, run).is_ok(),
        "存在的项目内脚本不该被拒：{run}"
    );
    assert!(
        preflight_execute(&d.0, "mvnw -v").is_ok(),
        "裸名也要认（项目内脚本按名字就该放行）"
    );
    assert!(
        preflight_execute(&d.0, missing).is_err(),
        "不存在的路径要拒：{missing}"
    );
}

/// 端到端：被闸门挡住时不该留下"执行痕迹"（模型会以为跑过了）。
#[test]
fn tool_execute_refuses_garbage_before_touching_the_shell() {
    let d = TempDir::new("gate-exec");
    let r = tool_execute(&d.0, r"\admin-run\", None);
    assert!(r.starts_with('❌'), "{r}");
    assert!(!r.contains("exit="), "拒绝时不该有执行结果：{r}");
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
    let d = TempDir::new("unknown-handle");
    assert!(tool_proc(&d.0, ProcOp::Status, "p999999").is_err());
    assert!(tool_proc(&d.0, ProcOp::Log, "p999999").is_err());
    assert!(tool_proc(&d.0, ProcOp::Stop, "p999999").is_err());
}

/// **ISSUE-1 的门禁（doc/v1.1/bugs.md）**：按 handle 查进程必须**限定项目**。
///
/// handle 是全局递增的短名，两个项目各有一个 "p1" 是正常状态；只按名字扫全局表
/// 会让项目 B 读到 / 停掉项目 A 的服务。这条判据直接钉住"跨项目不许命中"：
/// 拿 A 的 handle 去 B 查，**必须**是"没这个进程"。
#[test]
fn a_handle_lookup_must_not_reach_across_projects() {
    let _g = crate::proc::table_lock();
    let a = TempDir::new("scope-a");
    let b = TempDir::new("scope-b");
    let cfg = AppConfig::default();
    let sleeper = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let spec = crate::proc::StartSpec {
        cmd: sleeper.into(),
        ready_cmd: Some(r#"netstat -ano | findstr ":65532" | findstr "LISTENING""#.into()),
        ready_timeout_secs: Some(2),
        keep_alive: false,
    };
    let started = crate::proc::start(&a.0, &spec, cfg.proc.max, cfg.proc.ready_timeout_secs, &a.0)
        .expect("起一个不监听端口的进程（判据没命中不等于失败）");
    let h = started.info.handle.clone();
    assert!(
        crate::proc::status(&a.0, &h).is_ok(),
        "自己的项目里必须查得到 handle={h}"
    );
    assert!(
        crate::proc::status(&b.0, &h).is_err(),
        "跨项目 status 必须失败：handle 是短名，不许按名字扫全局表命中别人的进程"
    );
    assert!(
        crate::proc::log_tail(&b.0, &h, 5).is_err(),
        "跨项目 log 必须失败"
    );
    assert!(crate::proc::stop(&b.0, &h).is_err(), "跨项目 stop 必须失败");

    // 收尾也必须按项目：清理 A 不许把 B 的条目抹掉。
    // 这一环钉的是残余偶发的**真正病根** —— 曾经的 `clear_table()` 清的是**全表**，
    // 并跑时把别人的条目一起收了，症状是"handle 的进程已被停止"而进程其实还活着。
    let spec_b = crate::proc::StartSpec {
        cmd: sleeper.into(),
        ready_cmd: Some(r#"netstat -ano | findstr ":65532" | findstr "LISTENING""#.into()),
        ready_timeout_secs: Some(2),
        keep_alive: false,
    };
    let b_started = crate::proc::start(
        &b.0,
        &spec_b,
        cfg.proc.max,
        cfg.proc.ready_timeout_secs,
        &b.0,
    )
    .expect("B 也起一个");
    crate::proc::shutdown_for(&a.0, false);
    assert!(
        crate::proc::status(&b.0, &b_started.info.handle).is_ok(),
        "清理 A 不许抹掉 B 的条目（否则并跑用例会互相看不见）"
    );
    crate::proc::shutdown_for(&b.0, false);
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

/// A（v0.11）：就绪判据与启动命令**自相矛盾**时，引擎**不起进程**、当面把证据摆给模型。
/// 真跑代价：那次 73 轮里 6 次"全停全起"，就是因为判据在等另一个端口 —— 判据永远命不中。
#[test]
fn a_self_contradicting_ready_criterion_is_refused_with_evidence() {
    let _g = crate::proc::table_lock();
    let d = TempDir::new("conflict-criterion");
    let cfg = AppConfig::default();
    let spec = crate::proc::StartSpec {
        cmd: "cd web && npm run dev -- --port 5174 --strictPort".into(),
        ready_cmd: Some(r#"netstat -ano | findstr ":5173""#.into()),
        ready_timeout_secs: Some(3),
        keep_alive: true,
    };
    let err = tool_exec_bg(&d.0, &cfg, &spec).expect_err("必须当面拒");
    assert!(err.contains("自相矛盾"), "{err}");
    assert!(
        err.contains("5174") && err.contains("5173"),
        "两边的端口都要摆出来：{err}"
    );
    assert!(err.contains("没有执行"), "要说清「没起」：{err}");
    assert!(
        crate::proc::listing_for(&d.0).is_empty(),
        "拒了就不该有托管进程"
    );
}

/// A 的第二条腿（v0.11）：判据没命中但进程活着时，观察里必须带**「判据错在哪」**的对比证据
/// —— 只回"没就绪"，模型的下一个动作就是重启。
#[test]
fn a_missed_criterion_carries_the_port_comparison() {
    let _g = crate::proc::table_lock();
    let d = TempDir::new("miss-evidence");
    let cfg = AppConfig::default();
    // 进程活着但**不监听任何端口** → 走"没有任何新端口"那一支（可复现、与平台无关）
    let sleeper = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    let spec = crate::proc::StartSpec {
        cmd: sleeper.into(),
        ready_cmd: Some(r#"netstat -ano | findstr ":65533" | findstr "LISTENING""#.into()),
        // 5 秒而不是 1 秒：**判据没变**（这个端口谁都不监听，必然"没命中"），
        // 变的是余量 —— 1 秒在并行跑全量时会翻分支，表现为"handle 已从进程表移除"。
        ready_timeout_secs: Some(5),
        keep_alive: false,
    };
    let note = tool_exec_bg(&d.0, &cfg, &spec).expect("判据没命中不等于启动失败");
    assert!(note.contains("没命中"), "{note}");
    assert!(note.contains("判据为什么没命中"), "必须给对比证据：{note}");
    assert!(note.contains("启动前在听的端口"), "{note}");
    assert!(note.contains("启动后新出现的端口"), "{note}");
    let (stopped, _) = crate::proc::shutdown_for(&d.0, false);
    assert_eq!(stopped.len(), 1, "收尾：别留孤儿");
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
