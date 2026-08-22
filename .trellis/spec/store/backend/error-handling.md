# Error Handling — store

## 错误类型（已定型，勿改形状）

```rust
pub enum StoreError {
    Sqlite(rusqlite::Error),
    UnsupportedSchemaVersion { found: i32 },
}
```

- 必须实现 `Display + std::error::Error`；`From<rusqlite::Error>` 使 `?` 直通。
- crate 级别名 `pub type Result<T> = std::result::Result<T, StoreError>;`——所有公共 API 返回它。
- **给 Store 及返回 Result 的类型加 `#[derive(Debug)]`**：否则集成测试里 `.unwrap()`/`{:?}` 编译失败（踩坑记录，见 database-guidelines.md Common Mistakes #1）。

## 版本守卫语义

- `open()` 时 `user_version > SUPPORTED_SCHEMA_VERSION` → 返回 `UnsupportedSchemaVersion { found }` 拒绝打开。这是「旧代码不写坏新库」的唯一防线，测试 `schema_version_rejects_newer_db_file` 锁定。

## 事务纪律

- 多语句写路径用 `BEGIN IMMEDIATE` / 显式 COMMIT，错误分支显式 ROLLBACK（见 `upsert_asset` 的 match-outcome 形态）。禁止依赖 drop 时隐式回滚来表达正确性。

## 禁止

- 把 rusqlite::Error 泄漏到公共 API 签名（一律包成 StoreError）。
- 用 panic 处理 SQL 失败。
