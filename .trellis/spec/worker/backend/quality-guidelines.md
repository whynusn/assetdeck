# Quality Guidelines — worker

## 红线（D11）

1. **解码依赖只进本 crate**：image 解码/视频抽帧/pHash 调用的实现代码只能出现在 worker；app-ui 的 deps_guard 测试 + deny.toml 是编译期守卫。
2. **崩溃隔离**：单个坏资产让该 job 失败，不得拖垮池（`poison_asset_fails_job_not_pool`）；worker 进程崩溃监督者在预算内重启。
3. **资源上限**：池大小按核数封顶；IO idle 优先级必须实际生效且经 `GetPriorityClass` 实测断言，非仅设置成功。机制（M4 裁决）：宿主设 `IDLE_PRIORITY_CLASS`（跨进程可设可测），worker 入口自设 `THREAD_MODE_BACKGROUND_BEGIN` 压低 IO/内存优先级；不用进程级 `PROCESS_MODE_BACKGROUND_BEGIN`——仅限当前进程自设、GetPriorityClass 读不回、且有 32MiB 工作集封顶副作用。

## 测试要求

- IPC 协议 roundtrip 必须先于一切实现（TDD：协议即契约）。
- 崩溃重启测试用真实子进程 kill 模拟，禁止 mock 掉被测对象本身。
- 时间预算类断言给出宽裕上界（CI 抖动），标注 best-effort。

## Code Review 清单

- [ ] 新任务类型是否进了 protocol.rs 并 derive 全集？
- [ ] 是否有线程化解码偷跑进 UI 侧？
- [ ] worker 崩溃时未完成 job 的状态是否明确（重试/失败回报）？
