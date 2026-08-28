# Implement — 素材 CRUD：回收站 + 多选模式 + 右键菜单

> 顺序纪律：每阶段先写红灯测试再实现（TDD_PLAN.md 节奏）；阶段间可独立回滚。
> 验证命令统一：`cargo test -p <crate>`（局部）→ 阶段末 `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` + `cargo fmt --all --check`。

## 阶段 0 — Spike（先做，结论回写本文件）

- [x] S1 Slint 1.17 `PointerEvent.modifiers` 在 `pointer-event` 回调（release/press）能否拿到 Ctrl/Shift；`PointerEventKind.right_down`（或等价）能否区分右键。**产出**：把结论写进 design §3.1（可行→按 R7 修饰键做；不可行→走降级路径并记录边界）。失败成本：一个最小 .slint 样本 + 运行。→ **可行（源码查证 1.17.1，结论+边界见 design §3.1 S1）**
- [x] S2 确认 app-ui `deps_guard` 白名单不含 `library` crate → 库写操作必须走子进程（`sample-library.exe --cmd …`），与 D33 子进程模式对齐。→ **成立，见 design §3.1 S2**

## 阶段 1 — store：schema v4 + tombstone（包：store）

- [ ] 1.1 红灯：`migration_v3_to_v4_adds_deleted_column`；`soft_delete_then_restore`；`distinct_categories_excludes_deleted`；`for_each_asset_active_skips_deleted`；`search_excludes_deleted`（JOIN 过滤）；`rename_asset_reindexes_fts`（新名命中/旧名不中）；`set_category_narrow_update`。
- [ ] 1.2 实现：迁移 v4（ALTER + user_version）；`soft_delete_assets/restore_assets/is_deleted/deleted_uuids/set_category/rename_asset/for_each_asset_active`；`search()` JOIN 加 `deleted=0`。
- [ ] 验证：`cargo test -p store`。回滚点：仅 store 层，无下游。

## 阶段 2 — library：trash 目录操作（包：library）

- [ ] 2.1 红灯：`move_to_trash_relocates_objects_dir_and_marks`；`rename_failure_rolls_back_flag`；`restore_returns_object_dir`；`purge_clears_row_dir_and_thumbs`；`reconcile_fixes_drift`（两个方向各一）。
- [ ] 2.2 实现 `src/trash.rs`（move/restore/purge/empty/reconcile），`Library::open` 时跑 reconcile（仅当存在 deleted=1 行或 trash 非空，避免常态开销）。
- [ ] 2.3 派生侧过滤：`tools/derive-thumbs`、`sample-library` 导出改 `for_each_asset_active`；`sample-library` 新增 `--cmd trash|restore|purge|empty-trash|rename|move-category --library <root>` 子命令族（PROGRESS 行协议复用，供壳层 ChildTaskRunner 驱动）。
- [ ] 验证：`cargo test -p library` + `cargo run -p sample-library -- --cmd` 冒烟。回滚点：阶段 1+2 可整体 revert（UI 未接线）。

## 阶段 3 — index：deleted 位图（包：index）

- [ ] 3.1 红灯：`mark_deleted_hides_from_all_and_facets`；`evaluate_result_never_contains_deleted`；`unmark_restores_category_membership`。
- [ ] 3.2 `FacetIndex` 增 `deleted: RoaringBitmap` + mark/unmark；`catalog_loader::load_real_library` 把 `deleted_uuids` 映射到行号后 mark（启动一次性）。

## 阶段 4 — ui-viewmodels：选区状态机 + 菜单 VM（红线测试先行）

- [ ] 4.1 红灯（interaction_spec 或新 selection_spec）：Multi 模式任意点击序列 → 零 `VmEvent::OpenAsset`；Ctrl 点击切换选区不发 OpenAsset；Shift 范围按视图序；Ctrl+A 全选可见；exit 清空；常态无修饰点击 = 行为与今日完全一致（回归守卫）。
- [ ] 4.2 实现 `Selection` 状态机；`VmEvent` 增 `ContextMenuRequested { id, items }`/`SelectionChanged` 复用；操作条 VM 字段（selected_count）。
- [ ] 4.3 菜单五项 + 文案常量入 VM 层（CONTEXT.md 用语穷举测试：`context_menu_items_are_exactly_five`）。

## 阶段 5 — app-ui 壳层接线

- [ ] 5.1 瓦片 `pointer-event` 路径：修饰键判定 + 右键出菜单（菜单 Rectangle 浮层，坐标夹取；外层 dismiss TouchArea 扩围，见 appwindow.slint:807）。
- [ ] 5.2 顶栏「选择」按钮 + 底部操作条（全选/移动到分类/删除/取消）；Esc 退出。
- [ ] 5.3 动作派发：菜单/操作条动作 → `ChildTaskRunner` 起 `sample-library --cmd …`（阶段 2.3 子命令）→ 完成后 `grid.sync()` + 必要时库重载（删除/恢复后）；UI 先行即时反馈（本地 mark_deleted 隐藏）与子进程结果对齐。
- [ ] 5.4 回收站入口（侧栏系统分类项「回收站」，选中 = 专用 filter 展示 deleted 集合 + 操作条变体「恢复｜彻底删除｜清空」）。
- [ ] 5.5 属性弹窗（字段：尺寸/大小/导入时间/绝对路径）；重命名弹窗（LineEdit + 校验非空/无路径分隔）。
- [ ] 验证：`cargo test -p app-ui` + `--bench` D43 驻留守卫仍绿 + 手跑导入样例库点删一张确认回收站闭环。

## 阶段 6 — 收口

- [ ] 6.1 全量三道门（test/clippy/fmt）+ layering_guard/deps_guard 绿。
- [ ] 6.2 DECISIONS.md 回写（D46–D48 落点 + spike 结论/边界）；CONTEXT.md 若冒出新词补录。
- [ ] 6.3 trellis-update-spec：store 的「FTS 行不随软删移除，查询侧必须 JOIN 过滤」纪律 + 库写子命令模式进 spec。
- [ ] 6.4 提交（Phase 3.4）。
