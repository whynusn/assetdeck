# Quality Guidelines — platform

## Scenario: Windows 多 IM 窗口事实采集

### 1. Scope / Trigger

- 新增或修改窗口枚举、前台观察、激活、readiness 或按键注入时适用。

### 2. Signatures

```rust
pub trait WindowEnumerator { fn windows(&self) -> Result<Vec<WindowSnapshot>>; }
pub trait WindowActivator {
    fn activate(&self, window: WindowHandle, confirm_timeout_ms: u64, settle_ms: u64)
        -> Result<bool>;
}
pub trait ForegroundObserver { fn next_foreground(&mut self) -> Result<Option<WindowSnapshot>>; }
pub trait ReadinessProbe {
    fn probe(&self, window: WindowHandle, timeout_ms: u64) -> ReadinessSignal;
}
```

### 3. Contracts

- `lib.rs` 不导入 windows crate、不含 cfg 门；Win32 实现只在 `win32.rs`。
- 观察回调只投递系统事实，不做 profile 匹配或热目标决策。
- `activate` 的成功含义是指定 HWND 在超时内成为前台，不是“输入框已就绪”。
- `Ready` 必须有可写输入框证据；探不到返回 `Inconclusive`。
- SendInput 与剪贴板格式是本层实现细节。
- 隐藏但有面积的目标窗口仍应进入冷目标选择列表，但 `visible=false` 必须传到上层 UI；热目标自动追踪仍只信任当前前台快照。
- `WindowSnapshot::process_id` 是当前会话级实例线索；同一 profile 的不同进程不能在该层被静默合并。
- `ReadinessMode::UiaStrict` 由 pipeline 把 `Inconclusive` 映射为“已复制、不注入”；平台层不得把 UIA 探不到伪装成 `Ready`。

### 4. Validation & Error Matrix

| 条件 | 结果 |
|---|---|
| EnumWindows/WinEvent/激活 API 失败 | `PlatformError::Window` |
| 指定 HWND 无效 | activation Err / readiness WindowGone |
| 激活超时 | `Ok(false)` |
| disabled HWND | readiness ModalBlocking |
| UIA 不可用或超时 | `Inconclusive` |
| SendInput 未完整送达 | `PlatformError::Inject` |
| 目标隐藏但可恢复 | 保留候选并标 `visible=false` |
| UIA 无焦点可写输入框 | `Inconclusive`（严格 profile 由上层拒绝注入） |

### 5. Good/Base/Bad Cases

- Good：激活 HWND 202 后轮询确认前台确实为 202，再交还 pipeline。
- Base：普通 Electron 窗口存活但无 UIA 证据，返回 `Inconclusive`。
- Bad：仅因 `IsWindow(hwnd)` 为真就返回 `Ready`。

### 6. Tests Required

- trait 可由纯 Rust Mock 覆盖上层顺序、错误和三态语义。
- Win32 真实枚举、激活、SendInput、UIA/readiness 测试必须 `#[ignore]` 并在真实桌面会话手动跑。
- A5/A6 必须记录每个 IM 的 exe/class/title、caret 归位和 UIA 可用性；Mock 不能替代。

### 7. Wrong vs Correct

#### Wrong

```rust
if IsWindow(hwnd) != 0 { ReadinessSignal::Ready }
```

#### Correct

```rust
if !window_is_valid(hwnd) {
    ReadinessSignal::Blocked(ReadinessBlocker::WindowGone)
} else {
    ReadinessSignal::Inconclusive
}
```

## Mechanical Guards

```powershell
rg 'windows-sys|windows::|cfg' crates/platform/src/lib.rs
cargo test -p platform
```

## Scenario: 剪贴板文件载荷（CF_HDROP）

### 1. Scope / Trigger

- 新增或修改 `ClipboardPayload::Files` 的写入路径、或任何构造给剪贴板的文件路径的代码时适用。

### 2. Contracts

- 写入 `CF_HDROP` 的每条路径**必须是绝对路径**。HDROP 的路径由接收方进程按它自己的工作
  目录解析，相对路径会让 IM 静默丢弃整次粘贴：输入框毫无变化，且不产生任何可捕获的错误。
- 绝对化在平台层强制执行，不依赖调用方自觉。无法绝对化或绝对化后仍相对 →
  `PlatformError::Clipboard`。**宁可报错，也不静默失败。**
- 路径列表布局：每条路径 UTF-16 且单 NUL 结尾，列表整体再补一个 NUL 终止；`DROPFILES.fWide = 1`。
- 空路径列表拒绝写入。

