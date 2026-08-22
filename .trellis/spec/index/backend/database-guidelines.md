# Database Guidelines — index

> index 是纯内存层，无数据库。本文件记录它与 store 的标识映射契约。

## 标识映射契约

- `FacetIndex` 以紧凑 `u32`（`domain::AssetId.0`）为键，RoaringBitmap 只吃 u32。
- 持久层主键是 `uuid TEXT`（store/assets 表）。**uuid ↔ AssetId 映射发生在 library 编排层**，index 与 store 互不依赖。
- 从库重建索引的路径：library 层读全量 AssetMeta → 分配/恢复 AssetId → 逐条 `insert(&Asset)`。索引是可重建缓存，不是事实源。

## 内存预算参考（D3/D10）

- 位图常驻几 MB / 百万条；assets HashMap（完整 Asset 元数据驻留）是最大头——若 M5/M7 实测超预算，优先把冷字段（如 tags 明细）下沉为 id→store 反查，而不是动位图。
