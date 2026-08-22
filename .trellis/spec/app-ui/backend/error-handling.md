# Error Handling — app-ui

- 启动失败（窗口创建、事件循环）允许 `expect` + 中文 panic 消息——进程无法继续的场景。
- 运行期错误一律经 VM 转 UI 状态（toast/占位图），main 层不处理业务错误。
- worker 进程崩溃对本进程不可见（隔离边界），UI 只响应池状态事件。
