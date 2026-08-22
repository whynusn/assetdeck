# Backend Guidelines — media

> 缩略图/抽帧任务的接口定义 crate（实现在 worker）。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/media` |
| 依赖 | （应保持近零；类型可序列化时加 serde） |
| 角色 | MediaJob/MediaDispatcher 等媒体任务契约的唯一归属地 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 仅接口；MediaJob 迁移计划 |
| [Database Guidelines](./database-guidelines.md) | 产物路径与回写契约 |
| [Error Handling](./error-handling.md) | fire-and-forget 派发语义 |
| [Quality Guidelines](./quality-guidelines.md) | 防火墙红线、依赖方向 |
| [Logging Guidelines](./logging-guidelines.md) | 零日志 |

## 关键事实速记

- M3 时 `MediaJob`/`MediaDispatcher` 暂居 library/src/lib.rs；M4 接 worker 时迁入本 crate（迁移须保持 library 测试全绿）。
- 参考：`crates/library/src/lib.rs`（现状）、TDD_PLAN 第二节依赖图。
