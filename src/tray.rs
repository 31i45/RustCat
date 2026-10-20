//! 托盘：Shell_NotifyIconW 生命周期 + tooltip + 单项菜单。薄 FFI，零业务分支。

use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

pub const WM_TRAYICON: u32 = WM_USER + 1;
pub const CMD_EXIT: u16 = 1;
pub const CMD_AUTOSTART: u16 = 2;

pub struct Tray {
    hwnd: HWND,
}

impl Tray {
    /// 托盘图标锚定窗口（浮窗定位与 Shell_NotifyIconGetRect 需要）。
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    }
}

/// szTip 上限 128 码元；截断至 127 保终止符。
fn write_tip(data: &mut NOTIFYICONDATAW, tip: &str) {
    let wide = tip.encode_utf16().take(127).chain(std::iter::once(0));
    for (dst, src) in data.szTip.iter_mut().zip(wide) {
        *dst = src;
    }
}

impl Tray {
    pub fn add(hwnd: HWND, icon: HICON, tip: &str) -> Option<Self> {
        unsafe {
            let mut data = nid(hwnd);
            data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            data.uCallbackMessage = WM_TRAYICON;
            data.hIcon = icon;
            write_tip(&mut data, tip);
            if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                return None;
            }
            // 原版 WinForms 同款：切换到现代消息口径
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
            Some(Self { hwnd })
        }
    }

    pub fn set_icon(&self, icon: HICON) {
        unsafe {
            let mut data = nid(self.hwnd);
            data.uFlags = NIF_ICON;
            data.hIcon = icon;
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    pub fn set_tooltip(&self, tip: &str) {
        unsafe {
            let mut data = nid(self.hwnd);
            data.uFlags = NIF_TIP;
            write_tip(&mut data, tip);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    /// 右键菜单：自启（按当前状态勾选）+ 退出。每次调用重建，天然反映最新状态。
    pub fn show_menu(&self, autostart_on: bool) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let flags = if autostart_on { MF_STRING | MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(menu, flags, CMD_AUTOSTART as usize, w!("自启"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT as usize, w!("退出"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.hwnd); // 焦点修复：否则点菜单外不收起
            let _ = TrackPopupMenu(
                menu,
                TPM_RIGHTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                None,
                self.hwnd,
                None,
            );
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid(self.hwnd));
        }
    }
}
