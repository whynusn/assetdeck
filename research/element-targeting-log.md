# D75 元素级定位实验记录（2026-09-04，解锁后真机）

> 本文是 D75「去坐标化定位」的完整实验台账：逐条列出尝试过的方案、
> 客观观察与当前结论。调研背景与决策语境见 `DECISIONS.md` D75 段。
> 探针代码：`tools/real-im-verify/src/bin/focus_probe.rs`（commit 67fdd2a / cd54bcb，
> 外加当日未提交的 `--tabs` / `--focus-doc` 实验参数）。

## 实验对象

| 目标 | 架构 | 关键观察面 |
|---|---|---|
| 千牛商家工作台 | Qt 5.15 外壳（`Qt5152QWindowIcon`）+ 2×CEF webview：`千牛商家工作台`、`千牛消息聊天`（各配一个 `Chrome_RenderWidgetHostHWND`） | CEF 渐进式无障碍 |
| 微信 4.1.13 | Qt 5.15.14 mmui（`MMUIRenderSubWindowHW`） | 自绘 Qt，无 CEF |

环境：物理分辨率 2560×1600 @150%（探针 DPI-unaware，读虚拟化 1707×1067）。
测试期间用户偶尔回 ZCode 窗口（真机共用），构成两次前台劫持事故（见 §四）。

---

## 一、CEF 目标（千牛）：无障碍树物化

### A1. WM_GETOBJECT(OBJID_CLIENT) 消息探针
- **做法**：向 `Chrome_RenderWidgetHostHWND` 发 `WM_GETOBJECT`（lparam=OBJID_CLIENT）。
- **观察**：只触发 Chromium 渐进式无障碍最低档（kNativeAPIs）；UIA 后代仅 8~11 个浏览器级表面元素，无任何 Edit。
- **结论**：单独发消息**不能**让 CEF 建 web DOM 树。D74 时期「结构性不可用」的判断即止步于此档——升级钩子在别处。

### A2. a11y 激活协议（真实 COM 属性调用 + 蜜罐）✅ 有效
- **做法**：RWH 发现 → 对拿到的 IAccessible 做真实跨进程 COM 调用 `get_accName` → `get_accDefaultAction`（+`get_accRole`）→ 蜜罐 `WM_GETOBJECT(objid=1)`（必须先 Name 后蜜罐）。
- **观察**：AXMode 升到 kAXModeBasic；UIA 后代 8 → 97/99；工单面板的 web Edit 物化（`买家账号` aid=`buyer` readonly=false）。**可复现**。
- **限制**：同一序列在拼多多 webview 未生效（分目标差异）；树在窗口失前台后 ~10 秒塌缩回 8 元素，须即用即触发。

### A3. 激活后全树 dump（--activate-dump，99 元素逐项检查）
- **观察**：`千牛消息聊天` Document 下只物化了消息历史（8 组视频播放器/进度条/时间戳）；**聊天输入框（composer）任何形态都不在树里**——没有 Edit、没有带 value 的节点。工单面板的 `买家账号` Edit 在树中且 ValuePattern 可读。
- **结论**：CEF kAXModeBasic 只物化「焦点子树 + 媒体类默认节点」。composer 从未获焦 → 不进树。

---

## 二、CEF 目标（千牛）：设焦路径（目标：让聊天输入框获得焦点）

背景：composer（contenteditable）不在 a11y 树中，需先让它获焦才会物化。以下按顺序穷尽。

### B1. UIA SetFocus → 已物化可写 Edit（买家账号）✅ 链路通，但目标错位
- **做法**：枚举 Edit/Document 候选 → 按属性过滤（readonly=false + 名称提示 + 面积兜底）→ `element.SetFocus()` → SendInput Ctrl+V → 重枚举 ValuePattern 读回。
- **观察**：marker 三次全部落进 `买家账号` 搜索框（读回 value 命中），**PASTE_ELEMENT: SUCCESS ×3**（D75MARKER2026 / -D 后缀 / D75-DEMO-20260904）。
- **结论**：激活 → 物化 → 属性选框 → 设焦 → 粘贴 → 读回验证，全链路零坐标成立。但可物化的可写 Edit 只有买家账号（工单面板），**聊天输入框不在其中**。

### B2. 焦点自恢复（--no-setfocus）✅
- **做法**：激活窗口后不设焦，直接 Ctrl+V。
- **观察**：marker 落进**最后聚焦的元素**（该次为买家账号）。
- **结论**：Qt/CEF web 内部焦点跨失活存续（Blink document focused element）；「焦点在哪，粘贴落哪」。这是真实上框场景的机理基础（见 §五）。

