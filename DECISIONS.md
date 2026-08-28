# DECISIONS.md — 架构与产品决策记录

> 产生方式：grill-me 决策树拷问会（2026-08-21）
> 状态标记：✅ 已锁定 ｜ ⚠️ 有连带风险/义务 ｜ 📋 待办

---

## 一、决策总览

| # | 分支 | 决策 | 状态 |
|---|---|---|---|
| D1 | 首发目标用户 | 通用 IM 用户冷启动，不绑定客服场景 | ✅ |
| D2 | v1 范围切分 | 双线并行 MVP（管理器 + 发送器同时做，均砍到最小） | ✅ |
| D3 | 资产规模验收目标 | 10 – 100 万条规模下必须丝滑 | ✅ |
| D4 | 检索机制 | RoaringBitmap 分面索引 + FTS5 全文；v1 不做向量检索 | ✅ |
| D5 | 分类标签生产方式 | 导入时手动分类 +「待分类」收件箱兜底 | ✅ ⚠️ |
| D6 | 视频支持深度 | 缩略图 + 时长 + 点击弹原生播放；悬停 scrub 留 v2 | ✅ |
| D7 | 资产入库模型 | Eagle 式复制入库（.library 包） | ✅ ⚠️ |
| D8 | auto-send 形态 | 双击 = 复制 + 粘贴进输入框；回车直发为独立开关，默认关 | ✅ |
| D9 | 技术栈 | Rust + Slint | ✅ ⚠️ |
| D10 | 内存验收线 | 务实档：空闲 ≤100MB / 浏览 10 万条 ≤250MB | ✅ |
| D11 | 进程模型 | UI 单进程 + 解码 worker 进程池 | ✅ |
| D12 | 平台策略 | v1 纯 Windows；Linux/X11/Wayland 整体推迟至 v2 | ✅ |
| D13 | 多 IM 目标路由 | 一键上框为核心（终点=素材进输入框）；热/冷目标统一路由 + 就绪度探测 + 统一反馈；auto-send 降为 P2 可选开关 | ✅ ⚠️ |
| D14 | 上框失败根因 | `CF_HDROP` 必须写绝对路径，平台层强制 | ✅ |
| D15 | 就绪策略 | 否证阻塞才不注入；`Inconclusive` 照常注入并标未验证 | ✅ |
| D16 | 装配边界 | Win32 具体类型只在两处 `win32_runtime_deps()` 里 new | ✅ |
| D17 | 候选淘汰 | picker 按可选择性三态淘汰，不全量罗列窗口 | ✅ |
| D18 | 粘贴即发送 | 是画像级能力（`paste_sends`，按 类别 × 格式 声明），命中则降级为只复制 | ✅ |
| D19 | picker 生命周期 | 热目标锁定「上一次打开的目标」，无 TTL | ✅ |
| D20 | 非 PNG 图像 | 旁挂派生 `paste.png` + `CF_PNG` 内联 | ✅ ⚠️ |
| D21 | 焦点获取 | 三级降级（已可写 / UIA SetFocus / 锚点点击）；`Unavailable` 不降级为只复制 | ✅ |
| D22 | 图片上框格式 | 首选 `CF_HDROP` 交文件引用，`CF_PNG` 退为兜底（解码成本从对端 2s 降到 0.2s） | ✅ |

---

## 二、决策详情与连带义务

### D1 首发目标用户：通用 IM 用户冷启动
- **内容**：不绑定电商客服场景，先做通用素材管理 + IM 粘贴发送，用真实使用数据反推垂直领域。
- **后果**：主动放弃「对抗千牛内置素材库」的差异化叙事，护城河只能来自体验本身。
- **验证手段**：「无缝衔接」闭环是唯一早期假设，必须可演示。

### D2 v1 范围：双线并行 MVP
- **内容**：管理器与发送器同步开发，各自砍到最小，第一天即可演示完整闭环。
- **已知风险**：两头都可能做不深。缓解：以闭环演示质量为唯一优先级锚点。

### D3 资产规模：10 – 100 万条
- **内容**：性能验收以该量级为前提，而非「能打开就行」。
- **连带义务**：
  - 索引层必须用紧凑结构（位图索引），禁止朴素全量载入；
  - 渲染层必须自建虚拟化网格（见 D9）；
  - 缩略图必须两级缓存（磁盘 LRU + GPU 显存 LRU）。

### D4 检索机制：分面位图索引
- **查询路径**：分类/标签/属性 → RoaringBitmap 交集（100 万条 <1ms、常驻内存仅几 MB）；文件名/备注文本 → SQLite FTS5。
- **v1 明确不做**：向量检索 / 以图搜图。架构上预留候选集抽象层，v2 可插拔。
- **可选增强**（已定必做，见 D7）：pHash 查重（每图 8 字节，100 万条 ≈ 8MB）。
- **关键洞察存档**：若未来需要语义分类，embedding 只需导入时离线用一次，产出离散标签落库后向量即弃——查询路径始终是纯元数据运算。

### D5 分类来源：手动导入时分类
- **内容**：用户拖入图片/视频时手动选择分类；暂不确定的归入「待分类」收件箱。
- **⚠️ 已记录风险**：与「一天几百张截图丝滑拖入」存在摩擦，「待分类」收件箱大概率积压死亡（GTD 收件箱经典死法）。
- **📋 缓解待办**：后续迭代加「按来源自动建议分类 + 单键确认」。

### D6 视频深度：缩略图 + 点开播放
- **内容**：导入时抽 1–3 帧生成缩略图 + 提取时长；点击弹原生播放器预览。
- **边界**：悬停 scrub 时间轴预览、波形、关键帧条 → v2。

### D7 入库模型：Eagle 式复制入库
- **内容**：文件拷贝进 .library 包，数据自洽、可迁移。
- **⚠️ 连带义务**：
  1. 大视频拷贝与丝滑导入冲突 → **异步拷贝队列**：拷贝期间直接从源文件出缩略图与预览，体感瞬时入库；
  2. 双倍磁盘占用 → **pHash 导入去重从可选升级为必做**，重复拖入不得重复占盘。

### D8 auto-send 解耦形态
- **管线**：`触发(双击/热键) → 资产解析 → 格式协商(CF_HDROP/PNG/DIB/text) → 剪贴板写入 → 焦点校验 → 合成 Ctrl+V → [开关] 合成 Enter`
- **行为定义**：双击素材至少完成到「素材进入对话框」；回车直发是管线末端的独立布尔开关，**默认关**。
- **焦点校验规则**：热键唤起面板时记录「前一前台窗口」；选择素材后先校验该窗口仍存活再注入 Ctrl+V；校验失败降级为仅复制并 toast 提示。

### D9 技术栈：Rust + Slint
- **⚠️ 三项已记录风险**：
  1. **许可证陷阱**：Slint 免费版为 GPLv3。闭源商业化前必须三选一：GPL 开源 / 商业授权 / 免版税桌面许可（读条款）。此项影响是否需要早期换框架，**越早确认越好**（见行动项 A1）；
  2. 百万级变宽高比瀑布流网格需自建虚拟化组件（Slint ListView 不够用）；
  3. 视频纹理管线（解码帧 → GPU 上传）自研，无 libmpv 现成轮子。

### D10 内存预算：务实档
- **合同数字**：空闲 RSS ≤100MB；浏览 10 万条 ≤250MB。
- **参照系**：Eagle（Electron）空闲 300–500MB，本产品须低于其一半以下。
- **📋 强制义务**：写入 CI 做内存回归监控——没有监控的预算等于没定。

### D11 进程模型
- **UI 主进程**：单实例，永不执行解码等重活。
- **解码 worker 进程池**：缩略图生成、视频抽帧、pHash 计算全部隔离在 worker；池大小按 CPU 核数封顶；worker IO 优先级降为 idle；worker 崩溃自动重启且不拖垮 UI。

### D12 平台策略：v1 纯 Windows
- Linux（含 X11/Wayland）整体推迟至 v2。
- Windows 侧注意项：UIPI——管理员权限窗口收不到普通进程 SendInput；焦点校验用 Win32（GetForegroundWindow + 进程/窗口标题匹配）。

---

### D13 多 IM 目标路由：一键上框为核心

> 背景：后台可能同时存在多个 IM（微信、QQ、Telegram、千牛、拼多多商家版、钉钉、企业微信等），
> 用户要能「一键把素材上到自己想要的那个聊天框」。本决策是 D8 的延伸与细化。
> ⚠️ 全文的 exe 名 / 窗口类名 / 标题格式 / UIA 可用性均为设计推演，尚未在本机验证（见行动项 A5、A6）。

**一句话定盘**：产品成功标准是「素材一键进入正确 IM 的输入框」；按回车自动发送是可选增强，永远不进核心链路。
由此把设计分成三层优先级：

- **P0 核心**：目标解析（热/冷）· 激活并确认前台 · 就绪度探测 · 剪贴板写入 + Ctrl+V 上框 · 统一降级为「仅复制」。核心链路终点=素材进输入框、光标就绪、**绝不合成回车**。
- **P1 精准**：热目标粘性锁定 · 窗口级会话身份 · 悬停可视预览 · 分级体检 · 目标健康色。
- **P2 增强**：auto-send（合成回车，默认关，沿用 D8）· click_anchor 输入框聚焦 · 实时缩略图预览。

**目标身份不能用 HWND**：微信/QQ/千牛「关到托盘」时主窗口 HWND 被销毁，再开是新句柄。
故目标分双层——`TargetId`（逻辑身份，持久化，profile_id + scope + account_hint）与 `TargetBinding`（运行时绑定，`hwnd: Option`，None=休眠而非死亡，窗口回来自动重绑）。

