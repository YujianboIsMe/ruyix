//! 多实例检测。
//!
//! 通过 Windows 命名互斥体判断是否已有其他 darkhorse-code 实例在运行：
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
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS,
    };
    use windows_sys::Win32::System::Threading::CreateMutexW;

    /// 首实例持有的互斥体句柄（进程退出时由系统释放）
    static GUARD: OnceLock<isize> = OnceLock::new();

    // "Local\\" 前缀：互斥体仅在当前登录会话内可见
    let name: Vec<u16> = "Local\\darkhorse-code-instance-mutex"
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

// ============================================
// 测试
// ============================================

#[cfg(all(test, windows))]
mod tests {
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    /// 验证检测所依赖的内核契约：同一进程内第二次创建同名互斥体
    /// （模拟第二实例）时，系统返回 ERROR_ALREADY_EXISTS。
    /// 使用测试专用名，避免与正在运行的实例互相干扰。
    #[test]
    fn duplicate_mutex_reports_already_exists() {
        let name: Vec<u16> = "Local\\darkhorse-code-instance-mutex-test"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let h1 = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        assert!(!h1.is_null(), "首次创建应成功");
        assert_ne!(unsafe { GetLastError() }, ERROR_ALREADY_EXISTS, "首次创建不应冲突");

        let h2 = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        assert!(!h2.is_null(), "已存在时仍应返回句柄");
        assert_eq!(unsafe { GetLastError() }, ERROR_ALREADY_EXISTS, "重复创建应返回 ERROR_ALREADY_EXISTS");

        unsafe {
            CloseHandle(h2);
            CloseHandle(h1);
        }
    }
}
