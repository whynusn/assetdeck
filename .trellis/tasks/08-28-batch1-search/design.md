# Design — 搜索范围 + FTS5 混合路由

## 1. SearchProvider 扩展（ui-viewmodels/search.rs）

```rust
pub enum SearchScope { All, FileName, Category, Tag }   // D51 四档

/// FTS 名检索接缝：FTS 查询 + uuid→AssetId 二分封装在 resolver 内（它同持
/// store 与升序 uuids），provider 只见 trait——ui-viewmodels 保持可 mock。
pub trait FtsNameSource {
    fn name_ids(&self, query: &str, limit: usize) -> Result<Vec<u32>, SearchError>;
}

pub struct HybridSearchProvider<'a> {
    pub facets: &'a LibraryFacets,
    pub fts: Option<&'a dyn FtsNameSource>,   // bench/无库路径 = None → 纯内存路
}
impl SearchProvider for HybridSearchProvider {
    fn search(&self, query: &str, scope: SearchScope, base: &Filter) -> Result<Filter, SearchError>;
}
```

- 现 `FacetSearchProvider` 保留为 `HybridSearchProvider` 的退化别名或直接改造（调用点 main.rs:696 与 search.rs 内单测同步改）。trait 加 `scope` 参数即 breaking——D30 说「统一入口」，参数演化在 v1 内部可接受，测试同步改。
- scope 路由：
  - `All` = 名子句 ∪ 分类子句 ∪ 标签子句（= 现行为 + 大小写修复）
  - `FileName` = 仅名子句；`Category`/`Tag` = 仅对应 fuzzy 子句（短查询内存路即够，三档均不走 FTS——FTS 表只有 name 列）。
- 名子句生成：`query.chars().count() >= 3 && store.is_some()` → `Filter::FtsNames(uuid_bitmap 入口)`？**不**——D4 候选集抽象 = Filter 是纯声明，不能携带位图外物。定案：Provider 返回 `Filter::NameIn(Vec<AssetId>)` 新变体（domain::Filter 扩展，index::evaluate 直接并成 RoaringBitmap 返回；Vec 来自 FTS 命中→二分映射→**瞬时**构造，与 NameContains 同级）。1–2 字符或无 store → `Filter::NameContains`（现语义，无分配改造见 §3）。

## 2. uuid→AssetId 映射（零常驻，design 事实已核实）

- 映射前提**已核实**（2026-08-28）：`Store::for_each_asset`（store/src/lib.rs:453-462）SQL 为 `ORDER BY uuid`，行序 = uuid 字典序确定；`load_real_library` 的 `uuids: Vec<String>` 随之天然升序，二分合法。crud 子任务的 `for_each_asset_active`（带 `deleted=0`）必须保留 `ORDER BY uuid`，本设计加一条守卫测试：加载断言 `uuids` 升序不变量（防未来改动悄悄破坏二分）。
- 映射函数：`binary_search(uuids, uuid) -> Option<usize>`，命中即 AssetId。`RealAssetResolver` 暴露 `uuid_rank(&str) -> Option<u32>`（持 `&uuids` 引用，无克隆）。
- FTS `limit`：SearchHit 现带 limit 参数——UI 路给 `base 集大小上限` 或固定 10_000 后 `NameIn` 再与 base 交集；浏览期候选再被 grid 截断，无内存风险。

## 3. 内存扫描无分配化（index::search_names）

- 现状：每行 `name.to_lowercase().contains(&needle_lower)` → 每行一次堆分配。
- 改造：needle 一次 `to_lowercase()`（每次查询一次）；行匹配用 `unicase`-风格无分配滑窗：ASCII 快速路径 `eq_ignore_ascii_case` 逐位 + 非 ASCII 回退 `char::to_lowercase()` 迭代器比较（中文无大小写，等价）。不引新依赖（自实现 ~40 行 + 测试），或引 `unicase`（许可证 MIT/Apache，依赖增量 KB 级）——**倾向自实现**，避免为 40 行逻辑加 crate（内存纪律 D10 的从简原则）。
- 测试：现 `name_contains_filter_matches_case_insensitive_substring` 保持绿 + 新增 CJK+ASCII 混名、土耳其语 İ 类 Unicode 大小写表用例。

## 4. 大小写统一（D51 修复）

- `LibraryFacets::fuzzy_filter`：`entry.name.contains(needle)` → 双侧 ASCII 快速小写化比较（同 §3 工具函数，放 `domain` 或 `ui-viewmodels` 内共享模块；ASCII 折叠对中文无影响，与 FTS trigram 行为对齐，见 R9 边界）。
- facets_spec.rs 测试夹具现全 CJK → 新增 ASCII 大小写用例守卫修复不退行。

## 5. 壳层 UI

- 搜索框前缀下拉：appwindow.slint 顶栏 :228 LineEdit 左侧加 `ComboBox`（四档）或 chip 按钮弹四选一菜单（与目标条 chip 风格一致）；选档 = 内存态 `in property <int> search-scope`（UiEnums 收口，D32 纪律）→ `search-changed(string)` 改为 `search-changed(string, int)` 或新回调带 scope；当前查询在 Rust 侧缓存（`current_query` Cell），切档即时重跑（不要求重输）。
- 下拉开合 = Flickable 浮层纪律（与 motion 子任务两段式动效一致，新控件出生即正确）。

## 6. bench 接线

- bench-harness 增加「查询延迟」场景：生成 10 万行库 → 分别对 FTS 路（≥3 字符）与内存路（≤2 字符）跑 N 查询取分位数，输出对比 `research/latency-ledger.md` 格式新节。
