# Error Handling — pipeline

## 降级优先于误注入（D13 红线）

- targeted 管线先写剪贴板。无目标、HWND 失效、激活失败、明确 NotReady、前台漂移和注入失败均返回 `CopiedOnly`，不得强注。
- 剪贴板写入失败返回 `Failed{feedback}`；此时不能声称“已复制”。
- `ReadinessSignal::Blocked(reason)` 是明确否证，绝不注入。
- `ReadinessSignal::Inconclusive` 是探测能力不足，可注入但返回 `Injected{verified:false}`，不能伪装成 Ready。
- 反馈必须回显目标名、先说明已复制、给一个可执行动作；技术细节只放 `diagnostic`。

## 错误形态

- M8 使用 `TargetPasteOutcome::{Injected{verified}, CopiedOnly{feedback}, Failed{feedback}}`。
- M6 兼容入口仍使用 `PasteOutcome`；新增代码不得混淆两个入口的语义。

## 禁止

- 校验失败时“重试后强注”。
- 把 UIA 超时直接映射为 `NotReady`。
- 在未确认前台 HWND 等于锁定 HWND 时调用注入器。
