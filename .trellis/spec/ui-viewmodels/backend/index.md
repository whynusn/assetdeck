# Backend Guidelines — ui-viewmodels

> ViewModel 层：桥接 UI 与核心 crates，纯 Rust 可全量单测。当前为占位，M5 实施（关键路径最大风险项）。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/ui-viewmodels` |
| 依赖 | domain, index, store, library, pipeline |
| 角色 | TDD 第一原则的支点：业务逻辑住纯 Rust，`.slint` 哑渲染 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | VM 划分、虚拟化数据窗口职责 |
| [Database Guidelines](./database-guidelines.md) | 经门面访问、分页窗口 |
| [Error Handling](./error-handling.md) | Result → UI 状态转译 |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ 禁 slint 依赖、内存守卫红线 |
| [Logging Guidelines](./logging-guidelines.md) | 用户行为观测点 |

## 关键事实速记

- M5 风险预案：瀑布流两周 spike 达不到帧预算 → 回退等宽网格（TDD_PLAN 第十节）。
- 参考：TDD_PLAN M5、DECISIONS.md D3/D9/D10。
