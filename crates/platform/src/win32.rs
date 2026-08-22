//! Win32 真实实现（仅 Windows）：剪贴板写入 / 前台窗口观测 / 按键注入。
//!
//! 「仅 Windows」红线的编译期体现：本模块整体带仅-Windows 条件门（下方内属性），
//! 平台 API 全部收拢于此，业务 crate 只经 trait 使用。

#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;

use windows_sys::Win32::Foundation::{GlobalFree, POINT};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatA, SetClipboardData,
};
// 剪贴板格式常量在 windows-sys 元数据中归属 Ole 模块（类型 CLIPBOARD_FORMAT = u16）。
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::System::Threading::Sleep;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
};
use windows_sys::Win32::UI::Shell::DROPFILES;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow};

use crate::{
    ClipboardPayload, ClipboardSink, FocusWatcher, KeyInjector, PlatformError, Result,
    WindowHandle, KEY_UP,
};

/// 剪贴板打开失败的竞争重试等待时长（毫秒）。
const CLIPBOARD_RETRY_DELAY_MS: u32 = 10;

// ---------------------------------------------------------------------------
// 剪贴板
// ---------------------------------------------------------------------------

/// 系统剪贴板写入实现。
///
/// 失败语义：Open/Empty/Set 任一步失败即返回 Err；GlobalAlloc 的内存块仅在
/// SetClipboardData 成功（所有权移交系统）后不再回收，其余失败路径一律
/// GlobalFree 防泄漏；CloseClipboard 无条件执行，避免其他进程被锁死。
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32Clipboard;

impl ClipboardSink for Win32Clipboard {
    fn write(&mut self, payload: &ClipboardPayload) -> Result<()> {
        // 打开剪贴板可能与其他进程的占用竞争：失败后短暂等待重试一次。
        let mut opened = unsafe { OpenClipboard(ptr::null_mut()) };
        if opened == 0 {
            unsafe { Sleep(CLIPBOARD_RETRY_DELAY_MS) };
            opened = unsafe { OpenClipboard(ptr::null_mut()) };
        }
        if opened == 0 {
            return Err(PlatformError::Clipboard(
                "OpenClipboard 连续两次失败(可能被其他进程长期占用)".into(),
            ));
        }
        let outcome = Self::empty_then_set(payload);
        unsafe { CloseClipboard() };
        outcome
    }
}

impl Win32Clipboard {
    /// 前置条件：剪贴板已由调用方成功打开。
    fn empty_then_set(payload: &ClipboardPayload) -> Result<()> {
        // 安全：剪贴板已打开，以下为标准 Empty/Set 序列。
        if unsafe { EmptyClipboard() } == 0 {
            return Err(PlatformError::Clipboard("EmptyClipboard 失败".into()));
        }
        match payload {
            ClipboardPayload::Files(paths) => Self::set_hdrop(paths),
            ClipboardPayload::Png(bytes) => Self::set_registered_png(bytes),
            ClipboardPayload::Dib(bytes) => Self::set_raw_bytes(u32::from(CF_DIB), bytes),
            ClipboardPayload::Text(text) => Self::set_unicode_text(text),
        }
    }

    /// HDROP 布局：DROPFILES 头 + UTF-16 路径列表（每路径单 NUL 结尾）+
    /// 列表整体双 NUL 终止。pFiles 记录文件列表起始偏移。
    ///
    /// 安全：须在剪贴板已打开后调用；分配块按「Set 成功即移交系统」的所有权
    /// 规则处理，失败路径统一 GlobalFree。
    fn set_hdrop(paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Err(PlatformError::Clipboard(
                "HDROP 载荷需要至少一个文件路径".into(),
            ));
        }
        let mut list: Vec<u16> = Vec::new();
        for path in paths {
            list.extend(path.as_os_str().encode_wide());
            list.push(0); // 单路径 NUL 结尾
        }
        list.push(0); // 列表双 NUL 终止

