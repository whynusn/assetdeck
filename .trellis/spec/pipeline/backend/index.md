# Backend Guidelines — pipeline

> M6 兼容入口与 M8 精确目标入口并存。M8 核心终点是素材进入已锁定窗口的输入框，不包含发送。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/pipeline` |
| 依赖 | domain, platform(trait), targets |
| 角色 | 编排剪贴板、精确 HWND 激活、就绪度三态、前台复核与 Ctrl+V 注入 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | M6/M8 双入口、targeted 顺序、send 隔离 |
| [Database Guidelines](./database-guidelines.md) | 配置持久化边界 |
| [Error Handling](./error-handling.md) | 降级优先（焦点校验失败 → 仅复制） |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ 精确目标七段式契约与守卫测试 |
| [Logging Guidelines](./logging-guidelines.md) | 注入审计、禁记剪贴板内容 |

## 关键事实速记

- `paste_targeted()` 顺序固定为：协商 → 写剪贴板 → 精确 HWND 激活 → readiness → 最终前台复核 → Ctrl+V。
- `ReadinessSignal::Blocked` 不注入；`Inconclusive` 可注入但结果必须是 `verified=false`。
- `paste_targeted()` 不调用 `send()`，即使 `PasteConfig.auto_send=true` 也不合成 Enter。
- 真实 `Win32Readiness`：无效/disabled HWND → `Blocked`（`WindowGone`/`ModalBlocking`，win32.rs blockers 仅此两项 O(1) 检查）；其余走 UIA 浅探——探得可写输入框（全局焦点或后代）→ `Ready`，探不到 → `Inconclusive`。“无会话/只读”类否证仍仅由 Mock 契约证明。
- 参考：DECISIONS.md D8/D13、TDD_PLAN M6/M8。
