//! 平台抽象层：剪贴板写入、前台窗口观测、按键注入。
//!
//! 分层纪律（spec/platform）：
//! - 本文件为 trait 层，**零依赖、零条件编译门**——依赖方（pipeline/ui-viewmodels）
//!   可脱离 Win32 编译与测试；Win32 真实实现整体收拢在 `win32` 模块
//!   （仅-Windows 门在该文件内部），由二进制入口负责选择注入。
//! - trait 方法面向意图命名（write / foreground / is_alive / inject），
//!   签名不外泄平台术语；v2 新增平台 = 新增 `src/<platform>/` 模块，trait 不动。

use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;

/// 剪贴板载荷：变体与目标剪贴板格式一一对应（协商规则见 pipeline::negotiate）。
///
/// DIB/PNG 变体只接受上游提供的**已编码**字节——「UI 进程不解码」红线，
/// 本 crate 及依赖方均不得引入图像解码依赖。
///
/// 字节/文本变体是**借用（Cow::Borrowed）**而非拷贝：载荷与 AssetPayload 同源，
/// 写入端直接从源切片搬进系统剪贴板块（GlobalAlloc + memcpy 是剪贴板唯一必需的
/// 一次拷贝），协商层不再 to_vec 全量复制——大图上框热路径省一份整块内存搬运。
#[derive(Debug, Clone)]
pub enum ClipboardPayload<'a> {
    /// 文件列表（→ 系统 HDROP 格式）。
    Files(Vec<PathBuf>),
    /// PNG 编码字节（→ 注册格式 "PNG"）。借用上游已编码切片。
    Png(Cow<'a, [u8]>),
    /// DIB 字节（→ CF_DIB）。存在但不参与默认路由，仅供上游已持字节时的专用路径。
    Dib(Cow<'a, [u8]>),
    /// Unicode 文本（→ CF_UNICODETEXT）。借用上游文本。
    Text(Cow<'a, str>),
}

// 跨生命周期比较：借用自不同源（不同 'a/'b）的载荷在测试断言与日志比对里
// 需要直接相等判定，派生只能生成同生命周期实现，这里手工补上。
impl<'a, 'b> PartialEq<ClipboardPayload<'b>> for ClipboardPayload<'a> {
    fn eq(&self, other: &ClipboardPayload<'b>) -> bool {
        match (self, other) {
            (ClipboardPayload::Files(a), ClipboardPayload::Files(b)) => a == b,
            (ClipboardPayload::Png(a), ClipboardPayload::Png(b)) => a == b,
            (ClipboardPayload::Dib(a), ClipboardPayload::Dib(b)) => a == b,
            (ClipboardPayload::Text(a), ClipboardPayload::Text(b)) => a == b,
            _ => false,
        }
    }
}
impl<'a> Eq for ClipboardPayload<'a> {}

impl ClipboardPayload<'_> {
    /// 物化为自持（'static）载荷：测试替身记录操作日志等场景需要脱离
    /// 借用源存留载荷。生产路径（写入端）不需要它——写入发生在借用仍存活的作用域内。
    pub fn into_owned(self) -> ClipboardPayload<'static> {
        match self {
            ClipboardPayload::Files(paths) => ClipboardPayload::Files(paths),
            ClipboardPayload::Png(bytes) => ClipboardPayload::Png(Cow::Owned(bytes.into_owned())),
            ClipboardPayload::Dib(bytes) => ClipboardPayload::Dib(Cow::Owned(bytes.into_owned())),
            ClipboardPayload::Text(text) => ClipboardPayload::Text(Cow::Owned(text.into_owned())),
        }
    }
}

/// 前台窗口句柄裸值。不把平台句柄类型泄进 trait 签名之外。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct WindowHandle(pub isize);

/// 平台无关的窗口矩形快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl WindowRect {
    pub fn has_area(self) -> bool {
        self.right > self.left && self.bottom > self.top
    }
}

/// 路由层识别目标所需的最小窗口信息。采集发生在平台实现，决策发生在 targets。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub hwnd: WindowHandle,
    pub exe_name: String,
    pub class_name: String,
    pub title: String,
    pub visible: bool,
    pub minimized: bool,
    pub rect: WindowRect,
    pub process_id: u32,
}

