# Logging Guidelines — media

- 纯接口 crate，零日志。
- 任务生命周期的日志在 worker 侧（任务接收/完成/失败）与 library 侧（派发）记录，media 不重复。
