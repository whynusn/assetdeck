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

2026-08-29 用户裁定收窄「未运行」态的适用范围：它只属于**用户捕捉过的目标**——热/钉住目标休眠（窗口关到托盘）时置灰保留，兑现 D13 的「休眠目标置灰保留不消失」。从未上过框的内置画像一律不入候选列表（`TargetRoutingVm::refresh_windows` 整条跳过）；否则没装 Telegram 的机器上 picker 永远躺着一个灰色 Telegram，内置名单看起来就是硬编码广告位。

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
| D56 | 更新检测 = WinHTTP 零新依赖 + 静默检查（≥24h 可关）+ GitHub→镜像顺序回落 + 开浏览器 | ✅（2026-08-29 落地，见 D56 落地记） |

### D46 删除语义：库内回收站（2026-08-28）

**内容**：删除单张/多张素材 = 移入库内回收站（trash 目录 + tombstone 标记）；回收站内容不占浏览/搜索结果；支持「恢复」（重进索引）与「彻底删除」（索引 + meta.db + objects/ + thumbs/ 全清）；「清空回收站」为手动动作，**不做自动过期清理**。

**理由**：素材库是用户多年沉淀，误删代价高；OS 回收站方案与复制入库模型冲突（objects 内部路径无独立语义）。

**连带**：索引层 SoA remove 留行孔的已知边界不变——回收站用 tombstone 位图过滤，不触发行回收；恢复 = 重进索引。

**落点**：store（tombstone）、library、ui-viewmodels、appwindow.slint。

**落地（2026-08-28，批1-crud）**：store schema v4（deleted 列 + 迁移）；library `trash.rs`（move/restore/purge/empty/reconcile，open 时自动纠偏）；index `deleted: RoaringBitmap`「占号不显形」（墓碑行保留行号槽位，uuid→行二分不错位，见测试契约）；UI 侧 `Filter::Trash` 走侧栏 -3 哨兵入口 + 角标；库写全部走 `sample-library --cmd trash|restore|purge|empty-trash|rename|move-category` 子命令（单写者纪律，deps_guard 禁 app-ui 依赖 library）。删除反馈 UI 先行（本地 hide_locally），子命令收尾整库重载对齐，失败行显形回来 + 错误上通知条。

### D47 多选机制与显式多选模式（2026-08-28）

**内容**：修饰键多选（Ctrl 点选切换 / Shift 范围 / Ctrl+A 全选）+ 顶栏「选择」按钮进入的**多选模式**：期间单击 = 选中/取消（不再触发上框）、双击无操作，底部浮出操作条「已选 N 张｜全选｜移动到分类｜删除｜取消」；退出（再点/Esc/取消）即清空选区恢复常态。

**红线**：多选模式期间 D13 双击上框**完全屏蔽**——模式存在的意义。

**推迟**：橡皮筋框选（Flickable 滚动手势冲突 + 自绘 overlay 成本），记录为已知边界。

**落地（2026-08-28）**：`ui-viewmodels::selection` 状态机（锚点语义对齐 Explorer：无修饰单击移锚点、Ctrl 切换不移锚点、Shift 范围替换、Ctrl+Shift 范围并集；过滤变化选区自动修剪陈旧成员）；瓦片 `pointer-event` 修饰键拆件（spike S1 源码查证：up 事件 `modifiers` 取窗口全局键态可靠；带修饰的按下会抑制 clicked/double-clicked——mod-on-down 标志）；Ctrl+A/Esc 走根 FocusScope capture 通道（检索框持焦时让位）；多选模式期间上框链路连 active_asset_id 都不留（paste_asset 入口级红线 A）。

### D48 右键菜单与批量归类（2026-08-28）

**内容**：瓦片右键菜单五项——复制 / 移动到分类 / 重命名 / 属性（尺寸·大小·导入时间·路径）/ 删除。此前全库无任何右键菜单、无重命名/移动分类 UI。批量归类 = 多选 + 「移动到分类」，是 A4（D5 积压缓解）的第一半。

**后续计划**：编辑标签（动工前先查证标签写回层级）、大图预览浮层。

**落地（2026-08-28）**：菜单五项常量入 VM 层（穷举测试锁文案）；目标集 = 命中在选区内则整份选区、否则收窄到命中张。「复制」为独立缝合层 `copy_to_clipboard`（negotiate_detailed 后只写剪贴板，绝不 activate/注入——与上框链路共享格式协商、隔离注入面）；重命名/归类校验在壳层（Slint 无 trim 内建，spike 结论）；属性弹窗四字段 + explorer /select 定位。

**实测事故与修正（2026-08-29，用户真机冒烟发现）**：①「移动/重命名」卡退、「删除」后回收站空——同一根因：`apply_filter` 实参里内联 `filter_label.borrow().clone()`，Ref 卫队活到整条语句结束、横跨调用体内的 `borrow_mut` → BorrowMutError。移动/重命名在子命令**收尾重载**时炸（写入已落库，重启后行为“已生效”）；删除在**起子进程之前**炸（素材从未进库，用户视角=「回收站没加上」）。修正 = label 先落本地再传 + deps_guard 源码扫描守卫（apply_filter 实参禁 `.borrow()`）。②右键菜单曾迁内建 `ContextMenuArea`（skill 语义要求），**实测回退**：Slint 1.17.1 Windows 上瓦片内 TouchArea 对任何按钮按下都 `GrabMouse`（core items/input_items.rs，press 事件只投 top item），ContextMenuArea 的内建右键捕获被短路；`show()` 程序化唤出实测同样无效（debug 探针未触达 Up 分支，疑似 winit→slint 右键事件链在 grab 之后断裂）。手搓浮层回退（用户第一轮真机截图实测可弹），`ContextMenuArea` 迁移记为 Slint 升级后的待办。③操作条标签回归计数本位（「回收站 · 已选 N 项」），教学句移交回收站空态视图（长句曾把「清空回收站」按钮挤出 560px 条外）；文案 elide 兜底；张/项统一为「项」。

### D49 通用导入：混选 + 拖拽（2026-08-28）

**内容**：导入入口统一——文件对话框支持多选（素材文件与 .emo 混选），保留「导入文件夹」入口，新增**窗口拖拽导入**（文件或文件夹丢到窗口任意位置即导入）。单文件导入 = 收集层小改；管线层 D24 包注册表已就绪。

**⚠️ 前置 spike**：Slint 1.17 对原生文件拖入（OS 级 drag-drop 取路径）的支持未查证；若无现成支持则 platform 层 `RegisterDragDrop` 兜底（D16 装配边界不变）。

**连带修订（2026-08-29，用户实测）**：D37 的 `gpu_rendering` **默认值翻转为 false（软件渲染）**——femtovg 在窗口 resize 时逐帧重建渲染面，拖拽尺寸明显卡顿（用户真机实测），而目标用户正是低配机（D40–D45）；软件档渲染完整（software-renderer-path 已修图标）。另查明用户所报「切换无效」直接根因：安装版 exe 为 1d5ab6a（程序化 selector）之前的旧构建，旧代码连写名 `winit-femtovg` 静默回落默认渲染器——新代码 backend_name+renderer_name 分传已修，重启后切换生效。

**落地（2026-08-28，批1-import）**：spike S1 源码查证定论——Slint 1.17.1 **无 OS 文件拖入**（DragArea/DropArea 仅应用内 DnD；winit 后端对 `WindowEvent::DroppedFile` 零处理；Slint 自身未注册 drop 处理器，故无共存冲突），走兜底路线：platform `win32::dragdrop` 模块 `#[implement(IDropTarget)]` + `RegisterDragDrop`（Drop 取 CF_HDROP → DragQueryFileW 路径 → FileDropSink trait），OleInitialize 幂等；HWND 经 slint 的 raw-window-handle-06 提取（deps_guard 白名单扩一项，纯 trait 定义零逻辑）。主导入按钮升级为 `pick_open_files` 多选混选（FOS_ALLOWMULTISELECT），三入口全部汇流归类弹窗（R2 保留原入口语义）。

**真机验收两连修（2026-08-29，用户报「拖拽导入没实现」）**：落地后真机上拖拽是无声无息的 no-op，两处独立缺陷叠加：
1. **HWND 就绪退避从未起跑**：`register_file_drop_when_ready` 用局部 `slint::Timer::default().start(...)` 排重试——slint `Timer` 的 `Drop` 会把定时器从 `CURRENT_TIMERS` 摘除，临时值一离语句即被取消，重试链是死代码；事件循环首轮 winit 窗口未建 → hwnd 恒 0 → 注册从未发生。修复：改 `slint::Timer::single_shot`（静态，不依赖存活对象），挂载点随后重构为 WinEvent 钩子事件驱动（`mount_when_window_ready`）。教训与 D40 低配冷启动退路同款：**凡依赖延迟触发的回调，Timer 实例必须静态存活**。
2. **HDROP 路径末字符被吃**：`extract_hdrop_paths` 按 `DragQueryFileW` 首调返回的长度 `len`（**不含结尾 NUL**）分配缓冲，二次调用时 API 连 NUL 写不下，实际写入 `len-1` 字符 + NUL——`walrus.jpg` 变 `walrus.jp`，扩展名 `jp` 查无此类型 → `is_importable=false` → `plan_groups` 全过滤返回空表 → `finalize` 静默早退（R4 设计：不支持类型静默跳过）。修复：缓冲区 `len+1`。**教训：静默过滤路径必须可观测**——finalize 空表现打 WARN，Drop 送达/UI 回调/finalize 分支/classify_open 置位进 Info 日志（对齐 D41 分段计时纪律）。

### D50 导入归类弹窗（2026-08-28）

**内容**：所有导入入口统一弹、每批一次；「取消」= 放弃本次导入（零文件进库）。选项按来源条件化，默认项恒为该来源最合理的解释，回车即走默认：

- .emo / 千牛结构目录：**按包内分类**（默认，groupName，标注「含 N 个分类」）/ 统一归入 ▼ / 放入待分类
- 普通文件夹：**按文件夹名归类**（默认）/ 统一归入 ▼ / 放入待分类
- 零散文件：**统一归入 ▼**（默认选中「待分类」）/ 放入待分类

「统一归入 ▼」= 已有分类下拉 + 输入即新建。☐「记住我的选择，不再询问」：记住方式；方式为统一归入时连分类一并记住；设置面板可恢复询问。

**意义**：D5「导入时手动分类」自 2026-08-21 定盘以来首次真正落地（此前 RuleChain 静默自动归类），是「待分类收件箱积压死亡」已记录风险的主缓解。

