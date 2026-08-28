# Implement — 上框延迟：事件驱动取代固定睡眠与轮询

> 依据 `prd.md`（R1~R7 / AC1~AC10）与 `design.md`（§2~§9）。
> 顺序按「风险从低到高」排列，每步都是可独立编译、可独立回退的单元。
> 每步末尾的验证命令必须真跑，绿了才进下一步。

构建前置（每次 `cargo build/run` 之前）：

```powershell
cd "C:\Users\Administrator\Documents\Default Project"
Get-Process asset-manager -EA SilentlyContinue | Stop-Process -Force   # 否则 os error 5
```

---

## 步骤 0 — 改造前基线记账（AC2 的分母）

- [x] 0.1 建 `research/latency-ledger.md`，写入 prd「Confirmed Facts」表作为 **before** 列。
- [x] 0.2 微信 hwnd 2163916 跑一次完整验证，记录墙钟总时长与各阶段：
      `cargo run -q -p real-im-verify -- --library samples/library --profile wechat --hwnd 2163916 --asset-index 13 --asset-file dog.jpg`
- [x] 0.3 千牛 hwnd 721614 同样跑一次（图片素材，避开 `paste_sends=["files"]` 的视频路径）。
- [x] 0.4 两次的载荷字节数（`objects/<uuid>/paste.png` 大小）一并记入 ledger。

回滚点：无代码改动。

---

## 步骤 1 — UIA 实例线程内复用（R4，最低风险）

- [x] 1.1 `crates/platform/src/win32.rs`：`uia_automation()` 改为 `thread_local!` 缓存
      （`RefCell<Option<IUIAutomation>>`），`CoInitializeEx` 每线程一次，
      `RPC_E_CHANGED_MODE` 沿用现状容忍语义。
- [x] 1.2 注释写清：`IUIAutomation` 是 apartment-threaded 且非 `Send`，因此只能按线程缓存，
      不得放进 `OnceLock`/`static`。
- [x] 1.3 验证：`cargo clippy -p platform --all-targets -- -D warnings`；
      `cargo run -q -p real-im-verify -- --library samples/library --profile wechat --hwnd 2163916 --inspect-only --cleanup-input --quiet`
      仍能读出焦点信息，且首次 79ms 的 COM 冷启动只出现一次。

回滚点：commit A（单文件单函数）。

---

## 步骤 2 — 画像声明聚焦策略（R3 / AC4）

- [x] 2.1 `crates/platform/src/lib.rs`：新增 `FocusStep { AlreadyEditable, UiaSetFocus, AnchorClick }`
      与 `FocusPlan { steps, anchor }`；`InputFocuser::focus_input` 签名改成 `(&self, WindowHandle, &FocusPlan)`。
      trait 层仍零依赖零 cfg 门。
- [x] 2.2 `crates/targets/src/profile.rs`：加 `FocusStrategyStep` 可反序列化镜像 + `Profile::focus_strategy`
      + `Profile::focus_plan()`；**三处同步**（`ProfilePatch` 字段 / `merge_patch` 分支 / `resolve_patch` 兜底）。
      缺省 `["already","uia","anchor"]`；空数组 → 新错误 `ProfileError::EmptyFocusStrategy`。
- [x] 2.3 `profiles/profiles.builtin.toml`：wechat / qianniu 声明 `focus_strategy = ["already","anchor"]`，
      注释写实测依据（UIA 子树可写候选数 0，该级别必然失败；微信 22~27ms、千牛 83ms 纯损耗）。
      pdd / telegram 不声明（未取得真机会话，保留三级顺序）。
- [x] 2.4 `crates/platform/src/win32.rs`：`Win32InputFocuser::focus_input` 按 `plan.steps` 顺序执行，
      不再硬编码三级降级；步骤表为空由 targets 层拦下，平台层遇空返回 `Unavailable` 并保持纯函数性。
- [x] 2.5 `crates/pipeline/src/lib.rs:254`：改传 `&profile.focus_plan()`。
- [x] 2.6 修编译期暴露的替身：`crates/pipeline/tests/target_routing_spec.rs:109`、
      `crates/ui-viewmodels/tests/target_runtime_spec.rs:121`、
      `tools/bench-harness/tests/multi_target_routing_spec.rs:104`。
- [x] 2.7 新测试：`crates/targets` 内断言 (a) 未声明画像得到三级缺省；
      (b) wechat 内置画像的 steps 不含 `UiaSetFocus`；(c) 空数组报 `EmptyFocusStrategy`。
- [x] 2.8 新测试：`crates/pipeline/tests/target_routing_spec.rs` 断言假 focuser 收到的
      `plan.steps` 与画像声明一致（策略是数据、不是代码分支，由测试锁住）。
