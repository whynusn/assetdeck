//! Win32 真实实现（仅 Windows）：剪贴板写入 / 前台窗口观测 / 按键注入。
//!
//! 「仅 Windows」红线的编译期体现：本模块整体带仅-Windows 条件门（下方内属性），
//! 平台 API 全部收拢于此，业务 crate 只经 trait 使用。

#![cfg(windows)]

use std::cell::RefCell;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND as WinHWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    CoTaskMemFree,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
    IUIAutomationSelectionItemPattern, IUIAutomationTreeWalker, IUIAutomationValuePattern,
    TreeScope_Descendants, UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_InvokePatternId,
    UIA_SelectionItemPatternId, UIA_TextPatternId, UIA_ValuePatternId,
};
use windows_sys::Win32::Foundation::{CloseHandle, GlobalFree, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatA, SetClipboardData,
};
// 剪贴板格式常量在 windows-sys 元数据中归属 Ole 模块（类型 CLIPBOARD_FORMAT = u16）。
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, Sleep,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, IsWindowEnabled, SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
};
use windows_sys::Win32::UI::Shell::DROPFILES;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetAncestor, GetClassNameW, GetClientRect, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    PostThreadMessageW, SetCursorPos, SetForegroundWindow, ShowWindow, TranslateMessage,
    WindowFromPoint, EVENT_OBJECT_FOCUS, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
    GA_ROOT, MSG, OBJID_CARET, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SW_RESTORE, WINEVENT_OUTOFCONTEXT, WM_QUIT,
};

use crate::{
    ClipboardPayload, ClipboardSink, EventWait, FileDialogs, FocusAnchor, FocusOutcome, FocusPlan,
    FocusStep, FocusWatcher, ForegroundObserver, ForegroundRelation, InputFocuser, KeyInjector,
    PlatformError, ReadinessBlocker, ReadinessProbe, ReadinessSignal, Result, WaitOutcome,
    WindowActivator, WindowEnumerator, WindowEventSource, WindowHandle, WindowRect,
    WindowSnapshot, KEY_UP,
};

/// 原生文件对话框（IFileDialog）：ComCtl 版免 PowerShell 冷启动（消除数秒延迟）。
//
// 实现说明：IFileOpenDialog/IFileSaveDialog 是 shell32 的 COM 对象，首次 CoCreateInstance
// 涉及 COM 冷启动（本进程内 UIA 已预热过，实测 <50ms），远快于起一个 PowerShell 进程
// 加载 WinForms + PowerShell 宿主（~3s）。取消以 HRESULT_FROM_WIN32(ERROR_CANCELLED)
// （0x800704C7）返回，映射为 Ok(None)。COM 初始化按需做一次（线程公寓在本进程为 STA，
// CoInitializeEx 重复调用返回 S_FALSE，无害）。
pub struct Win32FileDialogs;

/// 按需确保当前线程完成 COM 初始化（幂等；UI 线程粘贴热路径的 UIA 已初始化过）。
fn ensure_com_initialized() {
    static COM_READY: OnceLock<()> = OnceLock::new();
    COM_READY.get_or_init(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
    });
}

impl FileDialogs for Win32FileDialogs {
    fn pick_folder(&self, title: &str) -> crate::Result<Option<PathBuf>> {
        ensure_com_initialized();
        unsafe {
            let dialog: windows::Win32::UI::Shell::IFileOpenDialog = CoCreateInstance(
                &windows::Win32::UI::Shell::FileOpenDialog,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|error| PlatformError::Window(format!("创建文件夹对话框失败: {error}")))?;
            dialog
                .SetOptions(
                    windows::Win32::UI::Shell::FOS_PICKFOLDERS
                        | windows::Win32::UI::Shell::FOS_FORCEFILESYSTEM
                )
                .map_err(|error| PlatformError::Window(format!("配置文件夹对话框失败: {error}")))?;
            dialog
                .SetTitle(&windows::core::HSTRING::from(title))
                .map_err(|error| PlatformError::Window(format!("设置对话框标题失败: {error}")))?;
            match dialog.Show(None) {
                Ok(()) => {
                    let item = dialog
                        .GetResult()
                        .map_err(|error| PlatformError::Window(format!("读取选择结果失败: {error}")))?;
                    let name = item
                        .GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)
                        .map_err(|error| PlatformError::Window(format!("读取选择路径失败: {error}")))?;
                    let path = name.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(name.as_ptr() as *const core::ffi::c_void));
                    Ok(Some(PathBuf::from(path)))
                }
                Err(error) if is_cancelled(&error) => Ok(None),
                Err(error) => Err(PlatformError::Window(format!("文件夹选择对话框失败: {error}"))),
            }
        }
    }

    /// 文件模式打开对话框（区别于 pick_folder 的 FOS_PICKFOLDERS）：
    /// 默认视图按 filter 列出文件——文件夹选择器里 .emo 这类文件根本不可见，
    /// 这是「导入素材包找不到文件」的直接原因。
    fn pick_open_file(&self, title: &str, filter: &str) -> crate::Result<Option<PathBuf>> {
        ensure_com_initialized();
        unsafe {
            let dialog: windows::Win32::UI::Shell::IFileOpenDialog = CoCreateInstance(
                &windows::Win32::UI::Shell::FileOpenDialog,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|error| PlatformError::Window(format!("创建文件对话框失败: {error}")))?;
            dialog
                .SetOptions(
                    windows::Win32::UI::Shell::FOS_FORCEFILESYSTEM
                        | windows::Win32::UI::Shell::FOS_FILEMUSTEXIST
                )
                .map_err(|error| PlatformError::Window(format!("配置文件对话框失败: {error}")))?;
            dialog
                .SetTitle(&windows::core::HSTRING::from(title))
                .map_err(|error| PlatformError::Window(format!("设置对话框标题失败: {error}")))?;
            // SetFileTypes 在调用期内复制过滤器字符串，之后即可释放临时 HSTRING；
            // kept 与 specs 必须存活到 SetFileTypes 调用结束（specs 只存指针）。
            let kept = parse_filter_pairs(filter);
            if !kept.is_empty() {
                let specs: Vec<windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC> = kept
                    .iter()
                    .map(|(name, spec)| windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC {
                        pszName: windows::core::PCWSTR(name.as_ptr()),
                        pszSpec: windows::core::PCWSTR(spec.as_ptr()),
                    })
                    .collect();
                dialog
                    .SetFileTypes(&specs)
                    .map_err(|error| PlatformError::Window(format!("设置文件类型过滤失败: {error}")))?;
            }
            match dialog.Show(None) {
                Ok(()) => {
                    let item = dialog
                        .GetResult()
                        .map_err(|error| PlatformError::Window(format!("读取选择结果失败: {error}")))?;
                    let name = item
                        .GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)
                        .map_err(|error| PlatformError::Window(format!("读取选择路径失败: {error}")))?;
                    let path = name.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(name.as_ptr() as *const core::ffi::c_void));
                    Ok(Some(PathBuf::from(path)))
                }
                Err(error) if is_cancelled(&error) => Ok(None),
                Err(error) => Err(PlatformError::Window(format!("文件选择对话框失败: {error}"))),
            }
        }
    }

    fn pick_save_path(
        &self,
        title: &str,
        default_name: &str,
        filter: &str,
    ) -> crate::Result<Option<PathBuf>> {
        ensure_com_initialized();
        unsafe {
            let dialog: windows::Win32::UI::Shell::IFileSaveDialog = CoCreateInstance(
                &windows::Win32::UI::Shell::FileSaveDialog,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|error| PlatformError::Window(format!("创建保存对话框失败: {error}")))?;
            dialog
                .SetOptions(
                    windows::Win32::UI::Shell::FOS_OVERWRITEPROMPT
                        | windows::Win32::UI::Shell::FOS_FORCEFILESYSTEM
                )
                .map_err(|error| PlatformError::Window(format!("配置保存对话框失败: {error}")))?;
            dialog
                .SetTitle(&windows::core::HSTRING::from(title))
                .map_err(|error| PlatformError::Window(format!("设置对话框标题失败: {error}")))?;
            dialog
                .SetFileName(&windows::core::HSTRING::from(default_name))
                .map_err(|error| PlatformError::Window(format!("设置默认文件名失败: {error}")))?;
            // SetFileTypes 在调用期内复制过滤器字符串，之后即可释放临时 HSTRING。
            let name_h = windows::core::HSTRING::from("素材包 (*.emo)");
            let spec_h = windows::core::HSTRING::from("*.emo");
            let filters = [windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC {
                pszName: windows::core::PCWSTR(name_h.as_ptr()),
                pszSpec: windows::core::PCWSTR(spec_h.as_ptr()),
            }];
            dialog
                .SetFileTypes(&filters)
                .map_err(|error| PlatformError::Window(format!("设置文件类型过滤失败: {error}")))?;
            let _ = filter; // filter 字符串参数为未来多类型场景预留
            match dialog.Show(None) {
                Ok(()) => {
                    let item = dialog
                        .GetResult()
                        .map_err(|error| PlatformError::Window(format!("读取保存结果失败: {error}")))?;
                    let name = item
                        .GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)
                        .map_err(|error| PlatformError::Window(format!("读取保存路径失败: {error}")))?;
                    let path = name.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(name.as_ptr() as *const core::ffi::c_void));
                    Ok(Some(PathBuf::from(path)))
                }
                Err(error) if is_cancelled(&error) => Ok(None),
                Err(error) => Err(PlatformError::Window(format!("保存对话框失败: {error}"))),
            }
        }
    }
}

/// IFileDialog 取消统一以 HRESULT_FROM_WIN32(ERROR_CANCELLED) = 0x800704C7 返回。
fn is_cancelled(error: &windows::core::Error) -> bool {
    error.code().0 == 0x8007_04C7u32 as i32
}

/// 把 `"名称|规格|名称|规格"` 过滤串解析为 (名称, 规格) 对。
/// 宽容语义：空白段忽略；段数为奇数时末组以规格兼任名称；空串得空表（不过滤）。
fn parse_filter_pairs(filter: &str) -> Vec<(windows::core::HSTRING, windows::core::HSTRING)> {
    let segments: Vec<&str> = filter
        .split('|')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect();
    let mut pairs: Vec<(windows::core::HSTRING, windows::core::HSTRING)> =
        Vec::with_capacity(segments.len().div_ceil(2));
    let mut index = 0;
    while index < segments.len() {
        if index + 1 < segments.len() {
            pairs.push((segments[index].into(), segments[index + 1].into()));
            index += 2;
        } else {
            pairs.push((segments[index].into(), segments[index].into()));
            index += 1;
        }
    }
    pairs
}


/// 等泵线程回报「钩子已装上」的上限。只在进程内首次订阅时付一次。
const PUMP_STARTUP_CAP_MS: u64 = 500;

/// 剪贴板打开失败的竞争重试等待时长（毫秒）。
const CLIPBOARD_RETRY_DELAY_MS: u32 = 10;
/// 虚拟键码：Ctrl（与 Win32 VK_CONTROL 同值）。
const VK_CONTROL: u16 = 0x11;
/// 虚拟键码：Alt（与 Win32 VK_MENU 同值）。
const VK_MENU: u16 = 0x12;
/// WinEvent 泵装钩子失败或泵线程退出后，至少隔这么久才允许重装。
///
/// 泵不可用 = 全进程事件驱动退级（前台确认/输入表面等待全部失去证据来源），
/// 因此冷却要短到人无感（秒级自愈），又要长到不会在持续性失败（无桌面会话）
/// 里变成忙重试。
const PUMP_REINSTALL_COOLDOWN_MS: u64 = 2_000;
/// 本实现会注入的修饰键全集。注入前逐个检查系统异步键状态，仍按下的先补
/// KEYUP 复位（见 [`stuck_modifier_recovery`]）。
const INJECTED_MODIFIERS: [u16; 2] = [VK_CONTROL, VK_MENU];

