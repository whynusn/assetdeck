# 上框延迟记账 — 08-24-im-paste-latency

口径说明：

- **我们自己占时** = 从 `paste_targeted` 进入到 `inject(Ctrl+V)` 返回之间，由本项目代码引入的等待。
  这是 AC2 的考核对象（要求降幅 ≥40%）。
- **尾段** = Ctrl+V 之后 IM 自己解码剪贴板载荷并渲染卡片的时间，发生在 IM 进程内，
  我们只能通过缩小载荷影响它（见 §3）。
- 墙钟总时长含 `cargo run` 启动与真实库装载，只作为量级参考，不作为 AC2 判据。

## 1. Before（改造前，08-23 与 08-24 实测）

| 阶段 | 位置 | 实测 | 性质 |
|---|---|---|---|
| 剪贴板写入 | `crates/pipeline/src/lib.rs:209` | 亚毫秒 | 已最优 |
| 前台确认轮询（10ms 步进，上限 80） | `win32.rs:411-422` `wait_for_foreground` | 21~40ms | 轮询 |
| `settle_ms` 固定睡眠 | `win32.rs:388` / `:403` | 微信 150 / 千牛 120 / 默认 80 | 固定睡眠 |
| UIA `GetFocusedElement` 判定 | `win32.rs:1011` | 首次 79ms（COM 冷启动），预热后 5ms | 每次重建 COM |
| UIA `SetFocus` 全子树扫描 | `win32.rs:1037-1091` | 微信 22~27ms、千牛 83ms | 注定失败（可写候选实测 0） |
| 锚点单击后固定沉降 | `win32.rs:1201` `ANCHOR_CLICK_SETTLE_MS` | 60ms | 固定睡眠 |
| 就绪度探测（又一次 UIA 往返） | `lib.rs:271` → `win32.rs:509-524` | 3~30ms | 结果只用于置 `verified` |
| Ctrl+V 注入 | `crates/pipeline/src/lib.rs:297` | 亚毫秒 | 已最优 |
| **我们自己占时合计** | — | **350~550ms** | — |

### 1.1 Before 墙钟基线（08-24 本轮实测）

| 目标 | HWND | 素材 | 载荷字节 | 墙钟 | 结果 |
|---|---|---|---|---|---|
| 微信 4.0（文件传输助手） | 2163916 | `dog.jpg` | 1434465 | 2065ms | `notice[warning] 已粘贴…请确认输入框内容`（浅探测未能证明，符合 D15） |
| 千牛（接待中心，自我会话） | 721614 | `dog.jpg` | 1434465 | 3111ms | `notice[success] 已上框到…` |

两次均未发送，千牛那次已 `--cleanup-input` 清场。

### 1.3 步骤 1~2 后的中途实测（同素材同 HWND，各 2 次）

| 目标 | 墙钟（步骤 0 基线） | 墙钟（步骤 1~2 后） | 差值 |
|---|---|---|---|
| 微信 4.0 | 2065ms | 1830 / 1881ms | −185~235ms |
| 千牛接待中心 | 3111ms | 2894 / 2948ms | −163~217ms |

两项手段合计：UIA 自动化对象改 `thread_local!` 复用（省 COM 冷启动），
画像 `focus_strategy = ["already", "anchor"]` 跳过注定失败的全子树扫描
（微信 22~27ms、千牛 83ms）。墙钟含 `cargo run` 启动噪声，仅作量级参考。
两次均停在输入框，未发送。

### 1.4 步骤 3 后的中途实测（同素材同 HWND，各 2 次）

| 目标 | 步骤 0 基线 | 步骤 1~2 后 | 步骤 3 后 |
|---|---|---|---|
| 微信 4.0 | 2065ms | 1830 / 1881ms | 1814 / 1801ms |
| 千牛接待中心 | 3111ms | 2894 / 2948ms | 2857 / 2552ms |

手段：非严格就绪档（微信/千牛都是 `uia_shallow`）不再做第二次 UIA 往返，
改走 `ReadinessProbe::blockers()` 的两项 O(1) 否证（`IsWindow` + `IsWindowEnabled`）。
提示文案与否证中止语义均未变：微信仍是 warning「请确认输入框内容」，千牛仍是 success。

### 1.5 步骤 5 后的中途实测（事件等待取代 sleep/轮询，同素材同 HWND，各 3 次）

| 目标 | 步骤 0 基线 | 步骤 3 后 | 步骤 5 后（run1/2/3） |
|---|---|---|---|
| 微信 4.0 | 2065ms | 1814 / 1801ms | 1997 / 1798 / 1807ms |
| 千牛接待中心 | 3111ms | 2857 / 2552ms | 2716 / 2695 / 2771ms |