### B3. win32 SetFocus(RWH hwnd)（AttachThreadInput）❌ composer 不接
- **做法**：AttachThreadInput + `SetFocus(消息聊天 RWH)`。
- **观察**：焦点确实离开买家账号、进入消息面板内部（UIA focus 报 50026 custom 容器，无名无类）；Ctrl+V 后 marker MISS（不在任何 Edit value 中）。
- **结论**：HWND 级设焦只把焦点给到 document 根/容器，Blink DOM 焦点**不会**自动落到 composer。会话未开时 composer 不存在（此前实验③）；**会话开着时同样不接**（本轮复验）。

### B4. Tab 键盘导航 ×25（--tabs）❌ 被 Qt 焦点链吞掉
- **做法**：RWH 设焦后 SendInput 合成 Tab（scancode 0x0F，非 0x0D）25 次，每按一次枚举一次树。
- **观察**：edit_candidates 恒 3、UIA has_focus 恒不变——Tab 在 Qt 原生焦点链里循环，从未进入 web 内容；composer 未物化。
- **结论**：键盘导航在「Qt 外壳 + CEF 内嵌」结构中不可达 web。

### B5. UIA SetFocus → 消息聊天 Document 本体（--focus-doc，accSelect TAKEFOCUS）❌ 同 B3
- **做法**：对 Document 元素本体走 UIA SetFocus（与 B3 的 win32 SetFocus 是不同通道：UIA↔MSAA 桥 accSelect）。
- **观察**：err=None，焦点到 50026 容器；Ctrl+V MISS。
- **结论**：与 B3 同病——document 容器获焦 ≠ composer 获焦，Blink 不做「焦点给容器就顺延到输入框」的顺延。

### B6. CDP（Chrome DevTools Protocol）❌ 未开放
- **观察**：千牛进程无任何监听端口（netstat 零命中）。
- **结论**：release 版未开 remote-debugging-port，无法绕过 a11y 直达 DOM。

### B7. 会话列表条目元素级导航 ❌ 条目不在树里
- **做法**：invoke_by_name（InvokePattern → LegacyIAccessible.DoDefaultAction → SelectionItemPattern.Select）找会话条目「[商]棉生家品」。
- **观察**：会话列表（左侧「正在接待」列表）不暴露为 UIA 元素（既非已物化 web，也非枚举出的 Qt 原生控件），无法元素级触发。
- **结论**：打开会话这一步（composer 存在的前提）暂时只能靠真实用户点击或坐标点击。

**B 组死结总结（含本轮收尾实验修正）**：
- composer 从未聚焦时：不进 a11y 树 → UIA 不可达 → 无法零坐标设焦（B3~B6 全灭）。
- **composer 被点击获焦后依然不进树**（决定性事实）：真实鼠标点击输入框获焦并成功粘贴后（marker 已见落框、光标在框内），立即 dump 全树仍只有 99 元素、3 个 Edit 候选——消息聊天 webview（recent.html）**无论焦点状态如何都不为 composer 暴露 UIA 节点**。对照：工单 webview 的表单控件（买家账号输入框、下拉框）从未聚焦过也在树中。
- 结论：这是**目标页面自身行为**（DOM/shadow DOM/aria 配置差异），不是 CEF 物化档位问题。对千牛聊天 composer，「UIA 元素级选中/设焦/ValuePattern 读回」三条路**结构性不可用**，无解。

### B8. 焦点自恢复 + 直接粘贴 → 聊天输入框 ✅ 演示成功
- **做法**：真实鼠标点击输入框（获焦）→ 不激活、不设焦、不枚举，直接合成 Ctrl+V。
- **观察**：marker `D75-SELF-PASTE` 落进聊天输入框（截图证实，光标在框内末尾）。
- **结论**：**焦点在 composer 时，粘贴直接落框**——这是真实上框场景的完整形态。composer 的 a11y 不可见性不影响此链路（焦点自恢复 ≠ a11y 可见）。

---

## 三、微信 4.1.13（Qt mmui）：自动通道全灭

| # | 方案 | 观察 | 结论 |
|---|---|---|---|
| C1 | SPI_SETSCREENREADER 置位 | 写入后被系统/应用**静默回退**（读回 false） | 此路不通 |
| C2 | 顶层/子窗 WM_GETOBJECT | 返回 0x0，无 IAccessible provider | mmui 未注册 MSAA |
| C3 | UIA GetFocusedElement | 无有效焦点元素返回 | 焦点查询不可用 |
| C4 | APPCOMMAND_PASTE（PostMessage） | 无任何效果 | 不响应媒体/应用命令 |
| C5 | 合成键盘 SendInput（Ctrl+V 等） | 4/4 事件送达（键盘钩子视角）但被应用忽略 | 输入事件被 mmui 过滤或焦点未在其预期窗口 |
| C6 | UIA 全树枚举 | 默认仅 2 个元素（死树） | 与千牛不同，无渐进式激活可用 |

