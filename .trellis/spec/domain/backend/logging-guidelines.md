# Logging Guidelines — domain

## 零日志原则

- domain 是纯数据层，**禁止引入 log/tracing 等任何日志门面**。
- 可观测性需求由调用方负责：library/store 层记录操作结果时携带 domain 类型的 Debug 输出即可。
- 若调试需要，临时 `dbg!` 不允许提交；用测试断言代替打印。

## 全 workspace 日志基线（供上层参考）

- 当前（M3 阶段）代码库尚未引入日志框架；引入时统一走 `tracing` 门面，在 app-ui 二进制入口初始化 subscriber。
- 错误路径优先返回 `Result` 而非打日志——错误处理见各 crate 的 error-handling.md。
