//! CPU 温度采样：PawnIO 驱动 + 已签名内核模块，按厂商读 MSR / SMN 得到封装温度。
//!
//! 为什么必须经过驱动：Intel DTS 只经 MSR 暴露，AMD Zen 的 `THM_TCON_CUR_TMP` 只经 SMN
//! （PCI 配置空间的间接索引）暴露，二者都要求 Ring 0，Windows 用户态没有受支持的读取途径。
//! 本机实测 ACPI 热区（PDH `\Thermal Zone Information`）是固件桩值：满载 3 分钟、
//! 0.1 K 分辨率下读数一位未动，故不作为数据源。
//!
//! PawnIO 是第三方已签名驱动（LGPL，见 `assets/pawnio/COPYING`），提供受限的内核执行沙箱；
//! 本模块只做一件事：把寄存器地址交给驱动、取回 64 位结果。设备节点仅管理员可打开，
//! 非管理员运行时 `open` 直接失败并整体降级，不抛错、不伪装读数。
//!
//! 覆盖面由 CPUID 厂商分流：Intel 用 `IntelMSR.bin`（`0x1A2` 取 TjMax、`0x1B1` 读封装温度），
//! AMD Zen 用 `AMDFamily17.bin`（家族 17h/19h/1Ah 的 `0x00059800`，解码对齐 Linux `k10temp`）。
//! 更老的 AMD（10h/0Fh）与没有 `0x1B1` 的 Intel 机型安静降级为无温度。

use super::{CounterSource, RollingSampler};

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> isize;
    fn GetProcAddress(module: isize, name: *const u8) -> isize;
    fn FreeLibrary(module: isize) -> i32;
}

type Open = unsafe extern "system" fn(*mut isize) -> i32;
type Load = unsafe extern "system" fn(isize, *const u8, usize) -> i32;
type Execute = unsafe extern "system" fn(
    isize,
    *const u8,
    *const u64,
    usize,
    *mut u64,
    usize,
    *mut usize,
) -> i32;
type Close = unsafe extern "system" fn(isize) -> i32;

/// PawnIO 客户端库，随驱动安装。
const LIB_PATH: &str = "C:\\Program Files\\PawnIO\\PawnIOLib.dll";

/// Intel：`ioctl_read_msr(u64 index) -> u64 value`。
const INTEL_MODULE: &[u8] = include_bytes!("../../assets/pawnio/IntelMSR.bin");
const FN_READ_MSR: &[u8] = b"ioctl_read_msr\0";
/// AMD：`ioctl_read_smn(u64 offset) -> u64 value`。
const AMD_MODULE: &[u8] = include_bytes!("../../assets/pawnio/AMDFamily17.bin");
const FN_READ_SMN: &[u8] = b"ioctl_read_smn\0";

/// IA32_TEMPERATURE_TARGET：bit 16..23 为 TjMax。
const MSR_TEMP_TARGET: u64 = 0x1A2;
/// IA32_PACKAGE_THERM_STATUS：bit 16..22 为距 TjMax 的读数差。
const MSR_PACKAGE_STATUS: u64 = 0x1B1;

/// Zen 家族（17h/19h/1Ah）共用的报告温度寄存器。
const SMN_THM_TCON_CUR_TMP: u64 = 0x0005_9800;
/// CUR_TEMP 起始位：高 11 位，单位 0.125 °C（Linux `k10temp` 口径）。
const ZEN_TEMP_SHIFT: u32 = 21;
/// RANGE_SEL：置位表示读数含 49 °C 表头偏移。
const ZEN_RANGE_SEL: u32 = 1 << 19;
/// TJ_SEL：双位全置时同样表示含 49 °C 偏移（部分新 Zen 用此位代替 RANGE_SEL）。
const ZEN_TJ_SEL: u32 = 3 << 16;

