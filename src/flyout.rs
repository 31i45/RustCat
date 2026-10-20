//! 悬停浮窗：Win11 原生 flyout 审美（白/近黑圆角卡片 + DWM 投影，无边框线），
//! 黑白灰单色 = 托盘猫视觉语言的延伸。
//! 筛选（设计决策）：CPU / 内存 / 磁盘三项，行序 = 心智优先级；排除 GPU（无可靠
//! 系统级聚合源，假精度）与网络（速率无参照系，一瞥价值最低）。
//! 字号分层：数值 15px Semibold（主视觉）、标签 11px Regular 灰（次级）。
//! 行为：无激活、不进 Alt-Tab、Win11 圆角、250ms 光标哨兵自动隐藏、随采样周期刷新。
//! 前提：进程 Per-Monitor DPI Aware（main 入口声明），坐标与字体全物理口径。

use std::ffi::c_void;
use std::mem::{size_of, size_of_val};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{Shell_NotifyIconGetRect, NOTIFYICONIDENTIFIER};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS_NAME: PCWSTR = w!("RustCatFlyout");
pub const IDT_HOVER: usize = 3; // 挂在主窗上的光标哨兵定时器

// 96 DPI 基准布局常量（运行时按真实 DPI 缩放）。对齐系统原生托盘浮窗口径：
// 紧凑宽度、Regular 字重（层级靠灰度而非字重）、细轨道、适中留白。
const W: i32 = 200;
const H: i32 = 120;
const PAD: i32 = 14;
const ROW_H: i32 = 34;
const BAR_H: i32 = 3;
const VALUE_FONT: i32 = 12;
const VALUE_H: i32 = 15; // 数值行行高
const LABEL_FONT: i32 = 10;
const GAP: i32 = 6; // 数值到条形的间距
const ANCHOR_GAP: i32 = 8; // 浮窗与托盘图标的间距
const ANCHOR_SLACK: i32 = 6; // 图标命中判定放宽（托盘小目标）

const LABELS: [&str; 3] = ["CPU", "内存", "磁盘"];

/// 窗口 GWLP_USERDATA 载荷：绘制所需全部状态。
struct Panel {
    value_font: HFONT,
    label_font: HFONT,
    dark: bool,
    snapshot: [Option<f64>; 3],
    scale: i32,
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.value_font.into());
            let _ = DeleteObject(self.label_font.into());
        }
    }
}

/// 悬停浮窗控制器：持有浮窗 HWND，定时器挂在主窗（与现有消息循环同线程）。
pub struct Flyout {
    parent: HWND,
    tray: HWND,
    hwnd: Option<HWND>,
    dark: bool,
}

impl Flyout {
    pub fn new(parent: HWND, tray: HWND, dark: bool) -> Self {
        Self { parent, tray, hwnd: None, dark }
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        if let Some(h) = self.hwnd {
            unsafe {
                let p = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Panel;
                if !p.is_null() {
                    (*p).dark = dark;
                    let _ = InvalidateRect(Some(h), None, false);
                }
            }
        }
    }

