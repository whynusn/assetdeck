# M2 设计

## Schema v1

```sql
CREATE TABLE assets (
  uuid        TEXT PRIMARY KEY,
  file_name   TEXT NOT NULL,
  rel_path    TEXT NOT NULL,
  category    TEXT,
  size_bytes  INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL DEFAULT 0,
  imported_at INTEGER NOT NULL DEFAULT 0,
  phash       BLOB
);
CREATE TABLE tags (
  asset_uuid TEXT NOT NULL REFERENCES assets(uuid) ON DELETE CASCADE,
  tag        TEXT NOT NULL,
  PRIMARY KEY (asset_uuid, tag)
);
CREATE VIRTUAL TABLE assets_fts USING fts5(uuid UNINDEXED, name, tokenize='trigram');
-- 触发器保持 assets.file_name ↔ assets_fts 同步（INSERT/UPDATE/DELETE）
PRAGMA user_version = 1;
```

## 关键决策

| 决策 | 理由 |
|---|---|
| FTS5 trigram tokenizer | unicode61 不切 CJK；trigram 支持子串匹配。**限制：查询须 ≥3 字符**，2 字查询返回空——以测试固化该行为 |
| uuid TEXT 主键 | 与 domain::AssetId(u32) 解耦，稠密 id 是索引层运行时概念 |
| user_version 守卫 | 打开 found > SUPPORTED 即报 UnsupportedSchemaVersion，防旧代码写坏新库 |
| rusqlite bundled | 免系统 sqlite 依赖，版本可控（≥3.34 才有 trigram） |

## API 面

```rust
Store::open(path) -> Result<Self>          // 自动跑迁移 + 版本守卫
Store::open_in_memory() -> Result<Self>    // 测试用
schema_version() -> i32
upsert_asset(&AssetMeta) / get_asset(&str) / delete_asset(&str)
search(&query, limit) -> Vec<SearchHit{uuid, file_name}>
thumbnail_cache_path(uuid: &str, ext: &str) -> PathBuf   // 纯函数，无需连接
```

## 错误模型

`StoreError::{Sqlite, UnsupportedSchemaVersion { found }}`，实现 `std::error::Error`。
