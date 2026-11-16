//! HICON 缓存：CreateIconIndirect 从构建期烘焙的原始 BGRA 像素建图标。
//! 本项目唯一 unsafe 密集区（GDI 句柄创建/销毁），对外只暴露安全 API。
//! 注：CreateIconFromResourceEx 实测拒绝 PNG-in-ICO（M3 探针证据 0x80070006），
//! 原版 .NET Icon 走 GDI+ 内部解码——故用像素路径，视觉结果与原版一致。

use crate::baked::{CAT_ICONS_DARK, CAT_ICONS_LIGHT, FRAME_COUNT, FRAME_HEIGHT, FRAME_WIDTH};
use crate::heat;
use crate::theme::Theme;
use std::ffi::c_void;
use std::ptr;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 主题维：Light / Dark 两个槽。
const THEMES: usize = 2;

/// 一个着色档的整套帧；None = 尚未生成。
type LevelSlot = Option<[HICON; FRAME_COUNT]>;

pub struct IconSet {
    light: [HICON; FRAME_COUNT],
    dark: [HICON; FRAME_COUNT],
    /// 键空间封闭且小 → 定长表：上限由类型保证，不分配、不查找。
    tinted: [[LevelSlot; heat::LEVELS]; THEMES],
}

/// 显式映射，不依赖枚举判别值。
fn theme_index(theme: Theme) -> usize {
    match theme {
        Theme::Light => 0,
        Theme::Dark => 1,
    }
}

/// 只替换 RGB（BGRA 布局：px[0]=B, px[1]=G, px[2]=R），alpha 逐字节保留。
fn recolor(bgra: &[u8], color: [u8; 3]) -> Vec<u8> {
    let [r, g, b] = color;
    let mut out = bgra.to_vec();
    for px in out.chunks_exact_mut(4) {
        px[0] = b;
        px[1] = g;
        px[2] = r;
    }
    out
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
        Some(Self { light, dark, tinted: [[None; heat::LEVELS]; THEMES] })
    }

    /// 取一帧：档位 0 直接复用烘焙图标（零 GDI 创建、像素与改动前一致）；
    /// 档位 1..=5 按 `(theme, level)` 懒生成整套帧并缓存，失败降级回烘焙图标。
    pub fn frame(&mut self, theme: Theme, idx: usize, level: u8) -> HICON {
        let i = idx % FRAME_COUNT;
        let level = (level as usize).min(heat::LEVELS - 1);
        let baked = match theme {
            Theme::Light => self.light[i],
            Theme::Dark => self.dark[i],
        };
        if level == 0 {
            return baked; // 0 档不入表、不建 GDI 对象（保持不变）
        }
        let tix = theme_index(theme);
        if let Some(frames) = &self.tinted[tix][level] {
            return frames[i];
        }
        let Some(frames) = tinted_frames(theme, level as u8) else {
            return baked; // 建图标失败仍降级回烘焙图标
        };
        self.tinted[tix][level] = Some(frames);
        frames[i]
    }
}

/// 用档位色逐帧重建整套图标；任一帧失败则销毁已建图标并返回 None（不留半成品）。
fn tinted_frames(theme: Theme, level: u8) -> Option<[HICON; FRAME_COUNT]> {
    let baked = match theme {
        Theme::Light => &CAT_ICONS_LIGHT,
        Theme::Dark => &CAT_ICONS_DARK,
    };
    let color = heat::tint_color(theme, level);
    let (w, h) = (FRAME_WIDTH as i32, FRAME_HEIGHT as i32);
    let mut frames = [HICON::default(); FRAME_COUNT];
    for (i, src) in baked.iter().enumerate() {
        let px = recolor(src, color);
        match make_icon(&px, w, h) {
            Some(icon) => frames[i] = icon,
            None => {
                for icon in &frames[..i] {
                    let _ = unsafe { DestroyIcon(*icon) };
                }
                return None;
            }
        }
    }
    Some(frames)
}

