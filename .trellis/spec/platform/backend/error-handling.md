# Error Handling — platform

- 每个 trait 配一个实现侧错误枚举（如 `ClipboardError`、`InjectError`），thiserror 风格与 store/library 一致（Display+Error+From）。
- Win32 API 返回 0/NULL 不是错误分支问题——立即转 Err，带 `GetLastError()` 信息；**禁止** `expect("win32 failed")`。
- UIPI 场景：SendInput 对管理员窗口静默失效（不报错）——这不是本层能检测的错误，由 pipeline 的焦点校验 + 降级路径兜底（见 pipeline/error-handling.md）。

## M8 三态映射

- HWND 无效 → `ReadinessSignal::Blocked(WindowGone)`。
- HWND disabled → `ReadinessSignal::Blocked(ModalBlocking)`。
- 探测不可用、超时或当前实现没有足够证据 → `ReadinessSignal::Inconclusive`。
- 只有平台层真正证明存在可写输入框时才能返回 `Ready`；窗口存活本身不够。
- WinEvent 观察器创建或通道失败返回 `PlatformError::Window`，上层可继续用枚举/前台采样兜底，不得伪造事件。
