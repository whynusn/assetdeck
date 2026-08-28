# Implement — M8 多 IM 目标路由

## 第四轮结果（2026-08-24）：自动送焦点 + 「上框不发送」双 IM 闭环

**补上的关键一环**：此前每次「通过」都隐含一个前提——验证前人手点过一次 IM 输入框。
`Win32InputFocuser`（DECISIONS D21）落地后，产品进程 `asset-manager.exe` 双击瓦片
即可自行把键盘焦点送进输入框，全程无需人工点击：

| 路径 | 目标 | 结果 | 取证 |
|---|---|---|---|
| 双击瓦片（热目标 chip 自动锁定） | 微信 2163916 | 落框，未发送 | `r14-prod-wechat-1.png` |
| picker 选冷目标后双击 | 千牛 721614 接待中心 | 落框，「发送」按钮未触发 | `r14-prod-qianniu-1.png` |
| `real-im-verify` 复跑 dog.jpg | 千牛 / 微信 | 均落框未发送 | `r15-qianniu-after.png`、`r15-wechat-after.png` |
| `real-im-verify` 复跑 raw.mp4 | 微信 2163916 | 文件卡片停在输入框 | `r15-wechat-mp4.png` |

**修正第三轮的一条记录**：千牛的 mp4 不再走注入。千牛 `paste_sends=["files"]`（粘贴文件
即发送），按 D18 在协商阶段就跳过该格式，停在「只复制 + 提示手动 Ctrl+V」。第三轮标的
「通过（MP4 卡片）」是在「发送」语义上通过，与「止步输入框」这条红线冲突，现按红线收紧。

**质量门**：`cargo build --workspace --all-targets` / `fmt --check` / `clippy -D warnings` /
`test --workspace` 四项全绿，测试 0 failed。焦点与 `paste_sends` 的守卫测试已回填 `TDD_PLAN.md`。

## 第三轮结果（2026-08-23 晚）：真实双目标闭环 + 根因修复

**根因找到了，不是就绪度问题。** `CF_HDROP` 里写的是相对路径，被接收方 IM 静默丢弃整次粘贴。
详见 `research/real-im-closed-loop.md` 第 6 节与 DECISIONS D14。修复落在两处（`catalog_loader`
逐段拼接 + 绝对化，`platform::win32::hdrop_path_list` 平台层强制绝对化并宁可报错），
回归守卫五条已绿。

**就绪策略翻转**（DECISIONS D15）：内置四画像 `uia_strict → uia_shallow`，语义从「证明就绪才注入」
变为「否证阻塞才不注入」。caret 探测已被实验否证：微信 Qt 自绘、千牛 CEF，在**能成功粘贴的窗口上**
同样探测不到输入框。`uia_strict` 保留为用户可显式开启的严格档。

**千牛画像显式化**：带输入框的是「接待中心」窗口（`tb940472610424-接待中心`），不是「千牛工作台」。
上一轮记录的「千牛无会话」是看错了窗口。

