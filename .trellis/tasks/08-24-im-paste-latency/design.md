# Design — 上框延迟：事件驱动取代固定睡眠与轮询

> 对应 `prd.md` R1~R7 / AC1~AC10。所有耗时数字来自 08-23 真机实测，见 prd「Confirmed Facts」。

## 1. 现状形态与问题定位

一次双击的同步调用链（全部跑在 Slint 事件循环回调里）：

```
app-ui::on_double_clicked            main.rs:346
  TargetRoutingRuntime::paste        target_runtime.rs:86
    Pipeline::paste_targeted         pipeline/src/lib.rs:184
      clipboard.write                亚毫秒
      activator.activate             SetForegroundWindow + 轮询 + sleep(settle_ms)
      focuser.focus_input            UIA 判定 → UIA 全子树扫描（注定失败）→ 锚点单击 + sleep(60)
      readiness.probe                又一次 UIA 往返，结果只用于置 verified
      focus/foreground 复核          亚毫秒
      injector.inject(Ctrl+V)        亚毫秒
```

四处「基于时钟」的等待，两类问题：

1. **轮询**：`wait_for_foreground` 10ms 步进（`win32.rs:411`）、`app-ui` 750ms `Timer`（`main.rs:289`）。
2. **固定睡眠**：`settle_ms`（`win32.rs:388` / `:403`）、`ANCHOR_CLICK_SETTLE_MS`（`win32.rs:1201`）、
   `UIA_FOCUS_SETTLE_MS`（`win32.rs:1085`）、调试辅助里的 400ms（`win32.rs:884`）。

另有两处非时钟浪费：每次上框重建 `IUIAutomation`（`win32.rs:955`）、以及在微信/千牛上
**候选数实测为 0** 的 UIA 全子树扫描（`win32.rs:1037`，微信 22~27ms / 千牛 83ms 纯损耗）。

## 2. 核心原语：先订阅，再动作，然后阻塞在事件上

### 2.1 为什么需要一个常驻事件泵

`SetWinEventHook(..., WINEVENT_OUTOFCONTEXT, ...)` 的回调由**安装钩子的线程的消息泵**投递。
现状之所以只能 `try_recv()` + 外部 `Timer`，正是因为钩子装在 Slint UI 线程上：
该线程一旦阻塞等待，就不再泵消息，事件永远不会到达——「阻塞等事件」在同一线程上自相矛盾。

因此新增一条**专用事件线程**：它安装钩子、跑 `GetMessage` 泵、把事件扇出给订阅者。
等待方（UI 线程或 verify 工具线程）只阻塞在 `mpsc::Receiver` 上，与消息泵解耦。

### 2.2 `Win32WinEventPump`（win32 模块内部，不进 trait 层）

```
进程内单例，首次订阅时惰性启动：
  thread "win32-winevent-pump"
    SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, ...)
    SetWinEventHook(EVENT_OBJECT_FOCUS,      EVENT_OBJECT_FOCUS, ...)
    SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE, ...)
    while GetMessage(&msg) > 0 { TranslateMessage/DispatchMessage }
    UnhookWinEvent × 3
```

- 三个钩子均 `WINEVENT_OUTOFCONTEXT`。**不再加 `WINEVENT_SKIPOWNPROCESS`**：
  锚点单击后的焦点事件来自目标进程，而前台事件里我们自己的窗口也需要被看见
  （目标条要能反映「用户切回素材管理器」）。自身事件由订阅过滤器排除，语义显式而非靠 flag 隐含。
- `EVENT_OBJECT_LOCATIONCHANGE` 只在 `idObject == OBJID_CARET` 时才有意义，扇出前先过滤，
  避免高频位置变更事件把通道灌满。
- 扇出结构：`Mutex<Vec<Subscription>>`，每项 `{ id, filter: EventFilter, sender: Sender<PumpEvent> }`。
  `sender.send()` 失败（接收端已 drop）即把该项从表中摘掉，不做定期清扫。
- `PumpEvent { kind: PumpEventKind, hwnd: WindowHandle, root: WindowHandle, process_id: u32, at: Instant }`。
  `root` 由 `GetAncestor(hwnd, GA_ROOT)` 在泵线程内解析：子控件事件也要能归属到目标顶层窗口。
