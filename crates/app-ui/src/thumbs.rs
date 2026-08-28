//! 壳层缩略图驻留与渐进装载（D43）。
//!
//! 内存纪律（D10 「可见窗口外零驻留」收口到壳层）：
//! 旧实现把解码图缓存放在无界 HashMap<u32, slint::Image> 里——grid_vm::ensure_window
//! 的窗口驱逐纪律只落在 VM 的字节 LRU 上，而生产路径从未调过 set_provider，VM 缓存
//! 恒空；壳层 slint::Image 强引用把每张浏览过的缩略图的解码 RGBA 缓冲永久钉住：
//! 切过的分类越多，常驻越大，且永不回收（实测「内存只涨不降」的根因）。
//! 现在：每次刷新都以「当前物化窗口」的 id 集合**显式驱逐窗外条目**，LRU 容量仅作
//! 视口极端情况下的兜底上界。驻留上界与浏览过的分类数无关。
//!
//! 装载节奏（修「切换分类瞬间卡顿」）：
//! 旧实现在一次 set_vec 里同步解码整个可见窗口——Slint 的 Image::load_from_path
//! 内部是 image::open 完整 PNG 解码 + 文件 IO，首切一个分类 = UI 线程单帧几十张
//! 解码 + 全批纹理首传，瞬间数百毫秒停顿。现在每次刷新/填充 pass 最多解码
//! THUMB_LOAD_BUDGET 张，余下的由 SingleShot Timer 按 FILL_INTERVAL_MS 渐进
//! 补齐（已装行走 set_row_data 定向更新，不整表重建模型）——分类切换立刻出布局
//! 与色块，缩略图按帧浮现。
//!
//! 红线边界（D11 不变）：这里是「显示装载」已派生的 320px 浏览缩略图——Slint 渲染器
//! 首次渲染本来就要做的解码，只是分期执行；缩略图生成 / 原始媒体解码仍全部在
//! worker 进程。

use std::cell::RefCell;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::time::Duration;

use lru::LruCache;
use slint::{Model, Timer, TimerMode, VecModel};

use ui_viewmodels::grid_vm::{LibraryGridVm, MAX_VISIBLE};
use ui_viewmodels::{AssetId, RealAssetResolver};

use crate::cards::{ResolverCardProvider, TileCardData, TileCardDataProvider};
use crate::ui_enums;
use crate::{AppWindow, TileData};

/// 缩略图来源：真实库有派生 PNG 就取其路径，演示数据一律无图。
///
/// 用 Rc<RefCell<Option<_>>> 而不是 Option<Rc<RefCell<_>>>，是为了让“导入后
/// 从演示库切换成真实库”时，所有已捕获此引用的回调都能看到新的 resolver。
pub(crate) type ThumbSource = Rc<RefCell<Option<RealAssetResolver>>>;

/// 每 pass 最多解码的缩略图张数：6 张 320px PNG ≈ 12-24ms，压在单帧预算内。
/// 余下的缺口由填充 Timer 续跑补齐（渐进浮现，而不是一帧几百毫秒的硬停）。
pub(crate) const THUMB_LOAD_BUDGET: usize = 6;

/// 填充 pass 的间隔：SingleShot 定时器，下一轮事件循环即触发；16ms 对应
/// 「一帧的余量」，保证填充不挤占当前帧的渲染。
pub(crate) const FILL_INTERVAL_MS: u64 = 16;

/// 缓存容量兜底：与 VM 的可见区间上限 MAX_VISIBLE 一致。
/// 窗口显式驱逐是主纪律；容量只在「视口合法但极大」时兜底——
/// 取值 ≥ 单窗上限，容量驱逐永不与窗口驱逐打架（不会把窗内条目挤掉造成抖动）。
pub(crate) const THUMB_CACHE_CAPACITY: usize = MAX_VISIBLE;