/// 生产线装配：主循环的温度入口。温度不是百分比，钳制上限取 120 °C。
pub fn cpu_temp_sampler() -> RollingSampler<PawnCpu> {
    RollingSampler::capped(PawnCpu::open(), 120.0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Vendor {
    Intel,
    Amd,
}

/// 厂商专属的解码状态；读取通道本身（模块 + 函数名）由 `PawnCpu::read_fn` 承载。
enum Backend {
    Intel { tj_max: i32 },
    Amd { tdie_offset: f64 },
}

pub struct PawnCpu {
    module: isize,
    handle: isize,
    close: Close,
    execute: Execute,
    read_fn: &'static [u8],
    backend: Backend,
}

impl PawnCpu {
    /// 加载 PawnIOLib 并按厂商载入对应模块；任一步失败都释放已取得的资源并返回 None。
    /// 厂商/家族不受支持时在加载 DLL 之前就返回 None。
    pub fn open() -> Option<Self> {
        let vendor = vendor()?;
        // AMD 的 SMN 通道在 15h 起就存在，`ioctl_read_smn` 在非 Zen 上可能"成功"返回
        // 无意义的值（通常是 0）而不是报错——那会被当成 0 °C 的假读数。故按家族门控：
        // 只有 Zen（17h/19h/1Ah）的 0x00059800 才有 THM_TCON_CUR_TMP 语义。
        if vendor == Vendor::Amd && !matches!(cpu_family(), 0x17 | 0x19 | 0x1A) {
            return None;
        }
        unsafe {
            let wide: Vec<u16> = LIB_PATH.encode_utf16().chain(std::iter::once(0)).collect();
            let module = LoadLibraryW(wide.as_ptr());
            if module == 0 {
                return None;
            }
            match Self::bind(module, vendor) {
                Some(cpu) => Some(cpu),
                None => {
                    FreeLibrary(module);
                    None
                }
            }
        }
    }

    unsafe fn bind(module: isize, vendor: Vendor) -> Option<Self> {
        let open: Open = sym(module, b"pawnio_open\0")?;
        let load: Load = sym(module, b"pawnio_load\0")?;
        let execute: Execute = sym(module, b"pawnio_execute\0")?;
        let close: Close = sym(module, b"pawnio_close\0")?;

        let (blob, read_fn) = match vendor {
            Vendor::Intel => (INTEL_MODULE, FN_READ_MSR),
            Vendor::Amd => (AMD_MODULE, FN_READ_SMN),
        };

        let mut handle = 0isize;
        if open(&mut handle) != 0 {
            return None;
        }
        if load(handle, blob.as_ptr(), blob.len()) != 0 {
            close(handle);
            return None;
        }

        let mut cpu = Self {
            module,
            handle,
            close,
            execute,
            read_fn,
            backend: Backend::Amd { tdie_offset: 0.0 }, // 占位，随厂商立即覆盖
        };
        cpu.backend = match vendor {
            Vendor::Intel => {
                let Some(tj_max) = cpu.intel_tj_max() else {
                    close(handle);
                    return None;
                };
                Backend::Intel { tj_max }
            }
            Vendor::Amd => Backend::Amd { tdie_offset: tdie_offset(&cpu_brand()) },
        };
        Some(cpu)
    }

    /// TjMax 由 MSR 给出；6600/14900 这一代通常为 100，超出合理区间说明模块不可信。
    fn intel_tj_max(&mut self) -> Option<i32> {
        let target = self.read(MSR_TEMP_TARGET)?;
        let tj_max = ((target >> 16) & 0xFF) as i32;
        (50..=120).contains(&tj_max).then_some(tj_max)
    }

    /// `ioctl_read_msr` 与 `ioctl_read_smn` 入参出参形状一致：一个 u64 进、一个 u64 出。
    fn read(&mut self, index: u64) -> Option<u64> {
        let input = [index];
        let mut output = [0u64];
        let mut returned = 0usize;
        let status = unsafe {
            (self.execute)(
                self.handle,
                self.read_fn.as_ptr(),
                input.as_ptr(),
                1,
                output.as_mut_ptr(),
                1,
                &mut returned,
            )
        };
        (status == 0 && returned == 1).then_some(output[0])
    }
}

impl CounterSource for PawnCpu {
    fn next_value(&mut self) -> Option<f64> {
        let celsius = match self.backend {
            Backend::Intel { tj_max } => {
                let raw = self.read(MSR_PACKAGE_STATUS)?;
                let delta = ((raw >> 16) & 0x7F) as i32;
                f64::from(tj_max - delta)
            }
            Backend::Amd { tdie_offset } => {
                let raw = self.read(SMN_THM_TCON_CUR_TMP)? as u32;
                zen_celsius(raw) + tdie_offset
            }
        };
        (-20.0..=150.0)
            .contains(&celsius)
            .then_some(celsius.clamp(0.0, 120.0))
    }
}

impl Drop for PawnCpu {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.handle);
            FreeLibrary(self.module);
        }
    }
}

