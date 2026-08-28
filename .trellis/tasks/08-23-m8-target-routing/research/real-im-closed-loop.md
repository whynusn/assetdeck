# Research: 真实 IM 上框闭环

- Query: 在真实微信 4.0 两个实例、千牛运行状态下，如何验证“素材进入输入框”且不发送？
- Scope: internal + mixed（本机桌面、Win32、UIA、产品管线）
- Date: 2026-08-23

## Findings

### 1. 产品路径可以打通

复用 `TargetRoutingRuntime::paste`，不要自己实现一套注入逻辑。

实测命令（微信目标需先确认 HWND，运行时依赖真实桌面会话）：

```powershell
cargo run -p real-im-verify -- `
  --quiet `
  --library samples/library `
  --profile wechat `
  --hwnd 197440 `
  --asset-index 0 `
  --asset-file verify-sentinel.txt `
  --wechat-open-file-transfer `
  --cleanup-input
```

结果：

```text
notice[success] 已上框到 微信 (4.0) · 微信 · 窗口 197440
readback:
text[谢振宇]: AM_VERIFY_20260823_千牛上框哨兵
READBACK_OK sentinel=AM_VERIFY_20260823_千牛上框哨兵
cleanup=Ctrl+A+Delete
```

这里没有注入 Enter。`Ctrl+A + Delete` 只清空未发送的草稿。

### 2. 微信 4.0 UIA 的关键事实

- 主窗口：`Weixin.exe / Qt51514QWindowIcon / 微信`。
- 未进入聊天输入框前，`ElementFromHandle(root).FindAll(Descendants)` 通常只暴露 2 个 Pane：
  `Weixin`、`MMUIRenderSubWindowHW`。
- 目标窗口获得聊天输入框焦点后，微信会物化 UIA 树；此时可看到：
  `mmui::ChatInputField`、`mmui::XButton`、会话列表、聊天消息列表。
- 聊天输入框的 `CurrentNativeWindowHandle()` 为 0，不能只按 native HWND 关联；要同时看
  `CurrentProcessId()`、目标窗口进程号和 UIA 树内焦点元素。
- `CurrentValue()`/TextPattern 可读出输入框内容，这是本次哨兵读回成功的原因。
- 微信 4.0 的相关元素类名、输入框焦点规则与旧版不同；不要把旧版 `WeChat.exe` / 普通 Edit
  定位作为唯一依赖。

### 3. 多实例与快捷键风险

本机有两个 `Weixin.exe`：

- `2163916 / PID 17940`
- `197440 / PID 7308`

`Ctrl+Alt+W` 在千牛也占用、且可能由另一个 WeChat 实例接收；**不能用它作为生产环境目标定位手段**。
它只适合在诊断环境中确认“当前 IM 聊天输入框”。生产目标定位必须依赖：

1. `TargetId + TargetBinding` 精确 HWND；
2. `instance_id`（当前用 `exe_name:process_id`）防止同 profile 不同实例串绑；
3. 只允许“UIA 证明当前焦点是可写输入框”时注入；
4. 用户从 picker 选择具体窗口。

隐藏的微信窗口应出现在选择列表中，并标明“隐藏中”；之前 `Win32WindowEnumerator` 直接过滤
`IsWindowVisible=false`，导致隐藏实例无法被用户选择。已改为保留有面积、未失效的隐藏顶层窗口，
由 `TargetBinding.visible` 驱动 UI 状态。

### 4. 千牛没有会话时不注入

千牛当前有两个顶层窗口：

- `656054 / 千牛工作台`
- `721614 / 接待中心`

它们内部是 Chromium webview：`Document` 名称可能是
`千牛商家工作台`、`消息中心`、`千牛消息聊天`，ValuePattern 只是 URL。当前机器没有可打开的会话，
因此 UIA 得不到“可写输入框焦点”。此时产品管线返回：

```text
已复制，未能上框到 千牛工作台 · ... · 无法确认输入框状态，请在目标应用中手动粘贴
```

这是安全行为，不把空文档当作“上框成功”。要得到千牛正向结果，需要先在千牛打开一个真实可输入会话，
或者后续用安全测试账号/客服入口显式打开后再验证。