**热目标：粘性锁定，两条铁律**。放弃瞬时采样 `GetForegroundWindow`，改常驻 `TargetTracker` + `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`（事件驱动、无轮询，1–2s 低频兜底轮询防漏）。前台事件三分类：
- 合格目标（命中某 profile 且可注入）→ **改写热目标**；
- 自身（我们的面板/主窗口/浮层）→ 忽略；
- 无关（浏览器/Explorer/截图工具/桌面/任务栏/UAC/锁屏）→ 忽略。
- 铁律 A（粘性）：只有合格目标能改写热目标，**无 TTL**——用户中途做任何别的操作都不动它。
- 铁律 B（宁缺勿错）：不确定就降级为仅复制。发错客户的代价远高于少发一次。图钉=更强冻结，优先级 `Pinned > Explicit > Tracked > NotFound`。
- 追踪器是纯函数状态机（`on_foreground(snapshot) -> Option<TargetId>`），粘性语义全量可单测。

**冷目标：内置目标册 + 捕获式自定义**。三层数据源：`profiles.builtin.toml`（随版本发布）· `profiles.user.toml`（同 id 覆盖内置，升级不冲掉）· `targets.json`（实际目标册）。
首批内置：微信、QQ、Telegram、千牛、拼多多商家版、飞书、钉钉、企业微信，外加 `generic_im` 兜底。
自定义不填表：点「添加目标」→ 光标变准星 → 点一下那个 IM 窗口，反查 exe/class/title 自动生成草稿。
profile 关键字段：`match`(exe/class) · `formats`（有序回落：有的 IM 只吃文件不吃位图）· `settle_ms`（激活后稳定延时，Electron 壳偏大）· `title_pattern`（抽会话名，决定 scope）· `send_key`（仅 P2 auto-send 用，核心上框不读此字段）。

**就绪度探测（关键取舍）**：窗口存活≠有可写输入框。反例=未登录/未选会话/只读群/模态遮挡/启动中。
最危险特征是 **Ctrl+V 会「成功」但静默失败**，比明确报错更危险，故注入前必须捕获。两档探测：
- P0 廉价否证（微秒级，纯 Win32）：`IsWindowEnabled` / 模态弹窗 / `title_pattern` 判非聊天视图 / 尺寸异常判启动中。只能否证。
- P1 UIA 肯定（10–50ms）：焦点元素 `ControlType==Edit/Document` 且 `IsEnabled && !IsReadOnly`。独立 COM 线程 + 超时，不阻塞 UI 主线程；Electron 壳可能不暴露完整 UIA 树。
- **`Unknown` 取中间档**：照常上框，但结果标 `verified:false`，文案说「已粘贴到 X，请确认」。因此 `PasteOutcome::Injected` 携带 `verified: bool`——这是产品里唯一区分「确认成功」与「大概成功」的地方。探测时序必须在激活之后、注入之前。

**健康性/连通性校验：分级体检 L0–L3**（用户明确要求，自定义目标必过）。越往后越需许可，UI 用四色点回显：

| 级 | 校验内容 | 判定 |
|---|---|---|
| L0 | 配方合法性（字段齐全、正则可编译、格式列表非空） | 纯离线，零许可 |
| L1 | 窗口存活（枚举命中 `match`） | 无窗口=休眠，非失败 |
| L2 | 可注入性（提权/UIPI 探测 + 激活后前台确认 + 就绪度可探测性） | 失败即红，不可上框 |
| L3 | 端到端自证（用户显式发起） | 绿点唯一来源 |

L3 自证流程：切到自我会话（微信文件传输助手 / QQ 我的电脑 / Telegram Saved Messages）→ 上框哨兵文本 → `Ctrl+A`+`Ctrl+C` 读回逐字比对 → `Ctrl+A`+`Delete` 清场。
**全程绝不合成 Enter**，且只验「素材是否成功进入输入框」，不验发送。一次读回比对同时证明「格式协商 + 激活 + 落框」三环全通。
四色：绿(L3 过) / 黄(L2 过但就绪度探不到) / 灰(休眠) / 红(L2 失败)。

**统一反馈 `PasteFeedback`**（severity/target_label/headline/hint/action/diagnostic），四条纪律：先说「已复制、可手动粘贴」→ 说人话（API 名词进折叠区）→ 一条提示配一个动作 → headline 必须回显目标名（让用户发现选错了 IM）。

文案表（每条对应一个 `NotReadyReason` 或降级原因，`every_not_ready_reason_maps_to_nonempty_feedback` 守卫穷举）：

| 情形 | headline | hint |
|---|---|---|
| 未选中会话 | 微信 还没打开聊天窗口 | 先点开一个会话，素材已复制 |
| 未登录 | 微信 尚未登录 | 登录后再试，素材已复制 |
| 只读会话 | 该会话不能发送消息 | 可能是已解散或被禁言，素材已复制 |
| 模态遮挡 | 微信 有弹窗挡住输入框 | 关掉弹窗后重试，素材已复制 |
| 提权 (UIPI) | 无法向管理员身份的 微信 上框 | 素材已复制，请手动 Ctrl+V |
| 目标休眠 | 微信 未在运行 | 素材已复制 |
| Unknown 未验证 | 已粘贴到 微信 · 张三，请确认 | 若输入框是空的，按 Ctrl+V 再试 |

**UI/UX**：两种唤起路径共用一条目标条——热键在 IM 内唤起（人在微信里 → 热键 → 粘回微信）时追踪器已给出目标，**零点击**；人在管理器内唤起时若有 Pinned/Tracked 命中同样零点击，否则一次点选。
目标条用 chip 不用下拉框（下拉把当前目标藏起来，正是发错对象的温床）；选择单元是窗口/会话而非应用（`微信·张三` 确定 vs `微信·当前会话` 不确定，如实标注，绝不把不确定显示成确定）；悬停即所见（画描边高亮 + 300ms 后 `PrintWindow` 快照贴 chip 上方）；`Alt+1..9` 直选；右键钉住；拖素材到 chip = 发那里并钉住；休眠目标置灰保留不消失。
未过体检或走 `generic_im` 兜底解析的目标，双击**只上框到剪贴板**并要求确认一次，之后转常态零点击。

**连带义务/风险**：
- 全文数值（exe/class/title/UIA 可用性/settle_ms）均为推演，须在本机装 IM 实测后写入 builtin（A5）。
- `PrintWindow + PW_RENDERFULLCONTENT` 对 Electron 壳可能全黑或挂线程，须走 worker + 超时（A6）。
- WinEvent 钩子常驻不得侵蚀 100MB 空闲预算（进 M8 实测）。
- **焦点归位未决**：热键路径下我们的面板会抢走前台，IM 内部焦点子控件（输入框 caret）是否在 `SetForegroundWindow` 回切后自动归位，各 IM 行为不一致，Electron 壳风险最高。若不归位则 Ctrl+V 落到消息列表而非输入框——这正是「静默失败」的一种。缓解备选：面板做无焦点浮层（`WS_EX_NOACTIVATE`）从根上不抢焦点；P2 的 `click_anchor` 作为兜底。须实测后定（A6）。
- **不自动拉起未运行的 IM**：目标休眠时只复制并提示，不代替用户启动进程（启动耗时不可控，且登录态未知）。
- 落地为 D8 的超集：核心链路绝不合成回车这条红线由此收紧并新增守卫测试。

### M8 实现复核（2026-08-23）

- `crates/targets`、`platform` 四个窗口 trait、`pipeline.paste_targeted`、`ui-viewmodels` 目标条与路由 VM 均已就绪并有纯逻辑/ Mock 测试。
- 核心上框序列不合成 Enter，`paste_targeted()` 不调用 `send()`；被**否证**的就绪度（`Blocked`：未登录/无会话/只读/模态/窗口消失）一律不注入。`Inconclusive` 的处理取决于画像的 `readiness` 档位，见下方「就绪策略翻转」。
- 冷目标选择键为 `TargetId@HWND`，图钉锁定具体窗口。
- 真实素材已通过 `RealAssetResolver` 读取，微信 4.0 已验证文本、图片、MP4 可进入聊天输入框并读回；全程无 Enter。
- 微信隐藏窗口、实例级进程号已落地。此前记录的「千牛无会话」是**看错了窗口**：千牛有两个顶层窗口，带输入框的是「接待中心」（`tb940472610424-接待中心`），不是「千牛工作台」。画像的 `title_regexes` 已显式声明 `接待中心$`。
- **仍未交付**：热键唤起/无焦点浮层、自定义目标持久化、L0-L3 真实执行器、PDD/Telegram 真实验证、WinEvent 内存与 PrintWindow 收口。
- **实例身份已有保守实现**：`TargetBinding.instance_id` 来自 `exe_name:process_id`，同 profile 不同进程不再自动重绑；跨进程重启后的永久身份仍需显式确认。
- A5 已取得微信/千牛/拼多多的真实 exe/class/title、UIA 可用性和严格就绪结果；A6 仍有 WinEvent、PrintWindow、caret 常态化收尾。

### D14 上框失败的根因是 HDROP 相对路径（2026-08-23，已修复并有回归守卫）

**症状**：图片（jpg）与视频（mp4）「上框」后输入框毫无变化，且没有任何错误可捕获；PNG 与纯文本一直正常。

**根因**：`CF_HDROP` 里写的是相对路径。HDROP 的路径由**接收方进程**按它自己的工作目录解析，微信/千牛解析不到文件就丢弃整次粘贴动作 —— 静默失败，无错误码。PNG 走字节载荷、文本走 `CF_UNICODETEXT`，都不经 HDROP，所以碰巧一直是好的。路径来源有两处相对性叠加：`--library samples/library` 是相对 root，`rel_path` 又是 `/` 分隔字符串。

**决策**：交给剪贴板的文件路径必须绝对，且这条约束在**平台层强制**而不是靠调用方自觉。

