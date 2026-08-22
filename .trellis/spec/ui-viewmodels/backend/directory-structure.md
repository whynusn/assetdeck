# Directory Structure — ui-viewmodels

## 布局（M5 实施目标，当前为占位 stub）

```
crates/ui-viewmodels/
├── Cargo.toml    # deps: domain, index, store, library, pipeline —— 禁 slint
└── src/lib.rs
```

## 模块组织规则

- ViewModel = 普通 Rust struct + trait 接口，与 Slint 通过属性/回调桥接；**本 crate 不得依赖 slint**（TDD 第一原则：业务逻辑全在纯 Rust，`.slint` 只做哑渲染）。
- 一个界面区域一个 VM struct（如 `LibraryGridVm`、`FilterPanelVm`），跨 VM 通信经共享的 app state 或事件总线（M5 design.md 定型）。

## M5 首个红灯测试（内存守卫）

`viewmodel_window_of_100k_model_loads_only_visible_slice`——可见窗口外零缩略图驻留。虚拟化网格的数据窗口逻辑属于本 crate 而非 UI。
