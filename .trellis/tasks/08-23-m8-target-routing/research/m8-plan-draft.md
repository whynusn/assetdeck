# M8_PLAN.md — 多 IM 目标路由实施计划（可交付粒度）

依据：`DECISIONS.md` D13（设计定盘）· `TDD_PLAN.md` M8（红灯测试清单）· `AGENTS.md` 红线。
本文件是任务级分解，回答「怎么排、谁先谁后、每步交付什么文件、什么条件算做完」。
工期 2 周 / 10 个工作日。

**一句话锚点**：产品成功标准是「素材一键进入正确 IM 的输入框」。自动发送是 P2 可选增强，
全程注入序列绝不出现 `0x0D`。任何为了赶工期而动摇这一条的取舍，直接判不可交付。

## 零、开工闸门

| 闸门 | 内容 | 阻塞范围 | 不满足时的处置 |
| --- | --- | --- | --- |
| G1 | 新建 `crates/targets` 并登记进 `.trellis/config.yaml` 的 `packages` | T1 起全部 | 必须先做，0.5h 内完成 |
| G2 | 行动项 A5：本机装 8 个 IM，实测 exe / 窗口类名 / 标题模板 / 可接受剪贴板格式 / 未就绪态标题 | 只阻塞 `profiles.builtin.toml` 的**数值**，不阻塞加载器代码 | 拿不到的 IM 不进 builtin，留给用户自定义捕获 |
| G3 | 行动项 A6：`PrintWindow + PW_RENDERFULLCONTENT` 对 Electron 壳实测、WinEvent 钩子常驻内存实测、面板回切后 IM 输入框 caret 是否自动归位 | 阻塞 T7、T9 | 走「三、风险与回退」的对应回退项 |

**关键排期决定**：T1–T6 是纯逻辑（零 IO、零平台调用），不依赖任何实测数据，可在 D1 立即开工。
A5/A6 的实测与编码**并行**推进，实测结果只在 T7/T10 回填。这样实测拖期不会拖死整条关键路径。

## 一、任务分解

### T1 crate 骨架与类型地基（0.5d）

交付物：`crates/targets/Cargo.toml`、`crates/targets/src/lib.rs`、`crates/targets/src/model.rs`。

定义：`TargetId(String)` · `TargetScope{App, Conversation}` · `TargetBinding{id, hwnd: Option<WindowHandle>, label, last_seen_seq}` ·
`Health{Green, Yellow, Red, Unknown}` · `Readiness{Ready, Unknown, NotReady(NotReadyReason)}` ·
`NotReadyReason{NotLoggedIn, NoConversation, ReadOnly, ModalBlocking, Starting, WindowGone, Ambiguous, ProbeUnavailable}`。

验收门：`cargo test -p targets` 绿；`rg 'windows-sys|windows::' crates/targets` 零命中；`rg 'std::fs|std::io' crates/targets/src` 零命中。

### T2 profile 模型与加载/覆盖（1d）

交付物：`crates/targets/src/profile.rs`、`profiles/profiles.builtin.toml`（先放 schema + 2 条占位）。

要点：加载器签名吃 `&str` 而非路径（`fn load_profiles(builtin: &str, user: Option<&str>) -> Result<ProfileSet, ProfileError>`），
读文件的责任留给 `app-ui`，`targets` 保持零 IO。同 `id` 时 user 覆盖 builtin，字段级覆盖而非整条替换。

红灯测试：`user_profile_overrides_builtin_by_id` · `malformed_profile_is_rejected_not_silently_defaulted` ·
`unknown_exe_falls_back_to_generic_profile` · `negotiate_honors_profile_ordered_format_fallback`（协商侧断言在 T6 补齐）。

### T3 窗口匹配与打分（1d）

交付物：`crates/targets/src/matcher.rs`。

`fn resolve(snapshots: &[WindowSnapshot], profiles: &ProfileSet) -> MatchResult`，
`MatchResult{Matched(TargetBinding), Ambiguous(Vec<TargetBinding>), NoMatch}`。
打分维度按权重：exe 名 > 窗口类名 > 标题正则 > 可见性/尺寸阈值。

红线：`Ambiguous` 绝不静默取第一个，必须冒泡到 UI 让用户选；`generic_im` 兜底命中时 `TargetBinding` 标 `fallback: true`。

红灯测试：`resolve_two_wechat_windows_returns_ambiguous` · `minimized_window_still_matches_but_marked` · `generic_fallback_sets_fallback_flag`。

### T4 TargetTracker 粘性状态机（1.5d）★关键路径

交付物：`crates/targets/src/tracker.rs`。