**落地（2026-08-28）**：弹窗 = ui-viewmodels `classify.rs` 纯函数（D50 选项表穷举测试锁定；混选按来源分桶、N 标注多包求和；散文件按 media 注册表过滤=R4 语义）；「统一归入」控件 = LineEdit + ComboBox 组合（spike S2：ComboBox 无自由文本输入）；N 预扫描走 `sample-library --probe-categories` 子进程（.emo 只读 zip 中央目录不解压，千牛目录与读取器同判定数 groupName——C2 零解码）；每批决策以 `--import-paths` 逐行指令（kind<HT>mode<HT>path，auto/inbox/category:名）一次子进程跑完（修订 design §1.2：全局 override 会覆盖 .emo 包内分类，弃用）；R8 记忆 = AppSettings `ask_classify_on_import`（默认询问）+ 三组 mode/category 字段，全组有记忆则跳过弹窗直通，设置面板恢复询问；取消路径零副作用（空清单不建库）；弹窗动效出生即正确（下一帧翻转入场 + 两段式出场，motion 任务后续回改旧三处）。

### D51 搜索范围与大小写统一（2026-08-28）

**内容**：搜索框前缀下拉（全部 / 文件名 / 分类 / 标签）。范围语义 = 本次查询跑哪些过滤器——收窄范围兼得省资源与结果可预期。修复现状不一致：分类/标签匹配区分大小写、文件名不区分——统一为全部大小写不敏感。备注搜索等 notes 字段落地后再议。

**落地（2026-08-28，批1-search）**：LibraryFacets 拆出 `category_matches`/`tag_matches`（scope 互斥子句集的数据源）；大小写统一 = 共享 `domain::text` 折叠工具（needle 一次折叠 + haystack 环形窗口流式比对，双侧同 `char::to_lowercase`，İ 展开对称，中文恒等）；搜索框前缀 chip 下拉四档（UiEnums 收口 int，D32 纪律），壳层缓存 current_query/current_scope，切档即时重跑；顶栏后缀带范围（「搜索「X」· 仅文件名」）。守卫：真库 fixture 软删后四档 × 两路查询均不含已删行。

### D52 搜索混合路由：FTS5 本批接线（2026-08-28，修订 D30 排期）

**根因数据**：现状每次按键对全库文件名逐个 `to_lowercase().contains()`（index/src/lib.rs:136，每名每次按键分配一个新字符串）：老设备 10 万条约 10–30ms/键、逼近百万级 100ms+/键不可用；低配机用户真实存在（D40–D45 全部来自低配机实测日志）。

**决策**：≥3 字符查询 → `Store::search`（FTS5 trigram；触发器自建库起自动维护索引，本次为查询侧首次接线）；1–2 字符 → 内存扫描回落。同走 D30 的 SearchProvider 单一入口。

**限制归属**：≥3 字符是 SQLite FTS5 **trigram 分词器层**的固有机制（3 字窗口倒排索引，短查询无索引键），不可去除——换分词器（unicode61）会毁掉中文子串搜索（中文无分词边界）。混合路由将其对用户隐藏。

**连带**：①结果 uuid→行号映射须内存安全（D10），实现时定方案；②内存扫描侧治理每键字符串分配；③trigram 默认 ASCII 折叠，与内存扫描 Unicode 小写的语义差异记为边界（中文+ASCII 文件名无感知）。

**落地（2026-08-28）**：①映射 = `RealAssetResolver::uuid_rank`（升序 uuids 二分，零克隆；升序不变量有守卫测试，`for_each_asset*` 的 `ORDER BY uuid` 是契约前提）；②内存路改 `domain::text::contains_case_fold` 环形窗口无分配滑窗（百万行预算测试不退红）；③混合路由 = `HybridSearchProvider`：≥3 字符 + FTS 源 → `Filter::NameIn(行号白名单)`（D4 纯声明，求值与活集求交——FTS 行不随软删移除），FTS 失败降级内存路不断视图，短查询/无库恒内存路；oracle 一致性测试锁定 NameIn == NameContains（同库同查询，trigram 下限用例显式覆盖）。bench 查询延迟分位数场景暂缓：bench-harness 有并发会话在途 WIP（未提交、暂不编译），待其收口后补 `research/latency-ledger.md` 新节。

### D53 动画修复包（2026-08-28）

**根因（用户体感「已实现动画没生效」成立）**：三处弹层入场动画用「init 置 shown」技巧（appwindow.slint:821/927/973），但 Slint 的 init 回调跑在**首帧渲染之前**——opacity 从未以 0 被渲染，元素直接以终态出现，150ms 过渡形同虚设。`ui_animations` 默认开（settings.rs:126），非设置问题。

**修复四项**：①入场 = 挂载后下一帧（16ms Timer）翻 shown，首帧以透明态出现再过渡；②出场 = 两段式延迟销毁（先翻出播 150ms，Timer 到点真销毁）；③瓦片缩略图 150ms 淡入（配合 D43 渐进装载，「闪烁」→「浮现」）；④全部受既有 ui_animations 开关钳制（关 = 0ms 直达）。


**落地（2026-08-28，批1-motion）**：旧三弹层（目标下拉/导入菜单/设置面板）入场修复 = init 只报数（`overlay-mounted(which)` 回调），翻转交给 16ms 单发 Timer（必然落在首帧后）；关动画直达（时长钳 0ms）；卸载点重置 shown 防重挂载残留跳过入场。**走查差异（2.2）**：旧三处只修入场、出场维持即时卸载——目标下拉的关闭由轮询驱动的 `sync_target_bar` 拍板，两段式卸载会与轮询状态机竞态；新弹层（归类弹窗/范围下拉）已是全两段式。瓦片淡入 = TileData `thumb-fade`（缓存命中/新装出=true，缺图/负缓存=false），Slint 侧 opacity animate；挂载即命中初值就是 1 不重播，fill 补齐 false→true 翻转播 150ms——与 D43 渐进装载天然吻合。

### D54 瀑布流底边漏补修复（2026-08-28）

**症状（用户实测）**：底部数张瓦片要手动再滚动一下才显示；有时已到底无法继续滚动，永不触发显示。

**机理**：缩略图填充链停表条件 =「当前窗口缺图补完」（thumbs.rs fill_pass）；触底时 Flickable 惯性回弹期间 content_y 仍在动画，最后一次填充按回弹**中间位置**算窗口；静止后不再有滚动事件 → 无 sync → 漏补。「手动再滚一下」= 用户手动制造了一次 sync 事件。

**修复**：fill 停表条件加「几何稳定」判据——本轮与上轮滚动位置一致才允许停表；静止后至多多跑一轮空 pass，成本可忽略。

**落地（2026-08-28，批1-motion）**：纯函数 `fill_should_stop(missing, y_new, y_last)`（±0.5px 阈值，NaN 首轮判据恒假保底续跑）+ GridCtx `last_fill_y`；停表 = `missing==0 && stable`，静止后最终一轮空 pass 收敛。表驱动单测 7 组。顺带修 e2e 假阳性根因：平坦灰度图 pHash 全同值（DCT 低频与亮度均值无关），纯色 fixture 触发与并发调度相关的导入去重——测试图改 x/y 梯度结构。


### D55 皮肤：token 补全 + 外置主题包（📋 批 2，2026-08-28 定盘）

**内容**：先补 token——字号（10/11/12/13/14px 内联字面量）与间距 token 化 + Rust 侧 12 处内联色值收口（thumbs 兜底色 8 + 健康色 4）；皮肤 = 外置 TOML 主题包（`themes/` 目录即放即用），内置 Dark/Light 随版本发布。

**边界**：std-widgets 内部配色只认 Palette Light/Dark 两态（D37）——自定义主题映射到最接近的明暗档，如实记边界。

**词汇**：规范用语为「主题包 / 主题 token」（见 CONTEXT.md），弃用「皮肤」。

### D56 更新检测（📋 批 2，2026-08-28 定盘）

**网络栈**：WinHTTP（platform 已有 windows-rs 生态，**零新依赖**，schannel 出 TLS；ureq+rustls 为备选）。独立线程 + 超时，UI 线程零阻塞。

**触发**：启动后静默检查（≥24h 间隔，设置可关）+ 设置面板手动按钮。

**源策略**：GitHub API 主源 → 镜像**顺序回落**（带超时）；镜像做成可配置 TOML 列表，实测哪个可用再定默认。不并发竞速——未认证 GitHub API 限 60 次/时，竞速白烧配额且镜像数据可能滞后。

**命中动作**：弹窗（新版本号 + release notes）→「打开发布页」开浏览器。**应用内下载安装明确推迟**（签名校验/安装器自更新是独立大项目）。版本比较 semver 对当前 0.1.0。

**落地（2026-08-29，批 2 提前动工）**：

- **分层**：`platform` 增 `HttpTextFetcher`/`UrlOpener` trait（lib.rs 零依赖纪律不变），win32 侧 `http` 模块实现——WinHTTP `AUTOMATIC_PROXY` 跟随系统代理（国内直连 GitHub 的生命线，镜像回落之外的第二条路）、session→connect→request 全 RAII、每相位 10s 超时、响应体 2 MiB 上限；windows-sys 加 `Win32_Networking_WinHttp` feature，**零新 crate**（serde_json 本就在编译树）。
- **纯逻辑**：`ui-viewmodels::update_check`——手写三段版本比较（不引 semver crate；tag 解析不出按「不可判定」处理绝不误弹）、GitHub `releases/latest` JSON 解析（tag_name 必需、notes 截 4000 字）、源**顺序回落**（主源健康时其答案终局，哪怕「不更新」——镜像数据可能滞后）、`UpdateCheckVm` 状态机（静默失败回 Idle 零打扰、手动失败面板可见；「跳过此版本」只静音自动弹窗与角标，手动检查照样显示）。单测 21 条随模块内联。
- **UI 三件**：设置面板「关于」区（版本读数 + 检查按钮 + 状态行，失败才着 danger）；新版本弹窗（两段式入场同 classify；notes 滚动区高度钉 `min(200px, 内容高)` 防 ScrollView 塌陷——「移动到分类」同款坑；打开发布页经 `Win32UrlOpener`→ShellExecuteW）；设置齿轮 8px 红点角标（有更新且未跳过时）。弹窗顶部锚定 y:130 与全部既有弹窗（96–150px）同一约定，不做垂直居中。
- **线程纪律**：检查在 `std::thread`（WinHTTP 会话线程局部，UI 线程零阻塞）；收尾经 `invoke_from_event_loop` 弹回，闭包只带 `Weak<AppWindow>` + 纯数据（Send）——VM/设置 Rc 经 UI 线程 `UPDATE_WIRING` 槽位取回（Rc 非 Send 不能跨线程）。设置写盘仍只发生在 UI 线程。
- **配置**：更新源清单 = 内置 GitHub 主源（`api.github.com/repos/whynusn/assetdeck/releases/latest`）+ `update_feeds.toml`（与 settings.toml 同目录，`feeds = [...]`）覆盖；**镜像默认值仍留白**——D56 原话「实测哪个可用再定默认」，待真机实测镜像连通性后补默认清单。
- **验收**：版本比较/回落顺序/状态机/settings round-trip 全绿；slint-viewer 渲染四态（弹窗带/不带 notes、面板正常/失败态）经裁剪放大核验角标与配色。

### 批次归档：明确不做 / 推迟清单（2026-08-28）