    /// 托盘 mousemove：显示/重新定位浮窗（幂等）。
    /// 光标复核：菜单模态期积压的过期 MOUSEMOVE 在分发时若光标已离开图标则落空，
    /// 杜绝菜单关闭瞬间的幽灵浮窗闪现。决策依据是光标实况，而非消息到达事件。
    pub fn show(&mut self) {
        let Some(anchor) = tray_icon_rect(self.tray) else { return };
        unsafe {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_err() || !in_rect(pt, anchor, ANCHOR_SLACK) {
                return;
            }
        }
        let scale = dpi_scale(self.parent);
        unsafe {
            if self.hwnd.is_none() {
                self.hwnd = self.create(scale);
            }
            let Some(h) = self.hwnd else { return };
            let (w, hgt) = (W * scale / 96, H * scale / 96);
            let (x, y) = position(anchor, w, hgt);
            let _ = SetWindowPos(
                h,
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            SetTimer(Some(self.parent), IDT_HOVER, 250, None);
        }
    }

    pub fn hide(&mut self) {
        if let Some(h) = self.hwnd {
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
        unsafe {
            let _ = KillTimer(Some(self.parent), IDT_HOVER);
        }
    }

    /// 光标哨兵（250ms 周期）：false = 已离开图标∪浮窗，调用方应 hide。
    pub fn tick(&self) -> bool {
        unsafe {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_err() {
                return false;
            }
            if let Some(h) = self.hwnd {
                if IsWindowVisible(h).as_bool() {
                    let mut wr = RECT::default();
                    if GetWindowRect(h, &mut wr).is_ok() && in_rect(pt, wr, 0) {
                        return true;
                    }
                }
            }
            tray_icon_rect(self.tray).is_some_and(|r| in_rect(pt, r, ANCHOR_SLACK))
        }
    }

    /// 采样周期刷新：仅当浮窗可见时更新快照并重绘。
    pub fn refresh(&self, snapshot: [Option<f64>; 3]) {
        let Some(h) = self.hwnd else { return };
        unsafe {
            if !IsWindowVisible(h).as_bool() {
                return;
            }
            let p = GetWindowLongPtrW(h, GWLP_USERDATA) as *mut Panel;
            if !p.is_null() {
                (*p).snapshot = snapshot;
            }
            let _ = InvalidateRect(Some(h), None, false);
        }
    }

    fn create(&self, scale: i32) -> Option<HWND> {
        unsafe {
            let hinstance = HINSTANCE::from(GetModuleHandleW(None).unwrap_or_default());
            let wc = WNDCLASSW {
                style: CS_DROPSHADOW, // 轻投影：参考原生 flyout 的悬浮感
                lpfnWndProc: Some(flyout_wndproc),
                hInstance: hinstance,
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            RegisterClassW(&wc); // 已注册则失败，幂等忽略

            let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_COMPOSITED;
            let (w, hgt) = (W * scale / 96, H * scale / 96);
            let hwnd = CreateWindowExW(
                ex,
                CLASS_NAME,
                w!(""),
                WS_POPUP,
                0,
                0,
                w,
                hgt,
                None,
                None,
                Some(hinstance),
                None,
            )
            .ok()?;
            // Win11 圆角裁切；Win10 无此属性，忽略失败
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &DWMWCP_ROUND as *const _ as *const c_void,
                size_of_val(&DWMWCP_ROUND) as u32,
            );
            let panel = Box::new(Panel {
                value_font: create_font(VALUE_FONT, FW_NORMAL.0 as i32, scale),
                label_font: create_font(LABEL_FONT, FW_NORMAL.0 as i32, scale),
                dark: self.dark,
                snapshot: [None; 3],
                scale,
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(panel) as isize);
            Some(hwnd)
        }
    }
}

impl Drop for Flyout {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.parent), IDT_HOVER);
            if let Some(h) = self.hwnd {
                let raw = SetWindowLongPtrW(h, GWLP_USERDATA, 0) as *mut Panel;
                if !raw.is_null() {
                    drop(Box::from_raw(raw));
                }
                let _ = DestroyWindow(h);
            }
        }
    }
}

