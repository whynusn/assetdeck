//! 交互传播：过滤面板变更 → VM 查询刷新；双击选择 → OpenAsset 事件。

use domain::{
    Asset, AssetId, AssetKind, CategoryId, Filter, SortDirection, SortField, SortSpec, Sorter,
    TagId,
};
use index::FacetIndex;
use ui_viewmodels::grid_vm::{LibraryGridVm, VmEvent};

/// 100 条带标签资产：tag(i%10)、category(i%5)，created_at 随 i 递增。
fn tagged_index() -> FacetIndex {
    let mut idx = FacetIndex::new();
    for i in 0..100u32 {
        idx.insert(&Asset {
            id: AssetId(i),
            name: format!("asset-{i:02}"),
            category: Some(CategoryId(i % 5)),
            tags: vec![TagId(i % 10)],
            created_at: i as i64,
            size_bytes: None,
            kind: AssetKind::Other,
        });
    }
    idx
}

#[test]
fn filter_panel_changes_propagate_to_viewmodel_query() {
    // 面板默认视图：按创建时间倒序的全库
    let sorter = Sorter {
        keys: vec![SortSpec {
            field: SortField::CreatedAt,
            direction: SortDirection::Desc,
        }],
    };
    let mut vm = LibraryGridVm::new(tagged_index(), sorter, 64);
    assert_eq!(vm.total(), 100, "初始视图为全库");

    // 过滤面板点击标签 3 → 位图求值 + 排序重建 id 序列
    vm.set_filter(&Filter::HasTag(TagId(3)));
    assert_eq!(vm.total(), 10, "HasTag(3) 应命中 i%10==3 的 10 条");
    let ids: Vec<u32> = (0..vm.total()).map(|i| vm.id_at(i).0).collect();
    assert_eq!(
        ids,
        vec![93, 83, 73, 63, 53, 43, 33, 23, 13, 3],
        "过滤后序列必须按 CreatedAt 倒序重建"
    );

    // 切回标签 0 → 查询结果随之刷新（面板变更持续传播）
    vm.set_filter(&Filter::HasTag(TagId(0)));
    assert_eq!(vm.total(), 10);
    let ids: Vec<u32> = (0..vm.total()).map(|i| vm.id_at(i).0).collect();
    assert_eq!(ids, vec![90, 80, 70, 60, 50, 40, 30, 20, 10, 0]);
}

#[test]
fn selection_double_click_emits_open_asset_event() {
    let mut vm = LibraryGridVm::new(tagged_index(), Sorter::default(), 64);

    // 双击素材 → VM 依次发出 SelectionChanged 与 OpenAsset
    vm.double_click(AssetId(42));
    let events = vm.take_events();
    assert_eq!(
        events,
        vec![
            VmEvent::SelectionChanged(AssetId(42)),
            VmEvent::OpenAsset(AssetId(42)),
        ],
        "双击须依次发出 SelectionChanged 与 OpenAsset"
    );

    // take_events 语义：取走即清空，不重复投递
    assert!(vm.take_events().is_empty());

    // 容错：不在当前视图中的 id 不产生事件（迟到/乱序消息不得破坏状态）
    vm.double_click(AssetId(999));
    assert!(vm.take_events().is_empty());
}

#[test]
fn set_layout_params_rebuilds_rects_and_content_height() {
    let mut vm = LibraryGridVm::new(tagged_index(), Sorter::default(), 64);
    assert_eq!(vm.total(), 100);
    let default_h = vm.content_height();
    assert!(default_h > 0.0, "初始布局必须产出正的内容高度");

    // 收窄容器 + 减列：Rect 表整体重算，content_height 与末块底边一致
    vm.set_layout_params(400.0, 2, 8.0);
    let total = vm.total();
    // content_height 是所有列中最深的底边，而不是最后一项的底边：masonry 把每项
    // 放进当时最短的列，末项常常落在浅列，用它算高度会把真正最深那列的尾行裁掉。
    let deepest = (0..total).fold(0.0f32, |acc, i| {
        let r = vm.rect_of(i);
        acc.max(r.y + r.h)
    });
    assert_eq!(
        vm.content_height(),
        deepest,
        "content_height 必须覆盖最深列的底边"
    );
    let last = vm.rect_of(total - 1);
    assert!(
        last.y + last.h <= vm.content_height() + 0.25,
        "末项底边不得超出内容总高"
    );
    let first = vm.rect_of(0);
    assert!(first.w > 0.0 && first.x >= 0.0 && first.x + first.w <= 400.0 + 0.25);
    // 更少列 + 更窄容器 → 内容显著变高
    assert!(
        vm.content_height() > default_h,
        "收窄后内容高度必须增长: {} → {}",
        default_h,
        vm.content_height()
    );

    // 重算不破坏 id 序列与 rect 表的同索引对应关系
    assert_eq!(
        vm.id_at(total - 1).0,
        99,
        "默认排序下序列末位仍是 AssetId(99)"
    );
}