- 单例句柄保存 `thread_id`，`Drop`/进程退出时 `PostThreadMessage(WM_QUIT)`。
  常驻代价：1 个线程 + 1 个 `Vec` 订阅表，远低于 AC10 的 100MB 空闲预算（实测记账见 §7）。

### 2.3 trait 层新增（`crates/platform/src/lib.rs`，仍零依赖零 cfg 门）

```rust
/// 一次「等某个可观测事实发生」的订阅。必须在触发动作**之前**建立：
/// 先动作后订阅会丢掉在两步之间发出的事件（本设计的核心竞态防线）。
pub trait EventWait {
    /// 阻塞直到订阅的事实发生或到达兜底上限。cap_ms 是上限，不是等待手段。
    fn wait(&mut self, cap_ms: u64) -> WaitOutcome;
}

pub enum WaitOutcome {
    /// 观察到事实发生，携带从订阅建立到事件到达的实际毫秒数（用于记账）。
    Observed { elapsed_ms: u64 },
    /// 到达兜底上限仍未观察到。调用方按「没能证明」处理，不得当成失败。
    CappedOut,
    /// 平台不提供该订阅（非 Windows 或钩子安装失败）。
    Unavailable,
}

/// 窗口事件订阅源。实现负责把平台事件模型收敛成「窗口成为前台」「输入面响应」两类事实。
pub trait WindowEventSource {
    fn await_foreground(&self, window: WindowHandle) -> Box<dyn EventWait>;
    /// 输入面响应＝目标窗口所属进程发生焦点变更或插入符出现。
    fn await_input_surface(&self, window: WindowHandle) -> Box<dyn EventWait>;
}
```

`WaitOutcome::CappedOut` 与既有 `FocusOutcome::Unavailable` / `ReadinessSignal::Inconclusive`
的语义对齐：**没能证明 ≠ 证明失败**，链路继续往下走并降级标记，不中止上框。

### 2.4 三处时钟等待的替换

| 位置 | 现状 | 改造后 |
|---|---|---|
| `wait_for_foreground` 轮询 | 10ms 步进查 `GetForegroundWindow` | 订阅 `EVENT_SYSTEM_FOREGROUND`(root==目标) → `SetForegroundWindow` → `wait(confirm_cap_ms)`；订阅建立后先做一次 `GetForegroundWindow` 快照，覆盖「订阅前就已是前台」 |
| `activate` 的 `sleep(settle_ms)` | 无条件睡满 | 订阅 `await_input_surface` → 激活 → `wait(settle_cap_ms)`；目标一报焦点/插入符即返回 |
| `click_anchor` 的 `Sleep(60)` | 无条件睡满 | 点击**前**订阅 `await_input_surface` → 点击 → `wait(ANCHOR_CLICK_SETTLE_CAP_MS)` → 复核前台未漂移 |
| `uia_set_focus_on_editable` 的 `Sleep(40)` | 无条件睡满 | 同上，`SetFocus` 前订阅、之后等事件 |
| `app-ui` 750ms `Timer` | 定时 `poll()` | 观察器唤醒回调 → `slint::invoke_from_event_loop` → `poll()` |
| `Win32Clipboard` 打开重试的 `Sleep` | 竞争失败后退避 | **保留**，唯一白名单项（§6 守卫测试显式声明理由） |

「订阅建立后先查一次当前状态」这条对每个等待都成立：事件驱动必须配一次初始状态读取，
否则事实在订阅前已经成立时会白等到上限。

## 3. 画像声明聚焦策略（R3 / AC4）

跳过注定失败的级别必须是**数据**，与 D18 `paste_sends` 同一建模方式。

`platform` trait 层（无 serde）：

```rust
pub enum FocusStep { AlreadyEditable, UiaSetFocus, AnchorClick }
pub struct FocusPlan { pub steps: Vec<FocusStep>, pub anchor: Option<FocusAnchor> }
pub trait InputFocuser { fn focus_input(&self, window: WindowHandle, plan: &FocusPlan) -> FocusOutcome; }
```

`targets` 侧镜像（可反序列化，与 `InputAnchor` 同一手法）：

