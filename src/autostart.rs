//! 开机自启：HKCU Run 键，REG_SZ = 当前 exe 路径。切换即生效，注册表即持久化，
//! 无设置文件、无轮询。启动时零成本（状态仅菜单显示时按需读取一次）。

use std::os::windows::ffi::OsStrExt;
use windows::core::{PCWSTR, w};
use windows::Win32::System::Registry::*;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("RustCat");

/// 当前 exe 全路径（UTF-16 + NUL 终止）。主进程内调用即主程序自身路径。
pub fn exe_path_utf16() -> Vec<u16> {
    std::env::current_exe()
        .expect("current_exe")
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn is_enabled() -> bool {
    unsafe {
        let mut buf = [0u16; 512];
        let mut len = (buf.len() * 2) as u32;
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .0
            == 0 // ERROR_SUCCESS
    }
}

pub fn set(enabled: bool) {
    unsafe {
        if enabled {
            let path = exe_path_utf16();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE_NAME,
                REG_SZ.0, // 函数签名取 u32，非 REG_VALUE_TYPE
                Some(path.as_ptr().cast()),
                (path.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME); // 值不存在即已达成目标状态
        }
    }
}

/// 切换并返回切换后的状态。
pub fn toggle() -> bool {
    let now = !is_enabled();
    set(now);
    now
}

#[cfg(test)]
mod tests {
    #[test]
    fn exe_path_is_nul_terminated_and_absolute() {
        let p = super::exe_path_utf16();
        assert!(!p.is_empty());
        assert_eq!(*p.last().expect("non-empty"), 0);
        let s = std::env::current_exe().expect("current_exe");
        assert!(s.is_absolute());
    }
}
