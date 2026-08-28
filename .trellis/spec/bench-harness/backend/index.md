# Backend Guidelines — bench-harness

> 内存/帧率测量夹具：合成库生成器 + RSS 采样器。M7 已完成；M8 新增多目标路由 Mock 闭环。

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `tools/bench-harness` |
| 角色 | D10 验收的执行者：没有监控的预算等于没定 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 生成器/RSS 采样器/CI 接入规划 |
| [Database Guidelines](./database-guidelines.md) | 经公共 API 写合成库 |
| [Error Handling](./error-handling.md) | 测量失败 = 红 |
| [Quality Guidelines](./quality-guidelines.md) | 确定性红线、诚实测量 |
| [Logging Guidelines](./logging-guidelines.md) | CI 可解析输出 |

## 关键事实速记

- M7 五个红灯测试：生成器 100k 行 / idle RSS / browse 100k RSS / 双击→输入框 <500ms / CI mem-regression job。
- M8 新增 `multi_target_routing_spec.rs` 证明：选择 Telegram 后全链路只激活/inject Telegram，绝不触碰微信，且核心序列无 Enter。
- 该测试使用纯 Mock 平台依赖，证明的是路由与无 Enter 编排，不证明真实 IM 窗口可写。
- 参考：TDD_PLAN 第五、八节，DECISIONS.md D10、A2/A3。
