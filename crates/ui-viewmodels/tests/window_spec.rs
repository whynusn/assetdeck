//! 布局数学不变量、内存守卫与滚动跳转帧预算（D10 验收线的落点）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use domain::{Asset, AssetId, AssetKind, CategoryId, Sorter};
use index::FacetIndex;
use ui_viewmodels::grid_vm::{LibraryGridVm, ThumbnailProvider};
use ui_viewmodels::{masonry_layout, Rect};

/// 边界/重叠断言的浮点容差。
const EPS: f32 = 0.25;

/// 收缩 ε 后仍相交才算实质重叠（相邻矩形共享边不算）。
/// 标准相交判定：交集宽 = min(右边) − max(左边)，双轴交集均 > ε 才算重叠。
fn strictly_overlapping(a: &Rect, b: &Rect, eps: f32) -> bool {
    let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    ox > eps && oy > eps
}

#[test]
fn grid_layout_math_variable_aspect_no_overlap() {
    // 混合宽高比：横图/竖图/极端比例混合，覆盖最短列并列路径
    let aspects: Vec<f32> = (0..400u32)
        .map(|i| (i % 7 + 1) as f32 / ((i % 5) as f32 + 1.0))
        .chain(std::iter::once(7.0))
        .chain(std::iter::once(0.2))
        .collect();

    let rects = masonry_layout(800.0, 4, 12.0, &aspects);
    assert_eq!(rects.len(), aspects.len(), "输出顺序与数量必须与输入一致");

    for (i, a) in rects.iter().enumerate() {
        // 容器边界不变量
        assert!(a.x >= -EPS, "rect[{i}] x={} 越出左边界", a.x);
        assert!(
            a.x + a.w <= 800.0 + EPS,
            "rect[{i}] 右侧越界: x+w={}",
            a.x + a.w
        );
        assert!(a.y >= -EPS && a.h > 0.0, "rect[{i}] y/h 非法: {a:?}");
        // 两两无重叠
        for b in rects.iter().skip(i + 1) {
            assert!(
                !strictly_overlapping(a, b, EPS),
                "rect[{i}] 与后续矩形重叠: {a:?} vs {b:?}"
            );
        }
    }

    // 确定性：同输入两次调用输出完全一致
    let again = masonry_layout(800.0, 4, 12.0, &aspects);
    assert_eq!(rects, again, "同输入两次调用输出必须相同");
}

/// 合成分面索引：id 即序号，确定性可复现。
fn synthetic_index(n: u32) -> FacetIndex {
    let mut idx = FacetIndex::new();
    for i in 0..n {
        idx.insert(&Asset {
            id: AssetId(i),
            name: format!("asset-{i}"),
            category: Some(CategoryId(i % 200)),
            tags: vec![],
            created_at: i as i64,
            size_bytes: None,
            kind: AssetKind::Other,
        });
    }
    idx
}

/// 记录每次 load 调用的 stub 提供者（返回 32 字节假缩略图）。
#[derive(Clone)]
struct RecordingProvider {
    calls: Arc<Mutex<Vec<u32>>>,
}

impl RecordingProvider {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded(&self) -> Vec<u32> {
        self.calls.lock().unwrap().clone()
    }
}

impl ThumbnailProvider for RecordingProvider {
    fn load(&self, id: AssetId) -> Option<Vec<u8>> {
        self.calls.lock().unwrap().push(id.0);
        Some(vec![0xAB; 32])
    }
}

