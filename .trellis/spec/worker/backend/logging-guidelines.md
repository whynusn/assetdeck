# Logging Guidelines — worker

- worker 进程 stdout/stderr 保留给 IPC 协议（若走 stdio）时**禁止**混入日志——日志走文件或命名管道外的通道（M4 定型时与传输选型一并锁定，防协议污染）。
- 必记：任务接收/完成/失败（带 uuid）、进程启动/退出/重启（带 pid 与退出码）。
- 禁记：解码内容本体、用户路径以外的机器信息。
- 日志目录由宿主显式注入（`WorkerPool::with_priority_and_log_dir` → `DSH_LOG_DIR`，替补拉起同样生效）；未注入时回落平台标准目录，**禁止**把日志写进当前工作目录（D39 补充）。
