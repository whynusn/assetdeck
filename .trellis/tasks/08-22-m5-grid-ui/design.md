# Design — M5 UI 壳与虚拟化网格

## 边界

```
crates/ui-viewmodels/
├── Cargo.toml    # deps: domain/index/store/library + lru;dev-deps: tempfile, criterion
├── src/
│   ├── lib.rs        # 导出
│   ├── layout.rs     # 瀑布流布局数学(纯函数,零 IO)
│   └── grid_vm.rs    # LibraryGridVm:过滤状态/可见窗口物化/LRU 缩略图缓存/事件
├── benches/layout.rs         # criterion(布局数学独立基准)
└── tests/
    ├── window_spec.rs        # 内存守卫 + 滚动跳转帧预算
    └── interaction_spec.rs   # 双击事件 + 过滤传播

crates/app-ui/
├── ui/appwindow.slint    # 哑渲染:Flickable + 绝对定位瓦片;属性/回调绑定 VM
├── components/           # 自写最小组件(tile/toolbar);slintcn 视网络情况另议
└── src/main.rs           # VM 装配 + 滚动回调回填 Slint 属性
```

## 核心契约

### layout.rs(纯函数)

```rust
pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }
/// 固定列数 masonry:每项缩放到列宽保持宽高比,放入当前最短列。
pub fn masonry_layout(container_width: f32, columns: u32, gap: f32, aspects: &[f32]) -> Vec<Rect>;
```

不变量(测试断言):任意两 Rect 无重叠;全部满足 x≥0 且 x+w ≤ container_width+ε;输出顺序与输入一致(确定性)。criterion:`benches/layout.rs` @10k aspects。

### grid_vm.rs

```rust
pub enum VmEvent { OpenAsset(AssetId), SelectionChanged(AssetId) }
pub trait ThumbnailProvider { fn load(&self, id: AssetId) -> Option<Vec<u8>>; }   // M7 接 worker,M5 用 stub
pub struct LibraryGridVm { /* FacetIndex + Sorter + ids + Lru<AssetId, Vec<u8>> + events */ }
impl LibraryGridVm {
    pub fn new(index: FacetIndex, sorter: Sorter, thumb_capacity: usize) -> Self;
    pub fn set_filter(&mut self, f: &Filter);              // evaluate → 排序 → 重建 id 序列
    pub fn total(&self) -> usize;
    pub fn rect_of(&self, i: usize) -> Rect;               // 全量预计算 rect 表(O(1) 跳转)
    pub fn ensure_window(&mut self, first: usize, count: usize); // 物化 [first,first+count)+overscan 进 LRU,窗外驱逐
    pub fn visible_cache_ids(&self) -> Vec<AssetId>;       // 测试钩子:当前驻留集
    pub fn double_click(&mut self, id: AssetId);
    pub fn take_events(&mut self) -> Vec<VmEvent>;
}
```

### 内存模型(D10 论证)

- **常驻数字层**:FacetIndex(位图+元数据,复用 index crate)+ 全量 Rect 表(10 万条 ≈ 3.2MB,纯数字可接受;100 万 ≈ 32MB,M7 实测复核)。
- **有界物化层**:缩略图字节只进 LRU(capacity 注入),ensure_window 驱逐窗外条目——内存守卫测试锁定「窗口外零驻留」。
- 布局参数(columns/gap/row 高度语义)由 VM 构造时给定,Slint 层不参与计算。

### 滚动跳转帧预算(scroll_jump_10k_items_keeps_frame_budget)

跳转路径 = O(1) rect 索引 + 窗口切片 + ~20 张 stub 缩略图物化。断言上界取宽裕值(如 50ms,注释标 best-effort;真实 GPU 帧率属手工清单)。

## app-ui 接线形态

- `.slint` 只声明:属性(total/content_y/tiles model)、回调(scroll-changed/double-clicked/filter-changed)。
- main.rs:构建合成或空库的 VM → 绑定回调把 content_y 换算成 first/count → ensure_window → 刷新 tiles 模型(VecModel<tile struct>)。
- 双击回调 → vm.double_click(id);事件回流仅 println 占位(M6 接管线)。
- 过滤面板 v1 最小化:分类下拉/全部按钮 → set_filter。

## 权衡记录

| 决策 | 备选 | 理由 |
|---|---|---|
| 固定列数 masonry | justified rows / 等宽网格 | 变宽高比需求 + 数学简单;等宽留作回退 |
| 全量预计算 Rect 表 | 惰性增量布局 | O(1) 跳转、实现简单;内存代价已论证可接受 |
| lru crate | 手写 LRU | 成熟稳定;MIT/Apache 过 deny 清单 |
| stub 缩略图 provider | 直接接 worker 池 | 内存守卫测的是缓存策略;异步接线复杂度推给 M7 |

## 兼容与回滚

- ui-viewmodels 为新增代码;app-ui 仅改 main.rs/appwindow.slint,deps_guard 不动。
- 布局 spike 若超帧预算 → 按 PRD 回退预案降级等宽(layout 函数签名不变,内部换均匀尺寸)。

## 测试钩子先例

- `visible_cache_ids()` 暴露驻留集(仿 library::set_paused / worker::worker_pids 的确定性测试模式)。
