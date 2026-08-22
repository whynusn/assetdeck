# Backend Guidelines — worker

> 解码 worker 进程池：IPC 协议、监督重启、背压。M4 已完成(2026-08-22)。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/worker` |
| 依赖 | media(任务类型), serde + 解码实现依赖 |
| 角色 | D11 载体：UI 主进程零解码；缩略图/抽帧/pHash 全在此进程池 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 进程模型、IPC 协议模块、M4 测试清单 |
| [Database Guidelines](./database-guidelines.md) | 产物落点、不直接写 meta.db |
| [Error Handling](./error-handling.md) | 单 job 隔离、重启上限、degraded 状态 |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ 解码依赖只进本 crate 等红线 |
| [Logging Guidelines](./logging-guidelines.md) | 协议通道与日志通道分离 |

## 关键事实速记

- M4 五个红灯测试 + degraded 补充测试全部落地（pool_spec.rs，6/6）。
- 协议：stdio NDJSON，`{"v":1,"req"/"res":…}`；任务 Echo/ThumbnailPng；可执行契约见 directory-structure.md 的 Scenario 节。
- 参考：`crates/worker/src/{lib.rs,protocol.rs}`、DECISIONS.md D11、TDD_PLAN M4。
