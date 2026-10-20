//! CPU 计数器回退链，行为对齐原版 CPURepository.cs：
//! 类别 Processor Information 回退 Processor，实例 _Total；
//! build < 26100 先试 % Processor Utility 失败回退 % Processor Time，否则直接 % Processor Time。

use super::{PdhCounter, RollingSampler};
use std::ffi::c_void;
use std::iter::once;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Performance::*;
use windows::Win32::System::Registry::*;

const BUILD_SPLIT: u32 = 26100;
const CATEGORIES: [&str; 2] = ["Processor Information", "Processor"];
const COUNTERS_LEGACY: [&str; 2] = ["% Processor Utility", "% Processor Time"];
const COUNTERS_MODERN: [&str; 1] = ["% Processor Time"];

/// 生产线装配：主循环的唯一入口。
pub fn cpu_sampler() -> RollingSampler<PdhCounter> {
    RollingSampler::new(PdhCounter::open())
}

/// 回退链选择：类别外循环、计数器内循环，try_open 首次成功即定。
/// 决策与 PDH 解耦——注入试探闭包即可离线测试全部回退路径。
pub fn select_path(
    os_build: u32,
    mut try_open: impl FnMut(&str, &str) -> bool,
) -> Option<(&'static str, &'static str)> {
    let counters: &[&str] = if os_build >= BUILD_SPLIT {
        &COUNTERS_MODERN
    } else {
        &COUNTERS_LEGACY
    };
    for &cat in &CATEGORIES {
        for &ctr in counters {
            if try_open(cat, ctr) {
                return Some((cat, ctr));
            }
        }
    }
    None
}

impl PdhCounter {
    /// 按回退链打开 CPU 计数器；任一步失败返回 None。构建号读不出时按现代（≥26100）处理。
    pub fn open() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let mut chosen: Option<PDH_HCOUNTER> = None;
            let found = select_path(os_build().unwrap_or(u32::MAX), |cat, ctr| {
                let path: Vec<u16> =
                    format!("\\{cat}(_Total)\\{ctr}").encode_utf16().chain(once(0)).collect();
                let mut counter = PDH_HCOUNTER::default();
                if PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut counter) == 0
                {
                    chosen = Some(counter);
                    true
                } else {
                    false
                }
            });
            match (found, chosen) {
                (Some(_), Some(counter)) => Some(Self { query, counter }),
                _ => {
                    PdhCloseQuery(query);
                    None
                }
            }
        }
    }
}

/// HKLM\...\CurrentVersion\CurrentBuild（REG_SZ，如 "26100"，定长缓冲足够）。
fn os_build() -> Option<u32> {
    unsafe {
        let mut buf = [0u16; 16];
        let mut size = (buf.len() * 2) as u32;
        if RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuild"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        String::from_utf16_lossy(&buf[..size as usize / 2]).trim_matches(char::from(0)).parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempts(
        build: u32,
        open_ok: impl Fn(&str, &str) -> bool,
    ) -> (Vec<String>, Option<(&'static str, &'static str)>) {
        let mut log = Vec::new();
        let r = select_path(build, |c, k| {
            log.push(format!("{c}|{k}"));
            open_ok(c, k)
        });
        (log, r)
    }

    // —— 回退链 ——
    #[test]
    fn modern_build_uses_processor_time_only() {
        let (log, r) = attempts(26100, |_, _| true);
        assert_eq!(r, Some(("Processor Information", "% Processor Time")));
        assert_eq!(log.len(), 1, "build≥26100 不得尝试 Utility");
    }
    #[test]
    fn legacy_build_prefers_utility() {
        let (_, r) = attempts(19041, |_, _| true);
        assert_eq!(r, Some(("Processor Information", "% Processor Utility")));
    }
    #[test]
    fn legacy_falls_back_to_processor_time() {
        let (_, r) = attempts(19041, |_, k| k != "% Processor Utility");
        assert_eq!(r, Some(("Processor Information", "% Processor Time")));
    }
    #[test]
    fn category_falls_back_to_processor() {
        let (_, r) = attempts(19041, |c, _| c != "Processor Information");
        assert_eq!(r, Some(("Processor", "% Processor Utility")));
    }
    #[test]
    fn all_fail_returns_none() {
        let (log, r) = attempts(19041, |_, _| false);
        assert_eq!(r, None);
        assert_eq!(log.len(), 4, "应穷尽 2 类别 × 2 计数器");
    }

    /// 真机探针：`cargo test -- --ignored --nocapture`，10 秒读数人工比对任务管理器。
    #[test]
    #[ignore = "真机手动验证"]
    fn real_machine_probe() {
        let mut s = cpu_sampler();
        assert!(s.is_available(), "PDH 计数器不可用");
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            s.update();
            println!("CPU load = {:?}", s.load());
        }
    }
}
