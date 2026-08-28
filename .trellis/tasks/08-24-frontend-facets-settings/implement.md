# Implement - 前端改造：分类/标签机制 + 设置页 + 检索器 + IM 垂直下拉

配套 prd.md（R1-R4 / A1-A7）、design.md（技术方案）。本文件只列有序执行清单、
验证命令、风险文件与回滚点。ASCII 优先；含中文的新文件用 PowerShell here-string 写。

## 前置

- 每条命令先 cd "C:\Users\Administrator\Documents\Default Project"。
- 构建前先 Get-Process asset-manager,real-im-verify | Stop-Process -Force（占用 exe 报 os error 5）。
- 不 commit（未获用户明确同意）。不 revert 工作树历史改动。

## 执行清单（有序）

### 步骤 1 — R1.3 store distinct 查询（crates/store/src/lib.rs）
- 新增 pub fn distinct_categories(&self) -> Result<Vec<(String, i64)>>：
  SELECT COALESCE(category, INBOX 常量) 分组计数，按名排序。
- 新增 pub fn distinct_tags(&self) -> Result<Vec<(String, i64)>>：
  从 tags 表分组计数，按名排序。
- 测试（crates/store/tests/store_spec.rs 追加）：插入两类三标签，断言计数与 NULL 归「待分类」。
- 验证：cargo test -p store。

### 步骤 2 — R1 catalog_loader 注册表（crates/ui-viewmodels/src/catalog_loader.rs）
现状：load_real_library 已把真实 category/tags 灌进 FacetIndex，resolver 已暴露 category_names()。
本步补齐检索所需的名称注册表：
- 新增 pub struct FacetEntry { pub id: u32, pub name: String, pub count: u32 }。
- 新增 pub struct LibraryFacets：持 categories: Vec<FacetEntry>、tags: Vec<FacetEntry>、
  以及私有 name->id 映射。方法：categories()/tags() 返回切片；category_id(name)->Option<CategoryId>；
  fuzzy_filter(query:&str)->Option<Filter>（分类名+标签名子串匹配，命中组 Filter::AnyOf，空/纯空白 query 返回 None，无命中返回 Some(AnyOf(vec![]))=空集）。
- load_real_library 同一趟 for_each_asset 累加 FacetEntry.count（分类/标签各自计数），
  遍历结束把 category_ids/tag_ids + 计数装配成 LibraryFacets。
- RealAssetResolver 加私有 facets: LibraryFacets + pub fn facets(&self)->&LibraryFacets。
  category_names() 保留（现有调用方 apply_categories 依赖），可改为委托 facets.categories()。
- load_library_catalog（--bench）保持恒空装配不动。
- 测试（新建或追加 crates/ui-viewmodels/tests/facets_spec.rs）：fuzzy_filter 四态
  （命中分类片段 / 命中标签片段 / 空串 None / 无命中空集）；用小型内存 Store 或直接构造 LibraryFacets。
- 验证：cargo test -p ui-viewmodels。

### 步骤 3 — R3 设置模型（新建 crates/ui-viewmodels/src/settings.rs）
- pub struct AppSettings { activate_on_single_click: bool(默认 false=双击), send_after_paste: bool(默认 false) }，
  两字段 #[serde(default)]，derive Serialize/Deserialize/Clone/Debug/PartialEq，impl Default。
- pub fn load(path:&Path)->AppSettings（文件缺失/解析失败 -> Default，绝不 panic）。
- pub fn save(&self, path:&Path)->io::Result<()>（写 tmp 同目录 + rename 原子替换）。
- pub fn settings_path(library_root: Option<&Path>)->PathBuf（有库根 -> <root>/settings.toml；否则 <exe_dir>/settings.toml）。
- 源码回避 "Win32"/"platform::win32"/"cfg(windows)" 字面串（layering_guard）。
- Cargo.toml（crates/ui-viewmodels）[dependencies] 加 serde={version="1",features=["derive"]} + toml="1"。
- lib.rs 加 pub mod settings; pub use settings::AppSettings; 并导出 facets 相关类型（FacetEntry/LibraryFacets）。
- 测试：AppSettings round-trip（save->load 相等）；缺字段 toml 回落默认；损坏内容回落默认不 panic。
- 验证：cargo test -p ui-viewmodels；cargo test -p app-ui（deps_guard 三测 + layering_guard 两测须绿）。

### 步骤 4 — R2/R3/R4 Slint 结构重排（crates/app-ui/ui/appwindow.slint）
- 顶层由裸 VerticalLayout 改为填满窗口的 overlay 宿主 Rectangle：
  内部先放原 VerticalLayout（工具栏+目标条+notice+进度+网格），再声明绝对定位浮层。
- R2 检索框：工具栏内加 search := TextInput（占位属性 in-out property <string> search-text），
  changed text => root.search-changed(self.text)；旁边一个清空按钮。
- R4 IM 垂直下拉：删除 if root.target-mode==3 的水平 band Rectangle（占垂直流那段），
  改为 overlay 内 if root.target-mode==3 的绝对定位 Rectangle，x/y 锚 target-chip 下方，
  内部 VerticalLayout 纵向 for choice in target-choices（沿用 TargetChoiceData 与 target-choice-selected）。