- `platform::win32::hdrop_path_list` 对每条路径 `std::path::absolute()`，仍为相对则返回 `PlatformError::Clipboard`。**宁可报错让用户看到提示，也不静默失败。**
- `RealAssetResolver` 按 `/` 逐段拼接 `rel_path` 后再绝对化。
- 守卫测试：`platform` 三条 `hdrop_*` 单测 + `ui-viewmodels/tests/asset_payload_spec.rs`（相对 root 建库 → 断言物化路径绝对且存在）。

**教训**：先怀疑载荷本身，再怀疑就绪度。上一轮把静默失败归因于「输入框未就绪」，方向偏了整整一轮。

### D15 就绪策略翻转为「否证阻塞才不注入」（2026-08-23）

caret 探测与 UIA 焦点元素查询在**能够成功粘贴**的微信、千牛窗口上同样返回空值：微信 Qt 自绘，千牛 CEF，两者都无法在注入前证明「存在可写输入框」。「证明就绪才注入」这条策略因此被实验否证 —— 它在真实内置 IM 上等价于永不注入。

新策略（`profiles.builtin.toml` 四个内置画像 `readiness = "uia_shallow"`）：

- `Blocked(...)`：明确否证 → 只复制、不注入、给出对应原因的友好提示。
- `Inconclusive`：照常注入，结果标 `verified: false`，UI 出 `warning`「已粘贴到 X，请确认输入框内容」。
- `Ready`：注入并标 `verified: true`。

`uia_strict` 保留为**用户可显式开启**的严格档（`Inconclusive` 仍只复制不注入），但不作为内置默认。这是一个有意识的权衡：把「偶尔需要用户瞄一眼输入框」换成「功能可用」，而不是为了追求确定性把主路径堵死。D8 的「绝不合成回车」红线不受影响。

### D16 Win32 具体类型只在两处装配（2026-08-24）

`Win32Clipboard` / `Win32WindowActivator` / `Win32InputFocuser` 等具体实现只允许在 `crates/app-ui/src/main.rs::win32_runtime_deps()` 与 `tools/real-im-verify/src/main.rs::win32_runtime_deps()` 里 `new`。其余层一律吃 trait 对象。`crates/ui-viewmodels/tests/layering_guard.rs` 与 `crates/app-ui/tests/deps_guard.rs`（白名单严格三项：`ui-viewmodels` / `slint` / `platform`）把这条约束固化成测试。

理由：验证工具与产品必须走**同一条**依赖装配，否则「工具里能上框、产品里不能」这类差异会反复消耗真机排查时间——M8 中段已经发生过一次。

### D17 画像候选按「可选择性」淘汰，而不是全量罗列（2026-08-24）

picker 里每个候选带三态：`运行中 · 可选择` / `未运行 · 选择后仅复制` / 不可用则不入列。最小化窗口、悬浮条、通知窗、Loading 壳统统淘汰（千牛一个进程能开出 20+ 个窗口，PDD 更多）。判定依据是窗口可见性 + 类名画像 + 标题模板，不依赖快捷键或进程枚举顺序。

### D18 「粘贴即发送」是画像级能力，按 (类别 × 格式) 声明（2026-08-24，08-25 修订）

千牛对 `CF_HDROP` 的行为是**粘贴即发送**，微信不是。因此在 `Profile` 上新增 `paste_sends`：当协商出的组合落在其中且用户没有显式开启自动发送时，**降级到只复制 + 提示**，绝不用「发送」冒充「上框」。这条直接来自用户红线：默认只做到输入框，发送是可选项。

2026-08-25 受控复验修订了它的维度。原先建模成纯格式集合（`paste_sends = ["files"]`），但实测千牛的即发语义**按素材类别分叉**：视频 HDROP 当场发出会话流，图片 HDROP 停在输入框并渲染真缩略图（取证 `Default_Project_probe/q3-our-hdrop-image.png`，输入框出现缩略图、会话流无新消息）。纯格式建模把图片一起误判为即发，代价是图片被迫走高成本的 `CF_PNG`。因此值域改为按类别分行的表：

```toml
paste_sends = { image = [], video = ["files"], text = [], other = [] }
```

旧的数组写法保留兼容（语义为「所有类别都即发」），因为用户画像里可能已经写了它，静默解析失败或静默缩小范围都等于悄悄拆掉用户自己装的发送保护。mp4 在千牛仍保持「只复制 + 提示」，这是已接受的边界。

### D19 picker 的生命周期归属用户意图，不设 TTL（2026-08-24）

热目标锁定在「上一次打开的目标」，不因超时失效、不因用户中途做别的事失效；只有用户显式打开另一个既定目标才改写。picker 的展开/收起同理由用户操作驱动。

理由：TTL 会让「切出去查个资料再回来」这种最常见的动作变成一次误投递。宁可让用户偶尔手动改目标，也不让系统在背后悄悄换靶。

### D20 图片走旁挂派生 `paste.png` + `CF_PNG`，PNG 原图同样派生封顶（2026-08-24，08-26 扩展）

jpg/webp 等直接以原字节写剪贴板时，千牛不认。方案是在对象目录旁挂一份 `objects/<uuid>/paste.png`，上框时内联这份派生文件的字节并以 `CF_PNG` 交付。UI 进程仍然只做 `fs::read`，**不解码**（D11 不变）。

因此 `.trellis/spec/ui-viewmodels/backend/quality-guidelines.md` 里「只有 PNG 才内联 `png_bytes`」的旧表述被推翻，改为「PNG 原图或旁挂派生 `paste.png` 均可内联」。自动派生已接线：`derive-thumbs` 每次导入后跑，对图片（**含 PNG 原图**）在同一份 worker 解码里旁挂 `paste.png`（协议 `ThumbnailPng` 可选 `paste_dest` 第二输出），旧库重跑一次即回填；`tools/derive-paste-png` 保留为命令行回填入口。

**2026-08-26 扩展（D20 补丁，上框大图卡顿修复）**：PNG 原图不再直接内联，同样经 worker 以 `paste_max_edge=4096`（`protocol::DEFAULT_PASTE_MAX_EDGE`）派生封顶。原因：PNG 原图尺寸无上限时，大图（几十 MB / 万级像素）的完整字节会同时放大触发侧成本（UI 线程 `fs::read` + 拷贝）与 IM 进程尾段成本（跨进程取走整块 + 全分辨率解码 + 卡片渲染）——后者是上框体感延迟的主要来源（见 `research/latency-ledger.md` §3）。旧库未回填派生文件时，物化层对 PNG 回退内联原图（仍是合法 `CF_PNG` 载荷），上框不退化。

### D22 图片上框首选交文件引用（`CF_HDROP`），`CF_PNG` 退为兜底（2026-08-25）

图片的画像格式顺序从 `["png", "files"]` 改为 `["files", "png"]`。

这条推翻了 D20 里「jpg/webp 直接以原字节写剪贴板时千牛不认」所隐含的推论——不认的是 **jpg 原字节冒充 `CF_PNG`**，不是文件引用。实测微信与千牛对图片 `CF_HDROP` 都停在输入框并渲染**真缩略图**（不是文件名卡片），微信按下发送后发出的是图片消息（`v6-wx-hdrop-sent.png`）。

改动理由是实测的进程成本差，同一张 4096x3072 素材（`raw.jpg` 2.33MB / `paste.png` 8.84MB）：

| 路径 | 微信全流程 / 进程 CPU | 千牛全流程 / 进程 CPU |
|---|---|---|
| `CF_HDROP`（files） | 436~587ms / 250~312ms | 1027~1693ms / 125~156ms |
| `CF_PNG` | 2061~2082ms / 1859~2031ms | 3346ms / 2234ms |

差距的来源是**谁付解码**：交路径时对端只向外壳要几百像素缩略图（`IShellItemImageFactory` 256px 实测 18~21ms，且有系统缓存）；交 `CF_PNG` 时对端必须在自己进程内解码 12.6 兆像素。这段开销全在对方进程，我们的 `timing[paste]` 量不到（始终 6~16ms），所以旧记账看起来很漂亮却掩盖了真实体感——这正是「我们的上框卡、手动粘贴不卡」的根因，手动粘贴走的就是文件引用路径。

`png` 保留在末位而不是删掉：库外素材、未来的剪贴板临时载荷等场景没有可交的稳定路径，仍需字节兜底；`FormatPolicy` 的顺序语义已经是「首个可承载者胜」，兜底不会在正常路径上被触达。

已知语义变化：走 files 后交给 IM 的是库内 `objects/<uuid>/raw.jpg`，文件名是 `raw.jpg`（微信按图片消息渲染时不显示文件名）；`CF_PNG` 路径原本兼带格式归一化（webp/bmp → 标准 PNG），改走 files 后由对端负责解码，gif 反而能保住动画。

### D21 输入框焦点获取采用三级降级，`Unavailable` 不降级为只复制（2026-08-24）

`SetForegroundWindow` 只把窗口提到前台，键盘焦点会停在窗口根控件（微信 `Qt51514QWindowIcon`、千牛 `Qt5152QWindowIcon`），Ctrl+V 因此落空——这正是此前每次都要手工点一下输入框的原因。`platform::InputFocuser` 定义三级降级：

1. 已经聚焦在可写控件 → `AlreadyEditable`；
2. 遍历 UIA 子树找可写 Edit/Document，`SetFocus()` 后**复核**焦点确实落在可写控件 → `FocusedByUia`；
3. 按画像 `input_anchor` 比例点击客户区锚点，点击前用 `WindowFromPoint` + `GetAncestor(GA_ROOT)` 确认锚点没被遮挡，点击后还原鼠标位置并复核前台未漂移 → `FocusedByAnchor`。

