# Directory Structure — platform

## 布局（M6 目标形态，当前为占位）

```
crates/platform/
├── Cargo.toml    # trait 部分：零依赖；win32 impl：windows/windows-rs crate
└── src/
    ├── lib.rs        # trait 定义：Clipboard / Injector / WindowProvider
    └── win32/        # cfg(windows) 实现
```

## 模块组织规则

- **trait 与实现同 crate 分模块**（非双 crate）：依赖方只 use trait；二进制入口（app-ui）负责选择 win32 实现注入。
- v2 Linux 分层预留：新增平台 = 新增 `src/<platform>/` 模块 + cfg 门，trait 不动。
- 仅 Windows 红线的编译期体现：win32 模块整体 `#[cfg(windows)]`；CI 仅 windows runner。

## 命名约定

- trait 方法名面向意图（`write_hdrop`, `send_paste`, `foreground_window`），不暴露 Win32 术语于签名之外。