/// 平台探测出的阻塞事实。UI 文案和产品语义由上层映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessBlocker {
    NotLoggedIn,
    NoConversation,
    ReadOnly,
    ModalBlocking,
    Starting,
    WindowGone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessSignal {
    Ready,
    Inconclusive,
    Blocked(ReadinessBlocker),
}

/// 注入序列元素的「释放」相位标志（u16 最高位）。
///
/// [`KeyInjector::inject`] 以扁平 `&[u16]` 承载按键编排：每个元素是一次键事件，
/// 低 15 位为虚拟键码；本位为 0 表示按下、置 1 表示释放。
/// 选扁平序列而非封装方法，是为了让测试能直接断言注入序列内容（M6 测试语义）。
pub const KEY_UP: u16 = 0x8000;

#[derive(Debug)]
pub enum PlatformError {
    /// 剪贴板打开/写入/分配失败。
    Clipboard(String),
    /// 输入事件合成未全部送达（部分被系统或目标窗口拒绝）。
    Inject(String),
    /// 窗口枚举、观察、激活或状态查询失败。
    Window(String),
    Io(std::io::Error),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::Clipboard(msg) => write!(f, "剪贴板错误: {msg}"),
            PlatformError::Inject(msg) => write!(f, "输入注入错误: {msg}"),
            PlatformError::Window(msg) => write!(f, "窗口操作错误: {msg}"),
            PlatformError::Io(e) => write!(f, "IO 错误: {e}"),
        }
    }
}

impl std::error::Error for PlatformError {}

