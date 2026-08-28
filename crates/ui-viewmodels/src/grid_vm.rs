//! LibraryGridVm：过滤状态、可见窗口物化与 LRU 缩略图缓存、UI 事件队列。
//!
//! 内存模型（D10）：数字层（位图 + 全量 Rect 表）常驻；缩略图字节只进有界 LRU，
//! `ensure_window` 显式驱逐窗外条目——「可见窗口外零驻留」不依赖容量巧合。

use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;

use domain::{AssetId, AssetKind, Filter, Sorter};
use index::FacetIndex;
use lru::LruCache;

use crate::layout::{masonry_layout, Rect};
use crate::selection::{ContextMenuSpec, MenuItem, Modifiers, Selection};

/// VM → UI 事件。双击素材的语义止步于 OpenAsset；auto-send 属粘贴管线（M6）且默认关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmEvent {
    OpenAsset(AssetId),
    SelectionChanged(AssetId),
    /// 右键菜单请求（D48）：携带命中瓦片 id，内容由 `context_menu(id)` 取。
    ContextMenuRequested(AssetId),
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

/// 单次可见区间的条目上限：视口极高 + 瓦片极小时的渲染保护阀。
/// 超过此数的尾部条目本帧不物化，滚动会在下一帧继续补齐。
pub const MAX_VISIBLE: usize = 512;

/// 默认布局参数（壳层可经 `set_layout_params` 覆盖）。
const DEFAULT_CONTAINER_WIDTH: f32 = 984.0;
const DEFAULT_COLUMNS: u32 = 6;
const DEFAULT_GAP: f32 = 12.0;

/// 无真实媒体尺寸时的占位宽高比：由 id 确定性导出（PRD 指定公式）。
///
/// 真实尺寸经 [`LibraryGridVm::set_aspects`] 注入（来源 meta.db 的 width/height）；
/// 缺尺寸的条目（未派生缩略图 / 抽帧失败的视频）才回落到这里，
/// 保证布局始终确定且不出现 0 高瓦片。
fn fallback_aspect(id: AssetId) -> f32 {
    ((id.0 % 7) + 1) as f32 / ((id.0 % 5) as f32 + 1.0)
}

