# PRD — M8 多 IM 目标路由：一键上框

> 依据：`DECISIONS.md` D13（设计定盘）· `TDD_PLAN.md` M8（红灯测试清单）· `AGENTS.md` 红线。

## Goal

用户在后台同时挂着多个 IM（微信 / QQ / Telegram / 千牛 / 拼多多商家版等）时，一次操作就能把选中素材
送进**他真正想发的那一个** IM 的输入框。产品成功标准只有一句：素材进入正确 IM 的输入框。
按回车自动发送是可选增强，永远不进核心链路。

用户价值：当前 M6 管线只认「唤起面板那一刻的前台窗口」，多 IM 场景下极易粘错窗口，
用户必须先手动点亮目标 IM 才敢用，一键的承诺实际不成立。

## Confirmed Facts（已查证，非推演）

- `crates/pipeline/src/lib.rs`：`PasteSession{config, previous_foreground: Option<WindowHandle>}`，
  `paste()` 四步编排为「格式协商 → 写剪贴板 → `is_alive` 焦点校验 → 注入」，`PasteOutcome{Injected, CopiedOnly{reason}, Failed}`。
  `previous_foreground` 由 `begin_panel()` 瞬时采样 `focus.foreground()` 得到。
- `crates/pipeline/src/negotiate.rs`：`negotiate(req, TargetProfile::ImGeneric)` 是二维 match，目标画像是硬编码枚举而非数据。
- `crates/platform/src/lib.rs`：trait 层零依赖零 `cfg` 门（明文纪律），现有 `ClipboardSink` / `FocusWatcher` / `KeyInjector`；
  `windows-sys 0.59` 仅在 `cfg(windows)` target 门下。
- 现有 crates 无 `targets`；`.trellis/config.yaml` 的 `packages` 需登记新 crate。
- 内存红线：空闲 ≤100MB（M7 实测 62.8MB）/ 浏览 10 万条 ≤250MB（实测 29.9MB），由 CI `mem-regression` job 守。
- 仓库是 git 仓库，但当前沙箱用户与目录属主不同，`git status` 报 dubious ownership，需用户侧配置 `safe.directory` 后才能跑 git。

## Requirements

R1 目标身份不绑 HWND。微信 / QQ / 千牛「关到托盘」会销毁主窗口句柄，再开是新句柄。
身份用 `TargetId`（稳定）+ `TargetBinding`（可变 hwnd）双层表达，托盘往返后自动重绑。

R2 热目标粘性锁定。改写热目标的唯一路径是「一个 eligible 目标窗口成为前台」。
用户在此期间做任何其他操作（切浏览器、开资源管理器、唤起本应用面板）一律不改写热目标，
且无 TTL 无时间衰减。这是用户明确提出的容错要求。

R3 冷目标可选可扩展。内置目标册随版本发布，用户可自定义新目标，升级不冲掉用户配置。

R4 就绪度探测。窗口存活不等于有可写输入框（未登录 / 未选会话 / 只读群 / 模态遮挡 / 启动中）。
否证成立时**绝不注入**，降级为「已复制」并给出可执行提示。
探测不可用时归入 `Unknown` 中间档：允许注入但标记 `verified: false`，不得当成 `NotReady` 误杀。

R5 健康性/连通性校验分级体检 L0–L3，自定义目标必须过体检才能启用。四色回显，绿色只能来自 L3 自证通过。

R6 统一友好反馈。所有失败与降级路径收敛到一个 `PasteFeedback` 结构，四条纪律：
先说「已复制，可手动粘贴」→ 说人话不报错误码 → 给一个可执行动作 → 回显目标名让用户确认没搞错对象。

R7 零点击优先的目标选择。热键在 IM 内唤起时追踪器已给出目标，不需要用户选；
只在真正歧义（同一 IM 多开）或无匹配时才要求用户介入。

## Acceptance Criteria

功能（可观察结果）：
- [ ] AC1 人在微信里按热键 → 面板 → 选素材 → 素材出现在微信输入框，全程零点击选目标。
- [ ] AC2 期间切到浏览器、切回、开资源管理器，热目标仍锁定微信（R2）。
- [ ] AC3 微信关到托盘再打开，热目标自动重绑新 HWND，无需用户重新点亮（R1）。
- [ ] AC4 未选会话时上框：不注入，提示「已复制，请先在微信选择一个会话」（R4/R6）。
- [ ] AC5 自定义一个未内置的 IM，走完 L0–L3 体检后可正常上框（R3/R5）。
- [ ] AC6 同一 IM 开两个窗口时不静默选择，UI 展开选择列表（R7）。

红线（任一破则不可交付）：
- [ ] AC7 核心上框路径的注入序列不含 `0x0D`。
- [ ] AC8 `NotReady` 状态下从不注入。
- [ ] AC9 `Unknown` 状态注入时必标 `verified: false`。
- [ ] AC10 `targets` crate 零 IO 零平台依赖；`tracker.rs` 不出现任何时间调用。
- [ ] AC11 `mem-regression` job 仍绿（WinEvent 常驻钩子不得吃穿空闲 100MB 预算）。
- [ ] AC12 M6 原有 7 条 pipeline 测试零修改全绿。

## Out of Scope（明确不做）

- 自动发送（按回车）。P2 可选增强，本任务只做到「进输入框」，且开关默认关。
- 自动拉起未运行的 IM。目标不在运行时只提示，不代替用户启动进程。
- 非 Windows 平台实现。trait 留出边界，实现只做 win32。
- 指定会话/联系人级路由（发给某个具体好友）。本任务的目标粒度是应用窗口，`TargetScope::Conversation` 只留类型位不做实现。
- UIA 深度树遍历做内容识别。就绪度探测只做浅层且带超时。

## Technical Notes

全文出现的 exe 名 / 窗口类名 / 标题格式 / UIA 可用性 / `settle_ms` 数值**均为设计推演，本机尚未实测**
（对应 `DECISIONS.md` 行动项 A5、A6）。这些数值只影响 `profiles.builtin.toml` 的内容，不影响加载器与状态机代码，
因此实测与编码可并行，见 `implement.md` 的开工闸门。

已知未决风险：热键路径下本应用面板会抢走前台，IM 内部输入框 caret 是否在回切后自动归位，
各 IM 行为不一致，Electron 壳尤其可疑。这条属于结构性风险而非局部缺陷，回退方案见 `design.md`。