- [x] 2.9 验证：`cargo test -p targets -p pipeline -p ui-viewmodels`；真机跑微信 + 千牛各一次，
      上框仍成功，ledger 记下少掉的 UIA 扫描耗时。

回滚点：commit B。

---

## 步骤 3 — 就绪探测退出热路径（R5 / AC5）

- [x] 3.1 `crates/platform/src/lib.rs`：`ReadinessProbe` 加 `blockers(&self, WindowHandle) -> ReadinessSignal`
      （无默认实现，逼所有实现显式表态）。文档注释写明：只做微秒级否证，禁止 UIA 往返。
- [x] 3.2 `crates/platform/src/win32.rs`：`Win32Readiness::blockers` = `IsWindow` + `IsWindowEnabled`
      两项检查，其余一律 `Inconclusive`；`probe` 保持含 UIA 往返不变。
- [x] 3.3 `crates/pipeline/src/lib.rs:271`：按 `profile.readiness` 分流——
      `UiaStrict → probe(hwnd, 50)`；`UiaShallow | P0Only → blockers(hwnd)`。
      `Blocked` 分支的中止语义完全不变。
- [x] 3.4 注释写明语义变化：非严格档下 `verified` 改由 `FocusOutcome` 决定；
      按 D15 浅探测在微信/千牛从未返回 `Ready`，故非能力退化。`verified` 只影响文案。
- [x] 3.5 补齐所有替身的 `blockers`（pipeline / ui-viewmodels / bench-harness 三处测试）。
- [x] 3.6 新测试（AC5）：同一素材分别用 `uia_shallow` 与 `uia_strict` 画像跑 `paste_targeted`，
      断言 shallow 路径上假 probe 的 `probe()` 调用次数为 0、`blockers()` 为 1；strict 路径相反。
- [x] 3.7 验证：`cargo test -p pipeline`；真机微信 + 千牛各一次。

回滚点：commit C。

---

## 步骤 4 — WinEvent 事件泵（R1/R2 的基座，最高风险）

- [x] 4.1 `crates/platform/src/lib.rs`：新增 `WaitOutcome { Observed{elapsed_ms}, CappedOut, Unavailable }`、
      `trait EventWait { fn wait(&mut self, cap_ms: u64) -> WaitOutcome }`、
      `trait WindowEventSource { await_foreground, await_input_surface }`。
      文档注释里写死两条硬约束：**必须先订阅再动作**；**唤醒回调内禁止再订阅**（会死锁）。
- [x] 4.2 `crates/platform/src/win32.rs`：实现 `Win32WinEventPump` 单例
      （专用线程 + 三个 `SetWinEventHook` + `GetMessage` 泵 + `Mutex<Vec<Subscription>>` 扇出）。
      - `EVENT_SYSTEM_FOREGROUND` / `EVENT_OBJECT_FOCUS` / `EVENT_OBJECT_LOCATIONCHANGE`
      - `LOCATIONCHANGE` 仅 `idObject == OBJID_CARET` 放行，其余丢弃
      - 泵线程内解析 `GetAncestor(hwnd, GA_ROOT)` 与 `process_id`，扇出携带
      - 去掉 `WINEVENT_SKIPOWNPROCESS`，改由订阅过滤器排除自身（语义显式）
      - `Drop` → `PostThreadMessage(thread_id, WM_QUIT)`；`send` 失败即摘除该订阅
- [x] 4.3 实现 `Win32WindowEvents`（`WindowEventSource`）与 `Win32EventWait`（`EventWait`）：
      `wait` = `Receiver::recv_timeout(cap)`，循环丢弃不匹配事件直到匹配或超上限；
      返回 `Observed{elapsed_ms}`（订阅建立到事件到达）。
- [x] 4.4 import 核对（历史踩坑清单）：`SetWinEventHook`/`UnhookWinEvent`/`HWINEVENTHOOK` 在
      `windows_sys::Win32::UI::Accessibility`；`EVENT_*`/`WINEVENT_*`/`GetAncestor`/`GA_ROOT`/`OBJID_CARET`
      在 `Win32::UI::WindowsAndMessaging`。
- [x] 4.5 单元测试（可在 Windows 上跑）：装两个订阅，`PostThreadMessage` 唤醒后确认
      两个 receiver 都收到；drop 一个后确认订阅表收缩。
- [x] 4.6 验证：`cargo test -p platform`；`cargo clippy -p platform --all-targets -- -D warnings`。

回滚点：commit D（纯新增，未接线，行为零变化）。

---

## 步骤 5 — 把三处时钟等待换成事件等待（AC1）