pub struct LibraryGridVm {
    index: FacetIndex,
    sorter: Sorter,
    /// 当前过滤+排序后的 id 序列（数字层常驻）。
    ids: Vec<AssetId>,
    /// 全量预计算 rect 表，与 ids 同索引（O(1) 跳转的根基）。
    rects: Vec<Rect>,
    /// 真实宽高比（w/h），按 AssetId 索引；缺项回落 [`fallback_aspect`]。
    aspects: HashMap<AssetId, f32>,
    /// 当前 rect 表中最高瓦片的高度：可见区间回退扫描的上界依据。
    max_item_height: f32,
    content_height: f32,
    container_width: f32,
    columns: u32,
    gap: f32,
    cache: LruCache<AssetId, Vec<u8>>,
    provider: Box<dyn ThumbnailProvider>,
    events: VecDeque<VmEvent>,
    /// D47 选区状态机（集合 + 锚点 + 多选模式）。视图序直接借 `ids`：
    /// `self.selection.f(.., &self.ids)` 是互不相交字段借用，无冲突。
    selection: Selection,
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
            aspects: HashMap::new(),
            max_item_height: 0.0,
            content_height: 0.0,
            container_width: DEFAULT_CONTAINER_WIDTH,
            columns: DEFAULT_COLUMNS,
            gap: DEFAULT_GAP,
            cache: LruCache::new(capacity),
            provider: Box::new(NullProvider),
            events: VecDeque::new(),
            selection: Selection::default(),
        };
        vm.set_filter(&Filter::All);
        vm
    }

    /// 注入缩略图提供者（装配口：内存守卫测试经此观测 load 行为；M7 接 worker）。
    pub fn set_provider(&mut self, provider: Box<dyn ThumbnailProvider>) {
        self.provider = provider;
    }

    /// 注入真实宽高比表并重算布局（来源：meta.db 的 width/height）。
    ///
    /// 缺项条目继续用 [`fallback_aspect`]，因此部分素材尚无尺寸时布局依然完整。
    pub fn set_aspects(&mut self, aspects: HashMap<AssetId, f32>) {
        self.aspects = aspects;
        self.rebuild_rects();
    }

    /// 调整布局参数并重算 Rect 表（容器几何变化时由壳层调用）。
    pub fn set_layout_params(&mut self, container_width: f32, columns: u32, gap: f32) {
        self.container_width = container_width;
        self.columns = columns.max(1);
        self.gap = gap;
        self.rebuild_rects();
    }

    /// 过滤变更：位图求值 → SoA 直排 → 重建 id 序列与 Rect 表；缓存整体失效（序列已变）。
    ///
    /// 不物化 Asset（索引是 SoA 行表），排序在索引层按行读键完成（D3 百万级纪律）。
    pub fn set_filter(&mut self, f: &Filter) {
        let bitmap = self.index.evaluate(f);
        let ordered = self.index.sorted_ids(&self.sorter, &bitmap);
        self.ids = ordered.into_iter().map(AssetId).collect();
        // 掉出视图的选中项失效（操作条/子命令不得对隐形素材动手），模式保留。
        self.selection.prune_to_view(&self.ids);
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

    /// 序列第 i 项的显示名（真实库里是素材文件名）。
    ///
    /// 瓦片角标从前只显示内部 `#id`，对用户没有语义；渲染端需要一个可读名称。
    /// 索引里查不到该 id（迟到消息/合成库）时返回空串，由壳层回落 `#id`。
    pub fn name_at(&self, i: usize) -> &str {
        self.ids
            .get(i)
            .and_then(|id| self.index.name(id.0))
            .unwrap_or("")
    }

    /// 序列第 i 项的素材类别（卡片渲染按类别切表现；未知回落 Other）。
    pub fn kind_at(&self, i: usize) -> AssetKind {
        self.ids
            .get(i)
            .map(|id| self.index.kind(id.0))
            .unwrap_or(AssetKind::Other)
    }

    /// 视口内容总高（最后一个 rect 的底边）。
    pub fn content_height(&self) -> f32 {
        self.content_height
    }

    /// 视口 y 区间 → 需要渲染的索引区间 `[start, end)`。
    ///
    /// 为什么不是「按 content_y 二分求单个首项」：masonry 是多列布局，`rect.y`
    /// 只单调不减而非严格递增（首行 N 列的 y 全为 0）。对 y 做二分取「最后一个
    /// `y <= top`」会直接跳到该行最右列，把同行左侧条目整排漏掉——表现就是
    /// 首屏第一行不可见。这里改为区间求交：凡与 `[top, top + height)` 有交叠的
    /// rect 全部落在返回区间内。
    ///
    /// 复杂度：两次二分 + 一段有界回退（回退项的 y 都落在宽度为
    /// `max_item_height` 的一条带内，与总量无关）。
    pub fn visible_range(&self, top: f32, viewport_height: f32) -> (usize, usize) {
        if self.rects.is_empty() {
            return (0, 0);
        }
        let top = if top.is_finite() { top.max(0.0) } else { 0.0 };
        let height = if viewport_height.is_finite() && viewport_height > 0.0 {
            viewport_height
        } else {
            0.0
        };

        // 回退起点：y 单调不减，故 y ≤ top − max_h 的条目其底边必然不到 top。
        let mut start = self.lower_bound_y(top);
        while start > 0 && self.rects[start - 1].y + self.max_item_height > top {
            start -= 1;
        }

        // 结束点：第一个 y ≥ 视口底边的条目，其后全部不可见。
        // 高度未知（首帧布局前）时至少给一项，避免 tiles 恒空。
        let end = self
            .lower_bound_y(top + height)
            .max((start + 1).min(self.rects.len()));
        let end = end.min(start + MAX_VISIBLE);
        (start, end)
    }

    /// 第一个 `rect.y >= y` 的下标（rect.y 单调不减，标准 lower_bound）。
    fn lower_bound_y(&self, y: f32) -> usize {
        let (mut lo, mut hi) = (0usize, self.rects.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.rects[mid].y < y {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
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
    ///
    /// 红线 A（D47）：多选模式期间双击**完全无操作**——模式存在的意义就是
    /// 屏蔽上框链路；事件队列不产生任何痕迹。
    pub fn double_click(&mut self, id: AssetId) {
        if self.selection.is_multi() {
            return;
        }
        if !self.ids.contains(&id) {
            return;
        }
        self.events.push_back(VmEvent::SelectionChanged(id));
        self.events.push_back(VmEvent::OpenAsset(id));
    }

    /// 单击（含修饰键）入口：壳层从 `pointer-event` 的 Up 事件映射
    /// [`Modifiers`] 后调用。选区变化时发 SelectionChanged。
    ///
    /// 语义表见 `selection::Selection::on_click`；关键红线：
    /// - 带任何修饰键的点击**绝不**发 OpenAsset（上框只属于无修饰双击）；
    /// - 常态无修饰单击无操作（与改造前行为逐字一致）。
    pub fn single_click(&mut self, id: AssetId, mods: Modifiers) {
        if self.selection.on_click(id, mods, &self.ids) {
            self.events.push_back(VmEvent::SelectionChanged(id));
        }
    }

    /// 右键命中瓦片：只记「菜单请求」事件，内容由壳层 `context_menu` 取。
    /// id 不在视图时忽略（与 double_click 同一容错）。
    pub fn right_click(&mut self, id: AssetId) {
        if !self.ids.contains(&id) {
            return;
        }
        self.events.push_back(VmEvent::ContextMenuRequested(id));
    }

    /// 进入多选模式（顶栏「选择」按钮，R8）。
    pub fn enter_multi(&mut self) {
        self.selection.enter_multi();
    }

    /// 退出多选模式 = 清空选区恢复常态（R9；Esc / 操作条「取消」同路）。
    pub fn exit_multi(&mut self) {
        self.selection.exit_multi();
    }

    pub fn multi_mode(&self) -> bool {
        self.selection.is_multi()
    }

    pub fn is_selected(&self, id: AssetId) -> bool {
        self.selection.contains(id)
    }

    /// 操作条「已选 N 张」数据源。
    pub fn selected_count(&self) -> usize {
        self.selection.len()
    }

    /// 选区内容按视图序输出（子命令派发与菜单目标都从这里取）。
    pub fn selection_ids(&self) -> Vec<AssetId> {
        self.selection.ids_in_view(&self.ids)
    }

    /// Ctrl+A / 操作条「全选」：选中当前视图全部条目（R7）。
    pub fn select_all_visible(&mut self) {
        let changed = if self.ids.is_empty() {
            false
        } else {
            let before = self.selection.len();
            for &id in &self.ids {
                if !self.selection.contains(id) {
                    // 经状态机逐枚加入会挪锚点；全选后锚点落在最后一枚即可。
                    self.selection.on_click(
                        id,
                        Modifiers {
                            ctrl: true,
                            shift: false,
                        },
                        &self.ids,
                    );
                }
            }
            before != self.selection.len()
        };
        if changed {
            // 批量变化用最后一枚作事件载体（壳层只拿它做重绘信号）。
            if let Some(&last) = self.ids.last() {
                self.events.push_back(VmEvent::SelectionChanged(last));
            }
        }
    }

    /// 右键菜单数据（D48/R10/R11）：五项固定，目标集 = 命中瓦片在选区内
    /// 则整份选区，否则收窄到命中瓦片本身。
    pub fn context_menu(&self, hit: AssetId) -> ContextMenuSpec {
        let targets = if self.selection.contains(hit) {
            self.selection.ids_in_view(&self.ids)
        } else {
            vec![hit]
        };
        ContextMenuSpec {
            targets,
            items: crate::selection::MENU_ITEMS
                .iter()
                .map(|(action, label)| MenuItem {
                    action: *action,
                    label,
                    enabled: true,
                })
                .collect(),
        }
    }

    /// 取走全部待处理事件（取走即清空）。
    pub fn take_events(&mut self) -> Vec<VmEvent> {
        self.events.drain(..).collect()
    }

    fn rebuild_rects(&mut self) {
        let aspects: Vec<f32> = self
            .ids
            .iter()
            .map(|&id| {
                self.aspects
                    .get(&id)
                    .copied()
                    .filter(|a| a.is_finite() && *a > 0.0)
                    .unwrap_or_else(|| fallback_aspect(id))
            })
            .collect();
        self.rects = masonry_layout(self.container_width, self.columns, self.gap, &aspects);
        // 内容总高 = 所有列的最低底边。masonry 下最后一项不一定落在最深的列，
        // 取 `rects.last()` 会漏掉尾部差值，滚动到底时最后一行被裁掉。
        let (height, max_h) = self.rects.iter().fold((0.0f32, 0.0f32), |(bot, tall), r| {
            ((r.y + r.h).max(bot), r.h.max(tall))
        });
        self.content_height = height;
        self.max_item_height = max_h;
    }
}
