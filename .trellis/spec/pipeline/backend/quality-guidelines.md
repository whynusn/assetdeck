# Quality Guidelines — pipeline

## Scenario: 精确目标上框编排

### 1. Scope / Trigger

- 修改 targeted 粘贴顺序、目标激活、readiness、反馈、格式协商或自动发送边界时适用。

### 2. Signatures

```rust
pub fn paste_targeted(
    &mut self,
    req: &AssetPayload<'_>,
    profile: &Profile,
    deps: &mut TargetPipelineDeps<'_>,
) -> TargetPasteOutcome;

pub fn send(&self, injector: &mut dyn KeyInjector) -> platform::Result<()>;
```

### 3. Contracts

- 顺序是 `write -> activate(exact hwnd) -> focus_input -> probe -> alive/foreground -> Ctrl+V`。
  `focus_input` 是独立步骤：`SetForegroundWindow` 只把窗口提前台，键盘焦点仍停在窗口根控件，
  少了这一步 Ctrl+V 会落空（DECISIONS D21）。
- 选择、激活、probe 和最终复核必须使用同一个具体 HWND。
- `Blocked` 不注入；`Inconclusive` 注入后标 `verified=false`。
- `FocusOutcome::AlreadyEditable` / `FocusedByUia` 视为「焦点已证实」，可把 `ReadinessSignal::Inconclusive`
  升格为 `verified = true`；`FocusedByAnchor` 与 `Unavailable` 不升格。
- `FocusOutcome::Unavailable` 在默认档**仍然注入**并标 `verified=false`（与 D15 同一权衡）；
  只有 `ReadinessMode::UiaStrict` 才据此按「已复制、不注入」中止。
- 协商出的格式命中画像 `paste_sends` 且用户未显式开启自动发送时，降级为「只复制 + 提示」，
  绝不用「发送」冒充「上框」（DECISIONS D18；千牛 `paste_sends = ["files"]`）。
- profile 为 `ReadinessMode::UiaStrict` 时，`Inconclusive` 也按“已复制、不注入”处理；只有 `Readiness::Ready` 才进入注入。该档位是**用户可显式开启的严格档，不是内置默认**：真实实测证明微信（Qt 自绘）与千牛（CEF）在可粘贴窗口上也返回 `Inconclusive`，内置画像一律 `uia_shallow`，判定语义是「否证阻塞才不注入」（见 DECISIONS D15）。
- targeted 核心路径不调用 `send()`，注入序列不得含 `0x0D`。
- profile 格式列表按声明顺序尝试，载荷缺失时继续下一格式。
- 走 `ClipboardPayload::Files` 的格式行（视频恒定、图片回落）依赖平台层保证路径绝对；相对路径会让 IM 静默丢弃粘贴（DECISIONS D14）。本层不得把「剪贴板写入成功」当作「素材已进输入框」。

### 4. Validation & Error Matrix

| 条件 | 结果 |
|---|---|
| 无可用格式或剪贴板写失败 | `Failed{feedback}` |
| 无目标/HWND 休眠/激活失败 | 已复制，`CopiedOnly` |
| readiness 明确阻塞 | 已复制，`CopiedOnly`，零注入 |
| readiness 不可判定 | 注入，`verified=false` |
| 焦点已证实 + readiness 不可判定 | 注入，`verified=true` |
| 焦点 `Unavailable`（默认档） | 注入，`verified=false` |
| 焦点 `Unavailable` + `UiaStrict` | 已复制，`CopiedOnly`，零注入 |
| 格式命中 `paste_sends` 且未开自动发送 | 已复制，`CopiedOnly` + 提示，零注入 |
| `UiaStrict` readiness 不可判定 | 已复制，`CopiedOnly`，零注入 |
| 注入前 HWND 失活或前台漂移 | 已复制，`CopiedOnly`，零注入 |
| Ctrl+V 注入失败 | 已复制，`CopiedOnly` |

### 5. Good/Base/Bad Cases

- Good：用户选择 `telegram@202`，全链路只激活和校验 202。
- Base：readiness 不可判定，仍上框但 UI 明确提示用户确认。
- Bad：已写剪贴板后发现前台变成微信，却继续把 Telegram 素材注入微信。

### 6. Tests Required

- `targeted_pipeline_order_is_write_activate_probe_validate_inject`（期望序列含 `Focus`）。
- `focus_step_runs_between_activate_and_probe`。
- `focus_step_never_injects_keys_before_paste_chord`。
- `focus_unavailable_still_injects_but_marks_unverified`。
- `confirmed_focus_upgrades_inconclusive_probe_to_verified`。
- `uia_strict_aborts_when_focus_unavailable`。
- `profile_anchor_is_forwarded_to_focuser_verbatim`、`profile_without_anchor_forwards_none`。
- `not_ready_no_conversation_never_injects`。
- `unknown_readiness_injects_but_marks_unverified`。
- `uia_strict_inconclusive_copies_without_injecting`。
- `foreground_drift_before_inject_aborts`。
- `core_upload_path_never_synthesizes_enter`。
- 反馈穷举、目标名回显、所有降级先说已复制。

> `probe_timeout_falls_back_to_unknown_not_notready` 当前只以 Mock `Inconclusive` 证明上层映射；真实 UIA 超时实现落地前不得写成端到端已验证。

### 7. Wrong vs Correct

#### Wrong

```rust
deps.injector.inject(&chord_paste())?;
self.send(deps.injector)?;
```

#### Correct

```rust
if deps.focus.is_alive(hwnd) && deps.focus.foreground() == hwnd {
    deps.injector.inject(&chord_paste())?;
}
```

## Mechanical Guards

```powershell
rg '0x0D|VK_RETURN|chord_enter' crates/pipeline/src/lib.rs
cargo test -p pipeline
```
