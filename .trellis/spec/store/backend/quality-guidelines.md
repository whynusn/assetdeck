# Quality Guidelines — store

## 红线

1. **rusqlite 必带 `bundled` feature**：自带 SQLite 源码，免系统依赖（windows-gnu 下经 scoop mingw gcc shim 编译）。
2. **FTS5 trigram 四约束不可破**：详见 database-guidelines.md 表格——tokenizer 固定 trigram、查询连续子串 ≥3 字符、引号短语包裹、MATCH 左侧真实表名。2 字中文查询返回空是**已知限制，禁止"修复"**。
3. **外键必须开**：`PRAGMA foreign_keys=ON` 在 init() 强制。
4. **schema 版本守卫**：见 error-handling.md。

## 测试要求（M2 红灯测试集）

- `migration_v1_creates_assets_fts_tags_tables`
- `fts_search_chinese_filename_hits_trigram`
- `metadata_roundtrip_survives_reopen`
- `schema_version_rejects_newer_db_file`
- `thumbnail_cache_path_stable_per_asset_id`

新增迁移 = 追加 MIGRATION_V{n} 常量 + init() 分支 + bump 常量 + 新红灯测试。

## Code Review 清单

- [ ] 新 SQL 是否在 JOIN 里用别名做 MATCH？（禁止）
- [ ] ON CONFLICT UPDATE 路径的 FTS 触发器是否覆盖？
- [ ] 临时测试库是否用 in-memory / tempfile？

参考：`.trellis/spec/store/backend/database-guidelines.md`（已沉淀踩坑）。