手段：`Win32WindowActivator::activate` 的两处 `sleep(settle_ms)` 与 `wait_for_foreground`
轮询、`uia_set_focus_on_editable`/`click_anchor`/`uia_focus_wechat_input` 的固定 `Sleep`，
全部换成步骤 4 事件泵的「先订阅再动作 + `wait(cap)`」。`real-im-verify` 里打开 IM 会话
后的 700ms 固定睡眠、清场后的 250ms 固定睡眠也已改为同样的输入面事件等待，事件先到即走。
墙钟含 `cargo run` 启动噪声，仅作量级参考；分阶段 `Observed{elapsed_ms}` 由步骤 7
`--timings` 统一测量后回填 §2。两次均停在输入框、未发送，千牛已 `--cleanup-input` 清场。

注意（待步骤 7/8 复核）：步骤 5 后千牛的 notice 从 `success` 变为
`warning 已粘贴到 千牛…请确认`。上框行为红线未破（只上框未发送，readback 有内容、
清场生效），但 tone 变化需对照 D15 确认是否属焦点判定退化，不得当作正常忽略。

### 1.2 载荷字节参考（`samples/library/objects/*/paste.png`）

| 素材 | 字节 |
|---|---|
| 小 | 712690 |
| 中 | 1053370 |
| 大 | 1434465 / 1461400 |

## 2. After（改造后，逐步回填）

分阶段实测（08-24，`--timings`，微信 2163916 / 千牛 721614，`dog.jpg` 1434465B）。
`timing[activate]` 打两行：第一行是产品 `activate`（Alt 释放前台锁 → SetForegroundWindow
→ 稳定前台复核 → 输入面事件等待），第二行是 pipeline 聚焦前的二次 `activate`（此时目标已是
前台，走 `already_foreground` 早退＝0ms）。

| 目标 | activate（冷激活） | activate（热二次） | 我们占时（到 Ctrl+V 返回） |
|---|---|---|---|
| 微信 4.0 | 4~7ms | 0ms | `timing[paste] 18~87ms` |
| 千牛接待中心 | 6~7ms | 0~6ms | `timing[paste] 85~88ms` |

对照 §1 Before「我们自己占时合计 350~550ms」：冷激活的前台确认+settle 从 100~210ms
压到个位数毫秒，AC2 ≥40% 降幅达标且有余量。

### 2.1 冷激活稳定性修复（D16，本轮关键）

症状：千牛↔微信交替制造真冷切换时，产品第一轮**裸 `SetForegroundWindow`（无 Alt）**只拿到
一条*瞬时*前台事件就早退返回 `Ok(true)`——但该前台不稳定，随即弹回原前台（用户看到的
「微信反复被拉起、千牛只闪红任务栏」）。于是 pipeline 的 `preinject` 校验发现 `fg != target`
（例：`fg=721614 target=2163916`），以 `WindowGone` 中止，提示「已复制，未能上框…目标窗口已关闭」。

根因：成功判据太松——**拿到一条瞬时前台事件 ≠ 稳定前台**。裸 `SetForegroundWindow` 跨进程
会被 Windows 前台锁拒绝所有权移交（千牛闪红），或只产生瞬时前台（微信弹回）。

修复（`win32.rs` `activate` + 新增 `drive_foreground`）：冷目标不再单独尝试裸
`SetForegroundWindow`，直接走已验证可靠的 **Alt 按下+抬起释放前台锁 → SetForegroundWindow**，
并把成功判据升级为「稳定前台」——动作后要么立刻 `GetForegroundWindow()==hwnd`，要么在等到
一条 `EVENT_SYSTEM_FOREGROUND` 后**再复核一次** `GetForegroundWindow()==hwnd`，把瞬时前台
筛掉，不稳则交由第二轮重试。全程无 sleep/轮询。

A/B 实测（交替冷切换，微信↔千牛，各 3 轮）：两者均 `activate 4~7ms result=Ok(true)`、
`trace[preinject] fg==target`、notice 变为「已粘贴到 …，请确认输入框内容」。
修复前微信冷激活 `fg=721614`（漂移）→ `WindowGone`；修复后 `fg=2163916` 稳定命中。

| 阶段 | Before | After | 手段 |
|---|---|---|---|
| 前台确认 | 21~40ms 轮询 | 冷激活并入 `activate` 4~7ms | `EVENT_SYSTEM_FOREGROUND` 事件等待 + 稳定前台复核 |
| `settle_ms` | 80~150ms 睡满 | 冷激活并入 `activate` 4~7ms | `await_input_surface` 事件等待 |
| UIA COM 冷启动 | 每次重建 | 待测 | `thread_local!` 复用 |
| UIA 全子树扫描 | 微信 22~27 / 千牛 83ms | 待测 | 画像 `focus_strategy` 跳过 |
| 锚点沉降 | 60ms 睡满 | 待测 | 点击前订阅 + 事件等待 |
| 就绪探测 | 3~30ms UIA 往返 | 待测 | 非严格档改走 `blockers` |

## 3. 尾段（IM 进程内，非本任务可优化范围）