/// 构造 CF_HDROP 的 UTF-16 路径列表：每条路径提升为绝对路径并以单 NUL 结尾，
/// 整个列表以额外一个 NUL 终止。
///
/// 为什么必须绝对化（真实 IM 实测根因）：HDROP 里的相对路径由**接收方进程**
/// 按它自己的工作目录解析。微信/千牛解析不到文件时会静默丢弃整次粘贴——输入框
/// 毫无变化，且不产生任何可捕获的错误。所以这里宁可返回 Err 让上层弹出提示，
/// 也不把相对路径原样写进剪贴板。
fn hdrop_path_list(paths: &[PathBuf]) -> Result<Vec<u16>> {
    if paths.is_empty() {
        return Err(PlatformError::Clipboard(
            "HDROP 载荷需要至少一个文件路径".into(),
        ));
    }
    let mut list: Vec<u16> = Vec::new();
    for path in paths {
        let absolute = std::path::absolute(path).map_err(|error| {
            PlatformError::Clipboard(format!(
                "无法将 {} 规范化为绝对路径: {error}",
                path.display()
            ))
        })?;
        if absolute.is_relative() {
            return Err(PlatformError::Clipboard(format!(
                "{} 规范化后仍是相对路径，拒绝写入 HDROP",
                path.display()
            )));
        }
        list.extend(absolute.as_os_str().encode_wide());
        list.push(0); // 单路径 NUL 结尾
    }
    list.push(0); // 列表双 NUL 终止
    Ok(list)
}

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
            unsafe { Sleep(CLIPBOARD_RETRY_DELAY_MS) }; // sleep-allowed(剪贴板占用无事件可订阅,只能退避重试)
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
        let list = hdrop_path_list(paths)?;
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

    fn foreground_relation(
        &self,
        foreground: WindowHandle,
        target: WindowHandle,
    ) -> ForegroundRelation {
        if foreground == target {
            return ForegroundRelation::Target;
        }
        let fg_pid = window_process_id(foreground.0 as HWND);
        let target_pid = window_process_id(target.0 as HWND);
        // 安全：无参查询自身进程号。
        classify_foreground_relation(fg_pid, target_pid, unsafe { GetCurrentProcessId() })
    }
}

/// 纯分类：前台 pid 相对目标进程与自身进程的归属（D44）。
/// pid 为 0（句柄已失效/无主）视作无主第三方，按 Foreign 保守处理。
fn classify_foreground_relation(fg_pid: u32, target_pid: u32, own_pid: u32) -> ForegroundRelation {
    if fg_pid != 0 && fg_pid == target_pid {
        return ForegroundRelation::SameAsTarget;
    }
    if fg_pid != 0 && fg_pid == own_pid {
        return ForegroundRelation::OwnProcess;
    }
    ForegroundRelation::Foreign
}

// ---------------------------------------------------------------------------
// 窗口枚举、激活与前台观察
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
pub struct Win32WindowEnumerator;

impl WindowEnumerator for Win32WindowEnumerator {
    fn windows(&self) -> Result<Vec<WindowSnapshot>> {
        let mut windows = Vec::new();
        // 安全：回调仅在 EnumWindows 同步调用期间使用 lparam 指向的 Vec。
        let ok = unsafe {
            EnumWindows(
                Some(enum_window),
                (&mut windows as *mut Vec<WindowSnapshot>) as LPARAM,
            )
        };
        if ok == 0 {
            return Err(PlatformError::Window("EnumWindows 失败".into()));
        }
        Ok(windows)
    }
}

unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> i32 {
    let Some(snapshot) = snapshot_window(hwnd) else {
        return 1;
    };
    if !snapshot.rect.has_area() {
        return 1;
    }
    let windows = unsafe { &mut *(lparam as *mut Vec<WindowSnapshot>) };
    windows.push(snapshot);
    1
}

fn snapshot_window(hwnd: HWND) -> Option<WindowSnapshot> {
    if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
        return None;
    }
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return None;
    }
    Some(WindowSnapshot {
        hwnd: WindowHandle(hwnd as isize),
        exe_name: process_exe_name(hwnd).unwrap_or_default(),
        class_name: window_class(hwnd),
        title: window_title(hwnd),
        visible: unsafe { IsWindowVisible(hwnd) } != 0,
        minimized: unsafe { IsIconic(hwnd) } != 0,
        rect: WindowRect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        },
        process_id: window_process_id(hwnd),
    })
}

fn window_process_id(hwnd: HWND) -> u32 {
    let mut process_id = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
    process_id
}

fn window_title(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

fn window_class(hwnd: HWND) -> String {
    let mut buffer = vec![0u16; 256];
    let copied = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

fn process_exe_name(hwnd: HWND) -> Option<String> {
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
    if process_id == 0 {
        return None;
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return None;
    }
    let mut buffer = vec![0u16; 32_768];
    let mut len = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut len) };
    unsafe { CloseHandle(process) };
    if ok == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buffer[..len as usize]);
    path.rsplit(['\\', '/']).next().map(ToOwned::to_owned)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Win32WindowActivator;

impl WindowActivator for Win32WindowActivator {
    fn activate(
        &self,
        window: WindowHandle,
        confirm_timeout_ms: u64,
        settle_ms: u64,
    ) -> Result<bool> {
        let hwnd = window.0 as HWND;
        if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
            return Err(PlatformError::Window("目标窗口已失活".into()));
        }
        let events = Win32WindowEvents;
        // 先订阅再动作。订阅建立后立刻读一次前台，覆盖「动作前已满足」的情形——
        // 否则窗口本来就是前台时不会再有 EVENT_SYSTEM_FOREGROUND，白等一场。
        //
        // 关键（真机实测 D15 修正）：微信/千牛在被激活的**同一毫秒**里，除了
        // `EVENT_SYSTEM_FOREGROUND` 还会立刻发一条 `EVENT_OBJECT_FOCUS`(obj=OBJID_CARET)——
        // 那正是我们等的「可输入表面就绪」信号。若等前台确认后再订阅输入表面，这条焦点
        // 事件早已飞过，`settle` 只能干等满 `settle_ms`。因此**输入表面必须与前台一起、
        // 在发起激活动作之前就订阅**，让激活期间到达的焦点事件被通道缓冲下来，随后
        // `wait` 一抽即中，把这段从「睡满 120~150ms」降到个位数毫秒。
        let first_attempt_cap = confirm_timeout_ms.min(FOREGROUND_FIRST_ATTEMPT_CAP_MS);
        let mut foreground = events.await_foreground(window);
        let mut surface = events.await_input_surface(window);
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
        if unsafe { GetForegroundWindow() } == hwnd
            || matches!(
                foreground.wait(first_attempt_cap),
                WaitOutcome::Observed { .. }
            )
        {
            log::debug!("activate round=first confirmed");
            settle_on_input_surface(&mut surface, settle_ms);
            return Ok(true);
        }
        drop(foreground);

        // Windows 的前台权限会限制连续切换。轻量键敲击可让当前前台线程释放
        // 输入焦点的所有权，随后再对目标调用 SetForegroundWindow；不会落入 IM
        // 输入框，也不会产生任何发送语义。
        //
        // 用 Ctrl 而不是更常见的 Alt 敲击（D40）：Alt 的 KEYUP 会被前台应用按
        // 「菜单模式激活」处理——低配机上事件处理延迟时表现为焦点闪动/菜单栏高亮，
        // 且 Alt 一旦卡在按下态，目标 IM 会被拖进键盘菜单导航，正是「反复拉起聚焦
        // 又失焦」的放大器。Ctrl 同样满足「进程最近收到输入」的前台切换资格，
        // 没有菜单副作用。
        // 敲击之后重新订阅前台：等待窗口的时间基准要对齐这一次动作。输入表面沿用
        // 上面那一个订阅——它的通道持续缓冲激活期间的焦点/插入符事件，无需重订。
        let mut foreground = events.await_foreground(window);
        let mut injector = Win32Injector;
        injector.inject(&[VK_CONTROL, VK_CONTROL | KEY_UP])?;
        unsafe {
            SetForegroundWindow(hwnd);
        }
        let retry_cap = confirm_timeout_ms
            .saturating_sub(first_attempt_cap)
            .max(FOREGROUND_RETRY_MIN_CAP_MS);
        if unsafe { GetForegroundWindow() } == hwnd
            || matches!(foreground.wait(retry_cap), WaitOutcome::Observed { .. })
        {
            log::debug!("activate round=tap confirmed");
            settle_on_input_surface(&mut surface, settle_ms);
            return Ok(true);
        }
        log::debug!("activate failed confirm_timeout_ms={confirm_timeout_ms}");
        Ok(false)
    }
}

/// 前台确认的第一轮上限。留出余量给键敲击兜底那一轮，避免第一轮把预算吃干。
const FOREGROUND_FIRST_ATTEMPT_CAP_MS: u64 = 80;
/// 键敲击兜底轮至少要给的等待窗口，防止上层传入过小的 `confirm_timeout_ms` 时退化成不等。
const FOREGROUND_RETRY_MIN_CAP_MS: u64 = 20;

/// 窗口已到前台之后，等目标进程把可输入表面建起来。
///
/// 语义是「**最多**等 `settle_ms`」：目标一报焦点/插入符事件就立刻返回；
/// 没报也不阻断上框——`CappedOut` 只意味着「没能证明」，后续焦点三级降级照走。
///
/// `Unavailable`（事件源不可用）只打告警**不补任何时序等待**：产品路径的固定
/// 睡眠被 `no_timed_waits` 守卫钉死；该分支的正确处理是让泵自愈（D40）后由
/// 事件驱动接管，而不是用一次不可靠的睡眠冒充证据。
///
/// 入参是**在发起激活动作之前就已订阅**的输入表面等待器（见 `activate`）。
/// 因此激活瞬间到达的焦点事件已被通道缓冲，`wait` 会先抽干缓冲再决定是否阻塞，
/// 命中即毫秒级返回。
fn settle_on_input_surface(surface: &mut Box<dyn EventWait>, settle_ms: u64) {
    if settle_ms == 0 {
        return;
    }
    let outcome = surface.wait(settle_ms);
    if matches!(outcome, WaitOutcome::Unavailable) {
        // 泵未就绪：前台确认与输入表面等待都失去了证据来源。这是低频异常态
        // （设计目标：稳态触发率 <0.1%，见 D40 概率论证），必须进日志让现场可回溯。
        log::warn!("输入表面事件源不可用(WinEvent 泵未就绪)，settle 无证据跳过等待");
    } else {
        // 低配机延迟归因（D41）：事件几毫秒到、还是等满 cap，是定位「上框慢」
        // 在我们侧还是对端侧的关键观测点（Debug 级，开 verbose 细查时可见）。
        log::debug!("settle 输入表面等待 outcome={outcome:?} cap_ms={settle_ms}");
    }
    // （D41 收编）原 PASTE_LATENCY_TRACE 的 trace[settle] 与上方 debug 行同点
    // 同数据，属严格冗余，直接删除。
}