        let header_len = std::mem::size_of::<DROPFILES>();
        let total = header_len + list.len() * 2;
        Self::set_global_block(u32::from(CF_HDROP), total, |dst| unsafe {
            // 安全：dst 指向刚分配的 total 字节可写块；两个 copy 均在其边界内。
            let header = DROPFILES {
                pFiles: header_len as u32,
                pt: POINT { x: 0, y: 0 },
                fNC: 0,
                fWide: 1, // 路径为 UTF-16 宽字符
            };
            ptr::copy_nonoverlapping(
                ptr::addr_of!(header).cast::<u8>(),
                dst.cast::<u8>(),
                header_len,
            );
            ptr::copy_nonoverlapping(
                list.as_ptr().cast::<u8>(),
                dst.cast::<u8>().add(header_len),
                list.len() * 2,
            );
        })
    }

    /// PNG → 注册格式 "PNG"。格式号每次会话可能不同，须现场注册。
    fn set_registered_png(bytes: &[u8]) -> Result<()> {
        // 安全：传入 NUL 结尾的字面量指针，API 只读。
        let format = unsafe { RegisterClipboardFormatA(c"PNG".as_ptr().cast()) };
        if format == 0 {
            return Err(PlatformError::Clipboard("注册 PNG 格式失败".into()));
        }
        Self::set_raw_bytes(format, bytes)
    }

    /// 文本 → CF_UNICODETEXT（NUL 结尾 UTF-16LE）。
    fn set_unicode_text(text: &str) -> Result<()> {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0); // NUL 结尾

        // u16 → 原生端序字节展开（Windows 目标即 LE），全程安全代码，无需重解释指针。
        let mut bytes = Vec::with_capacity(wide.len() * 2);
        for unit in &wide {
            bytes.extend_from_slice(&unit.to_ne_bytes());
        }
        Self::set_raw_bytes(u32::from(CF_UNICODETEXT), &bytes)
    }

    /// 通用字节载荷写入（标准格式或注册格式的公共路径）。
    fn set_raw_bytes(format: u32, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Err(PlatformError::Clipboard("空字节载荷拒绝写入剪贴板".into()));
        }
        Self::set_global_block(format, bytes.len(), |dst| unsafe {
            // 安全：dst 指向 bytes.len() 字节的已分配可写块。
            ptr::copy_nonoverlapping(bytes.as_ptr(), dst.cast::<u8>(), bytes.len());
        })
    }

    /// 公共骨架：GlobalAlloc → GlobalLock → writer 填充 → GlobalUnlock →
    /// SetClipboardData。失败路径 GlobalFree 回收；成功后内存块所有权移交系统。
    fn set_global_block(
        format: u32,
        size: usize,
        fill: impl FnOnce(*mut std::ffi::c_void),
    ) -> Result<()> {
        let block = unsafe { GlobalAlloc(GMEM_MOVEABLE, size) };
        if block.is_null() {
            return Err(PlatformError::Clipboard(format!(
                "GlobalAlloc({size} 字节) 失败"
            )));
        }
        let dst = unsafe { GlobalLock(block) };
        if dst.is_null() {
            unsafe { GlobalFree(block) };
            return Err(PlatformError::Clipboard("GlobalLock 失败".into()));
        }
        fill(dst);
        unsafe { GlobalUnlock(block) };
        // 安全：format 为刚注册/常量格式号；block 为合法全局句柄。
        if unsafe { SetClipboardData(format, block) }.is_null() {
            // 系统拒收：所有权未转移，必须自行回收防泄漏。
            unsafe { GlobalFree(block) };
            return Err(PlatformError::Clipboard(format!(
                "SetClipboardData(格式 {format}) 失败"
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 前台窗口观测
// ---------------------------------------------------------------------------

/// 前台窗口观测实现（D12：GetForegroundWindow + IsWindow 存活校验）。
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32Focus;

impl FocusWatcher for Win32Focus {
    fn foreground(&self) -> WindowHandle {
        // 安全：无参查询；HWND 归一化为裸值跨层传递。
        WindowHandle(unsafe { GetForegroundWindow() } as isize)
    }

    fn is_alive(&self, window: WindowHandle) -> bool {
        // 安全：IsWindow 仅校验句柄有效性，不触碰窗口内容。
        unsafe { IsWindow(window.0 as *mut std::ffi::c_void) != 0 }
    }
}

// ---------------------------------------------------------------------------
// 按键注入
// ---------------------------------------------------------------------------

/// 按键注入实现：把键事件序列逐个合成键盘输入事件并一次性送达。
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32Injector;

impl KeyInjector for Win32Injector {
    /// 序列元素编码见 [`crate::KEY_UP`]：低 15 位虚拟键码、最高位标记释放相位。
    fn inject(&mut self, keys: &[u16]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut events: Vec<INPUT> = Vec::with_capacity(keys.len());
        for &key in keys {
            // 安全：zeroed 补齐联合体尾部与保留字段，随后立即显式赋值所需字段。
            let mut event: INPUT = unsafe { std::mem::zeroed() };
            event.r#type = INPUT_KEYBOARD;
            // Copy 类型向联合体字段赋值是安全操作（读取才需 unsafe）。
            event.Anonymous.ki = KEYBDINPUT {
                wVk: key & !KEY_UP,
                wScan: 0,
                dwFlags: if key & KEY_UP != 0 {
                    KEYEVENTF_KEYUP
                } else {
                    0
                },
                time: 0,
                dwExtraInfo: 0,
            };
            events.push(event);
        }
        let sent = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent != events.len() as u32 {
            return Err(PlatformError::Inject(format!(
                "输入事件仅送达 {sent}/{} 个(目标窗口可能拒绝注入,如 UIPI)",
                events.len()
            )));
        }
        Ok(())
    }
}