#[test]
fn viewmodel_window_of_100k_model_loads_only_visible_slice() {
    const N: u32 = 100_000;
    // 内存守卫红线：小容量 LRU（64），杜绝「恰好不超」的巧合断言
    let provider = RecordingProvider::new();
    let mut vm = LibraryGridVm::new(synthetic_index(N), Sorter::default(), 64);
    vm.set_provider(Box::new(provider.clone()));

    // 首屏：可见 20 条 → 驻留恰为 [0, 40)（可见窗口 + 右侧过扫描）
    vm.ensure_window(0, 20);
    let resident = vm.visible_cache_ids();
    let expected_first: Vec<AssetId> = (0..40u32).map(AssetId).collect();
    assert_eq!(
        resident, expected_first,
        "首屏驻留必须恰为可见窗口+过扫描，窗外零缩略图"
    );
    assert_eq!(provider.recorded().len(), 40, "每条仅加载一次");

    // 缓存命中：重复请求同一窗口零新增加载
    vm.ensure_window(0, 20);
    assert_eq!(provider.recorded().len(), 40, "命中缓存不得重复加载");

    // 远距跳转：旧窗口全部驱逐，新窗口物化；总加载有界（= 两窗口大小之和）
    vm.ensure_window(90_000, 20);
    let resident = vm.visible_cache_ids();
    let expected_far: Vec<AssetId> = (89_980u32..90_040).map(AssetId).collect();
    assert_eq!(
        resident, expected_far,
        "跳转后窗外零驻留（显式驱逐，不依赖容量巧合）"
    );
    let calls = provider.recorded();
    assert_eq!(
        calls.len(),
        100,
        "总加载次数 = 两窗口大小(40+60)，绝不允许全量加载"
    );
    let mut unique = calls.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), calls.len(), "任何条目不得被重复加载");

    // 驻留规模有界：不超过注入的 LRU 容量
    assert!(
        resident.len() <= 64,
        "驻留 {} 超 LRU 容量 64",
        resident.len()
    );
}

#[test]
fn scroll_jump_10k_items_keeps_frame_budget() {
    let mut vm = LibraryGridVm::new(synthetic_index(100_000), Sorter::default(), 64);
    vm.set_provider(Box::new(RecordingProvider::new()));

    // 预热：初始视口已物化
    vm.ensure_window(0, 20);

    // 从顶部直接跳到第 10000 项：rect O(1) 索引 + 可见切片物化
    let start = Instant::now();
    vm.ensure_window(10_000, 20);
    let probe = vm.rect_of(10_000);
    std::hint::black_box(&probe);
    let elapsed = start.elapsed();

    // best-effort 宽裕上界（50ms）：软件渲染近似下的跳转路径耗时；
    // 真实 GPU 渲染帧率属手工验收清单，不做自动化断言（TDD_PLAN 第六节诚实清单）。
    assert!(
        elapsed.as_secs_f64() < 0.050,
        "跳转到第 1 万项耗时 {elapsed:.3?} 超出 50ms 宽裕上界"
    );
    assert!(probe.w > 0.0 && probe.h > 0.0, "目标项 rect 必须有效");
}

/// 多列 masonry 首行的 y 全为 0，取窗口时不能因二分跳到最右列而漏掉整行。
/// 这是「素材管理界面第一行显示不出来」的直接回归守卫。
#[test]
fn visible_range_covers_whole_first_row_at_top() {
    let vm = LibraryGridVm::new(synthetic_index(60), Sorter::default(), 64);
    let (start, end) = vm.visible_range(0.0, 600.0);

    assert_eq!(start, 0, "视口顶部时首项必须在可见区间内");

    // 首行 = 所有 y == 0 的条目（列数由 VM 默认布局参数决定）。
    let first_row: Vec<usize> = (0..vm.total())
        .filter(|&i| vm.rect_of(i).y == 0.0)
        .collect();
    assert!(first_row.len() > 1, "本用例前提是多列布局，首行应有多项");
    for i in &first_row {
        assert!(
            *i >= start && *i < end,
            "首行第 {i} 项落在可见区间 [{start}, {end}) 之外，第一行会整排消失"
        );
    }
}

