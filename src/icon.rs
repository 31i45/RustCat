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
    use super::*;
    use crate::baked;
    use png::{ColorType, Decoder};
    use std::path::Path;

    /// 真机探针：图标创建全链路（cargo test -- --ignored --nocapture）。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_create_icons() {
        assert!(super::IconSet::create().is_some(), "IconSet::create failed");
    }

    /// ———— 烘焙产物护栏（原 tests/baked_icons.rs，并入 CI 可见的单元测试）————

    fn decode_rgba(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut decoder = Decoder::new(bytes);
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().expect("png info");
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("png frame");
        assert_eq!(info.color_type, ColorType::Rgba);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    /// 与 build.rs 同规则的独立重算：RGBA → BGRA。
    fn to_bgra(rgba: &[u8]) -> Vec<u8> {
        let mut v = rgba.to_vec();
        for px in v.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        v
    }

    #[test]
    fn five_frames_both_themes() {
        assert_eq!(baked::FRAME_COUNT, 5);
        assert_eq!(baked::CAT_ICONS_LIGHT.len(), 5);
        assert_eq!(baked::CAT_ICONS_DARK.len(), 5);
    }

    #[test]
    fn frame_bytes_are_32bpp_raw() {
        let n = (baked::FRAME_WIDTH * baked::FRAME_HEIGHT * 4) as usize;
        for f in baked::CAT_ICONS_LIGHT.iter().chain(baked::CAT_ICONS_DARK.iter()) {
            assert_eq!(f.len(), n, "raw BGRA frame size mismatch");
        }
    }

    #[test]
    fn light_frames_pixel_equal_original_assets() {
        for (i, baked_bgra) in baked::CAT_ICONS_LIGHT.iter().enumerate() {
            let path = Path::new("assets/runners/cat").join(format!("cat_{i}.png"));
            let original = std::fs::read(&path).expect("original png");
            let (ow, oh, rgba) = decode_rgba(&original);
            assert_eq!(
                (ow, oh),
                (baked::FRAME_WIDTH, baked::FRAME_HEIGHT),
                "frame {i} size drifted"
            );
            assert_eq!(&to_bgra(&rgba), baked_bgra, "frame {i} pixels drifted");
        }
    }

    #[test]
    fn dark_frames_whiten_rgb_and_preserve_alpha() {
        for (i, dark) in baked::CAT_ICONS_DARK.iter().enumerate() {
            let light = baked::CAT_ICONS_LIGHT[i];
            assert_eq!(light.len(), dark.len());
            for (l, d) in light.chunks_exact(4).zip(dark.chunks_exact(4)) {
                assert_eq!(&d[0..3], &[255, 255, 255], "frame {i} bgr must be white");
                assert_eq!(l[3], d[3], "frame {i} alpha must be preserved");
            }
        }
    }
}
