# Logging Guidelines — library

## 现状：无日志框架

- 导入编排的观测点规划（引入 tracing 后）：
  - enqueue 结果（Ticket/Duplicate/Backpressure）：`debug!` 带 uuid 与源文件名；
  - 拷贝完成/失败：`info!` / `warn!`（失败带 uuid + 错误）；
  - 去重命中：`info!`（existing_uuid），这是用户可感知行为。

## 禁止

- 在拷贝进度回调（64KB chunk 粒度）打日志——高频热路径；
- 日志替代 CopyState：UI 查询进度走 `state_of(ticket)`，日志只是旁路观测。
