//! 速度域纯函数：5 点滑动窗口 + 速度公式。
//! 与原版行为等价（证据：Program.cs CalculateInterval / CPURepository 的窗口）：
//!   speed    = max(1.0, load / 5.0)   （固定 40fps 档，rate = 1）
//!   interval = (int)(500.0 / speed)   （毫秒，向零取整）

/// 原版 CPU_INFO_LIST_LIMIT_SIZE = 5。
const WINDOW: usize = 5;

/// 5 点滑动平均。平均值与写入顺序无关，环形写入即可，免移动、免堆分配。
#[derive(Default)]
pub struct RollingAverage {
    buf: [f64; WINDOW],
    len: usize,
    head: usize,
}

impl RollingAverage {
    pub fn push(&mut self, v: f64) {
        self.buf[self.head] = v;
        self.head = (self.head + 1) % WINDOW;
        self.len = (self.len + 1).min(WINDOW);
    }

    pub fn average(&self) -> Option<f64> {
        // 无样本即 None（诚实空语义）；满窗后全缓冲有效（平均值与顺序无关）。
        (self.len > 0).then(|| self.buf[..self.len].iter().sum::<f64>() / self.len as f64)
    }
}

/// 负载(0–100) → 帧间隔(ms)。钳制在接缝处兜底：正常路径上游已 min(100, v)。
pub fn interval_ms(load: f64) -> u64 {
    let speed = (load.clamp(0.0, 100.0) / 5.0).max(1.0);
    (500.0 / speed) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // —— 速度公式（对照 plan §6 数值表）——
    #[test]
    fn zero_load_is_slowest() {
        assert_eq!(interval_ms(0.0), 500);
    }
    #[test]
    fn below_speed_floor_stays_slowest() {
        assert_eq!(interval_ms(1.0), 500);
    }
    #[test]
    fn ten_percent() {
        assert_eq!(interval_ms(10.0), 250);
    }
    #[test]
    fn fifty_percent() {
        assert_eq!(interval_ms(50.0), 50);
    }
    #[test]
    fn near_full_truncates_toward_zero() {
        assert_eq!(interval_ms(99.9), 25);
    }
    #[test]
    fn full_load_is_fastest() {
        assert_eq!(interval_ms(100.0), 25);
    }
    #[test]
    fn over_hundred_is_clamped() {
        assert_eq!(interval_ms(150.0), 25);
    }
    #[test]
    fn negative_is_clamped() {
        assert_eq!(interval_ms(-3.0), 500);
    }
    #[test]
    fn nan_degrades_to_slowest() {
        assert_eq!(interval_ms(f64::NAN), 500);
    }

    // —— 5 点滑动窗口 ——
    #[test]
    fn empty_window_has_no_average() {
        assert_eq!(RollingAverage::default().average(), None);
    }
    #[test]
    fn average_of_partial_window() {
        let mut w = RollingAverage::default();
        w.push(10.0);
        w.push(20.0);
        assert_eq!(w.average(), Some(15.0));
    }
    #[test]
    fn average_of_full_window() {
        let mut w = RollingAverage::default();
        for v in [1.0, 2.0, 3.0, 4.0, 5.0] {
            w.push(v);
        }
        assert_eq!(w.average(), Some(3.0));
    }
    #[test]
    fn sixth_sample_evicts_oldest() {
        let mut w = RollingAverage::default();
        for v in [1.0, 2.0, 3.0, 4.0, 5.0, 6.0] {
            w.push(v);
        }
        assert_eq!(w.average(), Some(4.0));
    }

    // —— 组合语义：窗口平均喂给公式 ——
    #[test]
    fn window_feeds_interval() {
        let mut w = RollingAverage::default();
        for v in [0.0, 0.0, 0.0, 0.0, 100.0] {
            w.push(v);
        }
        assert_eq!(interval_ms(w.average().unwrap()), 125);
    }
}
