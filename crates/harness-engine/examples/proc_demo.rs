//! 托管进程（永不退出的服务）实测工具：跑一遍完整生命周期，每一步都打印**证据**。
//!
//! 用法：`cargo run -q -p harness-engine --example proc_demo`
//!
//! 存在的意义是**可核对**：起 / 判就绪 / 查状态 / 看日志 / 停 / 收尾，不许"应该没问题"。
//! 被测的"服务"用**本示例自己**当替身（`--hold` 模式：写一个 marker 文件后长睡），
//! 所以不依赖 python / java / maven —— 任何平台都能跑出同一份结论。
//!
//! 两点说明：
//! - 这里直接调 `proc::start`，**绕过**了 agent 层的执行前闸门（`preflight_execute`）。
//!   走模型那条路时闸门照旧生效；本示例验的是托管生命周期本身。
//! - 「就绪判据」在这里是"文件存不存在"，对引擎而言它只是**一条命令**（退出码 0 即就绪）——
//!   引擎不认识它测的是什么，这正是"零 app 知识"的落点。

use harness_engine::proc;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 子模式：写 marker 文件、打印几行日志，然后长睡（模拟 servlet 容器起来后的样子）
fn hold(marker: &Path, secs: u64) {
    std::fs::write(marker, b"ready\n").expect("写 marker 失败");
    println!("child: marker 已写入 {}", marker.display());
    println!("child: Started DemoApplication");
    println!("child: Tomcat started on port 8083");
    for _ in 0..secs {
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("child: 时间到，自己退出");
}

/// 「文件存在」当就绪判据。落回引擎视角，它只是**一条命令**：退出码 0 = 就绪。
fn marker_probe(marker: &Path) -> String {
    let m = marker.display();
    if cfg!(windows) {
        format!("if exist \"{m}\" (exit 0) else (exit 1)")
    } else {
        format!("test -f \"{m}\"")
    }
}

/// 子模式：**真监听一个端口**（日志里那个"后端/前端"的替身），永不退出。
/// 用自己当替身是为了不依赖 python / java / maven —— 任何平台都能跑出同一份结论。
///
/// 两种传端口的方式都要能解析，因为演示要区分两种启动命令：
/// - `--serve --port 18100` → **命令里带端口标记**（引擎抽得出来 → 判据矛盾时能当面拒）；
/// - `--serve 18099` → **命令里没有可识别的端口**（端口由配置决定，就像 `mvn spring-boot:run`）
///   → 引擎无从判定，只能靠"没命中时的对比证据"。
fn serve_args(args: &[String]) -> Option<u16> {
    if !args.iter().any(|a| a == "--serve") {
        return None;
    }
    for (i, a) in args.iter().enumerate() {
        if a == "--port" || a == "-p" {
            return args.get(i + 1).and_then(|s| s.parse().ok());
        }
        if a == "--serve"
            && let Some(p) = args.get(i + 1).and_then(|s| s.parse().ok())
        {
            return Some(p);
        }
    }
    None
}

fn serve(port: u16) {
    let l = std::net::TcpListener::bind(("0.0.0.0", port)).expect("绑端口失败（可能已被占用）");
    println!("child: LISTENING on {port}");
    loop {
        if l.accept().is_ok() {}
    }
}

/// 端口判据 —— **真跑里的原样写法**（`netstat -ano | findstr ":端口" | findstr "LISTENING"`）。
fn netstat_probe(port: u16) -> String {
    format!("netstat -ano | findstr \":{port}\" | findstr \"LISTENING\"")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--hold" {
        let secs = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
        hold(Path::new(&args[2]), secs);
        return;
    }
    if let Some(port) = serve_args(&args) {
        serve(port);
        return;
    }

    let me = std::env::current_exe()
        .expect("拿不到自身路径")
        .display()
        .to_string();
    let proj: PathBuf = std::env::temp_dir().join("ruyix_proc_demo");
    std::fs::create_dir_all(&proj).expect("创建项目根失败");
    let marker = proj.join("ready.flag");
    let _ = std::fs::remove_file(&marker);

    let probe = marker_probe(&marker);
    println!("项目根：{}", proj.display());
    println!("就绪判据（一条命令）：{probe}\n");

    let spec = proc::StartSpec {
        cmd: format!("\"{me}\" --hold \"{}\" 30", marker.display()),
        ready_cmd: Some(probe.clone()),
        ready_timeout_secs: Some(15),
        keep_alive: false,
    };

    // ① 后台起 + 等判据
    println!("--- ① start：后台起，并按一条命令等就绪 ---");
    let out = match proc::start(&proj, &spec, 4, 60, &proj) {
        Ok(o) => o,
        Err(e) => {
            println!("{e}");
            std::process::exit(1);
        }
    };
    println!(
        "handle={} pid={} state={} 等了 {:?}",
        out.info.handle,
        out.info.pid,
        out.info.state,
        Duration::from_millis(out.waited_ms as u64)
    );
    match &out.kind {
        proc::StartKind::Ready { evidence } => println!("出口=Ready，证据：{evidence}"),
        other => println!("出口={other:?}（预期 Ready）"),
    }
    println!("日志文件（路径由引擎给，模型不写重定向）：{}", out.info.log);

    // ② 查状态
    println!("\n--- ② status ---");
    match proc::status(&proj, &out.info.handle) {
        Ok(i) => println!(
            "state={} elapsed={}ms keep_alive={}",
            i.state, i.elapsed_ms, i.keep_alive
        ),
        Err(e) => println!("{e}"),
    }

    // ③ 看日志尾（证据在文件里 —— 这正是实测那次"服务其实起来了、模型看不见"的病根）
    println!("\n--- ③ log：日志尾部 ---");
    match proc::log_tail(&proj, &out.info.handle, 20) {
        Ok(t) => println!("{}", t.trim_end()),
        Err(e) => println!("{e}"),
    }

    // ④ 再起一次：判据在启动**之前**就已命中 → 拒绝。这条守卫直接掐掉"孤儿占着端口、
    //    每次重启都拿到假失败"的自我强化循环。
    println!("\n--- ④ 再起一次（判据已命中，应被拒并附证据）---");
    match proc::start(&proj, &spec, 4, 60, &proj) {
        Ok(o) => println!("⚠️ 不该成功：handle={}", o.info.handle),
        Err(e) => println!("{}", e.trim_end()),
    }

    // ⑤ 停（连子进程树一起）
    println!("\n--- ⑤ stop：连子进程树一起停 ---");
    match proc::stop(&proj, &out.info.handle) {
        Ok(i) => println!("已停 handle={} state={}", i.handle, i.state),
        Err(e) => println!("{e}"),
    }

    // ⑥ 再起一个，然后用 shutdown_for 证明"引擎持有就引擎收"，且按**项目**划界
    println!("\n--- ⑥ 再起一个 + shutdown_for（项目级收尾）---");
    let _ = std::fs::remove_file(&marker);
    match proc::start(&proj, &spec, 4, 60, &proj) {
        Ok(o2) => {
            println!(
                "handle={} pid={} state={}",
                o2.info.handle, o2.info.pid, o2.info.state
            );
            let (stopped, kept) = proc::shutdown_for(&proj, true);
            println!("收掉 {} 个，留下 {} 个", stopped.len(), kept.len());
            for i in &stopped {
                println!("  ✗ {} pid={} state={}", i.handle, i.pid, i.state);
            }
            if let Ok(i) = proc::status(&proj, &o2.info.handle) {
                println!("被收掉的那个，现在 state={}", i.state);
            }
        }
        Err(e) => println!("{e}"),
    }

    // ⑦ v0.11：**判据与命令自相矛盾 → 当面拒**（命令里带可识别端口标记的情形）。
    //    复刻真跑那次的形状：命令写着一个端口、判据却在等另一个 —— 判据永远命不中，
    //    白等一整个超时之后必然误判成"没起来"。真跑代价：一次 run 里 6 次"全停全起"、73 轮。
    println!("\n--- ⑦ 判据与命令自相矛盾（应被拒，且不该起进程）---");
    let conflicting = proc::StartSpec {
        cmd: format!("\"{me}\" --serve --port 18100"),
        ready_cmd: Some(netstat_probe(18999)),
        ready_timeout_secs: Some(5),
        keep_alive: false,
    };
    match proc::start(&proj, &conflicting, 4, 60, &proj) {
        Ok(o) => println!("⚠️ 不该成功：handle={}", o.info.handle),
        Err(e) => println!("{}", e.trim_end()),
    }

    // ⑧ v0.11：**命令里没有可识别的端口**（端口由配置决定 —— 就像 `mvn … spring-boot:run`）
    //    ＋判据是别的服务的端口 → 引擎**不能拒**（那是误伤），但判据没命中时必须给出
    //    **判据错在哪**的对比证据。真跑就是死在这一句没给：模型拿到"没就绪"→ 重启 →
    //    还是同一条错判据 → 自我强化。
    println!("\n--- ⑧ 判据没命中：给出「判据错在哪」的对比证据 ---");
    let mismatched = proc::StartSpec {
        cmd: format!("\"{me}\" --serve 18099"), // 真监听 18099，但命令里没有端口标记
        ready_cmd: Some(netstat_probe(18999)),  // 永远命不中：判据在等另一个端口
        ready_timeout_secs: Some(4),
        keep_alive: false,
    };
    let mut wrong_handle = None;
    match proc::start(&proj, &mismatched, 4, 60, &proj) {
        Ok(o) => {
            wrong_handle = Some(o.info.handle.clone());
            match &o.kind {
                proc::StartKind::NotReady { hint, .. } => {
                    println!(
                        "出口=NotReady（进程还活着 —— 不是启动失败）handle={}",
                        o.info.handle
                    );
                    println!("{}", hint.clone().unwrap_or_default());
                }
                other => println!("出口={other:?}（预期 NotReady）"),
            }
        }
        Err(e) => println!("{e}"),
    }

    // ⑨ 阳性对照：先收掉 ⑧ 那个（否则判据在启动前就已命中，会被①号守卫拒），
    //    再拿**同一条命令**配**正确**的判据 → 应当 Ready（证明闸门没误伤、happy path 没坏）
    println!("\n--- ⑨ 阳性对照：同一条命令 + 正确的判据（应 Ready）---");
    if let Some(h) = &wrong_handle {
        match proc::stop(&proj, h) {
            Ok(i) => println!("先收掉 ⑧ 那个：handle={} state={}", i.handle, i.state),
            Err(e) => println!("{e}"),
        }
    }
    let good = proc::StartSpec {
        cmd: format!("\"{me}\" --serve 18099"),
        ready_cmd: Some(netstat_probe(18099)),
        ready_timeout_secs: Some(15),
        keep_alive: false,
    };
    match proc::start(&proj, &good, 4, 60, &proj) {
        Ok(o) => {
            println!(
                "handle={} pid={} 等了 {:?}",
                o.info.handle,
                o.info.pid,
                Duration::from_millis(o.waited_ms as u64)
            );
            match &o.kind {
                proc::StartKind::Ready { evidence } => println!("出口=Ready，证据：{evidence}"),
                other => println!("出口={other:?}（预期 Ready）"),
            }
        }
        Err(e) => println!("{}", e.trim_end()),
    }
    let (stopped, kept) = proc::shutdown_for(&proj, false);
    println!(
        "\n收尾：收掉 {} 个、留下 {} 个（不留孤儿）",
        stopped.len(),
        kept.len()
    );

    println!(
        "\n完成：三个出口、判据即命令、重复启动守卫、项目级收尾、**判据自相矛盾守卫** —— 都验过了。"
    );
}
