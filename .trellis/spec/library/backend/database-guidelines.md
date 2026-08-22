# Database Guidelines — library

> library 是 store 与 index 的编排层，持有 uuid↔AssetId 映射职责。

## 映射与重建

- store 主键 `uuid TEXT`（uuid v4 hyphenated）；index 位图键 `AssetId(u32)`。
- 当前映射关系在 Library 内存中维护；**库重开时的索引重建顺序**：Store::open → 读全量 AssetMeta → 建 FacetIndex → 恢复映射。（M5 ui-viewmodels 接入时落地，届时补集成测试。）

## 写路径契约

- 一切资产写入经 `store.upsert_asset`（事务 + tags 全量重写），library 不手写 SQL。
- 失败回滚需同时清 objects 目录残留与 meta.db 行——两个存储位置的一致性由 rollback_failed_import 保证。

## 缩略图缓存

- 路径纯函数：`store::Store::thumbnail_cache_path(uuid, ext)` → `thumbs/{u}/{uu}/{uuid}.{ext}` 两级分片。library/worker 生成缩略图时必须用它，禁止自拼路径。
