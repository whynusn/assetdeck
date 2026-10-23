# UX 审计运行日志

> 由 `automation/ux-audit/scripts/state.mjs` 追加；每次运行记录组件、结论、门禁与截图。
> 完整证据截图在 `target/ux-audit/shots/`（本地，不进库）。

## init · 2026-10-23 · 基础设施

- 建立独立 `ux-audit` 工作树、设计契约 v1、状态机、状态截图夹具、UI 卫生 ratchet。
- 基线计数：appwindow.slint hex 6 / font-size 110 / border-radius 12。
- 预记录 findings：F-001（语义色硬编码）、F-002（选中白描边）、F-003（字号未 token 化）。

## init · 2026-10-23 · 截图与护栏补齐

- 截图夹具 42 个状态（`states/fixtures.json`），覆盖全部 56 个待审组件的 `states`；`capture.mjs --check` 双向校验「组件引用→夹具」「夹具→归属」，映射单一事实来源 = `state.json`。
- 无头截图能力已验证：暗/亮两套主题（`Theme.*` + `Palette.color-scheme` 全局覆盖）、960×600 最小窗（按真实 `appwindow.slint` 生成仅改尺寸的临时副本，不存第二份 UI 源码）、全部弹层/空态/错误态。
- 卫生 ratchet 指标扩为 5 项：hex 6、font-size 字面量 110（非字阶 16）、border-radius 字面量 12（非圆角阶 5）；基线在 `hygiene-baseline.json`，允许阶写进 `ui_hygiene.rs` 防止悄悄放宽。
- 截图实测确认 F-001/F-002 现象；另记录候选问题（分享中心弹窗在 700px 高度下内容裁切、菜单删除项无危险色、设置面板滚动无可见提示）留待对应组件运行处理。
- 截图实测新增 finding F-004（M7-C6，P1/layout）：设置面板「关于」区正文被裁切且无滚动余量；1200px 高窗口复现，证据 `target/ux-audit/shots/_states/before/settings-default-tall.png`。
## run-20261024-003547 · M0-C8 · done

- 组件：UI 卫生 ratchet 护栏
- 结论：初始化自检：begin/complete/journal/lock 与卫生度测试全链路通过
- 门禁：ui-hygiene=pass(hex 6/font 110/off16/radius 12/off5);capture-check=pass(42 states);state-validate=pass
- 基线：780dd9a

