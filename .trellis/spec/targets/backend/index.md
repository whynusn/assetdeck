# Backend Guidelines — targets

> 多 IM 目标路由的纯逻辑层：画像合并、窗口匹配、热目标粘性状态机与健康等级判定。

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/targets` |
| 依赖 | `platform` 的平台无关值类型 + serde/regex/toml；禁止 Win32 实现、文件 IO 与时钟 |
| 角色 | 把稳定目标身份、运行时 HWND、自动追踪资格和健康状态定义成可单测契约 |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 模块所有权、依赖边界、稳定身份与 HWND 分层 |
| [Quality Guidelines](./quality-guidelines.md) | 精确路由七段式 code-spec、守卫与必测场景 |
| [Error Handling](./error-handling.md) | 画像错误、歧义、fallback 与就绪度三态 |
| [Database Guidelines](./database-guidelines.md) | 零 IO 边界、builtin/user 配置归属 |
| [Logging Guidelines](./logging-guidelines.md) | 纯逻辑层不记录窗口标题或用户内容 |

## Pre-Development Checklist

- [ ] 先读 `quality-guidelines.md` 的“精确多 IM 目标路由”七段式契约。
- [ ] 修改身份或重绑前，区分 `TargetId`、窗口实例身份与当前 `HWND`。
- [ ] 修改匹配器前，确认 generic fallback 不会进入自动热目标路径，profile 并列不会猜测。
- [ ] 修改 tracker 前，确认无时间调用，图钉锁定的是具体窗口而不只是应用画像。
- [ ] 修改 profile 加载前，确认函数仍只接收 `&str`，同一文档重复 id 返回错误。

## Quality Check

- [ ] `rg 'windows-sys|windows::' crates/targets` 零命中。
- [ ] `rg 'std::fs|std::io' crates/targets/src` 零命中。
- [ ] `rg 'Instant|SystemTime|Duration' crates/targets/src/tracker.rs` 零命中。
- [ ] `cargo test -p targets` 全绿，包含属性测试与精确 HWND 回归。
- [ ] 任何自动重绑都有稳定实例身份依据；只有 profile id 时不得把同应用的另一个窗口当成原窗口。

## 当前状态（2026-08-23）

纯逻辑与 Mock 测试已落地；真实用户 profile 持久化、实例级稳定身份、L0-L3 运行时编排及真实 IM 体检尚未交付。