/// 常驻前台观察器。不再自持 `SetWinEventHook`——改为订阅进程级事件泵（见 `pump()`），
/// 因此可以与其它订阅者（等待前台/输入表面的一次性等待）共存，「同一进程只允许一个
/// 观察器」的历史限制随之取消。
///
/// 订阅是**惰性建立**的：低配机冷启动时泵可能还没装好钩子，若在构造期把「装不上」
/// 固化成永久空订阅，进程整个生命周期都会停在壳层的退路 Timer 上（时序驱动）。
/// 因此每次 `next_foreground` / `set_wakeup` 都重试 `pump()`，泵自愈后自动接上——
/// 配合壳层「退路 Timer 每轮重试接管」形成闭环（D40）。
pub struct Win32ForegroundObserver {
    inner: Option<&'static Arc<PumpInner>>,
    /// 供 UI 线程 `next_foreground()` 抽取快照的订阅通道。
    events_id: u64,
    events_rx: Option<Receiver<PumpEvent>>,
    /// `set_wakeup` 注册后的独立唤醒订阅与线程。Drop 时摘除订阅即令线程收到
    /// `Disconnected` 而自然退出。
    wakeup_id: Option<u64>,
    wakeup_thread: Option<std::thread::JoinHandle<()>>,
}

impl Win32ForegroundObserver {
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: None,
            events_id: 0,
            events_rx: None,
            wakeup_id: None,
            wakeup_thread: None,
        })
    }

    /// 泵就绪且尚未订阅时建立订阅；返回是否已具备可用的事件通道。
    /// 建立后不再重建——重复 `next_foreground` 不得累积订阅。
    fn ensure_events(&mut self) -> bool {
        if self.events_rx.is_some() {
            return true;
        }
        let Some(inner) = pump() else {
            return false;
        };
        let (id, receiver) = inner.subscribe();
        self.inner = Some(inner);
        self.events_id = id;
        self.events_rx = Some(receiver);
        true
    }
}

impl ForegroundObserver for Win32ForegroundObserver {
    fn next_foreground(&mut self) -> Result<Option<WindowSnapshot>> {
        if !self.ensure_events() {
            return Ok(None);
        }
        let Some(receiver) = self.events_rx.as_ref() else {
            return Ok(None);
        };
        // 一次抽干通道，只认前台事件，返回最新一次的窗口快照。事件洪泛下取最后一个，
        // 避免逐个回放已经过时的前台。
        let mut latest: Option<HWND> = None;
        loop {
            match receiver.try_recv() {
                Ok(event) if event.event == EVENT_SYSTEM_FOREGROUND => {
                    latest = Some(event.hwnd.0 as HWND);
                }
                Ok(_) => continue,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(PlatformError::Window("WinEvent 前台观察通道已断开".into()))
                }
            }
        }
        Ok(latest.and_then(snapshot_window))
    }

    fn set_wakeup(&mut self, wakeup: Box<dyn Fn() + Send + Sync>) -> bool {
        if self.wakeup_thread.is_some() {
            return true;
        }
        let Some(inner) = pump() else {
            return false;
        };
        // 唤醒走独立的第二条订阅，与 `next_foreground` 的通道互不争用（mpsc 单消费者）。
        let (id, rx) = inner.subscribe();
        let handle = std::thread::Builder::new()
            .name("foreground-wakeup".into())
            .spawn(move || {
                // 阻塞收；只对前台事件敲一次唤醒。回调体本身只做「唤醒 UI 事件循环」，
                // 真正的 poll 在 UI 线程上跑，故不违反「唤醒回调内禁止再订阅」。
                while let Ok(event) = rx.recv() {
                    if event.event == EVENT_SYSTEM_FOREGROUND {
                        wakeup();
                    }
                }
            })
            .ok();
        match handle {
            Some(handle) => {
                self.wakeup_id = Some(id);
                self.wakeup_thread = Some(handle);
                true
            }
            None => {
                inner.unsubscribe(id);
                false
            }
        }
    }
}

impl Drop for Win32ForegroundObserver {
    fn drop(&mut self) {
        if let Some(inner) = self.inner {
            inner.unsubscribe(self.events_id);
            if let Some(id) = self.wakeup_id.take() {
                // 摘除订阅 → 泵侧 sender 释放 → 唤醒线程 recv 得到 Disconnected 后退出。
                inner.unsubscribe(id);
            }
        }
        if let Some(handle) = self.wakeup_thread.take() {
            let _ = handle.join();
        }
    }
}

/// 泵线程扇出的一条窗口事件。
///
/// `root` 是 `GetAncestor(hwnd, GA_ROOT)`：微信/千牛的会话区、输入框与主窗口是不同
/// HWND，只比 `hwnd` 会漏掉子控件上的事件，所以在泵线程内就把根窗口解析出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PumpEvent {
    event: u32,
    hwnd: WindowHandle,
    root: WindowHandle,
    process_id: u32,
    object_id: i32,
}

/// 订阅方要等的那件事。过滤放在订阅侧而不是钩子侧，是为了让「排除自身进程」这类
/// 判断显式可读——因此钩子不加 `WINEVENT_SKIPOWNPROCESS`。
#[derive(Debug, Clone, Copy)]
enum EventMatcher {
    /// 目标窗口成为前台。
    Foreground { root: WindowHandle },
    /// 目标进程内出现可输入表面的迹象：焦点落到其控件上，或插入符位置变化。
    InputSurface { root: WindowHandle, process_id: u32 },
    /// 目标进程内的任意已过滤事件（供 `real-im-verify --tail-probe` 观测渲染静默）。
    AnyInProcess { process_id: u32 },
}

impl EventMatcher {
    fn matches(&self, event: &PumpEvent) -> bool {
        match *self {
            EventMatcher::Foreground { root } => {
                event.event == EVENT_SYSTEM_FOREGROUND && (event.hwnd == root || event.root == root)
            }
            EventMatcher::InputSurface { root, process_id } => {
                // 同一目标可以由「窗口树命中」或「进程命中」两种方式认领：微信的输入区
                // 在某些版本上 GA_ROOT 不回到会话窗口，仅靠 root 会漏。
                let belongs = event.root == root
                    || event.hwnd == root
                    || (process_id != 0 && event.process_id == process_id);
                if !belongs {
                    return false;
                }
                match event.event {
                    EVENT_OBJECT_FOCUS => true,
                    EVENT_OBJECT_LOCATIONCHANGE => event.object_id == OBJID_CARET,
                    _ => false,
                }
            }
            EventMatcher::AnyInProcess { process_id } => {
                process_id != 0 && event.process_id == process_id
            }
        }
    }
}

struct Subscription {
    id: u64,
    sender: Sender<PumpEvent>,
}

/// 事件泵的共享状态。订阅表由泵线程（扇出）与调用线程（增删）共用一把锁，
/// 因此**唤醒回调内禁止再订阅**，否则自锁。
///
/// `hooks_installed` / `thread_id` / `last_attempt_ms` 三个字段构成泵的自愈状态机：
/// 低配机冷启动时泵线程可能没能在 [`PUMP_STARTUP_CAP_MS`] 内装完钩子，这只是
/// 「暂未就绪」而不是「永久失败」——线程装完后自行翻位，下一次 `pump()` 即恢复
/// 事件驱动；真正装失败（权限/会话）则冷却 [`PUMP_REINSTALL_COOLDOWN_MS`] 后重装。
struct PumpInner {
    /// 泵线程 id，由泵线程自己回报；仅用于 `Drop` 时投递 `WM_QUIT`。
    thread_id: AtomicU32,
    next_id: AtomicU64,
    subscriptions: Mutex<Vec<Subscription>>,
    /// 是否存在「已装好钩子」的活跃泵线程。
    hooks_installed: AtomicBool,
    /// 上一次发起装钩子尝试的时刻（[`monotonic_ms`] 基准），兼作重装互斥。
    last_attempt_ms: AtomicU64,
}

impl PumpInner {
    fn subscribe(&self) -> (u64, Receiver<PumpEvent>) {
        let id = self.next_id.fetch_add(1, AtomicOrdering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        if let Ok(mut subs) = self.subscriptions.lock() {
            subs.push(Subscription { id, sender });
        }
        (id, receiver)
    }

    fn unsubscribe(&self, id: u64) {
        if let Ok(mut subs) = self.subscriptions.lock() {
            subs.retain(|sub| sub.id != id);
        }
    }

    fn fan_out(&self, event: PumpEvent) {
        if let Ok(mut subs) = self.subscriptions.lock() {
            // 发送失败意味着订阅方已走，顺手摘除，避免订阅表随时间单调增长。
            subs.retain(|sub| sub.sender.send(event).is_ok());
        }
    }
}

impl Drop for PumpInner {
    fn drop(&mut self) {
        // 泵线程阻塞在 GetMessageW 上，只有 WM_QUIT 能让它有序退出并摘钩子。
        let thread_id = self.thread_id.load(AtomicOrdering::Relaxed);
        if thread_id != 0 {
            unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
        }
    }
}

static PUMP: OnceLock<Arc<PumpInner>> = OnceLock::new();
/// 回调侧单独持有一份引用：钩子在泵线程启动时就装上，那时 `PUMP` 还没写入
/// （`get_or_init` 尚未返回）。分开两个 static 让回调路径不依赖初始化时序。
static PUMP_INNER: OnceLock<Arc<PumpInner>> = OnceLock::new();

unsafe extern "system" fn pump_callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object_id: i32,
    _child_id: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if hwnd.is_null() {
        return;
    }
    // LOCATIONCHANGE 是全系统最吵的事件之一；只有插入符那一路对我们有意义，
    // 其余在扇出之前就丢掉，避免把噪声灌进每个订阅通道。
    if event == EVENT_OBJECT_LOCATIONCHANGE && object_id != OBJID_CARET {
        return;
    }
    let Some(inner) = PUMP_INNER.get() else {
        return;
    };
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    inner.fan_out(PumpEvent {
        event,
        hwnd: WindowHandle(hwnd as isize),
        root: WindowHandle(if root.is_null() { hwnd } else { root } as isize),
        process_id: window_process_id(hwnd),
        object_id,
    });
}

/// 取进程内唯一的事件泵，首次调用时惰性拉起泵线程。
///
/// 为什么必须是专用线程：`WINEVENT_OUTOFCONTEXT` 的回调由**安装钩子的那个线程的
/// 消息泵**投递。若把钩子装在 Slint UI 线程上，再在同一线程阻塞等事件，就等于
/// 让投递者去等自己——事件永远不会到。
///
/// 返回 `None` 只表示「此刻还没有已装好钩子的泵线程」，**不是永久判决**：
/// 低配机冷启动时首装握手可能超时（D40 根因），泵线程会继续把钩子装完并翻位
/// `hooks_installed`；之后任何一次 `pump()`（订阅、观察器轮询、壳层 Timer 退路
/// 的接管重试）都会自动恢复事件驱动。真装失败则冷却
/// [`PUMP_REINSTALL_COOLDOWN_MS`] 后由 [`try_reinstall_pump`] 重装。
fn pump() -> Option<&'static Arc<PumpInner>> {
    let inner = PUMP.get_or_init(spawn_pump);
    if inner.hooks_installed.load(AtomicOrdering::Acquire) {
        return Some(inner);
    }
    try_reinstall_pump(inner);
    None
}