### 3. Validation & Error Matrix

| 条件 | 结果 |
|---|---|
| 相对路径可绝对化 | 提升为绝对路径后写入 |
| 绝对化失败（如 cwd 不可用） | `PlatformError::Clipboard` |
| 绝对化后仍相对 | `PlatformError::Clipboard` |
| 空列表 | `PlatformError::Clipboard` |

### 4. Wrong vs Correct

#### Wrong

```rust
list.extend(path.as_os_str().encode_wide()); // 相对路径原样写入 → IM 静默丢弃
```

#### Correct

```rust
let absolute = std::path::absolute(path).map_err(/* → PlatformError::Clipboard */)?;
list.extend(absolute.as_os_str().encode_wide());
```

### 5. Tests Required

- `hdrop_promotes_relative_paths_to_absolute`、`hdrop_keeps_absolute_paths_and_terminates_list`、
  `hdrop_rejects_empty_path_list`（`crates/platform/src/win32.rs` 内联单测，无需真实桌面）。
- 真实 IM 图片/视频上框须有截图证据；剪贴板写入成功不等于素材进了输入框。

---

## Scenario: 键盘焦点送进输入框（InputFocuser）

### 1. Scope / Trigger

- 新增或修改 `InputFocuser` 实现、`FocusAnchor` 换算、或任何在注入前改变焦点/前台状态的代码时适用。

### 2. Signatures

```rust
pub struct FocusAnchor { pub x_ratio: f32, pub y_ratio: f32 }

pub enum FocusOutcome { AlreadyEditable, FocusedByUia, FocusedByAnchor, Unavailable }

pub trait InputFocuser {
    fn focus_input(&self, window: WindowHandle, anchor: Option<FocusAnchor>) -> FocusOutcome;
}
```

### 3. Contracts

- `SetForegroundWindow` 只保证窗口到前台，**不保证键盘焦点在输入框**。微信 4.0 与千牛的焦点会停在
  窗口根控件（`Qt51514QWindowIcon` / `Qt5152QWindowIcon`），Ctrl+V 因此落空。聚焦必须是注入前的独立步骤。
- 三级降级顺序固定：已聚焦可写控件 → UIA `SetFocus` → 锚点点击。每一级都必须**复核**成功，
  不允许「调用没报错就当成功」。UIA 分支复核 `uia_focused_is_editable()`，锚点分支复核前台窗口未漂移。
- 锚点比例先夹紧到 `[0.02, 0.98]`，再经 `GetClientRect` + `ClientToScreen` 换算成屏幕坐标；
  `SendInput` 用 `ABSOLUTE | VIRTUALDESK`，坐标须减去虚拟桌面原点后归一化到 `0..65535`（多显示器/负坐标必需）。
- 点击前用 `WindowFromPoint` + `GetAncestor(GA_ROOT)` 确认锚点属于目标窗口；被别的窗口遮挡时**放弃点击**，
  返回 `Unavailable`，绝不在其他应用界面上点。
- 点击后必须还原鼠标原位置（`GetCursorPos` / `SetCursorPos`）。
- `Unavailable` 语义是「没能证明拿到焦点」，**不是**「证明没拿到」，处置权归 pipeline（见 DECISIONS D21）。
- 本 trait 只负责焦点，**不得**注入任何按键。粘贴组合键仍只在 pipeline 的注入步骤发出。

### 4. Validation & Error Matrix

| 条件 | 结果 |
|---|---|
| 当前焦点已是可写控件 | `AlreadyEditable`，不动鼠标 |
| UIA 找到可写 Edit/Document 且复核通过 | `FocusedByUia` |
| UIA `SetFocus` 后复核失败（千牛 CEF） | 继续降级到锚点点击 |
| 画像无 `input_anchor` 且 UIA 失败 | `Unavailable` |
| 锚点被其他窗口遮挡 | `Unavailable`（不点击） |
| 锚点点击后前台漂移 | `Unavailable` |

### 5. Tests Required

- pipeline 侧守卫（mock focuser）：聚焦步骤位于 activate 与 probe 之间；聚焦步骤**不注入按键**；
  `Unavailable` 在默认档仍注入但标 `verified: false`；`uia_strict` 档 `Unavailable` 中止注入；
  画像锚点原样转发、无锚点转发 `None`。
- 真实 IM 验证：**不手工点输入框**，双击素材后截图证明素材落进输入框且未发送。