- 框选橡皮筋（见 D47）；编辑标签、大图预览浮层（见 D48）；备注（notes）字段与备注搜索
- 瀑布流方向感知预取 + 空闲预算提升（症状驱动；D54 落地后观察再定）
- 「来源建议分类 + 单键确认」（A4 的另一半）
- ~~应用内更新下载安装~~（D70 落地，2026-09-02；签名校验 ed25519 仍后置）

### D57 构建链硬化（2026-08-29）

**内容**：对统一打包流水线（scripts/package.ps1 + ci.yml 五 job）做供应链与一致性修补，四个动作：

1. **工具链钉死**：新增 `rust-toolchain.toml`（channel=1.98.0、profile=minimal、clippy/rustfmt）作为版本**单一真相源**；CI 换本地 composite action `.github/actions/setup-toolchain`（从该文件读 channel 安装，零第三方 action 依赖；dtolnay/rust-toolchain 实测不读此文件才自建）。废止 stable 浮动——「本地 gnu ↔ CI MSVC 源码级兼容」的假设不再随 stable 升级静默失效。**环境记录**：2026-08-29 实测 TUNA 镜像 rustup/dist 对 1.97.0/1.98.0 全 404（用户级 RUSTUP_DIST_SERVER 配置未动），1.98.0-gnu 一次性安装经 `RUSTUP_DIST_SERVER=USTC` 覆盖完成。
2. **cargo-deny 补 RustSec advisories 硬门禁**（并放开每日 schedule 触发——公告库是持续变化的外部输入；cargo-deny 版本钉 0.20.2，schema 有漂移前科）。策略：vulnerability/unsound 全量 deny；`unmaintained = "workspace"`（0.20 v2 schema 收作用域而非严重级）——现存 paste/rustybuzz/ttf-parser 三条 unmaintained 全是 Slint 1.17.1 + image 传递依赖、无安全升级可走，作用域外降级为警告。连带 `lru 0.12.5 → 0.18.3`（RUSTSEC-2026-0002/0253 两条 unsound 清零，仅 grid_vm/thumbs 两处构造点，编译零改动）。
3. **产物校验和**：package.ps1 收尾生成 `artifacts/SHA256SUMS.txt`（sha256sum 标准格式），随 artifact 上传并附进 GitHub Release——安装包未签名前提下的最低完整性保障。
4. **release tag/版本一致性校验**：release job 首步断言 `${GITHUB_REF_NAME#v}` == Cargo.toml version，杜绝「tag v0.2.0 发出 0.1.0 命名正式包」；installer/Cargo.toml 加注：其独立 version 不参与分发命名，勿当第二版本真相源。

**已知残留（P2，未排期）**：package job 每 PR 全量打包（宜收窄到 main+tag）；`+crt-static` 仍在 package.ps1 里走临时 RUSTFLAGS（与 mem-regression 构建指纹不同，rust-cache 双份；宜进 `.cargo/config.toml` 的 msvc target 段）；产物缺 BUILD_INFO（host triple/工具链/git hash——「安装版 exe 滞后」类误报排查只能靠核时间戳）；打包产物无冒烟测试（mem-regression 测的是 target/release 直出 exe，非 zip 内那份）。

### D58 画像严格档（require_title）：千牛优惠弹窗误激活的根治（2026-08-29）

**症状**：用户反馈上框有概率拉起千牛的优惠弹窗（或其他杂窗口）而非接待中心。

**根因（两条污染路径，同一根因）**：激活器本身只认单个 hwnd（`SW_RESTORE`+`SetForegroundWindow`），无任何按应用名拉起的逻辑；问题在上游绑定。千牛所有普通 Qt 窗口**共享同一个类名** `Qt5152QWindowIcon`（Qt 按运行时版本命名），而匹配规则是「类名或标题命中其一即可」——优惠弹窗类窗口凭类名即命中 qianniu 画像。路径 A：弹窗抢到前台 → WinEvent 前台钩子 → 热目标被**静默**改写为弹窗（`on_foreground` 此前零日志）→ 下次上框激活弹窗。路径 B：接待中心 hwnd 从枚举短暂消失（D42 竞态）时，同进程 (`exe:pid`) 的弹窗成为「唯一替代」被静默重绑。概率性来自两条路径都需要时机配合。注入前校验只查「存活+前台」，弹窗完全满足，身份校验从未发生。

**决策**：画像新增 `require_title` 严格档（类名+标题须**同时**命中，用户画像可整字段覆盖），qianniu 启用；标题正则同时收编用户级变体 `接待(中心|台)$`（部分用户窗口为「接待台」）。严格档下弹窗不命中画像 → 不进候选、不能被热目标跟随、不能被重绑、不可能被激活——从绑定源头根治，不靠运行时补丁。配套：`TargetBinding` 新增 `session_window`（标题命中即会话窗口证据）；热目标切换打 Info 日志（旧→新 + 标题 + 会话窗标志），现场可回溯。

**取舍记录**：评估过运行时「只升不降」守卫（会话窗口不被同应用非会话窗口顶替），**放弃**——它对启用严格档的画像纯冗余；对微信这类宽松画像则有害：微信独立聊天窗口标题是联系人名（类名命中、标题不命中），恰是守卫会误杀的合法目标。绑定精度归画像（数据），不做运行时启发式。微信不启用严格档的同一理由。

**附：取焦点方式实测结论（同日多角度真机实验，工具 `tools/real-im-verify/src/bin/focus_probe.rs`）**：微信 4.0 UIA 树今天暴露 2 个可写元素（`mmui::ChatInputField`，FindFirst 8ms 落焦可验证），旧画像注释「候选数 0」已过时；千牛 CEF 输入框 UIA 不可达且盲找会误聚焦「买家账号」输入框（FindFirst/FindAll 全中招），PropertyCondition FindAll 在 CEF 上 151–421ms 禁入热路径。`GetGUIThreadInfo` caret（4–15µs）是微信+千牛**唯一**通用有效的「锚点点击后落焦验证」信号（点击输入框必现 2px blinking caret，点偏则无）——待落地为 `click_anchor` 点击后复核，把「只上框不误发」红线闭环。多开微信的账号昵称不在主窗口 UIA 树内（160 元素全量 dump 实证），自动获取需另行设计。

### D59 目标配置双通道接线 + 实例别名册（2026-08-29）

**内容**：把 D13 设计多年未接线的后两层目标数据源接通，解决「多开微信无法区分是哪一个微信」：

1. **`profiles.user.toml` 用户画像**：与设置同目录约定（库根优先，回退 exe 旁），同 id 字段级覆盖内置画像（机制 D13 既有，`load_profiles` 一直支持，缺的只是装配层读取）。损坏退回内置画像并记 error 日志，不拖垮主程序（`TargetRoutingRuntime::profile_load_check` 预检）。
2. **`targets.json` 实例别名册**：键 `instance_id`（`exe:pid`），纯模型 `targets::AliasMap`（BTreeMap 键序稳定、坏文件按空册、空白别名=清除）。装配层启动加载、重命名确认后原子写回（tmp+rename，与设置保存同模式）。UI 入口：右键目标 picker 行 → 重命名弹层（默认名回显，留空保存=恢复默认）。
3. **标签链路**：`TargetChoice` 增加 `base_label`（别名覆盖前的默认标签，清除时恢复用）；别名在匹配后、进 tracker 前统一应用，chip/picker/钉住绑定同步改名。

**诚实边界**：别名键的 `exe:pid` 随目标进程重启变化，别名只在目标进程存活期内稳定——这是窗口层可观测身份（exe+pid+标题，微信标题恒为「微信」）的诚实上限；账号昵称不在微信 4.0 主窗口 UIA 树内（D58 附注实证）。稳定别名需产品级账号身份源（如读微信数据目录），v1 不做，用户重启 IM 后重命名一次。

### D60 库根与 exe 解耦 + 去重命中浮出 + 导入链路可观测性（2026-08-29）

**症状**：用户反馈「导入素材有问题」，三层叠加：① 5 次 PNG 拖入全被 pHash 去重静默跳过（DEDUP_THRESHOLD=8，duplicate 走 stdout 被 UI 侧丢弃，UI 弹「导入完成」实际入库 0 条——同窗口连拍截图极易互判重复）；② GBK 编码 txt 上框物化 `read_to_string` 报错走零日志分支（asset_id=1×10、asset_id=12×4 静默无果）；③ 库根=exe 同目录，机器上 4 份 exe 副本（安装版/dist/debug/release）各带独立库互不可见、互不去重，且安装目录里的库被重装/验证流程清空（16:15 重装清掉 14:21 导入的全部素材）。

**决策**：
1. **库根与 exe 解耦**：`default_library_root` 改为 `%LOCALAPPDATA%\asset-manager\library`（与 logs 平台缺省目录同一数据根）。全部副本共享一库，重装不再触及库；LOCALAPPDATA 缺失回落 exe 同目录并告警。开发/便携隔离用 `--library-root` 显式指定。存量真实库一次性手工迁移（`target\release\library` → 新根，13 行完整），不写自动迁移代码。
2. **去重命中必须浮出**：sample-library 在 `skipped>0` 时发 NOTICE 点名（源路径 ≈ 已有素材 file_name，上限 3 条）；duplicate 语义不变（仍跳过不占盘，符合导入去重红线）。
3. **可观测性**：`spawn_import_pipeline` 两阶段接 `with_line` 把 worker stdout 非协议行（imported/duplicate/failed/done/timing）落日志；上框物化落空/出错分支补 warn 日志（旧实现零日志，是三次排障黑洞的元凶）。
4. **文本素材入库即转 UTF-8**（库内不变量，非读取端兜底）：`media::normalize_text_to_utf8`——BOM 三态确定性识别（UTF-8 剥壳 / UTF-16 精确解码）→ 无 BOM 合法 UTF-8 零拷贝直通 → 其余按 GBK 转码（中文 Windows ANSI 事实标准）；新增依赖 encoding_rs（Apache-2.0/MIT）。library 入库计量按转码后字节、>8MB 入口硬拒绝（粘贴端对文本是同步读盘，病态大文本不进拷贝队列），拷贝线程走归一化写盘保证 size_bytes 与实盘一致。存量 2 条 GBK raw.txt 一次性重编码（22139→27854B）并同步 size_bytes。
5. **上框域 = 活动素材**：materialize 拒绝已删除行与对象文件缺失（`CatalogError::PasteBlocked` 带原因上日志与通知条）——旧实现照常产出载荷，HDROP 指向不存在文件，IM 端静默丢弃整次粘贴（提示「成功」、实际贴空气）。

**取舍记录**：文本编码在**导入边界**建立不变量而非读取端加解码回退（用户拍板：不打运行时补丁、只走根因）；GBK 是中文 Windows 的事实 ANSI，异种编码会得到替换字符而非失败——显式记录这一已知退化。双实例共享一库后，UI 单实例守卫（仅架构意向、代码未落地）优先级上升：并发写库目前依赖 SQLite 锁 + 单写者子进程纪律兜住，守卫待落地。

