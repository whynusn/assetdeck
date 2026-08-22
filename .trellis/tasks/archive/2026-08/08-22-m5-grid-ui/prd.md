# PRD — M5 UI 壳与虚拟化网格

> 依据:TDD_PLAN M5 清单 + DECISIONS.md D3/D9/D10。关键路径最大风险项;五个自动化红灯测试全绿 + Slint 壳可启动即为验收。

## 需求(与 TDD_PLAN M5 一一对应)

1. `viewmodel_window_of_100k_model_loads_only_visible_slice` — 内存守卫:VM 对 10 万条模型只物化可见切片,可见窗口外零缩略图驻留(LRU 驱逐)。
2. `grid_layout_math_variable_aspect_no_overlap` — 变宽高比瀑布流布局数学:criterion 基准 + 无重叠不变量。
3. `scroll_jump_10k_items_keeps_frame_budget` — 从顶部跳到第 1 万项:布局重算 + 可见切片求取耗时在帧预算内(软件渲染近似,best-effort 宽裕上界)。
4. `selection_double_click_emits_open_asset_event` — 双击选择 → VM 发出 OpenAsset 事件。
5. `filter_panel_changes_propagate_to_viewmodel_query` — 过滤面板变更 → VM 查询结果集更新(走 FacetIndex 求值)。
6. app-ui 接线:Slint 壳展示 VM 驱动的网格(可见窗口切片),双击回调接通 VM 事件。

## 约束

- **TDD 第一原则**:业务逻辑全在纯 Rust(ui-viewmodels),`.slint` 只做哑渲染。app-ui 禁 slint 依赖渗入 ui-viewmodels(反向亦然)。
- ui-viewmodels 依赖 domain/index/store/library(+pipeline 可选);**禁止** media/phash/worker(deps_guard 同款纪律)。
- 内存红线 D10:缩略图缓存必须 LRU 有界;位图/索引常驻部分复用 index crate。
- windows-gnu 工具链可编译;clippy -D warnings 全绿。

## 范围外(明确不做)

- slintcn 组件源码引进与冒烟实例化测试:**若网络不可用则推迟**,不阻塞里程碑(记录到任务 notes);优先用自写最小组件。
- 视频悬停 scrub(D6 边界)、真实 GPU 渲染帧率自动化(TDD_PLAN 第六节诚实清单)。
- 手工验收清单执行:120fps 体感/IME/DPI —— 留人工项,TDD_PLAN 已标注。
- worker 缩略图管线接线(异步取图):M5 用确定性 stub 缩略图提供者验证缓存策略;真接线随 M7 闭环做。

## 回退预案(TDD_PLAN 第十节)

变宽高比布局 spike 达不到帧预算 → 降级等宽网格(布局数学简化一个数量级)。判定点:步骤 2 的 criterion 结果。

## 验收标准

- 五个自动化测试全绿;criterion 基准数据落档(benches)。
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` 保持绿。
- app-ui 窗口能以 VM 数据启动显示(手工运行确认一次即可)。
