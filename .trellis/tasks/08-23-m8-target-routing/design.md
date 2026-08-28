# Design — M8 多 IM 目标路由

## 边界

```
crates/targets/                  # 新 crate：纯 Rust，零 IO / 零平台依赖 / 零 cfg 门
├── Cargo.toml                   # deps: serde, toml(仅 de), thiserror；禁 windows-sys / std::fs
├── src/
│   ├── lib.rs
│   ├── model.rs                 # TargetId / TargetScope / TargetBinding / Health / Readiness / NotReadyReason
│   ├── profile.rs               # Profile / ProfileSet / load_profiles(吃 &str 不吃路径)
│   ├── matcher.rs               # WindowSnapshot[] → MatchResult 打分
│   ├── tracker.rs               # TargetTracker 粘性状态机（无时间依赖）
│   └── health.rs                # L0–L3 体检编排
crates/platform/src/lib.rs       # + WindowSnapshot + 4 个 trait（仍零依赖）
crates/platform/src/win32.rs     # + 4 个 trait 的 Windows 实现
crates/pipeline/src/lib.rs       # PasteSession 改造：previous_foreground → target
crates/pipeline/src/negotiate.rs # negotiate 吃 &Profile
crates/pipeline/src/feedback.rs  # 新增：PasteFeedback 收敛层
crates/ui-viewmodels/src/target_bar_vm.rs  # 新增：目标条 VM
profiles/profiles.builtin.toml   # 内置目标册（随版本发布）
.trellis/config.yaml             # packages 登记 targets
```

依赖方向：`app-ui → ui-viewmodels → pipeline → targets → platform(trait)`。
`targets` 只依赖 `platform` 的 trait 与 `WindowSnapshot`，绝不反向。

## 关键设计决策

### 目标身份双层化

`TargetId(String)` 稳定，来自 profile 的 id；`TargetBinding{id, hwnd: Option<WindowHandle>, label, fallback: bool}` 可变。
托盘关闭只把 `hwnd` 置 `None` 并保留 `TargetId`，再开时由匹配器重新填 `Some(new_hwnd)`。
这条是 R1 的全部机制，其余组件都不许把 HWND 当身份。

### 粘性状态机为什么必须是纯函数

`TargetTracker` 完全不感知时间与数据来源。这带来两个好处：
粘性语义可以用 proptest 做数学证明（任意长度非 eligible 事件序列后热目标恒定）；
以及万一 WinEvent 钩子因内存问题被换成 1s 轮询，状态机代码一行不改。

决策全在纯函数侧，WinEvent 回调只负责投递 `WindowSnapshot`，不做任何判断。

### 就绪度三档而非两档

`Readiness{Ready, Unknown, NotReady(reason)}`。两档设计会逼迫「探不到」二选一，
要么误杀 Electron 壳（当成 NotReady 永远不注入），要么假装没事（当成 Ready 骗用户）。
第三档 `Unknown` 允许注入但把不确定性透传到 `PasteOutcome::Injected{verified: false}`，由 UI 诚实回显。

P0 探测纯 Win32：窗口存活 / 非最小化 / 无模态遮挡 / 标题不匹配未就绪模板。
P1 探测走 UIA，在独立 COM 线程执行，**超时 50ms 即返回 `Unknown`**，绝不返回 `NotReady`。

### 体检 L0–L3 与四色语义

L0 静态校验（profile 字段完整、正则可编译）→ L1 进程与窗口存在 → L2 激活可达 →
L3 自证（写哨兵文本 → 读回比对 → 清理，全程无 Enter）。

一次 L3 读回同时证明「格式协商 + 激活 + 落框」三环全通，比分别测三次更可信。
四色映射：绿只能来自 L3 通过；L1/L2 过但 L3 未跑或探测不可用一律黄；L1 失败红；未体检灰。
黄色不等于绿色，这个区分是为了不让用户以为「点是亮的就一定能成」。

### 反馈收敛

`PasteFeedback{severity, target_label, headline, hint, action, diagnostic}`。
`diagnostic` 是唯一容纳技术细节的字段，默认折叠。
`NotReadyReason` 是穷举枚举，`every_not_ready_reason_maps_to_nonempty_feedback` 用穷举匹配防止新增原因时漏写文案。

### auto-send 的结构性隔离

把 `chord_enter()` 的调用点从 `paste()` 内部整个移出，成为独立的 `pub fn send(&self, deps)`。
改动后 `paste()` 函数体内不存在任何 `chord_enter` 引用，「核心链路不碰回车」从纪律约束变成结构事实，
靠 `rg` 就能机械验证，不依赖后人自觉。

## 契约变更

| 位置 | 现状 | 改后 | 兼容性 |
| --- | --- | --- | --- |
| `negotiate` | `negotiate(req, TargetProfile::ImGeneric)` | `negotiate(req, &Profile)`，按 profile 声明的有序格式列表回落 | 破坏性；`TargetProfile::ImGeneric` 保留为 `Profile` 的默认构造，M6 测试用它保持绿 |
| `PasteSession` | `previous_foreground: Option<WindowHandle>` | `target: Option<TargetBinding>` | 破坏性；`begin_panel` 不再瞬时采样，改为从 `TargetTracker` 取 |
| `paste()` 编排 | 4 步 | 6 步：协商 → 写剪贴板 → activate+settle → 就绪度 → 注入前最后一次前台校验 → 注入 | 操作日志断言需扩展，不需重写 |
| `PasteOutcome::Injected` | 无字段 | `Injected{verified: bool}` | 破坏性但只影响调用方 match |
| auto-send | `paste()` 内末端追加 | 独立 `send()` | 行为等价，结构隔离 |

## 风险与回退

| 风险 | 触发信号 | 回退方案 |
| --- | --- | --- |
| 面板回切后 IM caret 不归位 | A6 实测发现粘贴落到窗口而非输入框 | 面板改 `WS_EX_NOACTIVATE` 无焦点浮层，彻底不抢前台；退一步用 profile 里的 `click_anchor` 坐标点一下输入框。**这是结构性改动，须在 T6 收口前有结论** |
| UIA 探不到 Electron 壳内部 | P1 探测恒返回 `ProbeUnavailable` | 该 profile 标 `readiness: p0_only`，健康点停黄，允许注入但 `verified: false` |
| WinEvent 钩子吃内存 | 常驻内存突破 100MB 红线 | 降级为 1s 轮询 `GetForegroundWindow`；`tracker.rs` 是纯函数，零改动 |
| `PrintWindow + PW_RENDERFULLCONTENT` 对 Electron 全黑或挂线程 | A6 实测 | 走 worker 线程 + 超时；拿不到预览图就不显示预览，不阻塞上框 |
| A5 拿不到某 IM（无法安装/无账号） | 实测受阻 | 不进 builtin，靠 `generic_im` 兜底 + 用户自定义捕获，文档写明未覆盖清单 |
| T4 状态机拖期 | D5 proptest 未绿 | T9 降级为只显示热目标、不做图钉与选择器，把余量还给 T6 |

## 操作性说明

新 crate 必须登记进 `.trellis/config.yaml` 的 `packages`，否则 `get_context.py --mode packages` 看不到它，
后续 spec 沉淀与质量检查会漏掉整个 crate。收尾时在 `.trellis/spec/targets/backend/` 建立该 crate 的踩坑沉淀。
