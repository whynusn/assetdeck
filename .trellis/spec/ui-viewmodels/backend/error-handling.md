# Error Handling — ui-viewmodels

- VM 层把底层 Result 转译为 UI 状态：错误以可展示消息（枚举/字符串资源）+ 重试动作暴露，不 panic。
- 后台任务（导入进度、worker 结果）经事件/回调进入 VM 时必须容错：乱序、迟到、重复消息不得破坏状态机。
- Slint 回调内的 Rust 代码禁止 unwrap 用户输入路径——解析失败显示占位并记录。

## M8 目标反馈

- VM 将 `TargetPasteOutcome` 转成 `TargetPasteNotice{tone,text}`；不得丢失“已复制”、目标名和可执行提示。
- `Injected{verified:false}` 使用 Warning，不得显示为确定成功。
- 无目标、休眠、NotReady 和前台漂移不 panic，保持选择状态并显示可恢复提示。
- 选择键不存在时 `choose()` 返回 false，不得按 profile id 或列表第一项兜底。
- 平台轮询失败可以显示 Warning，但不得清空已经锁定的热目标。