副作用与限制必须写明：锚点点击是真实鼠标事件，会落在 IM 界面上，因此比例值必须落在输入框区域（千牛 `0.394/0.787` 落在中栏——右侧是「千牛工单」面板），越界配置直接报 `InvalidAnchor` 而**不静默夹紧**，否则错配画像看起来「能用」却在点别的控件。

`Unavailable` 语义与 `ReadinessSignal::Inconclusive` 对称：它表示「没能证明拿到焦点」，不是「证明没拿到」。默认档照常注入并标 `verified: false`；只有用户显式开启 `uia_strict` 才中止注入。理由同 D15——微信 UIA 树只有 2 个节点、千牛聊天区是 CEF `Document` 不暴露 `Edit`，要求「证明」等价于永不注入。

## 三、遗留行动项

| # | 事项 | 截止时机 | 影响 |
|---|---|---|---|
| A1 | 确认 Slint 许可证路线（GPL / 商业 / 免版税） | 框架选型最终冻结前 | 可能推翻 D9 |
| A2 | 「双击素材 → 0.5s 内出现在**正确** IM 输入框，光标就绪」闭环跑通（D13 收紧：不含发送，回车留给用户） | MVP 第一周 | 验证核心卖点 |
| A3 | CI 内存回归监控搭建 | 性能调优开始前 | D10 的验收基础 |
| A4 | 「来源建议分类 + 单键确认」方案设计 | v1.1 规划期 | 缓解 D5 积压风险 |
| A5 | 实测首批 8 个 IM 的 exe/窗口类名/可接受剪贴板格式/标题模板/未就绪态标题，写入 `profiles.builtin.toml` | M8 开工前 | D13 的 builtin 数据基础 |
| A6 | `PrintWindow + PW_RENDERFULLCONTENT` 对 Electron 壳实测（全黑/耗时/挂起）；WinEvent 钩子常驻内存实测；**面板回切后 IM 输入框 caret 是否自动归位**（决定是否改无焦点浮层 `WS_EX_NOACTIVATE`） | M8 悬停预览/追踪器实现前 | D13 浮层、追踪器与「落框成功率」 |

---

## 四、Wayland 调研归档（v2 输入）

> 结论日期：2026-08-21，基于当时最新生态状态。

### 能力矩阵

| 能力 | Windows | X11 | Wayland-KDE | Wayland-GNOME | Sway/Hyprland 等 |
|---|---|---|---|---|---|
| 全局热键 | 原生 | XGrabKey | ✅ portal | ✅ portal (GNOME 48+) | ⚠️ xdg-desktop-portal-wlr 未实现 |
| 输入注入 | SendInput | XTest | libei (RemoteDesktop portal) | libei (46+) | wlr virtual-keyboard/pointer |
| 窗口枚举/焦点校验 | Win32/UIA | EWMH | KWin D-Bus 脚本 | 需自写 Shell 扩展 | wlr-foreign-toplevel |
| 剪贴板读写 | Win32 CLIPBOARD | XCLIP | ✅ wl-clipboard-rs | ✅ | ✅ |

### 关键事实
1. Wayland 安全模型下**不存在「向指定窗口注入输入」的标准 API**——只能「先聚焦目标，再注入」。
2. libei + RemoteDesktop portal 是官方正道（GNOME 46+/KDE Plasma 6），有 consent 弹窗；portal `restore_token` 可缓存授权，但 GNOME 实测无永久授权入口。
3. uinput/ydotool 路线（内核 `/dev/uinput`）：compositor 无关、无弹窗，但需 root 或 udev 规则，绕过 compositor 安全边界，Flatpak 沙箱内不可用——仅 power-user 路线。
4. portal GlobalShortcuts 注册要求应用从 `.desktop` 文件启动（终端启动静默失效）；ConfigureShortcuts UI 目前仅 KDE 支持。
5. GNOME 上窗口枚举需自写 GNOME Shell 扩展（参考 wdotool 的 D-Bus 做法）。
6. 剪贴板是最顺的一环：wl-clipboard-rs 读写 selection 无权限门槛。

### v2 推荐分层
- **Tier 1**：X11 完整平价（XTest + EWMH，成本低）。
- **Tier 2**：Wayland 默认「复制优先」模式（热键面板 → 选中 → 写剪贴板 → 手动粘贴）；auto-send 检测到 libei/ydotool 可用才亮起并标实验性。
- **不做**：per-compositor 适配器矩阵（KWin 脚本 + GNOME 扩展 + wlr 协议三套并行）。

---

## 五、扩展性重构落地（2026-08-26）

> 依据：综合分析与百万级分类底层设计报告（扩展点硬编码 / FacetIndex 内存形态）。
> 原则：行为变化走 Rust trait；格式/类型集合变化走注册表；UI 表现变化走数据字段驱动；
> SQLite 为真相源、内存索引只做加速。

### 已落地（全部伴随测试）