/// 自进程起点的单调毫秒数，用于泵重装的冷却计时（`Instant` 无法原子存储）。
fn monotonic_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 创建泵共享状态并起首个泵线程。永不返回「失败态」——线程起不来这类资源
/// 耗尽级异常也只记冷却时刻，留给后续 `pump()` 重试；握手超时同样只是暂态。
fn spawn_pump() -> Arc<PumpInner> {
    let inner = Arc::new(PumpInner {
        thread_id: AtomicU32::new(0),
        next_id: AtomicU64::new(1),
        subscriptions: Mutex::new(Vec::new()),
        hooks_installed: AtomicBool::new(false),
        last_attempt_ms: AtomicU64::new(0),
    });
    // 回调可能在 spawn 返回之前就触发，所以先让回调侧看得见共享状态。
    let _ = PUMP_INNER.set(Arc::clone(&inner));

    let (ready_tx, ready_rx) = mpsc::channel::<bool>();
    if spawn_pump_thread(&inner, Some(ready_tx)).is_none() {
        inner
            .last_attempt_ms
            .store(monotonic_ms(), AtomicOrdering::Release);
        log::warn!("WinEvent 泵线程创建失败，冷却后将重试");
        return inner;
    }
    // 钩子必须先装上，订阅才有意义；同步等首次握手，但超时不是失败——
    // 线程仍在装，装好后 `hooks_installed` 自动翻位。低配机冷启动（UI 初始化
    // 抢占 CPU）下 500ms 握手超时是已观测到的常态而非异常，必须允许迟到。
    match ready_rx.recv_timeout(Duration::from_millis(PUMP_STARTUP_CAP_MS)) {
        Ok(true) => {}
        _ => log::warn!(
            "WinEvent 泵 {PUMP_STARTUP_CAP_MS}ms 内未完成钩子握手(低配机冷启动常见)，\
             事件订阅暂不可用，钩子装好后自动恢复"
        ),
    }
    inner
}

/// 起一个泵线程：装钩子 → 翻位就绪 → 消息循环，退出时摘钩并复位自愈状态。
/// `ready` 为 `Some` 时是首装路径（调用方同步等握手）；`None` 是重装路径。
fn spawn_pump_thread(
    inner: &Arc<PumpInner>,
    ready: Option<Sender<bool>>,
) -> Option<std::thread::JoinHandle<()>> {
    let inner = Arc::clone(inner);
    std::thread::Builder::new()
        .name("winevent-pump".into())
        .spawn(move || {
            inner
                .thread_id
                .store(unsafe { GetCurrentThreadId() }, AtomicOrdering::Release);
            let hooks = install_pump_hooks();
            if hooks.is_empty() {
                // 没装上任何一个钩子就全部摘掉重来；记录尝试时刻，冷却后可重装。
                inner.thread_id.store(0, AtomicOrdering::Release);
                inner
                    .last_attempt_ms
                    .store(monotonic_ms(), AtomicOrdering::Release);
                if let Some(sender) = &ready {
                    let _ = sender.send(false);
                }
                return;
            }
            inner.hooks_installed.store(true, AtomicOrdering::Release);
            if let Some(sender) = &ready {
                let _ = sender.send(true);
            }

            // windows-sys 的 MSG 是纯 POD，无 Default 实现；零初始化即合法初值。
            let mut message: MSG = unsafe { std::mem::zeroed() };
            // GetMessageW 返回 0 即收到 WM_QUIT；-1 是错误，同样退出。
            while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            for hook in hooks {
                unsafe { UnhookWinEvent(hook) };
            }
            // 退出即摘钩：复位就绪标记并记录时刻，冷却过后允许重装。
            inner.hooks_installed.store(false, AtomicOrdering::Release);
            inner.thread_id.store(0, AtomicOrdering::Release);
            inner
                .last_attempt_ms
                .store(monotonic_ms(), AtomicOrdering::Release);
        })
        .ok()
}

/// 泵重装判定（纯函数，单测锁定）：钩子没装上 && 无活跃泵线程 && 冷却已过。
/// 「活跃泵线程」必须检查——握手超时的线程可能还在装钩子，此刻再 spawn 一个
/// 只会得到两个泵双份扇出。
fn should_reinstall(
    hooks_installed: bool,
    pump_thread_alive: bool,
    last_attempt_ms: u64,
    now_ms: u64,
) -> bool {
    !hooks_installed
        && !pump_thread_alive
        && now_ms.saturating_sub(last_attempt_ms) >= PUMP_REINSTALL_COOLDOWN_MS
}

/// 尝试重装泵钩子。每次 `pump()` 发现泵不可用都会走到这里，因此任何下游动作
/// （订阅、观察器惰性重连、壳层退路 Timer 的接管重试）都是自愈入口。
fn try_reinstall_pump(inner: &Arc<PumpInner>) {
    let now = monotonic_ms();
    let last = inner.last_attempt_ms.load(AtomicOrdering::Acquire);
    if !should_reinstall(
        inner.hooks_installed.load(AtomicOrdering::Acquire),
        inner.thread_id.load(AtomicOrdering::Acquire) != 0,
        last,
        now,
    ) {
        return;
    }
    // CAS 抢占安装权：并发调用者只有一个能赢；冷却与互斥共用同一字段。
    if inner
        .last_attempt_ms
        .compare_exchange(last, now, AtomicOrdering::AcqRel, AtomicOrdering::Acquire)
        .is_err()
    {
        return;
    }
    log::info!("WinEvent 泵不可用且冷却期已过，重装钩子(事件驱动自愈)");
    if spawn_pump_thread(inner, None).is_some() {
        log::info!("WinEvent 泵重装线程已启动");
    }
}

