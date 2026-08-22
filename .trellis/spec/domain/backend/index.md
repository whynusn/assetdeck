# Backend Guidelines — domain

> 实体与查询模型 crate：纯数据与纯函数，零 IO。TDD 主战场之一（M1 已完成部分）。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/domain` |
| 依赖 | 仅 serde |
| 角色 | Asset/Category/Filter/Sorter/SmartFolder 定义；被所有层引用 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 单 lib.rs + 内联测试的组织方式 |
| [Database Guidelines](./database-guidelines.md) | 与持久层的边界契约（ID 映射、时间戳） |
| [Error Handling](./error-handling.md) | 纯函数层不定义错误类型 |
| [Quality Guidelines](./quality-guidelines.md) | 零 IO 红线、derive 全集、serde roundtrip |
| [Logging Guidelines](./logging-guidelines.md) | 零日志原则与 workspace 日志基线 |

## 关键事实速记

- `Filter` 是递归谓词树，求值在 index 层（位图运算），domain 只承载结构。
- `Sorter` 与 Filter 解耦（M1 决策：`sorter_decoupled_from_filter_pipeline_order`）。
- 参考：`crates/domain/src/lib.rs`。
