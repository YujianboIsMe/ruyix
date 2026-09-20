//! 平台探针：验证"到底哪种方式才能正确发现一个命令"。
//!
//! 用法：`cargo run -q -p harness-engine --example cmd_probe -- mvn`
//!
//! ## 为什么留着它
//!
//! `exec::resolve_bin` 的两步走法（先 `where` / `command -v` 解析，再过 shell 取版本）
//! 不是风格偏好，是被这一组实测逼出来的：
//!
//! - `Command::new("mvn")` 在 Windows 上直接 `program not found` —— `CreateProcess`
//!   不认 PATHEXT，`.cmd` / `.bat` / `.ps1` 一律看不见（Maven 装的正是 `mvn.cmd`）；
//! - 所以"在不在"不能靠跑一下看退出码：`cmd /C <不存在的命令>` 的退出码是 **1**，
//!   和"命令跑了但自己退出 1"分不开。
//!
//! 这个例子把几条路并列打出来，**任何一台机器上都能复核**，而不是听我说。
//! Windows-first 的项目里这类坑会反复出现，留着比写在注释里有用。

use std::process::Command;

fn show(label: &str, program: &str, args: &[&str]) {
    match Command::new(program).args(args).output() {
        Ok(o) => {
            let so = String::from_utf8_lossy(&o.stdout);
            let se = String::from_utf8_lossy(&o.stderr);
            let first: String = so
                .lines()
                .chain(se.lines())
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .chars()
                .take(72)
                .collect();
            println!("{label}\n    status={:?} first={first}", o.status.code());
        }
        Err(e) => println!("{label}\n    SPAWN_ERR {e}"),
    }
}

fn main() {
    let bin = std::env::args().nth(1).unwrap_or_else(|| "mvn".to_string());
    println!("探测目标：{bin}\n");

    show(
        &format!("[1] Command::new({bin:?}) —— 直接跑"),
        &bin,
        &["--version"],
    );
    show(&format!("[2] where {bin} —— 解析路径"), "where", &[&bin]);
    show(
        &format!("[3] cmd /C {bin} --version —— 过 shell"),
        "cmd",
        &["/C", &format!("{bin} --version")],
    );
    // 第二步的退出码能不能当"存在性"判据 —— 答案是不能
    show(
        "[4] cmd /C zzz-no-such-tool-zzz —— 不存在的命令的退出码",
        "cmd",
        &["/C", "zzz-no-such-tool-zzz"],
    );
    println!(
        "\n结论：[1] 对 .cmd 工具失明；[2] 的退出码才是\"在不在\"的干净判据；\n\
         [4] 说明 [3] 的退出码不能当判据（不存在与跑失败都是非 0）。"
    );
}
