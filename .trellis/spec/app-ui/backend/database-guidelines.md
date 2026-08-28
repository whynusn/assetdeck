# Database Guidelines — app-ui

- **UI 进程不触碰 meta.db，也不依赖 library crate**（deps_guard 白名单锁死：
  app-ui 允许 ui-viewmodels/slint/platform/logging/lru）。
- 一切持久化 = 两条通道：
  1. **导入/导出/库写命令** → `sample-library.exe` 子进程（单写者纪律）。
     CRUD 动作走 `--cmd trash|restore|purge|empty-trash|rename|move-category
     --library <root> [--uuid u]… [--value v]`；协议复用导入管线的
     `PROGRESS\t<done>\t<total>` 行 + `done:` 汇总 + 非零退出码（D33）。
  2. **读取** → ui-viewmodels 的 `RealAssetResolver`（内部经 store/index 只读装载）。
- 壳层派发的标准形态：UI 先行即时反馈（本地 mark_deleted 之类）→ 起子命令 →
  `*-finished` 回调弹回 UI 线程 → 整库重载对齐（失败不静默：错误上通知条，
  分叉的行显形回来）。子进程闭包只许 Fn+Send，Rc 上下文一律经 ui 句柄转接。
- 库位置：启动参数或默认目录（M5 定型时锁定单实例与「最近库」逻辑）。
