//! 构建期图标烘焙：5 张猫帧 PNG → 亮/暗两套原始 BGRA 字节（32bpp 自顶向下 DIB 序）。
//! 运行时零解码、零像素循环，icon.rs 的 CreateIconIndirect 直接消费。失败即构建失败。
//! 注：原为 PNG-in-ICO 方案，M3 探针发现 CreateIconFromResourceEx 拒绝该格式（E_HANDLE），
//! 而原版 .NET Icon 走 GDI+ 内部解码——故改为像素路径，视觉结果与原版一致。

use png::{BitDepth, ColorType, Decoder};
use std::path::Path;
use std::{env, fs};

const FRAMES: usize = 5;

fn decode_png_rgba(path: &Path) -> (u32, u32, Vec<u8>) {
    let file = fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let mut decoder = Decoder::new(file);
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().expect("png info");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    assert_eq!(info.bit_depth, BitDepth::Eight);
    assert_eq!(info.color_type, ColorType::Rgba);
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

fn decode_png_rgba_from_bytes(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut decoder = Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().expect("png info");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

/// RGBA → BGRA（Win32 32bpp DIB 内存序）。
fn to_bgra(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
}

/// 原版 BitmapExtension.Recolor 语义：RGB 全替换为白，Alpha 原样保留。
fn recolor_white(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        p[0] = 255;
        p[1] = 255;
        p[2] = 255;
    }
}

/// exe 文件图标：assets/rustcat.ico（PNG-in-ICO）→ .res（RT_ICON + RT_GROUP_ICON），
/// 经 rustc-link-arg 交给 link.exe 写入 PE 资源段。零外部工具（不需要 rc.exe）。
fn gen_exe_icon_res(out_dir: &str) {
    println!("cargo:rerun-if-changed=assets/rustcat.ico");
    let ico = fs::read("assets/rustcat.ico").expect("assets/rustcat.ico");
    assert_eq!(&ico[0..6], &[0, 0, 1, 0, 1, 0], "ICO dir: reserved/type/count");
    assert_eq!(&ico[6..8], &[32, 32], "ICO must be 32x32");
    let png_len = u32::from_le_bytes(ico[14..18].try_into().unwrap()) as usize;
    assert_eq!(22 + png_len, ico.len(), "ICO payload size");
    // 漂移护栏：ico 载荷像素必须与 cat_0.png 一致
    let (_, _, rgba_ico) = decode_png_rgba_from_bytes(&ico[22..]);
    let (_, _, rgba0) = decode_png_rgba(&Path::new("assets/runners/cat").join("cat_0.png"));
    assert_eq!(rgba_ico, rgba0, "rustcat.ico 载荷漂移：必须与 cat_0.png 像素一致");

    let png = &ico[22..];
    let mut res = Vec::new();
    push_res_entry(&mut res, 0, 0, 0, &[]); // 空根条目
    push_res_entry(&mut res, 3, 1, 0x1010, png); // RT_ICON = PNG 载荷
    let mut group = Vec::with_capacity(20);
    group.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // reserved/type/count
    group.extend_from_slice(&[32, 32, 0, 0]); // 宽/高/色数+保留
    group.extend_from_slice(&1u16.to_le_bytes()); // planes
    group.extend_from_slice(&32u16.to_le_bytes()); // bpp
    group.extend_from_slice(&(png.len() as u32).to_le_bytes()); // bytes in res
    group.extend_from_slice(&1u16.to_le_bytes()); // 引用 RT_ICON id=1
    push_res_entry(&mut res, 14, 1, 0x1030, &group); // RT_GROUP_ICON
    let path = Path::new(out_dir).join("rustcat.res");
    fs::write(&path, &res).expect("write rustcat.res");
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

/// .res 条目：32 字节头（DataSize/HeaderSize/Type/Name/DataVersion/Flags/Lang/Ver/Char）+ DWORD 对齐数据。
fn push_res_entry(res: &mut Vec<u8>, rtype: u16, rid: u16, flags: u16, data: &[u8]) {
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes()); // header size
    res.extend_from_slice(&0xFFFFu16.to_le_bytes()); // ordinal marker
    res.extend_from_slice(&rtype.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes()); // ordinal marker
    res.extend_from_slice(&rid.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // data version
    res.extend_from_slice(&flags.to_le_bytes()); // memory flags
    res.extend_from_slice(&0x0409u16.to_le_bytes()); // language
    res.extend_from_slice(&0u32.to_le_bytes()); // version
    res.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    res.extend_from_slice(data);
    while res.len() % 4 != 0 {
        res.push(0);
    }
}

fn main() {
    println!("cargo:rerun-if-changed=assets/runners/cat");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let mut code = String::from("pub const FRAME_COUNT: usize = 5;\n");
    let mut light_entries = String::new();
    let mut dark_entries = String::new();
    let mut frame_size = (0u32, 0u32);

    for i in 0..FRAMES {
        let path = Path::new("assets/runners/cat").join(format!("cat_{i}.png"));
        let (w, h, rgba) = decode_png_rgba(&path);
        if i == 0 {
            frame_size = (w, h);
        }
        assert_eq!((w, h), frame_size, "all frames must share one size");
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        // 亮套：原图换序。暗套：白化重着色（Alpha 保留）后换序。
        let mut light = rgba;
        to_bgra(&mut light);
        let mut dark = light.clone();
        recolor_white(&mut dark); // BGR 同置 255，与换序先后无关
        // 暗套不变量：BGR 全白、A 与亮套逐像素一致（构建期即断言）
        for (l, d) in light.chunks_exact(4).zip(dark.chunks_exact(4)) {
            assert_eq!(&d[0..3], &[255, 255, 255], "dark recolor must whiten bgr");
            assert_eq!(l[3], d[3], "dark recolor must preserve alpha");
        }

        code.push_str(&format!("pub static CAT_LIGHT_{i}: &[u8] = &{light:?};\n"));
        code.push_str(&format!("pub static CAT_DARK_{i}: &[u8] = &{dark:?};\n"));
        light_entries.push_str(&format!("&CAT_LIGHT_{i},"));
        dark_entries.push_str(&format!("&CAT_DARK_{i},"));
    }

    code.push_str(&format!("pub const FRAME_WIDTH: u32 = {};\n", frame_size.0));
    code.push_str(&format!("pub const FRAME_HEIGHT: u32 = {};\n", frame_size.1));
    code.push_str(&format!(
        "pub static CAT_ICONS_LIGHT: [&[u8]; FRAME_COUNT] = [{light_entries}];\n"
    ));
    code.push_str(&format!(
        "pub static CAT_ICONS_DARK: [&[u8]; FRAME_COUNT] = [{dark_entries}];\n"
    ));
    fs::write(Path::new(&out_dir).join("baked_icons.rs"), code).expect("write baked_icons.rs");
    gen_exe_icon_res(&out_dir);
}