- [x] 5.1 `Win32WindowActivator::activate`：
      订阅 `await_foreground(target)` → 快照一次 `GetForegroundWindow`（覆盖「已经是前台」）
      → `ShowWindow`+`SetForegroundWindow` → `wait(confirm_timeout_ms)`；
      成功后订阅 `await_input_surface(target)` → `wait(settle_ms)`。
      删掉 `win32.rs:388` / `:403` 的 `sleep` 与 `wait_for_foreground` 整个函数。
      Alt 敲击兜底路径同样改为事件等待（Alt 之后重新订阅再等）。
- [x] 5.1b（D16 冷激活稳定性修复）：真机交替冷切换暴露出「拿到一条瞬时前台事件≠稳定前台」——
      裸 `SetForegroundWindow` 跨进程会被前台锁拒绝（千牛闪红）或只产生瞬时前台（微信弹回），
      第一轮据此早退 `Ok(true)`，pipeline `preinject` 随后发现 `fg!=target` 以 `WindowGone` 中止。
      修复：冷目标直接走 Alt 释放前台锁 → `SetForegroundWindow`（抽成 `drive_foreground`），
      成功判据升级为「稳定前台」——动作后立刻 `GetForegroundWindow()==hwnd`，或等到一条
      `EVENT_SYSTEM_FOREGROUND` 后**再复核一次**前台仍在目标；不稳则第二轮重试。全程无 sleep/轮询。
      A/B（微信↔千牛交替冷切换各 3 轮）：两者均 `activate 4~7ms`、`preinject fg==target`、notice 正常。
- [x] 5.2 `click_anchor`：`SendInput` **之前**订阅 `await_input_surface`，点击后 `wait(ANCHOR_CLICK_SETTLE_CAP_MS)`，
      随后复核 `GetForegroundWindow() == hwnd`。删 `win32.rs:1201` 的 `Sleep`。
- [x] 5.3 `uia_set_focus_on_editable`：`SetFocus` 前订阅，之后 `wait(UIA_FOCUS_SETTLE_CAP_MS)`。
      删 `win32.rs:1085` 的 `Sleep`。
- [x] 5.4 常量改名以反映语义：`ANCHOR_CLICK_SETTLE_MS → ANCHOR_CLICK_SETTLE_CAP_MS`、
      `UIA_FOCUS_SETTLE_MS → UIA_FOCUS_SETTLE_CAP_MS`；`profiles` 的 `settle_ms` 字段名保留
      （用户可见配置不做破坏性改名），注释写明语义已从「睡多久」变成「最多等多久」。
- [x] 5.5 `win32.rs:884` 调试辅助里的 400ms：这是 `uia_focus_wechat_input`（仅 verify 工具用），
      同样改为事件等待，不留时钟兜底以外的睡眠。
- [x] 5.6 守卫测试 `crates/platform/tests/no_timed_waits.rs`（AC1）：
      扫 `src/win32.rs` 源文本，禁 `std::thread::sleep` / `Sleep(` / `Instant` 步进循环，
      仅允许带 `// sleep-allowed(<理由>)` 标记的行；当前白名单只有剪贴板打开重试退避。
- [x] 5.7 验证：`cargo test -p platform`；真机微信 + 千牛各跑一次，
      ledger 记 `Observed`/`CappedOut` 与各步实际等待毫秒。

回滚点：commit E。**这一步若真机失败，回退到 commit D 即恢复今日行为。**

---

## 步骤 6 — `app-ui` 去 `Timer`（AC3）

- [x] 6.1 `crates/platform/src/lib.rs`：`ForegroundObserver` 加
      `fn set_wakeup(&mut self, Box<dyn Fn() + Send + Sync>) -> bool { false }`（带默认实现，替身不受影响）。
- [x] 6.2 `Win32ForegroundObserver` 改为订阅泵（不再自持钩子），`set_wakeup` 返回 true。
      移除「同一进程只允许一个观察器」的限制（泵支持多订阅），或保留限制但改由泵表达。
- [x] 6.3 `crates/app-ui/src/main.rs`：`set_wakeup` 成功 → 回调体只做
      `slint::invoke_from_event_loop(...)`（内部 `routing.poll()` + `sync_target_bar`）；
      返回 false → 保留 `Timer` 但周期放宽到 2000ms，且仅存在于这一条退路分支。
- [x] 6.4 `TargetRoutingRuntime::poll()`：全窗口枚举不再每次都做——
      前台事件到达与 `open_picker()` 时做，其余复用上次快照。
- [x] 6.5 验证：启动 `asset-manager`，手动在微信/千牛/素材管理器之间切换，
      目标条即时跟随（不是最多 750ms 后）；`cargo test -p ui-viewmodels`。