用户体感最重的一段是 Ctrl+V 之后微信 4.0 自己解码 0.7~1.4MB `CF_PNG` 并渲染卡片。
步骤 7 用 `--tail-probe`（注入后继续收目标进程 `EVENT_OBJECT_*`，记录最后事件距注入的毫秒数）
作为可观测代理，验证「这段能否由我们优化」。结论预期：只能靠缩小载荷，
而降质需另立任务征得用户同意。

2026-08-25 修订：「只能靠缩小载荷」不成立，还可以**换交付物**——交文件引用而非像素，
见 §6 与 D22。同时更正本节旧假设「千牛 `paste_sends=["files"]` 所以图片不能改走 HDROP」：
实测千牛只有**视频** HDROP 会即发，图片 HDROP 稳定停在输入框，画像已按类别拆分声明。

## 4. 常驻代价

新增 WinEvent 泵线程后的空闲工作集（AC10 预算 <100MB）：待步骤 8 回填。

## 5. 大素材上框卡顿修复（2026-08-26，D20 扩展）

问题：素材较大时「上框」有明显卡顿，而手动复制粘贴图片不卡。

根因拆解（两段成本叠加，且 PNG 原图路径无任何尺寸上限）：

1. **触发侧（我们进程、UI 线程同步）**：materialize 对 PNG 原图直接
   fs::read 整个文件（几十 MB 无 cap）→ 协商层 to_vec 一次全量拷贝 →
   剪贴板写入端 GlobalAlloc + memcpy 一次全量拷贝。三份全量搬运全在
   单击/双击回调里（crates/app-ui/src/main.rs 上框闭包），帧直接冻结。
   记账表「剪贴板写入亚毫秒」是在 1.4MB 载荷上量的，大图线性放大后即肉眼可见。
2. **尾段（IM 进程内）**：Ctrl+V 之后 IM 跨进程取走整块 → 解码全分辨率 PNG →
   渲染卡片。延迟记账 §3 已确认这是体感最重段，且随载荷字节增长。

手动复制不卡的对照：看图软件里图片早已解码在内存（复制无需读盘/编码），
且规范应用用延迟渲染（SetClipboardData(NULL) + WM_RENDERFORMAT 按需渲染），
复制动作 O(1)；我们则是急切 + 全量 + 同步。

修复（三项，均不碰「UI 进程不解码」红线）：

| 手段 | 落点 | 效果 |
|---|---|---|
| PNG 原图也走 worker 派生 paste.png（4096 cap） | catalog_loader.rs 物化层 + derive-thumbs/derive-paste-png 扩展 PASTEABLE/DERIVABLE | 同时压住触发侧拷贝与 IM 尾段两个成本源；旧库缺派生时 PNG 回退原图不退化 |
| 协商层不再 to_vec 全量拷贝 | platform::ClipboardPayload 字节/文本变体改 Cow::Borrowed，写入端单次搬进剪贴板块 | 触发侧少一份整块内存搬运 |
| 物化 LRU 缓存（条目 ≤4、总字节 ≤16MB，超预算不缓存） | RealAssetResolver | 同一素材反复上框不重复读盘，热路径不碰 meta.db |

回填（2026-08-25，`微信图片_20260823181412_20_5.jpg`，raw 2.3MB / paste.png 8.84MB，12.6 兆像素）：
触发侧已按预期回到常数级，`timing[paste]` 6~16ms；但**端到端仍有 2s 级卡顿**，
因为成本整块转移到了对端解码（见 §6）。这说明缩小载荷只是治标。

## 6. 图片改交文件引用（2026-08-25，D22）

对照实测（同一素材、产品协商路径、`real-im-verify.exe --timings`）：

| 路径 | 微信 exe_wall / 进程 CPU | 千牛 exe_wall / 进程 CPU |
|---|---|---|
| `CF_PNG`（8.84MB 全量像素） | 2061~2082ms / 1859~2031ms | 3346ms / 2234ms |
| `CF_HDROP`（248 bytes 路径） | 587ms / 312ms | 1693ms / 156ms |

两条路径我们侧的 `timing[paste]` 都是 6~16ms，差异全在对端：交像素时 IM 必须在自己进程内
全量解码 12.6 兆像素；交路径时 IM 只向外壳要几百像素缩略图
（`IShellItemImageFactory` 256px 实测 18~21ms，且有系统缩略图缓存）。
这也解释了「手动从文件管理器复制粘贴不卡」——外壳交的是 `CF_HDROP`，不是像素。

格式集探针 `target/thumb-probe/clip-formats.ps1`：外壳复制会放 13 种格式
（含 `FileContents` 延迟流、`FileGroupDescriptorW`、`Shell IDList Array`）。
我们只写裸 `CF_HDROP`（TOTAL_FORMATS=1），微信与千牛照样渲染真缩略图，
因此「必须构造完整 OLE `IDataObject` + `FileContents` 延迟流」并非必要条件。

视觉取证：`Default_Project_probe/r17-wx-prodpath-files.png`、`r17-qn-prodpath-files.png`
（输入框内是缩略图，不是文件名卡片）；微信按发送后发出的是图片消息
（`v6-wx-hdrop-sent.png`）。