```toml
focus_strategy = ["already", "anchor"]   # wechat / qianniu：跳过 UIA 全子树扫描
```

- 缺省值 = `["already", "uia", "anchor"]`，未声明画像行为与今日一致。
- 三处必须同步：`ProfilePatch` 字段、`merge_patch` 覆盖分支、`resolve_patch` 兜底值。
- 空数组视为配置错误（`ProfileError::EmptyFocusStrategy`），不静默变成「不聚焦」——
  静默的「什么都不做」会让 Ctrl+V 落空且没有任何线索。
- 微信/千牛的 `["already", "anchor"]` 依据写进 `profiles.builtin.toml` 注释：
  UIA 子树可写 Edit/Document 候选数实测 0，该级别在这两个目标上必然失败。

## 4. UIA 实例复用（R4）与就绪探测退出热路径（R5 / AC5）

### 4.1 复用

`IUIAutomation` 是 apartment-threaded、非 `Send`，只能按线程缓存：

```rust
thread_local! {
    static UIA: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
}
```

`CoInitializeEx(COINIT_APARTMENTTHREADED)` 每线程一次（`RPC_E_CHANGED_MODE` 仍按现状容忍：
宿主可能已把线程初始化成 MTA，此时沿用宿主套间）。首次 79ms 的 COM 冷启动只付一次。

### 4.2 就绪探测

```rust
pub trait ReadinessProbe {
    /// 完整探测：可能做 UIA 往返。只在 uia_strict 的「先证明再注入」路径上调用。
    fn probe(&self, window: WindowHandle, timeout_ms: u64) -> ReadinessSignal;
    /// 微秒级否证检查：只回答「有没有明确阻塞事实」（窗口失活 / 模态禁用），不做 UIA 往返。
    fn blockers(&self, window: WindowHandle) -> ReadinessSignal;
}
```

`paste_targeted` 分流：

```
match profile.readiness {
    UiaStrict            => readiness.probe(hwnd, 50)   // 语义不变：证明不了就不注入
    UiaShallow | P0Only  => readiness.blockers(hwnd)    // 只否证，注入前零 UIA 往返
}
```

**语义变化必须写明**：非严格档下 `verified` 不再可能由 UIA 探测置真，改由
`FocusOutcome` 决定（`AlreadyEditable` / `FocusedByUia` → true）。按 D15，微信/千牛上
浅探测本来就从未返回 `Ready`，所以这不是能力退化，而是把已知无效的往返从热路径拿掉。
`verified` 只影响提示文案，不影响是否注入。

## 5. `app-ui` 去 `Timer`（AC3）

```rust
pub trait ForegroundObserver {
    fn next_foreground(&mut self) -> Result<Option<WindowSnapshot>>;
    /// 注册「有事件到达」唤醒回调。返回 false = 该实现不支持推送，宿主需自行安排刷新。
    /// 默认实现返回 false，既有测试替身不受影响。
    fn set_wakeup(&mut self, _wakeup: Box<dyn Fn() + Send + Sync>) -> bool { false }
}
```

- `Win32ForegroundObserver` 改为订阅泵，`set_wakeup` 把回调交给泵线程在扇出后调用。
- `main.rs` 里回调体 = `slint::invoke_from_event_loop(|| routing.poll() + sync_target_bar())`。
  回调在泵线程上执行，只允许做 `invoke_from_event_loop` 这一件事，不触碰 `Rc`/UI 状态。
- `set_wakeup` 返回 false 时（非 Windows、钩子失败）保留 `Timer` 作为退路，
  但周期放宽到 2000ms 并只在这一条分支上存在。
- 目标条刷新里的**全窗口枚举**从每 750ms 一次变为「前台事件到达时」+「打开选择器时」，
  枚举本身不是时钟驱动的了。

## 6. 守卫测试（AC1）

`crates/platform/tests/no_timed_waits.rs`：读 `concat!(env!("CARGO_MANIFEST_DIR"), "/src/win32.rs")`
源文本，逐行扫描 `std::thread::sleep` / `Sleep(` / `Instant::now()` 步进循环，
除携带 `// sleep-allowed(<理由>)` 标记的行外一律断言不存在。
当前唯一白名单：`Win32Clipboard::write` 的打开重试退避（跨进程剪贴板锁竞争，
与「等目标应用做完某件事」不是同一类等待）。

