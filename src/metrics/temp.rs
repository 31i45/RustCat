//! CPU 温度采样：PDH 计数器 `\Thermal Zone Information(*)\Temperature`，
//! 枚举全部热区实例取最大值，再由开尔文换算为摄氏度。
//! 本机实测该计数器单位为开尔文（非十分之一开尔文），实例名形如 `\_TZ.TZ00`；
//! 部分热区不上报时读数可能为 0，按不合理值丢弃。

use super::{CounterSource, RollingSampler};
use std::iter::once;
use std::mem::size_of;
use windows::core::PCWSTR;
use windows::Win32::System::Performance::*;

const COUNTER_PATH: &str = "\\Thermal Zone Information(*)\\Temperature";
/// 开尔文合理区间：约 -23°C 至 177°C，区间外均为未上报的脏读数。
const KELVIN_VALID: (f64, f64) = (250.0, 450.0);

/// 生产线装配：主循环的温度入口。
pub fn temp_sampler() -> RollingSampler<ThermalZoneCounter> {
    RollingSampler::new(ThermalZoneCounter::open())
}

/// 多实例通配计数器：路径含 `*`，读数须走 PdhGetFormattedCounterArrayW。
pub struct ThermalZoneCounter {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl ThermalZoneCounter {
    /// 打开温度计数器；任一步失败都关闭 query 并返回 None，不留句柄。
    pub fn open() -> Option<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let path: Vec<u16> = COUNTER_PATH.encode_utf16().chain(once(0)).collect();
            let mut counter = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut counter) == 0 {
                Some(Self { query, counter })
            } else {
                PdhCloseQuery(query);
                None
            }
        }
    }

    /// 采集一次并返回全部实例的原始开尔文读数（状态码非成功的实例被丢弃）。
    fn read_kelvins(&mut self) -> Option<Vec<f64>> {
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            // 标准两步法：先空缓冲探需字节数（预期 PDH_MORE_DATA），再按需分配。
            let mut buf_size: u32 = 0;
            let mut item_count: u32 = 0;
            let status = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut buf_size,
                &mut item_count,
                None,
            );
            if status != PDH_MORE_DATA && status != 0 {
                return None;
            }
            if buf_size == 0 || item_count == 0 {
                return None;
            }
            // Vec<u8> 对齐为 1，无法直接承载 PDH_FMT_COUNTERVALUE_ITEM_W；
            // 用 u64 缓冲保证 8 字节对齐后再按结构体指针解释。
            let unit = size_of::<u64>();
            let mut buf = vec![0u64; (buf_size as usize + unit - 1) / unit];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut buf_size,
                &mut item_count,
                Some(items),
            ) != 0
            {
                return None;
            }
            // 防御 PDH 回报的条目数超出缓冲容量，避免越界读取。
            let capacity = buf.len() * unit / size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            let count = (item_count as usize).min(capacity);
            let slice = std::slice::from_raw_parts(items, count);
            Some(
                slice
                    .iter()
                    .filter(|item| item.FmtValue.CStatus == 0)
                    .map(|item| item.FmtValue.Anonymous.doubleValue)
                    .collect(),
            )
        }
    }
}

impl CounterSource for ThermalZoneCounter {
    fn next_value(&mut self) -> Option<f64> {
        let kelvins = self.read_kelvins()?;
        max_celsius(kelvins.into_iter())
    }
}

impl Drop for ThermalZoneCounter {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

/// 从各实例的开尔文读数里选出最大值并换算为摄氏度。
/// 非法/不合理的读数（如 0，代表该热区未上报）直接跳过；
/// 结果钳制到 0..=120，全部非法则 None。
fn max_celsius(values: impl Iterator<Item = f64>) -> Option<f64> {
    values
        .filter(|k| (KELVIN_VALID.0..=KELVIN_VALID.1).contains(k))
        .reduce(f64::max)
        .map(|k| (k - 273.15).clamp(0.0, 120.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_none() {
        assert_eq!(max_celsius(std::iter::empty()), None, "无实例时必须返回 None");
    }
    #[test]
    fn invalid_readings_skipped() {
        let v = max_celsius([0.0, f64::NAN, 1000.0, 300.0].into_iter());
        assert!(
            matches!(v, Some(c) if (c - 26.85).abs() < 1e-9),
            "0 / NaN / 超界读数应被跳过，只保留 300K"
        );
    }
    #[test]
    fn multiple_instances_take_max() {
        let v = max_celsius([283.0, 301.0, 295.0].into_iter());
        assert!(
            matches!(v, Some(c) if (c - 27.85).abs() < 1e-9),
            "多实例应取最大值 301K"
        );
    }
    #[test]
    fn kelvin_converts_to_celsius() {
        let v = max_celsius(std::iter::once(301.0)).unwrap();
        assert!((v - 27.85).abs() < 1e-9, "301K 应换算为约 27.85°C，实际 {v}");
    }
    #[test]
    fn above_upper_bound_clamped() {
        let v = max_celsius(std::iter::once(450.0)).unwrap();
        assert_eq!(v, 120.0, "超上限应钳制到 120°C");
    }
    #[test]
    fn all_invalid_returns_none() {
        assert_eq!(
            max_celsius([0.0, -1.0, f64::INFINITY].into_iter()),
            None,
            "全部非法时必须返回 None"
        );
    }

    /// 真机探针：`cargo test temp_probe -- --ignored --nocapture`，
    /// 打印每个热区实例的原始开尔文值与最终摄氏度，人工比对。
    #[test]
    #[ignore = "真机手动验证"]
    fn temp_probe() {
        let mut counter = ThermalZoneCounter::open();
        assert!(counter.is_some(), "PDH 计数器不可用：Thermal Zone Information 不存在");
        let counter = counter.as_mut().unwrap();
        for _ in 0..5 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            match counter.read_kelvins() {
                Some(kelvins) => {
                    let celsius = max_celsius(kelvins.iter().copied());
                    println!("kelvin = {kelvins:?} -> celsius = {celsius:?}");
                }
                None => println!("本次读数失败"),
            }
        }
    }
}
