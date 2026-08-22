# Backend Guidelines — index

> RoaringBitmap 分面索引：Filter 求值、facet 计数缓存与失效。M1 已完成。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/index` |
| 依赖 | domain, roaring |
| 角色 | 分类/标签/属性过滤的位图求值引擎（D4 核心） |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | src + tests(pipeline/oracle/budget) + benches 布局 |
| [Database Guidelines](./database-guidelines.md) | uuid↔AssetId 映射边界、内存预算 |
| [Error Handling](./error-handling.md) | 无 IO 错误；缺失 facet = 空位图 |
| [Quality Guidelines](./quality-guidelines.md) | 禁全量载入、缓存失效纪律、proptest/criterion 要求 |
| [Logging Guidelines](./logging-guidelines.md) | 热路径禁日志 |

## 关键事实速记

- `insert` 为 upsert 语义；`evaluate` 递归求值 Filter 树；`tag_counts()` 带变更即失效缓存。
- criterion 基线 @1M：交集 126µs / 单面 3.2µs / 全集 11.8µs（budget.rs 断言防回归）。
- 参考：`crates/index/src/lib.rs`、`crates/index/tests/oracle.rs`。
