//! 入口：单实例 → 隐藏窗 → 双定时器消息循环（产品名 RustCat）。
//! 定时器分工（对齐原版双定时器模型，采样间隔重算为 ADR-001 有意识偏离）：
//!   IDT_SAMPLE  1000ms 固定：PDH 采样 → tooltip → 重算动画间隔
//!   IDT_ANIMATE 动态：推进一帧托盘图标（起步 200ms，原版默认档）

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod autostart;
mod flyout;
mod icon;
mod metrics;
mod speed;
mod theme;
mod tray;

#[allow(dead_code)] // FRAME_WIDTH/HEIGHT 供断言与调试，运行时不消费
mod baked {
    include!(concat!(env!("OUT_DIR"), "/baked_icons.rs"));
}

use flyout::{Flyout, IDT_HOVER};
use icon::IconSet;
use metrics::{cpu_sampler, disk_sampler, disk_activity, memory_pct, PdhCounter, RollingSampler};
use speed::interval_ms;
use theme::{Theme, system_theme};
use tray::{CMD_AUTOSTART, CMD_EXIT, Tray, WM_TRAYICON};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const IDT_SAMPLE: usize = 1;
const IDT_ANIMATE: usize = 2;

/// 主窗口类名（探针共用单一来源，类名改动即全处一致）。
pub const MAIN_CLASS: PCWSTR = w!("RustCatCoreWnd");

struct App {
    sampler: RollingSampler<PdhCounter>,
    disk: RollingSampler<PdhCounter>,
    tray: Tray,
    icons: IconSet,
    theme: Theme,
    frame: usize,
    flyout: Flyout,
    autostart: bool,
}

/// 常驻双行 tooltip：CPU 一行 + 自启状态一行。状态与右键菜单永远一致，
/// 左键 toggle 的反馈直接落在第二行，无"1s 后消失"的突兀。
fn tooltip_text(load: Option<f64>, autostart_on: bool) -> String {
    let cpu = match load {
        Some(v) => format!("CPU: {v:.1}%"),
        None => "CPU: n/a".to_string(),
    };
    format!("{cpu}\n自启: {}", if autostart_on { "开" } else { "关" })
}

fn next_frame(i: usize) -> usize {
    (i + 1) % baked::FRAME_COUNT
}

