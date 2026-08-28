# Backend Guidelines — ui-viewmodels

> ViewModel 层：桥接 UI 与核心 crates，目标选择和反馈状态应保持纯 Rust 可单测。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/ui-viewmodels` |
| 依赖 | domain, index, store, library, targets, pipeline；平台 trait 可作为值/接口边界 |
| 角色 | TDD 第一原则的支点：业务逻辑住纯 Rust，`.slint` 哑渲染 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | VM 划分、目标条与平台装配边界 |
| [Database Guidelines](./database-guidelines.md) | 经门面访问、分页窗口 |
| [Error Handling](./error-handling.md) | Result → UI 状态转译 |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ 精确 HWND 选择、纯 VM 与装配边界 |
| [Logging Guidelines](./logging-guidelines.md) | 用户行为观测点 |

## 关键事实速记

- M5 落地:`layout.rs`(masonry 纯函数,criterion 173µs@10k)+ `grid_vm.rs`(LibraryGridVm:过滤/排序/O(1) rect 表/ensure_window LRU 物化/事件)。
- 内存守卫已测试锁定:100k 数据 + 容量注入式 LRU,窗外零缩略图驻留。
- app-ui 只能依赖 ui-viewmodels+slint → Filter/Sorter/FacetIndex 经本 crate 再导出。
- M8 新增 `TargetBarVm` / `TargetRoutingVm`：chip、精确窗口选择键、图钉和 notice 映射可纯 Rust 单测。
- 当前 `target_runtime.rs` 直接导入 `platform::win32` 并持有具体实现，违反“Win32 由二进制入口装配”的既有边界；这是待裁决的架构债，不能写成规范模式。
- 当前 `refresh_windows` 只凭 profile id 唯一候选自动重绑，尚无账号/实例稳定身份证明；同一 IM 多开时必须保守处理。
- M7 待办:aspect 来源从 id 导出换媒体元数据;Rect 表 @1M ≈32MB 论证值需实测复核。
- 参考:`crates/ui-viewmodels/src/{grid_vm.rs,layout.rs}`、TDD_PLAN M5、DECISIONS.md D3/D10。
