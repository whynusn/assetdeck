# Quality Guidelines — app-ui

## 红线

1. **依赖白名单**：只许依赖 ui-viewmodels + slint。deps_guard 测试（`ui_cargo_toml_has_no_decode_layer_deps`）禁止 media/phash/worker 出现在 Cargo.toml。
2. **`.slint` 不写业务**：过滤/排序/状态迁移全在 VM；.slint 只绑定属性与转发回调。
3. **空闲 RSS ≤100MB（D10）**：本 crate 是预算的直接责任人——M0 实测空窗 WorkingSet 77.8MB，新增组件/渲染特性时必须复核。

## 测试要求

- `.slint` 只做冒烟级验证（可实例化、回调接通），不做单测。
- 手工验收清单（诚实标注，不自动化）：120fps 滚动体感、IME 中文输入、DPI 缩放。

## Code Review 清单

- [ ] 新依赖是否过 cargo-deny licenses（GPLv3 警示：Slint 社区版，A1 未裁决）。
- [ ] main.rs 是否仍无业务逻辑。
- [ ] 双击上框是否走 `RealAssetResolver` 物化载荷而非演示文本（仅无 `--library-root` 的演示回退允许演示文本）。
- [ ] 是否把 Win32 具体实现留在 VM crate（应迁回本二进制装配）。

## D53 弹层/瓦片动效纪律（2026-08-28）

- **入场**：Slint 的 `init` 跑在首帧渲染之前——「init 里置 shown=true」永远不播过渡。
  正确形态 = `init => root.overlay-mounted(which)` 报数，壳层 16ms 单发 Timer 翻转
  对应 `*-shown` in-property（回调必然落在首帧后）。归类弹窗/范围下拉用同模式。
- **出场**：新弹层两段式（shown=false 播淡出 → 170ms Timer 收 open 卸载）；旧三处
  （目标下拉/导入菜单/设置面板）出场维持即时卸载——目标下拉的关闭由轮询驱动的
  `sync_target_bar` 拍板，两段式会与轮询状态机竞态（差异经走查回写 design）。
- **卸载点必须重置 shown**：否则重挂载时初值已是 true，入场动画被跳过。
- **瓦片淡入**：`TileData.thumb-fade`（缓存命中/新装出=true；缺图/负缓存=false）。
  false→true 翻转播 150ms opacity；挂载即命中时初值为 1，滚动/切页不重播。
- 全部时长绑定 `root.animations-enabled ? …ms : 0ms`；关闭动效时壳层直接置终态。
