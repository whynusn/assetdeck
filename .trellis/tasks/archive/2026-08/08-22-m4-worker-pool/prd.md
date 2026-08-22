# PRD — M4 Worker 进程池

> 依据：TDD_PLAN M4 清单 + DECISIONS.md D11。里程碑性质:五个红灯测试全部转绿即为验收。

## 背景

D11 红线:UI 主进程永不执行解码重活(缩略图生成/视频抽帧/pHash 计算),全部隔离在独立 worker **进程**池。
M3 已在 library 层定义 `MediaJob`/`MediaDispatcher` trait 并用 NullDispatcher 占位;M4 提供真实实现。

## 需求(与 TDD_PLAN M4 一一对应)

1. `job_result_roundtrips_over_ipc_protocol` — IPC 协议类型 serde roundtrip(协议即契约,先于一切实现)。
2. `worker_crash_supervisor_respawns_within_budget` — worker 进程被外部 kill 后,监督者在预算内重启,池容量恢复。
3. `pool_size_capped_at_cpu_count` — 池大小请求值超过 CPU 核数时被钳制到核数。
4. `idle_priority_set_on_worker_process` — 每个 worker 进程的优先级类被设为 `PROCESS_MODE_BACKGROUND_BEGIN`(实际断言,非仅设置成功)。
5. `poison_asset_fails_job_not_pool` — 坏资产(不存在/损坏文件)使该 job 返回 Failed,池与其余任务不受影响。

## 约束

- 解码实现依赖(image 等)只允许出现在 worker crate(app-ui deps_guard + deny.toml 守卫)。
- worker 不直接打开 meta.db;产物路径用 `store::thumbnail_cache_path`;结果经池回报,由上层回写元数据。
- 协议通道与日志通道分离(worker spec/logging-guidelines)。
- 仅 Windows(v1);windows-gnu 工具链必须可编译。

## 范围外(明确不做)

- 视频抽帧实装:CI 无 ffmpeg,引入解码栈需先立决策(候选:ffmpeg-cli sidecar / 纯 Rust crate)。本里程碑只交付图片缩略图 job 类型;视频抽帧另立任务。
- pHash 计算迁移到 worker:M3 已同步实现且测试全绿,迁移无行为收益,推迟到有性能证据再做。
- UI 层接入(ui-viewmodels 消费池):属 M5。

## 验收标准

- 五个红灯测试全绿;全 workspace `cargo fmt --check && cargo clippy -- -D warnings && cargo test` 保持绿。
- library 的既有 MediaDispatcher 契约不破坏(RecordingDispatcher 测试不动)。