/// 兜底色块调色板（ARGB）：缩略图尚未派 生 / 派生失败时露出，保证瓦片仍可点。
const PALETTE: [u32; 8] = [
    0xFF4C6EF5, 0xFFF76707, 0xFF20C997, 0xFFBE4BDB, 0xFF228BE6, 0xFFFFA8A8, 0xFF82C91E, 0xFFFAB005,
];

/// 有界缩略图 Image 缓存：窗口显式驱逐 + LRU 容量兜底（D43）。
///
/// 每个条目持有 slint::Image 强引用（解码缓冲的生命周期）。负缓存：缩略图文件
/// 缺失时以 Image::default() 入缓存，避免缺图瓦片每个 pass 都重试读盘/解码。
pub(crate) struct ThumbCache {
    entries: LruCache<u32, slint::Image>,
}

impl ThumbCache {
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity.max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            entries: LruCache::new(capacity),
        }
    }

    /// LRU 命中（触碰 recency）：返回已解码 Image 的共享句柄，未驻留返回 None。
    pub(crate) fn get(&mut self, id: u32) -> Option<slint::Image> {
        self.entries.get(&id).cloned()
    }

    /// 插入或更新；超容量时按 LRU 驱逐最久未用条目（兜底上界，主纪律是窗口驱逐）。
    pub(crate) fn put(&mut self, id: u32, image: slint::Image) {
        self.entries.put(id, image);
    }

    /// 窗口显式驱逐：保留 window 内的条目，窗外一律清除。
    /// 这是「可见窗口外零驻留」的执行者——与 grid_vm::ensure_window 同一纪律，
    /// 只是作用对象从 VM 的字节缓存换成了壳层的 Image 强引用。
    pub(crate) fn retain_window(&mut self, window: &HashSet<u32>) {
        let stale: Vec<u32> = self
            .entries
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| !window.contains(k))
            .collect();
        for id in stale {
            self.entries.pop(&id);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

// 渐进装载的单发续跑计时器。Slint 的 Timer 非 Send，只能活在 UI 线程；
// 放线程局部（仿 main.rs 的 NOTICE_TIMER 先例）。
thread_local! {
    static THUMB_FILL_TIMER: RefCell<Timer> = RefCell::new(Timer::default());
}

/// 网格同步上下文：滚动 / 过滤 / 库重载共用的唯一刷新入口（D43）。
///
/// 持有壳层刷新所需的全部共享引用，避免每个回调各自捕获一组 Rc。
pub(crate) struct GridCtx {
    ui: slint::Weak<AppWindow>,
    vm: Rc<RefCell<LibraryGridVm>>,
    tiles: Rc<VecModel<TileData>>,
    thumbs: ThumbSource,
    cache: Rc<RefCell<ThumbCache>>,
}

impl GridCtx {
    pub(crate) fn new(
        ui: slint::Weak<AppWindow>,
        vm: Rc<RefCell<LibraryGridVm>>,
        tiles: Rc<VecModel<TileData>>,
        thumbs: ThumbSource,
        cache: Rc<RefCell<ThumbCache>>,
    ) -> Self {
        Self {
            ui,
            vm,
            tiles,
            thumbs,
            cache,
        }
    }

    /// 全量刷新：求可见区间 → ensure_window → 预算化装载 → set_vec →
    /// 窗外显式驱逐 → 回填内容总高；仍有缺口则排填充 pass。
    pub(crate) fn sync(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let viewport_height = viewport_height_of(&ui);
        let scroll_top = -ui.get_content_y();

        let built = {
            let mut vm = self.vm.borrow_mut();
            let (first, end) = vm.visible_range(scroll_top, viewport_height);
            vm.ensure_window(first, end.saturating_sub(first));
            let mut cache = self.cache.borrow_mut();
            let built = build_rows(&vm, &self.thumbs, &mut cache, first, end);
            cache.retain_window(&built.window);
            built
        };

        self.tiles.set_vec(built.rows);
        ui.set_content_height(self.vm.borrow().content_height());

        if built.missing > 0 {
            self.schedule_fill();
        }
    }

    /// 填充 pass：只补当前窗口内仍缺的缩略图，定向 set_row_data，
    /// 不整表重建模型（避免 set_vec 让全部瓦片重新挂纹理）。
    fn fill_pass(self: &Rc<Self>) {
        let Some(ui) = self.ui.upgrade() else {
            return;
        };
        let viewport_height = viewport_height_of(&ui);
        let scroll_top = -ui.get_content_y();

        let built = {
            let vm = self.vm.borrow_mut();
            let (first, end) = vm.visible_range(scroll_top, viewport_height);
            let mut cache = self.cache.borrow_mut();
            let built = build_rows(&vm, &self.thumbs, &mut cache, first, end);
            cache.retain_window(&built.window);
            built
        };

        if built.missing == 0 {
            // 补齐（含负缓存收敛）：停表。
            return;
        }

        let row_count = self.tiles.row_count();
        for (row, tile) in &built.updates {
            if *row < row_count {
                self.tiles.set_row_data(*row, tile.clone());
            }
        }
        self.schedule_fill();
    }

    /// 起一个单发 Timer 在下一轮事件循环续跑填充。每次 sync 都会重排，
    /// 滚动/切分类自然打断旧的填充序列（窗口已变，下一轮用新窗口重算）。
    fn schedule_fill(self: &Rc<Self>) {
        let grid = Rc::clone(self);
        THUMB_FILL_TIMER.with(|slot| {
            slot.borrow().start(
                TimerMode::SingleShot,
                Duration::from_millis(FILL_INTERVAL_MS),
                move || grid.fill_pass(),
            );
        });
    }
}

/// 一次 build_rows 的产物。
struct WindowBuild {
    /// 全窗口行（sync 用 set_vec 整体铺入）。
    rows: Vec<TileData>,
    /// 当前物化窗口的资产 id 集合（窗外显式驱逐的依据）。
    window: HashSet<u32>,
    /// 本 pass 新装出**真图**的行：fill_pass 用它做 set_row_data 定向更新。
    /// 负缓存（文件缺失）不在此列——视觉不变，无需更新行。
    updates: Vec<(usize, TileData)>,
    /// 预算用尽仍未装载的行数：>0 时由填充 Timer 续跑。
    missing: usize,
}

/// 物化 [first, end) 的行数据，预算内同步装载缺失缩略图。
///
/// 预算纪律：命中缓存直接复用；缺项且预算未耗尽 → 装载（成功入缓存，失败以
/// Image::default() 负缓存防重试）；缺项且预算耗尽 → 占位并计入 missing。
fn build_rows(
    vm: &LibraryGridVm,
    thumbs: &ThumbSource,
    cache: &mut ThumbCache,
    first: usize,
    end: usize,
) -> WindowBuild {
    let thumbs_guard = thumbs.borrow();
    let card_provider = thumbs_guard.as_ref().map(ResolverCardProvider::new);
    let mut budget = THUMB_LOAD_BUDGET;
    let mut window = HashSet::new();
    let mut updates = Vec::new();
    let mut missing = 0usize;

    let rows = (first..end)
        .map(|i| {
            let r = vm.rect_of(i);
            let id = vm.id_at(i);
            window.insert(id.0);
            let kind = vm.kind_at(i);
            let card = card_provider
                .as_ref()
                .map(|provider| provider.card_data(kind, id))
                .unwrap_or(TileCardData {
                    kind,
                    preview: String::new(),
                });

            let mut loaded_real = false;
            let thumb = match cache.get(id.0) {
                Some(img) => img,
                None if budget > 0 => {
                    budget -= 1;
                    match load_display_thumb(thumbs_guard.as_ref(), id) {
                        Some(img) => {
                            loaded_real = true;
                            cache.put(id.0, img.clone());
                            img
                        }
                        None => {
                            // 负缓存：文件缺失的缩略图不再每帧重试读盘（库重载时 clear）。
                            cache.put(id.0, slint::Image::default());
                            slint::Image::default()
                        }
                    }
                }
                None => {
                    missing += 1;
                    slint::Image::default()
                }
            };

            let tile = TileData {
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                asset_id: id.0 as i32,
                // 角标显示素材文件名；索引查不到名字（迟到消息/演示库）才回落内部 #id。
                label: match vm.name_at(i) {
                    "" => format!("#{}", id.0),
                    name => name.to_string(),
                }
                .into(),
                color: slint::Color::from_argb_encoded(PALETTE[(id.0 % 8) as usize]),
                thumb,
                kind: ui_enums::card_kind(kind),
                preview: card.preview.into(),
            };
            if loaded_real {
                updates.push((i, tile.clone()));
            }
            tile
        })
        .collect();

    // 注：rows 在 fill_pass 里用不到（只消费 updates/missing/window）；
    // 保留同一构建函数是为了让 sync 与 fill 的窗口语义完全一致。
    WindowBuild {
        rows,
        window,
        updates,
        missing,
    }
}

/// 显示装载单张缩略图：只读已派生的浏览缩略图文件并解码为 Image。
/// 与旧实现同一装载路径（Image::load_from_path，path+mtime 键，Slint 自带
/// 5MB 解码 LRU 仍参与暖命中）；差异只在调用节奏从「一次全量」变成「预算渐进」。
fn load_display_thumb(resolver: Option<&RealAssetResolver>, id: AssetId) -> Option<slint::Image> {
    resolver?
        .thumbnail_path(id)
        .and_then(|path| slint::Image::load_from_path(&path).ok())
}

/// 视口高度：Slint 首帧尚未布局时为 0，回退到窗口预设高度。
fn viewport_height_of(ui: &AppWindow) -> f32 {
    let measured = ui.get_viewport_height();
    if measured > 1.0 {
        measured
    } else {
        700.0
    }
}

#[cfg(test)]
mod thumb_cache_spec {
    use super::ThumbCache;
    use std::collections::HashSet;

    #[test]
    fn capacity_evicts_least_recently_used() {
        let mut cache = ThumbCache::new(2);
        cache.put(1, slint::Image::default());
        cache.put(2, slint::Image::default());
        // 触碰 1 → 2 变为 LRU 尾；放 3 时被驱逐。
        assert!(cache.get(1).is_some());
        cache.put(3, slint::Image::default());
        assert!(cache.get(1).is_some());
        assert!(cache.get(2).is_none());
        assert!(cache.get(3).is_some());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn retain_window_drops_everything_outside_window() {
        let mut cache = ThumbCache::new(16);
        for id in 0..8u32 {
            cache.put(id, slint::Image::default());
        }
        let window: HashSet<u32> = [2u32, 3, 4].into_iter().collect();
        cache.retain_window(&window);
        for id in 0..8u32 {
            assert_eq!(
                cache.get(id).is_some(),
                window.contains(&id),
                "id={id} 的驻留状态与窗口不一致"
            );
        }
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn retain_window_keeps_everything_when_window_covers_all() {
        let mut cache = ThumbCache::new(16);
        for id in 0..8u32 {
            cache.put(id, slint::Image::default());
        }
        let window: HashSet<u32> = (0..8u32).collect();
        cache.retain_window(&window);
        assert_eq!(cache.len(), 8);
    }

    #[test]
    fn negative_entry_behaves_like_a_resident_thumb() {
        // 文件缺失时以 Image::default() 入缓存：get 命中即不再触发装载，
        // 与 D43 负缓存语义一致（库重载路径 clear 后才会重试）。
        let mut cache = ThumbCache::new(8);
        cache.put(7, slint::Image::default());
        assert_eq!(cache.len(), 1);
        assert!(cache.get(7).is_some());
    }
}