**根因二分未完成**：待区分「A：mmui 过滤合成输入」vs「B：焦点从未进入 composer」——判别实验需要**用户手动点击一次微信输入框后**立即合成 Ctrl+V（若落框则是 B 焦点问题，若不落则是 A 过滤问题）。测试期间用户正在真机接待顾客，为不干扰营单暂停（D74 遗留：微信 composer 锚点也未量取）。

---

## 四、实验环境事实（影响取证解读）

1. **前台锁**：后台进程 `SetForegroundWindow` 被系统拒绝 → 绝对坐标点击会落到当时的前台窗口（本次落到了 ZCode 会话界面，未造成副作用）。`AttachThreadInput(当前线程, 前台线程)` 技巧可绕过，置前后必须 `GetForegroundWindow` 复核再动手。
2. **真实鼠标覆盖合成鼠标**：用户握持物理鼠标时，SendInput 鼠标移动被真实鼠标流冲掉；Rust 探针点击可生效（settle=Observed）。
3. **DPI 双空间**：物理 2560×1600 vs DPI-unaware 虚拟化 1707×1067（1.5×）。截图、UIA rect、SetWindowPos 各自所在空间不同，任何坐标比对前先确认空间。
4. **树塌缩**：CEF 无障碍树在窗口失前台 ~10 秒内塌回表面档；「物化」不是持久状态。
5. **千牛最小宽约束**：SetWindowPos(1300) 不生效（实测宽 1845 物理 px），布局量取时注意。
6. **shell 坑**（复发确认）：`findstr` 管道会吞行且 cargo 退出码经管道不可靠；PowerShell 内联复杂脚本/here-string 在 cmd 里易碎，一律落盘 `-File` 执行。
7. **截图读图缩放坑**（本轮最贵的教训）：2560×1600 全屏 PNG 在读图工具里被缩放显示（→2000 宽），直接在显示图上量的坐标要 ×1.28 才是物理坐标；不换算就点击会全部点偏。对策：坐标量取一律用「已知物理坐标的裁剪放大图」反推，禁用全屏缩放图直读。

---

## 五、结论矩阵

| 断言 | 状态 | 证据 |
|---|---|---|
| CEF a11y 激活协议（Name→DefaultAction→蜜罐） | ✅ 已证实可复现 | UIA 后代 8→97/99，web Edit 物化 |
| 元素级设焦 + Ctrl+V + ValuePattern 读回（已物化 Edit） | ✅ 全链路零坐标 SUCCESS ×3 | marker 均落 `买家账号` 并读回命中 |
| 焦点自恢复（激活后不设焦直接粘贴） | ✅ 落进最后聚焦元素 | 实验②；Blink 焦点跨失活存续 |
| composer 冷启动零坐标聚焦 | ❌ 死结（B3~B6 全灭） | CEF 只物化焦点子树；composer 不进树则 UIA 不可达 |
| **composer 获焦后 a11y 可见** | ❌ **不可见（决定性事实）** | 点击获焦+粘贴成功后 dump 仍无 composer 节点（99 元素恒定） |
| 焦点在 composer 时直接粘贴 | ✅ **落框成功（演示完成）** | marker 落输入框，截图证实 |
| 会话列表条目元素级触发 | ❌ 条目不在任何树中 | dump 无此类元素 |
| 微信 mmui 自动激活/注入 | ❌ 全通道死（C1~C6） | 待人工协作二分根因 |

**产品推论（供 D75 评审，本轮收尾后定稿）**：真实「上框」时刻 = 用户正在聊天 = 光标必然已在聊天输入框。该状态无法从外部零坐标制造（B 系列死结），但**天然存在于每一次真实操作**，且 B2 已证明粘贴会落进该焦点——B8 更在千牛 composer 上完整复现（点击获焦 → 直接粘贴 → 落框）。因此产品链路「激活 → 焦点自恢复 → Ctrl+V」在真实场景成立。元素级定位对千牛的价值边界最终收窄：
1. **主路径**不依赖元素级定位——焦点自恢复已覆盖真实场景，B8 即产品时刻的完整复现；
2. **verified 读回对聊天 composer 不可用**（composer 永不在 UIA 树，ValuePattern 读不到）——千牛的 verified 升级只能依赖 settle=Observed（焦点/插入符事件）或视觉证据；ValuePattern 读回仅对工单面板类表单 Edit 有效；
3. **锚点点击降级路径**继续保留（D74 bottom-up 模型），覆盖焦点不在输入框的边缘情况。

