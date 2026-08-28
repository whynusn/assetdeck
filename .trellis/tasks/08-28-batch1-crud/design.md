# Design — 素材 CRUD：回收站 + 多选模式 + 右键菜单

## 1. 数据层：tombstone 语义（D46）

### 1.1 store schema v4（crates/store）

- `assets` 增列 `deleted INTEGER NOT NULL DEFAULT 0`（0=正常，1=回收站）。迁移仿 v2 先例：`ALTER TABLE assets ADD COLUMN`，`user_version=4`。
- 新窄 API（全部 `deleted` 显式出现在 SQL 里，杜绝隐式过滤）：
  - `soft_delete_assets(&[uuid]) -> usize` / `restore_assets(&[uuid]) -> usize`：批量 UPDATE deleted 标志；**不动 file_name**，FTS 触发器（AFTER UPDATE OF file_name）天然不触发，回收站条目自动退出搜索 = 靠查询侧 JOIN 过滤（见下）。
  - `is_deleted(uuid) -> bool`、`deleted_uuids() -> Vec<String>`（仅启动回填一次）。
  - 既有读取路径加过滤：`distinct_categories()`、`all_phashes()`、`uuids_for_phash_exact()`、`for_each_asset()`（提供 `for_each_asset_active`，保留无过滤版本给工具侧审计）。
  - `search()` 属子任务 3，但本任务先在其 JOIN 里加 `AND a.deleted = 0`（当前无调用方，零风险）。
  - 彻底删除仍走 `delete_asset`（硬删行；FTS delete 触发 + tags 级联已就绪）。
  - 窄列更新：新增 `set_category(uuid, Option<&str>)`、`rename_asset(uuid, new_name)`（UPDATE file_name → 触发器自动重排 FTS）——子任务 2「统一归入」写回复用 `set_category`。
- FTS 一致性细节：软删不改名，因此 FTS 行仍在；**搜索侧必须 JOIN assets 过滤 deleted**，这一点写进 store 的 database-guidelines（trellis-update-spec 时落）。

### 1.2 library：trash 目录（crates/library）

- 新函数（`Library` 或独立 `trash.rs`）：
  - `move_to_trash(uuids)`：meta 置标 + `objects/<uuid>/` → `trash/<uuid>/`（同卷 `fs::rename`，失败回滚标志位——宁可不删也不出现「标志说删了、正本还在 objects」的不一致）。缩略图**留在 thumbs/ 不搬**（体积小、恢复零成本；彻底删除时一并清）。
  - `restore(uuids)`：`trash/<uuid>/` → `objects/<uuid>/` + 标志复位。
  - `purge(uuids)`：硬删行 + 清 `trash/<uuid>/` + 清 thumbs 路径（`Store::thumbnail_cache_path` 已是唯一真相源）。
  - `empty_trash()`：枚举 deleted 行，批量 purge。
- 顺序纪律：**先 rename 目录成功、再置 DB 标志**？否——UI 进程重启时以 DB 为准，目录漂移要可检测。定为：置标 → rename；rename 失败则回滚该批置标并报错（`LibraryError::Trash`）。重启一致性由 `library_check`（若存在）或启动扫描兜底：DB=1 但正本仍在 objects → 补搬；DB=0 但正本在 trash → 补回。此对账函数 `reconcile_trash()` 随子任务落地。
- 派生工序过滤：`tools/derive-thumbs` 改用 `for_each_asset_active`；`sample-library` 导出（`for_each_asset` :443）同改，回收站素材不得进 .emo 包。

### 1.3 index（crates/index）

- `FacetIndex` 增 `deleted: RoaringBitmap`（与 `all` 平行）。`insert/remove` 维持现状；新 `mark_deleted(id)/unmark_deleted(id)`：deleted 中的 id 从 `all` 移出、从 facet 位图移出但**记住原 membership**（`by_category` 不减行孔语义，恢复时按原值回填；简单实现：mark 时把 id 从 `all` 与 `by_tag/by_category` 摘除，unmark 时按行表 categories 回填 category 位图，tags 索引层本就不持有 → 恢复走整库重载兜底，v1 接受「恢复后需重载才见标签」为边界并写测试注释）。
  - **v1 简化定案**：恢复操作后由壳层触发一次库重载（与 clear/import 同路），index 层不承诺增量恢复保真——避免在 SoA 里造第二套真相。`mark_deleted` 仅供浏览期即时隐藏。
- `evaluate` 无需新 Filter：网格侧以 `index.all()` 为活集即自动排除（与既有 base 交集语义兼容）。

## 2. VM 层（crates/ui-viewmodels）

### 2.1 选区状态机（grid_vm 新模块 selection.rs 或并入 grid_vm）

- `Selection { set: HashSet<AssetId>, anchor: Option<AssetId>, mode: Mode }`，`Mode = Normal | Multi`。
- 纯函数入口：`on_click(id, mods: Modifiers)`、`enter_mode()/exit_mode()`、`select_all(visible_ids)`、`range_select(anchor, id, visible_ids)`。Shift 范围 = 按当前视图顺序（`ids` 已排序）取区间。
- **模式即屏蔽**：`VmEvent::OpenAsset` 仅在 `mode == Normal` 时发出；Multi 模式单击只发 `SelectionChanged`（壳层据此不 materialize、不 paste）。红线 A 的守卫测试放在 VM 层：Multi 模式下任意点击序列 → 事件流中零 OpenAsset。
- 常态下修饰键：Ctrl+点（无模式）也可加选（不触发上框？——是：带 Ctrl 的点击 = 选中切换，不发 OpenAsset）。Shift 同理。无修饰点击 = 上框（现状不变）。

