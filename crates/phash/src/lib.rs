//! pHash 计算与汉明距离匹配。

use image::GrayImage;

const SIZE: usize = 32;
const GRID: usize = 8;

fn dct_cos(i: usize, k: usize) -> f64 {
    (std::f64::consts::PI * (2 * i + 1) as f64 * k as f64 / (2.0 * SIZE as f64)).cos()
}

/// 64-bit 感知哈希：32×32 灰度 DCT-II，左上 8×8 系数（去 DC）按中位数阈值取位。
pub fn perceptual_hash_gray(img: &GrayImage) -> u64 {
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

    let mut vals = [0f64; GRID * GRID - 1];
    let mut k = 0;
    for (v, row) in coef.iter().enumerate().take(GRID) {
        for (u, &val) in row.iter().enumerate().take(GRID) {
            if u == 0 && v == 0 {
                continue;
            }
            vals[k] = val;
            k += 1;
        }
    }

    let mut sorted = vals;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2];

    let mut hash = 0u64;
    for (i, &val) in vals.iter().enumerate() {
        if val > median {
            hash |= 1 << i;
        }
    }
    hash
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
            "无关图案距离 {d} 应 ≥16（去重阈值 8 的 2 倍安全边际）"
        );
    }

    #[test]
    fn hamming_distance_known_values() {
        assert_eq!(hamming_distance(0, u64::MAX), 64);
        assert_eq!(hamming_distance(0b1010, 0b0110), 2);
        assert_eq!(hamming_distance(42, 42), 0);
    }
}