### D61 停发示例库 + 一键迁移旧版数据目录 + 全类目内容去重（2026-08-29）

**症状（8-29 覆盖事故收账）**：用户重装后素材库只剩 11 条示例素材。根因：installer `install()` = `archive.unpack(dir)` **原地覆盖**，payload 里的 `dist/library/meta.db` 把用户 exe 旁的真库整个盖掉（对象文件 18 个仍在，索引只剩 11 条示例——用户 7 个真文件变孤儿，meta.db 不可信）。

**决策**：
1. **payload 停发示例库**：package.ps1 删除 dist/library 生成块（$sampleExe 调用 + meta.db 校验），安装包内不再携带任何素材库——覆盖事故从源头消失。sample-library.exe 本体保留（D11 运行时导入 worker）。示例库今后只在首次启动按需引导（未排期）。
2. **一键迁移旧版数据目录**（设置面板「数据迁移」区，启动检测 + 面板打开时刷新）：检测 exe 旁 `library`（含已改名留档的 `library.migrated-*` 回退场景）→ 详情行显示文件数/体积 → 按钮走**改名先行 + 重放导入**：旧库整目录改名 `library.migrated-<unix_ts>`（天然幂等标记，迁移中断不重入）→ 以该目录为 `--import-paths` 清单源复用既有导入管线（进度条/derive-thumbs/去重全复用，零新导入机器）→ 成功后写 `migration.done` 标记、通知留档目录名；失败则改名回滚（回滚再失败只落日志，不阻塞）。
   - **为什么不搬目录/收养 DB**：被覆盖事故污染过的 meta.db 不可信（索引与实盘不一致），只有源文件本身可信——重放导入让新库索引从可信数据重建。
   - **分类保留（用户指正后补全）**：v0.1.0 **有**分类机制（schema v4 `assets.category` 列 + 导入归类链路）——初版「v0.1.0 无分类功能故不迁移」的论断是错的，已修正。保留走管线原生指令，零后处理：`store::read_category_by_uuid` 用裸连接**只读**旧库 `uuid→category`（不带 CREATE 的打开、只发 SELECT、不跑 schema 迁移——「读取不改写用户旧库」有守卫测试钉死）→ 清单 mode 列写 `category:<名>`（对象目录名即旧库 uuid：v0.1.0 起 `rel_path = objects/<uuid>/raw.<ext>`，uuid 直接联结磁盘文件与分类行）；映射缺项 / 旧库 NULL / 读失败（库缺失、损坏、非 SQLite）→ `auto` 落待分类，迁移照常。分类名清洗制表符/换行/首尾空白（清单一行一条，脏字符断行）。
   - **诚实边界**：迁移 = 复制入库，旧目录改名留档占双份盘，用户确认后手动删（不做自动清理）；**重复内容不重复入库**（去重红线）——被判 duplicate 的文件其分类指令不生效，既有同内容素材的分类原样保留（内容相同即同一素材，用户在新库的归类决策优先）；旧库 file_name 列的原始文件名不带回（入库 file_name = 磁盘 canonical 名 raw.\*，需扩展 D49 清单格式才能携带，未做）。
   - **清单只认 `raw.*`**：真机旧库实勘发现对象目录里还有上框物化写的 `paste.png` 粘贴载荷旁车（18 对象 22 文件——4 个上过框的对象各带一份）。旁车不是素材，进清单就会把每次上框都重放成重复图；walk_objects 只放行 `raw.<ext>`（canonical 素材文件的入库不变量），检测计数/体积与清单共用同一过滤。
3. **全类目内容去重**（复用前提）：pHash 只覆盖图片，视频/文本/未知类型同内容文件可无限重复占盘——迁移重放旧库时必然全部重复入库。补齐：非图片素材一律计算 **SHA-256 内容摘要**（视频/未知 = 流式读盘哈希；文本 = 归一化 UTF-8 字节哈希，零额外 IO），store v5 加 `content_hash` 列 + 索引（PRAGMA user_version 迁移，向后兼容），去重查找 = 会话内 map → SQL `uuid_by_content_hash`（排除已删除行）。图片仍走 pHash（感知哈希语义不变）。
   - **无预过滤**：曾按 size 预过滤（同体积才哈希），被测试打回——首份该体积的素材不落摘要，之后同内容文件永远找不到比对目标（同会话/跨会话双漏）。一律哈希的诚实代价：每个非图片素材多一次顺序读；对照重复素材永久占盘，值。

**取舍记录**：迁移入口放在 app 内而非自解压 installer——installer 保持「哑」（解包+快捷方式），任何数据操作都归 app 源码（可测试、可回滚、可观测）。检测按**位置**不按版本号（exe 旁有 library 即候选），避免版本识别的脆弱性；同一检测逻辑天然覆盖「库曾被覆盖事故」的受害者（孤儿对象文件会被重放导入收编）。**流程教训**：初版实现把清单生成放在改名之前（意图：清单写失败不动旧目录），真机 GUI 验证抓出致命序——清单引用的是改名前路径，改名后 18 条全灭，worker 对缺失文件静默跳过（imported=0），迁移却照常「成功」收账；单测全绿因为各函数都只被孤立测试，组合序只有端到端能暴露。修正为改名先行、清单从备份路径生成、清单写失败回滚改名——「写清单失败不动旧目录」改由回滚保证。

### D62 唤出黑屏根因闭环：slint 软件渲染器部分重绘在表面丢弃后漏画（2026-08-30）

**症状（真机复发，134e6f0 的守卫在场无效）**：唤出窗口后局部黑屏，只有鼠标悬浮经过的组件重新渲染出来。安装版日志证实 `winit/software` 渲染档 + 守卫已安装，但黑屏依旧。

**根因（源码级闭环，slint 1.17.1 + softbuffer 0.4.8 + winit 0.30.13）**：软件渲染档的黑屏与 femtovg 的 WGL 未定义缓冲是**不同失效链**，134e6f0 的 `RDW_INTERNALPAINT` 兜不住前者：
1. 最小化时 winit 报 `Resized(0×0)`，slint `winitwindowadapter::resize_event` **直接丢弃零尺寸**（防渲染器炸）——场景零变化，恢复时 item 几何零 diff；
2. `sw` 渲染器 `render()` **每帧用 softbuffer 的 buffer age 选重绘档**，Windows 上 age 恒返回 1 ⇒ `ReusedBuffer` 部分重绘——把 `occluded(true)`（Resized(0×0) 时手动调用）设的 `NewBuffer` 全量档**当场覆写**，该重置在这条链上形同虚设；
3. 部分·重绘算出空脏区 ⇒ 一像素不上屏；而 softbuffer win32 的 `present_with_damage` 收尾 `ValidateRect(整窗)`，系统从此不再补发 WM_PAINT；
4. DWM 在最小化/完全遮挡期间可能丢弃窗口重定表面 ⇒ 黑区定格，直到鼠标悬浮把悬浮件区域标脏（与截图症状完全吻合）。
旧探针「全绿」的验证是假阴性：只跑了 plain 变体 7 个周期，且脚本会在最小化态采样（GetWindowRect 返回 -32000 屏幕外坐标）——本轮 30 周期即抓到一次 100% 纯黑假阳性。

**决策**：黑屏兜底从「补触发一次重绘」改为「**系统整窗失效时把 slint 整窗标脏**」，双渲染档通用：
- `platform::win32::paint_guard` 子类 proc 增钩 **WM_PAINT**：转发给 winit（其 ValidateRect 会清更新区）之前用 `GetUpdateRect` 判更新区是否 ≥90% 客户区——是即「系统判定表面内容不可信」（最小化恢复/遮挡重现的 DWM 丢弃特征）⇒ 调应用层回调（thread_local 存非 Send 闭包，与 window_ready 同纪律）。局部失效（拖拽改尺寸的新暴露边条）不触发，不伤部分重绘的吞吐；「最小化→非最小化」补发 RDW_INTERNALPAINT 保留（femtovg 档靠它触发重绘）。
- 应用层回调翻转 `AppWindow.repaint-nudge` 布尔位；`appwindow.slint` 垫底放一个铺满窗口的透明哨兵 Rectangle，`opacity: repaint-nudge ? 1.0 : 0.999`——Opacity 类项变化命中 partial_renderer 的 `must_refresh_children` 分支，自身几何=全窗 ⇒ 下一帧全量重绘，盖掉黑区。哨兵透明无 TouchArea：不参与布局、不挡输入、两档透明度差 0.1% 不可感知。
- **为什么不用别的路**：slint 公开 API 无「强制全量重绘」入口（只有 `request_redraw`，仍走空脏区路径）；改 1px 尺寸骗 buffer 重建会破坏最大化态且有闪烁；等 slint 上游修 age 语义/Windows Occluded 派发不可控。属性标脏是应用层唯一可靠杠杆。
- **诚实边界**：nudge 每次整窗失效多画一帧全量（恢复时一次，代价可忽略）；WM_PAINT 钩子在拖拽改尺寸时不触发（局部更新区），性能无回退。

**验证**：platform/app-ui 测试全绿 + workspace clippy（--exclude bench-harness）无新告警；探针 plain 30/30、showDesktop 20/20 全绿，日志证实每次恢复周期恰好触发一次「整窗失效重绘哨兵」；探针脚本已修「iconic 态采样」假阳性（iconic 等待 500ms 后跳过该轮）。

### D63 .emo 经清单导入整包丢失：cleanup 时序违反读者契约 + 全失败必须非零退出（2026-08-30）

**症状（用户日志，ZhangYue 机）**：拖入千牛 .emo 素材包走归类弹窗导入，提示「导入完成」，应用里 **0 素材**。app 日志里整包几百条 `sample-library: failed <%TEMP%\qianniu_emo_*\...> : 图片解码失败：系统找不到指定的路径 (os error 3)`。

**根因（两条叠加，主犯是前者）**：
1. **cleanup 时序**：`EmoReader.read()` 解包到临时目录，`ImportedAsset.source` 全部指向里面，契约注释明写「由 main **导入完成后**删除」。旧两位置参数流程（`import_package`）顺序正确（先 `run_import` 后清理解包目录）；D49 清单流程（`run_import_paths`）却在**收集完 assets 的当场**就 `remove_dir_all(cleanup)`，`run_import` 拿到的全部源路径已失效 → 逐条解码失败 → imported=0。e2e 测试全用 `d:`/`f:` 行、唯独没有 `p:`（.emo）行，盲区正中。
2. **上报失真**：批次全失败时 sample-library 仍 exit 0（单文件失败不拖垮整批的既有语义），壳层 `with_finished(success=true)` 弹「导入完成」成功调——0 素材与成功提示自相矛盾，D60 的 NOTICE 警示被淹没。

