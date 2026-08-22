# Error Handling — media

## 接口层无错误类型

- 任务派发（dispatch）是 fire-and-forget：失败在 worker 侧处理并通过 IPC 结果回报，不在接口层传播。
- 结果回报的错误形态由 worker crate 的 IPC 协议定义（M4 `job_result_roundtrips_over_ipc_protocol`）；media crate 只保证任务描述类型可序列化（serde derive）。

## 禁止

- 在 trait 方法签名引入 Result 强迫所有实现者处理派发失败——背压/降级策略属于 worker 池的内部决策。
