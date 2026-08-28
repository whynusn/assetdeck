# PRD — 上框延迟：事件驱动取代固定睡眠与轮询

> 依据：用户第 N+25 轮指令 · `DECISIONS.md` D15/D18/D21 · `AGENTS.md` 红线 · 08-23-m8-target-routing 真机实测数据。

## Goal

把「双击素材 → 素材出现在目标 IM 输入框」这条链路上**属于我们自己**的等待时间，从
基于时钟的固定睡眠与轮询，改成基于可观测系统事件的等待，并给出「窗口亮起 → 图片进框」
这一段的真实分项账目，说清哪一部分我们能优化、哪一部分在 IM 进程内。

用户原话两条：
1. 「sleep 和轮询这种基于时间的操作都是不可取的，我需要巧妙地事件驱动。」
2. 「我手动操作中，体感延迟最大的是 IM 界面打开后到图片上框这一段等待时间。」

## Confirmed Facts（本机实测，非推演）

当前一次上框里我们自己占 **350~550ms**，构成如下（PowerShell 复现同样 Win32/UIA 调用所得）：

| 阶段 | 位置 | 实测 | 性质 |
|---|---|---|---|
| 剪贴板写入 | `crates/pipeline/src/lib.rs:209` | 亚毫秒 | 已最优 |
| 前台确认轮询（10ms 步进） | `crates/platform/src/win32.rs:411-421` | 21~40ms | **轮询** |
| `settle_ms` 固定睡眠 | `win32.rs:388` / `win32.rs:403` | 微信 150ms / 千牛 120ms | **固定睡眠** |
| UIA `GetFocusedElement` 判定 | `win32.rs:1011` | 首次 79ms（COM 冷启动），预热后 5ms | 每次重建 COM 实例 |
| UIA `SetFocus` 全子树扫描 | `win32.rs:1037` | 微信 22~27ms、千牛 83ms | **注定失败**（候选数实测 0） |
| 锚点单击后固定沉降 | `win32.rs:1201` `ANCHOR_CLICK_SETTLE_MS=60` | 60ms | **固定睡眠** |
| 就绪度探测（又一次 UIA 往返） | `lib.rs:271` → `win32.rs:510` | 3~30ms | 结果只用于置 `verified` |
| Ctrl+V 注入 | `lib.rs:297` | 亚毫秒 | 已最优 |

另外两条已查证的事实：
- Ctrl+V 之后，微信 4.0 自己要解码 **0.7~1.4MB 的 `CF_PNG`** 并渲染输入框卡片
  （`samples/library/objects/*/paste.png` 实测 712690 / 1053370 / 1434465 / 1461400 字节）。
  这段发生在 IM 进程内，我们无法直接加速。
- `Win32ForegroundObserver`（`win32.rs:447`）已经用 `SetWinEventHook` 拿到了事件，
  但 `next_foreground()` 是 `try_recv()`，被 `crates/app-ui/src/main.rs:289-308` 的
  750ms `Timer` 轮询消费——已有钩子却仍在轮询。
- 整条上框序列同步跑在 Slint 事件循环的 `on_double_clicked` 回调里（`main.rs:346`）。

## Requirements

R1 **等待即订阅**。链路上每一处「等目标应用做完某件事」都必须先订阅对应系统事件、
再触发动作、然后阻塞在事件上。时间只能作为**兜底上限**出现，不得作为等待手段本身。

R2 **零轮询**。不得以固定步长反复查询系统状态来判断事实是否发生（`wait_for_foreground`
的 10ms 循环、`app-ui` 的 750ms `Timer` 都在此列）。

R3 **不做注定失败的工作**。微信/千牛的 UIA 子树里可写 Edit/Document 候选数实测为 0，
这条降级级别在这两个画像上必然失败。跳过它必须是**画像声明的数据**，不是代码里的 if 分支
或运行时猜测（与 D18 `paste_sends` 同一建模方式）。

R4 **COM 实例复用**。`IUIAutomation` 不得在一次上框内被重复 `CoCreateInstance`。

R5 **就绪度探测退出热路径**。`uia_shallow`（内置默认）下注入前只做微秒级的否证检查
（窗口存活、非模态阻塞），UIA 往返不得挡在 Ctrl+V 前面。`uia_strict` 语义不变：
仍必须在注入前拿到证明。

R6 **给出尾段真相**。必须实测并记录「Ctrl+V 送达 → 图片出现在输入框」的耗时，
以及它与剪贴板载荷体积的关系，让「这一段还能不能优化」有据可依。

R7 **红线不变**。`auto_send` 默认 false；产品双击走 `paste_targeted()`；注入序列不含 `0x0D`；
聚焦动作只允许 UIA `SetFocus` 或画像锚点单次左键单击。

## Acceptance Criteria

可观察结果：
- [ ] AC1 `crates/platform/src/win32.rs` 的上框路径中不存在 `std::thread::sleep` /
      `Sleep(` / 固定步长轮询循环；由一条守卫测试锁定，不靠人眼复查。
- [ ] AC2 真机（微信 hwnd 2163916 文件传输助手 / 千牛 hwnd 721614 接待中心）双击真实素材
      仍然上框成功，且我们自己占用的时间相对改造前**至少下降 40%**（实测数字对比记录在
      `research/latency-ledger.md`）。
- [ ] AC3 `app-ui` 不再用 `Timer` 轮询目标条状态；目标条刷新由窗口前台/生命周期事件驱动。
- [ ] AC4 画像可声明聚焦策略顺序；微信/千牛声明后不再执行 UIA 全子树扫描。
- [ ] AC5 `uia_shallow` 下注入前不再发生 UIA 往返；`uia_strict` 下仍然先证明后注入
      （由 pipeline mock 测试断言两种模式的调用序列差异）。
- [ ] AC6 `research/latency-ledger.md` 给出改造前/后分项账目与 Ctrl+V 尾段实测。

红线（任一破则不可交付）：
- [ ] AC7 注入序列不含 `0x0D`；三道既有守卫测试全绿。
- [ ] AC8 `cargo fmt --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
      `cargo test --workspace` 全绿。
- [ ] AC9 `platform` trait 层仍零依赖零 `cfg` 门；Win32 具体类型仍只在
      `win32_runtime_deps()` 内构造（D16，`layering_guard.rs` 守卫）。
- [ ] AC10 内存红线不破：新增的常驻事件线程不得吃穿空闲 100MB 预算。

## Out of Scope

- 自动发送。本任务不碰 `send()`，不引入任何 Enter 合成。
- 静默降低素材画质来缩小剪贴板载荷。载荷体积与 IM 解码耗时的关系要实测记录，
  但「为了快而降质」是产品决策，需另行征得用户同意后单独立项。
- 把整条上框序列挪到工作线程（UI 冻结问题）。本任务用专用事件线程消除轮询，
  上框序列仍同步跑在 Slint 回调里；挪线程是独立可验收的改造，另立任务。
- 热键唤起、自定义目标持久化（属 08-23-m8-target-routing 的既有缺口）。
