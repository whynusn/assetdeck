# Database Guidelines

> store crate 的数据库模式与约定（SQLite via rusqlite bundled）。

---

## Overview

- **引擎**：rusqlite + `bundled` feature（自带 SQLite ≥3.4x，免系统依赖；windows-gnu 下经 scoop mingw 的 gcc shim 编译 C 源）。
- **版本守卫**：`PRAGMA user_version`；打开时 `found > SUPPORTED_SCHEMA_VERSION` 必须报 `UnsupportedSchemaVersion` 拒绝，防止旧代码写坏新库。
- **外键**：连接初始化必须 `PRAGMA foreign_keys=ON`。
- **资产主键**：`uuid TEXT`。`domain::AssetId(u32)` 是索引层运行时概念，两者映射发生在 library 层，store 不感知。

## FTS5 中文检索（硬约束，均有测试锁定）

| # | 约束 | 后果/测试 |
|---|---|---|
| 1 | tokenizer 必须 `trigram`（unicode61 不切 CJK） | `fts_search_chinese_hits_trigram` |
| 2 | 查询必须是**连续子串且 ≥3 字符**；2 字中文查询返回空集 | 测试固化该限制，勿"修复" |
| 3 | 用户查询**必须包裹为引号短语** `"..."`（内嵌引号双写转义）；裸词遇前导标点（如 `.jpg`）直接 FTS5 语法错误 | search() 统一处理 |
| 4 | `MATCH` 左侧必须是**真实表名**，不能用别名（`f MATCH` → "no such column: f"） | SQL 书写规范 |
| 5 | assets_fts 列为 `(uuid UNINDEXED, name)`；其他字段一律 JOIN 回 assets 取 | search() 的 JOIN 形态 |

## 软删除与 FTS（v4 起，D46）

- **FTS 行不随软删移除**：`deleted=1` 墓碑行的 assets_fts 条目仍在（恢复路径免重建），
  一切检索 SQL **必须** JOIN 回 assets 后加 `deleted=0` 过滤。新查询忘加 = 回收站内容
  混进搜索结果。测试锁定：`search_excludes_deleted`。
- 占号不显形：墓碑行保留 rowid 槽位，uuid→行号二分（ui-viewmodels 的 D52 契约）不得
  因行回收而错位；行回收只在整库重建/清库时发生。

## Query Patterns

- 写路径统一 `upsert_asset`：事务内 `INSERT .. ON CONFLICT(uuid) DO UPDATE` + tags 全量重写（DELETE+INSERT OR IGNORE）。
- **注意**：ON CONFLICT 走 UPDATE 分支时触发的是 UPDATE 触发器而非 INSERT 触发器——依赖触发器同步 FTS 时两个都要建。
- 错误统一收敛到 `StoreError::{Sqlite, UnsupportedSchemaVersion{found}}`，实现 `std::error::Error`。

## Migrations

- 内嵌于源码常量（`MIGRATION_V1`），`execute_batch` 执行；按 `user_version` 逐级推进。
- 新增迁移 = 追加 `MIGRATION_V2` 常量 + 在 `init()` 补分支 + bump `SUPPORTED_SCHEMA_VERSION`，并新增对应红灯测试。

## Naming Conventions

- 表名小写复数（assets/tags/assets_fts）；FTS 触发器 `<table>_fts_{ai,au,ad}`。

## Common Mistakes

1. 给 `Store` 忘记 `#[derive(Debug)]` → 集成测试 `Result<Store,_>` 的 `{:?}` 编译失败。
2. 直接把用户输入当裸词传给 MATCH → 见上表 #3。
3. 在 JOIN 里用别名做 MATCH 左侧 → 见上表 #4。