**决策**：
1. `run_import_paths` 清理**后置**：cleanups 收进 Vec，`run_import` 返回后再逐个删除——镜像 import_package 的正确序，契约回到注释所写。
2. **全军覆没 = 批次失败**：`run_import` 在 `imported == 0 && failed_total > 0` 时返回 `Err`（非零退出），壳层走既有失败路径报「导入失败：全部 N 个素材导入失败：…」；部分失败维持 Ok + NOTICE（D60 语义）；全重复（imported=0 skipped>0 failed=0）是合法幂等重导不算失败。
3. TDD：进程边界 e2e 补两例——`emo_package_via_manifest_imports_all_assets`（真 zip 改名 .emo + `p:` 清单，红灯复现 imported=0，修复后 imported=1 且 groupName 分类生效；Compress-Archive 拒绝非 .zip 扩展名，fixture 先落 .zip 再改名）；`all_failed_batch_exits_nonzero`（坏图全失败 → 断言非零退出且零入库行）。

**诚实边界**：部分失败（imported>0）仍 exit 0 + NOTICE 警示——「整批完成、个别失败」的既定语义不变；被打断的 .emo 重导是幂等的（pHash/内容去重）。fixture 用 Compress-Archive（与解压侧同族 .NET 实现），真实千牛 .emo 为标准 zip，互通已由解压侧既有行为背书。

### D64 发布流程纠偏 + 迁移入口可发现性 + 导入结果诚实摘要（2026-08-30）

**三件事收账**（v0.1.2 发布与随后两起用户侧反馈）：

1. **tag/版本门禁首秀拦下错误发布**：v0.1.2 tag 直推时 Cargo.toml 还在 0.1.1，release job 的 D57 一致性校验 3 秒判红（全 job 唯一失败点）。按设计工作，非缺陷；补 0.1.2 版本号后删远端 tag 重打，v0.1.2 正常发布（installer/portable/SHA256SUMS 三件）。教训：打 tag 前先对版本号，门禁不绕。
2. **迁移入口可发现性**（用户实测「一开始就没有迁移按钮」）：检测此前**只看当前 exe 旁**——便携 zip 换目录装、安装路径变过，旧库就在检测范围外且零日志可查。加固三件：① 候选扩到 exe 旁 + 安装器默认安装目录（`%LOCALAPPDATA%\Programs\素材管理器`，与 installer 同源约定；canonical 去重，exe 旁优先）；② 检测结果每次落 Info 日志（候选数/命中/留档），「按钮为什么没出现」从此有现场；③ **已完成态不再隐身**——只剩已收账备份时迁移区显示「打开旧库留档目录」按钮 + 留档路径文案（回应此前「找不到原本的数据源」），跨目录「最新」按备份名时间戳比较（全路径字典序会被目录段干扰——单测抓出）。
3. **导入结果诚实摘要**（同日两起「导入完成配 0 新增」被读成失败）：done 行统计捕获进壳层，完成通知改「导入完成：新增 X · 重复 Y · 失败 Z」；全重复明说「N 个素材已在库中，未新增」。NOTICE 协议加「提示：/警示：」前缀，壳层按前缀选色调——重复素材从常驻黄条（被读成失败）降为可自动消隐的绿条；未知前缀仍按警示兜底。失败消息剥掉「sample-library failed: 」工具名前缀，UI 只说人话。

**验证**：legacy_migration 9 单测（含 multi 回落优先级与已收账查找两新例）+ e2e 7 例全绿；迁移区三态 slint-viewer 渲染经 judge 验收（间距/折行/括号完整）；workspace test/clippy/fmt 归零。

### D65 导入去重重构：判死权收归字节等值 + pHash 降为相似提醒 + 结果逐项点名（2026-08-31）

**背景**：D60 已记录「5 次 PNG 拖入全被 pHash 去重静默跳过（DEDUP_THRESHOLD=8，同窗口连拍截图极易互判重复）」，当时的修复只把命中浮出（NOTICE 点名 + D64 摘要），**跳过本身仍是自动的**。用户裁定：去重不得无感知，算法也要重新设计。

**算法重构（两级判定）**：
1. **判死权收归 SHA-256 字节等值，覆盖全类目**（图片不再例外——D61 的分工反转）：字节相同 = 系统**唯一**自动跳过的形态（D7「不重复占盘」红线保持），且必须点名上报。副产品：字节相同的重导入不再白付一次图片解码（摘要先行短路）。
2. **pHash 从「判死」降级为「相似提醒」**：汉明距离 ≤ 12（原判重阈值 8，`SIMILAR_DISTANCE_THRESHOLD`）照常入库，结果标注「与某已有素材相似（距离 N）」。阈值放大的理由：FP 代价从「不可逆丢素材」降为「多一条提醒」，同时把 6–12 距离的重压缩/缩放重复纳入捕获范围（旧阈值漏掉）。裁决权交还用户——素材已在库里，删不删用户定。
3. **低信息守卫**（phash crate `reliable_phash` + `AC_ENERGY_FLOOR=4.0`）：8×8 DCT 网格最大 |AC| 低于地板 = 近纯色图 → 不产出 hash。实测两张纯色图的 hash 距离可恰落在 12（阈值边界）——这种 hash 由浮点取整噪声决定、与内容无关，必须从根上禁出场（历史缺陷：纯色/极小图互判重复静默丢素材，D54 测试期已踩到一次）。
4. **软删对齐**：内容摘要的会话 map 不感知软删/清空，会话命中必须复核「行存在且未删」再采信——trash_spec 回归抓出「清空回收站后重导同一文件被判重复、指向已清空的 uuid」（静默丢素材的又一体）。

**感知面（三层）**：
1. done 行加 `similar=` 计数；NOTICE 三分：失败（警示黄）/ 完全相同跳过点名（提示绿）/ 相似已导入待复核点名（提示绿）。
2. `RESULTITEM\t<kind>\t<extra>\t<source>\t<existing>` 协议行（kind = exact|similar|failed；每类目上限 150，真实计数恒在 done 行）——壳层收集，喂结果弹窗。
3. **导入结果弹窗**（持久、可停留）：跳过/相似/失败逐项点名 + 摘要行；干净导入不弹（维持轻提示，不加点击成本）。两段式动效同 classify（D53），顶部锚定 y120（弹窗族约定）。

**取舍记录**：评估过「导入前重复裁决」交互（扫描期暂停管线、弹窗逐项选跳过/导入、stdin 回传决策）——被否：同批并发存在「首份登记前第二份已查重」的判定窗口（D61 诚实边界），裁决前置要么引入全量预扫描双解码、要么与并发流水线互斥，且对万级导入是阻塞点。「相似照常导入 + 结果点名」让信息零丢失、管线零改动，裁决发生在导入后（素材已在库中可自行删除）。结果弹窗 v1 只读；逐项「移入回收站」待后续（需批量 trash 子命令接线）。

**落点**：crates/phash（reliable_phash）；crates/library（enqueue 重排：文本归一化/尺寸闸 → 全类目摘要先行查重 → 解码 → find_similar；`EnqueueOutcome::Ticket{ticket, similarity}` 结构化；PHashIndex::nearest_within 取最近命中）；tools/sample-library（Summary.similar + record_similar + RESULTITEM）；app-ui（DoneStats 四字段、结果弹窗数据面、ui_enums IMPORT_RESULT_*）；appwindow.slint（ImportResultRowData + 弹窗组件）。

**守卫**：phash 低信息单测（AC 能量地板/结构图可信）；library 五测（字节相同判重、纯色对不互判不丢、近重复带提醒、无关不提醒、图片落 content_hash）；e2e 三测（跨批字节相同点名跳过、近重复入库带提醒、纯色对进程级零误判）；sample-library e2e 全量回归（D63 幂等重导语义不变）。

### D66 归类弹窗重构：批次级单决策 + 单输入框检索 + 可解析包静默直通（2026-09-01）

**背景**：D50 归类弹窗按「来源组」逐行问（组内 chips + 统一归入输入框），分类选择靠 ComboBox 逐个滚动，用户点名两条：①无法输入快速定位分类（太慢）；②「总的应该分成三种操作：归入，新建，和待分类」——批次级语义，而逐组三 chip 重复 UI 迷惑。迭代中用户补充定稿：**一个文本框，输入实时匹配已有分类，点候选即归入；匹配不到就点导入，弹提示「已自动创建并导入」**。

**交互定稿（三步收敛）**：
1. 初版把「三操作」做成每组一行、行行重复三 chip——被用户否（重复 UI 迷惑）。回到 CONTEXT.md 既定原则「归类发生在批次级，每批一次」：**整批一个决策**，来源组信息降级为只读清单行（「本次导入：散文件 4 个 · 文件夹「旅行」 · 素材包「broken」（结构未识别）」，单项组带名、残包组标注）。
2. 用户提出单输入框方案，采纳并补三处强化：**实时预告行**（打字时即显示「将导入到已有分类「X」/没有同名分类，点导入将新建「X」/留空 = 放入待分类」，导入前结果可见——自动建分类不留无感知，色分 accent/黄/灰）；**大小写不敏感精确匹配取列表规范名**（输入 screenshots 归入已有 Screenshots，不产生大小写重复项）；**待分类保留显式按钮**（取消|放入待分类|导入），空输入点导入同效兜底。
3. 归入/新建两枚 chip 被「输入框 + 预告行」吸收：confirm 时 `resolve_target` 统一解析（空 → inbox；精确命中 → category:规范名；未命中 → 新建 + Success toast 点名）。输入「待分类」经 `decision_to_mode_field` 归 inbox 指令，不会真的建重名分类。

**静默直通（按包内分类）**：probe 探得 ≥1 分类的 .emo / 千牛结构目录**不弹窗**，壳层直接发 `p\tauto\t` 清单行按包内分类导入（整批皆可解析 = 零点击）。门槛 probe≥1 的理由：探得 0 分类或 probe 失败 = 结构可疑（残包），交用户裁决（进清单行，预填包名 stem）。CLI 清单格式零改动（`<kind>\t<auto|inbox|category:名>\t<path>`，`auto` 本就存在）。

**记忆（R8）收拢**：六个 per-kind 记忆字段 → `remember_mode`/`remember_category` 一对（批次级）。串语义 `category`（含旧 `into`/`create` 兼容映射，指令层同形）/`inbox`；未知串（per_source/unified/乱值）= 没记，重新弹窗。R8 直通条件不变：ask 关 + 有记忆 → 整批套用不弹窗；ask 关但没记忆 → 照常弹窗。预填优先级：记忆分类 > 单一来源组建议名（文件夹名/包 stem）> 空。

**取舍记录**：①「按文件夹名归类」自动方式正式裁撤（用户三操作语义 + D50 遗留魔法行为），降级为「单文件夹导入时输入框预填目录名」的建议；多文件夹各自成类需分两次导入（v1 接受）。②probe 分类数不再进 UI（可解析包静默、残包组里只可能是 None/0，「含 N 个分类」角标是死代码），`SourceGroup.category_count` 字段删除。③归入候选封顶 8 条保留（继续输入收窄）。