该测试不依赖 Windows，任何平台都跑；靠源文本而不是人眼复查锁住性质。

## 7. 记账与验证（R6 / AC2 / AC6）

- `WaitOutcome::Observed { elapsed_ms }` 让每一步的真实等待时间可被上报。
  `real-im-verify` 增加 `--timings`：打印每阶段实测毫秒与 `Observed`/`CappedOut`。
- 尾段（Ctrl+V → 图片出现在输入框）用**目标进程事件静默**作为可观测代理：
  注入后继续收该进程的 `EVENT_OBJECT_*`，记录「最后一个事件距注入的毫秒数」，
  并同时记录剪贴板载荷字节数。`--tail-probe` 输出，写入 `research/latency-ledger.md`。
  这不是像素级证据（D15：截图才是判据），但足以回答「这段能不能由我们优化」。
- 真机复测：微信 hwnd 2163916（文件传输助手）、千牛 hwnd 721614（接待中心），
  注入后 `--cleanup-input` 清场，不向真实联系人发送。

## 8. 分层与红线

- 新增 `EventWait` / `WaitOutcome` / `WindowEventSource` / `FocusStep` / `FocusPlan` 全部落在
  trait 层，**不出现任何 Win32 类型、不加 cfg 门**；`Win32WinEventPump` 是 win32 模块私有实现。
- 具体类型仍只在 `app-ui::win32_runtime_deps()` 与 `tools/real-im-verify` 的同名函数内构造（D16），
  由 `crates/ui-viewmodels/tests/layering_guard.rs` 守卫。
- 注入序列不变：`chord_paste()`，不含 `0x0D`。聚焦动作仍只有 UIA `SetFocus` 与画像锚点单次左键单击。
  三道既有守卫（`auto_send_flag_defaults_off`、`auto_send_off_never_synththesizes_enter`、
  `target_runtime_spec.rs` 的 `0x0D` 断言）不得放宽。

## 9. 兼容性与回滚

| 改动 | 兼容性 | 回滚点 |
|---|---|---|
| 事件泵 + `EventWait` | 纯新增；`Unavailable`/`CappedOut` 退化为「等到上限」，不差于今日 | 单独 commit，可整块回退 |
| `activate` 参数语义（sleep → cap） | 签名不变，`settle_ms` 画像字段名保留（语义改为上限，注释写明） | 同上 |
| `focus_strategy` | 新字段，缺省等于今日三级顺序 | 删字段即回到旧行为 |
| `ReadinessProbe::blockers` | trait 新方法，无默认实现 → 所有替身需补；`uia_strict` 语义不变 | 分流处一行改回 `probe` |
| `InputFocuser::focus_input` 签名换成 `&FocusPlan` | 破坏性（3 个测试替身 + bench-harness），编译期可穷尽 | — |
| `app-ui` 去 `Timer` | `set_wakeup` 返回 false 时自动退回定时刷新 | 恢复 750ms `Timer` |

## 10. 已知风险

1. **事件不来**。Qt 自绘/CEF 可能不报 `EVENT_OBJECT_FOCUS`。后果是等到 cap，
   与今日睡满等价，不会更慢；真机实测会记录 `Observed`/`CappedOut` 比例，
   若某目标恒为 `CappedOut`，则把该目标的 cap 调小才是正确动作（有据可依地调，而不是拍脑袋）。
2. **泵线程回调重入**。扇出在持锁状态下调用 `sender.send()` 与唤醒回调；
   唤醒回调只允许 `invoke_from_event_loop`，禁止回调里再订阅（会死锁）。
   在 trait 文档注释里写成硬约束。
3. **UIA 线程缓存跨用途污染**。`real-im-verify` 的调试辅助与产品路径可能在同线程共用实例——
   本来就是同一套间同一用途，无额外风险；但缓存不得跨线程共享（非 `Send`，编译器保证）。
4. **前台事件的 root 归属**。微信/千牛的会话窗口与主窗口是不同 HWND，
   过滤必须用 `GetAncestor(GA_ROOT)` 后的值比对目标，否则会漏掉子窗口事件。