---

## 六、2026-09-05 只读复核与历史纠偏

本节覆盖旧结论的证据口径，不抹除历史。此次仅开发探针与文档修改，未变更画像或产品代码，未提交、重启应用、启用 CDP、发送消息或操作剪贴板。

### 历史纠偏

- 525668 是历史 HWND，不是输入框元素。旧 paste 分支先枚举候选、后 SetFocus；但历史独立 dump 与手工点击的严格时序没有足够日志证明。
- `--activate-dump` 不直接 SetFocus，却激活窗口；`--no-setfocus` 在它的分支被忽略。该命令不能作为无干扰原始快照。
- B1 SUCCESS ×3 全是买家账号工单 Edit 的命中，不是 composer 成功。当前开发工具在 UIA 初始化前禁用整个 `--paste-element`，包括 no-setfocus/tabs/nav 各变体；空 marker 拒绝。没有假装已经实现安全 composer targeting。
- 当前 qianniu `focus_strategy=[already,anchor]` 已无 uia 步；already 仅验证 pid 和可写 Edit/Document，不验证 composer 或会话身份。
- B8 没有失活/再激活，不能证明面板抢焦后的焦点恢复。用户操作聊天不意味着输入框必持焦。
- verified 是注入前信号，不是注入后的内容证明；焦点/caret 事件本身也不证明 composer 身份。旧“永久不可见/结构性无解/每次真实操作天然有焦点”的绝对断言撤回。

### 实现与边界

`tools/real-im-verify/src/focus_probe_raw.rs` 隔离只读路径，`--raw-snapshot HWND`（也接管默认 `--hwnd HWND`）仅接受 live AliWorkbench.exe。RawView walker 不按角色过滤，打印 root/child 索引路径（仅当前快照身份，非跨快照稳定 ID）、rect/pid/native HWND/framework/provider/name/aid/class/焦点属性及 Value/Text/Legacy 只读模式。每个 snapshot 前后记录 foreground、目标与前台 GTI、UIA focus。查询 provider 本身可能触发延迟 a11y 初始化，因此“只读”指不主动更改前台、焦点、输入、剪贴板，不保证 provider 内部完全无状态变化。

预算：512 节点、24 层、12 秒协作截止；字符串输出限 256 UTF-16 units，Text.GetText 显式有限长度。Value/Legacy API 本身只能返回完整 BSTR，不能限制 provider 返回前的分配；COM 单调用可能超过截止，运行工具时应另设进程执行超时。树 walker 的空接口与调用失败均显式记 end_or_error（windows-rs 可将 null 接口报告为 HRESULT 0）；不把截断/错误当作零节点。Text 恰好达 cap 时只能说可能截断。父路径为 RawView 遍历生成；未实现额外向桌面根反向追溯 focused element 父链。

`--composer-region l,t,r,b` 是已观察区域的屏幕坐标交集诊断，绝不据面积选焦点或证明身份；必须先确认与 UIA 矩形同一 DPI 空间，本轮无安全实测区域，未传该选项。

显式 `--qt-native-access` 对顶层取 OBJID_CLIENT 并读 Name/Role/ChildCount；`--cef-access` 复用现有 CEF 协议。调用前订阅现有 WinEvent 进程活动源，有限事件等待无 sleep/polling。但该源仅 focus/foreground/location，不含 UIA StructureChanged；renderer 跨 pid 事件可能漏掉，timeout 不代表树没建。旧 CEF 协议仍有同步 COM 与 SendMessageW 调用，无硬取消；本轮未执行显式触发选项。旧变更桌面的其他电池需额外 `--legacy-mutations`，不纳入本轮安全推荐入口。旧实现保留历史代码，不代表可安全针对 composer 使用。

### 实际 live 结果

只读枚举发现当前接待中心 HWND=8259616、pid=16672；历史 525668 未复用。执行 `target\\debug\\focus_probe.exe --raw-snapshot 8259616`：164 节点，1255ms，无 node/depth/time TRUNCATED 行；仍标 complete_not_guaranteed，因为 provider 空节点/错误边界不能证明全局完整。