/// 浮窗快照：CPU / 内存 / 磁盘，行序 = 心智优先级。数据源见 metrics 模块。
fn flyout_snapshot(cpu: Option<f64>, disk_idle: Option<f64>) -> [Option<f64>; 3] {
    [cpu, memory_pct().map(f64::from), disk_activity(disk_idle)]
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
                app.disk.update();
                let load = app.sampler.load();
                app.tray.set_tooltip(&tooltip_text(load, app.autostart));
                app.flyout.refresh(flyout_snapshot(load, app.disk.load()));
                SetTimer(Some(hwnd), IDT_ANIMATE, load.map_or(500, |v| interval_ms(v) as u32), None);
            } else if wp.0 == IDT_ANIMATE {
                app.frame = next_frame(app.frame);
                app.tray.set_icon(app.icons.frame(app.theme, app.frame));
            } else if wp.0 == IDT_HOVER {
                if !app.flyout.tick() {
                    app.flyout.hide();
                }
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            if theme_broadcast(lp) {
                app.theme = system_theme();
                app.tray.set_icon(app.icons.frame(app.theme, app.frame));
                app.flyout.set_dark(app.theme == Theme::Dark);
            }
            LRESULT(0)
        }
        WM_TRAYICON => match lp.0 as u16 {
            // VERSION_4 口径：lParam 低字 = 通知码，wParam 低/高字 = 屏幕坐标
            m if m == WM_RBUTTONUP as u16 || m == WM_CONTEXTMENU as u16 => {
                // 弹层互斥：菜单与浮窗同占图标上方，先收浮窗再开菜单
                app.flyout.hide();
                app.tray.show_menu(app.autostart);
                LRESULT(0)
            }
            m if m == WM_LBUTTONUP as u16 => {
                // 点击 = 意图从浏览切到操作，hover UI 即收（对齐原生托盘惯例）
                app.flyout.hide();
                app.autostart = autostart::toggle();
                app.tray.set_tooltip(&tooltip_text(app.sampler.load(), app.autostart));
                LRESULT(0)
            }
            m if m == WM_MOUSEMOVE as u16 => {
                app.flyout.show();
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
                app.autostart = autostart::toggle();
                app.tray.set_tooltip(&tooltip_text(app.sampler.load(), app.autostart));
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
        // Per-Monitor DPI Aware：托盘 rect / GetCursorPos / 窗口坐标 / GDI 字体全部
        // 进入同一物理坐标系。缺失它 = 浮窗错位 + 真实悬停被哨兵误杀 + 字体模糊。
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // 单实例：互斥体名为 RustCat 产品身份（原版 _RUNCAT_MUTEX，机制等价：唯一常量串即单实例）
        let Ok(_mutex) = CreateMutexW(None, true, w!("_RUSTCAT_MUTEX")) else { return };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        let hinstance = HINSTANCE::from(GetModuleHandleW(None).unwrap_or_default());
        let class_name = MAIN_CLASS;
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
        let tray_hwnd = tray.hwnd(); // struct 字面量先 move tray，先取锚窗
        let app = Box::new(App {
            sampler: cpu_sampler(),
            disk: disk_sampler(),
            tray,
            icons,
            theme,
            frame: 0,
            flyout: Flyout::new(hwnd, tray_hwnd, theme == Theme::Dark),
            autostart: autostart::is_enabled(),
        });
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
    fn tooltip_two_lines() {
        assert_eq!(super::tooltip_text(Some(42.04), true), "CPU: 42.0%\n自启: 开");
        assert_eq!(super::tooltip_text(None, false), "CPU: n/a\n自启: 关");
    }
    #[test]
    fn frame_cycles_modulo_count() {
        assert_eq!(super::next_frame(4), 0);
        assert_eq!(super::next_frame(0), 1);
    }

    /// 真机探针：读取运行中实例的托盘图标屏幕矩形（需先启动 runcat.exe）。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_tray_rect() {
        use super::*;
        use windows::Win32::UI::Shell::*;
        unsafe {
            let hwnd = FindWindowW(MAIN_CLASS, None).expect("runcat 未运行");
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
            let hwnd = FindWindowW(MAIN_CLASS, None).expect("runcat 未运行");
            let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(1), LPARAM(0));
        }
    }

    /// 真机探针：模拟左键点击两次（toggle→还原），验证注册表 Run 键随消息切换。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_autostart() {
        use super::*;
        unsafe {
            let hwnd = FindWindowW(MAIN_CLASS, None).expect("rustcat 未运行");
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

    /// 真机探针：光标悬停图标 → 浮窗可见 → 光标移远 → 哨兵隐藏 → 复原退出。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_flyout() {
        use super::*;
        use windows::Win32::UI::Shell::*;
        unsafe {
            // 测试进程同样声明 DPI Aware：否则 GetRect/GetWindowRect 返回虚拟化坐标，
            // 测量失真（物理 360 宽被读成 240、中心 2129 被读成 1419）。
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let hwnd = FindWindowW(MAIN_CLASS, None).expect("rustcat 未运行");
            let id = NOTIFYICONIDENTIFIER {
                cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
                hWnd: hwnd,
                uID: 1,
                ..Default::default()
            };
            let rect = Shell_NotifyIconGetRect(&id).ok();
            if let Some(r) = rect {
                println!("ANCHOR=({},{},{},{})", r.left, r.top, r.right, r.bottom);
            }
            let anchor = rect.expect("GetRect");
            let mut orig = POINT::default();
            let _ = GetCursorPos(&mut orig);
            // show() 现在有光标实况守卫：必须先把真实光标驻留到图标中心，再发消息
            let _ = SetCursorPos((anchor.left + anchor.right) / 2, (anchor.top + anchor.bottom) / 2);
            let _ = PostMessageW(Some(hwnd), WM_TRAYICON, WPARAM(0), LPARAM(WM_MOUSEMOVE as isize));
            std::thread::sleep(std::time::Duration::from_millis(60)); // 等主进程分发 show()
            let fly = FindWindowW(w!("RustCatFlyout"), None).expect("浮窗未创建");
            // 把光标驻留到浮窗自身中心保活（>250ms 后哨兵以浮窗 rect 判定存续）
            let mut wr = RECT::default();
            let _ = GetWindowRect(fly, &mut wr);
            println!("FLYOUT_RECT=({},{},{},{})", wr.left, wr.top, wr.right, wr.bottom);
            let _ = SetCursorPos((wr.left + wr.right) / 2, (wr.top + wr.bottom) / 2);
            std::thread::sleep(std::time::Duration::from_millis(400));
            let visible = IsWindowVisible(fly).as_bool();
            println!("FLYOUT_VISIBLE={visible}");
            assert!(visible, "mousemove 后浮窗应可见");
            let _ = SetCursorPos(10, 10);
            std::thread::sleep(std::time::Duration::from_millis(700)); // > 250ms 哨兵周期
            let hidden = !IsWindowVisible(fly).as_bool();
            println!("FLYOUT_HIDDEN={hidden}");
            assert!(hidden, "光标离开后浮窗应隐藏");
            let _ = SetCursorPos(orig.x, orig.y); // 复原用户光标
            let _ = PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(1), LPARAM(0));
        }
    }

    /// 真机探针：验证 DPI 感知声明是否生效（决定浮窗坐标系是否统一）。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_dpi() {
        use windows::Win32::UI::HiDpi::*;
        unsafe {
            let r = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            println!("SETCTX={r:?}");
            println!("SYS_DPI={}", GetDpiForSystem());
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
            let tray_hwnd = tray.hwnd();
            let app = Box::new(App {
                sampler: cpu_sampler(),
                disk: RollingSampler::new(None),
                tray,
                icons,
                theme,
                frame: 0,
                flyout: Flyout::new(hwnd, tray_hwnd, false),
                autostart: false,
            });
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
