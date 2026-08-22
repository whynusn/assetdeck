# Logging Guidelines — store

## 现状：无日志框架

- M3 阶段代码库未引入 log/tracing；store 层静默返回 Result，由 library/app 层决定上报方式。
- 引入 tracing 后的本层约定：
  - `open()` 失败、schema 版本拒绝：`warn!`（含 found 版本号）；
  - 迁移执行：`info!`（v1→vN）；
  - 单条 CRUD 不打日志——导入批量路径的观测在 library 层做。

## 禁止

- 日志里输出用户文件完整路径以外的敏感内容（本项目暂无密钥类数据，保持现状即可）；
- 在 FTS search 热查询路径打日志（浏览路径高频调用）。
