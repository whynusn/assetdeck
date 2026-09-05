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
use std::path::{Path, PathBuf};

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
    /// 网络请求失败（DNS/连接/超时/HTTP 状态非 2xx/响应体超限或非法编码）。
    Network(String),
    /// 哈希/摘要计算失败（D70 更新包校验；BCrypt CNG 侧错误）。
    Crypto(String),
    /// 外部链接或程序经系统默认方式打开失败（ShellExecute 返回值 ≤ 32）。
    External(String),
    Io(std::io::Error),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::Clipboard(msg) => write!(f, "剪贴板错误: {msg}"),
            PlatformError::Inject(msg) => write!(f, "输入注入错误: {msg}"),
            PlatformError::Window(msg) => write!(f, "窗口操作错误: {msg}"),
            PlatformError::Network(msg) => write!(f, "网络错误: {msg}"),
            PlatformError::Crypto(msg) => write!(f, "哈希计算错误: {msg}"),
            PlatformError::External(msg) => write!(f, "外部打开失败: {msg}"),
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

/// OS 拖入文件的接收端（D49）。win32 侧在主窗口 HWND 上注册 IDropTarget；
/// Drop 回调经注册线程的消息泵派发（= UI 线程），实现方拿到完整路径列表
/// （文件与目录混排，过滤语义在上层 VM）。Send+Sync：注册后由 COM 持有。
pub trait FileDropSink: Send + Sync + 'static {
    fn files_dropped(&self, paths: Vec<std::path::PathBuf>);
}

/// 原生文件对话框（打开/保存）。trait 化：实现收拢在 win32 模块（IFileDialog），
/// 壳层只表达意图。用户取消返回 `Ok(None)`，与「失败」语义区分。
pub trait FileDialogs {
    /// 选择文件夹（模态），title 为对话框标题。
    fn pick_folder(&self, title: &str) -> Result<Option<PathBuf>>;

    /// 选择已存在的文件（模态）。`filter` 形如 `"千牛素材包 (*.emo)|*.emo|所有文件 (*.*)|*.*"`；
    /// 段数为奇数时末组以规格兼任名称，空串表示不过滤。用户取消返回 `Ok(None)`。
    fn pick_open_file(&self, title: &str, filter: &str) -> Result<Option<PathBuf>>;

    /// 多选打开（D49 主导入混选素材+.emo）。默认退化为单选向量——只支持
    /// 单选的后端自动可用，无需每个实现方陪跑。
    fn pick_open_files(&self, title: &str, filter: &str) -> Result<Option<Vec<PathBuf>>> {
        Ok(self.pick_open_file(title, filter)?.map(|path| vec![path]))
    }

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
///
/// 注意（D74）：top-down 比例有一个结构性失效模式——底部固定像素高度的
/// 工具条/提示条会把「比例」和「输入框」解耦，窗口越高，同一比例落点离
/// 输入框越远（2026-09-03 真机实测：客户区高 1185 时 y=0.92 命中输入区，
/// 高 ~1400+ 时同一比例落进上方消息列表）。新目标/新测量请用
/// [`BottomUpAnchor`]，本类型仅为兼容既有画像保留。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusAnchor {
    pub x_ratio: f32,
    pub y_ratio: f32,
}

/// 底部锚点（D74）：x 仍为比例，y 是**距客户区底边**的 96-DPI 逻辑像素。
///
/// 为什么 bottom-up 稳定：输入框总是贴着底部固定高度的工具条/提示条排布，
/// 底边到输入框中心的像素距离随窗口高度几乎不变（实测拼多多 1185 高客户区
/// 输入区底距 55~150px，千牛 80~295px），而 top-down 比例随高度漂移。
/// 平台层按目标窗口的实时 DPI 把逻辑像素换算成物理像素。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BottomUpAnchor {
    pub x_ratio: f32,
    /// 距客户区底边的 96-DPI 逻辑像素。
    pub y_from_bottom: f32,
}

/// 锚点几何形态：声明使用的是哪一种定位模型（现场记录的判读键）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnchorGeometry {
    Ratio(FocusAnchor),
    BottomUp(BottomUpAnchor),
    /// 表达式点击点（已求值）：客户区逻辑坐标（原点=客户区左上角）。
    /// 表达式与变量见 [`InputPointExpr`]；现场记录只留求值结果，表达式本身在计划里。
    ExprPoint { x_logical: i32, y_logical: i32 },
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
    /// 通过原生 caret 与 MSAA 语义确认编辑器。
    FocusedByCaretSemantic,
    /// 两条路都不可用（无锚点、UIA 不暴露输入框、或锚点被其它窗口遮挡）。
    Unavailable,
}

