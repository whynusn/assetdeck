# Error Handling — bench-harness

- 采样失败（子进程退出、API 失败）→ harness 非零退出码，CI 红——**测量失败按超预算处理**，禁止静默跳过让预算检查形同虚设。
- app `--bench` 模式的约定：跑指定浏览脚本后可被 harness 终止；harness 不解析 UI 内部状态。

## Win32 采样踩坑（M7 实录）

- **僵尸进程残留值**：harness 持有子进程句柄期间，已退出进程的内核对象仍可被
  `OpenProcess` 成功打开，且 `GetProcessMemoryInfo` 返回**恒定残留值**（实测
  32768 字节）而非报错——只看「API 是否成功」会把死进程当活进程采样。必须叠加
  `GetExitCodeProcess` 判 `!= STILL_ACTIVE(259)` 才算存活。
- **idle 提前退出 = 测量失败**：idle 模式的 app 应被 harness 终止而非自行退出；
  窗口内任何自行退出（哪怕退出码 0）都意味着部分窗口样本不代表稳态空闲，
  一律红。browse 模式例外：跑完脚本自然退出是合法收窗终点，但退出码必须为 0。
- **报红前先收尸**：测量失败的各 return 路径必须先 `kill()+wait()` 再退出，
  否则泄漏的存活子进程会污染同 runner 后续步骤的内存环境。
