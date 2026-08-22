# Error Handling — library

## 错误类型（已定型）

```rust
pub enum LibraryError {
    Store(store::StoreError),
    Io(std::io::Error),
    Image(image::ImageError),
}
```

- Display + std::error::Error + 三个 `From` 实现；crate 级 `Result<T>` 别名。
- 新增依赖产生的错误类别 → 加变体 + From，禁止 anyhow 泄漏到公共 API。

## 失败语义分层

- **同步路径**（enqueue 内的解码/pHash/落库）：错误直接返回给调用方，不入队。
- **异步路径**（拷贝线程）：失败转 `CopyState::Failed(String)` 并触发 rollback（删残留文件 + 删 meta.db 行）——「无半成品」不变量。UI 通过 `state_of(ticket)` 轮询感知。
- 去重命中不是错误：`EnqueueOutcome::Duplicate { existing_uuid }`。

## 禁止

- 在 worker_loop 里 panic——拷贝线程 panic 会静默卡死 active 计数（背压泄漏）；一切 IO 错误走 Failed 状态。
