# Database Guidelines — app-ui

- UI 进程不直接触碰 meta.db；一切持久化经 library/Store 门面。
- 库位置：启动参数或默认目录（M5 定型时锁定单实例与「最近库」逻辑）。