impl Drop for IconSet {
    fn drop(&mut self) {
        let base = self.light.iter().chain(&self.dark);
        let tinted = self.tinted.iter().flatten().flatten().flatten();
        for h in base.chain(tinted) {
            let _ = unsafe { DestroyIcon(*h) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baked;
    use crate::heat;
    use png::{ColorType, Decoder};
    use std::path::Path;

    /// 真机探针：图标创建全链路（cargo test -- --ignored --nocapture）。
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_create_icons() {
        assert!(super::IconSet::create().is_some(), "IconSet::create failed");
    }

    /// 真机探针：缓存命中、键隔离、0 档不入表（cargo test probe_tint_cache -- --ignored --nocapture）
    #[test]
    #[ignore = "真机手动验证"]
    fn probe_tint_cache() {
        let mut set = super::IconSet::create().expect("IconSet::create failed");

        // 1. 只取 0 档 → 所有槽位仍为 None（0 档不入表、不建 GDI 对象）
        for theme in [Theme::Light, Theme::Dark] {
            for idx in 0..FRAME_COUNT {
                let h = set.frame(theme, idx, 0);
                assert!(!h.is_invalid(), "{theme:?} 0 档帧 {idx} 句柄无效");
            }
        }
        for (tix, row) in set.tinted.iter().enumerate() {
            for (level, slot) in row.iter().enumerate() {
                assert!(slot.is_none(), "主题槽 {tix} 档位 {level} 不应因 0 档取帧而建表");
            }
        }

        // 2. 同键同索引连续两次取帧 → 句柄相等（缓存命中），槽位变 Some
        for theme in [Theme::Light, Theme::Dark] {
            for level in 1..heat::LEVELS as u8 {
                let a = set.frame(theme, 2, level);
                let b = set.frame(theme, 2, level);
                assert_eq!(
                    a.0 as isize, b.0 as isize,
                    "{theme:?} 档位 {level} 同键二次取帧应命中缓存"
                );
                assert!(
                    set.tinted[super::theme_index(theme)][level as usize].is_some(),
                    "{theme:?} 档位 {level} 取帧后槽位应为 Some"
                );
            }
        }

        // 3. 收集所有 (theme, level>=1) 的句柄，两两不相等（键之间不共享句柄）
        let mut handles = Vec::new();
        for theme in [Theme::Light, Theme::Dark] {
            for level in 1..heat::LEVELS as u8 {
                for idx in 0..FRAME_COUNT {
                    let h = set.frame(theme, idx, level);
                    println!("{theme:?} level {level} frame {idx} -> {:#x}", h.0 as isize);
                    handles.push(h.0 as isize);
                }
            }
        }
        for a in 0..handles.len() {
            for b in (a + 1)..handles.len() {
                assert_ne!(handles[a], handles[b], "不同 (theme, level, frame) 不应共享句柄");
            }
        }
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

    // ———— 按温度档位换色 ————

    /// 0 档无感的像素级证据：用 0 档色重着色与烘焙像素逐字节一致。
    #[test]
    fn recolor_with_level_zero_is_byte_identical() {
        for (i, f) in baked::CAT_ICONS_LIGHT.iter().enumerate() {
            let out = recolor(f, [0, 0, 0]);
            assert_eq!(out, *f, "浅色帧 {i} 按 0 档黑色重着色后必须逐字节相同");
        }
        for (i, f) in baked::CAT_ICONS_DARK.iter().enumerate() {
            let out = recolor(f, [255, 255, 255]);
            assert_eq!(out, *f, "深色帧 {i} 按 0 档白色重着色后必须逐字节相同");
        }
    }

    /// 任意档位色：alpha 序列原样保留，RGB 三通道全部等于该色（BGRA 对调）。
    #[test]
    fn recolor_keeps_alpha_and_applies_bgr() {
        let color = [0x7A, 0x1A, 0x12]; // RGB：浅色主题 1 档
        for (i, f) in baked::CAT_ICONS_LIGHT.iter().enumerate() {
            let out = recolor(f, color);
            assert_eq!(out.len(), f.len(), "帧 {i} 重着色不得改变像素数量");
            for (n, (src, dst)) in f.chunks_exact(4).zip(out.chunks_exact(4)).enumerate() {
                assert_eq!(src[3], dst[3], "帧 {i} 第 {n} 像素 alpha 必须逐字节保留");
                assert_eq!(
                    &dst[..3],
                    &[color[2], color[1], color[0]],
                    "帧 {i} 第 {n} 像素应为档位色（BGRA 下 B/G/R 对调）"
                );
            }
        }
    }

    /// 档位 0 的调色板取值与两套烘焙基线一致。
    #[test]
    fn level_zero_tint_matches_baked_baseline() {
        let light = heat::tint_color(Theme::Light, 0);
        let dark = heat::tint_color(Theme::Dark, 0);
        assert_eq!(light, [0, 0, 0], "浅色主题 0 档应为烘焙黑猫的 RGB");
        assert_eq!(dark, [255, 255, 255], "深色主题 0 档应为烘焙白猫的 RGB");
        for (i, f) in baked::CAT_ICONS_DARK.iter().enumerate() {
            assert!(
                f.chunks_exact(4).all(|p| p[..3] == dark),
                "深色帧 {i} 的 RGB 应全等于 0 档色"
            );
        }
    }
}
