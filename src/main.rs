//! 入口：单实例 → 隐藏窗 → 双定时器消息循环（产品名 RustCat）。
//! 定时器分工（对齐原版双定时器模型，采样间隔重算为 ADR-001 有意识偏离）：
//!   IDT_SAMPLE  1000ms 固定：PDH 采样 → tooltip → 重算动画间隔
//!   IDT_ANIMATE 动态：推进一帧托盘图标（起步 200ms，原版默认档）

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod autostart;
mod cpu;
mod icon;
mod speed;
mod theme;
mod tray;

#[allow(dead_code)] // FRAME_WIDTH/HEIGHT 供断言与调试，运行时不消费
mod baked {
    include!(concat!(env!("OUT_DIR"), "/baked_icons.rs"));
}

use cpu::{CpuSampler, PdhCounter, system_cpu_sampler};
use icon::IconSet;
use speed::interval_ms;
use theme::{Theme, system_theme};
use tray::{CMD_AUTOSTART, CMD_EXIT, Tray, WM_TRAYICON};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const IDT_SAMPLE: usize = 1;
const IDT_ANIMATE: usize = 2;

struct App {
    sampler: CpuSampler<PdhCounter>,
    tray: Tray,
    icons: IconSet,
    theme: Theme,
    frame: usize,
}

fn tooltip_text(load: Option<f64>) -> String {
    match load {
        Some(v) => format!("CPU: {v:.1}%"),
        None => "CPU: n/a".to_string(),
    }
}

fn next_frame(i: usize) -> usize {
    (i + 1) % baked::FRAME_COUNT
}

/// 左键切换自启后的即时反馈；1s 内被 CPU 采样 tooltip 覆盖，短反馈即可。
fn autostart_tip(on: bool) -> String {
    if on { "RustCat: autostart ON" } else { "RustCat: autostart OFF" }.to_string()
}

