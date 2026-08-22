//! 平台抽象层：剪贴板写入、前台窗口观测、按键注入。
//!
//! 分层纪律（spec/platform）：
//! - 本文件为 trait 层，**零依赖、零条件编译门**——依赖方（pipeline/ui-viewmodels）
//!   可脱离 Win32 编译与测试；Win32 真实实现整体收拢在 `win32` 模块
//!   （仅-Windows 门在该文件内部），由二进制入口负责选择注入。
//! - trait 方法面向意图命名（write / foreground / is_alive / inject），
//!   签名不外泄平台术语；v2 新增平台 = 新增 `src/<platform>/` 模块，trait 不动。

use std::fmt;
use std::path::PathBuf;

/// 剪贴板载荷：变体与目标剪贴板格式一一对应（协商规则见 pipeline::negotiate）。
///
/// DIB/PNG 变体只接受上游提供的**已编码**字节——「UI 进程不解码」红线，
/// 本 crate 及依赖方均不得引入图像解码依赖。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardPayload {
    /// 文件列表（→ 系统 HDROP 格式）。
    Files(Vec<PathBuf>),
    /// PNG 编码字节（→ 注册格式 "PNG"）。
    Png(Vec<u8>),
    /// DIB 字节（→ CF_DIB）。存在但不参与默认路由，仅供上游已持字节时的专用路径。
    Dib(Vec<u8>),
    /// Unicode 文本（→ CF_UNICODETEXT）。
    Text(String),
}

/// 前台窗口句柄裸值。不把平台句柄类型泄进 trait 签名之外。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowHandle(pub isize);

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
    Io(std::io::Error),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::Clipboard(msg) => write!(f, "剪贴板错误: {msg}"),
            PlatformError::Inject(msg) => write!(f, "输入注入错误: {msg}"),
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
    fn write(&mut self, payload: &ClipboardPayload) -> Result<()>;
}

/// 前台窗口观测端：唤起面板时记录此刻前台窗口，注入前校验其仍存活（D12/D8）。
pub trait FocusWatcher {
    fn foreground(&self) -> WindowHandle;
    fn is_alive(&self, window: WindowHandle) -> bool;
}

/// 按键注入端。keys 为键事件序列，实现按序合成输入事件；
/// 序列编排（按下/释放相位、和弦顺序）由调用方负责。
pub trait KeyInjector {
    fn inject(&mut self, keys: &[u16]) -> Result<()>;
}

// 平台实现模块：声明本身不带条件门，「仅 Windows」的门在该文件内部，
// 保证本 trait 文件可被逐字 grep 验证纯净（无门、无平台 crate 引用）。
pub mod win32;
