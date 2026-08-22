# Directory Structure — library

## 布局

```
crates/library/
├── Cargo.toml          # deps: store, phash, image(png,jpeg), uuid(v4)
├── src/
│   └── lib.rs          # Library 编排 + 异步拷贝队列 + MediaDispatcher trait
└── tests/
    └── import_pipeline.rs
```

## 模块组织规则

- `Library` 是编排门面：同步路径（解码→pHash→去重→落库）+ 异步路径（拷贝工作线程）。
- 与 worker 的解耦点：`MediaDispatcher` trait（`fn dispatch(&self, job: MediaJob)`）。测试用 `RecordingDispatcher` 收集断言，生产用 NullDispatcher 占位（M4 接真 worker）。
- 共享队列状态模式：`Arc<(Mutex<Shared>, Condvar)>`，`Shared { queue, states, active, paused }`。

## 命名约定

- 库目录布局常量：`objects/{uuid}/raw.{ext}`、`thumbs/`；「待分类」收件箱常量 `INBOX_CATEGORY`（D5）。
- 测试确定性钩子：`set_paused(bool)` 暂停拷贝线程，保证队列语义可断言——新增异步行为必须配同类钩子或轮询等待辅助。
