//! 温度 → 红度档位：纯函数映射 + 双主题 6 色调色板。
//! 设计要点（spec「主题与温度的叠加」）：主题与温度正交——主题决定查哪张表，
//! 温度只决定索引；**不做基色到红色的 RGB 线性插值**，直接查表。
//! 档位区间（L>=1）：`[60 + (L-1)*6, 60 + L*6)`，档位 0 为 `(-inf, 60)`：
//!   0: (-inf, 60)  1: [60, 66)  2: [66, 72)  3: [72, 78)  4: [78, 84)  5: [84, +inf)
//! 迟滞 2°C：升档需 `temp >= 上边界 + 2`，降档需 `temp < 下边界 - 2`，边界抖动不闪。

use crate::theme::Theme;

/// 档位总数：0..=5。
pub const LEVELS: usize = 6;
/// 起染温度（°C）：不高于此值保持主题色。
const START_C: f64 = 60.0;
/// 每档 6°C：(90 - 60) / 5。
const STEP_C: f64 = 6.0;
/// 迟滞带宽（°C）。
const HYSTERESIS_C: f64 = 2.0;

/// 浅色主题调色板：基线黑猫（#000000），暗红 → 亮红。
const LIGHT_PALETTE: [[u8; 3]; LEVELS] = [
    [0x00, 0x00, 0x00],
    [0x7A, 0x1A, 0x12],
    [0x97, 0x20, 0x1A],
    [0xB3, 0x25, 0x1E],
    [0xC9, 0x2D, 0x24],
    [0xD2, 0x35, 0x27],
];

/// 深色主题调色板：基线白猫（#FFFFFF），淡暖粉 → 亮红。
const DARK_PALETTE: [[u8; 3]; LEVELS] = [
    [0xFF, 0xFF, 0xFF],
    [0xFF, 0xB4, 0xA8],
    [0xFF, 0x9C, 0x8C],
    [0xFF, 0x85, 0x73],
    [0xFF, 0x6C, 0x5A],
    [0xFF, 0x53, 0x46],
];

/// 温度(°C) → 档位 0..=5（无迟滞的纯映射）。
/// 非有限值（NaN / ±∞）一律 0 档：读数不可用时不伪造热度。
pub fn level_for(temp: f64) -> u8 {
    if !temp.is_finite() || temp <= START_C {
        return 0;
    }
    if temp >= 90.0 {
        return (LEVELS - 1) as u8;
    }
    // 60 < temp < 90 时 (temp-60)/6 ∈ (0, 5)，截断后落在 0..=4
    (1 + ((temp - START_C) / STEP_C) as u8).min((LEVELS - 1) as u8)
}

/// 带 2°C 迟滞的档位状态机。温度不可用时归 0（保持主题色）。
pub struct HeatStage {
    level: u8,
}

impl HeatStage {
    pub fn new() -> Self {
        Self { level: 0 }
    }

    pub fn level(&self) -> u8 {
        self.level
    }

    /// 喂入一次温度读数并返回（可能更新的）档位。
    /// 允许一次跨多档：越过迟滞边界后直接取 `level_for(temp)`。
    pub fn update(&mut self, temp: Option<f64>) -> u8 {
        let Some(t) = temp.filter(|t| t.is_finite()) else {
            self.level = 0;
            return 0;
        };
        let cur = self.level;
        let upper = START_C + f64::from(cur) * STEP_C;
        let lower = if cur == 0 {
            f64::NEG_INFINITY // 0 档无下边界，只可能升档
        } else {
            START_C + f64::from(cur - 1) * STEP_C
        };
        if t >= upper + HYSTERESIS_C || t < lower - HYSTERESIS_C {
            self.level = level_for(t);
        }
        self.level
    }
}

impl Default for HeatStage {
    fn default() -> Self {
        Self::new()
    }
}

