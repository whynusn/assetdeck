//! pHash 计算与汉明距离匹配。

use image::GrayImage;

const SIZE: usize = 32;
const GRID: usize = 8;

/// 低信息阈值：8×8 DCT 网格（去 DC）的最大 |AC| 系数低于该值（灰度 0-255
/// 尺度）即视为近纯色图。此时中位数阈值化作用在浮点取整噪声上，hash 与
/// 图像内容无关（实测两张纯色图距离可落在相似阈值边界）——历史上这类图
/// 互判重复、后者被静默丢弃（D65）。
pub const AC_ENERGY_FLOOR: f64 = 4.0;

fn dct_cos(i: usize, k: usize) -> f64 {
    (std::f64::consts::PI * (2 * i + 1) as f64 * k as f64 / (2.0 * SIZE as f64)).cos()
}

/// 8×8 DCT 系数网格（去 DC）与最大 |AC| 能量。hash 与可信度判定共用。
struct AcGrid {
    values: [f64; GRID * GRID - 1],
    max_abs: f64,
}

fn ac_grid(img: &GrayImage) -> AcGrid {
    let mut px = [[0f64; SIZE]; SIZE];
    for (y, row) in px.iter_mut().enumerate() {
        for (x, cell) in row.iter_mut().enumerate() {
            *cell = img.get_pixel(x as u32, y as u32)[0] as f64;
        }
    }

    let mut rows = [[0f64; SIZE]; SIZE];
    for (y, src_row) in px.iter().enumerate() {
        for (u, out) in rows[y].iter_mut().enumerate() {
            let mut acc = 0.0;
            for (x, &val) in src_row.iter().enumerate() {
                acc += val * dct_cos(x, u);
            }
            *out = acc;
        }
    }

    let mut coef = [[0f64; SIZE]; SIZE];
    for (v, out_row) in coef.iter_mut().enumerate() {
        for (u, out) in out_row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (y, src_row) in rows.iter().enumerate() {
                acc += src_row[u] * dct_cos(y, v);
            }
            *out = acc;
        }
    }

    let mut values = [0f64; GRID * GRID - 1];
    let mut max_abs = 0f64;
    let mut k = 0;
    for (v, row) in coef.iter().enumerate().take(GRID) {
        for (u, &val) in row.iter().enumerate().take(GRID) {
            if u == 0 && v == 0 {
                continue;
            }
            values[k] = val;
            max_abs = max_abs.max(val.abs());
            k += 1;
        }
    }
    AcGrid { values, max_abs }
}

/// 按中位数阈值把 AC 系数取成 64-bit hash（阈值化语义：> 中位数置位）。
fn median_threshold(values: &[f64; GRID * GRID - 1]) -> u64 {
    let mut sorted = *values;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2];

    let mut hash = 0u64;
    for (i, &val) in values.iter().enumerate() {
        if val > median {
            hash |= 1 << i;
        }
    }
    hash
}

/// 64-bit 感知哈希：32×32 灰度 DCT-II，左上 8×8 系数（去 DC）按中位数阈值取位。
///
/// 注意：本函数对近纯色图也返回值（历史上恒为 0）——它只适合「算出 hash」，
/// 不适合直接做相似判定；判定请走 [`reliable_phash`]。
pub fn perceptual_hash_gray(img: &GrayImage) -> u64 {
    let grid = ac_grid(img);
    median_threshold(&grid.values)
}

/// 可信 pHash：AC 能量不足（近纯色图）时返回 None。
///
/// 语义是「宁缺勿错」：拿不出的相似证据就明说没有，绝不拿噪声 hash 把
/// 两张不同的图判成相似（D65 低信息守卫）。
pub fn reliable_phash(img: &GrayImage) -> Option<u64> {
    let grid = ac_grid(img);
    if grid.max_abs < AC_ENERGY_FLOOR {
        return None;
    }
    Some(median_threshold(&grid.values))
}