/// 一次锚点单击的现场证据（D74）。「报成功但没落框」类故障的判读全在这里：
/// 同一目标在两台机器上各行一次上框，对 diff 这两条记录即可定位差异源。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickEvidence {
    pub geometry: AnchorGeometry,
    /// 实际点击的屏幕物理像素坐标。
    pub point_screen: (i32, i32),
    /// 点击时的客户区尺寸（物理像素）。
    pub client_size: (i32, i32),
    /// 目标窗口的实时 DPI（96=100%）。
    pub dpi: u32,
}

/// 一个聚焦级别的尝试记录。`settle` 为 `Observed` 是「目标出现输入迹象」的
/// 正证据；`CappedOut`/`Unavailable` 都是「没能证明」，不是「证明没发生」。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusAttempt {
    pub step: FocusStep,
    pub outcome: FocusOutcome,
    /// 锚点步的单击现场；非锚点步为 None。
    pub click: Option<ClickEvidence>,
    /// 该步的事件等待结局；未做等待的级别为 None。
    pub settle: Option<WaitOutcome>,
}

/// 一次「把键盘焦点送进聊天输入框」的完整报告：最终结论 + 逐级尝试证据。
///
/// 为什么从裸 [`FocusOutcome`] 升级为报告：旧实现里锚点步的 settle 证据被
/// 丢弃（只进 debug 日志），「点击顶满 60ms 上限」与「点击立刻确认」在上层
/// 无法区分——这正是 2026-09-03 拼多多「报成功但没落框」远程排障的盲区。
#[derive(Debug, Clone, PartialEq)]
pub struct FocusReport {
    pub outcome: FocusOutcome,
    pub attempts: Vec<FocusAttempt>,
}

impl FocusReport {
    /// 是否存在「锚点单击且观测到输入迹象」的正证据。
    pub fn anchor_click_observed(&self) -> bool {
        self.attempts.iter().any(|attempt| {
            attempt.step == FocusStep::AnchorClick
                && attempt.outcome == FocusOutcome::FocusedByAnchor
                && matches!(attempt.settle, Some(WaitOutcome::Observed { .. }))
        })
    }
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
    /// 对画像声明的表达式点击点（[`InputPointExpr`]）做单次左键单击。
    /// 无声明时该级别自动跳过。与 `AnchorClick` 的差别只在定位模型：
    /// 这里是用户可配的「窗口内任意一点」（原点=客户区左上角，逻辑像素，
    /// 变量 WINDOW_WIDTH/WINDOW_HEIGHT 实时求值），失焦恢复输入面的
    /// 真机实测通路（2026-09-05 千牛/微信）即以此落地。
    InputPointClick,
    /// 读取原生 caret 并以 MSAA 语义确认编辑器，不执行任何动作。
    CaretSemantic,
}

/// 用户可配置的点击点表达式（失焦恢复输入面的定位模型）。
///
/// 坐标系：客户区，原点=左上角，96-DPI 逻辑像素（点击时按目标窗口实时 DPI
/// 换算物理像素）。表达式支持整数四则运算、括号与两个实时变量：
/// `WINDOW_WIDTH`/`WINDOW_HEIGHT` = 客户区逻辑宽高。四个角的「内缩 N 像素」
/// 与窗口内任意一点因此是同一配置面，例如：
/// - 左下角内缩 8：`x="8"`, `y="WINDOW_HEIGHT - 8"`
/// - 右下角内缩 8：`x="WINDOW_WIDTH - 8"`, `y="WINDOW_HEIGHT - 8"`
/// - 底部居中偏上 100：`x="WINDOW_WIDTH / 2"`, `y="WINDOW_HEIGHT - 100"`
///
/// 为什么暴露表达式而不是 dx/dy：不同设备/版式下「输入面」相对窗口角的偏移
/// 不同（微信右下角内缩 4~20 物理像素即中、千牛则须落在中栏输入框内部），
/// 让用户按自己机器实测调一个点，比猜一组普适常数诚实。
#[derive(Debug, Clone, PartialEq)]
pub struct InputPointExpr {
    pub x: String,
    pub y: String,
}