/// 按主题选表、按档位取色；越界档位钳制到 5（不 panic）。
pub fn tint_color(theme: Theme, level: u8) -> [u8; 3] {
    let palette = match theme {
        Theme::Light => &LIGHT_PALETTE,
        Theme::Dark => &DARK_PALETTE,
    };
    palette[(level as usize).min(LEVELS - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    // —— 纯映射：档位边界 ——
    #[test]
    fn boundaries_map_to_expected_levels() {
        assert_eq!(level_for(0.0), 0, "0°C 应为 0 档");
        assert_eq!(level_for(60.0), 0, "60.0°C 应为 0 档（起染阈值含等号）");
        assert_eq!(level_for(60.1), 1, "60.1°C 应升到 1 档");
        assert_eq!(level_for(66.0), 2, "66.0°C 应为 2 档（区间左闭）");
        assert_eq!(level_for(90.0), 5, "90.0°C 应为满档 5");
        assert_eq!(level_for(120.0), 5, "高于 90°C 应钳制在 5 档");
        assert_eq!(level_for(f64::NAN), 0, "NaN 应降级为 0 档");
        assert_eq!(level_for(f64::INFINITY), 0, "正无穷非有限值应降级为 0 档");
        assert_eq!(level_for(f64::NEG_INFINITY), 0, "负无穷非有限值应降级为 0 档");
    }

    #[test]
    fn level_is_monotonic_in_temp() {
        let mut prev = 0u8;
        let mut t = 0.0;
        while t <= 120.0 {
            let lv = level_for(t);
            assert!(lv >= prev, "{t}°C 档位 {lv} 低于更低温度的 {prev}，应单调不减");
            assert!(lv < LEVELS as u8, "{t}°C 档位 {lv} 越界");
            prev = lv;
            t += 0.5;
        }
    }

    // —— 迟滞 ——
    #[test]
    fn hysteresis_holds_level_within_band() {
        let mut s = HeatStage::new();
        assert_eq!(s.level(), 0, "新建状态应为 0 档");
        assert_eq!(s.update(Some(61.0)), 0, "61°C 未越过 60+2 迟滞边界，应保持 0 档");
        assert_eq!(s.update(Some(62.5)), 1, "62.5°C 越过迟滞边界，应升到 1 档");
        assert_eq!(s.update(Some(59.0)), 1, "59°C 未低于 60-2 迟滞边界，应保持 1 档");
        assert_eq!(s.update(Some(57.0)), 0, "57°C 低于迟滞边界，应降回 0 档");
    }

    // —— 跨档 ——
    #[test]
    fn jumps_multiple_levels_at_once() {
        let mut s = HeatStage::new();
        assert_eq!(s.update(Some(90.0)), 5, "0 档遇 90°C 应直接跳到 5 档");
        assert_eq!(s.update(Some(20.0)), 0, "5 档遇 20°C 应直接跌回 0 档");
    }

    // —— 温度不可用 ——
    #[test]
    fn none_resets_to_zero() {
        let mut s = HeatStage::new();
        assert_eq!(s.update(Some(75.0)), 3, "75°C 应为 3 档");
        assert_eq!(s.update(None), 0, "温度不可用应归 0 档");
        assert_eq!(s.level(), 0, "None 之后内部档位应为 0");
        assert_eq!(s.update(None), 0, "连续 None 仍为 0 档");
    }

    // —— 调色板 ——
    #[test]
    fn level_zero_matches_theme_baseline() {
        assert_eq!(tint_color(Theme::Light, 0), [0, 0, 0], "浅色主题 0 档应为纯黑");
        assert_eq!(tint_color(Theme::Dark, 0), [255, 255, 255], "深色主题 0 档应为纯白");
    }

    #[test]
    fn palettes_are_distinct() {
        for level in 1..=5u8 {
            assert_ne!(
                tint_color(Theme::Light, level),
                tint_color(Theme::Dark, level),
                "档位 {level} 两主题颜色不应相同"
            );
        }
        for theme in [Theme::Light, Theme::Dark] {
            for a in 0..LEVELS as u8 {
                for b in (a + 1)..LEVELS as u8 {
                    assert_ne!(
                        tint_color(theme, a),
                        tint_color(theme, b),
                        "{theme:?} 主题档位 {a} 与 {b} 颜色不应相同"
                    );
                }
            }
        }
    }

    #[test]
    fn top_level_is_red_in_both_themes() {
        for theme in [Theme::Light, Theme::Dark] {
            let [r, g, b] = tint_color(theme, 5);
            assert!(r > g && r > b, "{theme:?} 满档颜色应偏红（R 通道最大）");
        }
    }

    #[test]
    fn out_of_range_level_is_clamped() {
        assert_eq!(tint_color(Theme::Light, 6), tint_color(Theme::Light, 5), "越界档位应钳制到 5");
        assert_eq!(tint_color(Theme::Dark, 255), tint_color(Theme::Dark, 5), "越界档位应钳制到 5");
    }
}
