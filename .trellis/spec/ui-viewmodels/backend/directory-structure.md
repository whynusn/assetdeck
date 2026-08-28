# Directory Structure — ui-viewmodels

## 实际布局（节选）

```
crates/ui-viewmodels/
├── Cargo.toml    # 禁 slint
└── src/
    ├── lib.rs
    ├── grid_vm.rs
    ├── target_bar_vm.rs   # 纯目标条/路由状态与 PasteNotice 转译
    └── target_runtime.rs  # 只吃 trait 对象的路由编排；Win32 具体类型由装配层注入
```

## 模块组织规则

- ViewModel = 普通 Rust struct + trait 接口，与 Slint 通过属性/回调桥接；**本 crate 不得依赖 slint**（TDD 第一原则：业务逻辑全在纯 Rust，`.slint` 只做哑渲染）。
- 一个界面区域一个 VM struct（如 `LibraryGridVm`、`FilterPanelVm`），跨 VM 通信经共享的 app state 或事件总线（M5 design.md 定型）。
- `TargetBarVm` 拥有 chip/picker/pin/首次确认的呈现状态；选择项的稳定 UI key 必须包含 HWND。
- `TargetRoutingVm` 拥有 profile/matcher/tracker/pipeline 之间的纯业务编排，不得通过 exe/title 在 UI 再实现一套匹配规则。
- Win32 枚举器、观察器、剪贴板、激活器、focuser、readiness 和 injector 的具体类型只允许在两个装配点 `new`：`crates/app-ui/src/main.rs::win32_runtime_deps()` 与 `tools/real-im-verify/src/main.rs::win32_runtime_deps()`（DECISIONS D16）。`target_runtime.rs` 已收口为纯 trait 对象持有方，`tests/layering_guard.rs` 守卫这条边界。

## M5 首个红灯测试（内存守卫）

`viewmodel_window_of_100k_model_loads_only_visible_slice`——可见窗口外零缩略图驻留。虚拟化网格的数据窗口逻辑属于本 crate 而非 UI。

## M8 身份边界

- `selection_key = TargetId@HWND`，同一 IM 两个窗口不得共用只含 profile id 的选择键。
- 图钉是具体窗口绑定，不是“固定微信这个应用”。
- HWND 消失可保留休眠 chip；自动重绑必须有稳定实例身份。只有 profile id 时应保持休眠或要求显式选择。