/// 主题变更广播：lParam 为空（全量广播）或 "ImmersiveColorSet" 时响应。
fn theme_broadcast(lp: LPARAM) -> bool {
    if lp.0 == 0 {
        return true;
    }
    unsafe { PCWSTR(lp.0 as *const u16).to_string() }.unwrap_or_default() == "ImmersiveColorSet"
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if app.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let app = &mut *app;
    match msg {
        WM_TIMER => {
            if wp.0 == IDT_SAMPLE {
                app.sampler.update();
                let load = app.sampler.load();
                app.tray.set_tooltip(&tooltip_text(load));
                SetTimer(Some(hwnd), IDT_ANIMATE, load.map_or(500, |v| interval_ms(v) as u32), None);
            } else if wp.0 == IDT_ANIMATE {
                app.frame = next_frame(app.frame);
                app.tray.set_icon(app.icons.frame(app.theme, app.frame));
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            if theme_broadcast(lp) {
                app.theme = system_theme();
                app.tray.set_icon(app.icons.frame(app.theme, app.frame));
            }
            LRESULT(0)
        }
        WM_TRAYICON => match lp.0 as u16 {
            // VERSION_4 口径：lParam 低字 = 通知码，wParam 低/高字 = 屏幕坐标
            m if m == WM_RBUTTONUP as u16 || m == WM_CONTEXTMENU as u16 => {
                app.tray.show_menu(autostart::is_enabled());
                LRESULT(0)
            }
            m if m == WM_LBUTTONUP as u16 => {
                app.tray.set_tooltip(&autostart_tip(autostart::toggle()));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        },
        WM_COMMAND => match (wp.0 & 0xFFFF) as u16 {
            CMD_EXIT => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            CMD_AUTOSTART => {
                app.tray.set_tooltip(&autostart_tip(autostart::toggle()));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        },
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe {
        // 单实例：互斥体名为 RustCat 产品身份（原版 _RUNCAT_MUTEX，机制等价：唯一常量串即单实例）
        let Ok(_mutex) = CreateMutexW(None, true, w!("_RUSTCAT_MUTEX")) else { return };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        let hinstance = HINSTANCE::from(GetModuleHandleW(None).unwrap_or_default());
        let class_name = w!("RustCatCoreWnd");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return;
        }
        // 隐藏顶层窗：消息-only 窗收不到 WM_SETTINGCHANGE 广播，故用不 Show 的普通窗。
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("RustCat"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        ) else {
            return;
        };

        let Some(icons) = IconSet::create() else { return };
        let theme = system_theme();
        let Some(tray) = Tray::add(hwnd, icons.frame(theme, 0), "RustCat") else { return };
        let app = Box::new(App { sampler: system_cpu_sampler(), tray, icons, theme, frame: 0 });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as isize);
        SetTimer(Some(hwnd), IDT_SAMPLE, 1000, None);
        SetTimer(Some(hwnd), IDT_ANIMATE, 200, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 退出零残留：定时器、托盘图标（Tray::drop → NIM_DELETE）、HICON、PDH、互斥体
        let _ = KillTimer(Some(hwnd), IDT_SAMPLE);
        let _ = KillTimer(Some(hwnd), IDT_ANIMATE);
        let raw = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut App;
        if !raw.is_null() {
            drop(Box::from_raw(raw));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tooltip_with_load() {
        assert_eq!(super::tooltip_text(Some(42.04)), "CPU: 42.0%");
    }
    #[test]
    fn tooltip_without_load() {
        assert_eq!(super::tooltip_text(None), "CPU: n/a");
    }
    #[test]
    fn frame_cycles_modulo_count() {
        assert_eq!(super::next_frame(4), 0);
        assert_eq!(super::next_frame(0), 1);
    }
    #[test]
    fn autostart_tip_reflects_state() {
        assert_eq!(super::autostart_tip(true), "RustCat: autostart ON");
        assert_eq!(super::autostart_tip(false), "RustCat: autostart OFF");
    }

    /// 真机探针：读取运行中实例的托盘图标屏幕矩形（需先启动 runcat.exe）。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_tray_rect() {
        use super::*;
        use windows::Win32::UI::Shell::*;
        unsafe {
            let hwnd = FindWindowW(w!("RustCatCoreWnd"), None).expect("runcat 未运行");
            let id = NOTIFYICONIDENTIFIER {
                cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
                hWnd: hwnd,
                uID: 1,
                ..Default::default()
            };
            match Shell_NotifyIconGetRect(&id) {
                Ok(r) => println!(
                    "RECT={}x{}@{},{}",
                    r.right - r.left,
                    r.bottom - r.top,
                    r.left,
                    r.top
                ),
                Err(e) => println!("RECTERR={e:?}"),
            }
        }
    }

    /// 真机探针：逐段验证初始化链（cargo test -- --ignored --nocapture）。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_init_stages() {
        use super::*;
        let icons = IconSet::create();
        println!("stage icons: {}", icons.is_some());
        unsafe {
            let hinstance = HINSTANCE::from(GetModuleHandleW(None).unwrap_or_default());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                lpszClassName: w!("RustCatProbeWnd"),
                ..Default::default()
            };
            let atom = RegisterClassW(&wc);
            println!("stage register: atom={atom}");
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("RustCatProbeWnd"),
                w!("probe"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            );
            println!("stage window: ok={}", hwnd.is_ok());
            if let (Ok(hwnd), Some(ic)) = (hwnd, &icons) {
                let tray = Tray::add(hwnd, ic.frame(system_theme(), 0), "probe");
                println!("stage tray: {}", tray.is_some());
                drop(tray);
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    /// 真机探针：向运行中实例发送 Exit 命令，验证真实退出路径。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_exit() {
        use super::*;
        unsafe {
            let hwnd = FindWindowW(w!("RustCatCoreWnd"), None).expect("runcat 未运行");
            let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(1), LPARAM(0));
        }
    }

    /// 真机探针：模拟左键点击两次（toggle→还原），验证注册表 Run 键随消息切换。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_autostart() {
        use super::*;
        unsafe {
            let hwnd = FindWindowW(w!("RustCatCoreWnd"), None).expect("rustcat 未运行");
            let before = autostart::is_enabled();
            // VERSION_4 口径：lParam 低字 = WM_LBUTTONUP；左键 toggle
            let left = |h: HWND| {
                let _ = PostMessageW(Some(h), WM_TRAYICON, WPARAM(0), LPARAM(WM_LBUTTONUP as isize));
            };
            left(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let after = autostart::is_enabled();
            println!("AUTOSTART {before} -> {after}");
            assert_ne!(before, after, "toggle 未生效");
            left(hwnd);
            std::thread::sleep(std::time::Duration::from_millis(300));
            println!("RESTORED={}", autostart::is_enabled() == before);
            assert_eq!(autostart::is_enabled(), before, "还原失败");
            let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(1), LPARAM(0));
        }
    }

    /// 真机探针：release 优化配置下验证定时器动画推进（cargo test --release -- --ignored）。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_timer_pump() {
        use super::*;
        unsafe {
            let hinstance = HINSTANCE::from(GetModuleHandleW(None).unwrap_or_default());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                lpszClassName: w!("RustCatPumpProbe"),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("RustCatPumpProbe"),
                w!("pump"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
            .expect("probe window");
            let icons = IconSet::create().expect("icons");
            let theme = system_theme();
            let tray = Tray::add(hwnd, icons.frame(theme, 0), "probe").expect("tray");
            let app = Box::new(App { sampler: system_cpu_sampler(), tray, icons, theme, frame: 0 });
            let raw = Box::into_raw(app);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
            SetTimer(Some(hwnd), IDT_SAMPLE, 1000, None);
            SetTimer(Some(hwnd), IDT_ANIMATE, 200, None);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2600);
            let mut msg = MSG::default();
            while std::time::Instant::now() < deadline {
                if GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let frames = (*raw).frame;
            println!("TICKS_FRAME={frames}");
            let _ = DestroyWindow(hwnd);
            drop(Box::from_raw(raw));
            assert!(frames > 0, "动画帧 2.6s 内未推进（WM_TIMER 未生效）");
        }
    }
}