/// 求值点击点表达式。支持 `+ - * /`（i32 整除）、括号、一元正负号、整数
/// 字面量与变量 `WINDOW_WIDTH`/`WINDOW_HEIGHT`；除零、整数溢出、未知变量
/// 与任何语法错误都返回 Err——画像加载期即拒绝，不把坏配置带进点击路径。
pub fn eval_point_expr(expr: &str, window_width: i32, window_height: i32) -> std::result::Result<i32, String> {
    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
        width: i32,
        height: i32,
    }

    impl Parser<'_> {
        fn skip_ws(&mut self) {
            while self
                .bytes
                .get(self.pos)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.pos += 1;
            }
        }

        fn peek(&mut self) -> Option<u8> {
            self.skip_ws();
            self.bytes.get(self.pos).copied()
        }

        fn expr(&mut self) -> std::result::Result<i32, String> {
            let mut left = self.term()?;
            loop {
                match self.peek() {
                    Some(b'+') => {
                        self.pos += 1;
                        let right = self.term()?;
                        left = left
                            .checked_add(right)
                            .ok_or_else(|| "整数溢出".to_string())?;
                    }
                    Some(b'-') => {
                        self.pos += 1;
                        let right = self.term()?;
                        left = left
                            .checked_sub(right)
                            .ok_or_else(|| "整数溢出".to_string())?;
                    }
                    _ => return Ok(left),
                }
            }
        }

        fn term(&mut self) -> std::result::Result<i32, String> {
            let mut left = self.unary()?;
            loop {
                match self.peek() {
                    Some(b'*') => {
                        self.pos += 1;
                        let right = self.unary()?;
                        left = left
                            .checked_mul(right)
                            .ok_or_else(|| "整数溢出".to_string())?;
                    }
                    Some(b'/') => {
                        self.pos += 1;
                        let right = self.unary()?;
                        if right == 0 {
                            return Err("除零".to_string());
                        }
                        left /= right;
                    }
                    _ => return Ok(left),
                }
            }
        }

        fn unary(&mut self) -> std::result::Result<i32, String> {
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    self.unary()
                }
                Some(b'-') => {
                    self.pos += 1;
                    let value = self.unary()?;
                    value
                        .checked_neg()
                        .ok_or_else(|| "整数溢出".to_string())
                }
                _ => self.atom(),
            }
        }

        fn atom(&mut self) -> std::result::Result<i32, String> {
            match self.peek() {
                Some(b'(') => {
                    self.pos += 1;
                    let value = self.expr()?;
                    if self.peek() == Some(b')') {
                        self.pos += 1;
                        Ok(value)
                    } else {
                        Err("缺少右括号".to_string())
                    }
                }
                Some(c) if c.is_ascii_digit() => {
                    let start = self.pos;
                    while self
                        .bytes
                        .get(self.pos)
                        .is_some_and(u8::is_ascii_digit)
                    {
                        self.pos += 1;
                    }
                    std::str::from_utf8(&self.bytes[start..self.pos])
                        .ok()
                        .and_then(|text| text.parse::<i32>().ok())
                        .ok_or_else(|| "整数过大".to_string())
                }
                Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                    let start = self.pos;
                    while self
                        .bytes
                        .get(self.pos)
                        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                    {
                        self.pos += 1;
                    }
                    match std::str::from_utf8(&self.bytes[start..self.pos]) {
                        Ok("WINDOW_WIDTH") => Ok(self.width),
                        Ok("WINDOW_HEIGHT") => Ok(self.height),
                        Ok(other) => Err(format!(
                            "未知变量 {other}(可用 WINDOW_WIDTH / WINDOW_HEIGHT)"
                        )),
                        Err(_) => Err("变量名含非法字节".to_string()),
                    }
                }
                Some(c) => Err(format!("无效字符 '{}'(位置 {})", c as char, self.pos)),
                None => Err("表达式意外结束".to_string()),
            }
        }
    }

    let mut parser = Parser {
        bytes: expr.as_bytes(),
        pos: 0,
        width: window_width,
        height: window_height,
    };
    let value = parser.expr()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(format!("表达式末尾有多余内容(位置 {})", parser.pos));
    }
    Ok(value)
}

