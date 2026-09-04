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
