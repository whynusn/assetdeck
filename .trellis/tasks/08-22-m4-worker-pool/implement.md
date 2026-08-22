# Implement — M4 Worker 进程池

## 顺序清单(Red→Green→Refactor)

1. **协议先行**
   - [ ] 红灯:`job_result_roundtrips_over_ipc_protocol`(tests/pool_spec.rs):Envelope/JobRequest/JobResult serde_json roundtrip + 版本字段存在。
   - [ ] 绿灯:src/protocol.rs 类型 + derive。
2. **worker 进程入口**
   - [ ] `[[bin]] decode-worker`:stdin NDJSON 循环;Echo 原样回;ThumbnailPng 解码→等比缩放(max_edge)→PNG 写 dest→Ok;解码失败→Err{reason};EOF exit(0)。
   - [ ] 依赖:serde/serde_json/image/png;windows-sys 仅 lib 侧。
3. **池骨架**
   - [ ] `WorkerPool::with_size(n)` 钳制核数;spawn 子进程 + SetPriorityClass(PROCESS_MODE_BACKGROUND_BEGIN)。
   - [ ] 红灯→绿灯:`pool_size_capped_at_cpu_count`、`idle_priority_set_on_worker_process`。
4. **提交与路由**
   - [ ] submit → writer channel → stdin;reader 按 job_id 路由 oneshot。
5. **监督重启**
   - [ ] reader EOF → pending 全 Failed + 替补 spawn(上限 3,超限 degraded)。
   - [ ] 红灯→绿灯:`worker_crash_supervisor_respawns_within_budget`(钩子拿 pid,外部 kill,10s 窗口断言容量恢复且新进程优先级正确)。
6. **毒资产隔离**
   - [ ] 红灯→绿灯:`poison_asset_fails_job_not_pool`:不存在路径 job → Failed;随后 Echo/合法 PNG thumbnail 成功;池未 degraded。
7. **收尾验证**

## 验证命令(CI 同序)

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check 2>$null        # 本地无 cargo-deny 时跳过,CI 兜底
cargo test --workspace
```

## 审查门

- 门 1(步骤 1 后):协议类型经 implement.jsonl 所列 spec 复核(可序列化、版本字段、通道分离)。
- 门 2(步骤 5 后):崩溃测试真实 kill 子进程(非 mock);degraded 路径有断言。
- 门 3(全部后):全量命令绿;library/index/store 既有测试零改动通过。

## 回滚点

- 步骤 1–2 独立可合(protocol + bin 无池依赖);
- 池实现若 gnu 工具链受阻:windows-sys 回退为仅文档化优先级设置 + 测试降级标注 best-effort(须在任务 notes 记录理由)。

## 明确不做(防 scope creep)

- 视频抽帧、pHash 迁移、media::MediaJob 迁移、M5 UI 接线 —— 见 prd.md 范围外。