fn install_pump_hooks() -> Vec<HWINEVENTHOOK> {
    let mut hooks = Vec::new();
    for (from, to) in [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_OBJECT_FOCUS, EVENT_OBJECT_FOCUS),
        (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
    ] {
        let hook = unsafe {
            SetWinEventHook(
                from,
                to,
                ptr::null_mut(),
                Some(pump_callback),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        if !hook.is_null() {
            hooks.push(hook);
        }
    }
    if hooks.len() < 3 {
        for hook in hooks.drain(..) {
            unsafe { UnhookWinEvent(hook) };
        }
    }
    hooks
}

/// 事件源实现。无状态：所有共享状态都在进程级泵里。
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32WindowEvents;

impl WindowEventSource for Win32WindowEvents {
    fn await_foreground(&self, window: WindowHandle) -> Box<dyn EventWait> {
        Box::new(Win32EventWait::new(EventMatcher::Foreground {
            root: window,
        }))
    }

    fn await_input_surface(&self, window: WindowHandle) -> Box<dyn EventWait> {
        let process_id = window_process_id(window.0 as HWND);
        Box::new(Win32EventWait::new(EventMatcher::InputSurface {
            root: window,
            process_id,
        }))
    }
}

impl Win32WindowEvents {
    /// 订阅目标进程的任意已过滤事件（前台/焦点/插入符位置变化）。
    ///
    /// 这是验证工具用的“原始事件流”入口：`--tail-probe` 用它持续观测 Ctrl+V 之后
    /// 目标进程还有多久才安静下来，作为 IM 内部渲染耗时的可观测代理。
    pub fn await_process_activity(&self, window: WindowHandle) -> Box<dyn EventWait> {
        let process_id = window_process_id(window.0 as HWND);
        Box::new(Win32EventWait::new(EventMatcher::AnyInProcess {
            process_id,
        }))
    }
}

/// 一个已建立的订阅。**构造即订阅**——调用方拿到它之后才发起动作，
/// 从而不存在「动作已完成但订阅还没装上」的窗口期。
struct Win32EventWait {
    inner: Option<&'static Arc<PumpInner>>,
    id: u64,
    receiver: Option<Receiver<PumpEvent>>,
    matcher: EventMatcher,
    since: Instant,
}

impl Win32EventWait {
    fn new(matcher: EventMatcher) -> Self {
        match pump() {
            Some(inner) => {
                let (id, receiver) = inner.subscribe();
                Self {
                    inner: Some(inner),
                    id,
                    receiver: Some(receiver),
                    matcher,
                    since: Instant::now(),
                }
            }
            None => Self {
                inner: None,
                id: 0,
                receiver: None,
                matcher,
                since: Instant::now(),
            },
        }
    }
}

impl EventWait for Win32EventWait {
    fn wait(&mut self, cap_ms: u64) -> WaitOutcome {
        let Some(receiver) = self.receiver.as_ref() else {
            return WaitOutcome::Unavailable;
        };
        // 第一步：先抽干**已经缓冲**的事件，不占用时间预算。
        // 订阅可能远早于本次 `wait`（如输入表面在激活动作之前就订阅），目标事件
        // 可能在 `wait` 被调用前就已到达并躺在通道里。这一步保证「事件已来」的
        // 快路径命中，不会因订阅至今的耗时超过 `cap_ms` 而被误判成 `CappedOut`。
        loop {
            match receiver.try_recv() {
                Ok(event) if self.matcher.matches(&event) => {
                    log::trace!(
                        target: "paste_trace::platform::events",
                        "evt[buffered-hit] {:?} evt=0x{:X} obj={}",
                        self.matcher, event.event, event.object_id
                    );
                    return WaitOutcome::Observed {
                        elapsed_ms: self.since.elapsed().as_millis() as u64,
                    }
                }
                Ok(other) => {
                    log::trace!(
                        target: "paste_trace::platform::events",
                        "evt[buffered-miss] evt=0x{:X} obj={} hwnd={:?} root={:?} pid={}",
                        other.event, other.object_id, other.hwnd.0, other.root.0, other.process_id
                    );
                    continue;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return WaitOutcome::Unavailable,
            }
        }
        // 第二步：缓冲里没有，再阻塞等待。`cap_ms` 是「从此刻起最多等这么久」的
        // 新鲜预算，不与订阅至今的耗时相减——否则早订阅的等待器会退化成不等。
        let budget_end = Instant::now() + Duration::from_millis(cap_ms);
        loop {
            let Some(remaining) = budget_end.checked_duration_since(Instant::now()) else {
                return WaitOutcome::CappedOut;
            };
            match receiver.recv_timeout(remaining) {
                Ok(event) if self.matcher.matches(&event) => {
                    log::trace!(
                        target: "paste_trace::platform::events",
                        "evt[live-hit] {:?} evt=0x{:X} obj={}",
                        self.matcher, event.event, event.object_id
                    );
                    return WaitOutcome::Observed {
                        elapsed_ms: self.since.elapsed().as_millis() as u64,
                    }
                }
                // 不匹配的事件继续丢弃：一条通道承载全部事件，过滤在此处完成。
                Ok(other) => {
                    log::trace!(
                        target: "paste_trace::platform::events",
                        "evt[live-miss] evt=0x{:X} obj={} hwnd={:?} root={:?} pid={}",
                        other.event, other.object_id, other.hwnd.0, other.root.0, other.process_id
                    );
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return WaitOutcome::CappedOut,
                Err(mpsc::RecvTimeoutError::Disconnected) => return WaitOutcome::Unavailable,
            }
        }
    }
}

impl Drop for Win32EventWait {
    fn drop(&mut self) {
        if let Some(inner) = self.inner {
            inner.unsubscribe(self.id);
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Win32Readiness;

impl ReadinessProbe for Win32Readiness {
    fn probe(&self, window: WindowHandle, timeout_ms: u64) -> ReadinessSignal {
        match self.blockers(window) {
            ReadinessSignal::Blocked(blocker) => return ReadinessSignal::Blocked(blocker),
            ReadinessSignal::Ready | ReadinessSignal::Inconclusive => {}
        }
        match uia_editable_focus(WindowHandle(window.0), timeout_ms) {
            Ok(true) => ReadinessSignal::Ready,
            Ok(false) => ReadinessSignal::Inconclusive,
            Err(_) => ReadinessSignal::Inconclusive,
        }
    }

    /// 只有两项本进程内的 O(1) 检查，没有 UIA、没有等待。
    fn blockers(&self, window: WindowHandle) -> ReadinessSignal {
        let hwnd = window.0 as HWND;
        if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
            return ReadinessSignal::Blocked(ReadinessBlocker::WindowGone);
        }
        if unsafe { IsWindowEnabled(hwnd) } == 0 {
            return ReadinessSignal::Blocked(ReadinessBlocker::ModalBlocking);
        }
        ReadinessSignal::Inconclusive
    }
}

/// UIA 浅层输入框探测：当前聚焦元素为可编辑控件，且父链最终属于目标 HWND。
///
/// 任何 UIA 失败都返回 `Ok(false)`/`Err`，上层统一映射为 `Inconclusive`，
/// 不会把“探不到”伪装成明确的 `NotReady`。
fn uia_editable_focus(window: WindowHandle, _timeout_ms: u64) -> windows::core::Result<bool> {
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
    if !matches!(hr, S_OK | S_FALSE | RPC_E_CHANGED_MODE) {
        return Ok(false);
    }

    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let mut raw_process_id = 0u32;
    unsafe { GetWindowThreadProcessId(window.0 as HWND, &mut raw_process_id) };
    if raw_process_id == 0 {
        return Ok(false);
    }
    let process_id = i32::try_from(raw_process_id).unwrap_or(-1);
    if uia_global_editable_focus(process_id, &automation)? {
        return Ok(true);
    }
    if uia_has_editable_descendant(window, &automation)? {
        return Ok(true);
    }
    Ok(false)
}

/// 微信 4.0 等自绘应用只在目标窗口获得焦点后才向 UIA 暴露内部输入框，
/// 因此除了扫描目标窗口树，还要查看系统当前焦点元素。
fn uia_global_editable_focus(
    target_process_id: i32,
    automation: &IUIAutomation,
) -> windows::core::Result<bool> {
    let focused = unsafe { automation.GetFocusedElement()? };
    let control_type = unsafe { focused.CurrentControlType()? };
    if control_type != UIA_EditControlTypeId && control_type != UIA_DocumentControlTypeId {
        return Ok(false);
    }
    if unsafe { focused.CurrentProcessId()? } != target_process_id {
        return Ok(false);
    }
    if !unsafe { focused.CurrentHasKeyboardFocus()? }.as_bool() {
        return Ok(false);
    }
    let pattern = match unsafe {
        focused.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
    } {
        Ok(pattern) => pattern,
        Err(_) => return Ok(false),
    };
    let enabled = unsafe { focused.CurrentIsEnabled()? };
    let read_only = unsafe { pattern.CurrentIsReadOnly()? };
    Ok(enabled.as_bool() && !read_only.as_bool())
}

fn uia_has_editable_descendant(
    window: WindowHandle,
    automation: &IUIAutomation,
) -> windows::core::Result<bool> {
    let root =
        unsafe { automation.ElementFromHandle(WinHWND(window.0 as *mut core::ffi::c_void))? };
    let condition = unsafe { automation.CreateTrueCondition()? };
    let all = unsafe { root.FindAll(TreeScope_Descendants, &condition)? };
    let length = unsafe { all.Length()? };
    for index in 0..length.min(200) {
        let element = unsafe { all.GetElement(index)? };
        let control_type = unsafe { element.CurrentControlType()? };
        if control_type != UIA_EditControlTypeId && control_type != UIA_DocumentControlTypeId {
            continue;
        }
        let has_focus = unsafe { element.CurrentHasKeyboardFocus()? };
        if !has_focus.as_bool() {
            continue;
        }
        let value_pattern = match unsafe {
            element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        } {
            Ok(pattern) => pattern,
            Err(_) => continue,
        };
        let enabled = unsafe { element.CurrentIsEnabled()? };
        let read_only = unsafe { value_pattern.CurrentIsReadOnly()? };
        if enabled.as_bool() && !read_only.as_bool() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 只读 UIA 焦点诊断，用于真实 IM 上框调试；产品路径不依赖它。
pub fn uia_focus_debug(window: WindowHandle) -> String {
    let mut lines = Vec::new();
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    {
        use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
        if !matches!(hr, S_OK | S_FALSE | RPC_E_CHANGED_MODE) {
            return format!("CoInitializeEx failed: {hr:?}");
        }
    }

    let automation: IUIAutomation =
        match unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) } {
            Ok(value) => value,
            Err(error) => return format!("CoCreateInstance failed: {error}"),
        };
    let focused = match unsafe { automation.GetFocusedElement() } {
        Ok(value) => value,
        Err(error) => return format!("GetFocusedElement failed: {error}"),
    };
    let control = unsafe { focused.CurrentControlType() };
    let enabled = unsafe { focused.CurrentIsEnabled() };
    let name = unsafe { focused.CurrentName() };
    let class_name = unsafe { focused.CurrentClassName() };
    let native = unsafe { focused.CurrentNativeWindowHandle() };
    let process_id = unsafe { focused.CurrentProcessId() };
    lines.push(format!("focused control={control:?} enabled={enabled:?}"));
    if let Ok(process_id) = process_id {
        lines.push(format!("focused process={process_id}"));
    }
    if let Ok(name) = name {
        lines.push(format!("focused name={name}"));
    }
    if let Ok(class_name) = class_name {
        lines.push(format!("focused class={class_name}"));
    }
    if let Ok(native) = native {
        lines.push(format!("focused native={}", native.0 as isize));
    }
    if let Ok(pattern) =
        unsafe { focused.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
    {
        if let Ok(value) = unsafe { pattern.CurrentValue() } {
            lines.push(format!("focused value={value}"));
        }
    }

    if let Ok(walker) = unsafe { automation.ControlViewWalker() } {
        let mut current = focused;
        for depth in 0..8 {
            let native = unsafe { current.CurrentNativeWindowHandle() };
            let control = unsafe { current.CurrentControlType() };
            let name = unsafe { current.CurrentName() };
            lines.push(format!(
                "parent[{depth}] control={control:?} native={:?} name={:?}",
                native.as_ref().map(|h| h.0 as isize),
                name.map(|v| v.to_string())
            ));
            match unsafe { walker.GetParentElement(&current) } {
                Ok(parent) => current = parent,
                Err(_) => break,
            }
            if native.map(|h| h.0 as isize == window.0).unwrap_or(false) {
                break;
            }
        }
    }
    if let Ok(condition) = unsafe { automation.CreateTrueCondition() } {
        if let Ok(root) =
            unsafe { automation.ElementFromHandle(WinHWND(window.0 as *mut core::ffi::c_void)) }
        {
            if let Ok(all) = unsafe { root.FindAll(TreeScope_Descendants, &condition) } {
                if let Ok(length) = unsafe { all.Length() } {
                    lines.push(format!("descendant count={length}"));
                    for index in 0..length {
                        if let Ok(element) = unsafe { all.GetElement(index) } {
                            let control = unsafe { element.CurrentControlType() };
                            let name = unsafe { element.CurrentName() };
                            let native = unsafe { element.CurrentNativeWindowHandle() };
                            let enabled = unsafe { element.CurrentIsEnabled() };
                            let process_id = unsafe { element.CurrentProcessId() };
                            let value = unsafe {
                                element
                                    .GetCurrentPatternAs::<IUIAutomationValuePattern>(
                                        UIA_ValuePatternId,
                                    )
                                    .and_then(|pattern| {
                                        pattern.CurrentValue().map(|value| value.to_string())
                                    })
                            };
                            lines.push(format!(
                                "desc[{index}] control={control:?} name={:?} native={:?} process={:?} enabled={enabled:?} value={value:?}",
                                name.map(|value| value.to_string()),
                                native.map(|h| h.0 as isize),
                                process_id
                            ));
                            if matches!(
                                &control,
                                Ok(value) if *value == UIA_DocumentControlTypeId
                            ) {
                                if let Ok(nested_all) =
                                    unsafe { element.FindAll(TreeScope_Descendants, &condition) }
                                {
                                    if let Ok(nested_length) = unsafe { nested_all.Length() } {
                                        let nested_name = unsafe { element.CurrentName() }
                                            .map(|value| value.to_string())
                                            .unwrap_or_default();
                                        lines.push(format!(
                                            "  nested_document[{nested_name}] count={nested_length}"
                                        ));
                                        for nested_index in 0..nested_length.min(120) {
                                            if let Ok(nested) =
                                                unsafe { nested_all.GetElement(nested_index) }
                                            {
                                                let nested_control =
                                                    unsafe { nested.CurrentControlType() };
                                                let nested_name = unsafe { nested.CurrentName() };
                                                let nested_value = unsafe {
                                                    nested
                                                        .GetCurrentPatternAs::<IUIAutomationValuePattern>(
                                                            UIA_ValuePatternId,
                                                        )
                                                        .and_then(|pattern| {
                                                            pattern.CurrentValue().map(|value| {
                                                                value.to_string()
                                                            })
                                                        })
                                                };
                                                lines.push(format!(
                                                    "    nested[{nested_index}] control={nested_control:?} name={:?} value={nested_value:?}",
                                                    nested_name
                                                        .map(|value| value.to_string())
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    lines.join("\n")
}

/// 只读读取目标窗口内聊天/编辑文档的可见文本，用于哨兵读回验证。
pub fn uia_read_visible_text(window: WindowHandle) -> std::result::Result<String, String> {
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    {
        use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
        if !matches!(hr, S_OK | S_FALSE | RPC_E_CHANGED_MODE) {
            return Err(format!("CoInitializeEx failed: {hr:?}"));
        }
    }
    let mut raw_target_process_id = 0u32;
    unsafe { GetWindowThreadProcessId(window.0 as HWND, &mut raw_target_process_id) };
    let target_process_id = i32::try_from(raw_target_process_id).unwrap_or(-1);
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|error| error.to_string())?;
    let mut values = Vec::new();

    if let Ok(focused) = unsafe { automation.GetFocusedElement() } {
        let process_id = unsafe { focused.CurrentProcessId() }.unwrap_or(-1);
        let Ok(control_type) = (unsafe { focused.CurrentControlType() }) else {
            return Err("无法读取当前焦点控件类型".to_string());
        };
        if process_id == target_process_id
            && (control_type == UIA_EditControlTypeId || control_type == UIA_DocumentControlTypeId)
        {
            let name = unsafe { focused.CurrentName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            if let Ok(pattern) = unsafe {
                focused.GetCurrentPatternAs::<windows::Win32::UI::Accessibility::IUIAutomationTextPattern>(
                    UIA_TextPatternId,
                )
            } {
                if let Ok(range) = unsafe { pattern.DocumentRange() } {
                    if let Ok(text) = unsafe { range.GetText(4096) } {
                        values.push(format!("text[{name}]: {text}"));
                    }
                }
            } else if let Ok(pattern) = unsafe {
                focused.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            } {
                if let Ok(value) = unsafe { pattern.CurrentValue() } {
                    values.push(format!("value[{name}]: {value}"));
                }
            }
        }
    }

    let root = unsafe { automation.ElementFromHandle(WinHWND(window.0 as *mut core::ffi::c_void)) }
        .map_err(|error| error.to_string())?;
    let condition = unsafe { automation.CreateTrueCondition() }.map_err(|e| e.to_string())?;
    let all =
        unsafe { root.FindAll(TreeScope_Descendants, &condition) }.map_err(|e| e.to_string())?;
    let length = unsafe { all.Length() }.map_err(|e| e.to_string())?;

    for index in 0..length {
        let element = unsafe { all.GetElement(index) }.map_err(|e| e.to_string())?;
        let control_type = unsafe { element.CurrentControlType() }.map_err(|e| e.to_string())?;
        if control_type != UIA_DocumentControlTypeId && control_type != UIA_EditControlTypeId {
            continue;
        }
        let name = unsafe { element.CurrentName() }
            .map(|v| v.to_string())
            .unwrap_or_default();
        if let Ok(pattern) = unsafe {
            element
                .GetCurrentPatternAs::<windows::Win32::UI::Accessibility::IUIAutomationTextPattern>(
                    UIA_TextPatternId,
                )
        } {
            if let Ok(range) = unsafe { pattern.DocumentRange() } {
                if let Ok(text) = unsafe { range.GetText(4096) } {
                    values.push(format!("text[{name}]: {text}"));
                    continue;
                }
            }
        }
        if let Ok(pattern) =
            unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
        {
            if let Ok(value) = unsafe { pattern.CurrentValue() } {
                values.push(format!("value[{name}]: {value}"));
            }
        }
    }
    Ok(values.join("\n"))
}

/// 切换微信会话后**最多**等输入区重建多久（仅开发验证入口用）。
const WECHAT_SESSION_SWITCH_CAP_MS: u64 = 400;

/// 在目标窗口内选择「文件传输助手」并把焦点移到微信聊天输入框。
///
/// 这个入口只用于真实 IM 的开发验证，避免多实例下全局快捷键落到错误账号。
/// 产品路径不调用本函数；上线前仍应通过目标画像的会话状态确认输入框。
pub fn uia_focus_wechat_input(window: WindowHandle) -> std::result::Result<String, String> {
    let automation = uia_automation()?;
    let mut raw_process_id = 0u32;
    unsafe { GetWindowThreadProcessId(window.0 as HWND, &mut raw_process_id) };
    let target_process_id = i32::try_from(raw_process_id).unwrap_or(-1);

    let contact = uia_search_from_focus(&automation, target_process_id, |element| {
        unsafe { element.CurrentName() }
            .map(|name| name.to_string().contains("文件传输助手"))
            .unwrap_or(false)
    })?
    .ok_or_else(|| "未找到微信「文件传输助手」会话项".to_string())?;

    // 先订阅再动作：切会话会让微信重建输入区，订阅必须早于 Select/Invoke。
    let mut surface = Win32WindowEvents.await_input_surface(window);
    let mut selected = false;
    if let Ok(pattern) = unsafe {
        contact.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    } {
        unsafe { pattern.Select() }.map_err(|e| e.to_string())?;
        selected = true;
    } else if let Ok(pattern) =
        unsafe { contact.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) }
    {
        unsafe { pattern.Invoke() }.map_err(|e| e.to_string())?;
        selected = true;
    }
    if !selected {
        return Err("微信「文件传输助手」不支持 UIA 选择".to_string());
    }

    let _ = surface.wait(WECHAT_SESSION_SWITCH_CAP_MS);
    let edit = uia_search_from_focus(&automation, target_process_id, |element| {
        let Ok(control_type) = (unsafe { element.CurrentControlType() }) else {
            return false;
        };
        let class_name = unsafe { element.CurrentClassName() }
            .map(|value| value.to_string())
            .unwrap_or_default();
        control_type == UIA_EditControlTypeId
            && (class_name.contains("ChatInputField") || class_name.contains("Input"))
    })?
    .ok_or_else(|| "选择会话后未找到微信聊天输入框".to_string())?;
    let name = unsafe { edit.CurrentName() }
        .map(|value| value.to_string())
        .unwrap_or_default();
    unsafe { edit.SetFocus() }.map_err(|e| e.to_string())?;
    Ok(format!("focused={name} selected_contact=true"))
}

fn uia_search_from_focus(
    automation: &IUIAutomation,
    target_process_id: i32,
    predicate: impl Fn(&IUIAutomationElement) -> bool,
) -> std::result::Result<Option<IUIAutomationElement>, String> {
    let walker = unsafe { automation.ControlViewWalker() }.map_err(|e| e.to_string())?;
    let focused = unsafe { automation.GetFocusedElement() }.map_err(|e| e.to_string())?;
    let mut current = focused;
    for _ in 0..8 {
        if unsafe { current.CurrentProcessId() }.unwrap_or(-1) != target_process_id {
            break;
        }
        if let Some(found) = uia_search_subtree(&walker, &current, 0, &predicate)? {
            return Ok(Some(found));
        }
        let parent = match unsafe { walker.GetParentElement(&current) } {
            Ok(parent) => parent,
            Err(_) => break,
        };
        current = parent;
    }
    Ok(None)
}

fn uia_search_subtree(
    walker: &IUIAutomationTreeWalker,
    current: &IUIAutomationElement,
    depth: u8,
    predicate: &impl Fn(&IUIAutomationElement) -> bool,
) -> std::result::Result<Option<IUIAutomationElement>, String> {
    if predicate(current) {
        return Ok(Some(current.clone()));
    }
    if depth >= 6 {
        return Ok(None);
    }
    let mut child = match unsafe { walker.GetFirstChildElement(current) } {
        Ok(child) => child,
        Err(_) => return Ok(None),
    };
    for _ in 0..200 {
        if let Some(found) = uia_search_subtree(walker, &child, depth + 1, predicate)? {
            return Ok(Some(found));
        }
        child = match unsafe { walker.GetNextSiblingElement(&child) } {
            Ok(next) => next,
            Err(_) => return Ok(None),
        };
    }
    Ok(None)
}

thread_local! {
    /// 线程内 `IUIAutomation` 缓存。
    ///
    /// 为什么只能按线程缓存：`IUIAutomation` 是 apartment-threaded 的 COM 对象，
    /// 不是 `Send`，跨线程使用需要 marshal。因此**不得**放进 `OnceLock`/`static`——
    /// 那样既过不了 `Send` 约束，也会把别的线程的套间搅乱。
    ///
    /// 为什么值得缓存：`CoInitializeEx` + `CoCreateInstance` 首次实测 79ms（COM 冷启动），
    /// 预热后同一实例的焦点查询只要 5ms。每次上框重建实例等于每次都付冷启动。
    static UIA_AUTOMATION: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
}

fn uia_automation() -> std::result::Result<IUIAutomation, String> {
    UIA_AUTOMATION.with(|slot| {
        if let Some(existing) = slot.borrow().as_ref() {
            return Ok(existing.clone());
        }
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
        // RPC_E_CHANGED_MODE：宿主（Slint / 测试工具）可能已把本线程初始化成 MTA。
        // 此时沿用宿主套间即可，UIA 仍可用，不必也不该重新初始化。
        if !matches!(hr, S_OK | S_FALSE | RPC_E_CHANGED_MODE) {
            return Err(format!("CoInitializeEx failed: {hr:?}"));
        }
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| e.to_string())?;
        *slot.borrow_mut() = Some(automation.clone());
        Ok(automation)
    })
}

// ---------------------------------------------------------------------------
// 输入框焦点获取
// ---------------------------------------------------------------------------

/// 锚点比例的安全区间：贴边的比例容易落到滚动条 / 窗口边框 / 工具条上，
/// 夹紧后即便画像写了 0.0 或 1.0 也不会点到输入框以外的控件。
const ANCHOR_RATIO_MIN: f32 = 0.02;
const ANCHOR_RATIO_MAX: f32 = 0.98;

/// UIA `SetFocus` 之后**最多**等目标应用移动键盘焦点多久。目标一报焦点事件即提前返回。
const UIA_FOCUS_SETTLE_CAP_MS: u64 = 40;
/// 锚点单击之后**最多**等目标应用建立插入符多久。目标一报焦点/插入符事件即提前返回。
const ANCHOR_CLICK_SETTLE_CAP_MS: u64 = 60;

/// 把键盘焦点送进聊天输入框的按计划降级实现。
///
/// 为什么需要它（真机根因）：`SetForegroundWindow` 只把窗口提到前台，
/// 刚激活时键盘焦点停在窗口根控件（微信 `Qt51514QWindowIcon` / 千牛
/// `Qt5152QWindowIcon`），此时 Ctrl+V 落空——素材已在剪贴板但没有进输入框。
///
/// 级别顺序由 [`FocusPlan`] 给出（画像声明，缺省仍是「已可写 → UIA → 锚点」）。
/// 每一级都要**验证**焦点真的落在目标进程的可写控件上才敢声明成功；
/// 验证不了就继续下一级，全部走不通返回 `Unavailable`（＝没能证明，不是证明失败）。
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32InputFocuser;

impl InputFocuser for Win32InputFocuser {
    fn focus_input(&self, window: WindowHandle, plan: &FocusPlan) -> FocusOutcome {
        let outcome = focus_input_by_plan(window, plan);
        // 低配机延迟归因（D41）：三级降级里实际走了哪一级、结果如何，
        // 配合 pipeline 的 focus 段耗时判断锚点单击后的等待是否是大头。
        log::debug!("focus_input outcome={outcome:?} steps={:?}", plan.steps);
        outcome
    }
}

fn focus_input_by_plan(window: WindowHandle, plan: &FocusPlan) -> FocusOutcome {
    let hwnd = window.0 as HWND;
    if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
        return FocusOutcome::Unavailable;
    }
    // 空计划由 targets 层拦下（ProfileError::EmptyFocusStrategy）；平台层遇空
    // 保持纯函数性，什么都不做并如实返回「没能证明」。
    for step in &plan.steps {
        match step {
            FocusStep::AlreadyEditable => {
                if uia_focused_is_editable(window) {
                    return FocusOutcome::AlreadyEditable;
                }
            }
            FocusStep::UiaSetFocus => {
                if uia_set_focus_on_editable(window) {
                    return FocusOutcome::FocusedByUia;
                }
            }
            FocusStep::AnchorClick => {
                if let Some(anchor) = plan.anchor {
                    if let FocusOutcome::FocusedByAnchor = click_anchor(hwnd, anchor) {
                        return FocusOutcome::FocusedByAnchor;
                    }
                }
            }
        }
    }
    FocusOutcome::Unavailable
}

/// 当前系统焦点是否已落在目标窗口所属进程的可写 Edit/Document 上。
fn uia_focused_is_editable(window: WindowHandle) -> bool {
    let Ok(automation) = uia_automation() else {
        return false;
    };
    let Some(process_id) = target_process_id(window) else {
        return false;
    };
    uia_global_editable_focus(process_id, &automation).unwrap_or(false)
}

/// UIA 侧要的是 i32 进程号（与 `IUIAutomationElement::CurrentProcessId` 对齐），
/// 与窗口枚举里返回 u32 的 [`window_process_id`] 不是同一个用途。
fn target_process_id(window: WindowHandle) -> Option<i32> {
    let mut raw = 0u32;
    unsafe { GetWindowThreadProcessId(window.0 as HWND, &mut raw) };
    if raw == 0 {
        return None;
    }
    i32::try_from(raw).ok()
}

/// 在目标窗口的 UIA 子树里找可写 Edit/Document 并 `SetFocus`。
///
/// 只有随后能复核「焦点确实落在目标进程的可写控件上」才返回 true——
/// 千牛聊天区是 CEF Document（渲染进程与窗口进程不同），复核必然不过，
/// 于是自动降级到锚点单击，而不是谎报成功。
fn uia_set_focus_on_editable(window: WindowHandle) -> bool {
    let Ok(automation) = uia_automation() else {
        return false;
    };
    let Ok(root) =
        (unsafe { automation.ElementFromHandle(WinHWND(window.0 as *mut core::ffi::c_void)) })
    else {
        return false;
    };
    let Ok(condition) = (unsafe { automation.CreateTrueCondition() }) else {
        return false;
    };
    let Ok(all) = (unsafe { root.FindAll(TreeScope_Descendants, &condition) }) else {
        return false;
    };
    let Ok(length) = (unsafe { all.Length() }) else {
        return false;
    };
    for index in 0..length.min(200) {
        let Ok(element) = (unsafe { all.GetElement(index) }) else {
            continue;
        };
        let Ok(control_type) = (unsafe { element.CurrentControlType() }) else {
            continue;
        };
        if control_type != UIA_EditControlTypeId && control_type != UIA_DocumentControlTypeId {
            continue;
        }
        if !unsafe { element.CurrentIsEnabled() }
            .map(|value| value.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        // 只读控件（消息流常被建模成只读 Document）不是输入框，跳过。
        if let Ok(pattern) =
            unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
        {
            if unsafe { pattern.CurrentIsReadOnly() }
                .map(|value| value.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
        }
        // 先订阅再动作：订阅在 SetFocus 之前建立，焦点事件即便瞬时到达也不会漏。
        let mut surface = Win32WindowEvents.await_input_surface(window);
        if unsafe { element.SetFocus() }.is_err() {
            continue;
        }
        let outcome = surface.wait(UIA_FOCUS_SETTLE_CAP_MS);
        log::debug!("uia-setfocus settle outcome={outcome:?} cap_ms={UIA_FOCUS_SETTLE_CAP_MS}");
        if uia_focused_is_editable(window) {
            return true;
        }
    }
    false
}

/// 客户区比例锚点 → 客户区内像素偏移。比例先夹紧到安全区间再换算。
fn anchor_offset_in_client(
    client_width: i32,
    client_height: i32,
    anchor: FocusAnchor,
) -> (i32, i32) {
    let clamp = |ratio: f32| ratio.clamp(ANCHOR_RATIO_MIN, ANCHOR_RATIO_MAX);
    let x = (client_width as f32 * clamp(anchor.x_ratio)).round() as i32;
    let y = (client_height as f32 * clamp(anchor.y_ratio)).round() as i32;
    (x, y)
}

/// 屏幕坐标 → `SendInput` 的虚拟桌面归一化坐标（0..=65535）。
///
/// `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` 的坐标基准是整个虚拟桌面，
/// 因此换算必须减去虚拟桌面原点（多显示器时 left/top 可能是负数）。
fn normalize_to_virtual_desktop(
    x: i32,
    y: i32,
    origin: (i32, i32),
    size: (i32, i32),
) -> Option<(i32, i32)> {
    let (origin_x, origin_y) = origin;
    let (width, height) = size;
    if width <= 1 || height <= 1 {
        return None;
    }
    let nx = ((x - origin_x) as i64 * 65_535 / (width - 1) as i64) as i32;
    let ny = ((y - origin_y) as i64 * 65_535 / (height - 1) as i64) as i32;
    Some((nx.clamp(0, 65_535), ny.clamp(0, 65_535)))
}

/// 对画像声明的锚点做一次左键单击。
///
/// 红线：只合成鼠标移动与左键按下/释放，绝不合成任何键盘事件；
/// 点击前必须确认目标仍是前台、且锚点处的顶层窗口就是目标窗口（防止点到遮挡物）。
fn click_anchor(hwnd: HWND, anchor: FocusAnchor) -> FocusOutcome {
    if unsafe { GetForegroundWindow() } != hwnd {
        return FocusOutcome::Unavailable;
    }
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
        return FocusOutcome::Unavailable;
    }
    let (offset_x, offset_y) = anchor_offset_in_client(
        client.right - client.left,
        client.bottom - client.top,
        anchor,
    );
    let mut point = POINT {
        x: client.left + offset_x,
        y: client.top + offset_y,
    };
    if unsafe { ClientToScreen(hwnd, &mut point) } == 0 {
        return FocusOutcome::Unavailable;
    }

    // 锚点必须真的落在目标窗口上：命中的顶层窗口不是目标就说明被遮挡，放弃点击。
    let hit = unsafe { WindowFromPoint(point) };
    if hit.is_null() || unsafe { GetAncestor(hit, GA_ROOT) } != hwnd {
        return FocusOutcome::Unavailable;
    }

    let virtual_origin = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        )
    };
    let virtual_size = unsafe {
        (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let Some((nx, ny)) =
        normalize_to_virtual_desktop(point.x, point.y, virtual_origin, virtual_size)
    else {
        return FocusOutcome::Unavailable;
    };

    let mut restore = POINT { x: 0, y: 0 };
    let has_restore = unsafe { GetCursorPos(&mut restore) } != 0;

    let absolute = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
    let events = [
        mouse_event(nx, ny, absolute | MOUSEEVENTF_MOVE),
        mouse_event(nx, ny, absolute | MOUSEEVENTF_LEFTDOWN),
        mouse_event(nx, ny, absolute | MOUSEEVENTF_LEFTUP),
    ];
    // 先订阅再动作：订阅必须早于 SendInput，否则点击立刻生效时插入符事件已经过去了。
    let mut surface = Win32WindowEvents.await_input_surface(WindowHandle(hwnd as isize));
    let sent = unsafe {
        SendInput(
            events.len() as u32,
            events.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if has_restore {
        unsafe { SetCursorPos(restore.x, restore.y) };
    }
    if sent != events.len() as u32 {
        return FocusOutcome::Unavailable;
    }
    let outcome = surface.wait(ANCHOR_CLICK_SETTLE_CAP_MS);
    log::debug!("anchor-click settle outcome={outcome:?} cap_ms={ANCHOR_CLICK_SETTLE_CAP_MS}");
    // 单击可能唤起了别的窗口（例如目标弹出模态框）；前台漂移即视为没拿到焦点。
    if unsafe { GetForegroundWindow() } != hwnd {
        return FocusOutcome::Unavailable;
    }
    FocusOutcome::FocusedByAnchor
}

fn mouse_event(nx: i32, ny: i32, flags: u32) -> INPUT {
    // 安全：zeroed 补齐联合体尾部与保留字段，随后立即显式赋值所需字段。
    let mut event: INPUT = unsafe { std::mem::zeroed() };
    event.r#type = INPUT_MOUSE;
    event.Anonymous.mi = MOUSEINPUT {
        dx: nx,
        dy: ny,
        mouseData: 0,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    event
}

// ---------------------------------------------------------------------------
// 按键注入
// ---------------------------------------------------------------------------

/// 按键注入实现：把键事件序列逐个合成键盘输入事件并一次性送达。
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32Injector;

/// 单个键事件（[`crate::KEY_UP`] 编码）→ SendInput 的 `INPUT` 结构。
fn keyboard_input(key: u16) -> INPUT {
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
    event
}

/// 检测本实现会注入的修饰键（[`INJECTED_MODIFIERS`]）里哪些仍处于系统级按下态，
/// 返回需要补发的 KEYUP 复位序列。
///
/// 为什么必须复位（D40 低配机实测根因之一）：`SendInput` 的部分拒收（UIPI）会
/// 只送出序列前半段——Ctrl↓ 已生效而 Ctrl↑ 被丢，此后用户的每次普通按键都被
/// 目标应用解释成快捷键；Alt 卡按下则把目标拖进菜单导航。更隐蔽的是和弦拆散：
/// 注入瞬间若发生前台切换（低配机焦点竞争），Ctrl↓ 与 V↓ 会落到**不同线程**的
/// 输入队列，而 Windows 的同步键盘状态按线程隔离——新线程看到的 Ctrl 是抬起，
/// 于是输入框里落下一个裸 'v'。注入前复位修饰键至少消除「残留状态跨次注入」
/// 这一来源；前台拆散则由 activate 的事件证据与注入前校验压制。
fn stuck_modifier_recovery(modifiers: &[u16]) -> Vec<u16> {
    modifiers
        .iter()
        .filter(|&&vk| (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0)
        .map(|&vk| vk | KEY_UP)
        .collect()
}

/// 组装最终注入事件：复位序列在前、业务序列在后（复位必须先于任何新的按下）。
fn assemble_inject_events(recovery: &[u16], keys: &[u16]) -> Vec<INPUT> {
    let mut events: Vec<INPUT> = Vec::with_capacity(recovery.len() + keys.len());
    for &key in recovery.iter().chain(keys) {
        events.push(keyboard_input(key));
    }
    events
}

impl KeyInjector for Win32Injector {
    /// 序列元素编码见 [`crate::KEY_UP`]：低 15 位虚拟键码、最高位标记释放相位。
    fn inject(&mut self, keys: &[u16]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let recovery = stuck_modifier_recovery(&INJECTED_MODIFIERS);
        if !recovery.is_empty() {
            log::info!("检测到修饰键残留，注入前先复位: {recovery:02X?}");
        }
        let events = assemble_inject_events(&recovery, keys);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 从 UTF-16 列表还原出路径字符串序列（去掉分隔 NUL 与末尾终止 NUL）。
    fn decode(list: &[u16]) -> Vec<String> {
        let body = &list[..list.len() - 1]; // 去掉列表终止 NUL
        body.split(|&unit| unit == 0)
            .filter(|segment| !segment.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    #[test]
    fn hdrop_promotes_relative_paths_to_absolute() {
        // 回归守卫：相对路径曾被原样写入 HDROP，导致微信/千牛静默丢弃粘贴。
        let list = hdrop_path_list(&[PathBuf::from("samples/library/objects/a/raw.jpg")])
            .expect("相对路径应被提升为绝对路径而非报错");
        let decoded = decode(&list);
        assert_eq!(decoded.len(), 1);
        let path = PathBuf::from(&decoded[0]);
        assert!(
            path.is_absolute(),
            "HDROP 中的路径必须是绝对路径，实际为 {}",
            decoded[0]
        );
        assert!(
            decoded[0].ends_with("raw.jpg"),
            "绝对化不得改写文件名，实际为 {}",
            decoded[0]
        );
    }

    #[test]
    fn hdrop_keeps_absolute_paths_and_terminates_list() {
        let list = hdrop_path_list(&[
            PathBuf::from(r"C:\a\one.png"),
            PathBuf::from(r"C:\b\two.mp4"),
        ])
        .expect("绝对路径应直接接受");
        assert_eq!(decode(&list), vec![r"C:\a\one.png", r"C:\b\two.mp4"]);
        // 单路径 NUL + 列表终止 NUL：末两个单元必须都是 0。
        assert_eq!(list[list.len() - 2..], [0, 0]);
    }

    #[test]
    fn hdrop_rejects_empty_path_list() {
        assert!(matches!(
            hdrop_path_list(&[]),
            Err(PlatformError::Clipboard(_))
        ));
    }

    fn event(kind: u32, hwnd: isize, root: isize, pid: u32, object_id: i32) -> PumpEvent {
        PumpEvent {
            event: kind,
            hwnd: WindowHandle(hwnd),
            root: WindowHandle(root),
            process_id: pid,
            object_id,
        }
    }

    /// `EVENT_OBJECT_LOCATIONCHANGE` 是全系统最吵的事件；只有插入符那一路是输入表面证据。
    #[test]
    fn input_surface_matcher_accepts_only_caret_location_changes() {
        let matcher = EventMatcher::InputSurface {
            root: WindowHandle(100),
            process_id: 7,
        };
        assert!(matcher.matches(&event(
            EVENT_OBJECT_LOCATIONCHANGE,
            555,
            100,
            7,
            OBJID_CARET
        )));
        assert!(!matcher.matches(&event(EVENT_OBJECT_LOCATIONCHANGE, 555, 100, 7, 0)));
        // 子控件的焦点事件：GA_ROOT 命中即认领，这正是微信/千牛输入区所在的形状。
        assert!(matcher.matches(&event(EVENT_OBJECT_FOCUS, 555, 100, 7, 0)));
        // 同进程但窗口树不命中也认领：某些版本上输入区的 GA_ROOT 不回到会话窗口。
        assert!(matcher.matches(&event(EVENT_OBJECT_FOCUS, 777, 999, 7, 0)));
        // 与目标无关的进程与窗口一律不认领，避免把别人的焦点当成自己的证据。
        assert!(!matcher.matches(&event(EVENT_OBJECT_FOCUS, 777, 999, 8, 0)));
        assert!(!matcher.matches(&event(EVENT_SYSTEM_FOREGROUND, 555, 100, 7, 0)));
    }

    #[test]
    fn any_in_process_matcher_filters_by_process_only() {
        let matcher = EventMatcher::AnyInProcess { process_id: 7 };
        assert!(matcher.matches(&event(EVENT_OBJECT_FOCUS, 555, 999, 7, 0)));
        assert!(matcher.matches(&event(
            EVENT_OBJECT_LOCATIONCHANGE,
            555,
            999,
            7,
            OBJID_CARET
        )));
        assert!(!matcher.matches(&event(EVENT_OBJECT_FOCUS, 555, 999, 8, 0)));
    }

    #[test]
    fn foreground_matcher_accepts_target_window_or_its_root() {
        let matcher = EventMatcher::Foreground {
            root: WindowHandle(100),
        };
        assert!(matcher.matches(&event(EVENT_SYSTEM_FOREGROUND, 100, 100, 7, 0)));
        assert!(matcher.matches(&event(EVENT_SYSTEM_FOREGROUND, 555, 100, 7, 0)));
        assert!(!matcher.matches(&event(EVENT_SYSTEM_FOREGROUND, 555, 999, 7, 0)));
        assert!(!matcher.matches(&event(EVENT_OBJECT_FOCUS, 100, 100, 7, 0)));
    }

    /// 一条事件必须扇出给所有在场订阅；订阅方走后订阅表要收缩，不能单调增长。
    #[test]
    fn pump_fans_out_to_every_subscriber_and_shrinks_after_drop() {
        let Some(inner) = pump() else {
            // 无窗口会话（如无桌面的 CI）装不上钩子；此处只能跳过，真机由步骤 7 覆盖。
            eprintln!("跳过：本会话装不上 WinEvent 钩子");
            return;
        };

        let (first_id, first) = inner.subscribe();
        let (second_id, second) = inner.subscribe();
        // 订阅表是全局共享的，并行测试的瞬时订阅会让「总数」漂移；
        // 断言只看自己这两个 id 的在场/退场。
        let subscribed = |id: u64| {
            inner
                .subscriptions
                .lock()
                .unwrap()
                .iter()
                .any(|sub| sub.id == id)
        };
        assert!(subscribed(first_id) && subscribed(second_id));

        let probe = event(EVENT_SYSTEM_FOREGROUND, 4242, 4242, 1, 0);
        inner.fan_out(probe);

        // 真实系统事件可能夹在中间，因此按「上限内必定出现这条」来断言。
        let seen = |receiver: &Receiver<PumpEvent>| {
            let deadline = Instant::now() + Duration::from_millis(500);
            while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                match receiver.recv_timeout(remaining) {
                    Ok(got) if got == probe => return true,
                    Ok(_) => continue,
                    Err(_) => return false,
                }
            }
            false
        };
        assert!(seen(&first), "第一个订阅必须收到扇出事件");
        assert!(seen(&second), "第二个订阅同样必须收到同一条事件");

        inner.unsubscribe(first_id);
        inner.unsubscribe(second_id);
        assert!(
            !subscribed(first_id) && !subscribed(second_id),
            "退订必须真的从表里摘除，订阅表不得单调增长"
        );
    }

    /// 等不到目标事件必须是 `CappedOut`（没能证明），且真的等满上限才放弃。
    #[test]
    fn wait_reports_capped_out_instead_of_pretending_success() {
        if pump().is_none() {
            eprintln!("跳过：本会话装不上 WinEvent 钩子");
            return;
        }
        // 一个不存在的窗口句柄：系统永远不会为它发前台事件。
        let mut wait = Win32EventWait::new(EventMatcher::Foreground {
            root: WindowHandle(0x7FFF_FFF0),
        });
        let started = Instant::now();
        assert_eq!(wait.wait(60), WaitOutcome::CappedOut);
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "必须等满上限再放弃，实际 {:?}",
            started.elapsed()
        );
    }

    /// D44 前台归属分类：目标进程优先于自身进程，pid=0（句柄已失效）视作第三方。
    #[test]
    fn classify_foreground_relation_prefers_target_then_own() {
        use ForegroundRelation::{Foreign, OwnProcess, SameAsTarget};
        // 前台与目标同进程（目标内部表面抖动）。
        assert_eq!(classify_foreground_relation(7, 7, 9), SameAsTarget);
        // 前台是自己进程（用户连点素材面板）。
        assert_eq!(classify_foreground_relation(9, 7, 9), OwnProcess);
        // 无关第三方（用户已切走）。
        assert_eq!(classify_foreground_relation(11, 7, 9), Foreign);
        // 前台句柄已失效（pid=0）：保守按第三方处理。
        assert_eq!(classify_foreground_relation(0, 7, 9), Foreign);
        // 目标句柄已失效（pid=0）且前台也是 0：仍按第三方，不误判成目标侧。
        assert_eq!(classify_foreground_relation(0, 0, 9), Foreign);
    }

    /// D40 概率模型的支点之一：只有「未装钩子 && 无活跃泵线程 && 冷却已过」
    /// 才允许重装。任何一条不满足都拒绝，尤其「线程活着」——握手超时的线程
    /// 可能正在装钩子，此刻再 spawn 只会得到双泵双份扇出。
    #[test]
    fn reinstall_requires_uninstalled_idle_and_cooldown_elapsed() {
        const NOW: u64 = 10_000;
        // 已装好：永不重装。
        assert!(!should_reinstall(true, true, NOW - 10_000, NOW));
        assert!(!should_reinstall(true, false, 0, NOW));
        // 未装但线程活着（可能正在装钩子）：不能重装。
        assert!(!should_reinstall(false, true, 0, NOW));
        // 冷却未过：不重装（持续失败场景下退化为限频重试）。
        assert!(!should_reinstall(
            false,
            false,
            NOW - PUMP_REINSTALL_COOLDOWN_MS + 1,
            NOW
        ));
        // 恰好冷却期满：允许重装。
        assert!(should_reinstall(
            false,
            false,
            NOW - PUMP_REINSTALL_COOLDOWN_MS,
            NOW
        ));
        // 从未尝试过（last=0，如首次线程 spawn 失败）且冷却已过：允许。
        assert!(should_reinstall(
            false,
            false,
            0,
            PUMP_REINSTALL_COOLDOWN_MS
        ));
    }

    /// 注入事件组装：修饰键复位必须排在业务序列之前；KEY_UP 位必须翻成
    /// KEYEVENTF_KEYUP。锁不住 SendInput 本体，锁序列语义。
    #[test]
    fn inject_events_prepend_modifier_recovery_before_chord() {
        let chord = [VK_CONTROL, 0x56, 0x56 | KEY_UP, VK_CONTROL | KEY_UP];
        let keys_of = |events: &[INPUT]| -> Vec<(u16, u32)> {
            events
                .iter()
                // 安全：union 字段读取；ki 与 INPUT_KEYBOARD 配套，由构造方保证。
                .map(|event| unsafe { (event.Anonymous.ki.wVk, event.Anonymous.ki.dwFlags) })
                .collect()
        };
        // 无残留：序列原样，相位位翻译正确。
        let plain = assemble_inject_events(&[], &chord);
        assert_eq!(
            keys_of(&plain),
            vec![
                (VK_CONTROL, 0),
                (0x56, 0),
                (0x56, KEYEVENTF_KEYUP),
                (VK_CONTROL, KEYEVENTF_KEYUP),
            ]
        );
        // Ctrl 残留：复位 KEYUP 必须是第一个事件（残留未清就按下 = 和弦错乱）。
        let recovered = assemble_inject_events(&[VK_CONTROL | KEY_UP], &chord);
        assert_eq!(recovered.len(), 5);
        assert_eq!(keys_of(&recovered)[0], (VK_CONTROL, KEYEVENTF_KEYUP));
        assert_eq!(
            keys_of(&recovered)[1..],
            keys_of(&plain),
            "复位事件之后必须完整保留原注入序列"
        );
    }

    /// 观察器订阅必须惰性且只建一次：构造期不订阅（泵可能未就绪），
    /// 泵就绪后首次调用建立，重复调用不得重建（否则订阅表泄漏）。
    #[test]
    fn observer_subscribes_lazily_and_keeps_single_subscription() {
        if pump().is_none() {
            eprintln!("跳过：本会话装不上 WinEvent 钩子");
            return;
        }
        let mut observer = Win32ForegroundObserver::new().expect("观察器构造不应失败");
        assert!(observer.events_rx.is_none(), "构造期不订阅");
        let _ = observer.next_foreground();
        assert!(observer.events_rx.is_some(), "泵就绪后首次调用应建立订阅");
        let id_after_first = observer.events_id;
        let _ = observer.next_foreground();
        assert_eq!(observer.events_id, id_after_first, "重复调用不得重建订阅");
    }

    /// 事件源不可用时 settle 不得补时序等待（`no_timed_waits` 守卫的运行时侧证）：
    /// 该分支的正确出口是泵自愈 + 日志，不是用一次不可靠的睡眠冒充证据。
    struct UnavailableWait;
    impl EventWait for UnavailableWait {
        fn wait(&mut self, _cap_ms: u64) -> WaitOutcome {
            WaitOutcome::Unavailable
        }
    }

    #[test]
    fn settle_returns_immediately_when_event_source_unavailable() {
        let mut wait: Box<dyn EventWait> = Box::new(UnavailableWait);
        let started = Instant::now();
        settle_on_input_surface(&mut wait, 500);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "Unavailable 不得补时序等待，实际 {:?}",
            started.elapsed()
        );
    }
}
