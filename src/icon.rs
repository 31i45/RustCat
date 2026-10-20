//! HICON 缓存：CreateIconIndirect 从构建期烘焙的原始 BGRA 像素建图标。
//! 本项目唯一 unsafe 密集区（GDI 句柄创建/销毁），对外只暴露安全 API。
//! 注：CreateIconFromResourceEx 实测拒绝 PNG-in-ICO（M3 探针证据 0x80070006），
//! 原版 .NET Icon 走 GDI+ 内部解码——故用像素路径，视觉结果与原版一致。

use crate::baked::{CAT_ICONS_DARK, CAT_ICONS_LIGHT, FRAME_COUNT, FRAME_HEIGHT, FRAME_WIDTH};
use crate::theme::Theme;
use std::ffi::c_void;
use std::ptr;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct IconSet {
    light: [HICON; FRAME_COUNT],
    dark: [HICON; FRAME_COUNT],
}

/// 32bpp 自顶向下 DIB + 全 0 AND 掩码 → 带 Alpha 的图标（Vista+ 标准配方）。
fn make_icon(bgra: &[u8], width: i32, height: i32) -> Option<HICON> {
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // 负 = 自顶向下，与 PNG 行序一致
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default(); 1],
        };
        let mut bits: *mut c_void = ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .ok()
            .filter(|h| !h.is_invalid());
        let Some(color) = color else { return None };
        if bits.is_null() {
            let _ = DeleteObject(color.into());
            return None;
        }
        ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len());
        let mask = CreateBitmap(width, height, 1, 1, None);
        if mask.is_invalid() {
            let _ = DeleteObject(color.into());
            return None;
        }
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        match icon {
            Ok(h) if !h.is_invalid() => Some(h),
            _ => None,
        }
    }
}

impl IconSet {
    /// 任一帧失败即整体失败并销毁已建图标（启动失败零残留）。
    pub fn create() -> Option<Self> {
        let (w, h) = (FRAME_WIDTH as i32, FRAME_HEIGHT as i32);
        let mut light = [HICON::default(); FRAME_COUNT];
        let mut dark = [HICON::default(); FRAME_COUNT];
        for i in 0..FRAME_COUNT {
            match (make_icon(CAT_ICONS_LIGHT[i], w, h), make_icon(CAT_ICONS_DARK[i], w, h)) {
                (Some(l), Some(d)) => {
                    light[i] = l;
                    dark[i] = d;
                }
                (l, d) => {
                    for icon in [l, d]
                        .into_iter()
                        .chain(light[..i].iter().copied().map(Some))
                        .chain(dark[..i].iter().copied().map(Some))
                        .flatten()
                    {
                        let _ = unsafe { DestroyIcon(icon) };
                    }
                    return None;
                }
            }
        }
        Some(Self { light, dark })
    }

    pub fn frame(&self, theme: Theme, idx: usize) -> HICON {
        let set = match theme {
            Theme::Light => self.light,
            Theme::Dark => self.dark,
        };
        set[idx % FRAME_COUNT]
    }
}

impl Drop for IconSet {
    fn drop(&mut self) {
        for h in self.light.iter().chain(&self.dark) {
            let _ = unsafe { DestroyIcon(*h) };
        }
    }
}

#[cfg(test)]
mod tests {
    /// 真机探针：图标创建全链路（cargo test -- --ignored --nocapture）。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_create_icons() {
        assert!(super::IconSet::create().is_some(), "IconSet::create failed");
    }
}