前后台快照 foreground 均为 `0x10298`，目标 `0x7e0820`；目标 GTI active/focus/caret 均 0。UIA focus 报 ZCode Chrome provider（pid=8860），不是千牛。接待中心树含 Win32/MSAA proxy 外壳、CEF recent.html 消息历史/媒体节点；本次未识别可靠 composer。后台仍可见 164 节点说明旧“失前台必在10秒塌到8个节点”不是普适规律，视图差异/既存 provider 状态仍是混杂变量。

原始日志（包含潜在私密界面文本，不复制入仓库）为 `C:\\Users\\Administrator\\.zcode\\cli\\exec\\sess_subagent_agent_90c2b973-b8c9-4bf4-9d3c-7cebd07ff629\\call_bKtN16M8vjg0TVusnga2OGDw-stdout.log`。未做桌面 GUI 操作；可用技能清单无 computer-use，仅 browser-use，不能安全确认当前 composer 空白或接待状态，故未点击/切前台/粘贴/清草稿。--list 被动显示了其他 IM 窗口元数据，但未对微信/PDD 做树探测或实验。

### 公开来源复核（2026-09-05 获取；不是本机兼容性证明）

| 来源 | 可引用事实 | 不可推出的结论 |
|---|---|---|
| https://doc.qt.io/qt-5/accessible.html | Qt Windows accessibility 与 QAccessibleInterface/QAccessibleWidget；自定义控件需暴露语义；Text/Value/Action 接口分工 | Qt 壳存在不等于自绘 composer 已实现这些接口 |
| https://github.com/nvaccess/nvda/blob/master/source/NVDAObjects/IAccessible/chromium.py | NVDA Chromium 适配基于 ia2Web、IAccessible、IA2 unique id/attributes 与虚拟缓冲 | NVDA 对 Chromium 支持不证明千牛 composer 可读 |
| https://github.com/nvaccess/nvda/blob/master/source/IAccessibleHandler/__init__.py | normalizeIAccessible 经 IServiceProvider.QueryService 尝试升级 IAccessible2；失败保留原接口 | IA2 是值得独立只读探测的另一层，不是 UIA 没节点就自动可用 |
| https://pywinauto.readthedocs.io/en/latest/getting_started.html | win32/UIA 两后端，Inspect 与 Spy++ 暴露面不同；Chrome force-renderer-accessibility 建议 | 改用 pywinauto 不会创造 provider 未暴露的节点；启动参数需另行授权 |
| https://github.com/FlaUI/FlaUI/wiki/FAQ | Chrome accessibility 开关/启动参数建议，页面更新时间2022-10-26 | 无千牛/Qt 实测保证，不能当现版本证据 |
| https://cef-builds.spotifycdn.com/docs/131.3/classCefBrowserHost.html | SetAccessibilityState；windowed native objects 与 windowless TreeOnly 不同；embedder 控制状态 | 不能用 CEF 131 API 文档推定本机嵌入版本或替进程调用该 API |
| https://github.com/Jamesits/qianliyun-desktop | 确有千牛 accessibility 自动化历史项目；2020-02-15 归档，README 明说不再工作 | 是历史存在性证据，不是当前可复用方案；页面版权 All rights reserved，未复制代码 |
| https://github.com/cs-lazy-tools/ChatGPT-On-CS | README 宣称支持千牛智能客服 | 已获取页面未揭示定位实现，不能推测是 UIA/IA2/CDP/坐标哪条路线 |
| https://api.github.com/search/repositories?q=qianniu+automation | 返回上述两个仓库及 zpoint/vibe-seller，可复核真实仓库名 | 搜索描述/营销能力不是端到端验证 |

ISimpleDOM 尚未完成源码级核查：尝试 NVDA source/IAccessibleHandler/ISimpleDOM.py 返回404；comInterfaces 目录仅列初始化与说明文件，不能以没找到路径断言 NVDA 没用 ISimpleDOM。未实现 IA2/ISimpleDOM 原生 COM 查询，本轮不能排除这两层能提供补充证据。CEF Bitbucket GeneralUsage URL 本次404，以版本化 CefBrowserHost 文档补充；一次错误候选仓库 URL ChandlerVer5/qianniu 也404，不作为来源。

### 测试记录

