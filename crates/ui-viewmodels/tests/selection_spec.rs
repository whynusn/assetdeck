//! D47/D48 VM 层契约：选区状态机 + 右键菜单数据（红线 A 的守卫在此层）。
//!
//! 红线 A（D47）：多选模式期间 `VmEvent::OpenAsset` 必须恰零——模式存在的
//! 唯一意义就是屏蔽 D13 双击上框。红线 B（D13 不回归）：常态双击行为与
//! 今日一字不变（另一半守卫在 interaction_spec，本文件不重复改它）。
//!
//! 纯函数化设计：修饰键由壳层从 `PointerEvent.modifiers` 传入（spike S1
//! 已确认 release 携带 Ctrl/Shift），状态机本体不碰键盘，全部可单测。

use domain::{Asset, AssetId, AssetKind, Sorter};
use index::FacetIndex;
use ui_viewmodels::grid_vm::{LibraryGridVm, VmEvent};
use ui_viewmodels::selection::{MenuAction, Modifiers};

/// 10 条素材，id 0..9；默认 Sorter（CreatedAt 升序）下视图序 = id 升序。
fn vm10() -> LibraryGridVm {
    let mut idx = FacetIndex::new();
    for i in 0..10u32 {
        idx.insert(&Asset {
            id: AssetId(i),
            name: format!("a{i}"),
            category: None,
            tags: vec![],
            created_at: i as i64,
            size_bytes: None,
            kind: AssetKind::Other,
        });
    }
    LibraryGridVm::new(idx, Sorter::default(), 16)
}

fn drain_open(events: &[VmEvent]) -> Vec<AssetId> {
    events
        .iter()
        .filter_map(|e| match e {
            VmEvent::OpenAsset(id) => Some(*id),
            _ => None,
        })
        .collect()
}

// ---------- 红线 A：多选模式零 OpenAsset ----------

