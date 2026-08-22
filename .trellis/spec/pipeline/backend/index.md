# Backend Guidelines — pipeline

> 粘贴管线：格式协商 → 剪贴板 → 焦点校验 → 注入 → [auto-send]。M6 已完成(2026-08-22)。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/pipeline` |
| 依赖 | domain, platform(trait) |
| 角色 | D8 的实现载体：「双击 = 素材进输入框」，回车直发独立开关默认关 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 八阶段管线、表驱动协商 |
| [Database Guidelines](./database-guidelines.md) | 配置持久化边界 |
| [Error Handling](./error-handling.md) | 降级优先（焦点校验失败 → 仅复制） |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ 五条红线与守卫测试映射 |
| [Logging Guidelines](./logging-guidelines.md) | 注入审计、禁记剪贴板内容 |

## 关键事实速记

- 闭环验收（行动项 A2）：双击素材 → 0.5s 内出现在 IM 输入框。
- 参考：DECISIONS.md D8/D12、TDD_PLAN M6。
