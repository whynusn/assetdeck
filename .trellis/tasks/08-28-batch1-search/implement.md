# Implement — 搜索：范围下拉 + 大小写统一 + FTS5 混合路由

> 依赖：crud 子任务已合并（schema v4 `deleted`、`Store::search` 已带 `deleted=0` JOIN、`FacetIndex.deleted` 位图、`for_each_asset_active` 保序）。
> 节奏：红灯先行；每阶段末 `cargo test -p <crate>`，收尾全量三道门。

## 阶段 1 — domain + index：`Filter::NameIn` 与无分配扫描

- [ ] 1.1 红灯（index/tests）：`name_in_filter_returns_given_ids`（含与 deleted 位图求交=恒等，NameIn 传入已过滤集）；`search_names_unicode_case_folding`（CJK+ASCII 混名、İ/ı 表用例）；`uuids_sorted_invariant_on_real_load`（若放阶段 2 亦可）。
- [ ] 1.2 实现：domain::Filter 增 `NameIn(Vec<u32>)` 变体；index::evaluate 对应分支（Vec→RoaringBitmap，与 base 语义正交）；`search_names` 改无分配滑窗匹配（needle 单次小写化；ASCII 快速路径 + char::to_lowercase 回退，~40 行自实现，零新依赖）。
- [ ] 1.3 守卫既有：`name_contains_filter_matches_case_insensitive_substring`、budget.rs 1M 行测试不退红（无分配改造应**更快**，budget 阈值可收紧不可放宽）。

## 阶段 2 — catalog_loader：`FtsNameSource` 实现

- [ ] 2.1 红灯：`fts_name_source_maps_uuid_to_row_by_bsearch`（造 3+ 行库，FTS 命中 ≥3 字符查询 → 行号与 name_contains  oracle 一致）；`uuids_vec_is_ascending`（加载后断言升序不变量，护二分）。
- [ ] 2.2 实现：`RealAssetResolver::name_ids(query, limit)` = `store.search(query, limit)` → 逐 hit `uuids.binary_search` → Vec<u32>（跳过 None=回收站/未知 uuid 容错）；impl `FtsNameSource`。

## 阶段 3 — search.rs：HybridSearchProvider + 大小写统一

- [ ] 3.1 红灯：路由判定四用例（`>=3 且 fts 有` → NameIn；`>=3 无 fts` 回落 NameContains；`<3` 恒内存；空查询 Err）；scope 四档子句集穷举（FileName 无分类子句等）；大小写修复用例（fuzzy ASCII 混大小写命中）；**oracle 一致性测试**：随机名集 × 随机 ≥3 字符查询，NameIn 集 == 内存暴力扫描集（proptest 或固定表；trigram 边界长度显式豁免）。
- [ ] 3.2 实现：`SearchScope` 枚举 + trait 签名加 scope；`fuzzy_filter` 双侧大小写不敏感化（共享 §1 工具函数）；`FacetSearchProvider` 改名/退化为 Hybrid。
- [ ] 3.3 调用点同步：main.rs:693-698 构造改 Hybrid + 传 scope + fts；search.rs 内既有单测夹具适配。

## 阶段 4 — 壳层：范围下拉 UI

- [ ] 4.1 appwindow.slint：搜索框前缀下拉（四档；UiEnums 收口 `search-scope` int，D32 纪律；开合浮层用两段式动效——新弹层出生即正确，不等 motion）。
- [ ] 4.2 main.rs：`current_query`/`current_scope` Cell 缓存；切档用缓存查询立即重跑（复用 on_search_changed 内核抽函数）；filter_label 文案带范围（「搜索「X」· 仅文件名」）。
- [ ] 4.3 回收站覆盖：确认四档 × 两路在删除后视图即时不含回收站素材（集成测试：真库 fixture 删一张 → 四档查询均不含）。

## 阶段 5 — bench + 收口

- [ ] 5.1 bench-harness 查询延迟场景（10 万行：FTS 路 vs 内存路 p50/p95），结果追加 `research/latency-ledger.md` 新节。
- [ ] 5.2 三道门全绿；D52 落点/边界（ASCII 折叠差异、trigram 下限）回写 DECISIONS.md + spec（store database-guidelines 增「软删条目 FTS 行在、查询侧必须 JOIN 过滤」；index guidelines 增 NameIn 语义与无分配匹配纪律）。
