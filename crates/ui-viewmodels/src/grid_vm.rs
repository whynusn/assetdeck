//! LibraryGridVm：过滤状态、可见窗口物化与 LRU 缩略图缓存、UI 事件队列。
//!
//! 内存模型（D10）：数字层（位图 + 全量 Rect 表）常驻；缩略图字节只进有界 LRU，
//! `ensure_window` 显式驱逐窗外条目——「可见窗口外零驻留」不依赖容量巧合。

use std::collections::{HashSet, VecDeque};
use std::num::NonZeroUsize;

use domain::{Asset, AssetId, Filter, Sorter};
use index::FacetIndex;
use lru::LruCache;

use crate::layout::{masonry_layout, Rect};

/// VM → UI 事件。双击素材的语义止步于 OpenAsset；auto-send 属粘贴管线（M6）且默认关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmEvent {
    OpenAsset(AssetId),
    SelectionChanged(AssetId),
}

/// 缩略图字节提供者。M5 用确定性 stub 验证缓存策略；M7 接 worker 进程池异步取图。
pub trait ThumbnailProvider {
    fn load(&self, id: AssetId) -> Option<Vec<u8>>;
}

struct NullProvider;

impl ThumbnailProvider for NullProvider {
    fn load(&self, _id: AssetId) -> Option<Vec<u8>> {
        None
    }
}

/// 窗口两侧过扫描条目数：邻近滚动免重复加载的缓冲带。
pub const OVERSCAN: usize = 20;

/// 默认布局参数（壳层可经 `set_layout_params` 覆盖）。
const DEFAULT_CONTAINER_WIDTH: f32 = 984.0;
const DEFAULT_COLUMNS: u32 = 6;
const DEFAULT_GAP: f32 = 12.0;

/// M5 占位：宽高比由 id 确定性导出（PRD 指定公式）；M7 换媒体元数据查询。
fn aspect_for(id: AssetId) -> f32 {
    ((id.0 % 7) + 1) as f32 / ((id.0 % 5) as f32 + 1.0)
}

pub struct LibraryGridVm {
    index: FacetIndex,
    sorter: Sorter,
    /// 当前过滤+排序后的 id 序列（数字层常驻）。
    ids: Vec<AssetId>,
    /// 全量预计算 rect 表，与 ids 同索引（O(1) 跳转的根基）。
    rects: Vec<Rect>,
    content_height: f32,
    container_width: f32,
    columns: u32,
    gap: f32,
    cache: LruCache<AssetId, Vec<u8>>,
    provider: Box<dyn ThumbnailProvider>,
    events: VecDeque<VmEvent>,
}

impl LibraryGridVm {
    /// 构造即全库视图（Filter::All），布局参数取默认值。
    pub fn new(index: FacetIndex, sorter: Sorter, thumb_capacity: usize) -> Self {
        // 容量注入为 0 时钳为 1，保证 LRU 构造合法
        let capacity = NonZeroUsize::new(thumb_capacity.max(1)).unwrap_or(NonZeroUsize::MIN);
        let mut vm = Self {
            index,
            sorter,
            ids: Vec::new(),
            rects: Vec::new(),
            content_height: 0.0,
            container_width: DEFAULT_CONTAINER_WIDTH,
            columns: DEFAULT_COLUMNS,
            gap: DEFAULT_GAP,
            cache: LruCache::new(capacity),
            provider: Box::new(NullProvider),
            events: VecDeque::new(),
        };
        vm.set_filter(&Filter::All);
        vm
    }

    /// 注入缩略图提供者（装配口：内存守卫测试经此观测 load 行为；M7 接 worker）。
    pub fn set_provider(&mut self, provider: Box<dyn ThumbnailProvider>) {
        self.provider = provider;
    }

    /// 调整布局参数并重算 Rect 表（容器几何变化时由壳层调用）。
    pub fn set_layout_params(&mut self, container_width: f32, columns: u32, gap: f32) {
        self.container_width = container_width;
        self.columns = columns.max(1);
        self.gap = gap;
        self.rebuild_rects();
    }

    /// 过滤变更：位图求值 → 排序 → 重建 id 序列与 Rect 表；缓存整体失效（序列已变）。
    pub fn set_filter(&mut self, f: &Filter) {
        let bitmap = self.index.evaluate(f);
        let mut assets: Vec<Asset> = bitmap
            .iter()
            .filter_map(|id| self.index.asset(id).cloned())
            .collect();
        self.sorter.sort_assets(&mut assets);
        self.ids = assets.into_iter().map(|a| a.id).collect();
        self.rebuild_rects();
        self.cache.clear();
    }

    pub fn total(&self) -> usize {
        self.ids.len()
    }

    /// O(1) rect 索引：全量预计算表支持任意距离跳转（D3）。越界 panic 属调用方契约违背。
    pub fn rect_of(&self, i: usize) -> Rect {
        self.rects[i]
    }

    /// 序列第 i 项的资产 id（瓦片回调携带用）。
    pub fn id_at(&self, i: usize) -> AssetId {
        self.ids[i]
    }

    /// 视口内容总高（最后一个 rect 的底边）。
    pub fn content_height(&self) -> f32 {
        self.content_height
    }

    /// 物化 [first, first+count) ± [`OVERSCAN`] 进 LRU，并驱逐窗外驻留条目。
    ///
    /// 内存守卫（D10）：窗外零缩略图驻留由显式驱逐保证，LRU 容量仅作兜底上界。
    pub fn ensure_window(&mut self, first: usize, count: usize) {
        if self.ids.is_empty() {
            return;
        }
        let total = self.ids.len();
        let first = first.min(total - 1);
        let end = (first + count).min(total);
        let win_start = first.saturating_sub(OVERSCAN);
        let win_end = (end + OVERSCAN).min(total);

        // 物化缺失项：命中缓存则跳过（不重复触发 provider.load）
        for &id in &self.ids[win_start..win_end] {
            if self.cache.peek(&id).is_none() {
                if let Some(bytes) = self.provider.load(id) {
                    self.cache.put(id, bytes);
                }
            }
        }

        // 窗外显式驱逐
        let window: HashSet<AssetId> = self.ids[win_start..win_end].iter().copied().collect();
        let stale: Vec<AssetId> = self
            .cache
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| !window.contains(k))
            .collect();
        for id in stale {
            self.cache.pop(&id);
        }
    }

    /// 测试钩子：当前缩略图驻留集（升序输出便于确定性断言，仿 library::set_paused 先例）。
    pub fn visible_cache_ids(&self) -> Vec<AssetId> {
        let mut ids: Vec<AssetId> = self.cache.iter().map(|(k, _)| *k).collect();
        ids.sort_unstable();
        ids
    }

    /// 双击：选中并发出打开事件。id 不在当前视图时忽略（迟到/乱序消息容错）。
    pub fn double_click(&mut self, id: AssetId) {
        if !self.ids.contains(&id) {
            return;
        }
        self.events.push_back(VmEvent::SelectionChanged(id));
        self.events.push_back(VmEvent::OpenAsset(id));
    }

    /// 取走全部待处理事件（取走即清空）。
    pub fn take_events(&mut self) -> Vec<VmEvent> {
        self.events.drain(..).collect()
    }

    fn rebuild_rects(&mut self) {
        let aspects: Vec<f32> = self.ids.iter().map(|&id| aspect_for(id)).collect();
        self.rects = masonry_layout(self.container_width, self.columns, self.gap, &aspects);
        self.content_height = self.rects.last().map_or(0.0, |r| r.y + r.h);
    }
}
