# M2 执行清单（TDD 红灯 → 绿灯）

## Red：先写测试（crates/store/tests/）

1. `migration_v1_creates_assets_fts_tags_tables` — sqlite_master 含三表 + user_version==1
2. `fts_search_chinese_hits_trigram` — 插入「红色卫衣.jpg」，MATCH「红卫衣」命中；2 字查询「卫衣」返回空（固化 trigram ≥3 字限制）
3. `metadata_roundtrip_survives_reopen` — 临时文件写→关→开→字段级相等
4. `schema_version_rejects_newer_db_file` — 手动置 user_version=999，open 报 UnsupportedSchemaVersion
5. `thumbnail_cache_path_stable_per_asset_id` — 同 id 幂等；异 id 不同；含两级分片目录

## Green：实现 src/lib.rs

- Store / AssetMeta / SearchHit / StoreError
- open(): 连接 → foreign_keys=ON → user_version 检查 → 按版本迁移
- upsert: assets UPSERT + tags 全量重写 + FTS 触发器自动同步
- thumbnail_cache_path: uuid sha 片段两级分片 `thumbs/<a>/<ab>/<uuid>.<ext>`（用 uuid 自身字符即可，无需哈希）

## Check

- cargo test -p store
- cargo fmt/clippy/test 全工作区三绿

## Rollback point

单 commit，失败 revert 即回 M1 收口态。
