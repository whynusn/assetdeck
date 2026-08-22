# Logging Guidelines — ui-viewmodels

- VM 是用户行为的最佳观测点：导入入队/去重/失败、粘贴触发、过滤切换记 `debug!`；性能敏感路径（滚动、布局计算）禁止逐帧日志。
- 用户可感知错误（粘贴降级 toast、worker degraded）记 `warn!`。