| # | 决策 | 落点 |
|---|---|---|
| D23 | 媒体类型注册表：扩展名 → 类别/导入/缩略图/粘贴派生能力的唯一真相源。新增格式只改 crates/media 的 MEDIA_TYPES 一行，四处分散映射（catalog_loader、sample-library、derive-thumbs、library）全部收口 | crates/media（AssetKind 本体在 domain，pipeline 转发） |
| D24 | 导入/导出包读写器：AssetPackageReader/Writer + PackageRegistry（首个命中者胜）。目录=DirectoryReader，.emo=EmoReader/EmoWriter；新格式只注册新 reader，不再改 CLI 主体 | tools/sample-library/src/packages.rs |
| D25 | FacetIndex 重构为 SoA：HashMap<u32, Asset> → 行表（names/categories/created_at/sizes/kinds Vec）+ RoaringBitmap 成员关系。AssetId 即数组下标；排序在索引层直排（sorted_ids），不再物化 Asset | crates/index |
| D26 | 卡片数据与文本卡片：TileData 增 kind/preview/badge/icon；瓦片按类别切表现（图片/视频走缩略图，文本走图标+首行预览，其他走图标）；TileCardDataProvider trait 统一产出 | appwindow.slint + app-ui/src/cards.rs |
| D27 | 主题 Provider：ThemeTokens + ThemeProvider（Dark/Light 两套 ARGB 色板）；theme.slint 全部颜色转 in-out token，壳层启动/切换时注入。std-widgets 仍 fluent-dark（v1 边界，仅自绘层随主题） | ui-viewmodels/src/theme.rs + theme.slint + main.rs |
| D28 | 设置描述化 + 通用面板：SETTING_SPECS/SettingSpec + AppSettings.describe()/toggle()；slint 侧 for setting in root.settings 通用渲染，新增设置项只改 Rust 侧 | ui-viewmodels/src/settings.rs + appwindow.slint |
| D29 | 分类规则器：CategoryRule trait + RuleChain（显式 > groupName > 父目录）；导入期执行，自动分类/重分类不再移动文件 | crates/library/src/rules.rs |
| D30 | 搜索 Provider：SearchProvider::search(query, base) 统一入口；v1 = 分类/标签名子串 ∪ 文件名 NameContains；≥3 字符全量检索后续接 FTS5 同入口 | ui-viewmodels/src/search.rs + domain Filter::NameContains |
| D31 | 排序器扩展：SortField 增 Size（未知恒垫底、不随方向翻转）、Kind；SoA 直排与 Vec 排序共用语义 | domain + index |
| D32 | UI 枚举映射收口：target-mode / notice-tone / card-kind 的 0/1/2/3 魔法数字收口到 UiEnums 全局常量 + ui_enums.rs 单一映射函数 | appwindow.slint + ui_enums.rs |
| D33 | 子进程任务运行器：ChildTaskRunner 统一「起进程 → PROGRESS 行 → stderr → finished」编排，导入/导出/缩略图派生三处重复合一 | app-ui/src/task_runner.rs |
| D34 | 原生文件对话框：导入/导出弹窗从 PowerShell+WinForms（~3s 冷启动）换成 Win32 IFileOpenDialog/IFileSaveDialog（<50ms，取消映射 Ok(None)）；FileDialogs trait 装配进 TargetRuntimeDeps（D16 边界不变） | platform win32 + TargetRuntimeDeps.dialogs |
| D35 | 导入/导出 UX：移除手输路径 LineEdit；「导入素材…」直接弹文件夹选择器 → 子进程导入 → 缩略图派生，「导出…」直接弹保存对话框；按钮语义统一 | appwindow.slint + main.rs |
| D36 | 导入修正（实测反馈）：① FOS_PICKFOLDERS 文件夹选择器天然看不到 .emo 这类文件 → FileDialogs 增 pick_open_file（FOS_FILEMUSTEXIST + COMDLG_FILTERSPEC 过滤），新增「导入 .emo 包…」按钮走同一子进程管线（包按扩展名分发到 EmoReader）；② 导入管线抽为 spawn_import_pipeline：起子进程前先 create_dir_all(库根)，阶段一失败时**不得触发** thumbnails_generated 库重载（库未建时重载只会抛误导性的「下无 meta.db」，掩盖真实错误）；③ --library-root 解析兼容空格截断 / = 形式 / 成对引号 | platform lib+win32、appwindow.slint、app-ui/main.rs |
| D37 | 浅色主题全量生效（实测反馈「控件内部仍是暗色」）：std-widgets（fluent 样式）的每个内部颜色都是 Palette.color-scheme 的活绑定 → appwindow.slint 显式 `import/export { Palette }`，壳层 apply_color_scheme 启动+实时切换时写 Light/Dark；自绘层照旧走 ThemeTokens。两层同明暗，无需重启 | appwindow.slint + app-ui/main.rs（apply_color_scheme） |
| D38 | 双速导入（万级 .emo 包导入 30 分钟 → 前台高速/后台不抢前台）：ImportMode::Fast（背压 64、拷贝线程 2..8、元数据批量 256、内存 pHash 索引）vs Background（背压 16、拷贝 1、批量 64）+ CliMode + PoolPriority（fast=BELOW_NORMAL、background=IDLE）；有界通道并发流水线（W 工作线程全链路 + 库内异步拷贝池 + 元数据写线程批量落库）；Store 连接 Mutex 化跨线程共享、write_asset 静态化避锁重入、set_dimensions/upsert 批量事务消 fsync；schema v3 = phash 等值索引 | sample-library、crates/library、crates/store、crates/worker、derive-thumbs、settings.fast_import_mode |
| D39 | 日志开关 + 一键导出（低频重要全量记、高频临时开）：log facade + 每进程独立文件 name-pid-millis.log 于 exe 同目录 logs/；child 经 DSH_LOG_DIR/DSH_LOG_LEVEL 继承约定；settings.verbose_diagnostics 实时切 Info/Trace；上框/导入/导出/焦点变化 = Info、轮询 = Trace；设置面板「打开日志目录」按钮 explorer 直达；同目录 8 文件轮转 | crates/logging、app-ui/main.rs、task_runner env、settings.verbose_diagnostics、sample-library/derive-thumbs init_from_env |
| D40 | WinEvent 泵自愈（低配机上框失效根因，2026-08-27）：握手超时不再永久缓存失败——泵线程装好钩子自动翻位、失败冷却 2s 重装、观察器惰性重连、退路 Timer 每轮重试接管成功即停表；前台锁定兜底 Alt 敲击改 Ctrl 敲击；注入前修饰键残留复位。事件驱动退级到时序驱动的稳态概率压到远低于 0.1%（论证见下节） | platform win32（泵/观察器/激活/注入器）、app-ui/main.rs |
| D41 | 上框延迟治理（2026-08-27，D40 后实测仍有肉眼可见延迟）：①物化层零读盘——图片不再预读 png_bytes（LRU 仅 4 条，每次 miss 是几十~几百 ms 的 UI 线程同步 fs::read，png 在 v1 内置画像下是 files 之后的不可达兜底），协商自然回落 CF_HDROP；②pdd/telegram 图片格式顺序回填 D22（files 优先，属落地遗漏而非新决策）；③上框全链路分段计时瀑布进 Info 日志（write/activate/focus/readiness/inject/total），平台关键等待（activate 轮次、settle/anchor/uia-setfocus 结局、focus_input 级别）进 Debug；④**评估结论：不更换底层注入方案**——端到端大头在对端 IM 进程（微信消化 CF_HDROP 实测 250~312ms CPU，D22），换任何注入实现都省不掉，替代路线（UIA 直写/WM_PASTE/OLE 拖放）均已被实测或机理否证 | catalog_loader、profiles.builtin.toml、pipeline lib、platform win32 |
| D42 | 上框解绑竞态自愈（2026-08-28，来自低配机真实日志 app-18212：92 次上框 27 次「目标窗口已关闭」≈30% 失败率，微信窗口始终开着，失败风暴约 2~3.5s 后自愈——与退路 Timer 轮询周期吻合）：窗口枚举的快照竞态（最小化/恢复动画中 GetWindowRect 失败→无面积被过滤等）会让热目标被误判解绑，解绑态的上框请求直接降级 WindowGone，要等下一轮轮询才重绑。修复：①paste 发现热目标「身份在但解绑」时**先强制全枚举重绑**，把「等轮询」变成「当次请求内自愈」；②解绑/重绑状态迁移进 Info 日志（此前只能看到降级 headline，无法回溯哪次枚举把活窗口判没）；③降级路径的 reason/diagnostic 进 Info 日志（copied_only 内），与成功路径的瀑布对称 | ui-viewmodels target_runtime/target_bar_vm、pipeline lib |
| D43 | 壳层缩略图驻留纪律 + 预算化渐进装载（2026-08-27）：修复「切分类瞬间卡顿 + 常驻内存只涨不降」——根因是壳层 slint::Image 缓存为无界 HashMap（grid_vm::ensure_window 的窗外零驻留纪律只落在 VM 字节缓存，生产路径 provider 未接、恒空）：①每次刷新以当前物化窗口 id 集合显式驱逐窗外条目 + LRU 容量兜底（= grid_vm::MAX_VISIBLE，永不与窗口驱逐打架）；②每帧预算 6 张解码，余下 SingleShot Timer 按 16ms 渐进补齐（定向 set_row_data，不整表重建），切分类不再单帧同步解码整窗；③文件缺失负缓存，缺图瓦片不再每帧重试读盘。显示装载≠缩略图生成：原始媒体解码仍在 worker（D11 不变）。附带修复 clear_library「新 VecModel 顶替 UI 模型后回调仍写旧模型 → 清库后重导入不显示」bug | crates/app-ui/src/thumbs.rs、app-ui/main.rs、--bench D43 驻留守卫 |
| D44 | 注入前前台漂移分流 + 再断言（2026-08-27，新低配机日志 app-8920 实证）：D40/D42 后事件驱动已健康（activate 11~27ms、无时序驱动节奏），但 21 次上框 11 次降级且全部 reason=WindowGone「前台发生漂移」，且每次降级后 140~150ms 内必有下一次点击——**点击间隔(~140ms) < 上框耗时(133~226ms)**，注入前前台校验拦掉的是「用户已在连点面板」的正常场景。修复：①校验拆分——`!alive` 才是 WindowGone（提示语恢复准确）；`alive && fg≠hwnd` 新增 ForegroundLost；②前台归属分类 ForegroundRelation（前台 pid vs 目标 pid vs 自身 pid，纯函数可测）：OwnProcess（用户连点自己面板）/ SameAsTarget（目标内部多顶层表面抖动）→ 上框意图未变，**一次快速再激活（100ms 预算、settle=0）复检前台后照常注入**；Foreign（第三方前台）→ 用户真的切走，立即降级、绝不抢回；③漂移与再断言结局进 Info 日志（relation/focus_outcome），下一轮日志可归因。D13「注入前校验」红线不变：再断言后仍复检，通过才注入 | targets/model、platform(lib+win32)、pipeline/lib、pipeline/feedback、target_routing_spec(+3 测试) |
| D45 | 连击合并（2026-08-27，用户证词「我明明没有连点」推翻连点归因后定位）：低配机日志同素材 ~300ms 成对/三连请求（97,97 / 92,92 / 1931×3）的真实来源是 **Slint TouchArea 的 `clicked` 信号在双击的第二次抬笔同样触发**（i-slint-core 1.17.1 `items.rs` Release 分支：先无条件发 `clicked`，`click_count % 2 == 1` 再追加 `double_clicked`——两信号不互斥），单击模式下用户的一次双击 = 两次完整上框请求；粘贴同步阻塞 UI 线程，尾随点击排队到首次完成后立即执行，其 mouse-down 又在 win32k 输入路径当场激活本应用、抢走首次注入前校验的前台（这就是「首条必败、尾条必成」的机理）。修复：`TargetRoutingRuntime` 记录最近一次实际注入的 (素材路径， 时刻)，**750ms（OS 双击时值 500ms + 一轮上框耗时）内同素材请求按连击尾随半程并入**，不再重复上框；`TargetPasteNotice` 增加 `injected` 标记（Warning tone 无法区分「已注入待确认」与「降级仅复制」）；降级/失败清空记录，失败后的立即重试不受影响；窗口内连点**不同**素材是切换意图，不合并。与 D44 构成完整闭环：尾随点击不再发起第二次上框，首次上框被 mouse-down 抢走的前台由再断言当场恢复 | ui-viewmodels/target_bar_vm（injected 标记）、ui-viewmodels/target_runtime（合并逻辑）、target_runtime_spec(+3 测试) |

### D44 补充：新低配机日志的读数与漂移分流语义（2026-08-27）

**日志证据**（app-8920-1787849906424.log，16:58~17:00，D40/D41/D42 修复后构建）：
- 事件驱动健康：activate 段 11~27ms（事件确认节奏，非 Timer 轮询）、write ≤1ms、readiness 2~5us；无裸 v、无反复拉起-失焦循环的旧症状。
- 成功瀑布（10 次）：total 133~226ms，大头 focus 段 85~189ms（UIA 树遍历，低配机物理节奏）。
- 失败 11 次全部 `reason=WindowGone diagnostic=注入前目标窗口失活或前台发生漂移`，且**每次降级后 118~150ms 内必有下一次请求**——21 次请求呈现完美规律：**连点串里除最后一次外全部失败，最后一次必成功**（97 失败失败成功；92 失败失败成功；826 失败失败弃；1931 失败失败失败弃；孤立点击全部成功）。

**机理**：粘贴链路在 UI 线程同步执行（等待用 `recv_timeout` 非泵阻塞，日志时间线证实无重入点击）。低配机上单次上框 133~226ms > 用户连点间隔 ~140ms：下一次点击的 `WM_MOUSEACTIVATE` 在本侧 paste 返回后立刻把前台拉回素材面板——但本侧校验发生在**每次 paste 内部**，任何一个 paste 只要其生命周期内前台被（下一次点击或目标内部抖动）挪走，`fg != hwnd` 就按 WindowGone 拒绝注入。连点串的中间点击于是全部死在校验闸门上，只有串尾（后面没有更多点击）能走完全程。旧文案「目标窗口已关闭，请重新打开后重试」对这种情况是误报——窗口活得好好的。

