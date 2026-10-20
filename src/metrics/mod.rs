//! 性能指标域：通用采样件（CounterSource 接缝 / 滑动窗口采样器 / PDH 薄封装）
//! + 按指标分文件（cpu / disk / memory），命名即领域。
//! 采样语义对齐原版 CPURepository.cs：构造预热、读数钳制、5 点滑动平均。

mod cpu;
mod disk;
mod memory;

pub use cpu::cpu_sampler;
pub use disk::{disk_activity, disk_sampler};
pub use memory::memory_pct;

use crate::speed::RollingAverage;

/// 测试接缝：None = 本次读数失败（计数器消失/查询失败）。
pub trait CounterSource {
    fn next_value(&mut self) -> Option<f64>;
}

/// 滑动窗口采样器：预热、钳制、5 点平均。source = None 即不可用（原版 IsAvailable=false）。
pub struct RollingSampler<S: CounterSource> {
    source: Option<S>,
    window: RollingAverage,
}

impl<S: CounterSource> RollingSampler<S> {
    /// PDH 首次读数恒为 0，构造即丢弃（原版构造预热语义）。
    pub fn new(mut source: Option<S>) -> Self {
        if let Some(s) = &mut source {
            s.next_value();
        }
        Self { source, window: RollingAverage::default() }
    }

    #[allow(dead_code)] // 探针与单测使用；主循环只消费 load()
    pub fn is_available(&self) -> bool {
        self.source.is_some()
    }

    pub fn update(&mut self) {
        if let Some(v) = self.source.as_mut().and_then(CounterSource::next_value) {
            self.window.push(v.clamp(0.0, 100.0));
        }
    }

    /// 尚无样本时为 None（启动第一个采样周期内）。
    pub fn load(&self) -> Option<f64> {
        self.window.average()
    }
}

// ———— PDH 薄 FFI：cpu / disk 共用的通用计数器封装 ————

use std::iter::once;
use windows::core::PCWSTR;
use windows::Win32::System::Performance::*;

pub struct PdhCounter {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl PdhCounter {
    /// 通用单路径构造：英文计数器全名（如 \PhysicalDisk(_Total)\% Idle Time）。
    /// 固定路径指标（磁盘等）用此入口；CPU 的回退链见 metrics::cpu。
    pub fn from_path(counter_path: &str) -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let path: Vec<u16> = counter_path.encode_utf16().chain(once(0)).collect();
            let mut counter = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut counter) == 0 {
                Some(Self { query, counter })
            } else {
                PdhCloseQuery(query);
                None
            }
        }
    }
}

impl CounterSource for PdhCounter {
    fn next_value(&mut self) -> Option<f64> {
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let mut value = PDH_FMT_COUNTERVALUE::default();
            if PdhGetFormattedCounterValue(self.counter, PDH_FMT_DOUBLE, None, &mut value) != 0
            {
                return None;
            }
            // 原版 NextValue 在坏状态上抛异常；此处等价地丢弃本次读数。
            (value.CStatus == 0).then_some(value.Anonymous.doubleValue)
        }
    }
}

impl Drop for PdhCounter {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    pub(crate) struct Mock {
        values: VecDeque<Option<f64>>,
    }
    impl Mock {
        pub(crate) fn new(values: &[Option<f64>]) -> Self {
            Self { values: values.iter().copied().collect() }
        }
    }
    impl CounterSource for Mock {
        fn next_value(&mut self) -> Option<f64> {
            self.values.pop_front().flatten()
        }
    }

    // —— 滑动窗口采样器 ——
    #[test]
    fn construction_primes_first_reading() {
        let mut s = RollingSampler::new(Some(Mock::new(&[Some(0.0), Some(42.0)])));
        s.update();
        assert_eq!(s.load(), Some(42.0), "预热读数不得进入窗口");
    }
    #[test]
    fn readings_clamped_to_hundred() {
        let mut s = RollingSampler::new(Some(Mock::new(&[Some(0.0), Some(250.0)])));
        s.update();
        assert_eq!(s.load(), Some(100.0));
    }
    #[test]
    fn window_slides_over_five() {
        let mut s = RollingSampler::new(Some(Mock::new(&[
            Some(0.0),
            Some(1.0),
            Some(2.0),
            Some(3.0),
            Some(4.0),
            Some(5.0),
            Some(6.0),
        ])));
        for _ in 0..6 {
            s.update();
        }
        assert_eq!(s.load(), Some(4.0));
    }
    #[test]
    fn failed_reading_is_skipped() {
        let mut s =
            RollingSampler::new(Some(Mock::new(&[Some(0.0), Some(10.0), None, Some(30.0)])));
        for _ in 0..3 {
            s.update();
        }
        assert_eq!(s.load(), Some(20.0), "失败读数不进窗口也不移位");
    }
    #[test]
    fn unavailable_source_stays_empty() {
        let mut s: RollingSampler<Mock> = RollingSampler::new(None);
        assert!(!s.is_available());
        s.update();
        assert_eq!(s.load(), None);
    }
}
