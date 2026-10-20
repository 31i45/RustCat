//! 磁盘活动率：% Idle Time 反演（100 − idle）。
//! % Disk Time 在 SSD/NCQL 上会虚高 >100%，Idle 反演是诚实口径；
//! 滑动平均为线性运算，100−mean(x) ≡ mean(100−x)，故复用通用采样器后再反演。

use super::{PdhCounter, RollingSampler};

const DISK_IDLE_PATH: &str = r"\PhysicalDisk(_Total)\% Idle Time";

pub fn disk_sampler() -> RollingSampler<PdhCounter> {
    RollingSampler::new(PdhCounter::from_path(DISK_IDLE_PATH))
}

pub fn disk_activity(idle_load: Option<f64>) -> Option<f64> {
    idle_load.map(|v| 100.0 - v)
}

#[cfg(test)]
mod tests {
    #[test]
    fn inverts_idle() {
        assert_eq!(super::disk_activity(Some(3.0)), Some(97.0));
        assert_eq!(super::disk_activity(None), None);
    }
}
