# PRD — 搜索：范围下拉 + 大小写统一 + FTS5 混合路由（D51/D52）

> 决策来源：`DECISIONS.md` D51（搜索范围）、D52（混合路由，修订 D30 排期）。
> 词汇：搜索范围（见 CONTEXT.md，避免「搜索类型」）。

## 前置依赖（必须在 crud 之后动工）

- D52 的「≥3 字符走 FTS5」必须排除回收站素材：依赖 crud 的 schema v4 `deleted` 列与 `Store::search` 的 `deleted=0` JOIN 过滤；`search_names` 内存扫描依赖 index 的 `deleted` 位图（活行集 `all` 不含 tombstone）。

## Goal

搜索框支持字段范围选择且全链路大小写不敏感；十万至百万级库下键入不再线性扫描全库文件名。

## Requirements

### 搜索范围（D51）

- R1 搜索框前缀下拉四档：**全部 / 文件名 / 分类 / 标签**。语义 = 本次查询应用哪些过滤器；默认「全部」= 现行为（分类名子串 ∪ 标签名子串 ∪ 文件名 NameContains）。
- R2 大小写统一不敏感：修复现状不一致——分类/标签名匹配当前**区分大小写**（`catalog_loader.rs` fuzzy_filter 的 `str::contains`），文件名不区分。统一为全部不区分。
- R3 备注（notes）字段搜索明确推迟（等字段落地后另议）。

### 混合路由（D52）

- R4 键入 ≥3 字符 → 文件名侧走 `Store::search`（FTS5 trigram 倒排）；1–2 字符 → 回落内存扫描（`Filter::NameContains` 线性）。两路统一经 D30 `SearchProvider` 单一入口，UI 不感知路由。
- R5 trigram ≥3 字符下限是 SQLite FTS5 分词器固有机制，**不可绕**（换 unicode61 毁中文子串，D52 已定）；混合路由对用户隐藏此限制。
- R6 内存扫描侧治理每键字符串分配（现状：每名字每次按键 `to_lowercase()` 分配一个新 String——index/src/lib.rs:136）：改为无分配匹配（`str::eq_ignore_ascii_case` 滑窗或 `unicase` 级预归一化，选型进 design）或每查询一次归一化。
- R7 FTS5 结果 uuid→AssetId 映射实现零新增常驻内存（D52 连带义务①；设计事实：`RealAssetResolver.uuids` 按 uuid 升序，二分即得行号）。
- R8 搜索结果的回收站素材恒不可见（依赖 crud；含「全部/文件名/分类/标签」四档 + FTS/内存两路 = 全路径覆盖）。
- R9 ASCII 折叠边界：trigram 默认 ASCII caseless，内存路 Unicode 小写；对「中文+ASCII 文件名」无感，记为已知边界写进 spec（不做治理）。

## Constraints

- C1 D10 内存预算：FTS 接线不得为 uuid 映射引入新 HashMap/Vec 常驻（≤ O(结果集) 瞬时分配）；索引侧常驻内存增量 = 零。
- C2 键入响应目标：10 万条库、任意长度查询单次重建候选集 ≤ 30ms（现状老设备 10–30ms/键为线性扫描恰好达标，百万级 100ms+/键不可用——路由后百万级应回 30ms 内）。
- C3 D4：检索层候选集抽象不变（SearchProvider 返回 Filter，不返回结果列表）。
- C4 不新增向量检索/新分词器依赖（v1 红线）。

## Acceptance Criteria

- [ ] 四档范围 × 大小写组合的纯函数测试表绿（含 ASCII 混大小写查询中文库名/分类名的不敏感命中）。
- [ ] ≥3 字符查询走 FTS、1–2 字符走内存——路由判定单测 + 集成测试各一（假 Store 计数调用路径）。
- [ ] 结果集与「暴力全扫 + 内存过滤」oracle 一致（proptest：随机 ASCII/CJK 名集 × 随机查询，FTS 路与内存路结果集相等；trigram 边界长度显式豁免）。
- [ ] uuid→行号二分映射测试：乱序 uuid 集合断言升序不变量 + 二分命中；**无新增常驻**由代码审查保证（无 lazy_static 映射）。
- [ ] `search_names` 无每名字分配（bench 或 miri 级断言可选；至少实现侧评审确认）。
- [ ] 百万级 bench（bench-harness 既有生成器）：≥3 字符查询候选集重建延迟进报告，回归对比 10 万条基线。
- [ ] 三道门绿；D52 落点回写 DECISIONS.md，trigram/ASCII 边界进 spec。
