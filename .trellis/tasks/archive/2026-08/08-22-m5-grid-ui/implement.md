# Implement — M5 UI 壳与虚拟化网格

## 顺序清单(Red→Green→Refactor)

1. **布局数学先行(风险最高,先 spike)**
   - [ ] 红灯:`grid_layout_math_variable_aspect_no_overlap`(tests/window_spec.rs 或独立 layout_spec.rs):构造混合宽高比输入,断言无重叠/容器内/确定性。
   - [ ] 绿灯:src/layout.rs masonry 实现。
   - [ ] criterion benches/layout.rs @10k;若单次布局 >16ms(debug)/>2ms(release) → 触发回退预案评估并记录。
2. **VM 骨架**
   - [ ] 红灯:`filter_panel_changes_propagate_to_viewmodel_query`:FacetIndex 造 100 条带标签资产,set_filter(HasTag) 后 total() 与 id 序列正确。
   - [ ] 红灯:`selection_double_click_emits_open_asset_event`:double_click → take_events 含 OpenAsset(id)。
   - [ ] 绿灯:grid_vm.rs 基础实现(filter/sorter/events)。
3. **虚拟化窗口与内存守卫**
   - [ ] 红灯:`viewmodel_window_of_100k_model_loads_only_visible_slice`:100k 合成资产(aspects 确定性生成),stub provider 记录 load 调用;ensure_window(可见 ~20 个)后 visible_cache_ids 仅含窗口+overscan,窗外零驻留;再跳到远处窗口,旧窗口条目被驱逐且 stub 未被重复加载超过 LRU 容量。
   - [ ] 绿灯:ensure_window + lru 接入。
4. **帧预算**
   - [ ] 红灯→绿灯:`scroll_jump_10k_items_keeps_frame_budget`:从顶部跳到第 10000 项(rect O(1)+窗口物化),耗时 < 宽裕上界(50ms,注释 best-effort;debug 档跑)。
5. **app-ui 接线**
   - [ ] appwindow.slint:Flickable + 绝对定位瓦片模型 + 回调(scroll-changed/double-clicked);main.rs 装配 VM(无库时用合成演示数据)+ 回调桥接;.slint 保持哑渲染。
   - [ ] `cargo build -p app-ui` 通过;deps_guard 测试保持绿。环境允许则短暂启动冒烟(启动 3s 存活即算),否则记录待人工验收。
6. **收尾验证**

## 验证命令(CI 同序)

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 审查门

- 门 1(步骤 1 后):布局数学不变量全绿 + criterion 数字落档;超预算则按 PRD 启动回退评审(等宽降级),不得静默放宽断言。
- 门 2(步骤 4 后):内存守卫测试必须真的用 100k 数据与有界 LRU(容量注入小值),禁止用「恰好不超」的巧合断言。
- 门 3(全部后):三命令全绿;既有 34 测试零改动通过。

## 回滚点

- 步骤 1 独立可合(layout 无 VM 依赖);
- app-ui 接线失败可回退到 M0 空窗(main.rs 恢复最小形态),不影响 ui-viewmodels 成果。

## 明确不做(防 scope creep)

- slintcn 组件源码引进(网络不可用时推迟)、视频 scrub、真实渲染帧率自动化、worker 异步取图接线、粘贴管线(M6)。