### 5. 真实素材库

`samples/inbox` 来源为公开测试素材；已被 `tools/sample-library` 导入到 `samples/library`：

- PNG/JPG 图片；
- 2 个 MP4；
- `verify-sentinel.txt` 文本哨兵。

`ui_viewmodels::RealAssetResolver` 按 `meta.db` 的 `rel_path` 读取真实字节，不再是演示文本。

## Caveats / Not Found

- 尚未验证 PDD 商家版、Telegram 的真实聊天输入框。
- 尚未做无焦点浮层 `WS_EX_NOACTIVATE`，因此“管理器当前窗口是否抢走 IM 输入框焦点”仍要结合真实产品路径验证。
- 尚未实现生产全局热键；`real-im-verify` 只证明目标路由/剪贴板/注入/读回路径。
- 尚未实现自动发送；该方向明确是 P2，当前任务不发送。
- `WinEvent` 常驻内存、`PrintWindow`、UIPI 尚未做最终实测。

---

## 第三轮：根因定位与就绪策略翻转（2026-08-23 晚）

### 6. 上框失败的真正根因：HDROP 写了相对路径

此前现象是「PNG 与纯文本能上框，jpg / mp4 不能，且输入框毫无变化、没有任何错误」。
根因不在就绪度探测，而在剪贴板载荷本身：

- `RealAssetResolver::materialize_debug` 用 `self.root.join(&meta.rel_path)` 生成路径。
  `--library samples/library` 是相对路径，`rel_path` 又是 `/` 分隔的字符串，拼出来是
  `samples/library\objects/xxx/raw.jpg` 这种相对混合分隔路径。
- 该路径写进 `CF_HDROP` 后，由**接收方进程**按它自己的工作目录解析。微信/千牛解析不到文件，
  就丢弃整次粘贴动作，既不报错也不改动输入框 —— 一个完全静默的失败。
- PNG 图片走 `ClipboardPayload::Png` 的字节路径、文本走 `CF_UNICODETEXT`，都不经 HDROP，
  所以它们「碰巧」一直是好的。这解释了此前全部的观测差异。

两处修复：

1. `crates/ui-viewmodels/src/catalog_loader.rs`：按 `/` 拆分 `rel_path` 逐段 `push`，
   再 `std::path::absolute()`。
2. `crates/platform/src/win32.rs` 的 `hdrop_path_list`：对每条路径做 `std::path::absolute()`,
   仍为相对则返回 `PlatformError::Clipboard`。**宁可报错，也不静默失败。**

对照证据：修复前 `wx2-image.png` 是空输入框，修复后 `wx2-image-abs.png` 图片渲染在输入框内，
两张截图除该修复外无其他变量。

回归守卫（本轮新增）：

- `platform` 单测 `hdrop_promotes_relative_paths_to_absolute`、
  `hdrop_keeps_absolute_paths_and_terminates_list`、`hdrop_rejects_empty_path_list`。
- `ui-viewmodels` 集成测试 `tests/asset_payload_spec.rs`：在**相对** root 下建库，断言
  物化出的 `source_path` 绝对且真实存在，视频路径不内联字节。

### 7. caret / UIA 焦点探测被实验否证，就绪策略随之翻转

在能够成功粘贴的微信与千牛窗口上，caret 探测与 UIA 焦点元素查询同样返回空值：
微信是 Qt 自绘，千牛是 CEF，两者都无法在注入前证明「存在可写输入框」。

因此 `profiles.builtin.toml` 的四个内置画像从 `uia_strict` 改为 `uia_shallow`，判定语义翻转为：

- **否证阻塞才不注入**（`ReadinessSignal::Blocked`：未登录 / 无会话 / 只读 / 模态 / 窗口消失）。
- 探测不到（`Inconclusive`）则照常注入，结果标 `verified: false`，UI 出 `warning` 文案
  「已粘贴到 X，请确认输入框内容」。

`uia_strict` 保留为用户可显式开启的严格档：它对 `Inconclusive` 仍然只复制不注入。这条选择
是有代价的 —— 严格档在微信/千牛上等于永不注入，所以不能做内置默认。