```rust
impl TargetTracker {
    pub fn on_foreground(&mut self, snap: &WindowSnapshot, profiles: &ProfileSet);
    pub fn hot(&self) -> Option<&TargetBinding>;
    pub fn pin(&mut self, id: TargetId);
    pub fn unpin(&mut self);
    pub fn on_window_gone(&mut self, hwnd: WindowHandle);
}
```

两条铁律固化在此：改写热目标的**唯一**路径是「一个 eligible 目标窗口成为前台」；
其余任何前台变化（浏览器、资源管理器、我们自己的面板）一律不动热目标，且**无 TTL 无衰减**。
`on_window_gone` 只把 `hwnd` 置 `None`，保留 `TargetId`，从而托盘关闭再打开能重绑。

验收门：`rg 'Instant|SystemTime|Duration' crates/targets/src/tracker.rs` 零命中（时间不得参与决策）。

红灯测试：`eligible_target_foreground_rewrites_hot_target` · `unrelated_foreground_does_not_change_hot_target` ·
`own_panel_foreground_is_ignored_by_tracker` · `hot_target_has_no_ttl` · `pinned_target_not_overwritten` ·
`hot_target_survives_close_to_tray_and_reopen`，外加 1 条 proptest：任意长度的非 eligible 前台事件序列后，`hot()` 恒等于初始值。

**这条 proptest 必须在 D5 结束前变绿**，它是整个「精准定位」承诺的数学保证。

### T5 platform 新 trait 与 mock（1d）

交付物：`crates/platform/src/lib.rs` 增补 trait 定义 + `WindowSnapshot`。

`WindowEnumerator` · `WindowActivator` · `ForegroundObserver` · `ReadinessProbe`。
`WindowSnapshot{hwnd, exe_name, class_name, title, visible, minimized, rect}` 放在 `platform`
而不是 `targets`，避免 `platform → targets` 的反向依赖。

纪律沿用：`platform` trait 层零依赖零 `cfg` 门，windows-sys 只出现在 `cfg(windows)` target 门下。

### T6 pipeline 改造与 PasteFeedback（1.5d）★关键路径

交付物：改 `crates/pipeline/src/lib.rs`、`crates/pipeline/src/negotiate.rs`，新增 `crates/pipeline/src/feedback.rs`。

逐条改动：
1. `negotiate(req, profile: &Profile)` 改吃 profile，按 profile 声明的有序格式列表回落（原 `TargetProfile::ImGeneric` 枚举保留为 profile 的默认构造）。
2. `PasteSession.previous_foreground: Option<WindowHandle>` → `target: Option<TargetBinding>`；`begin_panel` 不再瞬时采样，改为从 `TargetTracker` 取。
3. 在写剪贴板与注入之间插入两步：`activate + settle`（等窗口真正到前台，确认超时 200ms）与就绪度探测。
4. `PasteOutcome::Injected` 携带 `verified: bool`；`Readiness::Unknown` 允许注入但标 `verified: false`。
5. auto-send 从 `paste()` 内部移出，成为独立的 `pub fn send(&self, deps)`，`paste()` 内部再无 `chord_enter` 调用点。
6. `feedback.rs`：`PasteFeedback{severity, target_label, headline, hint, action, diagnostic}`，
   由 `PasteOutcome` + `NotReadyReason` 映射，覆盖 D13 的 7 行文案表。

红灯测试（9 条）：`core_upload_path_never_synthesizes_enter` · `not_ready_no_conversation_never_injects` ·
`unknown_readiness_injects_but_marks_unverified` · `probe_timeout_falls_back_to_unknown_not_notready` ·
`foreground_drift_before_inject_aborts` · `every_not_ready_reason_maps_to_nonempty_feedback` ·
`feedback_headline_contains_target_label` · `all_degraded_feedback_mentions_clipboard_copied` ·
`auto_send_off_never_synthesizes_enter`（沿用 M6 序列断言，开关独立）。

验收门：M6 原有 7 条 pipeline 测试**一条不改地全绿**。若必须改断言，说明改动破坏了既有契约，停下来重设计。

### T7 win32 实现（1.5d，依赖 G3）

交付物：`crates/platform/src/win32.rs` 增补四个实现。

- 枚举：`EnumWindows` + `GetWindowThreadProcessId` + `QueryFullProcessImageNameW`，过滤不可见与零尺寸。
- 前台观察：`SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` 常驻，回调只投递消息不做决策（决策全在 T4 的纯函数里）。
- 激活：`ShowWindow(SW_RESTORE)` → `SetForegroundWindow` → 轮询确认到前台，超时 200ms 即降级。**不自动拉起未运行的 IM**。
- 就绪度：P0 纯 Win32（窗口存活 / 非最小化 / 无模态遮挡 / 标题不匹配未就绪模板）；P1 UIA 在独立 COM 线程跑，
  超时 50ms 返回 `Unknown` 而非 `NotReady`。

