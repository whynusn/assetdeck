//! 瀑布流布局数学：固定列数 masonry，纯函数、零 IO、确定性输出。
//!
//! 内存模型（D10）：全量 Rect 表为纯数字常驻层（10 万条 ≈ 1.6MB），支持 O(1) 任意跳转。

/// 轴对齐矩形，坐标相对网格容器原点（单位：逻辑像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 固定列数 masonry：每项缩放到列宽并保持宽高比（aspect = w/h），放入当前最短列。
///
/// - 确定性：最短列并列时取最左列，同输入两次调用输出完全一致。
/// - 非法输入约定：`columns == 0` 按 1 列处理；列宽非正（容器过窄/NaN）返回空表；
///   aspect 非有限或 ≤0 按正方形（1.0）兜底，避免 inf 高度破坏确定性。
/// - 输出顺序与输入一致；第 i 个 Rect 对应 `aspects[i]`。
pub fn masonry_layout(container_width: f32, columns: u32, gap: f32, aspects: &[f32]) -> Vec<Rect> {
    let columns = columns.max(1) as usize;
    let col_w = (container_width - gap * (columns as f32 - 1.0)) / columns as f32;
    if col_w.is_nan() || col_w <= 0.0 {
        return Vec::new();
    }

    let mut col_bottoms = vec![0.0f32; columns];
    let mut rects = Vec::with_capacity(aspects.len());
    for &aspect in aspects {
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        };
        let h = col_w / aspect;

        // 最短列：严格小于保证并列取最左（确定性 tie-break）
        let mut shortest = 0;
        for c in 1..columns {
            if col_bottoms[c] < col_bottoms[shortest] {
                shortest = c;
            }
        }

        let x = shortest as f32 * (col_w + gap);
        let y = col_bottoms[shortest];
        rects.push(Rect { x, y, w: col_w, h });
        col_bottoms[shortest] = y + h + gap;
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_empty_layout() {
        assert!(masonry_layout(800.0, 4, 12.0, &[]).is_empty());
    }

    #[test]
    fn single_item_anchors_top_left_with_column_width() {
        let rects = masonry_layout(800.0, 4, 12.0, &[2.0]);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].x, 0.0);
        assert_eq!(rects[0].y, 0.0);
        let expected_w = (800.0 - 3.0 * 12.0) / 4.0;
        assert_eq!(rects[0].w, expected_w);
        assert_eq!(rects[0].h, expected_w / 2.0);
    }

    #[test]
    fn degenerate_inputs_are_handled_deterministically() {
        // 容器宽非正 → 空
        assert!(masonry_layout(-1.0, 4, 12.0, &[1.0]).is_empty());
        // 列数钳为 1
        assert_eq!(masonry_layout(800.0, 0, 12.0, &[1.0]).len(), 1);
        // aspect ≤0/NaN → 正方形兜底
        assert_eq!(masonry_layout(800.0, 4, 12.0, &[0.0]).len(), 1);
        assert_eq!(masonry_layout(800.0, 4, 12.0, &[-2.0, f32::NAN]).len(), 2);
    }
}