**分流语义（D13 红线不变）**：
- `!alive` → WindowGone，文案「目标窗口已关闭」（恢复准确）。
- `alive && fg≠hwnd` → 先问前台是谁（`ForegroundRelation`，按 pid 分类）：OwnProcess / SameAsTarget = 用户意图未变，`activate(hwnd, 100ms, settle=0)` 再断言一次并复检，通过即注入（Converts 本轮日志里 11 次降级中的连点类）；Foreign = 用户已切走，按 ForegroundLost 降级（文案「目标窗口不在前台…切回后 Ctrl+V 即可粘贴」），**绝不抢回前台**。
- 再断言失败也按 ForegroundLost 降级——不无限纠缠，单次代价 ≤100ms。

**观测**：下轮低配机日志看两条：`注入前前台漂移 relation=… 尝试再断言`（出现即说明分流命中）+ 成功率是否从 52% 显著回升；若 Foreign 类频繁出现则另查后台抢占源。

### D45 补充：「连点」归因的证伪与连击合并语义（2026-08-27）

**证伪过程**：D44 曾把失败归因为「用户连点」。用户证词「我明明没有连点」推翻了行为归因——日志里同素材 ~300ms 成对请求（97,97 / 92,92 / 826,826 / 1931×3）不是用户手速，而是**每次交互（一次双击）本身产生多条请求**。查 i-slint-core 1.17.1 `items/input_items.rs` 的 TouchArea Release 分支确认：

```rust
Self::FIELD_OFFSETS.clicked().apply_pin(self).call(&());      // 每次抬笔无条件触发
if (click_count % 2) == 1 {
    Self::FIELD_OFFSETS.double_clicked().apply_pin(self).call(&())  // 追加触发
}
```

`clicked` 与 `double-clicked` **不互斥**。appwindow.slint 的瓦片同时挂两个信号，main.rs 用 `single_click` 设置互斥拦截：单击模式下 `double-clicked` 被拦，但双击第二次抬笔的 `clicked` 照样放行 → 一次双击 = 两次完整 `paste_asset`。粘贴同步阻塞 UI 线程，尾随点击排队到首次完成后立即执行——这正是请求间隔 ~300ms 的构成（双击间隔 ~150ms + 首轮上框 ~170ms）。

**首条必败、尾条必成的机理**：尾随点击的 mouse-down 在 win32k 输入路径上**当场激活本应用**（前台校验读取发生在尾随点击的事件被 app 处理之前），首次上框进行到注入前校验时前台已回到面板 → 按红线拒绝注入；尾随请求随后完整走管线（此时没有更多点击）→ 成功。即：**每次双击的实际结局 = 一次失败提示 + 一次成功**，用户看到「焦点切来切去 + 一半失败」。

**合并语义（TargetRoutingRuntime::paste）**：
- 记录最近一次**实际注入**成功的 (素材路径， 时刻)；注入完成起 750ms 内（OS 双击时值 500ms + 低配机一轮上框 ≤250ms）的同素材请求 = 连击尾随半程 → 直接返回成功语义提示（「已上框到 X（连击已合并）」），不再走管线。Info 日志记录合并（`连击尾随点击并入…`），保证下轮日志可读。
- **降级/失败清空记录**：首次没注入成功，尾随点击必须放行为重试（这是 D42 时代用户「连续重试硬扛」行为的保留通道）。
- **不同素材不合并**：窗口内连点不同瓦片是切换意图，两次都要上框。
- 与 D44 的关系：合并砍掉重复请求后，D44 的再断言负责保住唯一那次上框（mouse-down 抢前台发生在首次注入前校验之前，再断言当场恢复）——若只做 D44 不做合并，双击会**连粘两份**；只做合并不做 D44，首次仍会失败。

**守卫**：target_runtime_spec 三测试——尾随点击合并（注入计数不增 + 文案含「连击已合并」）、降级后立即重试完整走管线（剪贴板写入计数 +1）、窗口内异素材不合并。

### D40 补充：低配机上框失效的根因与概率论证（2026-08-27）

**症状**：低配机点击素材上框经常失败——目标 IM 被反复拉起又失焦，偶发一个裸字母 `v` 落进输入框。

**根因（三层叠加，全部由「事件驱动失效退级到时序驱动」解释）**：

1. **泵启动握手 500ms 超时是吸收态失败**：`spawn_pump` 用 `OnceLock<Option<Arc<PumpInner>>>` 缓存握手结果，低配机冷启动（UI 初始化抢占 CPU，泵线程调度延迟）超时一次，`None` 被永久缓存——全进程 WinEvent 事件驱动就此失效：`set_wakeup` 失败 → main.rs 退到 2000ms Timer 轮询（时序驱动）；`activate` 的前台/输入表面事件等待全部变 `Unavailable`。
2. **失效后的行为链**：`settle` 在 `Unavailable` 下无证据直接跳过等待，注入时机失据；前台确认只剩即时读，第一轮判负后 Alt 敲击兜底几乎必走；**Alt 的 KEYUP 会被前台应用按「菜单模式激活」处理**（焦点闪动/菜单栏高亮 = 用户看到的「失焦」），且 Alt 一旦卡在按下态，目标 IM 被拖进键盘菜单导航。
3. **裸 v 机理**：Ctrl+V 四事件一次 `SendInput` 原子入队，但**注入瞬间若发生前台切换**（焦点竞争期），Ctrl↓ 与 V↓ 落到不同线程的输入队列；Windows 同步键盘状态（GetKeyState）按线程隔离，新线程看到的 Ctrl 是抬起——目标把 V 解释成裸字符。

**决策与落点**：
- 泵状态机（`hooks_installed`/`thread_id`/`last_attempt_ms`）：握手超时 ≠ 失败，泵线程装完自动翻位；真失败冷却 `PUMP_REINSTALL_COOLDOWN_MS`(2s) 后重装；任何 `pump()` 调用（订阅/观察器轮询/Timer 重试）都是自愈入口。
- 观察器订阅惰性建立：构造期不锁死泵状态，泵恢复后自动接上。
- 退路 Timer 每轮先 `install_wakeup` 重试接管，成功即停表——低配机冷启动后最多 ~2s 自动升级回事件驱动。
- Alt 敲击 → Ctrl 敲击（同样满足前台切换的「进程最近有输入」资格，无菜单模式副作用）。
- 注入前检测 Ctrl/Alt 异步键残留（SendInput 部分拒收会把修饰键卡在按下态）并先补 KEYUP 复位；只复位本实现注入过的两个键，不越权碰用户的 Shift/Win。
- `settle` 拿到 `Unavailable` 只打告警**不补时序等待**：`no_timed_waits` 守卫钉死「不得用固定睡眠冒充就绪证据」，该分支的正确出口是泵自愈。

**退级概率论证（目标 ≤0.1%）**：修复前失效是吸收态——首次握手超时概率 p（低配机实测为「经常」），一旦发生即 100% 停在时序驱动。修复后失效不再是吸收态：持续退级要求 `SetWinEventHook` **连续**失败（重试间隔 2s，任意时间窗内独立重试次数海量）；瞬态失败（冷启动负载）在秒级自愈，期间上框链路仍受注入前前台/存活即时校验保护（宁缺勿错，降级为仅复制而非乱注入）。稳态持续退级只剩无桌面会话/权限拒绝这类环境性场景，真机用户桌面不存在。以单次冷启动瞬态失败率 5% 估算，两次独立重试后仍在退级态的概率为 0.25%，三次为 0.0125%——冷却 2s 意味着上框类高频操作在分钟级内就会触发多次自愈入口，**稳态退级概率远低于 0.1% 的要求并有数量级余量**。

### D41 补充：上框延迟的构成与「换方案」评估（2026-08-27）

**症状**：D40 修复后链路可靠，但点击素材→素材出现在 IM 输入框的全过程仍有肉眼可见延迟（低配机尤甚）。

**延迟构成（按确定性排序）**：

1. **[已消除] 物化层预读 PNG**：旧实现在 materialize 时无条件 `fs::read` 旁挂派生 paste.png（几百 KB~几 MB）。缓存仅 4 条 LRU，浏览中换一张图就 miss 一次——低配机机械盘上单次几十~几百 ms，全部发生在 UI 线程。症状特征「时快时慢」与缓存命中与否完全吻合。而 png 在 D22 后是 files 之后的兜底，v1 库内素材 source_path 恒在，**files 永远先命中，预读是 100% 浪费**。pdd/telegram 画像残留的 `["png","files"]` 顺序属 D22 回填遗漏，已一并修正。
2. **[结构性预算，事件驱动下很小] 我们侧的等待**：activate 前台确认（首轮 80ms、兜底轮 120ms cap）+ settle 输入表面（微信 150 / 千牛 120ms cap）+ 锚点单击后 60ms cap。事件健康时各段个位数毫秒（D15 实测：微信激活同毫秒报焦点事件）；低配机事件迟到时最坏等满上限。这些是「最多等」的保守值，是否需要按实测收紧由 D41 的分段瀑布数据决定。
3. **[对端物理下限，不可消除] IM 消化粘贴**：微信对 CF_HDROP 实测进程 CPU 250~312ms（D22），发生在微信进程内——向 shell 要缩略图、读文件、渲染会话卡片。我们的 timing 量不到它，用户体感却包含它。

**「彻底更换底层实现」的评估（结论：不做）**：

- **UIA ValuePattern 直写输入框**：微信 Qt 自绘/千牛 CEF 都不暴露可写输入框元素（D15 已实验否证），等价于不可用。
- **PostMessage WM_PASTE**：只对 Win32 标准 Edit 控件有效，IM 自绘输入框不响应该消息。
- **模拟 OLE 拖放（IDropTarget）**：比粘贴语义更重、对端处理更慢，且实现复杂度上一个数量级。
- **换任何注入方式都改变不了第 3 项**：端到端 = 我们侧（目标 <100ms，事件驱动健康时可到几十 ms）+ 对端消化（图片/视频 250ms+）。「全流程百毫秒以内」对文本类素材可达（无文件 IO、对端处理快）；对图片/视频被对端进程卡在 ~300ms 量级，这是系统剪贴板交互模型的固有成本，不是我们的实现问题。