impl From<std::io::Error> for PlatformError {
    fn from(e: std::io::Error) -> Self {
        PlatformError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, PlatformError>;

/// 剪贴板写入端。实现负责把载荷落到系统剪贴板，失败时不留半成品状态。
pub trait ClipboardSink {
    fn write(&mut self, payload: &ClipboardPayload<'_>) -> Result<()>;
}

/// 原生文件对话框（打开/保存）。trait 化：实现收拢在 win32 模块（IFileDialog），
/// 壳层只表达意图。用户取消返回 `Ok(None)`，与「失败」语义区分。
pub trait FileDialogs {
    /// 选择文件夹（模态），title 为对话框标题。
    fn pick_folder(&self, title: &str) -> Result<Option<PathBuf>>;

    /// 选择已存在的文件（模态）。`filter` 形如 `"千牛素材包 (*.emo)|*.emo|所有文件 (*.*)|*.*"`；
    /// 段数为奇数时末组以规格兼任名称，空串表示不过滤。用户取消返回 `Ok(None)`。
    fn pick_open_file(&self, title: &str, filter: &str) -> Result<Option<PathBuf>>;

    /// 选择保存路径（模态）。`filter` 形如 `"Qianniu Emo (*.emo)|*.emo"`。
    fn pick_save_path(
        &self,
        title: &str,
        default_name: &str,
        filter: &str,
    ) -> Result<Option<PathBuf>>;
}

/// 注入前前台归属分类（D44）。前台漂移时决定能否安全地「再断言」前台：
/// 用户连点本应用面板（OwnProcess）或目标内部窗口抖动（SameAsTarget）时，
/// 用户的上框意图未变，再断言是安全的；第三方前台（Foreign）意味着用户已切走，
/// 必须尊重，降级为仅复制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForegroundRelation {
    /// 前台就是目标窗口本身。
    Target,
    /// 前台与目标窗口同属目标进程（目标内部多顶层表面抖动）。
    SameAsTarget,
    /// 前台属于本应用自身进程（用户连点了素材面板）。
    OwnProcess,
    /// 前台是无关第三方应用（用户已切走，不得抢回）。
    Foreign,
}

/// 前台窗口观测端：唤起面板时记录此刻前台窗口，注入前校验其仍存活（D12/D8）。
pub trait FocusWatcher {
    fn foreground(&self) -> WindowHandle;
    fn is_alive(&self, window: WindowHandle) -> bool;

    /// 前台窗口相对目标窗口与自身进程的归属（D44 注入前再断言的依据）。
    ///
    /// 默认 [`ForegroundRelation::Foreign`]：未知归属一律按「用户已切走」保守
    /// 处理，与「前台漂移即降级」的既有语义一致；事件驱动实现必须覆盖。
    fn foreground_relation(
        &self,
        _foreground: WindowHandle,
        _target: WindowHandle,
    ) -> ForegroundRelation {
        ForegroundRelation::Foreign
    }
}

/// 按键注入端。keys 为键事件序列，实现按序合成输入事件；
/// 序列编排（按下/释放相位、和弦顺序）由调用方负责。
pub trait KeyInjector {
    fn inject(&mut self, keys: &[u16]) -> Result<()>;
}

/// 顶层窗口枚举端。返回的是一次不可变快照，不把平台句柄之外的系统类型泄露出去。
pub trait WindowEnumerator {
    fn windows(&self) -> Result<Vec<WindowSnapshot>>;
}

/// 目标窗口激活端。返回 false 表示窗口存在但在超时内未成为前台。
pub trait WindowActivator {
    fn activate(
        &self,
        window: WindowHandle,
        confirm_timeout_ms: u64,
        settle_ms: u64,
    ) -> Result<bool>;
}

/// 常驻前台观察端。实现可由 WinEvent 或轮询驱动；状态机只消费快照。
pub trait ForegroundObserver {
    fn next_foreground(&mut self) -> Result<Option<WindowSnapshot>>;

    /// 注册一个「前台可能变了」的唤醒回调，返回是否接管成功。
    ///
    /// 事件驱动实现（Win32 泵）在前台事件到达时调用该回调，让 UI 线程立刻
    /// `next_foreground()` 取快照，而不必靠固定周期的 `Timer` 轮询。回调在泵线程上
    /// 触发，因此实现只应在回调里做「唤醒 UI 事件循环」这一件事，绝不可在其中阻塞或
    /// 再申请订阅（见 [`EventWait`] 的硬约束二）。
    ///
    /// 默认返回 `false`：轮询型实现与测试替身无需事件唤醒，调用方据此保留退路 `Timer`。
    fn set_wakeup(&mut self, _wakeup: Box<dyn Fn() + Send + Sync>) -> bool {
        false
    }
}

/// 输入框就绪度探测端。探测不可用或超时必须返回 Inconclusive，不得伪装成阻塞。
pub trait ReadinessProbe {
    fn probe(&self, window: WindowHandle, timeout_ms: u64) -> ReadinessSignal;

    /// 微秒级否证：只回答「有没有明确的阻塞事实」，绝不做 UIA 或任何跨进程往返。
    ///
    /// 存在的理由：`probe` 的 UIA 往返在微信/千牛上实测永远只能给出 `Inconclusive`
    /// （见 DECISIONS D15），却要占 3~30ms 的热路径。非严格就绪档只需要否证能力，
    /// 因此分出这个不花钱的入口。故意不给默认实现，逼每个实现显式表态。
    fn blockers(&self, window: WindowHandle) -> ReadinessSignal;
}

/// 聊天输入框在窗口内的相对位置（客户区宽高比例，0.0..=1.0）。
///
/// 为什么是比例而不是绝对坐标：IM 窗口尺寸随用户拖拽变化，但输入框在版式中的
/// 相对位置稳定。比例锚点由目标画像声明，平台层只负责换算与安全校验。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusAnchor {
    pub x_ratio: f32,
    pub y_ratio: f32,
}

/// 一次「把键盘焦点送进聊天输入框」的结果。
///
/// 语义与 [`ReadinessSignal`] 对称：`Unavailable` 是「没能证明拿到焦点」，
/// 不是「证明拿不到」——上层据此决定注入后是否标记 verified，而不是直接中止。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusOutcome {
    /// 当前焦点已经在目标进程的可写输入框上，未做任何动作。
    AlreadyEditable,
    /// 通过 UIA `SetFocus` 拿到焦点。
    FocusedByUia,
    /// 通过点击画像声明的锚点拿到焦点。
    FocusedByAnchor,
    /// 两条路都不可用（无锚点、UIA 不暴露输入框、或锚点被其它窗口遮挡）。
    Unavailable,
}

