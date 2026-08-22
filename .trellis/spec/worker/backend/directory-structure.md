# Directory Structure — worker

## 布局(M4 已落地)

```
crates/worker/
├── Cargo.toml    # deps: serde/serde_json/image(png+jpeg)/windows-sys; [[bin]] decode-worker
├── src/
│   ├── lib.rs        # WorkerPool:spawn/supervise/submit 路由/测试钩子
│   ├── protocol.rs   # Envelope/JobRequest/JobResult(serde 全 derive,协议唯一出处)
│   └── bin/
│       └── decode-worker.rs  # worker 进程入口:stdin NDJSON → 处理 → stdout NDJSON
└── tests/pool_spec.rs
```

## 进程模型(D11 红线,已实现)

- worker 是**独立进程**(`decode-worker` 二进制):UI 主进程零解码。
- 池大小 = min(请求值, available_parallelism);崩溃由监督者自动替补(上限 3 次/池),超限 degraded。
- 优先级机制见 quality-guidelines.md 红线 3(M4 裁决版)。

## IPC 协议契约(M4 定型,可执行)

### Scenario: stdio NDJSON 协议

1. **Scope / Trigger**:宿主与 worker 进程间的全部通信;新增任务类型时必读本节。
2. **Signatures**:
   - `Envelope`(untagged serde):线上形态严格 `{"v":1,"req":…}` / `{"v":1,"res":…}`。
   - `JobRequest::{Echo{job_id,payload}, ThumbnailPng{job_id,source,dest,max_edge}}`
   - `JobResult::{Ok{job_id,…}, Failed{job_id,reason}}`(**命名是 Failed 不是 Err**,避免与 Result::Err 混淆)
   - `WorkerPool::with_size(n)` / `with_exe(path)` / `submit(JobRequest) -> mpsc::Receiver<JobResult>`
3. **Contracts**:
   - 传输 = stdio + NDJSON(一行一信封);EOF(stdin 关闭)= 退出信号,worker exit(0)。
   - stdout 只走协议;stderr 直通 null——日志通道分离(spec/logging-guidelines)。
   - job_id 全局唯一性由调用方保证(池内 AtomicU64);M5 适配器需落实 MediaJob ticket → job_id 映射。
   - worker 收 `dest` 参数落盘缩略图;不直接打开 meta.db。M5 接线时由适配器用 `store::thumbnail_cache_path` 生成 dest。
4. **Validation & Error Matrix**:
   - 解码失败(坏文件/不存在路径)→ 该 job 回 `Failed{reason}`,进程存活;
   - worker 死亡 → 其 pending 全部立即 `Failed`(不重试,半成品不可信)+ 替补 spawn;
   - 重启 >3 次/池 → degraded,后续 submit 直接 Failed(有专测 `restart_budget_exhaustion_degrades_pool`)。
5. **Good/Base/Bad Cases**:
   - Good:合法 PNG → Ok 且 dest 文件存在;
   - Base:Echo 原样回(payload roundtrip);
   - Bad:不存在 source → Failed 且池可用、未 degraded。
6. **Tests Required**(pool_spec.rs,断言点):
   - `job_result_roundtrips_over_ipc_protocol`(纯 serde,三形态);
   - `worker_crash_supervisor_respawns_within_budget`(OpenProcess+TerminateProcess 真实 kill,10s best-effort 上界);
   - `pool_size_capped_at_cpu_count`;`idle_priority_set_on_worker_process`(GetPriorityClass 实测);
   - `poison_asset_fails_job_not_pool`;`restart_budget_exhaustion_degrades_pool`。
7. **Wrong vs Correct**:
   - Wrong:父进程 `SetPriorityClass(PROCESS_MODE_BACKGROUND_BEGIN, child)` —— MSDN 仅限当前进程句柄;GetPriorityClass 读不回;32MiB 工作集封顶副作用威胁 D10。
   - Correct:宿主设 `IDLE_PRIORITY_CLASS` + worker 入口自设 `THREAD_MODE_BACKGROUND_BEGIN`。

## 测试钩子先例

- `worker_pids()`(外部 kill 用)、`degraded()`、优先级查询——仿 library::set_paused 的确定性测试模式。

## 命名约定

- 测试名对齐 TDD_PLAN M4 清单(见上);新任务类型先进 protocol.rs 并 derive 全集。