**真实闭环矩阵**（截图证据在 `C:\Users\Administrator\Documents\Default_Project_probe\`）：

| 目标 | 文本 | 图片 | 视频 |
|---|---|---|---|
| 千牛 721614 接待中心 | 通过 | 通过（PNG + JPG） | 通过（MP4 卡片） |
| 微信 2163916 文件传输助手 | 通过 | 通过（JPG 渲染在输入框内） | 通过（1022.5K 卡片） |

> 上表「千牛 / 视频」一格已被第四轮修正为「只复制 + 提示」，见本文件顶部。

全程无 Enter，每次验证后 `Ctrl+A + Delete` 清场。PDD（停在培训页无会话）与 Telegram（未运行）
本轮跳过，记为「缺真实会话」。

**本轮剩余（不改变 `in_progress` 状态）**：`ui-viewmodels/target_runtime.rs` 的 Win32 直依赖迁到
`app-ui`；热键唤起 + 无焦点浮层；自定义目标持久化与 L0–L3 真实执行器；WinEvent 内存与
PrintWindow 收口；PDD/Telegram 真实验证。

## 当前复盘（2026-08-23）

权限已验证：当前进程为 `danger-full-access`，项目目录可读写、可运行真实桌面 Win32/UIA 自动化；Trellis 规范实际位于 `.trellis/workflow.md` 和 `.opencode/skills/trellis-*`，家目录不存在用户提到的 `.agent/`。

**真实闭环已取得第一个可信结果：**

1. `samples/library` 已通过 `library::Library::enqueue` 建立，含 4 张 PNG、4 张 JPG、2 个 MP4、文本哨兵。
2. `real-im-verify` 使用产品路径 `TargetRoutingRuntime::paste`，通过 UIA 将哨兵文本读回：`AM_VERIFY_20260823_千牛上框哨兵`；随后真实图片、真实 MP4 也成功上框并读回对象标记。
3. 没有按 Enter；验证结束后用 `Ctrl+A + Delete` 清场，不发送任何消息。
4. 微信 4.0 的两个进程已用 `process_id` 做实例区分，合并在 profile 后仍保持 `TargetId@HWND` 选择；托盘重绑不再只凭 profile id。
5. 新增 `ReadinessMode::UiaStrict`。**注意：本条已在第三轮被翻转**——内置画像改回 `uia_shallow`，
   `UiaStrict` 降级为用户可显式开启的严格档。理由见 DECISIONS D15：严格档在微信/千牛上等价于永不注入。

**“严格就绪”的真实结果（第二轮观测，其中千牛一条已被第三轮推翻）：**

- 微信当前聊天框有输入焦点：`notice[success]`，UIA 读回哨兵。
- 千牛有页面但无会话输入框：`notice[warning]`，不会注入。→ **第三轮推翻**：当时看的是「千牛工作台」窗口；真正带输入框的是「接待中心」窗口，切过去后文本/图片/视频均可上框。
- 两个微信实例同时存在时，`Ctrl+Alt+W` 是全局/会话级快捷键，可能被另一个实例或其他 IM（千牛同样占用 W/Alt 组合）抢走；**生产路径不能依赖该快捷键定位账号**，只能作为本地诊断辅助。

**任务仍保持 `in_progress`**，因为“生产可交付”还缺：

- 热键唤起/无焦点浮层/热目标捕获链路。
- 自定义目标持久化与 L0-L3 完整体检执行器。
- 千牛安全会话的自动选择；当前机器没有可点开的会话，因此只能作为“未就绪”严格阻断验证。
- PDD、Telegram 等真实上框验证。
- WinEvent 内存、PrintWindow、用户 profile 文件读写与最终质量门。

## 当前完成/未完成表

| 范围 | 状态 |
|---|---|
| `targets` 纯逻辑 crate | 已实现并有测试 |
| `platform` 窗口 trait + Win32 | 枚举/观察/激活/readiness 已实现；隐藏窗口、Alt 前台兜底、UIA 全局焦点盘 |
| `pipeline` targeted 编排 + 反馈 | 已实现并有测试 |
| `ui-viewmodels` 目标条/路由 VM | 已实现并有测试；实例号重绑守卫已落地 |
| Win32 装配 | 仍由 `ui-viewmodels` 持有具体 Win32，属于已知边界债 |
| 真实素材读取 | 已实现：文本/图片/视频可从 `.library` 物化 |
| 真实微信输入框闭环 | 已验证：文本、图片、MP4；未发送 |
| 严格 UIA 就绪 | 已实现：`uia_strict` 探测不到不注入 |
| 千牛真实会话输入框 | 未验证；当前无会话，严格阻断 |
| 热键唤起 | 未实现 |
| 自定义目标持久化/准星捕获 | 未实现 |
| 实例级稳定身份 | 已用进程号加窗口集做保守实例号；跨进程重启仍需显式确认 |
| 最终质量门 | 尚未全量跑完，下一步补 |

## Rev2 可交付实施顺序

1. **P0 闭环**：把当前 `real-im-verify` 真实验证固化成一个可重复的 `tools/real-im-verify` smoke 配方，并新增 `--quiet`、`--cleanup-input`、隐藏窗口选择。
2. **P1 生产装配**：把 Win32 具体实现从 `ui-viewmodels` 迁到 `app-ui`，或至少新增平台 trait 边界；目标聚焦于“双击素材一次上框”。
3. **P2 热目标/冷目标**：实现全局热键监听与前台快照捕获；面板无焦点浮层；同一 IM 多开时强制 picker。
4. **P3 无会话/错误**：补齐 `UiaStrict` 的“没有会话/只读/未登录”可证明状态，统一友好提示；当前代码已经不会注入未证明的输入框。
5. **P4 自定义目标**：新增用户 profile 文件读写、字段覆盖、L0-L3 执行器；无 L3 时黄色，不允许自动发送。
6. **P5 收口**：跑 `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo build --workspace`，再更新 spec/DECISIONS/TDD。

**验收规则**：只有“真实素材读回、目标窗口确认、未按 Enter、友好反馈”才算通过；Mock 通过不能替代真实 IM 结果。

## 每步状态（Red/Green）

### G1 crate 骨架与类型地基 — 已完成
- [x] `crates/targets/{Cargo.toml,src/lib.rs,src/model.rs}`，并登记 `.trellis/config.yaml` packages。
- [x] `TargetId` / `TargetScope` / `TargetBinding` / `Health` / `Readiness` / `NotReadyReason` 均已定义。
- [x] `rg 'windows-sys|windows::' crates/targets` 零命中；`rg 'std::fs|std::io' crates/targets/src` 零命中。

### T2 profile 模型与加载/覆盖 — 已完成
- [x] 红灯 `user_profile_overrides_builtin_by_id`（字段级覆盖）。
- [x] 红灯 `malformed_profile_is_rejected_not_silently_defaulted`。
- [x] 红灯 `unknown_exe_falls_back_to_generic_profile`。
- [x] `profile.rs` + `load_profiles(builtin, user)`（吃 &str）+ `profiles/profiles.builtin.toml` 两条占位。

### T3 窗口匹配与打分 — 已完成
- [x] 红灯 `resolve_two_wechat_windows_returns_ambiguous`（多开不静默取第一）。
- [x] 红灯 `minimized_window_still_matches_but_marked` / `generic_fallback_sets_fallback_flag`。
- [x] `matcher.rs`；exe 名 > 窗口类名 > 标题正则 > 可见性/尺寸。

### T4 TargetTracker 粘性状态机 — 已完成
- [x] 红灯 `eligible_target_foreground_rewrites_hot_target`。
- [x] 红灯 `unrelated_foreground_does_not_change_hot_target`。
- [x] 红灯 `own_panel_foreground_is_ignored_by_tracker`。
- [x] 红灯 `hot_target_has_no_ttl` / `pinned_target_not_overwritten`。
- [x] 红灯 `hot_target_survives_close_to_tray_and_reopen`（HWND 解绑后由同 profile 唯一候选重绑）。
- [x] proptest：任意长度非 eligible 事件序列后热目标恒定。
- [x] `tracker.rs` 暴露 `on_foreground / hot / pin / unpin / on_window_gone`。
- [x] `rg 'Instant|SystemTime|Duration' crates/targets/src/tracker.rs` 零命中。

> 注意：`hot_target_survives_close_to_tray_and_reopen` 目前只证明“同 profile 唯一窗口时可重新绑定”；没有证明它是同一账号/会话/窗口实例。AC3 的“可靠重绑”仍需实例级身份。

### T5 platform 新 trait 与 mock — 已完成
- [x] `WindowSnapshot` 放 `platform`（避免反向依赖）。
- [x] `WindowEnumerator` / `WindowActivator` / `ForegroundObserver` / `ReadinessProbe` 四个 trait。
- [x] `platform` trait 层零依赖零 `cfg` 门（既有 deps_guard 保持绿）。

### T6 pipeline 改造与 PasteFeedback — 已完成
- [x] `negotiate(req, &Profile)` 按 profile 有序格式回落；红灯 `negotiate_honors_profile_ordered_format_fallback`。
- [x] `PasteSession.target: Option<TargetBinding>`，`begin_targeted()` 从 tracker 取目标。
- [x] `paste_targeted()`：activate+settle → readiness → 最终前台复核 → inject。
- [x] 红灯 `not_ready_no_conversation_never_injects` / `unknown_readiness_injects_but_marks_unverified` / `foreground_drift_before_inject_aborts`。
- [x] `TargetPasteOutcome::Injected{verified: bool}`。
- [x] auto-send 移出 `paste_targeted()` 成独立 `send()`；红灯 `core_upload_path_never_synthesizes_enter`。
- [x] 新增 `feedback.rs`；红灯 `every_not_ready_reason_maps_to_nonempty_feedback` / `feedback_headline_contains_target_label` / `all_degraded_feedback_mentions_clipboard_copied`。
- [x] targeted 核心路径不调用 `send()`。

> `probe_timeout_falls_back_to_unknown_not_notready` 当前以 Mock `Inconclusive` 证明上层映射，不是真实 UIA 超时实现。真实 UIA 探测落地前不能写成已验证。

### T7 win32 实现 — 主体完成，焦点三级降级已落地（UIA 深探仍未做）
- [x] 枚举：`EnumWindows` + 进程 exe 名 + class + title，过滤不可见与零面积。
- [x] 前台观察：`SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`，回调只投递 HWND。
- [x] 激活：restore → `SetForegroundWindow` → 轮询确认 + settle。
- [x] `Win32InputFocuser`（D21）：`uia_focused_is_editable` → `uia_set_focus_on_editable`（带复核）→ `click_anchor`（客户区比例锚点 → `ClientToScreen` → `WindowFromPoint`+`GA_ROOT` 遮挡检查 → `SendInput ABSOLUTE|VIRTUALDESK` → 还原鼠标 → 复核前台）。真机验证：微信与千牛均无需人工点击输入框即可落框。
- [x] 焦点步只用鼠标/UIA，不合成任何按键；`Unavailable` 不降级为仅复制（守卫见 `crates/pipeline/tests`）。
- [ ] P1 UIA 独立 COM 线程、50ms 硬超时、Edit/Document 深度可写性检测：仍未实现。微信 4.0（Qt 自绘，UIA 树仅 2 节点）与千牛（CEF，不暴露 Edit）都会走到锚点点击这一级，故当前按 D15 走 `uia_shallow`。
- [x] `Win32Readiness` 只返回 Blocked(WindowGone/ModalBlocking) 或 Inconclusive；普通存活窗口恒 Inconclusive。

### T8 体检编排 L0–L3 — 仅判定层
- [x] 红灯 `l3_selftest_sequence_contains_no_enter`（从 SelfTestReport 判定，不注入）。
- [x] 红灯 `l3_selftest_reads_back_sentinel_and_cleans_up`（Mock 报告判定）。
- [x] 红灯 `custom_target_requires_l0_l2_before_enabling`。
- [x] 红灯 `health_grade_downgrades_to_yellow_when_readiness_unprobeable`。
- [x] 修正：窗口未运行时 health = `Unknown`（灰），不是 Red；有回归测试 `window_not_running_is_unknown_not_red`。
- [ ] 真实 L0-L3 执行编排（实际枚举、激活、哨兵写入/读回/清场）：未交付；当前 `health.rs` 只判定输入报告。

### T9 ui-viewmodels 目标条 — 已完成并有测试
- [x] `target_bar_vm.rs`：chip、冷目标选择、图钉、fallback 首次确认、四色点。
- [x] 红灯 `chip_shows_hot_target_without_user_click` / `ambiguous_expands_picker` / `fallback_target_requires_first_use_confirm` / `pin_toggle_freezes_chip`。
- [x] 选择键 `TargetId@HWND`；`same_profile_windows_are_selected_by_unique_window_key`。
- [x] `target_runtime.rs` 提供 Win32 装配与 `poll()` 低频兜底。
- [x] `.slint` / main.rs 目标条 UI 已接线。

### T10 闭环与文档 — 主链路闭环达成
- [x] `tools/bench-harness/tests/multi_target_routing_spec.rs`：选择 Telegram 只激活/inject Telegram，不触碰微信，且序列无 Enter。
- [x] `no_selected_target_still_copies_before_friendly_feedback`：无目标先复制再提示。
- [x] `.trellis/spec/targets/backend/` 已建立。
- [x] **真机双 IM 闭环（2026-08-24）**：产品进程 `asset-manager.exe` 双击真实素材 `dog.jpg`，热目标（微信 2163916）与显式 picker 选择（千牛 721614 接待中心）两条路径都把素材送进对应输入框，止步于输入框、发送按钮未触发、全程不手工点击输入框。取证 `Default_Project_probe/r14-prod-wechat-1.png`、`r14-prod-qianniu-1.png`、`r14-qianniu-after-verify.png`、`r14-wechat-after-verify.png`，收尾 `--cleanup-input` 见 `r14-qianniu-clean.png`。
- [x] 文档回填：`DECISIONS.md` D16–D21、`TDD_PLAN.md` M8 焦点/`paste_sends` 条目与红线映射、`platform`/`pipeline`/`targets`/`store`/`ui-viewmodels`/`app-ui` 各 spec。
- [ ] A5/A6 实测数值回填 `profiles.builtin.toml`：未完成（当前为占位）。
- [ ] 真实 IM caret 归位、PrintWindow、WinEvent 常驻内存：未验证。UIA 可用性已实测为「微信/千牛都读不到可写元素」（D15/D21），以截图为准。

## 验证命令

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
cargo test -p bench-harness --release -- --ignored --nocapture
```

