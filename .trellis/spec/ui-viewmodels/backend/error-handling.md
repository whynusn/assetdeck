# Error Handling — ui-viewmodels

- VM 层把底层 Result 转译为 UI 状态：错误以可展示消息（枚举/字符串资源）+ 重试动作暴露，不 panic。
- 后台任务（导入进度、worker 结果）经事件/回调进入 VM 时必须容错：乱序、迟到、重复消息不得破坏状态机。
- Slint 回调内的 Rust 代码禁止 unwrap 用户输入路径——解析失败显示占位并记录。
