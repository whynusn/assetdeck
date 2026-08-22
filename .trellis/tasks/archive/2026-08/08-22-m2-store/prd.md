# M2 存储层：SQLite 持久化与 FTS5 中文检索

## 需求

store crate 提供 .library 的元数据持久化底座：

1. **迁移系统**：v1 建 `assets` / `tags` / `assets_fts`（FTS5）表 + 同步触发器；`PRAGMA user_version` 记录版本
2. **中文全文检索**：FTS5 trigram tokenizer（锁定决策：unicode61 不切 CJK；trigram 子串匹配要求查询 ≥3 字符）
3. **元数据往返**：写入 → 关闭 → 重新打开 → 读回一致
4. **schema 版本守卫**：打开 `user_version` 高于当前支持的库文件必须报错拒绝（防降级写坏）
5. **缩略图缓存路径**：由资产 UUID 确定性生成两级分片路径，稳定可重算

## 约束

- rusqlite bundled（自带 SQLite，免系统依赖）；windows-gnu 下经 mingw gcc 编译（M0 已备）
- 外键开启；uuid 为资产主键（domain 的稠密 u32 是索引层概念，映射在 library 层完成——跨层边界）

## 验收标准

- [ ] 5 个红灯测试全绿：migration / trigram / roundtrip_reopen / schema_guard / thumb_path
- [ ] 全工作区 fmt / clippy -D warnings / test 三绿
- [ ] TDD_PLAN.md 勾选并提交

## 范围外

- 与 FacetIndex 的同步编排（M3 library 层）、缩略图文件的实际读写、pHash 计算