pub fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Luma};

    fn structured_pattern(size: u32, shift: i16) -> GrayImage {
        ImageBuffer::from_fn(size, size, |x, y| {
            let fx = x as f64 / size as f64;
            let fy = y as f64 / size as f64;
            let v = 110.0
                + 40.0 * (std::f64::consts::TAU * 3.0 * fx).sin()
                + 25.0 * (std::f64::consts::TAU * 2.0 * fy).cos()
                + 30.0 * fx
                + shift as f64;
            Luma([v.clamp(0.0, 255.0) as u8])
        })
    }

    fn stripes(size: u32, period: u32, horizontal: bool) -> GrayImage {
        ImageBuffer::from_fn(size, size, |x, y| {
            let coord = if horizontal { y } else { x };
            let band = (coord / (period / 2)).is_multiple_of(2);
            Luma([if band { 220 } else { 35 }])
        })
    }

    fn flat(size: u32, value: u8, noise: i16) -> GrayImage {
        ImageBuffer::from_fn(size, size, |x, y| {
            // 位置相关的微噪声：模拟 JPEG 压缩残留，幅度小于阈值。
            let wobble = if noise == 0 {
                0
            } else {
                ((x + y) % 2) as i16 * noise
            };
            Luma([(value as i16 + wobble).clamp(0, 255) as u8])
        })
    }

    #[test]
    fn identical_images_hash_distance_is_zero() {
        let a = perceptual_hash_gray(&structured_pattern(64, 0));
        let b = perceptual_hash_gray(&structured_pattern(64, 0));
        assert_eq!(hamming_distance(a, b), 0);
    }

    #[test]
    fn slight_brightness_shift_stays_under_threshold() {
        let a = perceptual_hash_gray(&structured_pattern(64, 0));
        let b = perceptual_hash_gray(&structured_pattern(64, 8));
        let d = hamming_distance(a, b);
        assert!(d <= 10, "轻微亮度平移距离 {d} 应 ≤10");
    }

    #[test]
    fn unrelated_patterns_exceed_threshold() {
        let a = perceptual_hash_gray(&stripes(64, 16, true));
        let b = perceptual_hash_gray(&stripes(64, 16, false));
        let d = hamming_distance(a, b);
        assert!(
            d >= 16,
            "无关图案距离 {d} 应 ≥16（相似阈值 12 之上的安全边际）"
        );
    }

    #[test]
    fn hamming_distance_known_values() {
        assert_eq!(hamming_distance(0, u64::MAX), 64);
        assert_eq!(hamming_distance(0b1010, 0b0110), 2);
        assert_eq!(hamming_distance(42, 42), 0);
    }

    // ----- D65 低信息守卫 -----

    /// 历史缺陷存证：纯色图（无论什么颜色）的裸 hash 完全相同——AC 系数
    /// 全族是同比例的浮点残差，中位数阈值化给出同一位型。这就是「不同
    /// 颜色的纯色图互判重复」的机理，也是 reliable_phash 存在的理由。
    /// 历史缺陷存证（2026-08-31 实测）：纯色图的裸 hash 由浮点取整噪声决定，
    /// 与图像内容无关——深色/亮色两张纯色图实测距离恰为 12（相似阈值边界），
    /// 旧阈值 8 语义下曾把内容为零的图互判重复静默丢弃。守卫语义 = 这类图
    /// 一律不出可信 hash（AC 能量低于地板），从根上不给噪声出场机会。
    #[test]
    fn flat_image_ac_energy_is_below_floor() {
        assert!(
            ac_grid(&flat(32, 128, 0)).max_abs < AC_ENERGY_FLOOR,
            "纯色图 AC 能量应低于地板"
        );
        assert!(
            ac_grid(&flat(32, 128, 1)).max_abs < AC_ENERGY_FLOOR,
            "±1 压缩残留噪声仍低于地板"
        );
        assert!(
            ac_grid(&structured_pattern(64, 0)).max_abs >= AC_ENERGY_FLOOR,
            "结构化图案必须有足够能量"
        );
        assert!(reliable_phash(&flat(32, 20, 0)).is_none());
        assert!(reliable_phash(&flat(32, 240, 0)).is_none());
    }

    #[test]
    fn reliable_phash_rejects_flat_and_noisy_flat() {
        assert!(reliable_phash(&flat(32, 128, 0)).is_none(), "纯色图不可信");
        assert!(
            reliable_phash(&flat(32, 128, 1)).is_none(),
            "幅度 ±1 的压缩残留噪声仍不可信（低于 4.0 地板）"
        );
    }

    #[test]
    fn reliable_phash_accepts_structured_and_matches_bare_hash() {
        let img = structured_pattern(64, 0);
        assert_eq!(reliable_phash(&img), Some(perceptual_hash_gray(&img)));
        assert!(reliable_phash(&stripes(64, 16, true)).is_some());
    }
}
