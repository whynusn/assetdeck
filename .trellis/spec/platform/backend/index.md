# Backend Guidelines — platform

> 平台抽象 trait + Win32 实现：剪贴板、窗口枚举/观察/激活、保守 readiness 与按键注入。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/platform` |
| 依赖 | trait 部分零依赖；win32 模块用 windows crate |
| 角色 | 提供平台事实，不做 profile 匹配、热目标状态迁移或 UI 文案决策 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | M8 trait、WinEvent/枚举/激活/readiness 实现边界 |
| [Database Guidelines](./database-guidelines.md) | 剪贴板格式常量归属 |
| [Error Handling](./error-handling.md) | GetLastError → Err、UIPI 边界 |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ 窗口路由平台契约与真实验证边界 |
| [Logging Guidelines](./logging-guidelines.md) | 错误上下文、隐私禁令 |

## 关键事实速记

- trait 层定义 `WindowSnapshot`、`WindowEnumerator`、`WindowActivator`、`ForegroundObserver`、`ReadinessProbe`。
- Win32 观察器使用 `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`；回调只投递 HWND，匹配和粘性决策在上层。
- `Win32Readiness` 已包含 UIA 全局焦点 + 目标窗口后代浅探测：探得可写输入框才返回 `Ready`，探不到返回 `Inconclusive`（不伪装成阻塞）。四个内置画像一律 `uia_shallow`，判定语义是「否证阻塞才不注入」，`Inconclusive` 照常注入并标 `verified:false`；`uia_strict` 仅为用户可显式开启的严格档，不是内置默认（DECISIONS D15）。
- 微信 4.0 在未打开聊天输入框时 UIA 树只暴露两个 Pane；进入聊天输入框后才会物化 `mmui::ChatInputField`。
- Wayland/v2 分层方案归档在 DECISIONS.md 第四节，v1 不实现。
