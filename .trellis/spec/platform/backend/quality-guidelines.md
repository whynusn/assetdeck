# Quality Guidelines — platform

## 红线

1. **trait 零依赖**：lib.rs 的 trait 定义不得引入 windows crate——保证 pipeline/ui-viewmodels 可脱离 Win32 编译与测试。
2. **仅 Windows（v1）**：win32 实现整体 cfg 门；禁止 Unix-only API 混入。
3. **SendInput/剪贴板属实现细节**：业务 crate 出现 `SendInput`/`CF_HDROP` 字样即违规（应在 platform 或 pipeline 的格式协商表内）。

## 测试要求

- trait 层：mock 实现即可测（pipeline 的 mock WindowProvider 是范例）。
- win32 实现：真实注入类测试一律 `#[ignore]` 本地手动跑，CI 不跑。

## 已知平台事实（D12）

- 焦点校验：`GetForegroundWindow` + 进程/窗口标题匹配；
- UIPI：管理员权限窗口收不到普通进程 SendInput → 降级为复制 + toast；
- 管理员运行自身可绕过 UIPI，但产品不要求管理员权限。
