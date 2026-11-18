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

/// exe 文件图标：assets/rustcat.ico（PNG-in-ICO）→ .res（RT_ICON + RT_GROUP_ICON +
/// RT_VERSION），经 rustc-link-arg 交给 link.exe 写入 PE 资源段。零外部工具（不需要 rc.exe）。
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
    push_res_entry(&mut res, 0, 0, 0, 0, &[]); // 空根条目
    push_res_entry(&mut res, 3, 1, 0x1010, 0, png); // RT_ICON = PNG 载荷
    let mut group = Vec::with_capacity(20);
    group.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // reserved/type/count
    group.extend_from_slice(&[32, 32, 0, 0]); // 宽/高/色数+保留
    group.extend_from_slice(&1u16.to_le_bytes()); // planes
    group.extend_from_slice(&32u16.to_le_bytes()); // bpp
    group.extend_from_slice(&(png.len() as u32).to_le_bytes()); // bytes in res
    group.extend_from_slice(&1u16.to_le_bytes()); // 引用 RT_ICON id=1
    push_res_entry(&mut res, 14, 1, 0x1030, 0, &group); // RT_GROUP_ICON
    let version = parse_version(&env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION"));
    let info = vs_versioninfo(&version);
    push_res_entry(&mut res, 16, 1, 0x1030, 1, &info); // RT_VERSION（DataVersion=1）
    let path = Path::new(out_dir).join("rustcat.res");
    fs::write(&path, &res).expect("write rustcat.res");
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

/// ———— VS_VERSIONINFO（MSDN VS_VERSIONINFO 布局，纯 Rust 构建，零 rc.exe）————

fn push_wstr(buf: &mut Vec<u8>, s: &str) {
    for u in s.encode_utf16() {
        buf.extend_from_slice(&u.to_le_bytes());
    }
}

fn pad4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

/// 通用节点：wLength（整块，含 6 字节头）/ wValueLength / wType / szKey(+NUL)(+pad) / value / children。
/// 对齐基准含 6 字节头：szKey 后 pad 使 value 落在 4 对齐处；value 必须自身 4 对齐。
/// wValueLength 单独给：字符串值的尾部补零不算长度，否则 Windows 解析器视为非法块。
fn make_block(key: &str, value: &[u8], value_len: u16, wtype: u16, children: &[u8]) -> Vec<u8> {
    let mut sz = Vec::new();
    push_wstr(&mut sz, key);
    sz.extend_from_slice(&0u16.to_le_bytes()); // szKey 必须以 NUL 结尾
    while (6 + sz.len()) % 4 != 0 {
        sz.push(0);
    }
    let total = 6 + sz.len() + value.len() + children.len();
    let mut b = Vec::with_capacity(total);
    b.extend_from_slice(&(total as u16).to_le_bytes());
    b.extend_from_slice(&value_len.to_le_bytes());
    b.extend_from_slice(&wtype.to_le_bytes());
    b.extend_from_slice(&sz);
    b.extend_from_slice(value);
    b.extend_from_slice(children);
    b
}

fn push_child(children: &mut Vec<u8>, child: Vec<u8>) {
    pad4(children);
    children.extend_from_slice(&child);
}

/// 字符串条目：value = UTF-16 + NUL，pad 到 4 对齐（wValueLength 只算到 NUL，不含补零）。
fn make_string(key: &str, val: &str) -> Vec<u8> {
    let mut value = Vec::new();
    push_wstr(&mut value, val);
    value.extend_from_slice(&0u16.to_le_bytes());
    let value_len = value.len() as u16;
    pad4(&mut value);
    make_block(key, &value, value_len, 1, &[])
}

/// "1.2.0" → [1, 2, 0, 0]；缺省段补 0，预发布/构建后缀（-beta.1、+sha）忽略。
fn parse_version(v: &str) -> [u16; 4] {
    let mut out = [0u16; 4];
    for (i, part) in v.split('.').take(4).enumerate() {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        assert!(!digits.is_empty(), "Cargo.toml version {v:?} 第 {i} 段非数字");
        out[i] = digits.parse().unwrap_or_else(|e| panic!("parse version {v:?}: {e}"));
    }
    out
}

/// Cargo.toml 的 version/description/license/name → FileVersion/ProductVersion；VOS_NT_WINDOWS32 + VFT_APP。
fn vs_versioninfo(version: &[u16; 4]) -> Vec<u8> {
    let [major, minor, patch, build] = *version;
    let file_ms = (u32::from(major) << 16) | u32::from(minor);
    let file_ls = (u32::from(patch) << 16) | u32::from(build);
    let words: [u32; 13] = [
        0xFEEF_04BD, // dwSignature
        0x0001_0000, // dwStrucVersion
        file_ms,     // dwFileVersionMS
        file_ls,     // dwFileVersionLS
        file_ms,     // dwProductVersionMS
        file_ls,     // dwProductVersionLS
        0x0000_003F, // dwFileFlagsMask
        0,           // dwFileFlags
        0x0004_0004, // dwFileOS = VOS_NT_WINDOWS32
        1,           // dwFileType = VFT_APP
        0,           // dwFileSubtype
        0,           // dwFileDateMS
        0,           // dwFileDateLS
    ];
    let ffi: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();

    let version_text = format!("{major}.{minor}.{patch}");
    let license = env::var("CARGO_PKG_LICENSE").unwrap_or_default();
    let copyright = if license.is_empty() { String::new() } else { format!("{license} License") };
    let mut strings = Vec::new();
    for (k, v) in [
        ("CompanyName", "31i45".to_string()),
        ("FileDescription", env::var("CARGO_PKG_DESCRIPTION").unwrap_or_default()),
        ("FileVersion", version_text.clone()),
        ("InternalName", env::var("CARGO_PKG_NAME").unwrap_or_default()),
        ("LegalCopyright", copyright),
        ("OriginalFilename", format!("{}.exe", env::var("CARGO_PKG_NAME").unwrap_or_default())),
        ("ProductName", "RustCat".to_string()),
        ("ProductVersion", version_text),
    ] {
        push_child(&mut strings, make_string(k, &v));
    }
    let table = make_block("040904b0", &[], 0, 1, &strings); // US English + Unicode
    let mut sfi = Vec::new();
    push_child(&mut sfi, table);
    let sfi = make_block("StringFileInfo", &[], 0, 1, &sfi);

    let mut translation = 0x0409u16.to_le_bytes().to_vec();
    translation.extend_from_slice(&0x04B0u16.to_le_bytes());
    let mut vfi = Vec::new();
    push_child(&mut vfi, make_block("Translation", &translation, 4, 0, &[]));
    let vfi = make_block("VarFileInfo", &[], 0, 0, &vfi);

    let mut children = Vec::new();
    push_child(&mut children, sfi);
    push_child(&mut children, vfi);
    make_block("VS_VERSION_INFO", &ffi, ffi.len() as u16, 0, &children)
}

/// .res 条目：32 字节头（DataSize/HeaderSize/Type/Name/DataVersion/Flags/Lang/Ver/Char）+ DWORD 对齐数据。
/// data_version：RT_VERSION 按惯例为 1，其余为 0。
fn push_res_entry(res: &mut Vec<u8>, rtype: u16, rid: u16, flags: u16, data_version: u32, data: &[u8]) {
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes()); // header size
    res.extend_from_slice(&0xFFFFu16.to_le_bytes()); // ordinal marker
    res.extend_from_slice(&rtype.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes()); // ordinal marker
    res.extend_from_slice(&rid.to_le_bytes());
    res.extend_from_slice(&data_version.to_le_bytes()); // data version
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