- `cargo test --manifest-path "C:\\Users\\Administrator\\Documents\\Default Project\\Cargo.toml" -p real-im-verify --bin focus_probe`：4/4 通过（空 marker、任意 marker 禁用、矩形边界、文本输出 cap）。初次编译发现 BSTR 无 as_wide，改为 UTF-16 slice 后通过。
- `cargo build --manifest-path "C:\\Users\\Administrator\\Documents\\Default Project\\Cargo.toml" -p real-im-verify --bin focus_probe`：通过。
- rustfmt 对两个开发探针文件执行完成。
- 最终 `cargo check --manifest-path "C:\\Users\\Administrator\\Documents\\Default Project\\Cargo.toml" -p real-im-verify --bin focus_probe`：通过。
- 最终 `cargo clippy --manifest-path "C:\\Users\\Administrator\\Documents\\Default Project\\Cargo.toml" -p real-im-verify --bin focus_probe -- -D warnings`：通过，无警告。
- 最终 `cargo test --manifest-path "C:\\Users\\Administrator\\Documents\\Default Project\\Cargo.toml" -p real-im-verify`：整个工具包通过（focus_probe 4 测，主程序0测）；未运行全 workspace 回归，产品代码无改动。
- 自查：标准面未新增依赖/产品常驻缓存；规范面仍有明确欠项（硬 COM 截止、UIA StructureChanged 订阅、IA2/ISimpleDOM 原生探测、focus 父链反查、安全现场 composer E2E）。不能把已编译的辅助显式触发参数写成已实测。

结论：安全只读取证入口已可用，旧假成功入口已封堵；尚未得到聊天 composer 的可靠元素身份，也未验证跨失活 E2E。没有改产品，也不声称100%或结构性不可能。下一步是只读 IA2/ISimpleDOM 与 Qt provider 深层对照；任何需要重启/开启 CDP/注入的路径仍需独立授权与安全空 composer 现场。

### 续查收尾（2026-09-05；覆盖上文“尚未执行”的阶段性记录）

**主线程 GUI 证据（本续查代理未操作 GUI）**：主线程在自我会话截图确认 composer 空白，点击后键入 `QNPROBE905`，截图确认标记在聊天输入框，保持焦点执行 `--raw-snapshot 8259616`。255 节点/1638ms；前后 foreground 均为目标 `0x7e0820`，GTI active/focus/caret 同为顶层 HWND，UIA focus 仍为 Window(50032)。日志输出的有界 Value/Text/Legacy 文本没有该 marker；这只是本次读取未命中，不代表任意节点/完整文本/其他接口绝对不存在。主线程随后仅以10次 Backspace 删除本轮10字符并截图确认空白；未 Enter、未操作剪贴板。此实验证明肉眼可见 composer 内容与当前有界读取不同步，仍没有跨失活恢复或元素级粘贴 E2E。原始日志（私密，不入库）：`C:\Users\Administrator\.zcode\cli\exec\sess_2cc0e37c-d0d1-4162-bd06-d8988d66bd8c\call_8H25caFrkr2Pa1gZezborBLR-stdout.log`。

**第一次显式激活续查**：增加并实跑 PMv2、Qt OBJID_CLIENT、CEF 协议及 `--extended-interfaces`。基线255节点/1779ms，触发后255节点/1766ms，前台全程目标；Qt 顶层 role=10、children=2，事件等待 CappedOut（仅现有 focus/foreground/location 订阅，不是 StructureChanged）。IA2 QueryService 在4个 Chrome HWND 成功，包括两个 RWH `0x108a8`、`0x1088e`；当时仅测试接口可取得，未调用 IA2 专属方法。ISimpleDOMNode 在 RWH 返回 `0x80040155`（REGDB_E_IIDNOTREG），不是 E_NOINTERFACE。PMv2 root rect `(1486,205)-(3331,1365)`，宽1845，对照之前 DPI-unaware 宽1230；窗口位置已变化，不能比较旧绝对坐标。GTI rcCaret 仍是 caret HWND 客户区坐标，不能直接与屏幕矩形求交。日志：`C:\Users\Administrator\.zcode\cli\exec\sess_subagent_agent_90c2b973-b8c9-4bf4-9d3c-7cebd07ff629\call_Nva9gauGG0GaL1EzJChezrcw-stdout.log`。

**本次新增只读 IA2 继承层遍历**：本机 cargo registry 未发现 IAccessible2/IAccessibleText 类型绑定；访问上游 AccessibleText.idl 的 WebFetch 和 curl 均 DNS 失败，因此不凭记忆拼 IA2 专属 vtable。对 QueryService 成功取得的对象 QueryInterface 到已有 typed `IAccessible`，安全读 Name/Value/Description/Role/State 并以 AccessibleChildren 递归；这属于 IA2 对象的 MSAA 继承层，不是 IA2 text/attributes 全覆盖。每个成功 HWND 限128节点、12层、单层64子节点、6秒协作预算，文本输出沿用256 UTF-16 units；简单 child ID 不作独立树根递归，所有 cap/错误显式输出。无新增依赖、注册、输入、剪贴板操作或前台激活，内存为有界临时诊断开销，不进入产品常驻路径。

