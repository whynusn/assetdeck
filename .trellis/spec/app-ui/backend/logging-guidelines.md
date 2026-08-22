# Logging Guidelines — app-ui

- main.rs 负责 tracing subscriber 初始化（引入后）；级别默认 info，`--verbose`/环境变量开 debug。
- 启动序列记 `info!`：版本、库路径、worker 池启动结果。
- 禁止记录：用户素材内容、完整窗口标题列表（隐私）。
