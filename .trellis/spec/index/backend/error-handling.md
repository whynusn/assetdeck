# Error Handling — index

## 设计：无错误类型

- 纯内存结构，操作不可失败。**不定义 IndexError**。
- 「不存在的 facet」不是错误：`evaluate(InCategory/HasTag)` 返回空位图（`cloned().unwrap_or_default()`），调用方无需处理错误分支。
- `remove` 不存在的 id 是 no-op（`all.remove` 返回 false 即早退）。

## 与上层的边界

- library 层编排 store→index 时，把 store::StoreError 收敛进 LibraryError；index 自身零错误传播。

## 禁止

- panic 作为错误通道（unwrap 仅限编译期可证安全的场景，如刚插入的键必然存在——并加注释说明不变量）。
