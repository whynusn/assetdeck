# Error Handling — bench-harness

- 采样失败（子进程退出、API 失败）→ harness 非零退出码，CI 红——**测量失败按超预算处理**，禁止静默跳过让预算检查形同虚设。
- app `--bench` 模式的约定：跑指定浏览脚本后可被 harness 终止；harness 不解析 UI 内部状态。
