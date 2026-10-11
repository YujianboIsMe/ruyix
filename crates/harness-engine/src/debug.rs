//! 详细调试日志 —— `--debug` 启动开关的引擎侧落点。
//!
//! ## 为什么要落盘，而不是只打 stdout
//!
//! 正式版是 Windows GUI 子系统（`src-tauri/src/main.rs` 首行
//! `cfg_attr(not(debug_assertions), windows_subsystem = "windows")`）—— **没有控制台**，
//! `println!` 的输出会被丢掉。所以"详细日志"必须有自己的落点文件，否则用户拿到的
//! 是"开了开关但什么都没有"。
//!
//! ## 三条纪律
//!
//! 1. **关闭时是纯 no-op**：默认关闭，正常运行的输出、开销、行为一字不变
//!    —— 详细日志写在主流程上，不能让它变成"给所有人加成本"。
//! 2. **落盘前一律脱敏**：这里写的是**完整提示词与原始响应体**，是全项目最容易把
//!    密钥带出去的地方。复用 `observe::redact` + 当前 secrets，脱敏在写入层做，
//!    不靠调用方自觉。
//! 3. **写失败绝不影响主流程**：观测宁可丢一行日志，也不能卡住被测代码
//!    （与 `observe::Tracer::append` 同一取舍）。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// 单条详细日志的字符上限。超限**留痕**（与 `observe::cap` 同一纪律：
/// 静默截断会把"没看到"伪装成"不存在"，正是本项目踩过的坑）。
pub const DEBUG_CAP: usize = 200_000;

/// 开关。默认关。
static ENABLED: AtomicBool = AtomicBool::new(false);

static LOG_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn path_slot() -> &'static Mutex<Option<PathBuf>> {
    LOG_PATH.get_or_init(|| Mutex::new(None))
}

/// 中毒的锁意味着"持锁时有人 panic 了"。观测应**继续可用**，不该 `unwrap` 再 panic 一次，
/// 那会顺着调用栈把被观测的流程一起带走。
fn with_path<R>(f: impl FnOnce(&mut Option<PathBuf>) -> R) -> R {
    match path_slot().lock() {
        Ok(mut g) => f(&mut g),
        Err(p) => f(&mut p.into_inner()),
    }
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// 指定落盘文件。没设路径时 `note` / `mirror` 只打印，不落盘。
pub fn set_path(p: PathBuf) {
    with_path(|g| *g = Some(p));
}

pub fn path() -> Option<PathBuf> {
    with_path(|g| g.clone())
}

/// 追加一段文本到调试文件。**刻意不加锁**：与 `observe` 同一取舍 ——
/// 观测不该成为"阻塞主流程"的风险点，宁可丢一行。
fn emit_text(text: &str) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        let _ = writeln!(f, "{text}");
    }
}

fn prepare(body: &str) -> String {
    crate::observe::cap(
        &crate::observe::redact(body, &crate::observe::secrets()),
        DEBUG_CAP,
    )
}

/// 详细日志：**落盘 + 尽力打印**。关闭时是 no-op。
///
/// 打印走 `writeln!` 并忽略错误，而不是 `println!` —— 正式版没有控制台，
/// `println!` 写不出来会 panic，那等于"开了调试开关把程序搞崩"。
pub fn note(body: &str) {
    if !enabled() {
        return;
    }
    let text = prepare(body);
    emit_text(&text);
    let _ = writeln!(std::io::stdout(), "{text}");
}

