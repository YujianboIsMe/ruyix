//! P0 探针 ②：单实例互斥体**按根区分**这件事到底成不成立。
//!
//! 现状（`src-tauri/src/instance.rs`）是一个写死的名字 `Local\ruyix-instance-mutex` ——
//! 也就是"全世界只有一份 ruyix"。便携形态要求"两份解压副本并存"，所以名字必须带**根的指纹**。
//! 这个探针跑在真进程上（跨进程才是真证据，单进程里 CreateMutexW 的语义 `instance.rs` 已有单测）：
//!
//! 用法：
//! ```text
//! instance_mutex_probe.exe <根路径> [--name-mode root|global] [--hold-secs N]
//! ```
//! `--name-mode global` = 复刻现状（写死名字），用于**对照臂**：它应当证明"不同根也撞"。
//!
//! 预注册判据（跑之前写死）：
//!   - 同根并发两份：恰好一个 `first_instance=true`、一个 `false`（判据 P2-A）
//!   - 不同根并发两份：两个都 `true`（判据 P2-B）
//!   - 对照臂（global 名字）不同根并发：第二个 `false` —— 撞了，说明现状必须改（判据 P2-C）
//!
//! 输出一行 JSON；退出码 0 = first_instance。

use sha2::{Digest, Sha256};
use std::time::Duration;

fn mutex_name(root: &str, mode: &str) -> String {
    match mode {
        "global" => "Local\\ruyix-instance-mutex".to_string(),
        _ => {
            let canon = std::fs::canonicalize(root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| root.to_string());
            let digest = Sha256::digest(canon.to_ascii_lowercase().as_bytes());
            let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            format!("Local\\ruyix-instance-{hex}")
        }
    }
}

#[cfg(windows)]
fn acquire(name: &str) -> (bool, isize) {
    use std::ptr;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe { CreateMutexW(ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        // 失败保守当"已有实例"，与 instance.rs 的口径一致
        return (false, 0);
    }
    let err = unsafe { GetLastError() };
    if err == ERROR_ALREADY_EXISTS || err == ERROR_ACCESS_DENIED {
        unsafe { CloseHandle(handle) };
        (false, 0)
    } else {
        (true, handle as isize)
    }
}

#[cfg(not(windows))]
fn acquire(_name: &str) -> (bool, isize) {
    (true, 0)
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let root = argv.get(1).cloned().unwrap_or_else(|| ".".into());
    let mut mode = "root".to_string();
    let mut hold = 3u64;
    let mut i = 2;
    while i < argv.len() {
        match argv[i].as_str() {
            "--name-mode" => {
                i += 1;
                mode = argv.get(i).cloned().unwrap_or(mode);
            }
            "--hold-secs" => {
                i += 1;
                hold = argv.get(i).and_then(|s| s.parse().ok()).unwrap_or(hold);
            }
            _ => {}
        }
        i += 1;
    }

    let name = mutex_name(&root, &mode);
    let (first, handle) = acquire(&name);
    let evidence = serde_json::json!({
        "pid": std::process::id(),
        "root": root,
        "name_mode": mode,
        "mutex_name": name,
        "first_instance": first,
        "hold_secs": hold,
    });
    println!("{evidence}");

    std::thread::sleep(Duration::from_secs(hold));
    let _ = handle; // 句柄持有到进程退出（与 instance.rs 同一条纪律）
    std::process::exit(if first { 0 } else { 1 });
}