### 8. 千牛画像显式化

带输入框的会话窗口是「接待中心」（标题形如 `tb940472610424-接待中心`），不是「千牛工作台」。
`title_regexes` 已显式声明 `["接待中心$", "千牛工作台", "千牛登录"]`，不再依赖
`-千牛工作台` 的巧合匹配。上一轮判断「千牛没有会话」其实是**看错了窗口**。

### 9. 真实闭环结果矩阵（截图存于 `C:\Users\Administrator\Documents\Default_Project_probe\`）

| 目标 | 文本 | 图片 | 视频 |
|---|---|---|---|
| 千牛 721614 接待中心 | 通过 `qn-round3-text.png` | 通过 `qn-round3-image.png` / `qn-jpg-hdrop.png` | 通过 `qn-video-abs.png` |
| 微信 2163916 文件传输助手 | 通过 `wx2-text.png` | 通过 `wx2-image-abs.png` | 通过 `wx2-video.png` |

全程无 Enter；每次验证后 `Ctrl+A + Delete` 清空草稿。千牛的 UIA 读不回输入框内容属预期
（CEF 只暴露 URL），由截图补足视觉证据。

可复现命令：

```powershell
cargo run -q -p real-im-verify -- --library samples/library --profile qianniu `
  --hwnd 721614 --asset-index 0 --asset-file dog.jpg --quiet
pwsh -NoProfile -File scripts/desktop-probe.ps1 -Action shot -Hwnd 721614 -Out <png>
```

### 10. PDD / Telegram 仍不可测

PDD `656860` 停在「能力认证」培训页，没有会话窗口（`pdd-round3.png`）；窗口清单里的
`jinbaochatwnd|多多进宝聊天`（`722420`）不可见。Telegram 未运行。记为「缺真实会话，本轮跳过」。

### 11. 自动送焦点后的复验（2026-08-24，前缀 `r14-` / `r15-`）

此前所有「通过」都建立在一个隐含前提上：验证前人手点过一次 IM 输入框，键盘焦点已经在里面。
`Win32InputFocuser`（D21）把这一步补上后重跑，结论如下。

| 路径 | 目标 | 结果 | 取证 |
|---|---|---|---|
| 产品进程双击瓦片（热目标 chip） | 微信 2163916 | 落框，未发送，未手工点输入框 | `r14-prod-wechat-1.png` |
| 产品进程 picker 选冷目标后双击 | 千牛 721614 | 落框，未发送，「发送」按钮未触发 | `r14-prod-qianniu-1.png` |
| `real-im-verify` 复跑 | 千牛 721614 | `notice[success] 已上框到 千牛 · tb940472610424-接待中心`，dog.jpg 停在待发区 | `r15-qianniu-after.png` |
| `real-im-verify` 复跑 | 微信 2163916 | `notice[warning] 已粘贴到 微信 (4.0) · 微信 · 窗口 2163916，请确认输入框内容`，大图停在输入区 | `r15-wechat-after.png` |
| mp4（`raw.mp4`，1022.5K） | 微信 2163916 | 文件卡片停在输入框，未发送 | `r15-wechat-mp4.png` |
| mp4（`raw.mp4`） | 千牛 721614 | 按 D18 只复制：`notice[warning] 已复制，未能上框到 千牛 …… 该应用会把粘贴的文件直接发出而不进输入框；素材已复制，确认要发送时再手动 Ctrl+V` | 无需截图（未注入） |

两侧收尾都跑了 `--inspect-only --cleanup-input`（`cleanup=Ctrl+A+Delete`），输入框清空。
微信侧提示是 warning 而非 success，因为 UIA 只有 2 个节点、读不回文本，`verified:false`；
这是 D15 既有结论，以截图为准。

千牛的 `paste_sends=["files"]` 意味着 mp4 走 CF_HDROP 会被千牛当场发出去，所以协商阶段
直接跳过该格式、停在「只复制 + 提示手动粘贴」（D18）。这是刻意保留的边界，不是缺陷：
宁可少一次自动上框，也不能替用户发消息。
