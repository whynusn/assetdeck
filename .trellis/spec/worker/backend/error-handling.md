# Error Handling — worker

- worker 进程内部：单 job 失败 → 回报 `JobResult::Failed { reason }`，继续下一个任务；不可恢复（panic/OOM）→ 进程消亡，由监督者重启。
- 池侧：重启计数有上限策略（M4 定型时写入 design.md），超过则池进入 degraded 状态并向 UI 报告——禁止无限快速重启循环。
- UI 侧视角：worker 层错误最终以「缩略图占位 + 重试入口」呈现，永不弹崩溃对话框。