### 2.2 右键菜单与操作条的数据驱动（D26/D28 模式延续）

- VM 出 `ContextMenuSpec { asset/selection, items: Vec<MenuItem{id,label,enabled}> }`，五项固定；壳层只渲染 + 回传 `menu-action(id)`。文案表进 VM（CONTEXT.md 用语）。
- 多选操作条数据 = `selected_count` + 可见全选语义，壳层渲染。

## 3. 壳层（crates/app-ui）

### 3.1 按键修饰符可行性（红线前置查证 → spike）

Slint 1.17 `TouchArea`：`PointerEvent` 在 `moved`/`pressed`/`released` 回调里有 `modifiers`（`KeyboardModifiers` ctrl/shift），但 `clicked`/`double-clicked` 信号**不带事件**。方案：瓦片改用 `pointer-event(event)` 回调自行判定按压+修饰（自记 pressed 位置，release 判定点击），或保留 clicked 并在 Rust 侧记「最后按键态」（Slint `key-pressed` 全局不可靠）。**实施第一步 = 30 分钟 spike 验证 `PointerEvent.modifiers` 在 release 携带 Ctrl/Shift**；不可行则降级：Ctrl/Shift 多选改由多选模式内的连续点选 + 操作条「全选」覆盖（R7 的「等效可达路径」），并在 DECISIONS 回写边界。右键：`pointer-event` 的 `kind == PointerEventKind.right_down`（Slint 支持）或 Spike 一并验证。

**Spike S1 结论（2026-08-28，slint 1.17.1 源码查证，i-slint-core `items/input_items.rs` TouchArea::input_event）：可行，按 R7 修饰键做。**
- `PointerEvent { button, kind, modifiers, touch_finger_id }` 内建结构（i-slint-common `builtin_structs.rs:54`），`modifiers: KeyboardModifiers { alt, control, shift, meta }`（同文件 :40）。
- TouchArea 的 `Down`/`Up`/`Move`/`Cancel` 四种 `PointerEventKind` 分发时，`modifiers` 一律取窗口全局按键态 `context().modifiers.get()`——**release 事件携带当时的 Ctrl/Shift，不依赖按下瞬间**，与原生资源管理器语义一致。
- 右键区分靠 `button == PointerEventButton.Right` + `kind == Down`（即 `right_down` 语义）；`Pressed{button}` 原样透传，`clicked` 信号只在 Left release 且落在界内时触发（右键不会误触发既有 clicked）。
- 实现取案：瓦片保留 `clicked`（无修饰上框现状不变），新增 `pointer-event` 回调处理修饰点击与右键；「release 判定点击」自记 pressed 位置仅用于框选拖拽（批 1 不做）。
- 已知边界：`Cancel` 事件 `button` 恒为 `Other`，不得当作点击。

**Spike S2 结论：成立。** app-ui `Cargo.toml` 依赖 = {logging, ui-viewmodels, platform, lru, slint}；`tests/deps_guard.rs::ui_cargo_toml_dependency_whitelist_is_exact` 白名单精确锁死，`library` 不在列。库写操作必须走 `sample-library.exe --cmd …` 子进程（阶段 2.3 已交付），与 §3.2 定案一致。

### 3.2 装配

- 删除/恢复/彻底删除 = 子进程命令（仿 D33 `ChildTaskRunner` 管线）还是直接 UI 侧函数调用？数据量小（UPDATE + rename 目录），但**进程纪律**（D11）只管解码重活，纯 fs/DB 操作允许 UI 直调（`sample-library` 已是库写方，避免双写进程——**定案：库写操作全部收进一个「库写子命令」**，UI 通过 `ChildTaskRunner` 起 `sample-library.exe --cmd trash|restore|purge|rename|move-category|empty-trash`，与导入管线同模式，单写者不变）。rename/set_category 也走此路（毫秒级子进程，进度行协议复用）。
  - 权衡：子进程启动 ~10-30ms + SQLite 重开——换来库写单入口、崩溃隔离、与 D16/既有工具复用一致。右键「删除」体感要求不高（软删瞬时反馈靠 UI 先行隐藏 + 子进程确认后对齐）。
- 菜单渲染：Flickable 内浮层用既有「绝对定位 Rectangle + 外层同系 Flickable」模式（settings/import 菜单先例）；弹出位置 = 命中瓦片坐标夹取视口。

## 4. 兼容与回滚

- v3→v4 单向迁移；回滚 = 旧版可开新库吗？**不可**（旧版 SELECT * 兼容多列，实际可读——但 `deleted=1` 行会被旧版看见。记录为已知边界：降级使用不做保证）。
- 每个行为变更对应测试先行（TDD 纪律沿用 TDD_PLAN.md 红绿节奏）。
