# Quality Guidelines — app-ui

## 红线

1. **依赖白名单**：只许依赖 ui-viewmodels + slint。deps_guard 测试（`ui_cargo_toml_has_no_decode_layer_deps`）禁止 media/phash/worker 出现在 Cargo.toml——「UI 进程不解码」的编译期守卫之一。
2. **`.slint` 不写业务**：过滤/排序/状态迁移全在 VM；.slint 只绑定属性与转发回调。
3. **空闲 RSS ≤100MB（D10）**：本 crate 是预算的直接责任人——M0 实测空窗 WorkingSet 77.8MB，新增组件/渲染特性时必须复核。

## 测试要求

- `.slint` 只做冒烟级验证（可实例化、回调接通），不做单测。
- 手工验收清单（诚实标注，不自动化）：120fps 滚动体感、IME 中文输入、DPI 缩放。

## Code Review 清单

- [ ] 新依赖是否过 cargo-deny licenses（GPLv3 警示：Slint 社区版，A1 未裁决）？
- [ ] main.rs 是否仍无业务逻辑？
