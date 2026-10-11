//! 多实例检测。
//!
//! 通过 Windows 命名互斥体判断是否已有其他 ruyix 实例在运行：
//! 首个实例创建互斥体并持有句柄直到进程退出（由操作系统自动释放），
//! 后续实例创建时发现互斥体已存在，即说明已有实例在运行。

use std::sync::OnceLock;

/// 检测结果只计算一次（互斥体句柄需持有到进程退出，不能重复创建）
static OTHER_INSTANCE: OnceLock<bool> = OnceLock::new();

/// 是否有其他实例正在运行
pub fn is_other_instance() -> bool {
    *OTHER_INSTANCE.get_or_init(detect)
}

#[cfg(windows)]
fn detect() -> bool {
    use std::ptr;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;

    /// 首实例持有的互斥体句柄（进程退出时由系统释放）
    static GUARD: OnceLock<isize> = OnceLock::new();

    // 名字里带**根的指纹**：便携形态天生允许多份副本并存，而写死的名字会让两份副本互相
    // 当成"另一个自己"（P0 实测：`--name-mode global` 时第二个 `first_instance=false`）。
    // 同一个根 → 同一个名字（同一份副本的第二次启动仍被识别）；不同根 → 各不相干。
    // "Local\\" 前缀：互斥体仅在当前登录会话内可见。
    let name: Vec<u16> = mutex_name(&crate::paths::current().root_fingerprint())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // 创建失败（极罕见，如权限不足）时保守视为已有实例，避免重复打开项目
    let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return true;
    }

    // ERROR_ALREADY_EXISTS / ERROR_ACCESS_DENIED 均表示互斥体已存在
    let err = unsafe { GetLastError() };
    if err == ERROR_ALREADY_EXISTS || err == ERROR_ACCESS_DENIED {
        unsafe { CloseHandle(handle) };
        true
    } else {
        let _ = GUARD.set(handle as isize);
        false
    }
}

#[cfg(not(windows))]
fn detect() -> bool {
    false
}

/// 互斥体名字：按根指纹区分。
///
/// 抽成纯函数是为了**能单测** —— 名字写错的表现是"两份副本互相以为对方是自己"，
/// 那要跑到真机上、装了两份副本时才会发现（P0 的探针就是这么抓到的）。
#[cfg_attr(not(windows), allow(dead_code))]
fn mutex_name(fingerprint: &str) -> String {
    format!("Local\\ruyix-instance-{fingerprint}")
}

// ============================================
// 测试
// ============================================

#[cfg(all(test, windows))]
mod tests {
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    /// 名字必须**按根区分**：同一个指纹出同一个名字，不同指纹必须不同。
    #[test]
    fn mutex_name_is_scoped_to_the_root() {
        assert_eq!(super::mutex_name("abc123"), super::mutex_name("abc123"));
        assert_ne!(super::mutex_name("abc123"), super::mutex_name("def456"));
        assert!(super::mutex_name("abc123").starts_with("Local\\ruyix-instance-"));
        // 真实指纹形状：16 位 hex（`paths::root_fingerprint`）
        let fp = crate::paths::current().root_fingerprint();
        assert_eq!(fp.len(), 16, "{fp}");
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()), "{fp}");
    }

    /// 验证检测所依赖的内核契约：同一进程内第二次创建同名互斥体
    /// （模拟第二实例）时，系统返回 ERROR_ALREADY_EXISTS。
    /// 使用测试专用名，避免与正在运行的实例互相干扰。
    #[test]
    fn duplicate_mutex_reports_already_exists() {
        let name: Vec<u16> = "Local\\ruyix-instance-mutex-test"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let h1 = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        assert!(!h1.is_null(), "首次创建应成功");
        assert_ne!(
            unsafe { GetLastError() },
            ERROR_ALREADY_EXISTS,
            "首次创建不应冲突"
        );

        let h2 = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        assert!(!h2.is_null(), "已存在时仍应返回句柄");
        assert_eq!(
            unsafe { GetLastError() },
            ERROR_ALREADY_EXISTS,
            "重复创建应返回 ERROR_ALREADY_EXISTS"
        );

        unsafe {
            CloseHandle(h2);
            CloseHandle(h1);
        }
    }
}