unsafe extern "system" fn flyout_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Panel;
        if !p.is_null() {
            paint(hwnd, &mut *p);
        }
        LRESULT(0)
    } else {
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

/// 双缓冲自绘：卡片背景 + 三行（灰标签 | 大粗数值 | 全宽条形）。无边框，DWM 圆角裁切。
unsafe fn paint(hwnd: HWND, p: &mut Panel) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);

    let mem = CreateCompatibleDC(Some(hdc));
    let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
    let old_bmp = SelectObject(mem, bmp.into());
    let old_font = SelectObject(mem, p.value_font.into());
    let _ = SetBkMode(mem, TRANSPARENT);

    let (bg, fg, label, track) = palette(p.dark);
    fill(mem, rc, bg);

    let (w, _) = (W * p.scale / 96, H * p.scale / 96);
    let pad = PAD * p.scale / 96;
    let row_h = ROW_H * p.scale / 96;
    let bar_h = BAR_H * p.scale / 96;
    let vh = VALUE_H * p.scale / 96;
    let gap = GAP * p.scale / 96;
    let inner = w - pad * 2;

    for (i, name) in LABELS.iter().enumerate() {
        let top = pad + (i as i32) * row_h;
        // 标签：左，小灰字（次级信息）
        let _ = SelectObject(mem, p.label_font.into());
        let _ = SetTextColor(mem, label);
        text(mem, name, pad, top);
        // 数值：右，大粗字（主视觉）
        let _ = SelectObject(mem, p.value_font.into());
        let _ = SetTextColor(mem, fg);
        let v = p.snapshot[i];
        let _ = SetTextAlign(mem, TA_RIGHT);
        let s = v.map_or("—".to_string(), |x| format!("{x:.0}%"));
        text(mem, &s, w - pad, top);
        let _ = SetTextAlign(mem, TA_LEFT);
        // 条形：全宽轨道 + 单色值，与猫同色语言
        let tr = RECT {
            left: pad,
            top: top + vh + gap,
            right: pad + inner,
            bottom: top + vh + gap + bar_h,
        };
        fill(mem, tr, track);
        if let Some(v) = v {
            let bw = (f64::from(inner) * v.clamp(0.0, 100.0) / 100.0) as i32;
            if bw > 0 {
                fill(mem, RECT { right: pad + bw, ..tr }, fg);
            }
        }
    }

    let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
    let _ = SelectObject(mem, old_bmp);
    let _ = SelectObject(mem, old_font);
    let _ = DeleteObject(bmp.into());
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

fn rgb(v: u32) -> COLORREF {
    COLORREF((v << 16) | (v << 8) | v)
}

/// (背景, 数值, 标签, 轨道)——黑白灰，主题反转。底色取系统 popover 口径
/// （浅色 #F9F9F9 非刺眼纯白；深色 #2C2C2C），层级靠灰度靠拢原生观感。
fn palette(dark: bool) -> (COLORREF, COLORREF, COLORREF, COLORREF) {
    if dark {
        (rgb(0x2C), rgb(0xF0), rgb(0xA0), rgb(0x3C))
    } else {
        (rgb(0xF9), rgb(0x1B), rgb(0x86), rgb(0xE6))
    }
}

unsafe fn fill(hdc: HDC, r: RECT, c: COLORREF) {
    let b = CreateSolidBrush(c);
    let _ = FillRect(hdc, &r, b);
    let _ = DeleteObject(b.into());
}

unsafe fn text(hdc: HDC, s: &str, x: i32, y: i32) {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let _ = TextOutW(hdc, x, y, &wide);
}

fn create_font(height: i32, weight: i32, scale: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -height * scale / 96,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Segoe UI"),
        )
    }
}

fn dpi_scale(hwnd: HWND) -> i32 {
    unsafe { GetDpiForWindow(hwnd).max(96) as i32 }
}

fn tray_icon_rect(tray: HWND) -> Option<RECT> {
    unsafe {
        let id = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: tray,
            uID: 1,
            ..Default::default()
        };
        Shell_NotifyIconGetRect(&id).ok()
    }
}

/// 图标上方居中，钳制到最近显示器工作区；顶部放不下则翻到图标下方。
/// DPI Aware 后：anchor（Explorer 物理像素）与 SetWindowPos（本进程物理像素）同口径。
fn position(anchor: RECT, w: i32, h: i32) -> (i32, i32) {
    let cx = (anchor.left + anchor.right) / 2;
    let mut x = cx - w / 2;
    let mut y = anchor.top - h - ANCHOR_GAP;
    unsafe {
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mon = MonitorFromPoint(POINT { x: cx, y: anchor.top }, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            let wa = info.rcWork;
            x = x.clamp(wa.left, wa.right - w);
            if y < wa.top {
                y = anchor.bottom + ANCHOR_GAP;
            }
            y = y.clamp(wa.top, wa.bottom - h);
        }
    }
    (x, y)
}

fn in_rect(pt: POINT, r: RECT, slack: i32) -> bool {
    pt.x >= r.left - slack
        && pt.x <= r.right + slack
        && pt.y >= r.top - slack
        && pt.y <= r.bottom + slack
}