回滚点：commit F。

---

## 步骤 7 — 尾段真相与记账（R6 / AC2 / AC6）

- [x] 7.1 `tools/real-im-verify`：加 `--timings`（打印每阶段 `Observed{elapsed_ms}`/`CappedOut`）
      与 `--tail-probe`（注入后持续收目标进程 `EVENT_OBJECT_*`，记录最后事件距注入的毫秒数）。
- [ ] 7.2 用三个不同体积的素材各跑一次（712KB / 1.05MB / 1.46MB 的 `paste.png`），
      记录尾段静默时间与载荷字节的关系。
- [ ] 7.3 写完 `research/latency-ledger.md`：before/after 分项对照 + 我们自己占时的降幅百分比
      （AC2 要求 ≥40%）+ 尾段实测 + 结论「这段在 IM 进程内，缩小载荷是唯一手段，属另立任务」。
- [ ] 7.4 若降幅未达 40%：不得调参凑数，回到 ledger 找最大剩余项并在本任务内继续优化，
      或在 prd 里如实记录未达标原因并交用户裁决。

回滚点：仅文档与工具，无产品代码。

---

## 步骤 8 — 全量质量门与红线复核

- [ ] 8.1 `cargo fmt --check`
- [ ] 8.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] 8.3 `cargo test --workspace`（约 50s，超时需 `write_stdin` 续等）
- [ ] 8.4 红线复核（AC7）：三道守卫全绿——`auto_send_flag_defaults_off`、
      `auto_send_off_never_synththesizes_enter`、`target_runtime_spec.rs` 的 `0x0D` 断言。
      再 grep 一次确认上框路径无 `0x0D`、无 `send()` 调用。
- [ ] 8.5 分层复核（AC9）：`crates/platform/src/lib.rs` 逐字确认无 `cfg(`、无平台 crate 引用；
      `cargo test -p ui-viewmodels --test layering_guard`。
- [ ] 8.6 内存复核（AC10）：启动 `asset-manager`，空闲 60s 后读工作集，确认未破 100MB 预算，
      数字记入 ledger（新增常驻泵线程的代价要有数）。
- [ ] 8.7 真机终验：微信 hwnd 2163916 + 千牛 hwnd 721614 各双击一个真实图片素材，
      截图确认素材停在输入框、**未发送**，随后 `--cleanup-input` 清场。

---

## 步骤 9 — Phase 3 收尾

- [ ] 9.1 `trellis-update-spec`：更新 `.trellis/spec/platform/backend/*`（事件泵与「先订阅再动作」纪律、
      `blockers` 与 `probe` 的分工）、`.trellis/spec/targets/backend/*`（`focus_strategy` 三处同步规则）、
      `.trellis/spec/app-ui/backend/*`（唤醒回调只允许 `invoke_from_event_loop`）。
- [ ] 9.2 新增决策 D23（事件驱动等待原语 + 先订阅再动作）与 D24（`focus_strategy` 为画像级数据）。
- [ ] 9.3 按 workflow 3.4 出示 commit 计划，等用户一次性确认后再提交。
      （工作树内已有上一轮缩略图工作的改动，属自己的改动，不 revert；
      commit 计划中与本任务无关的路径单列，交用户裁决。）

---

## 验证命令速查

```powershell
cd "C:\Users\Administrator\Documents\Default Project"
Get-Process asset-manager -EA SilentlyContinue | Stop-Process -Force
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pwsh -NoProfile -File scripts/enum-im-windows.ps1 | Format-Table -AutoSize
cargo run -q -p real-im-verify -- --library samples/library --profile wechat  --hwnd 2163916 --asset-index 13 --asset-file dog.jpg --timings
cargo run -q -p real-im-verify -- --library samples/library --profile qianniu --hwnd 721614  --asset-index 13 --asset-file dog.jpg --timings
cargo run -q -p real-im-verify -- --library samples/library --profile wechat  --hwnd 2163916 --inspect-only --cleanup-input --quiet
Start-Process target\debug\asset-manager.exe -ArgumentList '--library-root','samples\library'
```

## 红线（每步都成立，破则停手）

- 只上框，绝不自动发送。不合成 `VK_RETURN`(0x0D)，不调 `send()`，`auto_send` 默认 false。
- 不向真实联系人发消息：微信只用 hwnd 2163916（文件传输助手），**禁用 197440**；千牛只用 721614。
- Mock 绿 ≠ 交付。每个改变行为的步骤都必须有真机两 IM 的实测。
- 编辑一律 `apply_patch` + 绝对路径。未经用户要求不 commit。