**落点**：crates/ui-viewmodels classify.rs（`plan_import` 静默分流 / `manifest_summary` / `dialog_prefill` / `remembered_decision` / `resolve_target`+`ClassifyTarget`（hint/hint_kind 与 confirm 共用真源）/ `filter_category_matches`；`SourceGroup` 瘦身）、settings.rs（记忆字段收拢）；crates/app-ui main.rs（ImportFlow 去 rows 行模型，finalize 分流、confirm(kind,remember)、refresh_matches/refresh_target、do_import 三参 + auto 行、新建 toast）；appwindow.slint（ClassifyRowData 删除，classify-summary/argument/matches/hint/hint-kind 平铺属性，classify-confirmed(int,bool)）；ui_enums.rs + slint UiEnums（classify-hint-*/classify-confirm-* 双侧同步 + 测试锁定）；CONTEXT.md 词汇（归入/新建词条，「统一归入」「按文件夹名归类」降 _Avoid_）。

**守卫**：classify_spec 20 测（静默直通/残包仍问/批次预填/记忆串迁移/清单行格式/resolve_target 大小写规范名/hint 文案锁定/决策→指令映射/候选过滤）；settings_spec 往返含新字段；slint-viewer 渲染三态（新建预告黄/已有预告 accent+候选高亮/空输入待分类）；全 workspace test + clippy 归零。

### D67 浮层点击外部分级收口：菜单照旧收起，工作流弹窗只挡不关（2026-09-01）

**背景**：D46–D48 起「点外部 = 关闭」对全部浮层一刀切——一个铺满窗口的 dismiss TouchArea（appwindow.slint）在有浮层时吃掉一切背景点击，壳层 `overlay-dismissed` 全关。用户点名：所有弹窗都会被点到任何地方误关，工作流弹窗（尤其归类）误触即丢输入、要重新唤出。

**分级定稿**（用户确认）：
1. **菜单级**（右键菜单/范围菜单/导入菜单/移动到分类/目标选择器）：点外部照旧收起——菜单的标准交互。
2. **工作流弹窗**（设置/归类/重命名/属性/目标重命名/更新）：挡板继续铺满（背景点击被吃掉、不穿透），但壳层**不再据此关闭**——关闭只走显式按钮与 Esc。挡板不能摘：摘了背景控件恢复收点，弹窗飘着时点瓦片会误触发动作，比误关更糟。附带修正：点在弹窗面板自身空白处（此前穿透到挡板触发收起）现在也是死点击。
3. **出口补齐**：settings 面板原本没有任何显式关闭按钮（唯一路径就是点外部）——加固定头「关闭」按钮（不随滚动常驻）+ `settings-closed` 回调；Esc 链补齐此前缺失的导入菜单/范围菜单/目标重命名/设置四类（`pending_target_rename` 声明上移到链前共用同一 Rc）。

**取舍记录**：①点外部对工作流弹窗 = 死点击而非穿透，与多数模态弹窗一致；不加背景变暗（视觉面留待有需要再做）。②导入结果弹窗本就不在挡板名单（D65 设计为非阻断可停留面板），维持现状。③工具栏设置键在面板开着时被挡板吃点（与旧行为一致），关闭走面板头/Esc。

**落点**：appwindow.slint（挡板注释改写分级语义；settings 面板固定头 + 关闭按钮 + settings-closed 回调；update 弹窗注释）；main.rs（overlay-dismissed 只收菜单级并摘 import_flow/settings/rename/properties/target-rename/update 六处关闭；Esc 链补四类；on_settings_closed 注册）。

**守卫**：workspace test + clippy 归零 + fmt；slint-viewer 渲染 settings 面板（固定头/关闭按钮在位）。

### D68 拼多多商家版画像升格：借号真机实测回填，图片 files 单承载 + 视频诚实不支持（2026-09-01）

**背景**：M8 内置画像里的 `pdd` 是无真机会话的骨架（`paste_sends = []` 占位、无 input_anchor、focus_strategy 三级缺省，注释自认「未取得真实会话」）。用户借来拼多多商家版账号（后台运行）并给出已知事实「拼多多无法通过粘贴将视频等文件，只适配图片素材」，要求按千牛/微信同构标准实测回填。

**真机取证**（2026-09-01，商家工作台客服聊天页；全程注入前核前台、不合成 0x0D、粘后即清空输入框，无任何消息外发）：
1. **窗口指纹**：单主窗口承载全部功能（聊天是内嵌网页模块，`CChatWebPageBusiness::Init`），类名 `g_wszPDDWindowClass{E77EAED1-...}` 带实例级 GUID 后缀（matcher 的 `{GUID}` 变体规则本就是为它预写的），标题恒定「拼多多工作台」；同进程 38 个顶层窗口（催一催/FaceWnd/ShadeWindow/快捷回复/多多进宝聊天等）类名各异，无一与主类重名。
2. **粘贴行为**：图片（png/jpg）CF_HDROP 停在输入框内嵌渲染；CF_PNG（注册格式 "PNG"，与产品写法同参）粘贴**无任何反应**——网页层不认；文本正常；视频用户实测不可粘。
3. **聚焦**（focus_probe 三轮）：CEF 主窗口（Chrome_RenderWidgetHostHWND），激活后焦点停根控件/浮动提示（`MerchantDutyFloatTip`），UIA 全子树 editable=0，prop 条件变体 **109~124ms**（比千牛 83ms 更贵）。

**画像定稿**：`image = ["files"]` 单格式——CF_PNG 在拼多多是死路，png 兜底声明了只会把「素材不在库内」变成假成功的空注入，宁走 `Unsupported` 诚实报错；`video = []`——协商直接 Unsupported，管线报「无法承载」且连剪贴板都不写，不注入必落空的 Ctrl+V（用户明确 v1 只适配图片）；`paste_sends` 全空（图片/文本均停输入框，无即发事实）；`focus_strategy = ["already", "anchor"]`（同微信/千牛裁掉 uia）；`input_anchor = (0.49, 0.92)`——客服输入文本区中心（客户区比例 x 0.29~0.69 / y 0.88~0.96 实测量取，发送按钮 x≈0.63 起横向有余量）。宽松档不声明 require_title：PDD 的 DUI 框架给每个弹窗注册独立类名，「类名全应用共享」的千牛式误绑前提不存在，类名本身已是强身份信号（同微信的宽松理由）。

**取舍记录**：①「无法粘贴视频」不模仿千牛 video×files 的即发保护（那是「粘进去即发送」的实测事实），PDD 是「粘不进去」，两者在 Negotiated 里是不同行：Unsupported（不写剪贴板直接失败）vs WouldSend（写剪贴板但拒绝注入）。②`derive-thumbs` 的 paste.png 派生照旧全量执行（per-asset 与目标无关，对微信/千牛仍必要）。③多开行为同微信：两个主窗口并列最高分 → Ambiguous 进 picker。④冷启动提示文案「请先打开微信/千牛/拼多多商家版等目标应用」补点名。

**落点**：profiles/profiles.builtin.toml（pdd 块重写 + 头部 D22/焦点注释同步，telegram 维持三级缺省不动）；crates/targets/tests/real_im_profiles.rs（真机签名换成 GUID 后缀变体；新增 `builtin_pdd_image_is_files_only_and_video_uncarriable` / `builtin_pdd_focus_strategy_skips_uia_and_anchors_chat_input`）；app-ui main.rs（空目标提示文案）。

**守卫**：workspace test 全绿（real_im_profiles 5 测含 3 个 pdd 事实锁定）+ clippy 归零 + fmt；取证归档 `Documents\Default_Project_probe\pdd-*.png`（hdrop-image/jpg-hdrop/png-noop/text/workbench-layout/input-cleared）。



### D69 删除「记住我的选择」整机制：归类弹窗每次必弹（2026-09-01）

**背景**：用户发现弹窗族的「记住我的选择，不再询问」没有意义且影响不可逆（设置面板找不到可逆项）。盘点确认全项目唯一 checkbox 在归类弹窗：勾一次 → `ask_classify_on_import=false` + `remember_mode/remember_category` 落盘，此后**所有**导入静默套用同一对决策（R8 记忆直通），且没有任何 UI 可以清掉记忆重新询问。用户的裁决是直接删除而非增强：「直接删了所有记住项的开关，我认为这些都没必要」。

**决策**：三件套一起删——①弹窗 `remember-box` CheckBox 与 `classify-confirmed(int, bool)` 第二参；②`remembered_decision`/`remember_choice`/记忆直通块（finalize 里 R8 分支）；③设置项 `ask_classify_on_import`（唯一用途是放行记忆直通，「关闭后按记住的方式直接导入」的文案随记忆删除已不成立，留行即留死开关）。**保留**更新弹窗「跳过此版本」：它是按钮不是开关，按版本号自限（新版自动恢复提醒）、手动检查不受影响，与「记住选择永久改变后续行为」不同类。删除后归类弹窗**每次导入必弹**，天然可逆（取消即可），无需任何撤销闭环。

**行为变化**：存量 `settings.toml` 里的 `ask_classify_on_import/remember_*` 键被 serde 静默忽略（结构体无这些字段），曾经关掉询问的用户回到每次必弹——这正是本决策的意图，不做迁移。`dialog_prefill` 去掉 settings 参数，预填回落单一来源组建议名（目录名/包 stem），混合组留空不变。

**落点**：appwindow.slint（CheckBox 删 + import 减项 + 回调改单参）；app-ui main.rs（R8 直通块/confirm remember 参/remember_choice/settings_path 死字段——它唯一读者是 remember_choice）；ui-viewmodels classify.rs（remembered_decision 删、dialog_prefill 签名收窄）、settings.rs（三字段 + default_ask_classify + value_of/slot_mut/detail_for/SETTING_SPECS 行）；classify_spec.rs（4 个记忆用例删、prefill 用例改签名）、settings_spec.rs（roundtrip 字面量减项）。

### D70 应用内自更新：统一安装器路径——下载→校验→退位移交（2026-09-02）

**背景**：D56 推迟项重启。定盘前重估了便携版原地替换方案，被两个事实否掉：①dist 有 **4 个 exe**（asset-manager/decode-worker/sample-library/derive-thumbs），运行中 worker 会撞文件锁，rename 舞步要从「换一个 exe」膨胀成「编排四个」；②便携包是 zip，壳层解包要引 `zip` crate（新依赖）。而 dist.tar.gz 的解包器**本来就存在**——自研安装器（installer crate），且其 payload 是 `include_bytes!` 内嵌，**一个 installer exe 就等于完整新版本**。

**决策（统一路径）**：安装版与便携版走同一条链——下载 `assetdeck-installer-<ver>.exe` → SHA-256 校验 → spawn `--silent --install-dir=<exe 所在目录> --wait-pid=<本进程 pid>` → 本进程 `exit(0)`。安装版多刷一次快捷方式（无害保新）；便携版加 `--no-shortcuts`。模式判定 = exe 目录是否等于安装器默认目录（`%LOCALAPPDATA%\Programs\素材管理器`）；判错的差异上限是「快捷方式没刷新」，无破坏性。安装器**持 PROCESS_SYNCHRONIZE 句柄等老进程退出再解包**（`OpenProcess`+`WaitForSingleObject`，30s 上限；找不到进程=已退直接走）——内核对象等待不是轮询，运行中 exe 的文件锁由「先退出、安装器后解包」的时序消解，全程零 rename、零 zip 依赖。