**后续观测方式**：低配机正常运行即产出 `logs/app-*.log` 的「上框耗时分布」瀑布（Info 级）；设置面板开「细粒度诊断日志」后追加 activate 轮次、settle/anchor 结局、focus 级别（Debug 级）。一轮数据即可判定剩余延迟在「我们侧等待（收紧 cap）」还是「对端消化（无解）」，以及是否值得把 paste 链路挪出 UI 线程（消除面板卡顿观感，不缩短端到端）。

### D42 补充：解绑竞态的日志证据与自愈语义（2026-08-28）

**日志证据**（低配机 app-18212，2026-08-27 14:59~15:06，92 次上框）：

- 27 次降级全部是「目标窗口已关闭」（WindowGone），而微信窗口始终开着——失败率约 30%；
- 失败呈「风暴」形态：15:00:08~15:00:14 连续 7 连败后恢复，恢复间隔 2~3.5s，与退路 Timer 的 2s 轮询节奏吻合（事件驱动健康时，用户点击面板本身就会触发前台事件→立即 poll+重绑，恢复应为亚秒级）；
- 用户以 0.7~1.2s 间隔连续重试同一素材，失败后重试常直接成功——绑定状态在两次请求之间被轮询修复，这正是「等轮询」的浪费。

**机理推断**：窗口枚举的快照竞态。`snapshot_window` 对 `GetWindowRect` 失败或无面积的窗口直接跳过（过滤通知窗的同一把刀），微信窗口处于最小化/恢复动画或标题栏短暂异常时从快照消失 → `refresh_windows` 判定热目标窗口消失 → `on_window_gone` 解绑。下一次上框在重绑前的窗口期到达 → 降级 WindowGone。

**自愈语义**：`TargetRoutingRuntime::paste` 入口检测「热目标身份存在但 hwnd=None」→ 强制一次全枚举（成功路径零成本，仅解绑态付出）→ 同实例唯一窗口则当次请求内重绑 → 上框继续。D13 的「None=休眠而非死亡」语义不变，这里只是把重绑时机从「下一轮轮询」提前到「第一次被需要的时刻」。

**守卫**：`paste_heals_dormant_hot_target_by_forced_rebind`（编排枚举序列：锁定 → 消失解绑 → paste 当场重绑并注入成功；同时断言自愈恰好触发一次全枚举）。




### D43 壳层缩略图驻留纪律与渐进装载（2026-08-27）

**症状（实测）**：裸启动 RSS < 100MB 达标；但左侧分类每切一次，可见缩略图解码后常驻只增不减——切换过的分类越多驻留越大；首次切换某分类的那一帧明显卡顿，「后续切换更快了」是暖缓存假象，代价是内存持续膨胀。

**根因（两处叠加）**：

1. **切换卡顿**：push_tiles 在一次 set_vec 里对整个可见窗口同步 Image::load_from_path（Slint 内部即 image::open 完整解码，i-slint-core 1.17.1 graphics/image.rs）——首切分类 = UI 线程单帧几十次 PNG 解码 + 全批纹理首传。
2. **驻留只涨不降**：壳层 HashMap<u32, slint::Image> 无界、仅库重载时 clear；解码 RGBA 缓冲（320px 上限 ≈ ≤400KB/张）被强引用永久钉住。grid_vm::ensure_window 的窗外显式驱逐纪律只落在 VM 的字节 LRU 上，而生产路径从未调过 set_provider——VM 缓存恒空，真正的图缓存逃过了 D10 纪律。这是架构错位：D10 纪律写在 VM 层，而持图的层在壳层。

**决策**：

- **窗口纪律收口到壳层**：每次 sync 以当前物化窗口 id 集合 retain_window 显式驱逐窗外条目；LRU 容量兜底 = grid_vm::MAX_VISIBLE（≥ 单窗上限，容量驱逐永不与窗口驱逐打架）。驻留上界从「浏览过的分类总量」收敛到「单窗 + Slint 自身 5MB path 解码缓存」——切过的分类再多，常驻不再增长。
- **装载节奏预算化**：每 pass 解码 ≤6 张（THUMB_LOAD_BUDGET），余缺由 SingleShot Timer 按 16ms 续跑补齐，已装行走 set_row_data 定向更新（不整表重建模型）。分类切换首帧即出布局/色块/文字，缩略图按帧浮现；单帧成本从 O(整窗解码) 降为 O(6 张)。Slint 5MB path 缓存让「来回切」仍有暖命中。
- **负缓存**：缩略图文件缺失条目以 Image::default() 入缓存，缺图瓦片不再每个 pass 重试读盘；库重载/清库路径本就 clear，派生完成后自动重试。
- **边界申明**：这里是「显示装载」已派生的 320px 浏览缩略图——Slint 渲染器首次渲染本就要做的解码，只是分期执行；缩略图生成 / 原始媒体解码仍全部在 worker 进程（D11 红线不变）。
- **守卫**：--bench 增加 D43 驻留守卫——逐窗装载 + 窗口驱逐扫过整库后 thumb_cache_entries ≤ 单窗，违者退出码 5；ThumbCache 的容量驱逐/触碰/窗口驱逐有单元测试。
- **附带修复**：clear_library 曾用全新 VecModel 顶替 UI 的 tiles 模型，而后续回调仍写旧模型——清库后重新导入的素材永远不显示。顶替行删除（sync 空窗本就立即清屏）。

**落点**：crates/app-ui/src/thumbs.rs（ThumbCache + GridCtx + build_rows）、app-ui/main.rs（8 处调用点统一走 grid.sync()）、run_bench（D43 守卫）。

### D39 补充：日志缺省目录与粘贴 trace 收编约定（2026-08-28）

**缺省目录（永不回落 cwd）**：`init_from_env` 的目录解析固定为 `DSH_LOG_DIR` > 调用方 `fallback_dir` > 平台标准目录 `%LOCALAPPDATA%\asset-manager\logs`（`USERPROFILE\AppData\Local` → 系统临时目录逐级兜底），任何分支都不是当前工作目录——cargo test 的 cwd 是包根、任意宿主直跑的 cwd 不可控，实测 decode-worker 日志曾落进 crates/worker/ 源码树。桌面主进程不变（init 用 exe 同目录 logs/ 的便携约定）。`WorkerPool` 新增 `with_priority_and_log_dir`：每个 worker（含替补拉起）spawn 时显式注入 `DSH_LOG_DIR`，不依赖环境继承；worker 集成测试经 `with_test_pool` 钉死进程专属临时目录，源码树零日志污染。顺带：log 0.4.33 起默认 features 为空，crates/logging 显式声明 `features=["std"]`（`set_boxed_logger` 门在 std 后，单独 `-p logging` 构建时没有其它成员帮忙开特性）。

**粘贴 trace 收编**：三处 env 门控 eprintln（PASTE_PIPELINE_TRACE / PASTE_LATENCY_TRACE / PASTE_EVENT_TRACE）收编为 log facade，统一 `paste_trace` target 域——`paste_trace::pipeline`（注入前快照，Debug）与 `paste_trace::platform::events`（WinEvent 抽干明细 buffered/live hit/miss，Trace）。settle 处的 PASTE_LATENCY_TRACE 与既有 debug 行同点同数据，直接删除。默认 Info 下 log 宏退化为一次原子读（零格式化开销）；开关统一走 D39 的 verbose_diagnostics / DSH_LOG_LEVEL，真机 GUI 无控制台时 eprintln 本就不可见，收编后低配机日志才真正可回溯（grep `paste_trace` 即得完整上框故事）。

**落点**：crates/logging（platform_default_dir + log/std）、crates/worker（with_priority_and_log_dir + pool_spec）、crates/pipeline、crates/platform。

### 延迟决策（已记录，不做）

- objects 二级分片（objects/{uuid[0:2]}/{uuid}/raw.ext）：方向正确（thumbs 已分片），但改布局会让既有库的 rel_path/paste.png 定位漂移，需迁移脚本；v1 在百万级以内先保持现状，随内存/目录监控数据再定 v1.1 迁移时机。

### 已知边界

- 窗口 chrome（标题栏）明暗跟随系统，不随应用内主题切换；内容区自绘层与 std-widgets 已由 D37 打通全量切换。
- Filter::NameContains 为内存线性扫描（百万级每键一次；高吞吐场景走 FTS5，SearchProvider 同一入口；D52 已定本批接混合路由，落地后本条收窄为短查询路径）。
- SoA remove 留行孔（v1 删除路径是整库重建/清库，重载后回收；D46 回收站走 tombstone 位图过滤，不改此边界）。

---

## 六、产品功能扩展批次（2026-08-28，grill-with-docs 拷问会）

> 背景：MVP 闭环与低配机稳定性修复（D40–D45）收尾后，用户提出 7 项待优化方向，经三轮拷问收敛为两批。
> **批 1 动工顺序：CRUD（回收站/多选/右键菜单）→ 通用导入+归类弹窗 → 搜索（范围+混合路由）→ 动画修复+瀑布流底边补齐。**
> 批 2 预留：皮肤（D55）、更新检测（D56）、瀑布流方向感知预取。领域词汇表见根目录 `CONTEXT.md`。