#[test]
fn multi_mode_any_click_sequence_emits_zero_open_asset() {
    let mut vm = vm10();
    vm.enter_multi();
    // 任意点击序列：单击、双击、Ctrl、Shift、范围、全选混排。
    vm.single_click(AssetId(1), Modifiers::default());
    vm.double_click(AssetId(2)); // 模式内双击 = 无操作
    vm.single_click(
        AssetId(3),
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    vm.single_click(
        AssetId(7),
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    vm.select_all_visible();
    vm.double_click(AssetId(0));
    let events = vm.take_events();
    assert!(
        drain_open(&events).is_empty(),
        "红线 A 违规：多选模式出现 OpenAsset → {events:?}"
    );
}

#[test]
fn multi_mode_single_click_toggles_selection() {
    let mut vm = vm10();
    vm.enter_multi();
    vm.single_click(AssetId(4), Modifiers::default());
    assert!(vm.is_selected(AssetId(4)));
    assert_eq!(vm.selected_count(), 1);
    vm.single_click(AssetId(4), Modifiers::default());
    assert!(!vm.is_selected(AssetId(4)));
    assert_eq!(vm.selected_count(), 0);
    // 每次点击都发 SelectionChanged（壳层重绘钩子），但绝不发 OpenAsset。
    let events = vm.take_events();
    assert_eq!(
        events,
        vec![
            VmEvent::SelectionChanged(AssetId(4)),
            VmEvent::SelectionChanged(AssetId(4)),
        ]
    );
}

#[test]
fn exit_multi_clears_selection_and_restores_normal() {
    let mut vm = vm10();
    vm.enter_multi();
    vm.single_click(AssetId(2), Modifiers::default());
    vm.exit_multi();
    assert_eq!(vm.selected_count(), 0);
    assert!(!vm.multi_mode());
    // 退出即常态：双击恢复发 OpenAsset（红线 B 的另一面）。
    vm.double_click(AssetId(2));
    assert_eq!(drain_open(&vm.take_events()), vec![AssetId(2)]);
}

// ---------- 常态修饰键（R7）：不触上框 ----------

#[test]
fn normal_ctrl_click_toggles_without_open() {
    let mut vm = vm10();
    vm.single_click(
        AssetId(5),
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    assert!(vm.is_selected(AssetId(5)));
    let events = vm.take_events();
    assert!(drain_open(&events).is_empty(), "带 Ctrl 的点击不得上框");
    vm.single_click(
        AssetId(5),
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    assert!(!vm.is_selected(AssetId(5)));
}

#[test]
fn normal_plain_single_click_does_nothing_regression_guard() {
    // 常态无修饰单击 = 行为与今日完全一致（今日：单击无事，双击才上框）。
    let mut vm = vm10();
    vm.single_click(AssetId(6), Modifiers::default());
    assert!(vm.take_events().is_empty());
    assert_eq!(vm.selected_count(), 0);
}

#[test]
fn shift_range_follows_view_order_and_anchor_survives() {
    let mut vm = vm10();
    // 无锚点时 Shift 点击退化为单选。
    vm.single_click(
        AssetId(6),
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(vm.selection_ids(), vec![AssetId(6)]);
    // 锚点 = 上次点击（非范围键）的 3；从 3 到 7 按视图序闭区间。
    vm.single_click(AssetId(3), Modifiers::default());
    vm.take_events();
    vm.single_click(
        AssetId(7),
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(
        vm.selection_ids(),
        vec![AssetId(3), AssetId(4), AssetId(5), AssetId(6), AssetId(7)]
    );
    // 反向范围同样成立（锚点保留在 3）。
    vm.single_click(
        AssetId(1),
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(
        vm.selection_ids(),
        vec![AssetId(1), AssetId(2), AssetId(3)],
        "Shift 范围是替换式（资源管理器语义），锚点不动"
    );
}

#[test]
fn ctrl_shift_click_extends_range_union() {
    let mut vm = vm10();
    vm.single_click(AssetId(8), Modifiers::default());
    vm.single_click(
        AssetId(1),
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    // Ctrl+Shift = 从锚点(8)取范围并入既有选区（1 保留）。
    vm.single_click(
        AssetId(4),
        Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(
        vm.selection_ids(),
        vec![
            AssetId(1),
            AssetId(4),
            AssetId(5),
            AssetId(6),
            AssetId(7),
            AssetId(8)
        ]
    );
}

#[test]
fn select_all_covers_current_view() {
    let mut vm = vm10();
    vm.select_all_visible();
    assert_eq!(vm.selected_count(), 10);
    vm.exit_multi(); // 常态下也允许（Ctrl+A 不限模式）
    assert_eq!(vm.selected_count(), 0, "退出清空语义不变");
}

// ---------- 右键菜单（D48/R10）：五项穷举 ----------

#[test]
fn context_menu_items_are_exactly_five() {
    use ui_viewmodels::selection::MENU_ITEMS;
    let labels: Vec<&str> = MENU_ITEMS.iter().map(|&(_, l)| l).collect();
    assert_eq!(
        labels,
        vec!["复制", "移动到分类", "重命名", "属性", "删除"],
        "CONTEXT.md/D48 用语穷举：不得多一项少一项或换词"
    );
    let ids: Vec<MenuAction> = MENU_ITEMS.iter().map(|(a, _)| *a).collect();
    assert_eq!(
        ids,
        vec![
            MenuAction::Copy,
            MenuAction::MoveToCategory,
            MenuAction::Rename,
            MenuAction::Properties,
            MenuAction::Delete,
        ]
    );
}

#[test]
fn context_menu_targets_selection_or_hit_tile() {
    let mut vm = vm10();
    // 无选区：作用于右键命中瓦片（R11 后半句）。
    let menu = vm.context_menu(AssetId(3));
    assert_eq!(menu.targets, vec![AssetId(3)]);
    assert_eq!(menu.items.len(), 5);
    assert!(menu.items.iter().all(|i| i.enabled));
    // 有选区且命中瓦片在选区内：作用于整个选区（R11 前半句）。
    vm.enter_multi();
    vm.single_click(AssetId(2), Modifiers::default());
    vm.single_click(AssetId(3), Modifiers::default());
    let menu = vm.context_menu(AssetId(3));
    assert_eq!(menu.targets, vec![AssetId(2), AssetId(3)]);
    // 有选区但命中瓦片不在选区：收窄到命中瓦片（资源管理器语义）。
    let menu = vm.context_menu(AssetId(9));
    assert_eq!(menu.targets, vec![AssetId(9)]);
}

#[test]
fn right_click_emits_context_menu_event_only() {
    let mut vm = vm10();
    vm.right_click(AssetId(1));
    let events = vm.take_events();
    assert_eq!(events, vec![VmEvent::ContextMenuRequested(AssetId(1))]);
}

// ---------- 红线 B：常态双击链路一字不变 ----------

#[test]
fn normal_double_click_chain_unchanged() {
    let mut vm = vm10();
    vm.double_click(AssetId(7));
    assert_eq!(
        vm.take_events(),
        vec![
            VmEvent::SelectionChanged(AssetId(7)),
            VmEvent::OpenAsset(AssetId(7)),
        ]
    );
    // 视图外 id 的容错语义也不变。
    vm.double_click(AssetId(999));
    assert!(vm.take_events().is_empty());
}

#[test]
fn filter_change_clears_stale_selection() {
    // 选区里的 id 掉出当前视图时必须失效，否则操作条会对隐形素材动手。
    let mut vm = vm10();
    vm.single_click(
        AssetId(3),
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    );
    vm.enter_multi();
    vm.single_click(AssetId(4), Modifiers::default());
    vm.set_filter(&domain::Filter::NameContains("a9".into()));
    assert_eq!(vm.selected_count(), 0, "切视图清空选区（模式保留）");
    assert!(vm.multi_mode());
}
