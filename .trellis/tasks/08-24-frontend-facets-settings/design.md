# Design - 前端改造：分类/标签机制 + 设置页 + 分类检索器 + IM 垂直下拉

配套 prd.md（R1-R4 需求、A1-A7 验收、Q1 已决=受控占位）。本文件只谈技术设计：
架构落点、跨层契约、兼容性、取舍与回滚。样图（样图.png）仅作视觉参照，不逐像素复刻。

## 架构与分层落点

改动集中在三层，越靠上越薄：

- crates/store：只读补两个「distinct 分类/标签（含计数）」查询方法（R1.3）。schema 不动。
- crates/ui-viewmodels：新增分类/标签注册表（catalog_loader）+ 设置模型（settings.rs）；纯 Rust 可单测。
- crates/app-ui：Slint 结构重排（检索框、动态分类、设置浮层、IM 垂直下拉浮层）+ main.rs 装配。本层不加任何新依赖。

红线（继承 AGENTS.md / DECISIONS.md）：

- 只上框绝不自动发送：send_after_paste 默认 false，单/双击仍只走 routing.paste，注入序列绝不含 0x0D（D8/D13）。
- app-ui deps 白名单不扩张（crates/app-ui/tests/deps_guard.rs 三测）。serde/toml 只进 ui-viewmodels。
- VM 源码禁止出现 platform::win32 / Win32 / cfg(windows)（crates/ui-viewmodels/tests/layering_guard.rs）。settings.rs 回避这些字面串。
- UI 进程不解码/生成缩略图（D11）；分类过滤走 FacetIndex::evaluate（RoaringBitmap），v1 禁向量检索（D4，deny.toml）。

## R1 分类/标签注册表（修「全部未分类」）

根因：catalog_loader.rs 的 load_real_library 与 load_library_catalog 装配 FacetIndex 时硬编码 category:None, tags:vec![]。
meta.db 实际带 category="测试素材", tags=["真实闭环"]（sample-library 导入写入；真机千牛导入写 tags=["qianniu"]，目录名作 category）。

设计：在 load_real_library 的同一趟 for_each_asset 里 inline 建注册表，不改 store、不改遍历次数。

新增类型（catalog_loader.rs）：FacetEntry{id,name,count}；LibraryFacets 持 categories/tags 列表 + name->id 私有映射。
方法：categories()/tags() 返回切片；category_id(name)；fuzzy_filter(query) 对分类名+标签名做子串匹配，命中组 Filter::AnyOf，空 query 返回 None。

RealAssetResolver 加私有 facets: LibraryFacets + pub fn facets(&self)->&LibraryFacets。

装配流程改动（load_real_library）：遍历每行时
1. 分类名 -> 注册表分配/复用 CategoryId；tags 名 -> 分配/复用 TagId。
2. idx.insert(Asset{ category:Some(CategoryId(cid)), tags:vec![TagId..], .. })（原来恒 None/空，唯一实质改动）。
3. 计数在注册表内累加。

签名不变：load_real_library(root)->Result<(FacetIndex,RealAssetResolver)> 保持原型，3 处调用方 + tests + real-im-verify 零改动。facets 从 resolver 取。

load_library_catalog（--bench 内存守卫用）保持恒空分类装配不变：语义对象是行数与驻留结构，改它会动内存基线且无收益。

### CategoryId 稳定性契约

注册表 id 按「首次出现顺序」分配，for_each_asset 按 uuid 升序遍历，确定性，同库多次打开顺序一致。
id 仅单次库会话内有效，不持久化——检索/过滤都在同一会话内完成，无跨会话引用。

## R1.3 store 只读查询

新增 distinct_categories()->Vec<(String,i64)>（NULL 归「待分类」）与 distinct_tags()->Vec<(String,i64)>，各加单测。
catalog_loader 优先用同趟 for_each_asset 建表以省一次全表扫描；这两个方法作为 store 正式分面查询入口供未来复用。

## R2 检索器（工具栏红框区）

组合逻辑落在 main.rs 的过滤装配：

- search 非空 -> resolver.facets().fuzzy_filter(q)：分类名+标签名内存子串匹配（绕开 FTS5 trigram >=3 限制），命中 Filter::AnyOf，无命中给空集过滤。
- search 空 -> 回落当前选中分类过滤。
- 检索非空覆盖分类选择；清空检索恢复分类视图（A2）。

fuzzy_filter 是纯函数，进 ui-viewmodels 单测（命中/大小写/空串/无命中四态）。

## R3 设置页（受控占位，Q1 已决）

新文件 crates/ui-viewmodels/src/settings.rs：AppSettings{ activate_on_single_click:bool(默认false=双击), send_after_paste:bool(默认false) }，
两字段 #[serde(default)]；load(path)->Self（缺失/解析失败->Default不panic）；save 原子写 tmp+rename；settings_path(library_root:Option<&Path>)->PathBuf。

