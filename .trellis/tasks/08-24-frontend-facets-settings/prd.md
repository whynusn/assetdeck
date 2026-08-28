# PRD — 前端改造：分类/标签机制 + 设置页 + 分类检索器 + IM 垂直下拉

> 依据：用户第 N 轮指令（贴千牛「我的表情」面板截图作视觉参照，提 4 项前端需求）
> · `DECISIONS.md` D4/D5/D8/D13 · `AGENTS.md` 红线 · 已读代码确认的现状。

## Goal

把素材管理器前端从「占位工具栏 + 水平下推的目标条」升级为可用的组织与检索界面：
让真实分类/标签在 UI 里可见可筛，加一个设置页控制交互行为，把分类检索器放进标题工具栏，
并把 IM 目标选择从水平 band 改成垂直下拉浮层（不再挤走素材瀑布流）。截图仅作视觉参照，
不逐像素复刻。

用户原话（4 点 + 1 补充）：
1. 加设置页，控制「素材上框触发行为=单击还是双击」「上框后是否立即发送」等。
2. 在截图红框区域（顶部标题/工具栏）加筛选/检索器，支持针对素材分类（标签）的**模糊检索**。
3. 完善分类/标签机制——「目前导入的素材全部都是未分类的，你还没有实现吧」。
4. IM 目标的选择 UI 改成**垂直下拉菜单**（overlay 浮层），去掉现状的水平下推。

## Confirmed Facts（已读代码确认，非推演）

- 后端分类/标签已具备，UI 装配层写死未分类是「全部未分类」根因：
  - `crates/store/src/lib.rs`：schema v2，`assets.category TEXT` + 独立 `tags` 表 +
    `assets_fts`（FTS5 **trigram**，查询须 ≥3 连续字符）+ INSERT/UPDATE/DELETE 触发器。
    `search(query, limit)`、`for_each_asset`（逐行含 tags）已实现。**尚无「列出 distinct
    分类/标签」查询方法，需新增。**
  - `crates/ui-viewmodels/src/catalog_loader.rs`：`load_real_library` 装配 FacetIndex 时
    原本**硬编码 `category: None, tags: vec![]`**——UI 全部未分类的直接原因。
    工作树已修复该处：现按行灌入真实 `Some(CategoryId)`/`vec![TagId]`，并暴露 `category_names()`。
    R1 剩余工作是补齐检索所需的名称注册表（`LibraryFacets`/`fuzzy_filter`），见 design/implement。
  - `tools/sample-library/src/main.rs`：导入时写死 `category=Some("测试素材"), tags=["真实闭环"]`，
    所以 meta.db 里其实带 category/tags，只是 UI 从未读取。
  - `crates/library/src/lib.rs:15`：`INBOX_CATEGORY = "待分类"`，`enqueue` 落库时 category 缺省为它。
  - `crates/index/src/lib.rs`：`FacetIndex` 有 `by_category`/`by_tag`/`evaluate(&Filter)`/`tag_counts()`。
  - `crates/domain/src/lib.rs`：`Filter`(All/InCategory/HasTag/Not/AllOf/AnyOf) + `CategoryId(u32)`/`TagId(u32)` 齐全。
  - `crates/ui-viewmodels/src/grid_vm.rs`：`LibraryGridVm::set_filter(&Filter)` 已实现（改过滤即重建 id 序列 + rect 表 + 清 LRU）。
- 现有前端 `crates/app-ui/ui/appwindow.slint` + `crates/app-ui/src/main.rs`：
  - 工具栏是硬编码按钮 `["全部","分类0".."分类4"]` → `filter-selected(int)`，映射虚构 CategoryId，纯占位。
  - IM 目标条：`target-mode==3` 时 `if root.target-mode==3: Rectangle{height:122px...}` **水平 band 下推**——待改成垂直浮层。
    目标选择走 `TargetRoutingRuntime` + `TargetBarVm`，已支持热/冷目标、pin、多开歧义 picker、`toggle_picker`、`choose(key)`；
    Slint 回调 `target-chip-clicked`/`target-choice-selected(string)`/`target-pin-toggled` 已接线。
  - 双击上框：`on_double_clicked` → `vm.double_click` → `OpenAsset` → `RealAssetResolver::materialize` → `routing.paste`，止步输入框不发送（已确认符合预期）。
- 无设置持久化机制；app-ui 依赖白名单被 `deps_guard.rs` 锁死为 `{ui-viewmodels, slint, platform}`，
  故设置的读写/序列化只能落在 ui-viewmodels 或经它转发（app-ui 不能直接引 serde/toml 之外的新 crate）。

## 红线约束（不可违反）

- **只上框，绝不自动发送**：`auto_send` 默认 false（D8/D13）。设置页可暴露「上框后立即发送」开关，
  但**默认必须关**，且核心链路绝不合成回车（`0x0D`）。
  Q1 已决=**受控占位**：`send_after_paste` 开关持久化可切换，但 v1 不接真实发送链路——
  上框永远只走 `routing.paste`（止步输入框），打开开关暂无实际发送效果，UI 以文案说明「发送为后续能力」。
  真正接入 send_key 属独立任务，本任务不做，红线取最保守（绝不误发）。
