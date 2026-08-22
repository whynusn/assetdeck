# Backend Guidelines — library

> .library 管理、异步拷贝队列与导入编排。M3 已完成。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/library` |
| 依赖 | store, phash, image, uuid |
| 角色 | Eagle 式复制入库（D7）+ 导入去重 + 待分类收件箱（D5）+ 视频任务派发（D6） |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 编排门面 + Mutex/Condvar 队列 + Dispatcher trait |
| [Database Guidelines](./database-guidelines.md) | uuid↔AssetId 映射、双位置一致性、缩略图路径 |
| [Error Handling](./error-handling.md) | LibraryError 收敛三层错误 |
| [Quality Guidelines](./quality-guidelines.md) | pHash 先算后拷等五条红线 |
| [Logging Guidelines](./logging-guidelines.md) | 观测点规划 |

## 关键事实速记

- enqueue 返回三态：`Ticket` / `Duplicate { existing_uuid }` / `Backpressure`。
- `.library` 布局：`meta.db` + `objects/{uuid}/raw.{ext}` + `thumbs/`。
- 参考：`crates/library/src/lib.rs`、`crates/library/tests/import_pipeline.rs`。