持久化位置：随库目录 <library_root>/settings.toml；无库根回落 <exe_dir>/settings.toml。
依赖：ui-viewmodels/Cargo.toml 加 serde(derive)+toml（本 crate 可加，deps_guard 只查 app-ui）。

红线守卫（新测试）：AppSettings round-trip + 缺字段回落默认；settings.rs 源码回避 Win32/platform::win32/cfg(windows) 字面串。

send_after_paste 语义（Q1=受控占位）：开关持久化可切换，但不接真实发送链路——上框永远只 routing.paste（止步输入框）。
打开开关暂无实际发送效果，UI 用文案说明「发送为后续能力」。真正接入 send_key 属独立任务，本任务不做。绝不误发，红线最保守。

### 单/双击交互（R3.4）

Slint AppWindow 加 in property <bool> single-click-activate。瓦片 TouchArea 同时绑 clicked 与 double-clicked，都指向同一回调 activate-asset(int)。
main.rs 按 settings.activate_on_single_click 分流：

- 双击模式（默认）：double-clicked 触发上框；clicked 只做选中（不上框）。
- 单击模式：clicked 触发上框。

取舍：Slint 的 clicked 在 double-clicked 前也会触发。为避免双击模式下单击误上框，回调统一在 main.rs 按当前模式判定。
VM 的 double_click/事件语义不变，仅新增一条「单击上框」装配分支。

## R4 IM 目标垂直下拉浮层

现状：if root.target-mode==3: Rectangle{height:122px} 是顶层 VerticalLayout 子项，作为水平 band 占垂直流，把瀑布流整体下推（痛点）。

设计：顶层从裸 VerticalLayout 改为填满窗口的 overlay 宿主 Rectangle，内部：
1. 先放原 VerticalLayout（工具栏+目标条+notice+进度+Flickable 网格），走正常布局流；
2. 其后声明绝对定位浮层（x/y 锚定、不占流）：
   - IM 下拉：if target-mode==3 -> 锚目标 chip 正下方，内部 VerticalLayout 纵向排 for choice in target-choices（沿用 TargetChoiceData）。
   - 设置浮层：if settings-open -> 同法绝对定位，承载单双击开关 + 发送开关。

浮层浮于瀑布流之上、不下推布局（A5）。收起沿用现有 toggle_picker/choose/点 chip 语义。
点击浮层外部收起：加一个覆盖全窗、仅在浮层开启时出现的透明 TouchArea，clicked -> 收起。

样图对齐（非强制）：左侧分类竖排 rail 与样图「我的表情/团队表情/...」一致；顶部标题条承载当前目标标签+检索框；
右侧详情面板本任务不做（超出 R1-R4，留待后续）。分类 rail 用 R1 动态分类渲染。

## 数据流与契约

meta.db --for_each_asset--> load_real_library 建 LibraryFacets(注册表,计数) + FacetIndex.insert(Asset{category:Some,tags:[..]})（唯一实质改动）-> RealAssetResolver{facets}。
main.rs 装配：分类 rail 点击->Filter::InCategory->vm.set_filter；检索框输入->facets.fuzzy_filter->vm.set_filter（非空覆盖分类）；检索清空->回落分类 Filter；
瓦片 clicked/double-clicked->按 settings 分流->routing.paste（绝不发送）；设置开关->AppSettings.save(settings_path)。

## 兼容性与迁移

- load_real_library 签名不变 -> 现有调用方零改动。
- store 只增只读方法 -> schema 版本不变，老库照常打开。
- settings.toml 缺失/损坏 -> Default（双 false），行为等价现状（双击上框、不发送）。
- 演示库路径（无 --library-root）：demo_index 仍恒 category/tag 占位，检索/分类在演示库退化但不崩。

## 取舍与回滚

- 注册表 inline 建（不落库）：零 schema 迁移、零额外扫描，代价是 id 不跨会话持久——本任务无跨会话需求，可接受。
- send 开关占位：牺牲「打开即发送」，换绝不误发的红线保底；真实发送作为独立任务。
- Slint overlay：绝对定位浮层比 band 复杂，但这是消除「下推瀑布流」的唯一正解。
- 回滚点：R1（catalog_loader）与 R3/R4（Slint+main.rs）相互独立，任一步失败可单独还原。

## 验证

- cargo test -p store -p ui-viewmodels：distinct 查询、fuzzy_filter、settings round-trip、facets 计数。
- cargo test -p app-ui：deps_guard 三测 + layering_guard 两测全绿（白名单未扩张）。
- cargo build --release（先 Stop-Process 占用 exe）+ 真机 asset-manager.exe --library-root samples/library 逐条核 A1-A7。
