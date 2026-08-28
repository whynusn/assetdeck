# Directory Structure — store

## 布局

```
crates/store/
├── Cargo.toml          # deps: rusqlite(bundled)
├── src/
│   └── lib.rs          # Store + StoreError + MIGRATION_V1 + AssetMeta/SearchHit
└── tests/
    └── store_spec.rs   # 集成测试：迁移/FTS/roundtrip/版本拒绝/缩略图路径
```

## 模块组织规则

- 单 lib.rs 当前够用；拆分时机 = 出现第二张非资产域表（如 smart_folders 表落库时拆 `migrations.rs` + `smart_folder.rs`）。
- SQL 一律内联字符串常量或方法内字面量；**禁止**引入 sqlx/query-builder 之类的宏 DSL（windows-gnu 工具链兼容风险 + 可读性）。

## 命名约定

- 表小写复数；FTS 触发器 `<table>_fts_{ai,au,ad}`；测试名行为化（`schema_version_rejects_newer_db_file`）。
- 测试库一律 `Store::open_in_memory()` 或 tempfile 临时目录，禁止写死路径。

## 库内文件布局（纯函数生成，禁手工拼接）

- 原件：`objects/<uuid>/raw.<ext>`。
- 缩略图：`thumbs/<shard1>/<shard2>/<uuid>.<ext>`。
- 上框派生 PNG：`objects/<uuid>/paste.png`，由 `Store::paste_png_path(uuid)` 生成。
  存在动机是千牛把 `CF_HDROP` 当「直接发送文件」，只有 `CF_PNG` 才落进输入框（DECISIONS D18/D20）。
  由 worker 子进程解码产出，UI 只 `fs::read` 不解码；与 `raw.<ext>` 同目录，
  删除资产目录即连带回收，**不引入额外 GC 语义**。
