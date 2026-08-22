# Design — M4 Worker 进程池

## 边界

```
crates/worker/
├── src/
│   ├── lib.rs        # WorkerPool:进程生命周期、提交、监督、测试钩子
│   ├── protocol.rs   # Envelope/JobRequest/JobResult(serde 全 derive,协议唯一出处)
│   └── bin/
│       └── decode-worker.rs  # worker 进程入口:stdin 逐行读请求 → 处理 → stdout 写响应
└── tests/pool_spec.rs
```

- library 的 `MediaDispatcher` trait 保持不动;M5 由 app 装配层写一个 `WorkerPool`→`MediaDispatcher` 适配器(不在本里程碑)。
- `MediaJob` 类型**暂不迁移**到 media crate:本里程碑无第二个消费方,迁移推迟到 M5 接线时一并做(避免无谓 churn)。protocol 自带任务类型,与 media::MediaJob 的映射在适配器完成。

## IPC 协议契约

- **传输**:stdio + NDJSON(一行一信封)。选择理由:Windows 命名管道复杂度高;NDJSON 可被 serde_json 正确转义、人可读、易测。代价:单行长度上限(缩略图 job 无大载荷,不适用)。
- 信封:`{ "v": 1, "req": ... }` / `{ "v": 1, "res": ... }`;版本字段为前向兼容留位。
- 任务类型(v1):
  - `Echo { job_id, payload }` → `Ok { job_id, payload }`(协议测试/健康检查用)
  - `ThumbnailPng { job_id, source, dest, max_edge }` → `Ok` / `Err { reason }`(image 解码+等比缩放+PNG 落盘)
- EOF(stdin 关闭)= 退出信号,worker exit(0)。

## 池模型

- `WorkerPool::with_size(n)`:n = min(n, available_parallelism)(红线:核数封顶)。启动即拉满 n 个子进程。
- 每个 worker 一对线程:**writer**(宿主侧 stdin 写入,multi-producer 经 channel)+ **reader**(逐行读响应,按 job_id 路由到 pending 表的 oneshot sender)。
- 提交 API:`submit(JobRequest) -> Receiver<JobResult>`;pending 表 `(worker_id, job_id)`。
- 监督:reader 线程检测 EOF/解析失败 → 判定该 worker 死亡:
  1. 该 worker 全部 pending job 立即以 Failed 回报(不重试——坏进程的半成品状态不可信);
  2. 重启计数 ≤ 上限(默认 3 次/池)时拉起新 worker 替补;超限 → 池进入 degraded,后续 submit 直接返回 Failed。
- 崩溃重启预算:断言窗口取 10s(CI 抖动安全),实际目标亚秒。

## Windows idle 优先级

- spawn 后立即 `SetPriorityClass(hProcess, PROCESS_MODE_BACKGROUND_BEGIN)`(windows-sys,feature Win32_System_Threading)。
- 测试断言:`GetPriorityClass(child.handle()) == PROCESS_MODE_BACKGROUND_BEGIN`,经池暴露的测试钩子获取句柄快照。

## 数据流(缩略图)

```
library.enqueue ──dispatch(MediaJob)──▶ (M5 适配器) ──submit──▶ pool ──NDJSON──▶ worker 进程
   ▲                                                                    │ image 解码/resize
   └── state_of(ticket) ◀─ CopyState(拷贝队列,M3 不变)                  ▼
上层回写元数据(经 Store 门面) ◀── JobResult::Ok ─────────────────── thumbs/{u}/{uu}/{uuid}.png
```

## 权衡记录

| 决策 | 备选 | 取舍理由 |
|---|---|---|
| stdio NDJSON | 命名管道 | 实现简单、跨工具链稳;吞吐对缩略图场景绰绰有余 |
| 死亡 worker 的 pending 直接 Failed | 迁移重发到替补 | 语义诚实:解码是幂等的,但调用方重试比池内黑盒重试可控 |
| windows-sys | windows / winapi | 零额外运行时、feature 门最小 |
| 协议类型放 protocol.rs | 复用 media::MediaJob | 避免 worker↔library 循环依赖;media 迁移延后(M5) |

## 兼容与回滚

- 新 crate 独立,不动任何既有代码路径(library NullDispatcher 行为不变)→ 回滚 = 移除 workspace member。
- gnu 工具链风险点仅 windows-sys(纯 API 声明 crate,无 C 编译),低风险。

## 测试钩子(仿 library::set_paused 先例)

- `worker_pids() -> Vec<u32>`(崩溃测试用外部 kill);
- `worker_handles_snapshot()` 或经 pid OpenProcess 断言优先级;
- `degraded()` 状态只读查询。
