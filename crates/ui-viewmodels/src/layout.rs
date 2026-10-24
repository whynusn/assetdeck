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

/// 列宽纯函数：masonry 布局与文本卡统一高度换算（grid_vm）共用一处公式，
/// 防止两处推导漂移。
pub fn column_width(container_width: f32, columns: u32, gap: f32) -> f32 {
    let columns = columns.max(1) as usize;
    (container_width - gap * (columns as f32 - 1.0)) / columns as f32
}

/// 单项布局规格：`Tile(aspect)` 缩放到列宽参与分列；`FullRow` 不分列，
/// 独占一整行（全宽、高 = full_row_height）。文本卡（D88）用 FullRow——
/// 「占满瀑布流整个显示区域宽度」不是任何单列的 aspect 能表达出来的。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LayoutItem {
    Tile(f32),
    FullRow,
}

/// 全宽行列表块的行距（D88 复盘二轮）：连续文本行构成一个「列表块」，
/// 格式塔邻近原则——块内紧排（6px = Spacing.sm），块与图片网格交界仍走
/// masonry gap（12px），间距本身就在区分「同组」与「换组」。
pub const FULL_ROW_GAP: f32 = 6.0;

/// 混合 masonry：Tile 走固定列数最短列放置，FullRow 在当前所有列的最低底边
/// 处独占一整行、随后把所有列压回同一底边（输出顺序 = 输入顺序，视觉流与
/// ids 序一致）。
///
/// - 确定性约定与 [`masonry_layout`] 相同；`full_row_height` 仅被 FullRow 读取。
/// - FullRow 的收笔间距看下一项：下一项还是 FullRow 用 [`FULL_ROW_GAP`]，
///   否则用 gap——上一项的收笔间距即本项的段前距，故无需前视。
pub fn masonry_layout_mixed(
    container_width: f32,
    columns: u32,
    gap: f32,
    items: &[LayoutItem],
    full_row_height: f32,
) -> Vec<Rect> {
    let columns = columns.max(1) as usize;
    let col_w = column_width(container_width, columns as u32, gap);
    if col_w.is_nan() || col_w <= 0.0 {
        return Vec::new();
    }

    let mut col_bottoms = vec![0.0f32; columns];
    let mut rects = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        match *item {
            LayoutItem::FullRow => {
                let y = col_bottoms
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                rects.push(Rect {
                    x: 0.0,
                    y,
                    w: container_width,
                    h: full_row_height,
                });
                let after = match items.get(i + 1) {
                    Some(LayoutItem::FullRow) => FULL_ROW_GAP,
                    _ => gap,
                };
                col_bottoms.fill(y + full_row_height + after);
            }
            LayoutItem::Tile(aspect) => {
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
        }
    }
    rects
}

/// 固定列数 masonry：每项缩放到列宽并保持宽高比（aspect = w/h），放入当前最短列。
///
/// - 确定性：最短列并列时取最左列，同输入两次调用输出完全一致。
/// - 非法输入约定：`columns == 0` 按 1 列处理；列宽非正（容器过窄/NaN）返回空表；
///   aspect 非有限或 ≤0 按正方形（1.0）兜底，避免 inf 高度破坏确定性。
/// - 输出顺序与输入一致；第 i 个 Rect 对应 `aspects[i]`。
pub fn masonry_layout(container_width: f32, columns: u32, gap: f32, aspects: &[f32]) -> Vec<Rect> {
    let items: Vec<LayoutItem> = aspects.iter().map(|&a| LayoutItem::Tile(a)).collect();
    // 纯 Tile 输入触及不到 FullRow 路径，full_row_height 占位传 0。
    masonry_layout_mixed(container_width, columns, gap, &items, 0.0)
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
        // 全宽行同样受容器宽合法性约束
        assert!(masonry_layout_mixed(-1.0, 4, 12.0, &[LayoutItem::FullRow], 28.0).is_empty());
    }

    #[test]
    fn full_row_spans_container_and_bumps_all_columns() {
        let items = [
            LayoutItem::Tile(2.0),
            LayoutItem::FullRow,
            LayoutItem::Tile(2.0),
            LayoutItem::Tile(2.0),
        ];
        let rects = masonry_layout_mixed(800.0, 4, 12.0, &items, 28.0);
        let col_w = (800.0 - 3.0 * 12.0) / 4.0;
        assert_eq!(rects.len(), 4);
        // 全宽行横贯容器、高度即常量
        assert_eq!(rects[1].x, 0.0);
        assert_eq!(rects[1].w, 800.0);
        assert_eq!(rects[1].h, 28.0);
        // 行起点 = 前排瓦片的列底边（masonry 列底边约定含收尾 gap）；行之后
        // 所有列从同一底边重新并排
        assert_eq!(rects[1].y, col_w / 2.0 + 12.0);
        assert_eq!(rects[2].y, rects[1].y + 28.0 + 12.0);
        assert_eq!(rects[2].x, 0.0);
        assert_eq!(rects[3].x, col_w + 12.0);
        assert_eq!(rects[3].y, rects[2].y);
    }

    #[test]
    fn consecutive_full_rows_tighten_to_list_block_gap() {
        let items = [
            LayoutItem::FullRow,
            LayoutItem::FullRow,
            LayoutItem::Tile(1.0),
        ];
        let rects = masonry_layout_mixed(800.0, 4, 12.0, &items, 28.0);
        // 块内紧排（列表块行距），块与图片网格交界回到 masonry gap
        assert_eq!(rects[1].y, rects[0].y + 28.0 + FULL_ROW_GAP);
        assert_eq!(rects[2].y, rects[1].y + 28.0 + 12.0);
    }
}