**供应链**：对照 release 附件 `SHA256SUMS.txt`（D57 已随包产出上传）校验，**缺清单拒绝更新**、不符即中止并留错误行；哈希走 BCrypt CNG（`windows-sys` 已在编译树，零新 crate，密码学原语绝不手写）。sha256 防的是下载损坏/截断；HTTPS（schannel + 系统代理）防传输窃改；**ed25519 签名校验仍后置**（密钥保管是独立工程），与本清单划清边界。

**落地**：platform 增 `HttpFileDownloader` trait + `Win32FileDownloader`（同 WinHTTP 栈：AUTOMATIC_PROXY 跟系统代理、每相位超时、64 KiB 块边收边写盘不驻留内存、`read==0` 标签跳出防「available>0 但 read==0」死循环、512 MiB 异常上限、取消旗标逐块检查）；`crypto::sha256_file_hex`（BCrypt 流式喂块，FIPS 向量单测）。installer 增 `--wait-pid=`。ui-viewmodels 增 `update_apply` 模块：release 清单 `assets[]` 解析进 `ReleaseInfo`（缺失/为空不算坏源——镜像可能只回填 tag）、精确名挑附件（宁可报缺也不模糊匹配装错文件）、sha256sum 标准格式解析（`*` 二进制标记/大小写归一/坏行跳过）、`UpdateApplyVm` 四态状态机（Idle/Downloading/Launching/Failed；**Idle 态吞迟到失败**——取消后的下载线程错误不得把弹窗拽回错误态）。app-ui：弹窗四态（初始三按钮/下载中进度条+取消/启动中不可逆无按钮/失败红字+重试；下载中收 notes 防高度抖动），下载线程 100ms 节流回弹进度（格式化留在 VM 侧可测），**Esc 在下载中 = 先取消再关弹窗**——弹窗关了下载若继续，完成时会毫无预兆退出应用；导入进行中禁止更新（worker 子进程持有 exe）。设置零新增字段。

**落点**：platform lib.rs/win32.rs/Cargo.toml（+Cryptography feature）；installer main.rs/Cargo.toml（+Threading feature）；ui-viewmodels update_check.rs（assets 解析）/update_apply.rs（新模块 11 测例）/lib.rs；app-ui appwindow.slint（弹窗四态+update-later 回调删——按钮已不存在）/main.rs（UpdateWiring 扩 apply_vm+cancel Arc、spawn_update_install/cancel_update_download/is_installer_install、三回调）。

**守卫**：workspace test 全绿（platform 18 含 FIPS 向量、ui-viewmodels 76 含 assets 解析与状态机迁移）+ installer workspace clippy/test 全绿 + 三 crate clippy 归零（`&PathBuf`→`&Path`、手写 Default 改派生均为 clippy 抓出）+ slint-viewer 四态渲染裁剪放大逐态验视（进度条比例、错误行 danger 色、启动态无按钮、无截断叠压）。**遗留**：镜像默认清单仍留白（D56 原话实测再定）——2026-09-02 实测当日本机代理出口断（连主源基线也超时，ghfast.top/gh-proxy.com/moeyy.xyz 三候选不可达），不具备实测条件，维持「GitHub 主源 + update_feeds.toml 覆盖」现状。

### D71 下载镜像层：并行测速择优 + 锚定校验 + 原始压轴（2026-09-02）

**背景与决策**：D70 下载直连 GitHub，国内「检查能通、下载吃力」。用户定盘：备用多个镜像源、下载时自动用最快。与 D56「不竞速」哲学的边界——竞速禁的是**全量并发下载**（白烧带宽与 GitHub 限流配额），测速只取每个候选前 64 KiB（Range 请求，8s 上限），总损耗 ≈ 候选数 × 64 KiB，换来「最快源」的真实信号。

**机制**：①候选 = 内置镜像前缀改写原始 URL（gh-proxy 系约定：前缀直拼）+ **原始 URL 永远压轴**——镜像全灭时行为与 D70 直连等价，镜像层不可能让更新变得更不可用。②并行测速（`thread::scope`，WinHTTP 同栈新 `probe_sample`：Range 取样、不支持 Range 的对端回 200 也只读取样即止、416 按探测失败顺延），健康者按取样耗时升序、失败者不淘汰原序垫底；唯一候选（镜像被关）跳过测速直连。③**SHA256SUMS 锚定原始源**：信任锚不跟着镜像走——SUMS 永远先试原始源（清单仅几百字节，10s 上限），失败才降级「经镜像取得」并留 warn；锚住 SUMS 后，任何镜像的滞后/被篡改内容都会在哈希比对处现形并**顺延下一候选**（哈希不符也是换源信号，日志记「内容疑似滞后或被篡改」）。残余风险（镜像 SUMS 与镜像安装包同源同滞后的自洽旧版对）如实留档：批 C ed25519 签名前无解，降级告警保可诊断。④清单缺条目、本地摘要计算失败是恒定错误，换源无解，直接失败不空转。

**配置**：`update_feeds.toml` 增 `download_mirrors = [...]` 键，与 `feeds` 键语义**有意不同**——键存在即全量采纳（**空数组 = 关闭镜像加速只直连**，一等用户意图），键缺失回落内置三镜像（ghfast.top / gh-proxy.com / github.moeyy.xyz），文件写坏回落内置并告警（坏配置不弄哑更新链路）。内置默认系用户决策直接启用（不再等逐一手测），安全性由「测速落选 + 下载失败顺延 + 原始压轴 + 哈希兜底」四层护栏承接，不依赖镜像的可用性承诺。UI 阶段文案：测速中「正在选择最快下载源…」→「已选择最快源 ghfast.top（312ms）」→ 下载态进度条接管。

**落点**：platform lib.rs（trait 增 `probe_sample`）/win32.rs（WinHTTP Range 取样实现，第三份请求机械展开——故意不抽公共助手，不给 D56 已验证路径卷重构风险）；ui-viewmodels update_apply.rs（`DEFAULT_DOWNLOAD_MIRRORS`/`load_download_mirrors`/`mirror_candidates`/`mirror_label`/`rank_by_probe` 五件纯逻辑）；app-ui main.rs（下载线程重构：并行测速 + SUMS 锚定 + 逐候选下载换源，恒定错误与可换源错误分流）。

**守卫**：三 crate clippy 归零 + workspace 测试全绿（ui-viewmodels 37 更新域测例，新增清单语义四态/候选改写过滤/标签解析/排名稳定性）。

**守卫**：workspace test 全绿（classify_spec 16 例、settings_spec 6 例含 describe/ specs 同构断言）+ clippy 归零（抓出 settings_path 死字段连带清理）+ fmt；slint-viewer 渲染弹窗实证复选框移除后布局完好（候选列表/预告行/按钮行无空洞）。

### D72 一键发版：installer 版本元数据推导自主 workspace + scripts/release.ps1 单命令发版序（2026-09-02）

**背景与决策**：D57 定的发版序（升版→推 main→打 tag→推 tag）有两个人工缝：①installer 独立 workspace 的内部版本号冻结在 0.1.0（原注释「勿当第二版本真相源」），而 winresource 缺省 FileVersion 取本 crate 的 CARGO_PKG_VERSION——每个安装包内嵌的版本元数据都是陈旧值；②升版涉及手改 Cargo.toml + 手跑 cargo update + 手打 tag，漏一步即返工。用户定盘：**版本号一处维护、发版一条命令**。旧决策的问题不在「防两源漂移」的动机，而在手段——宣布第二字段无效并冻结，等于容忍它永远撒谎；正确形态是唯一真相源 + 其余全部推导。

**决策**：
1. **installer 版本元数据改推导**：installer/build.rs 编译期读 `../Cargo.toml` 的 `[workspace.package].version`，显式 `.set("FileVersion"/"ProductVersion")`（gnu/msvc 两分支同改）；读取失败回落 CARGO_PKG_VERSION + `cargo:warning`（构建不炸但元数据陈旧，warning 必须可见）。⚠ 一旦 build.rs 打印任何 `cargo:rerun-if-changed`，cargo 就**只**按列出的路径重跑 build.rs——故主清单与 `../assets/app-icon.ico` 必须一并钉入，否则换图标不再触发资源重嵌。installer/Cargo.toml 的 version 字段就此永久冻结 0.1.0：从「被忽略的第二真相源」变成「无需维护的派生消费点」，注释按新语义改写。
2. **scripts/release.ps1 <version> [-Subject 主题] [-DryRun]**：校验段全过才动手（x.y.z 格式→树干净→在 main→本地/远程 tag 不存在→旧≠新）→ 写 Cargo.toml（根清单第一条 `^version` 行即 [workspace.package] 的，与 package.ps1 同一假设、两处须同步改）→ cargo update --workspace → commit（显式路径 Cargo.toml+Cargo.lock）→ push main → annotated tag（vX.Y.Z：主题）→ push tag。CI 的 tag==Cargo.toml 版本强校验继续兜底错版。release.ps1 文件必须 **UTF-8 带 BOM**（PS 5.1 对无 BOM 按 GBK 解码，中文全乱码），且只可用 Write/Edit 工具改写。

**验证**：package.ps1 全流程重建 0.1.4，artifacts 安装包内嵌 FileVersion/ProductVersion 实测 = 0.1.4（gnu windres 8.3 短路径与 .set 互不干扰）；release.ps1 格式错误/树不干净/DryRun 计划三路径实测通过（真实写推路径 v0.1.5 首跑）。附带事故档：installer 经 include_bytes! 以 `dist.tar.gz` 为构建输入——它虽是可再生中间产物，但脱离 package.ps1 单跑 `cargo build`（installer 目录）也需要它，**清理工作区时不可删**（2026-09-02 清理事故已由 package.ps1 重建修复）。

**落点**：installer/build.rs、installer/Cargo.toml（注释改口径）、scripts/release.ps1（新）、.gitignore（/.mimosa/ 工具运行态）。

### D73 更新检查触发补全：运行中间隔档（2026-09-02）

**背景与决策**：D56 的静默检查只在启动时评估一次「到期与否」——窗口常驻多日不再触发，用户点名补「进入应用自动触发 + 时间间隔触发」。查明现状：启动档已有（`auto_update_check` 开关 + `last_check_unix` 24h 节流双门），手动档已有；缺的只有运行中间隔。

**决策**：Slint Repeated Timer 每 600s 醒一次，只做纯时钟判断——到期（`silent_check_due(last, now, auto_enabled)`）且不在途（`is_checking`）才发起静默检查。与「禁 Timer 轮询」纪律的边界：该纪律禁的是**等待外部条件**的轮询重试（窗口就绪/进程退出类，一律 WinEvent/回调）；每日更新检查是时钟驱动的日程任务，节拍器轮询的对象是时钟本身，网络请求仍 24h 至多一次，10min 粒度只影响触发相位不影响频率。到期谓词下沉 ui-viewmodels 成纯函数，启动档与间隔档共用同一把尺（避免两处判定漂移）；手动检查发起即刷新节流钟（D56「按发起过计」语义不变），下一次自动检查顺延满 24h；开关关闭时节拍器空转，每拍一次布尔判断零成本。

