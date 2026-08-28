# Logging Guidelines — pipeline

- 注入类操作必须有可观测性（出问题用户第一直觉是「没发出去」）：
  - 每次粘贴：`debug!` 记录 资产 uuid、协商格式、焦点校验结果、是否合成 Enter；
  - 降级（CopiedOnly）：`warn!`。
- **禁止**在日志记录剪贴板内容本体（可能含用户敏感素材）——只记格式与大小。
- auto-send 开关状态变化记 `info!`（审计用途：误发事故回溯）。
- 注入前最后一道门的状态快照记 `debug!`（target=`paste_trace::pipeline`，D39 补充）：alive/前台 HWND/焦点结局/signal_verified，与平台事件明细同一 grep 域。
