# Directory Structure — pipeline

## 实际布局

```
crates/pipeline/
├── Cargo.toml
├── src/
│   ├── lib.rs        # M6 兼容入口 + M8 targeted 编排 + 独立 send()
│   ├── negotiate.rs  # AssetPayload × Profile 有序格式回落
│   └── feedback.rs   # PasteFeedback 统一降级文案
└── tests/
    └── target_routing_spec.rs
```

## M8 targeted 阶段（D13 锁定，不可重排）

```
资产载荷 → profile 有序格式协商 → 写剪贴板 → 激活 target.hwnd
→ readiness 三态 → is_alive + foreground 最终复核 → 合成 Ctrl+V
```

- 每阶段经纯函数或 platform trait 边界；格式协商读取 `Profile.formats` 的有序列表。
- 剪贴板必须先写：后续无目标、休眠、NotReady、激活失败都降级为“已复制”。
- `paste_targeted()` 绝不调用 `send()`；自动发送只能由显式独立命令触发。
- M6 的 `paste()` 与 `previous_foreground` 仍作为兼容入口保留，不得把其自动发送行为误写成 M8 产品路径。

## 当前边界

- pipeline 不读取目标配置文件，不枚举窗口，不导入 `platform::win32`。
- `AssetPayload` 由调用方提供。当前 app-ui 仍传演示文本，不代表真实图片/文件素材链路已交付。
