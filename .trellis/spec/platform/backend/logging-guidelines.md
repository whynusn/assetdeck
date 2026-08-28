# Logging Guidelines — platform

- 平台层是错误高发区（权限、会话、DPI），Win32 调用失败转 Err 时在 debug 级别附 GetLastError 上下文；由上层决定是否上报。
- 前台窗口枚举/焦点校验不打日志（高频 + 隐私：窗口标题可能敏感）。
- 粘贴链路事件级 trace 用 `paste_trace::platform::events`（Trace 级，D39 补充）：WinEvent 抽干明细只记 event id/object/hwnd/pid，不含窗口标题（隐私红线不变）；默认 Info 下 log 宏零格式化开销，开关走 DSH_LOG_LEVEL / verbose_diagnostics。
