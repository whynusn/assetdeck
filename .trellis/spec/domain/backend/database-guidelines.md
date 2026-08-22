# Database Guidelines — domain

> domain 不直接接触数据库；本文件描述它与持久层的边界契约。

## 边界

- domain 定义的可序列化模型（`SmartFolder { name, filter, sorter }`）由 store 层落库（JSON 列或独立表均可，见 store/database-guidelines.md 的迁移规则）。
- `domain::AssetId(u32)` 是**索引层运行时标识**；store 层主键是 `uuid TEXT`。两者映射发生在 library 编排层——domain/store 互不知晓对方存在。
- 时间戳约定：i64 Unix 秒（`created_at` / `imported_at`）。新增时间字段沿用此粒度，除非有毫秒级排序的明确需求并写明理由。
