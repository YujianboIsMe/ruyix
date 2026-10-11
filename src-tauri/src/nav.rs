//! 外链与导航闸门（P0）—— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第二刀）。
//!
//! 判据在 [`nav_verdict`]（纯函数，`src-tauri/src/tests.rs` 钉着：localhost 不是自家页面、
//! 同源非文档路径拒掉、`about:blank` 放行），出口在 [`open_external`]（三种 scheme 白名单 +
//! 系统默认处理程序，**不走 shell**；`file:` / `javascript:` / `data:` 拦在能执行之前）。
//!
//! `main.rs` 只留两处接线：`build_main_window` 的 `on_navigation` / `on_new_window`
//! （真闸门 —— 窗口必须建在 Rust 里才挂得上）与 invoke_handler 里的 `nav::open_external`。
//!
//! 下面是拆出来时**原封不动**的那段说明（这条 P0 的"为什么"）：

// ============================================
// 外链：WebView 只是我们的画布，不是浏览器（P0）
// ============================================
//
// 症状：agent 回一句「服务已起，访问 http://localhost:8080」，点一下链接，**整块 IDE 被那张网页替换**
// —— 标签栏、文件树、会话全没了，而且回不去。
//
// 为什么一次点击能拆掉整个 IDE：聊天里的 agent 输出走 markdown-it 且开了 `linkify`，裸 URL 会变成
// 真 `<a href>`；而 WebView2 对普通导航的默认动作就是**在本 WebView 里导航过去**。我们的前端全活在
// **一个文档**里（标签页只是 DOM 状态），文档一换，状态跟着一起没。
// （`target="_blank"` 反而不会：wry 没设新窗口处理器时直接把请求吞掉 —— 所以真凶只有一种：普通导航。
//  加上 `on_new_window` 之后连这一种也不再"点了没反应"。）
//
// 这道闸门必须在**后端**：`on_navigation` 是 WebView 的导航闸门，任何来源（a 标签 / location.href /
// form / window.open / 以后某个忘了拦的角落）都得过它。前端的拦截（`ui/external.js`）是**显式**的
// 那一重 —— 在按下那一刻就把意图变成"交给系统浏览器"，连一次失败的导航都不发生。
// 两重都要有：只靠前端＝下一处漏网就是又一次 P0；只靠后端＝点了没反应（浏览器不开）。
//
// 唯一的判据是"这条 URL 是不是应用自己的文档"，结局只有三种：放行 / 交给系统浏览器 / 拒掉。

/// 一次导航的三种结局。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Nav {
    /// 应用自己的文档 —— 放行
    Allow,
    /// 外站 —— 拒掉导航，改用系统浏览器打开
    External,
    /// 既不是自家文档、也没有可交给浏览器的地方（同源的非文档路径，如 `tauri.localhost/src/main.rs`）
    /// —— 拒掉。这种链接点下去只会换来一张 404 白页，和跳去外站一样丢状态
    Refuse,
}

/// 应用自己的站点（自定义协议）在各平台的落地：Windows / Android 上是 `http://tauri.localhost`，
/// 其它平台是 `tauri://localhost`。
fn is_app_host(url: &tauri::Url) -> bool {
    match url.scheme() {
        "tauri" => true,
        "http" | "https" => url.host_str() == Some("tauri.localhost"),
        _ => false,
    }
}

/// 两个 URL 是不是同一个站点（scheme + host + port）。
/// 不能用 `Url::origin()`：`tauri://` 这类非特殊 scheme 的 origin 是 opaque，序列化出来是 `null`。
fn same_site(a: &tauri::Url, b: &tauri::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// 这条 URL 是不是"一个文档"（`/` 或 `*.html`）。
/// 只看文档：子资源（css/js）不走导航事件，而同源的 `src/main.rs` 这种相对链接一旦导航过去就是白页。
fn is_document_path(url: &tauri::Url) -> bool {
    let path = url.path();
    path.is_empty() || path == "/" || path.ends_with(".html")
}

/// 导航闸门（纯函数，可测）。
///
/// `dev_url` = `tauri.conf.json` 的 `build.devUrl`（本项目没配）。留着这条是因为 `tauri dev` 一旦改用
/// 开发服务器，判据必须认它 —— 认的依据是**配置里写的那一条**，而不是"凡是 localhost 都放行"：
/// agent 起的服务就在 localhost 上，那正是这次要拦的东西。
pub(crate) fn nav_verdict(url: &tauri::Url, dev_url: Option<&tauri::Url>) -> Nav {
    // WebView 给 iframe 用的空文档，不是"去了别的站点"
    if url.as_str() == "about:blank" {
        return Nav::Allow;
    }
    let own_site = is_app_host(url) || dev_url.is_some_and(|dev| same_site(url, dev));
    if !own_site {
        return Nav::External;
    }
    if is_document_path(url) {
        Nav::Allow
    } else {
        Nav::Refuse
    }
}

/// 外链白名单：只放行 http / https / mailto。
///
/// 这是**能执行之前**的闸门，不是措辞洁癖：`file:` 会让一句"打开 file:///C:/…"变成在系统里点开本地
/// 文件，`javascript:` / `data:text/html` 更是直接执行代码。链接可能来自模型输出、用户手输或页面上
/// 的任意文本，白名单之外一律不交给系统。
pub(crate) fn check_open_url(raw: &str) -> Result<String, String> {
    let url = tauri::Url::parse(raw.trim()).map_err(|e| format!("不是合法的链接：{e}"))?;
    match url.scheme() {
        "http" | "https" | "mailto" => Ok(url.to_string()),
        other => Err(format!(
            "只允许 http / https / mailto 链接，这条是 `{other}:`，已拦下"
        )),
    }
}

/// 用操作系统的默认处理程序打开链接 —— 外链**唯一**的出口。
#[tauri::command]
pub fn open_external(url: String) -> Result<String, String> {
    let url = check_open_url(&url)?;
    launch_in_browser(&url)?;
    Ok(url)
}

/// 交给操作系统。Windows 走 `ShellExecuteW`（"用默认处理程序打开"就是它），**不走 shell** ——
/// URL 里带 `&` / 空格 / 中文是常态，`cmd /C start` 那套引号规则本项目已经踩过一次。
#[cfg(windows)]
pub(crate) fn launch_in_browser(url: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let hinstance = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    // 返回值**不是**句柄：<= 32 全是错误码（微软文档写明了），所以"成功"的判据是 > 32 而不是"非零"。
    let code = hinstance as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!(
            "系统没有能打开它的程序（ShellExecuteW 返回 {code}）"
        ))
    }
}

#[cfg(not(windows))]
pub(crate) fn launch_in_browser(url: &str) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("系统没有能打开它的程序（{opener}: {e}）"))
}
