# Backend Guidelines — app-ui

> Slint UI 薄壳 + `asset-manager` 二进制入口。M0 已完成最小可运行窗体；M8 目标条已接线。

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/app-ui`（bin: asset-manager） |
| 依赖 | ui-viewmodels, slint(default-features=false + compat-1-2) |
| 角色 | 哑渲染层；消费 VM 快照，装配目标路由运行时 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 布局、Slint 工具链、目标条装配边界 |
| [Database Guidelines](./database-guidelines.md) | 不直接触库 |
| [Error Handling](./directory-structure.md) | 启动期 expect、运行期转 UI 状态 |
| [Quality Guidelines](./quality-guidelines.md) | deps_guard 守卫、D10 预算责任人 |
| [Logging Guidelines](./logging-guidelines.md) | subscriber 初始化归属 |

## 关键事实速记

- M0 实测：空窗 WorkingSet 77.8MB < 100MB 预算。
- GPLv3 风险：Slint 社区版，行动项 A1 未裁决，deny.toml copyleft=warn。
- M8 目标条：chip、四色点、图钉、冷目标选择列表、notice 均已接线。
- 双击上框走真实素材载荷（`RealAssetResolver` 物化 → 绝对路径 / 内联 PNG 字节），终点是 IM 输入框，不发送；热键唤起流程尚未实现。
- 参考：`crates/app-ui/tests/deps_guard.rs`、TDD_PLAN M0/M5/M8。
