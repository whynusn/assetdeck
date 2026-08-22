# Backend Guidelines — store

> SQLite 持久化、FTS5、迁移与 smart folder 序列化底座。M2 已完成。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/store` |
| 依赖 | rusqlite(bundled) |
| 角色 | 唯一事实源：assets/tags/assets_fts；schema 版本守卫 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 单 lib.rs、SQL 书写规范 |
| [Database Guidelines](./database-guidelines.md) | ⭐ 已填：FTS5 trigram 四约束（踩坑沉淀） |
| [Error Handling](./error-handling.md) | StoreError 形状、事务纪律 |
| [Quality Guidelines](./quality-guidelines.md) | bundled 强制、M2 红灯测试集 |
| [Logging Guidelines](./logging-guidelines.md) | 观测点规划 |

## 关键事实速记

- 表：`assets`(uuid 主键) / `tags` / `assets_fts`(fts5 trigram) + 三个同步触发器。
- `thumbnail_cache_path(uuid, ext)` 是缩略图路径唯一出处。
- 参考：`crates/store/src/lib.rs`、`.trellis/spec/store/backend/database-guidelines.md`。