执行 `target\debug\focus_probe.exe --raw-snapshot 8259616 --extended-interfaces`（外层45秒超时）成功结束：UIA baseline255节点/1614ms，after255节点/1653ms，前后 foreground 均保持 `0x10298`（ZCode），目标 GTI active/focus/caret=0。本轮不是 composer 聚焦现场，也没有保留 marker。4个 IA2 对象的继承树分别访问63、76、116、128节点（HWND依次 `0x108a6`、`0x108a8`、`0x20886`、`0x1088e`）；有 depth 截断，最后一路还触达 node cap，不能据此排除未遍历子树中的 composer。仍未识别可可靠操作的 composer 身份。ISimpleDOM 两个 RWH 再次 `0x80040155`，其他 HWND 返回 E_FAIL/E_INVALIDARG，不能混写成接口不存在。原始日志：`C:\Users\Administrator\.zcode\cli\exec\sess_subagent_agent_03bfc1f1-72cc-4936-8a83-2876794492d3\call_uXfT4lJxzMyCDw9ZMtZKlDk0-stdout.log`。

只读 `reg query HKCR\Interface\{1814CEEB-49E2-407F-AF99-FA755A7D2607} /s` 在当前进程注册表视图找不到该键。ISimpleDOM 的下一步依赖是该 IID 在客户端位数对应视图的有效 COM 封送支持（通常为匹配的 proxy/stub 及其注册，或经核实的免注册激活配置）；不能仅补 Rust 方法声明就消除跨进程封送错误。未验证任何具体 DLL 包/版本可解决，也没有安装或注册它；不同位数视图、provider 内部状态仍需分别核查。IA2 已可取得，不能把 ISimpleDOM 的注册障碍推广成 IA2 不可用；IA2 专属文本/属性仍待权威 IDL 绑定和逐节点验证。

**最终验证**：新增继承遍历后 `cargo test -p real-im-verify` 通过（4测）；`cargo build -p real-im-verify --bin focus_probe` 通过；只格式化两个探针 Rust 文件；`cargo check -p real-im-verify --bin focus_probe` 与 `cargo clippy -p real-im-verify --bin focus_probe -- -D warnings` 均通过。命令均使用本仓库绝对 `--manifest-path`。未跑整个 workspace，无产品修改或提交。`.zcode/plans/plan-sess_2cc0e37c-d0d1-4162-bd06-d8988d66bd8c.md` 是主会话既存批准计划，已检查并原样保留，不是本次新建报告。

**未闭合分支**：IA2 专属 text/attributes 与未遍历后代；ISimpleDOM 封送依赖；Qt 更深 provider 对照；独立 RWH RawView 根和 focus 父链反查；UIA StructureChanged；硬 COM 截止；安全现场 composer E2E。开源来源只证明可探索机制或历史实现，尚无当前千牛版本已证实100%的现成方案。以上不是产品化验收通过，不应恢复旧任意 Edit 粘贴入口。

### 动态 native caret 方案（2026-09-05）

已在 `tools/real-im-verify/src/focus_probe_raw.rs` 增加并编译 `--native-caret` 只读探针。它不使用窗口比例：实时读取目标线程 `GetGUIThreadInfo` 的 `hwndCaret/rcCaret`，以 caret HWND 客户区转换屏幕点，并拒绝前台或点归属根窗口变化；随后查询 `OBJID_CARET`、`accFocus`、UIA `ElementFromPoint`、MSAA `AccessibleObjectFromPoint` 与 Qt 根 `accHitTest`。这是当前最短可运行实验，但 caret 身份仍不能单独证明是 composer，故尚未接入粘贴注入。

命令：`target\\debug\\focus_probe.exe --raw-snapshot <fresh-decimal-Qianniu-HWND> --native-caret`。2026-09-05 已通过 `cargo test -p real-im-verify --bin focus_probe`（5 tests）及 debug build。主线程现场若接待中心不可恢复，不应等待 GUI：保持现状即可确认工具可执行；产品接缝应放在 Win32 target adapter/`TargetTracker` 的粘贴前校验，先要求 `foreground == tracked hwnd`、live caret、point root 一致，再调用现有 CF_HDROP/CF_PNG + Ctrl+V；任何“composer 未证明”结果必须降级只复制。

### 角落/输入面点击恢复 composer：配方定位与产品化 E2E（2026-09-05，用户线索驱动）

