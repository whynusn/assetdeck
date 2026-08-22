# Error Handling — domain

## 原则：纯函数不定义错误类型

- 本 crate 的操作要么总成功（排序、序列化），要么是逻辑不变量问题（不该发生）。
- **禁止**在本 crate 定义 `DomainError` 之类错误枚举——IO/解析错误的收敛点是各基础设施 crate（store::StoreError、library::LibraryError）。

## 不变量违规的处理方式

- 若发现「不可能状态」，优先把非法状态编码进类型系统（如 `Option<CategoryId>` 表示未分类），而不是 panic。
- 允许 panic 的唯一场景：程序员错误（切片越界等），且必须有测试锁定前置条件。

## 上层如何消费 domain

- store/library/index 用 `From<下层错误>` 收敛自己的错误枚举，UI 层最终只见到每层一个错误类型。