### T8 体检编排 L0–L3（1d）

交付物：`crates/targets/src/health.rs`。

L0 静态校验 → L1 进程/窗口存在 → L2 激活可达 → L3 自证（写哨兵文本 → 读回比对 → 清理，全程无 Enter）。
四色语义：绿只能来自 L3 通过；L1/L2 过但 L3 未跑或探测不可用一律黄。

红灯测试：`l3_selftest_sequence_contains_no_enter`（红线）· `l3_selftest_reads_back_sentinel_and_cleans_up` ·
`custom_target_requires_l0_l2_before_enabling` · `health_grade_downgrades_to_yellow_when_readiness_unprobeable`。

### T9 ui-viewmodels 目标条（1d，依赖 G3）

交付物：`crates/ui-viewmodels/src/target_bar_vm.rs`。

目标条是一枚 chip（不是下拉框）：显示当前目标名 + 四色健康点 + 图钉态。
热键路径下追踪器已给出目标 → **零点击**；管理器内唤起且有 Pinned/Tracked 命中同样零点击；
仅 `Ambiguous` 或 `NoMatch` 时展开选择列表。`fallback: true` 的目标首次使用弹一次确认。

红灯测试：`chip_shows_hot_target_without_user_click` · `ambiguous_expands_picker` ·
`fallback_target_requires_first_use_confirm` · `pin_toggle_freezes_chip`。

### T10 闭环收口与文档（1d）

扩展 M7 的 `closed_loop_*` 集成测试到多目标场景；把 A5/A6 实测数值回填 `profiles.builtin.toml`；
把 `DECISIONS.md` D13 中标注为「推演」的条目逐条改成实测结论或明确划掉；
在 `.trellis/spec/targets/backend/` 沉淀踩坑记录。

## 二、排期

| 日 | 内容 |
| --- | --- |
| D1 | G1 + T1 + T2 开工；A5 实测并行启动 |
| D2 | T2 收口 + T3 |
| D3 | T4（状态机主体） |
| D4 | T4 收口 + proptest |
| D5 | T5 + T6 开工；**T4 proptest 必须绿** |
| D6 | T6 主体（pipeline 六项改动） |
| D7 | T6 收口，M6 回归验证；A6 实测结果汇入 |
| D8 | T7 |
| D9 | T8 + T9 |
| D10 | T10 + DoD 逐条过 |

关键路径：T4 → T6 → T7。T3/T5/T8/T9 有浮动余量。

## 三、风险与回退

| 风险 | 触发信号 | 回退方案 |
| --- | --- | --- |
| 面板回切后 IM caret 不归位 | A6 实测发现粘贴落到窗口而非输入框 | 面板改 `WS_EX_NOACTIVATE` 无焦点浮层，彻底不抢前台；退一步用 profile 里的 `click_anchor` 坐标点一下输入框 |
| UIA 探不到 Electron 壳内部 | P1 探测恒返回 `ProbeUnavailable` | 该 profile 标 `readiness: p0_only`，健康点停在黄，允许注入但 `verified: false` |
| WinEvent 钩子吃内存 | 常驻内存突破 100MB 红线 | 降级为 1s 轮询 `GetForegroundWindow`，粘性语义不变（T4 是纯函数，不感知数据来源） |
| A5 拿不到某 IM | 无法安装或无账号 | 不进 builtin，靠 `generic_im` 兜底 + 用户自定义捕获，文档写明未覆盖清单 |
| T4 拖期 | D5 proptest 未绿 | T9 降级为只显示热目标、不做图钉与选择器，把余量还给 T6 |

## 四、验收清单（DoD）

功能：
1. 人在微信里按热键 → 面板 → 选素材 → 素材出现在微信输入框，全程零点击选目标。
2. 期间用户切到浏览器、切回、开资源管理器，热目标仍锁定微信。
3. 微信关到托盘再打开，热目标自动重绑新 HWND。
4. 未选会话时上框 → 不注入 → 提示「已复制，请先选择会话」。
5. 自定义一个未内置的 IM，走完 L0–L3 体检后可用。

红线（任一破则不可交付）：
1. 核心上框路径注入序列不含 `0x0D`。
2. `NotReady` 状态下从不注入。
3. `Unknown` 状态注入时必标 `verified: false`。
4. `Ambiguous` 从不静默选择。
5. `targets` crate 零 IO 零平台依赖，`tracker.rs` 无时间调用。
6. 空闲内存 ≤100MB、浏览 10 万条 ≤250MB 的 `mem-regression` job 仍绿。

质量：M6 原有测试零修改全绿；新增测试全部先红后绿，无一条是补写的事后测试。