用户提供关键线索：点击窗口特定边框空白区可直接让输入框恢复聚焦。用 `--corner-matrix` 批量探针（单进程原子循环：失焦→产品激活→HTCLIENT 守卫→SendInput 单击→GTI caret + MSAA point + UIA 焦点判决）系统排查：

**激活后自恢复假设（三连否证）**：SW_RESTORE+SetForegroundWindow、产品激活器、`--a11y-activate`（CEF 树 8→97 物化）后 hwndCaret 均为空。a11y 唤醒只暴露买家账号可写 Edit——不能 SetFocus。

**千牛（接待中心 1845x945 物理 @150% DPI）逐点证伪→证实**：
- 窗口四角极限点（bl/tl/tr/br 内缩 0~20px，最小化与 peer 两种失焦各一轮）：全部 foreground_only，UIA 焦点元素=窗壳(50032)。左下角是原生侧栏、右下是工单面板，均非输入面。用户早期观察到 resize 光标=贴角点击被 WM_NCHITTEST 非客户区吞掉（探针加 HTCLIENT 守卫后点击确证进客户区，仍无焦点——排除此因）。
- 面板 padding 条（y=935，按钮行下方）：foreground_only。
- **输入框本体内部（x 518~900，y 780~860 物理）：caret 出现 9/10 轮**（`caret_screen=Some(...)`，位置恒在框内 client≈(531,765)）；MSAA point 语义因 CEF 树塌缩降级 role=10 name=接待中心（对照：树物化基线为 role=7/编辑）。
- 手动路径粘贴 2 轮：Ctrl+V 后截图证实 `CNRCORNER1` ×2 落框（marker 前有用户 IME 残留组合串"哪个"，清理时一并消失；基线草稿本为空）；买家账号字段全程"请输入"占位未动；零发送。

**微信 4.x（主窗口 1398x1050 物理 @150% DPI，id=wechat 画像）**：
- 右下角贴角 0px：focused=[]（resize 咬角）；内缩 4~20 物理像素：**10/10 轮 composer 获焦**——UIA `class=mmui::ChatInputField` 可写 textfield（has_keyboard_focus），与买家账号类错误目标可语义区分。推翻「微信 UIA 全通道死」：主窗口前台时树完整（此前死通道结论限独立聊天窗/聊天记录查看器场景）。
- 探针内闭环：Ctrl+V marker → UIA ValuePattern 读回 `value=CNRCORNER2` → SetValue("") 清理复读空。无原生 caret（GTI 恒空），焦点证据只能走 UIA textfield。

**产品级 E2E（--via-product：失焦循环 + 产品激活器 + 产品 `FocusStep::InputPointClick` 全链 + 语义判决）**：
- 千牛 `x=413 y=WINDOW_HEIGHT-75`（逻辑像素）：**10/10** `FocusedByAnchor` + caret 恢复（ClickEvidence `ExprPoint{x:413,y:555}`→screen(985,1304)）；追加 2 轮产品路径粘贴（caret 前移证实落框）+ End+20BS 自清理（caret 归位、截图复核框空）。
- 微信 `x=WINDOW_WIDTH-8 y=WINDOW_HEIGHT-8`：**9/9 全闭环**（焦点+粘贴+读回+清理；1 轮激活竞态安全跳过未计入）。注：期间微信活动会话为真实联系人，marker 只进草稿且每轮即清，零发送。
- 耗时：产品路径整轮（含最小化失焦+激活）约 260~560ms；千牛点击→caret ~350ms 内。

**坑与守则**：
1. 跨进程命令间前台漂移/HWND 失效会让「点击→验证」失真——判定必须与点击同进程原子完成。
2. 贴角点命中 Qt resize 边框（HTBOTTOMLEFT 系）：WM_NCHITTEST 返回非 HTCLIENT 时点击被系统吞掉；产品 click_anchor 已加守卫+向心内缩重试（全部几何共用）。
3. 千牛粘贴读回不可用（UIA 焦点=窗壳无 ValuePattern），verified 只能靠 caret/settle/视觉；探针清理只删本轮 marker（End+N×BS），草稿保护=基线非空且不含 marker 时跳过。
4. VK 路径合成 End/Backspace 时禁用 scancode 路径（End sc=0x4F 无扩展位会被当 NumPad1 打进草稿）。
5. 用户提供的人工观察「左下角+5-8px」最终定位为「输入框本体左下区域」——窗口几何角落全部证伪；用户可感知的"角落"与窗口客户区几何角落不是同一参照物。
