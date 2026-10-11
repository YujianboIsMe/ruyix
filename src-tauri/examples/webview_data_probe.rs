//! P0 探针 ①：WebView2 的用户数据目录能不能落在**我们指定的位置**。
//!
//! 为什么必须先做这个：便携形态（A5）的成立与否全押在它身上 —— 只要 WebView2 的 profile
//! 还在 `%LOCALAPPDATA%`，`删文件夹即净` 就是假的。架构文档 §八 与 §十一 的第一条风险都指向这里。
//!
//! 用法（两条臂，缺一不可）：
//! ```text
//! cargo run -p ruyix --example webview_data_probe -- --data-dir <绝对路径>   # 指定臂
//! cargo run -p ruyix --example webview_data_probe -- --default              # 对照臂
//! ```
//! 预注册判据（**跑之前**就写死，不许事后解释）：
//!   - 指定臂 PASS 条件：① `<data-dir>/EBWebView` 出现；② 运行前后 `%LOCALAPPDATA%` 无新增顶层目录；
//!     ③ `%LOCALAPPDATA%\<identifier>` 的 `EBWebView/Local State` 时间戳**没有推进**（没被碰）。
//!   - 对照臂（不指定）预期：`%LOCALAPPDATA%\<identifier>` 被创建或被碰到 —— 它成立，才说明
//!     "今日外溢"不是我猜的，而是可复现的事实；也才说明指定臂的 ③ 有鉴别力。
//!
//! 输出：一行 JSON 证据到 stdout；退出码 0 = 该臂达预期，1 = 未达预期。
//!
//! 窗口是 `visible(false)`：探针不该打扰正在用 IDE 的人。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, SystemTime};

/// 等 WebView2 把 profile 落盘的宽限（它是异步初始化的）
const SETTLE: Duration = Duration::from_secs(6);

struct Args {
    data_dir: Option<PathBuf>,
    keep_open_secs: u64,
}

fn parse_args() -> Args {
    let mut a = Args {
        data_dir: None,
        keep_open_secs: 0,
    };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--data-dir" => {
                i += 1;
                a.data_dir = argv.get(i).map(PathBuf::from);
            }
            "--default" => a.data_dir = None,
            "--keep-open-secs" => {
                i += 1;
                a.keep_open_secs = argv.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            other => eprintln!("[probe] 忽略未知参数 {other}"),
        }
        i += 1;
    }
    a
}

fn local_appdata() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

/// 顶层目录名集合
fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// 某个文件/目录的最后修改时刻（用来判断"有没有被碰过"）
fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok().and_then(|m| m.modified().ok())
}

fn main() {
    let args = parse_args();
    let lads = local_appdata();
    let lads_before = lads.as_deref().map(entries).unwrap_or_default();

    // tauri 的 identifier（今日 WebView2 默认 profile 的落点就是它）
    let identifier = "com.ruyix.code".to_string();
    let local_state = lads
        .as_ref()
        .map(|p| p.join(&identifier).join("EBWebView").join("Local State"));
    let local_state_before = local_state.as_deref().and_then(mtime);

    println!(
        "[probe] 指定目录 = {}",
        args.data_dir
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "（无：对照臂）".into())
    );

    let verdict_code = Arc::new(AtomicI32::new(1));
    let code = verdict_code.clone();
    let data_dir = args.data_dir.clone();
    let keep_open = args.keep_open_secs;

    let app = tauri::Builder::default()
        .setup(move |app| {
            let url = tauri::WebviewUrl::External("about:blank".parse().unwrap());
            let mut builder = tauri::WebviewWindowBuilder::new(app, "probe", url)
                .title("ruyix webview data probe")
                .visible(false)
                .inner_size(400.0, 300.0);
            if let Some(d) = data_dir.as_ref() {
                builder = builder.data_directory(d.clone());
            }
            let window = builder.build()?;
            println!("[probe] 窗口已建（visible=false），等 {SETTLE:?} 让 WebView2 落盘…");

            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(SETTLE);

                let lads_after = local_appdata()
                    .as_deref()
                    .map(entries)
                    .unwrap_or_default();
                let new_entries: Vec<String> = lads_after
                    .iter()
                    .filter(|e| !lads_before.contains(e))
                    .cloned()
                    .collect();
                let local_state_after = local_state.as_deref().and_then(mtime);
                let local_touched = match (local_state_before, local_state_after) {
                    (Some(a), Some(b)) => b > a,
                    (None, Some(_)) => true,
                    _ => false,
                };

                let (profile_ok, profile_path) = match data_dir.as_ref() {
                    Some(d) => {
                        let eb = d.join("EBWebView");
                        (eb.is_dir(), Some(eb.display().to_string()))
                    }
                    None => {
                        // 对照臂：默认落点应当被创建/被碰
                        (
                            lads_after.contains(&identifier),
                            lads.as_ref().map(|p| p.join(&identifier).display().to_string()),
                        )
                    }
                };

                let pass = if data_dir.is_some() {
                    profile_ok && new_entries.is_empty() && !local_touched
                } else {
                    profile_ok && local_touched
                };

                let evidence = serde_json::json!({
                    "arm": if data_dir.is_some() { "data-dir" } else { "default" },
                    "data_dir_arg": data_dir.as_ref().map(|p| p.display().to_string()),
                    "profile_created_at_target": profile_ok,
                    "profile_path": profile_path,
                    "new_localappdata_entries": new_entries,
                    "localappdata_identifier_touched": local_touched,
                    "localappdata_identifier_local_state_before": local_state_before.map(|t| format!("{t:?}")),
                    "localappdata_identifier_local_state_after": local_state_after.map(|t| format!("{t:?}")),
                    "pass": pass,
                });
                println!("[probe] 证据 {evidence}");

                code.store(if pass { 0 } else { 1 }, Ordering::SeqCst);

                if keep_open > 0 {
                    std::thread::sleep(Duration::from_secs(keep_open));
                }
                let _ = window.is_visible();
                handle.exit(if pass { 0 } else { 1 });
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("tauri 应用构建失败");

    app.run(|_app, _event| {});
    std::process::exit(verdict_code.load(Ordering::SeqCst));
}