/// 表达式点击点求值并夹进客户区：返回 96-DPI 逻辑坐标（原点=客户区左上角）。
/// 越界值夹到客户区内（允许贴边——角落内缩配方可能就是贴边点），不报错。
pub fn input_point_logical(
    point: &InputPointExpr,
    client_logical: (i32, i32),
) -> std::result::Result<(i32, i32), String> {
    let x = eval_point_expr(&point.x, client_logical.0, client_logical.1)?;
    let y = eval_point_expr(&point.y, client_logical.0, client_logical.1)?;
    Ok((
        x.clamp(0, (client_logical.0 - 1).max(0)),
        y.clamp(0, (client_logical.1 - 1).max(0)),
    ))
}

/// 一次聚焦尝试的完整计划：按 `steps` 顺序降级，直到某级别被验证成功。
#[derive(Debug, Clone, PartialEq)]
pub struct FocusPlan {
    pub steps: Vec<FocusStep>,
    pub anchor: Option<FocusAnchor>,
    /// bottom-up 锚点（D74）：存在即**优先于** `anchor` 被使用。
    pub anchor_bottom: Option<BottomUpAnchor>,
    /// 表达式点击点：`steps` 含 [`FocusStep::InputPointClick`] 且本字段存在时，
    /// 该级对求值点做单次左键单击（定位模型见 [`InputPointExpr`]）。
    pub input_point_expr: Option<InputPointExpr>,
}

/// 聊天输入框焦点获取端。
///
/// 红线：实现只允许做「移动焦点」这一类动作——UIA `SetFocus` 或对画像声明锚点的
/// **单次左键单击**。禁止合成任何键盘事件（尤其 Enter），禁止点击未声明的位置。
pub trait InputFocuser {
    fn focus_input(&self, window: WindowHandle, plan: &FocusPlan) -> FocusReport;
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

/// HTTP 文本拉取端（D56 更新检查）。实现负责 TLS、系统代理与超时；
/// 非 2xx、超时、断连或响应非法编码都算失败。响应体大小上限由实现自定
/// （对端异常时不得无界吃内存）。签名不外泄平台术语，调用方按 URL 表达意图。
pub trait HttpTextFetcher {
    fn fetch_text(&self, url: &str, timeout_ms: u64) -> Result<String>;
}

/// 系统默认方式打开 URL（更新弹窗「打开发布页」，D56）。实现把「交给系统
/// 默认浏览器」这一意图收拢到平台层；用户取消之外的失败返回 Err。
pub trait UrlOpener {
    fn open_url(&self, url: &str) -> Result<()>;
}

/// 下载进度回调实参：(已接收字节, 总字节)。`total` 为 0 表示对端未报
/// Content-Length，进度条按不确定态处理，字节数照常上报。
pub type DownloadProgress<'a> = &'a mut dyn FnMut(u64, u64);

/// 文件流式下载端（D70 应用内自更新）。与 [`HttpTextFetcher`] 同栈不同职责：
/// 响应体必须**边收边写盘**落到 `dest`（安装包几十 MB，不得整体驻留内存——
/// 空闲 RSS 预算是选型的根本理由），逐块上报进度；`cancel` 置位后尽快中止
/// （中止语义 = Err 返回，调用方负责区分「用户取消」与真失败）。
/// 半成品文件留给调用方处置（覆盖式重下即自愈，不做临时文件体操）。
pub trait HttpFileDownloader {
    fn download_to_file(
        &self,
        url: &str,
        dest: &Path,
        timeout_ms: u64,
        max_bytes: u64,
        progress: DownloadProgress<'_>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<u64>;

    /// 下载测速取样（D71 镜像择优）：Range 请求取前 `sample_bytes` 字节并
    /// 计时，返回从发起请求到取样完成的毫秒数。对端不支持 Range 时回 200
    /// 全量响应——只读 `sample_bytes` 即中止，不落盘不耗尽；文件比取样还小
    /// 时提前 EOF 也算成功。失败（含取消置位）返回 Err。
    fn probe_sample(
        &self,
        url: &str,
        timeout_ms: u64,
        sample_bytes: u32,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<u64>;
}

// 平台实现模块：声明本身不带条件门，「仅 Windows」的门在该文件内部，
// 保证本 trait 文件可被逐字 grep 验证纯净（无门、无平台 crate 引用）。
pub mod win32;
