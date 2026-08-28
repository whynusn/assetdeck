# Logging Guidelines — domain

## 零日志原则

- domain 是纯数据层，**禁止引入 log/tracing 等任何日志门面**。
- 可观测性需求由调用方负责：library/store 层记录操作结果时携带 domain 类型的 Debug 输出即可。
- 若调试需要，临时 `dbg!` 不允许提交；用测试断言代替打印。

## 全 workspace 日志基线（供上层参考）

- 日志框架已落地（D38）：统一走 `log` 门面，`crates/logging` 提供文件 sink，在 app-ui 二进制入口初始化。
- 日志目录解析（D39 补充）：`DSH_LOG_DIR` > 调用方 fallback > 平台标准目录 `%LOCALAPPDATA%\asset-manager\logs`，**任何进程不得把日志写进当前工作目录**。
- 错误路径优先返回 `Result` 而非打日志——错误处理见各 crate 的 error-handling.md。