| # | 决策 | 状态 |
|---|---|---|
| D46 | 删除 = 库内回收站（软删除+恢复+彻底删除），无自动过期清理 | ✅ |
| D47 | 多选 = Ctrl/Shift/Ctrl+A + 显式「多选模式」（免按键连选，期间上框屏蔽）；框选推迟 | ✅ |
| D48 | 右键菜单五项（复制/移动到分类/重命名/属性/删除）；批量归类进本批 | ✅ |
| D49 | 通用导入 = 文件对话框混选（素材+.emo）+ 保留文件夹入口 + 窗口拖拽；Slint 拖放能力先 spike | ✅ ⚠️ |
| D50 | 导入归类弹窗：所有入口统一、每批一次、选项按来源条件化、可记忆；D5 首次真正落地 | ✅ |
| D51 | 搜索范围 = 搜索框前缀下拉（全部/文件名/分类/标签）；大小写统一不敏感 | ✅ |
| D52 | 搜索混合路由：≥3 字符走 FTS5、短查询回落内存扫描（D30 的「后续接」提前为本批） | ✅ |
| D53 | 动画修复包：入场 init 技巧无效（根因）→ 下一帧翻转；出场两段式；瓦片淡入 | ✅ |
| D54 | 瀑布流底边漏补：fill 停表加「几何稳定」判据 | ✅ |
| D55 | 皮肤 = 字号/间距/Rust 侧色值 token 补全 + 外置 TOML 主题包；std-widgets 映射边界 | 📋 批 2 |
| D56 | 更新检测 = WinHTTP 零新依赖 + 静默检查（≥24h 可关）+ GitHub→镜像顺序回落 + 开浏览器 | 📋 批 2 |

### D46 删除语义：库内回收站（2026-08-28）

**内容**：删除单张/多张素材 = 移入库内回收站（trash 目录 + tombstone 标记）；回收站内容不占浏览/搜索结果；支持「恢复」（重进索引）与「彻底删除」（索引 + meta.db + objects/ + thumbs/ 全清）；「清空回收站」为手动动作，**不做自动过期清理**。

**理由**：素材库是用户多年沉淀，误删代价高；OS 回收站方案与复制入库模型冲突（objects 内部路径无独立语义）。

**连带**：索引层 SoA remove 留行孔的已知边界不变——回收站用 tombstone 位图过滤，不触发行回收；恢复 = 重进索引。

**落点**：store（tombstone）、library、ui-viewmodels、appwindow.slint。

### D47 多选机制与显式多选模式（2026-08-28）

**内容**：修饰键多选（Ctrl 点选切换 / Shift 范围 / Ctrl+A 全选）+ 顶栏「选择」按钮进入的**多选模式**：期间单击 = 选中/取消（不再触发上框）、双击无操作，底部浮出操作条「已选 N 张｜全选｜移动到分类｜删除｜取消」；退出（再点/Esc/取消）即清空选区恢复常态。

**红线**：多选模式期间 D13 双击上框**完全屏蔽**——模式存在的意义。

**推迟**：橡皮筋框选（Flickable 滚动手势冲突 + 自绘 overlay 成本），记录为已知边界。

### D48 右键菜单与批量归类（2026-08-28）

**内容**：瓦片右键菜单五项——复制 / 移动到分类 / 重命名 / 属性（尺寸·大小·导入时间·路径）/ 删除。此前全库无任何右键菜单、无重命名/移动分类 UI。批量归类 = 多选 + 「移动到分类」，是 A4（D5 积压缓解）的第一半。

**后续计划**：编辑标签（动工前先查证标签写回层级）、大图预览浮层。

### D49 通用导入：混选 + 拖拽（2026-08-28）

**内容**：导入入口统一——文件对话框支持多选（素材文件与 .emo 混选），保留「导入文件夹」入口，新增**窗口拖拽导入**（文件或文件夹丢到窗口任意位置即导入）。单文件导入 = 收集层小改；管线层 D24 包注册表已就绪。

**⚠️ 前置 spike**：Slint 1.17 对原生文件拖入（OS 级 drag-drop 取路径）的支持未查证；若无现成支持则 platform 层 `RegisterDragDrop` 兜底（D16 装配边界不变）。

### D50 导入归类弹窗（2026-08-28）

**内容**：所有导入入口统一弹、每批一次；「取消」= 放弃本次导入（零文件进库）。选项按来源条件化，默认项恒为该来源最合理的解释，回车即走默认：

- .emo / 千牛结构目录：**按包内分类**（默认，groupName，标注「含 N 个分类」）/ 统一归入 ▼ / 放入待分类
- 普通文件夹：**按文件夹名归类**（默认）/ 统一归入 ▼ / 放入待分类
- 零散文件：**统一归入 ▼**（默认选中「待分类」）/ 放入待分类

「统一归入 ▼」= 已有分类下拉 + 输入即新建。☐「记住我的选择，不再询问」：记住方式；方式为统一归入时连分类一并记住；设置面板可恢复询问。

**意义**：D5「导入时手动分类」自 2026-08-21 定盘以来首次真正落地（此前 RuleChain 静默自动归类），是「待分类收件箱积压死亡」已记录风险的主缓解。

### D51 搜索范围与大小写统一（2026-08-28）

**内容**：搜索框前缀下拉（全部 / 文件名 / 分类 / 标签）。范围语义 = 本次查询跑哪些过滤器——收窄范围兼得省资源与结果可预期。修复现状不一致：分类/标签匹配区分大小写、文件名不区分——统一为全部大小写不敏感。备注搜索等 notes 字段落地后再议。

### D52 搜索混合路由：FTS5 本批接线（2026-08-28，修订 D30 排期）

**根因数据**：现状每次按键对全库文件名逐个 `to_lowercase().contains()`（index/src/lib.rs:136，每名每次按键分配一个新字符串）：老设备 10 万条约 10–30ms/键、逼近百万级 100ms+/键不可用；低配机用户真实存在（D40–D45 全部来自低配机实测日志）。

**决策**：≥3 字符查询 → `Store::search`（FTS5 trigram；触发器自建库起自动维护索引，本次为查询侧首次接线）；1–2 字符 → 内存扫描回落。同走 D30 的 SearchProvider 单一入口。

**限制归属**：≥3 字符是 SQLite FTS5 **trigram 分词器层**的固有机制（3 字窗口倒排索引，短查询无索引键），不可去除——换分词器（unicode61）会毁掉中文子串搜索（中文无分词边界）。混合路由将其对用户隐藏。

**连带**：①结果 uuid→行号映射须内存安全（D10），实现时定方案；②内存扫描侧治理每键字符串分配；③trigram 默认 ASCII 折叠，与内存扫描 Unicode 小写的语义差异记为边界（中文+ASCII 文件名无感知）。

### D53 动画修复包（2026-08-28）

**根因（用户体感「已实现动画没生效」成立）**：三处弹层入场动画用「init 置 shown」技巧（appwindow.slint:821/927/973），但 Slint 的 init 回调跑在**首帧渲染之前**——opacity 从未以 0 被渲染，元素直接以终态出现，150ms 过渡形同虚设。`ui_animations` 默认开（settings.rs:126），非设置问题。

**修复四项**：①入场 = 挂载后下一帧（16ms Timer）翻 shown，首帧以透明态出现再过渡；②出场 = 两段式延迟销毁（先翻出播 150ms，Timer 到点真销毁）；③瓦片缩略图 150ms 淡入（配合 D43 渐进装载，「闪烁」→「浮现」）；④全部受既有 ui_animations 开关钳制（关 = 0ms 直达）。

### D54 瀑布流底边漏补修复（2026-08-28）

**症状（用户实测）**：底部数张瓦片要手动再滚动一下才显示；有时已到底无法继续滚动，永不触发显示。

**机理**：缩略图填充链停表条件 =「当前窗口缺图补完」（thumbs.rs fill_pass）；触底时 Flickable 惯性回弹期间 content_y 仍在动画，最后一次填充按回弹**中间位置**算窗口；静止后不再有滚动事件 → 无 sync → 漏补。「手动再滚一下」= 用户手动制造了一次 sync 事件。

**修复**：fill 停表条件加「几何稳定」判据——本轮与上轮滚动位置一致才允许停表；静止后至多多跑一轮空 pass，成本可忽略。

### D55 皮肤：token 补全 + 外置主题包（📋 批 2，2026-08-28 定盘）

**内容**：先补 token——字号（10/11/12/13/14px 内联字面量）与间距 token 化 + Rust 侧 12 处内联色值收口（thumbs 兜底色 8 + 健康色 4）；皮肤 = 外置 TOML 主题包（`themes/` 目录即放即用），内置 Dark/Light 随版本发布。

**边界**：std-widgets 内部配色只认 Palette Light/Dark 两态（D37）——自定义主题映射到最接近的明暗档，如实记边界。

**词汇**：规范用语为「主题包 / 主题 token」（见 CONTEXT.md），弃用「皮肤」。

### D56 更新检测（📋 批 2，2026-08-28 定盘）

**网络栈**：WinHTTP（platform 已有 windows-rs 生态，**零新依赖**，schannel 出 TLS；ureq+rustls 为备选）。独立线程 + 超时，UI 线程零阻塞。

**触发**：启动后静默检查（≥24h 间隔，设置可关）+ 设置面板手动按钮。

**源策略**：GitHub API 主源 → 镜像**顺序回落**（带超时）；镜像做成可配置 TOML 列表，实测哪个可用再定默认。不并发竞速——未认证 GitHub API 限 60 次/时，竞速白烧配额且镜像数据可能滞后。

**命中动作**：弹窗（新版本号 + release notes）→「打开发布页」开浏览器。**应用内下载安装明确推迟**（签名校验/安装器自更新是独立大项目）。版本比较 semver 对当前 0.1.0。

### 批次归档：明确不做 / 推迟清单（2026-08-28）

- 框选橡皮筋（见 D47）；编辑标签、大图预览浮层（见 D48）；备注（notes）字段与备注搜索
- 瀑布流方向感知预取 + 空闲预算提升（症状驱动；D54 落地后观察再定）
- 「来源建议分类 + 单键确认」（A4 的另一半）
- 应用内更新下载安装（见 D56）