/// 一个聚焦级别。顺序由目标画像声明（[`FocusPlan::steps`]），平台层只按序执行。
///
/// 为什么是数据而不是代码分支：某些级别在某些目标上**注定失败**——微信/千牛的
/// UIA 子树里可写 Edit/Document 候选数实测为 0，`UiaSetFocus` 在这两个目标上
/// 只是纯损耗（微信 22~27ms、千牛 83ms）。哪个级别有效属于目标的观测事实，
/// 与 `paste_sends` 同一性质，因此建模成画像级数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusStep {
    /// 检查当前焦点是否已在目标进程的可写控件上，不做任何动作。
    AlreadyEditable,
    /// 在目标窗口的 UIA 子树里找可写控件并 `SetFocus`。
    UiaSetFocus,
    /// 对画像声明的锚点做单次左键单击。无锚点时该级别自动跳过。
    AnchorClick,
}

/// 一次聚焦尝试的完整计划：按 `steps` 顺序降级，直到某级别被验证成功。
#[derive(Debug, Clone, PartialEq)]
pub struct FocusPlan {
    pub steps: Vec<FocusStep>,
    pub anchor: Option<FocusAnchor>,
}

/// 聊天输入框焦点获取端。
///
/// 红线：实现只允许做「移动焦点」这一类动作——UIA `SetFocus` 或对画像声明锚点的
/// **单次左键单击**。禁止合成任何键盘事件（尤其 Enter），禁止点击未声明的位置。
pub trait InputFocuser {
    fn focus_input(&self, window: WindowHandle, plan: &FocusPlan) -> FocusOutcome;
}

/// 一次事件等待的结局。
///
/// 语义与 [`ReadinessSignal`] / [`FocusOutcome`] 一致：只有 `Observed` 是「证明发生了」，
/// 另两种都是「没能证明」，上层据此降级标记而不是中止上框。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// 目标事件在上限内到达，`elapsed_ms` 是从订阅建立到事件到达的实测毫秒数。
    Observed { elapsed_ms: u64 },
    /// 到达上限仍未观测到目标事件。这是「没能证明」，不是「证明没发生」。
    CappedOut,
    /// 事件源本身不可用（钩子未装上、泵线程已退出）。
    Unavailable,
}

/// 一个已建立的事件订阅，等待其目标事件到达。
///
/// 硬约束一：**必须先订阅再动作**。先 `SetForegroundWindow` 再订阅会漏掉在这两步之间
/// 到达的事件，退化成等满上限。所有调用点的形状必须是「订阅 → 读一次当前状态 →
/// 发起动作 → wait」。中间那次状态读取覆盖「动作发起前目标就已满足」的情形，
/// 那时不会有任何新事件到来。
///
/// 硬约束二：**唤醒回调内禁止再订阅**。订阅表由泵线程与调用线程共用一把锁，
/// 在事件扇出路径上再申请订阅会死锁。
pub trait EventWait {
    /// 阻塞至目标事件到达或耗尽 `cap_ms`。`cap_ms` 是上限而不是睡眠时长：
    /// 事件先到就立刻返回，这是本次改造把「睡满固定毫秒」换成「等到就走」的支点。
    fn wait(&mut self, cap_ms: u64) -> WaitOutcome;
}

/// 窗口事件源。实现负责把操作系统的窗口事件流按目标窗口过滤后投递给订阅方。
pub trait WindowEventSource {
    /// 订阅「`window` 成为前台窗口」。
    fn await_foreground(&self, window: WindowHandle) -> Box<dyn EventWait>;

    /// 订阅「`window` 所属进程出现可输入表面的迹象」：焦点落在其子控件上，
    /// 或其内部出现插入符位置变化。两者任一到达即视为观测到。
    ///
    /// 为什么不是「输入框已就绪」这么强的断言：按 D15，微信/千牛都不暴露可查询的
    /// 输入框元素，我们能观测到的只有这些迹象。观测到就早走，观测不到就等满上限后
    /// 照旧注入 —— 与改造前「睡满 settle_ms 再注入」相比只会更早，不会更晚。
    fn await_input_surface(&self, window: WindowHandle) -> Box<dyn EventWait>;
}

// 平台实现模块：声明本身不带条件门，「仅 Windows」的门在该文件内部，
// 保证本 trait 文件可被逐字 grep 验证纯净（无门、无平台 crate 引用）。
pub mod win32;