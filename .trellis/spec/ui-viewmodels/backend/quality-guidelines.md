# Quality Guidelines — ui-viewmodels

## 红线

1. **禁 slint 依赖**：VM 全量 `cargo test` 可测；`.slint` 只做冒烟级验证。
2. **内存守卫**（D10）：VM 持有的缩略图/资产数据必须按可见窗口分页加载——100k 浏览 ≤250MB 的验收线由本层保证。
3. 依赖方向：可依赖 domain/index/store/library/pipeline；**禁止**依赖 media/phash/worker 实现 crate。

## 测试要求

- 每个 VM 公共方法全覆盖单测（TDD 第一原则的落点）。
- 事件传播测试：filter panel 变更 → VM 查询刷新（`filter_panel_changes_propagate_to_viewmodel_query`）；双击选择 → open 事件（`selection_double_click_emits_open_asset_event`）。

## Code Review 清单

- [ ] 新 VM 字段是否引入了大对象常驻（缩略图缓存应走 LRU）？
- [ ] Slint 类型是否渗入签名？
