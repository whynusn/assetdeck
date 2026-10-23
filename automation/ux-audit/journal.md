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

## run-20261024-010014 · M0-C1 · done

- 组件：颜色 token 与明暗等价
- 结论：颜色令牌补齐 ok/shared/soft/badge-ring/tile-veil 并打通明暗注入；text-3 达标 4.5:1；目标健康色与共享/成功语义全部收口到 Theme 令牌。
- Findings：F-001(fixed) 成功/共享/健康语义色仍是硬编码十六进制；F-005(fixed) 语义色板缺 ok/shared/soft/badge-ring，badge/card-icon 未进运行时令牌；F-006(fixed) text-3 在抬起面上对比度不达标（暗 3.88:1 / 浅 3.19:1）；F-007(fixed) 目标健康色在 Rust 复制第二套语义色板，不随主题；F-008(fixed) 文本/文件瓦片正文压在固定占位色上，浅色主题深字压深底；F-009(fixed) 选中勾选图标用纯白压在 accent 上（暗色仅 1.80:1）；F-010(fixed) 成功/共享语义错配：成功图标用 accent，共享域文字用绿色
- 门禁：slint-check=pass;cargo-check=pass;deps-guard=pass;ui-hygiene=pass(hex 0);viewmodels-test=pass;capture-check=pass;visual=pass
- 截图：target/ux-audit/shots/M0-C1/after/settings-default.png;target/ux-audit/shots/M0-C1/after/shell-grid.png;target/ux-audit/shots/M0-C1/after/theme-light.png
- 基线：f222909

## run-20261024-020025 · M0-C2 · done

- 组件：字体阶梯与字号 token
- 结论：建立 Typography 五档令牌与 TitleLabel/SectionLabel 原语，110 处字号字面量清零；9px/13px 归并、主读数升 display、弹窗与分组标题层级理顺；截断文本 Tooltip 缺口留 M2-C1/M3-C2。
- Findings：F-003(fixed) 字号字面量 110 处，未形成字体阶梯；F-011(fixed) 上框点位表单使用 9px 字号，低于 caption 下限；F-012(fixed) 13 处弹窗标题停留在 13px 过渡档；F-013(fixed) SectionLabel 用 caption 10px 承载面板与分组结构标题；F-014(fixed) 错误与关键状态文案被压到 caption 10px；F-015(fixed) 主读数沿用 14px，未使用 display 档
- 门禁：slint-check=pass;cargo-check=pass;deps-guard=pass;ui-hygiene=pass(font 0/off 0/hex 0);capture-check=pass;visual=pass
- 截图：target/ux-audit/shots/M0-C2/after/shell-grid.png;target/ux-audit/shots/M0-C2/after/settings-default.png;target/ux-audit/shots/_states/after/tuning.png;target/ux-audit/shots/_states/after/update.png
- 基线：113f18a