/// CPUID 厂商串（leaf 0 的 EBX/EDX/ECX）。
fn vendor() -> Option<Vendor> {
    let r = cpuid(0);
    let mut raw = [0u8; 12];
    raw[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    raw[4..8].copy_from_slice(&r.edx.to_le_bytes());
    raw[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    match &raw {
        b"GenuineIntel" => Some(Vendor::Intel),
        b"AuthenticAMD" => Some(Vendor::Amd),
        _ => None,
    }
}

/// CPUID leaf 1 的家族号：基家族为 0xF 时取 `基 + 扩展`，否则就是基家族。
fn cpu_family() -> u32 {
    family_from_eax(cpuid(1).eax)
}

/// 家族解码的纯函数部分（CPUID leaf 1 的 EAX）。
fn family_from_eax(eax: u32) -> u32 {
    let base = (eax >> 8) & 0xF;
    if base == 0xF {
        base + ((eax >> 20) & 0xFF)
    } else {
        base
    }
}

/// CPUID 0x80000002..0x80000004 的 48 字节厂牌串（如 "AMD Ryzen 7 1700X ..."）。
fn cpu_brand() -> String {
    if cpuid(0x8000_0000).eax < 0x8000_0004 {
        return String::new();
    }
    let mut raw = Vec::with_capacity(48);
    for leaf in 0x8000_0002..=0x8000_0004 {
        let r = cpuid(leaf);
        for word in [r.eax, r.ebx, r.ecx, r.edx] {
            raw.extend_from_slice(&word.to_le_bytes());
        }
    }
    String::from_utf8_lossy(&raw).trim_end_matches('\0').trim().to_string()
}

fn cpuid(leaf: u32) -> std::arch::x86_64::CpuidResult {
    std::arch::x86_64::__cpuid(leaf)
}

/// Zen `THM_TCON_CUR_TMP` 解码（对齐 Linux `k10temp`）：CUR_TEMP × 0.125 °C，
/// `RANGE_SEL` 或 `TJ_SEL` 双置位时再减 49 °C。
fn zen_celsius(raw: u32) -> f64 {
    let mut temp = f64::from(raw >> ZEN_TEMP_SHIFT) * 0.125;
    if raw & ZEN_RANGE_SEL != 0 || (raw & ZEN_TJ_SEL) == ZEN_TJ_SEL {
        temp -= 49.0;
    }
    temp
}

/// 部分 1000/2000 系 Ryzen 的 Tctl 含固定偏移，Tdie 需扣除；表对齐 `k10temp` 的
/// `tctl_offset_table`。未知型号偏移为 0（即 Tctl 与 Tdie 相同）。
fn tdie_offset(brand: &str) -> f64 {
    if brand.contains("1600X") || brand.contains("1700X") || brand.contains("1800X") {
        -20.0
    } else if brand.contains("Threadripper 19") || brand.contains("Threadripper 29") {
        -27.0
    } else if brand.contains("2700X") {
        -10.0
    } else {
        0.0
    }
}

unsafe fn sym<T: Copy>(module: isize, name: &[u8]) -> Option<T> {
    let addr = GetProcAddress(module, name.as_ptr());
    (addr != 0).then(|| std::mem::transmute_copy(&addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 480 × 0.125 = 60 °C；带偏移的 872 × 0.125 − 49 = 60 °C。
    #[test]
    fn zen_temperature_decode() {
        assert!((zen_celsius(480 << ZEN_TEMP_SHIFT) - 60.0).abs() < 1e-9);
        assert!((zen_celsius(60 << ZEN_TEMP_SHIFT) - 7.5).abs() < 1e-9);
        assert!((zen_celsius((872 << ZEN_TEMP_SHIFT) | ZEN_RANGE_SEL) - 60.0).abs() < 1e-9);
        assert!((zen_celsius((872 << ZEN_TEMP_SHIFT) | ZEN_TJ_SEL) - 60.0).abs() < 1e-9);
        // TJ_SEL 只置一位不算偏移
        assert!((zen_celsius((872 << ZEN_TEMP_SHIFT) | (1 << 16)) - 109.0).abs() < 1e-9);
    }

    #[test]
    fn tdie_offset_table() {
        assert_eq!(tdie_offset("AMD Ryzen 7 1700X Eight-Core Processor"), -20.0);
        assert_eq!(tdie_offset("AMD Ryzen 5 1600X Six-Core Processor"), -20.0);
        assert_eq!(tdie_offset("AMD Ryzen 7 2700X Eight-Core Processor"), -10.0);
        assert_eq!(tdie_offset("AMD Ryzen Threadripper 2990WX 32-Core"), -27.0);
        assert_eq!(tdie_offset("AMD Ryzen 9 5900X 12-Core Processor"), 0.0);
    }

    /// 家族解码：基家族 0xF 时才叠加扩展家族（Zen 的 17h/19h/1Ah 全靠这一条）。
    #[test]
    fn cpu_family_decode() {
        // AMD Zen1 / Zen3 / Zen5 的真实 leaf 1 EAX
        assert_eq!(family_from_eax(0x0080_0F12), 0x17);
        assert_eq!(family_from_eax(0x00A0_0F11), 0x19);
        assert_eq!(family_from_eax(0x00B0_0F21), 0x1A);
        // 基家族非 0xF：K8 的 0Fh、Core 的 6
        assert_eq!(family_from_eax(0x0000_0F00), 0x0F);
        assert_eq!(family_from_eax(0x0003_06A9), 0x06);
    }

    #[test]
    fn cpu_brand_is_trimmed() {
        assert!(!cpu_brand().contains('\0'));
    }

    /// 真机探针：`cargo test cpu_temp_probe -- --ignored --nocapture`
    /// 需要 PawnIO 已安装且进程为管理员。
    #[test]
    #[ignore]
    fn cpu_temp_probe() {
        println!("vendor = {:?}, family = 0x{:X}, brand = {:?}", vendor(), cpu_family(), cpu_brand());
        let Some(mut cpu) = PawnCpu::open() else {
            println!("PawnIO 不可用（未安装或非管理员），本次降级为无 CPU 温度");
            return;
        };
        for _ in 0..3 {
            println!("cpu = {:?} °C", cpu.next_value());
        }
    }

    /// 缺失依赖时必须安静降级，而不是 panic。
    #[test]
    fn open_never_panics() {
        let _ = PawnCpu::open();
    }
}
