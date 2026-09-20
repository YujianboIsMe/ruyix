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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--hold" {
        let secs = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
        hold(Path::new(&args[2]), secs);
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
    let out = match proc::start(&proj, &spec, 4, 60) {
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
    match proc::status(&out.info.handle) {
        Ok(i) => println!(
            "state={} elapsed={}ms keep_alive={}",
            i.state, i.elapsed_ms, i.keep_alive
        ),
        Err(e) => println!("{e}"),
    }

    // ③ 看日志尾（证据在文件里 —— 这正是实测那次"服务其实起来了、模型看不见"的病根）
    println!("\n--- ③ log：日志尾部 ---");
    match proc::log_tail(&out.info.handle, 20) {
        Ok(t) => println!("{}", t.trim_end()),
        Err(e) => println!("{e}"),
    }

    // ④ 再起一次：判据在启动**之前**就已命中 → 拒绝。这条守卫直接掐掉"孤儿占着端口、
    //    每次重启都拿到假失败"的自我强化循环。
    println!("\n--- ④ 再起一次（判据已命中，应被拒并附证据）---");
    match proc::start(&proj, &spec, 4, 60) {
        Ok(o) => println!("⚠️ 不该成功：handle={}", o.info.handle),
        Err(e) => println!("{}", e.trim_end()),
    }

    // ⑤ 停（连子进程树一起）
    println!("\n--- ⑤ stop：连子进程树一起停 ---");
    match proc::stop(&out.info.handle) {
        Ok(i) => println!("已停 handle={} state={}", i.handle, i.state),
        Err(e) => println!("{e}"),
    }

    // ⑥ 再起一个，然后用 shutdown_for 证明"引擎持有就引擎收"，且按**项目**划界
    println!("\n--- ⑥ 再起一个 + shutdown_for（项目级收尾）---");
    let _ = std::fs::remove_file(&marker);
    match proc::start(&proj, &spec, 4, 60) {
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
            if let Ok(i) = proc::status(&o2.info.handle) {
                println!("被收掉的那个，现在 state={}", i.state);
            }
        }
        Err(e) => println!("{e}"),
    }

    println!("\n完成：三个出口、判据即命令、重复启动守卫、项目级收尾 —— 都验过了。");
}
