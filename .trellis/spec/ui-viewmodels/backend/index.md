# Backend Guidelines — ui-viewmodels

> ViewModel 层：桥接 UI 与核心 crates，纯 Rust 可全量单测。M5 已完成(2026-08-22)。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/ui-viewmodels` |
| 依赖 | domain, index, store, library, pipeline |
| 角色 | TDD 第一原则的支点：业务逻辑住纯 Rust，`.slint` 哑渲染 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | VM 划分、虚拟化数据窗口职责 |
| [Database Guidelines](./database-guidelines.md) | 经门面访问、分页窗口 |
| [Error Handling](./error-handling.md) | Result → UI 状态转译 |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ 禁 slint 依赖、内存守卫红线 |
| [Logging Guidelines](./logging-guidelines.md) | 用户行为观测点 |

## 关键事实速记

- M5 落地:`layout.rs`(masonry 纯函数,criterion 173µs@10k)+ `grid_vm.rs`(LibraryGridVm:过滤/排序/O(1) rect 表/ensure_window LRU 物化/事件)。
- 内存守卫已测试锁定:100k 数据 + 容量注入式 LRU,窗外零缩略图驻留。
- app-ui 只能依赖 ui-viewmodels+slint → Filter/Sorter/FacetIndex 经本 crate 再导出。
- M7 待办:aspect 来源从 id 导出换媒体元数据;Rect 表 @1M ≈32MB 论证值需实测复核。
- 参考:`crates/ui-viewmodels/src/{grid_vm.rs,layout.rs}`、TDD_PLAN M5、DECISIONS.md D3/D10。
