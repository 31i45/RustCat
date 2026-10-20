//! 主题：跟随系统（HKCU Personalize\SystemUsesLightTheme）。
//! 变更监听（WM_SETTINGCHANGE）在 main.rs 接线；本模块只负责读。

use std::ffi::c_void;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::*;
use windows::core::w;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Light,
    Dark,
}

/// 读失败兜底 Light（该键在受支持系统上恒存在，失败近乎不可能）。
pub fn system_theme() -> Theme {
    unsafe {
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        ) == ERROR_SUCCESS;
        match (ok, value) {
            (true, 0) => Theme::Dark,
            _ => Theme::Light,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_without_panic() {
        let _ = super::system_theme();
    }
}
