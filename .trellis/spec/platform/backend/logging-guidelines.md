# Logging Guidelines — platform

- 平台层是错误高发区（权限、会话、DPI），Win32 调用失败转 Err 时在 debug 级别附 GetLastError 上下文；由上层决定是否上报。
- 前台窗口枚举/焦点校验不打日志（高频 + 隐私：窗口标题可能敏感）。
