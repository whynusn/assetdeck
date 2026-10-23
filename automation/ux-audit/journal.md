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

## run-20261024-030013 · M0-C3 · done

- 组件：间距/尺寸/圆角/图标 token
- 结论：建立 Spacing/Metrics 两套令牌（4px 网格、栏/面板/弹窗/菜单内边距、控件/行高/开关/热区尺寸），结构性间距与高度全部改为消费令牌；清零圆角字面量与阶梯外 spacing，通知关闭/共享行按钮/归类候选热区提到 ≥28px。共享中心与设置面板的纵向溢出仍归 M9-C1/M7-C1。
- Findings：F-016(fixed) 间距与控件尺寸没有共享令牌，14/9/7/5/3px 阶梯外值散落；F-017(fixed) 圆角仍有 12 处字面量与 5 处阶梯外值，圆形/胶囊靠魔法数；F-018(fixed) 三处可点击热区低于 28px 下限
- 门禁：slint-check=pass;cargo-check=pass;ui-hygiene=pass(hex 0/font 0/off 0/radius 0/off 0/spacing 0);deps-guard=pass;capture-check=pass(42 states);visual=pass(8 组 before/after)
- 截图：target/ux-audit/shots/M0-C3/after/shell-grid.png;target/ux-audit/shots/M0-C3/after/settings-default.png;target/ux-audit/shots/_states/after/notice-success.png;target/ux-audit/shots/_states/after/selection-bar.png;target/ux-audit/shots/_states/after/share-ctl.png;target/ux-audit/shots/_states/after/classify.png;target/ux-audit/shots/_states/after/properties.png;target/ux-audit/shots/_states/after/rename.png
- 基线：b79d9e5

## run-20261024-040014 · M0-C4 · done

- 组件：共享原语状态矩阵
- 结论：补齐共享原语状态矩阵：开关关闭态新增 3:1 轨道描边并与禁用态分层；MenuItem 增加 danger 变体并为两个右键菜单的删除项加分隔线；DangerButton 按压态反白、暗色危险底调深；菜单副行升 label；IconButton 选中按压不再回落灰色。焦点模型留 M0-C5、设置行禁用层级留 M7-C2（P2 留档）。
- Findings：F-019(fixed) 开关关闭态轨道与浮层同色，关闭与禁用不可分；F-020(fixed) 右键菜单「删除」与普通项同色同组，危险动作无隔离；F-021(fixed) DangerButton 按压态红字压红底，浅色主题对比仅 0.82:1；F-022(fixed) 菜单副行用 caption 10px 承载安全承诺；F-023(fixed) 图标按钮选中后再按压，底色回落到中性灰；F-024(open) 自定义原语没有键盘焦点态（全局焦点模型留待 M0-C5）；F-025(open) 设置行禁用态只变淡标题，说明文字不变
- 门禁：slint-check=pass;cargo-check=pass;deps-guard=pass;ui-hygiene=pass;viewmodels-test=pass;capture-check=pass(42 states);visual=pass(4 组 before/after + theme-light/notice-error 复核)
- 截图：target/ux-audit/shots/M0-C4/after/settings-default.png;target/ux-audit/shots/M0-C4/after/shell-grid.png;target/ux-audit/shots/M0-C4/after/selection-bar.png;target/ux-audit/shots/M0-C4/after/context-menu.png;target/ux-audit/shots/_states/after/theme-light.png;target/ux-audit/shots/_states/after/notice-error.png;target/ux-audit/shots/_states/after/import-result.png
- 基线：80a548a