**落点**：ui-viewmodels update_check.rs（`silent_check_due` + 边界单测：恰满 24h 到期、差 1 秒不到、last=0 必到、时钟回拨 saturating 不误报、开关关永不触发）；app-ui main.rs（`UPDATE_CHECK_TIMER` thread_local + 启动档与间隔档共用 `spawn_due_check` 闭包——闭包内 `feeds_path` 必须 clone，按值捕获会 FnOnce 化、节拍器第二拍即断）。

**守卫**：update_check 域 24 测例全绿（新增边界 1 例）+ workspace test 全绿 + clippy 归零 + fmt。

### D74 上框锚点换底部锚定模型 + 聚焦现场留痕（2026-09-04）

**背景**：测试用户（张越机）拼多多上框「报成功但没落框」——日志 5 次全链路走完、verified=false、focus 69~81ms，本地同版本不复现。定点复盘得出根因：**比例锚 × 定高底栏**。聊天应用底栏（工具栏/合规提示条）是固定物理高度（PDD 实测 55~150 物理px、千牛 80~295），比例 y 随窗高线性上漂；测试用户窗口更高，0.92×H 越过输入框落进消息列表，Ctrl+V 落空但全程报成功。本地无法复现只因窗口高度恰好不同——同版本同环境不可复现的承诺在几何层就已破产。

**决策一（锚点模型）**：新增 `input_anchor_bottom = { x_ratio, y_from_bottom }`——客户区底边向上的物理像素距离，按 96 DPI 基准声明、运行时按 `GetDpiForWindow` 实时缩放；声明后优先于 `input_anchor`。锚定底边与窗高完全解耦，这是对「定高底栏」这一根因的直接对击。越界值报错不夹紧（沿用 InvalidAnchor 哲学），y_from_bottom 合法域 1..=500（远超任何合理底栏纵深）。实测回填：千牛 `0.394/127`、pdd `0.49/66`（见 profiles.builtin.toml 注释）；**微信未量取**（登录页需手机侧确认，自动点击进不去）保留比例锚，量取前不动；pdd/千牛端到端实贴 E2E 因账号收回/锁屏未能跑，待补。

**决策二（聚焦现场留痕）**：`focus_input` 从裸 `FocusOutcome` 升级为 `FocusReport`（逐级 attempts：step/outcome/click 现场几何/settle 结局）。锚点步的 ClickEvidence（几何形态、屏幕落点、客户区、DPI）与 settle 证据进 Info 日志「上框聚焦现场」——远程排障只需 diff 两台机器的各两行，不再有盲区。**verified 语义升级**：锚点路径从恒 false 改为「单击后 settle=Observed（焦点/插入符事件到达）即 true」——这是锚点路径唯一的事后可证证据，旧实现把它埋在 debug 日志里丢弃。同时补三处盲区：启动行带版本号（安装版滞后误报的识破点）、matcher 拒绝留痕（debug 级，弹窗顶替类故障可回溯）、探针 `--anchor-bottom` 走产品代码路径。

**实测否决记录（P1/P2 元素级定位，不采用）**：曾设想的「a11y 激活后元素级定位」被数据否决——WM_GETOBJECT(OBJID_CLIENT) 打在 Chrome_RenderWidgetHostHWND 上不会让 CEF 物化 web 内容 DOM 树（PDD/千牛激活后 UIA 后代仅 10~11 个浏览器级表面，无 Edit 元素；MSAA 递归同样颗粒无收）。微信 Qt 同样不暴露。三目标可写候选数均为 0，元素级定位在这批目标上**结构性不可用**，bottom-up 锚点是当前技术栈下最准的可达定位。

**⚠ 2026-09-04 修正（D75 调研推翻本段结论）**：上述「结构性不可用」只对了一半——OBJID_CLIENT 消息探针只触发 Chromium 渐进式无障碍的**最低档**（kNativeAPIs）。升级钩子在 `ax_platform_node_win.cc` 的属性 getter 内部：对拿到的 IAccessible **真实跨进程 COM 调用** `get_accName` + `get_accDefaultAction`（+ 可选蜜罐 WM_GETOBJECT objid=1，须在 Name 之后）即触发 AXMode 升到 kAXModeBasic，renderer 开始建 DOM 树。**千牛实机验证成功且可复现**：UIA 后代 8 → 97，`买家账号` 等 web Edit 元素物化（`focus_probe --a11y-activate`）；同序列在拼多多未生效（分目标差异）；树随窗口失前台 ~10s 内塌回，须即用即触发。详见 D75 调研报告。

**DPI 双空间教训**：同一窗口探针读到逻辑 1230×800、PrintWindow(PMv2) 读到物理 1845×1200，1.5× 整除无冲突但也无提示——后续任何坐标取证先问「这是哪个空间」，GetDpiForWindow 是换算桥。

**落点**：platform lib.rs/win32.rs（BottomUpAnchor/AnchorGeometry/ClickEvidence/FocusAttempt/FocusReport、click_point_in_client、window_dpi）、targets profile.rs/matcher.rs、pipeline lib.rs、profiles.builtin.toml（千牛/pdd 底部锚 + 头注释）、app-ui main.rs（版本行）、ui-viewmodels target_bar_vm（经 targets matcher 留痕）、focus_probe（--anchor-bottom + --a11y 深探）、tests（锚点数学 3 例、画像解析 4 例、verified 升级 1 例改写）。

**守卫**：platform/targets/pipeline/ui-viewmodels 全测绿（含 real_im_profiles 真实 TOML 集成校验）；fmt/clippy 待收尾提交前过门。锁屏期间实测到正信号：激活失败时前台守卫正确拒点（click=None → Unavailable），产品降级仅复制、绝不盲注。

### D75 去坐标化定位·解锁后真机实验（2026-09-04，调研落地日）

**承接 D75 调研**：解锁桌面后对候选机制逐项真机验证（探针 `focus_probe` 新增 `--paste-element` / `--nav` / `--activate-dump` / `--wx-uia`，提交 67fdd2a 后续提交）。

**千牛（接待中心 525668）——元素级全链路打通（SUCCESS ×2 可复现）**：
- 流程：产品激活器拉前台 → a11y 激活协议（COM Name→DefaultAction→蜜罐）→ UIA 枚举 99 元素 → 锁定可写 Edit「买家账号」（aid=buyer）→ `UIA SetFocus` → SendInput Ctrl+V → 重枚举 ValuePattern 读回验证。剪贴板标记连续落框（value 三连拼接逐轮可见），**全程零坐标零点击**。
- **焦点自恢复实锤**（实验②）：跳过显式 SetFocus，仅激活后 Ctrl+V 精准落进上次焦点元素——Qt/CEF 的 web 内部焦点跨失活保持（Blink document focused element），`has_focus=true` 可读复核。产品化时「激活→读焦点→只在已可写时注入」比盲 SetFocus 更稳。
- RWH SetFocus（实验③）：AttachThreadInput+SetFocus(RWH) 成功把焦点切到消息聊天 Document 级，但因未打开会话无 composer 可落——**composer/会话列表在 kAXModeBasic 下不物化**（懒物化，仅焦点区域子树可见），需开会话后复测。
- 解锁后产品锚点点击恢复有效（`settle=Observed` 11ms），合成鼠标/键盘在千牛正常。

**微信 4.1.13（Qt51514 mmui）——全部通道未通，需用户协作复测**：
- UIA 树默认不存在（仅 2 元素：窗壳+MMUIRenderSubWindowHW）；SPI_SETSCREENREADER 置位被系统静默回落（二次确认死亡）；WM_GETOBJECT（顶层+mmui 子窗）返回 0x0 无 provider；GetFocusedElement 不触发构建。
- WM_APPCOMMAND(APPCOMMAND_PASTE, device nibble=0) Send/Post 均无效果（源码验证的 Qt Key_Paste 通道在此版本不生效——焦点未进 composer 是混杂变量）。
- 合成键盘（Ctrl+V/Ctrl+F/Tab/字符 unicode+scancode）4/4 系统层送达但对 mmui 无效；合成鼠标移动被真实输入流覆盖（用户在机操作）。**根因无法二分**（A=微信过滤 injected 输入 vs B=焦点从未进 composer）——继续抢前台会干扰用户真实接待，实验中止。
- 待办（用户协作）：微信置前+手点 composer 后，跑 `focus_probe --paste-element` 键盘路径，即可二分 A/B；同时补 D74 遗留的微信 composer 锚点测量。

**产品启示（待实施评估，本轮不动产品代码）**：千牛目标可升级为「a11y 元素级 SetFocus 主路径 + 锚点点击后备」双层；微软雅黑暗线——verified 语义可再升级（ValuePattern 读回内容比对，粘贴后自证）。

**⚠ 2026-09-04 收尾实验修正（用户要求演示「粘贴进聊天输入框」后，台账 research/element-targeting-log.md B1~B8）**：
- **「买家账号 SUCCESS」与「composer 可贴」是两回事**：元素级链路 SUCCESS ×3 的落点全是工单面板的`买家账号` Edit（当时唯一物化的可写 Edit）。聊天 composer 的物化问题在会话打开后复测，结论反转——
- **composer 结构性不可见（决定性事实）**：会话开着、输入框在屏幕上时，dump 全树 99~100 元素，消息聊天 Document 下只有消息历史媒体节点，composer 无任何形态存在；设焦四路全灭（UIA 可写 Edit=选中买家账号；win32 RWH SetFocus=停 Document/50026 容器；Tab 键盘导航×25=被 Qt 焦点链吞掉；UIA SetFocus Document=同停容器）；**点击获焦+粘贴成功后 composer 依然不进树**。即 recent.html 该输入框无论焦点状态都不暴露 UIA 节点——「UIA 选中/设焦/ValuePattern 读回」对 composer 三路皆不可用，无解。
- **B8 产品时刻复现成功**：点击输入框获焦 → 不激活不设焦直接 Ctrl+V → marker 落框（截图证实）。焦点自恢复链路在 composer 上成立，这才是产品主路径；「UIA 元素级 SetFocus 主路径」提案据此否决——composer 不可达，锚点点击后备继续保留。
- verified 语义修正：千牛 composer 的 ValuePattern 读回**不可用**（永不在树），verified 升级只能靠 settle=Observed/视觉证据；ValuePattern 读回仅对工单类表单 Edit 有效。
- 环境事实补记：后台进程 SetForegroundWindow 被前台锁拒（AttachThreadInput 技巧解，置前后必须复核前台再点击）；2560×1600 全屏截图在读图工具中缩放显示（2000 宽），量取坐标须 ×1.28 换算物理（本轮最贵教训，对策=用已知物理坐标的裁剪放大图反推）。