- R3 设置入口：工具栏加「设置」按钮 => root.settings-toggled()；
  overlay 内 if root.settings-open 的绝对定位面板：两个开关行
  （单/双击触发：single-click-activate；上框后发送：send-after-paste），
  开关点击 => root.setting-single-click-toggled() / root.setting-send-after-toggled()。
- 新增 in property <bool> single-click-activate; in property <bool> send-after-paste; in property <bool> settings-open;
- 瓦片 TouchArea 加 clicked => root.tile-clicked(tile.asset-id)，保留 double-clicked => root.double-clicked(...)。
- 浮层开启时铺一层全窗透明 TouchArea，clicked => 收起（settings-close / picker 收起）。
- 验证：cargo build -p app-ui（Slint 编译期报错先修）。

### 步骤 5 — main.rs 装配（crates/app-ui/src/main.rs）
- 载入设置：main 开头据 library_root 求 settings_path，AppSettings::load，
  app.set_single_click_activate / set_send_after_paste。
- R2 检索：on_search_changed(q)：q 非空 -> resolver.facets().fuzzy_filter(q) -> vm.set_filter；
  q 空 -> 回落当前分类 Filter（记住当前分类选择，用一个 Rc<Cell<i32>> 或 Rc<RefCell<Filter>>）。
  刷新 content_y=0 + total + sync_window。
- R3 交互分流：把现有 on_double_clicked 的上框逻辑抽成一个闭包 paste_asset(asset_id)，
  on_double_clicked 在双击模式触发；on_tile_clicked 在单击模式触发；模式读 Rc<Cell<bool>>（single_click）。
  on_setting_single_click_toggled：翻转 + 持久化 save + app.set_single_click_activate。
  on_setting_send_after_toggled：翻转 + 持久化 save + app.set_send_after_paste（受控占位，不接真实发送链路）。
  on_settings_toggled：翻转 app.get_settings_open。
- R4：无需 main 改动（下拉浮层复用 target-mode / target-choice-selected）；仅确保 chip 点击仍 toggle picker。
- 保留 fn win32_runtime_deps（deps_guard 要求）。
- 验证：cargo build -p app-ui。

### 步骤 6 — 构建 + 测试 + 真机
- Get-Process asset-manager,real-im-verify | Stop-Process -Force。
- cargo build（含 --release 供真机）。
- cargo test -p store -p ui-viewmodels -p app-ui（deps_guard/layering_guard 全绿）。
- 真机 target/release/asset-manager.exe --library-root samples/library，逐条核 A1-A7：
  A1 真实分类「测试素材」+计数；A2 检索「测试」「闭环」实时收敛、清空恢复；
  A3 单/双击切换生效+重启保留；A4 发送开关默认关且不合成回车；
  A5 IM 垂直下拉浮层浮于瀑布流上不下推；A6 build+test 全绿；A7 首行+真实缩略图不回退。

## 验证命令速查
- cargo test -p store
- cargo test -p ui-viewmodels
- cargo test -p app-ui
- cargo build --release
- target/release/asset-manager.exe --library-root samples/library

## 风险文件与回滚点
- appwindow.slint：结构重排风险最高（overlay 绝对定位）。回滚点：R2/R3/R4 相互独立，
  单项失败可只还原该段 Slint + 对应 main.rs 回调。
- catalog_loader.rs：load_real_library 签名不得变（3 处调用方 + tests + real-im-verify 依赖）。
- ui-viewmodels/Cargo.toml：只加 serde+toml，绝不动 app-ui/Cargo.toml（deps_guard）。
- settings.rs：绝不出现 Win32/platform::win32/cfg(windows) 字面串（layering_guard）。

## 红线自检（收尾前）
- 上框链路绝不含 0x0D；send_after_paste 仅持久化占位不接真实发送。
- app-ui 依赖白名单未扩张（deps_guard 三测绿）。
- VM 层无平台字面串（layering_guard 两测绿）。
- UI 进程不解码/生成缩略图。

## 真机验证结果（2026-08-24，target/release/asset-manager.exe --library-root samples/library）
- [x] A1 左栏真实分类「测试素材(26)」+「全部(26)」计数，替换旧「分类0..4」。
- [x] A2 检索「闭环」（tag）保留 26 项；「zzz不存在」实时收敛「共 0 项」；✕ 清空恢复。
- [x] A3 设置面板两开关（素材上框触发=单/双击、上框后立即发送）；settings_spec 覆盖 save/load 往返。
- [x] A4 「上框后立即发送」默认关，受控占位仅持久化，核心链路绝不合成回车。
- [x] A5 目标 chip 弹垂直下拉浮层（微信×2/千牛×2/拼多多），浮于瀑布流上、不下推布局。
- [x] A6 cargo build + --release + test（deps_guard/layering_guard 全绿）。
- [x] A7 瓦片首行 #9/#16/#19/#22/#0/#23 + 真实水母缩略图正常渲染，未回退。