- 分类过滤走 RoaringBitmap（`FacetIndex::evaluate`），全文走 FTS5 trigram；**v1 禁止向量检索**（D4）。
- UI 进程绝不解码/生成缩略图（D11）。本任务纯 UI/装配，不触碰解码路径。
- app-ui 依赖白名单不得扩张（`deps_guard.rs` 三个守卫测试必须继续通过）。

## Requirements

### R1 分类/标签机制落地（修「全部未分类」）
- R1.1 打开库时扫描 meta.db 里真实的 distinct category 与 tags，分配稳定数字 id，
  建立 `String ↔ CategoryId/TagId` 双向注册表，喂给 `FacetIndex`，使分类过滤真实生效。
- R1.2 工具栏分类筛选项由真实分类动态生成（含每类计数），取代硬编码 `分类0..4`；
  「全部」与「待分类」（`INBOX_CATEGORY`）保留为固定入口。
- R1.3 store 新增「列出 distinct categories（含计数）」「列出 distinct tags（含计数）」查询方法。

### R2 分类/标签模糊检索器（标题工具栏红框区）
- R2.1 在工具栏放一个文本输入检索器，对**分类名/标签名做模糊（子串）匹配**，命中即组装
  `Filter::AnyOf([InCategory..., HasTag...])` 交给 grid_vm。
- R2.2 检索在内存注册表上做子串匹配（不受 FTS5 ≥3 字符限制约束）；清空检索恢复当前分类视图。

### R3 设置页
- R3.1 新增设置入口与设置面板（overlay 或独立视图）。
- R3.2 开关项：①上框触发行为 = 单击 / 双击（默认双击，保持现状）；②上框后是否立即发送（默认关，红线）。
- R3.3 设置持久化到磁盘（便携约定：随库目录或 exe 旁），重启后保留。
- R3.4 触发行为设置实时影响瓦片交互（单击模式下单击即上框，双击模式下双击才上框）。

### R4 IM 目标垂直下拉浮层
- R4.1 把 `target-mode==3` 的水平 band 改成绝对定位 overlay 下拉，锚定在目标 chip 下方，
  **不占垂直布局流**（素材瀑布流不再被下推）。
- R4.2 列表垂直排列每个候选目标（沿用现有 `TargetChoiceData`：健康点/标签/状态/可用性）。
- R4.3 点击候选项/点击外部/再次点击 chip 均可收起浮层，行为与现有 `toggle_picker`/`choose` 一致。

## Acceptance Criteria

- [ ] A1：用真实库（`samples/library`）启动，工具栏显示真实分类（至少「测试素材」）而非「分类0..4」；点某分类只显示该类素材，计数正确。
- [ ] A2：在检索器输入分类/标签名的片段（如「测试」「闭环」），瀑布流实时收敛到匹配集合；清空后恢复。
- [ ] A3：设置页可切换单击/双击触发；切到单击后单击瓦片即上框，切回双击后单击不触发；重启程序设置保留。
- [ ] A4：「上框后立即发送」开关默认关；即便打开也不违反“绝不合成回车”的既定实现边界（见 Open Q1 决策）。
      （Q1 已决=受控占位：打开开关仅持久化，不接真实发送链路，上框永远只 routing.paste。）
- [ ] A5：点击目标 chip 弹出**垂直下拉浮层**，浮层浮于瀑布流之上、不下推布局；选择/点外部收起。
- [ ] A6：`cargo build` + `cargo test`（含 `deps_guard`、`layering_guard`、既有守卫）全绿；app-ui 依赖白名单未扩张。
- [ ] A7：瓦片首行显示与真实缩略图预览不回退（保住 `visible_range` 与 `thumbnail_path` 现有修复）。

## Out of Scope

- 素材自动发送的实际实现（仍只做“上框”；发送开关仅作 UI 占位/受控，见 Open Q1）。
- 缩略图/解码路径改动、上框延迟事件驱动改造（属另一 in_progress 任务 08-24-im-paste-latency）。
- 打包便携 exe。
- 向量/语义检索（D4 禁止）。

## Resolved Decisions

- Q1（已决）：R3「上框后立即发送」开关采用**受控占位**——开关持久化可切换，但 v1 不接真实发送链路，
  上框永远只走 `routing.paste`（止步输入框），核心链路绝不合成回车（`0x0D`）。真正接入 send_key 属独立任务。
  取舍：占位实现风险最低、绝不误发；打开开关暂无实际发送效果，UI 以文案说明「发送为后续能力」。

## Notes

- 复杂任务：本任务跨 store/ui-viewmodels/app-ui 三层且改公共接口，`design.md` + `implement.md` 已补齐。
- 技术细节（分类注册表映射、设置持久化位置、检索内存匹配、overlay 浮层的 Slint 实现）见 `design.md`；有序执行清单见 `implement.md`。
- 阻塞规划的 Open Questions 已清空（Q1 已决）。