/// 只落盘、不再打印：给**已经被 `println!` 打过一遍**的行用（避免 dev 下重复）。
pub fn mirror(line: &str) {
    if !enabled() {
        return;
    }
    emit_text(&prepare(line));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全局单例的测试必须串行（`cargo test` 多线程跑，会互相覆盖开关与路径）。
    static LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 返回一个**目录尚不存在**的路径 —— 建目录这件事本身也要被测到。
    fn tmp_path(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ruyix-dbg-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        d.join("debug.log")
    }

    fn cleanup(p: &std::path::Path) {
        if let Some(d) = p.parent() {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn off_is_a_pure_noop() {
        let _g = lock();
        let p = tmp_path("off");
        set_path(p.clone());
        set_enabled(false);
        note("不该出现");
        mirror("也不该出现");
        assert!(!p.exists(), "关闭时不许建文件：{}", p.display());
    }

    #[test]
    fn on_writes_to_the_file_and_creates_parent_dirs() {
        let _g = lock();
        let p = tmp_path("on");
        assert!(!p.parent().unwrap().exists(), "前置条件：目录本来不存在");
        set_enabled(true);
        set_path(p.clone());
        note("MARKER-NOTE");
        mirror("MARKER-MIRROR");
        set_enabled(false);

        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("MARKER-NOTE"), "{t}");
        assert!(t.contains("MARKER-MIRROR"), "{t}");
        cleanup(&p);
    }

    #[test]
    fn secrets_never_reach_the_debug_file() {
        // 这里写的是完整提示词与原始响应体 —— 全项目最容易漏密钥的地方，脱敏必须兑现。
        // 用 `mirror`（只落盘）：测试里没必要把假密钥打到终端。
        let _g = lock();
        let p = tmp_path("secret");
        set_enabled(true);
        set_path(p.clone());
        mirror("Authorization: Bearer sk-abcdefghijklmnopqrstuvwxyz");
        set_enabled(false);

        let t = std::fs::read_to_string(&p).unwrap();
        assert!(!t.contains("abcdefghijklmnop"), "密钥原文进了文件：{t}");
        assert!(t.contains("[REDACTED]"), "{t}");
        cleanup(&p);
    }

    #[test]
    fn oversized_bodies_leave_a_trace_instead_of_silent_truncation() {
        // 同上用 `mirror`：200KB 的用例不该灌进测试输出
        let _g = lock();
        let p = tmp_path("cap");
        set_enabled(true);
        set_path(p.clone());
        mirror(&"x".repeat(DEBUG_CAP + 10));
        set_enabled(false);

        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("[截断"), "超限必须留痕：{}", &t[..120]);
        cleanup(&p);
    }

    #[test]
    fn enabled_without_a_path_does_not_panic() {
        let _g = lock();
        set_enabled(true);
        with_path(|g| *g = None);
        note("没有落点也不该崩");
        mirror("同上");
        set_enabled(false);
        assert!(path().is_none());
    }

    /// 端到端：**真的**走一次 `llm::chat`（打到一个本地假端点，走完整 HTTP 往返），
    /// 证明详细日志的三段埋点真会产出内容 —— 而不是"代码里写了三行 note，一行也没跑到"。
    ///
    /// 放在本模块而不是 `llm.rs`，是为了共用这把锁：开关与路径是**进程级全局**的，
    /// 只有同模块串行，别的测试才不会把它偷走（否则这条会随机变红）。
    #[test]
    fn a_real_chat_call_lands_in_the_debug_file() {
        let _g = lock();
        let p = tmp_path("e2e");
        set_enabled(true);
        set_path(p.clone());

        let llm = crate::testllm::fake_llm_raw(vec![r#"{"tool":"final"}"#.into()]);
        let cfg = crate::config::LlmConfig {
            base_url: llm.base_url.clone(),
            api_key: "smoke".into(),
            model: "fake-model".into(),
            ..Default::default()
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let out = rt
            .block_on(crate::llm::chat(
                &cfg,
                None,
                &[crate::llm::ChatMessage::user("用户的问题原文")],
                true,
                false,
            ))
            .expect("假 LLM 该回话");
        assert_eq!(out.content, r#"{"tool":"final"}"#);

        set_enabled(false);
        let t = std::fs::read_to_string(&p).unwrap();
        // 三段齐 —— 缺任何一段，"模型这一轮为什么跑偏"就有一半看不到
        assert!(t.contains("===== LLM 请求 ====="), "{t}");
        assert!(t.contains("===== LLM 原始响应体 ====="), "{t}");
        assert!(t.contains("===== LLM 解析结果 ====="), "{t}");
        // 请求段：端点与提示词都算"模型实际看到什么"的一部分
        assert!(t.contains(&llm.base_url), "端点没记下来：{t}");
        assert!(t.contains("用户的问题原文"), "提示词没记下来：{t}");
        // 原始响应体段：要的是**信封原文**，不是只有提取后的正文
        assert!(
            t.contains("choices"),
            "原始响应体（OpenAI 信封）没落盘：{t}"
        );
        // 解析段：finish_reason 与提取后的 content
        assert!(t.contains("finish_reason"), "{t}");
        assert!(t.contains(r#"{"tool":"final"}"#), "{t}");
        cleanup(&p);
    }
}