纪律守卫：

```powershell
rg 'windows-sys|windows::' crates/targets
rg 'std::fs|std::io' crates/targets/src
rg 'Instant|SystemTime|Duration' crates/targets/src/tracker.rs
rg '0x0D|VK_RETURN|chord_enter' crates/pipeline/src/lib.rs
```

## 已知缺口 / 待裁决

1. 托盘自动重绑缺少稳定实例身份依据：可能把固定的微信 A 迁到同 profile 的微信 B。当前只在“唯一 profile 候选”时重绑，多开时转歧义，但“唯一候选”仍可能是另一个账号窗口。
2. ~~`target_runtime.rs` 直接导入 `platform::win32` 并持有具体实现~~ 已收口（D16）：`target_runtime.rs` 只持 trait 对象，Win32 具体类型仅在两处 `win32_runtime_deps()` 内构造，由 `crates/ui-viewmodels/tests/layering_guard.rs` 守卫。
3. 真实输入框「内容级」就绪度仍缺失：`Win32Readiness` 对普通存活窗口恒 `Inconclusive`（微信/千牛都不暴露可写元素）。焦点侧已由 `Win32InputFocuser` 锚点点击兜住，落框不再依赖人工点击。
4. 真实素材端到端已闭环（微信 + 千牛，图片/文本；千牛 mp4 按 D18 停在只复制）。**热键唤起**与**自定义目标持久化**（`main.rs` 里 `TargetRoutingRuntime::new(BUILTIN_PROFILES, None, ...)` 硬传 `None`）仍未实现。
5. A5/A6 数值仍是占位；PDD 656860 最小化、Telegram 未运行，缺真实会话可验。
6. `cargo-deny` 当前不可用（`error: no such command: deny`），未擅自安装。
7. 缩略图链路未接（`samples\library\thumbs` 为空，UI 瓦片是纯色块 + `#<id>`），真机点击仍靠绝对坐标；导入时自动派生 `paste.png` 也未接（生产 `MediaDispatcher` 缺失，离线工具 `tools/derive-paste-png` 可用）。

## 明确不做

- 自动发送（`paste_targeted()` 不调用 `send()`）。
- 自动拉起未运行的 IM。
- 会话/联系人级路由（`TargetScope::Conversation` 只留类型位）。
- UIA 深度树遍历做内容识别。
