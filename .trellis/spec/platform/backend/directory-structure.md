# Directory Structure — platform

## 实际布局

```
crates/platform/
├── Cargo.toml    # windows-sys 仅在 [target.'cfg(windows)'.dependencies]，trait 层零依赖
└── src/
    ├── lib.rs        # 平台无关值类型 + clipboard/focus/window/readiness/inject trait
    └── win32.rs      # 文件内 #![cfg(windows)]：Win32 实现全集
└── tests/
    └── win32_manual.rs   # 真实注入类测试一律 #[ignore]
```

## 模块组织规则

- **trait 与实现同 crate 分模块**（非双 crate）：业务逻辑只 use trait；二进制装配层负责选择 win32 实现。
- v2 Linux 分层预留：新增平台 = 新增 `src/<platform>/` 模块 + cfg 门，trait 不动。
- 仅 Windows 红线的编译期体现：cfg 门放 **win32.rs 文件内属性** `#![cfg(windows)]`，
  而非 lib.rs 里给 `mod win32` 加门——lib.rs 因此可逐字 grep 验证纯净（无 cfg 字样、无平台 crate 引用），满足「trait 层可脱离 Windows 编译」的概念验证验收。非 Windows 目标下该模块内容被整体剥离，windows-sys 又被 target 门隔离在依赖表外，双保险。

## 命名约定

- trait 方法名面向意图（`write` / `foreground` / `windows` / `activate` / `probe` / `inject`），不暴露 Win32 术语于签名之外；平台句柄以裸值包装类型（`WindowHandle(isize)`）跨层传递。
- 注入序列的按下/释放相位协议定义在 lib.rs（`KEY_UP: u16 = 0x8000`，低 15 位为 VK）：编排方（pipeline）与解码方（win32 SendInput）必须消费同一常量，禁止各自定义魔法数。

## M8 Win32 形态

- 枚举：`EnumWindows`，采集 HWND/exe/class/title/visible/minimized/rect/process_id；隐藏但有面积的目标窗口保留为候选，零面积系统窗口仍过滤。
- 激活：restore → `SetForegroundWindow` → 轮询确认；连续切换受 Windows 前台权限限制时，轻量 Alt 敲击后重试；不负责自动启动应用。
- 观察：WinEvent 回调投递 HWND，`next_foreground()` 再形成快照；观察器失败可由上层低频枚举兜底。
- readiness：HWND 存活与 enabled 检查后，扫描目标窗口后代和系统当前焦点元素；`Edit/Document + enabled + !readonly` 才算 `Ready`。

## windows-sys 0.59 API 形态（踩坑记录）

- 剪贴板格式常量 `CF_HDROP` / `CF_DIB` / `CF_UNICODETEXT` 在 0.59 元数据中归属
  `Win32::System::Ole`（不在 DataExchange），类型为 `CLIPBOARD_FORMAT = u16`；
  `SetClipboardData` 形参是 `u32` —— 传参需 `u32::from(CF_*)` 转换。
- 需要 feature 组合：`Win32_Foundation` + `Win32_System_DataExchange` +
  `Win32_System_Memory`（GlobalAlloc 族）+ `Win32_System_Ole`（CF_*）+
  `Win32_UI_Shell`（DROPFILES）+ `Win32_UI_Input_KeyboardAndMouse`（SendInput）+
  `Win32_UI_WindowsAndMessaging` + `Win32_System_Threading`（Sleep 重试）。
- gnu 工具链兼容性已在 worker 里程碑验证过（raw-dylib 方案），platform 直接复用同版本即可。
