# Backend Guidelines — app-ui

> Slint UI 薄壳 + `asset-manager` 二进制入口。M0 已完成最小可运行窗体。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/app-ui`（bin: asset-manager） |
| 依赖 | ui-viewmodels, slint(default-features=false + compat-1-2) |
| 角色 | 哑渲染层；依赖红线的被守卫方 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | ⭐ 含 Slint windows-gnu 工具链踩坑 |
| [Database Guidelines](./database-guidelines.md) | 不直接触库 |
| [Error Handling](./directory-structure.md) | 启动期 expect、运行期转 UI 状态 |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ deps_guard 守卫、D10 预算责任人 |
| [Logging Guidelines](./logging-guidelines.md) | subscriber 初始化归属 |

## 关键事实速记

- M0 实测：空窗 WorkingSet 77.8MB < 100MB 预算。
- GPLv3 风险：Slint 社区版，行动项 A1 未裁决，deny.toml copyleft=warn。
- 参考：`crates/app-ui/tests/deps_guard.rs`、TDD_PLAN M0/M5。
