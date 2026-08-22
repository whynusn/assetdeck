# Backend Guidelines — platform

> 平台抽象 trait + Win32 实现：剪贴板 / SendInput / 前台窗口。当前为占位，M6 实施。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/platform` |
| 依赖 | trait 部分零依赖；win32 模块用 windows crate |
| 角色 | 依赖图最底层：`{domain,index,store,library,pipeline} → platform(trait)` |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | trait 与 win32 impl 分模块、v2 预留 |
| [Database Guidelines](./database-guidelines.md) | 剪贴板格式常量归属 |
| [Error Handling](./error-handling.md) | GetLastError → Err、UIPI 边界 |
| [Quality Guidelines](./quality-guidelines.md) | trait 零依赖红线、D12 平台事实 |
| [Logging Guidelines](./logging-guidelines.md) | 错误上下文、隐私禁令 |

## 关键事实速记

- Wayland/v2 分层方案归档在 DECISIONS.md 第四节，v1 不实现。
- 参考：DECISIONS.md D8/D12、TDD_PLAN M6。