/// 可见区间必须覆盖所有与视口相交的 rect，不能只覆盖「起点在视口内」的那些。
#[test]
fn visible_range_includes_every_rect_intersecting_viewport() {
    let vm = LibraryGridVm::new(synthetic_index(400), Sorter::default(), 64);
    let (top, height) = (1_500.0f32, 700.0f32);
    let (start, end) = vm.visible_range(top, height);

    for i in 0..vm.total() {
        let r = vm.rect_of(i);
        let intersects = r.y < top + height && r.y + r.h > top;
        if intersects {
            assert!(
                i >= start && i < end,
                "第 {i} 项与视口相交却不在 [{start}, {end}) 内: {r:?}"
            );
        }
    }
    assert!(end > start, "非空模型的可见区间不得为空");
}

/// 空模型与非法视口参数都必须给出确定性的安全区间，不 panic。
#[test]
fn visible_range_is_defensive_on_degenerate_inputs() {
    let empty = LibraryGridVm::new(FacetIndex::new(), Sorter::default(), 8);
    assert_eq!(empty.visible_range(0.0, 600.0), (0, 0));

    let vm = LibraryGridVm::new(synthetic_index(30), Sorter::default(), 8);
    // 高度为 0（首帧尚未布局）仍要给出至少一项，否则 tiles 恒空、界面全白。
    let (start, end) = vm.visible_range(0.0, 0.0);
    assert_eq!(start, 0);
    assert!(end >= 1, "高度未知时至少物化一项");
    // NaN/负值按 0 处理
    assert_eq!(vm.visible_range(f32::NAN, 600.0).0, 0);
    assert_eq!(vm.visible_range(-100.0, 600.0).0, 0);
}

/// 视口极高时可见区间受 MAX_VISIBLE 保护，不会一次物化整库。
#[test]
fn visible_range_is_bounded_by_render_guard() {
    let vm = LibraryGridVm::new(synthetic_index(10_000), Sorter::default(), 64);
    let (start, end) = vm.visible_range(0.0, 1_000_000.0);
    assert!(
        end - start <= ui_viewmodels::grid_vm::MAX_VISIBLE,
        "可见区间 {} 项超过渲染保护阀",
        end - start
    );
}

/// 注入真实宽高比后布局必须按真实比例走，缺尺寸的条目回落占位公式且高度仍为正。
#[test]
fn injected_aspects_drive_layout_and_missing_ones_fall_back() {
    let mut vm = LibraryGridVm::new(synthetic_index(12), Sorter::default(), 64);
    let baseline: Vec<Rect> = (0..vm.total()).map(|i| vm.rect_of(i)).collect();

    let mut aspects = HashMap::new();
    // 只给一半条目真实尺寸：另一半必须仍然可布局。
    for i in 0..6u32 {
        aspects.insert(AssetId(i), 16.0 / 9.0);
    }
    vm.set_aspects(aspects);

    let after: Vec<Rect> = (0..vm.total()).map(|i| vm.rect_of(i)).collect();
    assert_ne!(baseline, after, "注入真实宽高比后布局必须变化");
    for (i, r) in after.iter().enumerate() {
        assert!(r.h > 0.0, "第 {i} 项高度非正: {r:?}");
    }

    // 16:9 的瓦片高度 = 列宽 / (16/9)
    let wide = after[0];
    let expected_h = wide.w / (16.0 / 9.0);
    assert!(
        (wide.h - expected_h).abs() < 0.01,
        "真实宽高比未生效: h={} 期望 {expected_h}",
        wide.h
    );
}

/// 内容总高必须取所有列的最深底边，否则滚到底时最后一行被裁掉。
#[test]
fn content_height_covers_deepest_column() {
    let vm = LibraryGridVm::new(synthetic_index(97), Sorter::default(), 64);
    let deepest = (0..vm.total())
        .map(|i| {
            let r = vm.rect_of(i);
            r.y + r.h
        })
        .fold(0.0f32, f32::max);
    assert!(
        (vm.content_height() - deepest).abs() < 0.01,
        "content_height={} 未覆盖最深底边 {deepest}",
        vm.content_height()
    );
}
