//! D88 文本卡全宽行与检索片段表：文本素材不再走 id 取模的随机占位宽高比——
//! 文本卡不参与分列，独占一行横贯瀑布流整个显示区域宽度（全宽行、恒高），检索
//! 片段只改表现不改布局（片段替换头部预览时版式零抖动的地基）。

use std::collections::HashMap;

use domain::{Asset, AssetId, AssetKind, Sorter};
use index::FacetIndex;
use ui_viewmodels::grid_vm::{LibraryGridVm, TEXT_TILE_HEIGHT};

fn index_with_kinds(kinds: &[AssetKind]) -> FacetIndex {
    let mut index = FacetIndex::new();
    for (i, kind) in kinds.iter().enumerate() {
        index.insert(&Asset {
            id: AssetId(i as u32),
            name: format!("asset-{i}"),
            category: None,
            tags: vec![],
            created_at: i as i64,
            size_bytes: None,
            kind: *kind,
        });
    }
    index
}

#[test]
fn text_tiles_are_full_width_uniform_height_rows() {
    let mut vm = LibraryGridVm::new(
        index_with_kinds(&[AssetKind::Text, AssetKind::Text, AssetKind::Text]),
        Sorter { keys: vec![] },
        16,
    );
    vm.set_layout_params(984.0, 6, 12.0);
    for i in 0..3 {
        let r = vm.rect_of(i);
        assert!(
            (r.h - TEXT_TILE_HEIGHT).abs() < 1e-3,
            "文本卡 {i} 高度必须等于统一常量，实际 {}",
            r.h
        );
        assert_eq!(r.x, 0.0, "全宽行贴容器左缘");
        assert_eq!(r.w, 984.0, "全宽行横贯瀑布流整个显示区域");
        assert_eq!(
            r.y,
            i as f32 * (TEXT_TILE_HEIGHT + ui_viewmodels::FULL_ROW_GAP),
            "列表块内行距一致（紧排）"
        );
    }
    // 列数/容器宽变化不改变全宽行几何（除宽度跟随容器）——高度恒为常量。
    vm.set_layout_params(600.0, 4, 12.0);
    let r = vm.rect_of(0);
    assert_eq!(r.w, 600.0);
    assert!((r.h - TEXT_TILE_HEIGHT).abs() < 1e-3);
}

#[test]
fn theme_switch_reflows_rows_and_can_restore_builtin_geometry() {
    let mut vm = LibraryGridVm::new(
        index_with_kinds(&[AssetKind::Text, AssetKind::Text, AssetKind::Image]),
        Sorter { keys: vec![] },
        16,
    );
    vm.set_layout_params(984.0, 6, 12.0);
    let original = (vm.rect_of(0), vm.rect_of(1), vm.rect_of(2));
    vm.apply_theme_layout(36.0, 16.0, 1.2, 1.15);
    assert_eq!(vm.rect_of(0).h, 36.0);
    assert_eq!(vm.rect_of(1).h, 36.0);
    assert_eq!(vm.rect_of(0).w, 984.0);
    assert!(vm.rect_of(1).y >= vm.rect_of(0).h);
    assert!(vm.rect_of(2).y >= vm.rect_of(1).y + vm.rect_of(1).h + 16.0);
    vm.apply_theme_layout(TEXT_TILE_HEIGHT, 12.0, 1.0, 1.0);
    for (actual, expected) in [vm.rect_of(0), vm.rect_of(1), vm.rect_of(2)]
        .into_iter()
        .zip([original.0, original.1, original.2])
    {
        assert_eq!(
            (actual.x, actual.y, actual.w, actual.h),
            (expected.x, expected.y, expected.w, expected.h)
        );
    }
}

#[test]
fn full_width_rows_break_columns_without_overlap() {
    let mut vm = LibraryGridVm::new(
        index_with_kinds(&[AssetKind::Image, AssetKind::Text, AssetKind::Image]),
        Sorter { keys: vec![] },
        16,
    );
    vm.set_layout_params(984.0, 6, 12.0);
    let (r0, r1, r2) = (vm.rect_of(0), vm.rect_of(1), vm.rect_of(2));
    // 文本行横贯容器，起点在前排图片底边之下，行后图片接在行下且回到首列。
    assert_eq!(r1.w, 984.0);
    assert!(
        (r1.y - (r0.y + r0.h + 12.0)).abs() < 1e-3,
        "全宽行起始 = 前排底边"
    );
    assert!(
        (r2.y - (r1.y + r1.h + 12.0)).abs() < 1e-3,
        "行后瓦片接在行下"
    );
    assert_eq!(r2.x, 0.0);
    // 行后两张瓦片无重叠（r2 已回首列，再排一张落第二列）。
    let mut vm2 = LibraryGridVm::new(
        index_with_kinds(&[
            AssetKind::Image,
            AssetKind::Text,
            AssetKind::Image,
            AssetKind::Image,
        ]),
        Sorter { keys: vec![] },
        16,
    );
    vm2.set_layout_params(984.0, 6, 12.0);
    let (a, b) = (vm2.rect_of(2), vm2.rect_of(3));
    assert!(
        a.x + a.w <= b.x + 1e-3 || b.y >= a.y + a.h - 1e-3 || b.x >= a.x + a.w - 1e-3,
        "全宽行打断后的瓦片不得互相重叠"
    );
}

#[test]
fn text_row_height_is_immutable_constant_not_fraction_of_width() {
    // 回归锚：全宽行高度与列宽换算无关——常量即几何；行内名区宽度
    // （name_zone_px）由 search.rs 单测覆盖钳制语义。
    let mut vm = LibraryGridVm::new(
        index_with_kinds(&[AssetKind::Text]),
        Sorter { keys: vec![] },
        16,
    );
    for width in [984.0f32, 1200.0, 640.0] {
        vm.set_layout_params(width, 6, 12.0);
        assert!((vm.rect_of(0).h - TEXT_TILE_HEIGHT).abs() < 1e-3);
    }
    // 列表块行距常量被导出（布局与文档共用），落在合理区间。
    assert!((0.0..12.0).contains(&ui_viewmodels::FULL_ROW_GAP));
}

#[test]
fn image_tiles_keep_fallback_aspect_not_text_height() {
    let mut vm = LibraryGridVm::new(
        index_with_kinds(&[AssetKind::Image]),
        Sorter { keys: vec![] },
        16,
    );
    vm.set_layout_params(984.0, 6, 12.0);
    assert!(
        (vm.rect_of(0).h - TEXT_TILE_HEIGHT).abs() > 1.0,
        "图片卡仍走占位宽高比，不得被文本卡常量误伤"
    );
}

#[test]
fn search_snippets_roundtrip_and_clear() {
    let mut vm = LibraryGridVm::new(
        index_with_kinds(&[AssetKind::Text]),
        Sorter { keys: vec![] },
        16,
    );
    assert_eq!(vm.search_snippet(AssetId(0)), None, "初始无检索态");
    let mut map = HashMap::new();
    map.insert(AssetId(0), "带\u{E000}命中\u{E001}标记的片段".to_string());
    vm.set_search_snippets(map);
    assert_eq!(
        vm.search_snippet(AssetId(0)),
        Some("带\u{E000}命中\u{E001}标记的片段")
    );
    // 空 map = 退出检索态。
    vm.set_search_snippets(HashMap::new());
    assert_eq!(vm.search_snippet(AssetId(0)), None);
}
