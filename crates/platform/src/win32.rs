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

use windows::core::BSTR;
use windows::Win32::Foundation::HWND as WinHWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationValuePattern, TreeScope_Descendants,
    UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_TextPatternId, UIA_ValuePatternId,
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
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, IsWindowEnabled, SendInput, INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
};
use windows_sys::Win32::UI::Shell::DROPFILES;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetAncestor, GetClassNameW, GetClientRect, GetCursorPos,
    GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetSystemMetrics, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, PostThreadMessageW, SendMessageTimeoutW, SetCursorPos, SetForegroundWindow,
    ShowWindow, TranslateMessage, WindowFromPoint, EVENT_OBJECT_FOCUS,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND, GA_ROOT, MSG, OBJID_CARET,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE,
    WINEVENT_OUTOFCONTEXT, WM_QUIT,
};

use crate::{
    caret_identity_matches, input_point_logical, AnchorGeometry, CaretSemanticIdentity,
    ClickEvidence, ClipboardPayload, ClipboardSink, EventWait, FileDialogs, FocusAttempt,
    FocusOutcome, FocusPlan, FocusReport, FocusStep, FocusWatcher, ForegroundObserver,
    ForegroundRelation, InputFocuser, KeyInjector, PlatformError, ReadinessBlocker,
    ReadinessProbe, ReadinessSignal, Result, WaitOutcome, WindowActivator, WindowEnumerator,
    WindowEventSource, WindowHandle, WindowRect, WindowSnapshot, KEY_UP,
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
    COM_READY.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
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
                        | windows::Win32::UI::Shell::FOS_FORCEFILESYSTEM,
                )
                .map_err(|error| PlatformError::Window(format!("配置文件夹对话框失败: {error}")))?;
            dialog
                .SetTitle(&windows::core::HSTRING::from(title))
                .map_err(|error| PlatformError::Window(format!("设置对话框标题失败: {error}")))?;
            match dialog.Show(None) {
                Ok(()) => {
                    let item = dialog.GetResult().map_err(|error| {
                        PlatformError::Window(format!("读取选择结果失败: {error}"))
                    })?;
                    let name = item
                        .GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)
                        .map_err(|error| {
                            PlatformError::Window(format!("读取选择路径失败: {error}"))
                        })?;
                    let path = name.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(name.as_ptr() as *const core::ffi::c_void));
                    Ok(Some(PathBuf::from(path)))
                }
                Err(error) if is_cancelled(&error) => Ok(None),
                Err(error) => Err(PlatformError::Window(format!(
                    "文件夹选择对话框失败: {error}"
                ))),
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
                        | windows::Win32::UI::Shell::FOS_FILEMUSTEXIST,
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
                    .map(
                        |(name, spec)| windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC {
                            pszName: windows::core::PCWSTR(name.as_ptr()),
                            pszSpec: windows::core::PCWSTR(spec.as_ptr()),
                        },
                    )
                    .collect();
                dialog.SetFileTypes(&specs).map_err(|error| {
                    PlatformError::Window(format!("设置文件类型过滤失败: {error}"))
                })?;
            }
            match dialog.Show(None) {
                Ok(()) => {
                    let item = dialog.GetResult().map_err(|error| {
                        PlatformError::Window(format!("读取选择结果失败: {error}"))
                    })?;
                    let name = item
                        .GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)
                        .map_err(|error| {
                            PlatformError::Window(format!("读取选择路径失败: {error}"))
                        })?;
                    let path = name.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(name.as_ptr() as *const core::ffi::c_void));
                    Ok(Some(PathBuf::from(path)))
                }
                Err(error) if is_cancelled(&error) => Ok(None),
                Err(error) => Err(PlatformError::Window(format!(
                    "文件选择对话框失败: {error}"
                ))),
            }
        }
    }

    /// 多选打开（D49）：FOS_ALLOWMULTISELECT + GetResults（IShellItemArray）。
    /// 结果顺序保持对话框选择顺序。
    fn pick_open_files(&self, title: &str, filter: &str) -> crate::Result<Option<Vec<PathBuf>>> {
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
                        | windows::Win32::UI::Shell::FOS_ALLOWMULTISELECT,
                )
                .map_err(|error| PlatformError::Window(format!("配置文件对话框失败: {error}")))?;
            dialog
                .SetTitle(&windows::core::HSTRING::from(title))
                .map_err(|error| PlatformError::Window(format!("设置对话框标题失败: {error}")))?;
            let kept = parse_filter_pairs(filter);
            if !kept.is_empty() {
                let specs: Vec<windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC> = kept
                    .iter()
                    .map(
                        |(name, spec)| windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC {
                            pszName: windows::core::PCWSTR(name.as_ptr()),
                            pszSpec: windows::core::PCWSTR(spec.as_ptr()),
                        },
                    )
                    .collect();
                dialog.SetFileTypes(&specs).map_err(|error| {
                    PlatformError::Window(format!("设置文件类型过滤失败: {error}"))
                })?;
            }
            match dialog.Show(None) {
                Ok(()) => {
                    let results = dialog.GetResults().map_err(|error| {
                        PlatformError::Window(format!("读取多选结果失败: {error}"))
                    })?;
                    let count = results.GetCount().map_err(|error| {
                        PlatformError::Window(format!("读取多选数量失败: {error}"))
                    })?;
                    let mut paths = Vec::with_capacity(count as usize);
                    for index in 0..count {
                        let item = results.GetItemAt(index).map_err(|error| {
                            PlatformError::Window(format!("读取第 {index} 项失败: {error}"))
                        })?;
                        let name = item
                            .GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)
                            .map_err(|error| {
                                PlatformError::Window(format!("读取选择路径失败: {error}"))
                            })?;
                        paths.push(PathBuf::from(name.to_string().unwrap_or_default()));
                        CoTaskMemFree(Some(name.as_ptr() as *const core::ffi::c_void));
                    }
                    Ok(Some(paths))
                }
                Err(error) if is_cancelled(&error) => Ok(None),
                Err(error) => Err(PlatformError::Window(format!(
                    "文件选择对话框失败: {error}"
                ))),
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
                        | windows::Win32::UI::Shell::FOS_FORCEFILESYSTEM,
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
                    let item = dialog.GetResult().map_err(|error| {
                        PlatformError::Window(format!("读取保存结果失败: {error}"))
                    })?;
                    let name = item
                        .GetDisplayName(windows::Win32::UI::Shell::SIGDN_FILESYSPATH)
                        .map_err(|error| {
                            PlatformError::Window(format!("读取保存路径失败: {error}"))
                        })?;
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
                    };
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
                    };
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
    fn focus_input(&self, window: WindowHandle, plan: &FocusPlan) -> FocusReport {
        let report = focus_input_by_plan(window, plan);
        // 低配机延迟归因（D41）与「报成功但没落框」现场（D74）：实际走了哪一级、
        // 点击点与客户区几何、settle 证据，配合 pipeline 的 focus 段耗时判读。
        log::debug!(
            "focus_input outcome={:?} steps={:?} attempts={}",
            report.outcome,
            plan.steps,
            format_attempts(&report.attempts)
        );
        report
    }
}

/// 尝试记录的紧凑串（供 debug 日志与 pipeline 现场行共用）。
fn format_attempts(attempts: &[FocusAttempt]) -> String {
    attempts
        .iter()
        .map(|attempt| {
            let click = attempt
                .click
                .map(|evidence| {
                    format!(
                        " click={:?} point=({},{}) client={}x{} dpi={}",
                        evidence.geometry,
                        evidence.point_screen.0,
                        evidence.point_screen.1,
                        evidence.client_size.0,
                        evidence.client_size.1,
                        evidence.dpi
                    )
                })
                .unwrap_or_default();
            let settle = attempt
                .settle
                .map(|outcome| format!(" settle={outcome:?}"))
                .unwrap_or_default();
            format!("{:?}->{:?}{click}{settle}", attempt.step, attempt.outcome)
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 原生 caret + MSAA 身份确认（`FocusStep::CaretSemantic` 的机制实现）。
///
/// 身份谓词来自画像（[`CaretSemanticIdentity`]，缺省=千牛校准值），平台层不做
/// per-app 判断：读 caret 屏幕点 → `AccessibleObjectFromPoint` → 与声明比对。
fn native_caret_semantic(hwnd: HWND, identity: &CaretSemanticIdentity) -> bool {
    unsafe {
        if GetForegroundWindow() != hwnd || GetAncestor(hwnd, GA_ROOT) != hwnd {
            return false;
        }
        let mut info = windows_sys::Win32::UI::WindowsAndMessaging::GUITHREADINFO {
            cbSize: std::mem::size_of::<windows_sys::Win32::UI::WindowsAndMessaging::GUITHREADINFO>(
            ) as u32,
            ..std::mem::zeroed()
        };
        if GetGUIThreadInfo(
            GetWindowThreadProcessId(hwnd, std::ptr::null_mut()),
            &mut info,
        ) == 0
            || info.hwndCaret.is_null()
        {
            return false;
        }
        let mut p = POINT {
            x: info.rcCaret.left,
            y: info.rcCaret.top + (info.rcCaret.bottom - info.rcCaret.top) / 2,
        };
        if ClientToScreen(info.hwndCaret, &mut p) == 0 {
            return false;
        }
        let owner = WindowFromPoint(p);
        if owner.is_null() || GetAncestor(owner, GA_ROOT) != hwnd {
            return false;
        }
        let mut acc = None;
        let mut child = windows::Win32::System::Variant::VARIANT::default();
        let wp = windows::Win32::Foundation::POINT { x: p.x, y: p.y };
        if windows::Win32::UI::Accessibility::AccessibleObjectFromPoint(wp, &mut acc, &mut child)
            .is_err()
        {
            return false;
        }
        let Some(accessible) = acc else {
            return false;
        };
        let role = accessible
            .get_accRole(&child)
            .ok()
            .filter(|v| v.Anonymous.Anonymous.vt == windows::Win32::System::Variant::VARENUM(3))
            .map(|v| v.Anonymous.Anonymous.Anonymous.lVal);
        let name = accessible
            .get_accName(&child)
            .ok()
            .map(|v: BSTR| v.to_string());
        caret_identity_matches(identity, role, name.as_deref())
    }
}

fn focus_input_by_plan(window: WindowHandle, plan: &FocusPlan) -> FocusReport {
    let hwnd = window.0 as HWND;
    let mut attempts = Vec::new();
    if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
        return FocusReport {
            outcome: FocusOutcome::Unavailable,
            attempts,
        };
    }
    // 空计划由 targets 层拦下（ProfileError::EmptyFocusStrategy）；平台层遇空
    // 保持纯函数性，什么都不做并如实返回「没能证明」。
    for step in &plan.steps {
        match step {
            FocusStep::AlreadyEditable => {
                let editable = uia_focused_is_editable(window);
                attempts.push(FocusAttempt {
                    step: *step,
                    outcome: if editable {
                        FocusOutcome::AlreadyEditable
                    } else {
                        FocusOutcome::Unavailable
                    },
                    click: None,
                    settle: None,
                });
                if editable {
                    return FocusReport {
                        outcome: FocusOutcome::AlreadyEditable,
                        attempts,
                    };
                }
            }
            FocusStep::CaretSemantic => {
                let identity = plan.caret_identity.clone().unwrap_or_default();
                let ok = native_caret_semantic(hwnd, &identity);
                attempts.push(FocusAttempt {
                    step: *step,
                    outcome: if ok {
                        FocusOutcome::FocusedByCaretSemantic
                    } else {
                        FocusOutcome::Unavailable
                    },
                    click: None,
                    settle: None,
                });
                if ok {
                    return FocusReport {
                        outcome: FocusOutcome::FocusedByCaretSemantic,
                        attempts,
                    };
                }
            }
            FocusStep::UiaSetFocus => {
                let (focused, settle) = uia_set_focus_on_editable(window);
                attempts.push(FocusAttempt {
                    step: *step,
                    outcome: if focused {
                        FocusOutcome::FocusedByUia
                    } else {
                        FocusOutcome::Unavailable
                    },
                    click: None,
                    settle,
                });
                if focused {
                    return FocusReport {
                        outcome: FocusOutcome::FocusedByUia,
                        attempts,
                    };
                }
            }
            FocusStep::InputPointClick => {
                // 表达式点击点（2026-09-05 真机通路）：先按实时 DPI 求值逻辑点，
                // 再复用锚点单击的全套守卫与证据链。
                let Some(expr) = plan.input_point_expr.as_ref() else {
                    continue;
                };
                let dpi = window_dpi(hwnd);
                let scale = f64::from(dpi.max(1)) / 96.0;
                let mut client = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
                    attempts.push(FocusAttempt {
                        step: *step,
                        outcome: FocusOutcome::Unavailable,
                        click: None,
                        settle: None,
                    });
                    continue;
                }
                let logical = (
                    ((client.right - client.left) as f64 / scale).round() as i32,
                    ((client.bottom - client.top) as f64 / scale).round() as i32,
                );
                let Ok(evaluated) = input_point_logical(expr, logical) else {
                    // 求值失败（坏表达式）只在加载期校验过；此处按不可用降级，
                    // 不 panic 不猜测——后续级别（如 anchor）仍可接管。
                    attempts.push(FocusAttempt {
                        step: *step,
                        outcome: FocusOutcome::Unavailable,
                        click: None,
                        settle: None,
                    });
                    continue;
                };
                let geometry = AnchorGeometry::ExprPoint {
                    x_logical: evaluated.0,
                    y_logical: evaluated.1,
                };
                let (outcome, click, settle) = click_anchor(hwnd, geometry);
                attempts.push(FocusAttempt {
                    step: *step,
                    outcome,
                    click,
                    settle,
                });
                if outcome == FocusOutcome::FocusedByAnchor {
                    return FocusReport {
                        outcome: FocusOutcome::FocusedByAnchor,
                        attempts,
                    };
                }
            }
            FocusStep::AnchorClick => {
                // bottom-up 锚点（D74）存在即优先：旧比例锚点只作兼容兜底。
                let geometry = plan
                    .anchor_bottom
                    .map(AnchorGeometry::BottomUp)
                    .or(plan.anchor.map(AnchorGeometry::Ratio));
                if let Some(geometry) = geometry {
                    let (outcome, click, settle) = click_anchor(hwnd, geometry);
                    attempts.push(FocusAttempt {
                        step: *step,
                        outcome,
                        click,
                        settle,
                    });
                    if outcome == FocusOutcome::FocusedByAnchor {
                        return FocusReport {
                            outcome: FocusOutcome::FocusedByAnchor,
                            attempts,
                        };
                    }
                }
            }
        }
    }
    FocusReport {
        outcome: FocusOutcome::Unavailable,
        attempts,
    }
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
/// 返回值附带最后一次 SetFocus 后的事件等待结局（D74 现场记录）。
fn uia_set_focus_on_editable(window: WindowHandle) -> (bool, Option<WaitOutcome>) {
    let Ok(automation) = uia_automation() else {
        return (false, None);
    };
    let Ok(root) =
        (unsafe { automation.ElementFromHandle(WinHWND(window.0 as *mut core::ffi::c_void)) })
    else {
        return (false, None);
    };
    let Ok(condition) = (unsafe { automation.CreateTrueCondition() }) else {
        return (false, None);
    };
    let Ok(all) = (unsafe { root.FindAll(TreeScope_Descendants, &condition) }) else {
        return (false, None);
    };
    let Ok(length) = (unsafe { all.Length() }) else {
        return (false, None);
    };
    let mut last_settle = None;
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
        last_settle = Some(outcome);
        if uia_focused_is_editable(window) {
            return (true, last_settle);
        }
    }
    (false, last_settle)
}

/// 客户区比例锚点 → 客户区内像素偏移。比例先夹紧到安全区间再换算。
/// 锚点几何 → 客户区内点击点（物理像素）。
///
/// bottom-up（D74）：y = 客户区高 − y_from_bottom × (dpi/96)——用目标窗口的
/// 实时 DPI 把 96-DPI 逻辑像素换算成物理像素，锚定的是「距底边」而不是
/// 「距顶边比例」，窗口变高时不再从输入区漂进消息列表。y 夹进客户区内，
/// 画像写了离谱值也点不出窗口。
fn click_point_in_client(
    client_width: i32,
    client_height: i32,
    dpi: u32,
    geometry: AnchorGeometry,
) -> (i32, i32) {
    let clamp = |ratio: f32| ratio.clamp(ANCHOR_RATIO_MIN, ANCHOR_RATIO_MAX);
    match geometry {
        AnchorGeometry::Ratio(anchor) => (
            (client_width as f32 * clamp(anchor.x_ratio)).round() as i32,
            (client_height as f32 * clamp(anchor.y_ratio)).round() as i32,
        ),
        AnchorGeometry::BottomUp(anchor) => {
            let scale = dpi.max(1) as f32 / 96.0;
            let x = (client_width as f32 * clamp(anchor.x_ratio)).round() as i32;
            let offset = (anchor.y_from_bottom * scale).round() as i32;
            let y = (client_height - offset).clamp(8, (client_height - 8).max(8));
            (x, y)
        }
        AnchorGeometry::ExprPoint { x_logical, y_logical } => {
            // 求值已在计划级完成（逻辑像素），这里只按实时 DPI 放大并夹进客户区。
            // 允许贴边（角落内缩配方可能就是贴边点），只防越界点击到窗外。
            let scale = dpi.max(1) as f32 / 96.0;
            let x = ((x_logical as f32 * scale).round() as i32).clamp(0, (client_width - 1).max(0));
            let y =
                ((y_logical as f32 * scale).round() as i32).clamp(0, (client_height - 1).max(0));
            (x, y)
        }
    }
}

/// 目标窗口的实时 DPI；API 失败（老系统/无效句柄）回落 96（=不缩放）。
fn window_dpi(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        96
    } else {
        dpi
    }
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

/// `WM_NCHITTEST` 命中码：客户区。
const HTCLIENT: i32 = 1;

/// 跨进程询问窗口「这个屏幕点会被命中测试归到哪」。返回 None = 查询失败
/// （目标挂起/超时），调用方按「不点击」处理——宁可放弃也不猜。
fn window_hit_test(hwnd: HWND, point: POINT) -> Option<i32> {
    const WM_NCHITTEST: u32 = 0x0084;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;
    let packed = ((point.y as u16 as u32) << 16) | point.x as u16 as u32;
    let result = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NCHITTEST,
            0,
            packed as isize,
            SMTO_ABORTIFHUNG,
            300,
            std::ptr::null_mut(),
        )
    };
    (result != 0).then_some(result as i32)
}

/// 对画像声明的锚点做一次左键单击。
///
/// 红线：只合成鼠标移动与左键按下/释放，绝不合成任何键盘事件；
/// 点击前必须确认目标仍是前台、且锚点处的顶层窗口就是目标窗口（防止点到遮挡物）。
///
/// 返回三元组（D74）：聚焦结论 + 单击现场（几何/落点/客户区/DPI，守卫拦截时也
/// 尽量给）+ 点击后输入迹象的等待结局。旧实现把后两者丢弃，「点没点中输入框」
/// 在上层无从判读。
fn click_anchor(
    hwnd: HWND,
    geometry: AnchorGeometry,
) -> (FocusOutcome, Option<ClickEvidence>, Option<WaitOutcome>) {
    if unsafe { GetForegroundWindow() } != hwnd {
        return (FocusOutcome::Unavailable, None, None);
    }
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
        return (FocusOutcome::Unavailable, None, None);
    }
    let dpi = window_dpi(hwnd);
    let client_size = (client.right - client.left, client.bottom - client.top);
    let (offset_x, offset_y) = click_point_in_client(client_size.0, client_size.1, dpi, geometry);
    let mut point = POINT {
        x: client.left + offset_x,
        y: client.top + offset_y,
    };
    if unsafe { ClientToScreen(hwnd, &mut point) } == 0 {
        return (FocusOutcome::Unavailable, None, None);
    }

    // 内缩点可能落在非客户区命中区：真机实证（2026-09-05，千牛窗口角落），
    // WM_NCHITTEST 返回 HTBOTTOMLEFT 一类时点击被系统当尺寸调整吞掉、光标变
    // resize，客户区点击从未发生——而 SendMessage 跨进程查询与真实光标命中
    // 一致。命中非 HTCLIENT 时沿「指向窗口中心」方向内缩重试（8 物理像素/步，
    // 至多 5 步）；全程失败则放弃本级别：宁可不落框，也不点未确认为客户区的位置。
    let center = (
        client.left + client_size.0 / 2,
        client.top + client_size.1 / 2,
    );
    let mut hit_client = false;
    for _ in 0..=5 {
        match window_hit_test(hwnd, point) {
            Some(HTCLIENT) => {
                hit_client = true;
                break;
            }
            _ => {
                let step_x = (center.0 - point.x).signum() * 8;
                let step_y = (center.1 - point.y).signum() * 8;
                let (next_x, next_y) = (point.x + step_x, point.y + step_y);
                if (next_x, next_y) == (point.x, point.y) {
                    break;
                }
                point = POINT {
                    x: next_x,
                    y: next_y,
                };
            }
        }
    }
    if !hit_client {
        return (FocusOutcome::Unavailable, None, None);
    }

    let evidence = ClickEvidence {
        geometry,
        point_screen: (point.x, point.y),
        client_size,
        dpi,
    };

    // 锚点必须真的落在目标窗口上：命中的顶层窗口不是目标就说明被遮挡，放弃点击。
    let hit = unsafe { WindowFromPoint(point) };
    if hit.is_null() || unsafe { GetAncestor(hit, GA_ROOT) } != hwnd {
        return (FocusOutcome::Unavailable, Some(evidence), None);
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
        return (FocusOutcome::Unavailable, Some(evidence), None);
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
        return (FocusOutcome::Unavailable, Some(evidence), None);
    }
    let outcome = surface.wait(ANCHOR_CLICK_SETTLE_CAP_MS);
    // 单击可能唤起了别的窗口（例如目标弹出模态框）；前台漂移即视为没拿到焦点。
    if unsafe { GetForegroundWindow() } != hwnd {
        return (FocusOutcome::Unavailable, Some(evidence), Some(outcome));
    }
    (FocusOutcome::FocusedByAnchor, Some(evidence), Some(outcome))
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
    use crate::{BottomUpAnchor, FocusAnchor};

    /// 从 UTF-16 列表还原出路径字符串序列（去掉分隔 NUL 与末尾终止 NUL）。
    fn decode(list: &[u16]) -> Vec<String> {
        let body = &list[..list.len() - 1]; // 去掉列表终止 NUL
        body.split(|&unit| unit == 0)
            .filter(|segment| !segment.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }

    #[test]
    fn bottom_up_anchor_scales_with_dpi_and_ignores_window_height() {
        // 千牛接待中心实测值：1230x800 逻辑客户区、144 DPI（150%）。
        // 偏移先取整：round(127×1.5)=191，y = 800−191 = 609，即物理底边向上
        // 190.5px，落在实测文本带（物理 80~295）内；窗口变高时此距离不变。
        let point = click_point_in_client(
            1230,
            800,
            144,
            AnchorGeometry::BottomUp(BottomUpAnchor {
                x_ratio: 0.394,
                y_from_bottom: 127.0,
            }),
        );
        assert_eq!(point, (485, 609));

        // 同一锚点、同 DPI、窗口高 1000 逻辑：y 只随客户区底边平移，
        // 不像比例锚那样把点击漂进消息列表（D74 失效机理）。
        let tall = click_point_in_client(
            1230,
            1000,
            144,
            AnchorGeometry::BottomUp(BottomUpAnchor {
                x_ratio: 0.394,
                y_from_bottom: 127.0,
            }),
        );
        assert_eq!(tall, (485, 809));
    }

    #[test]
    fn bottom_up_anchor_clamps_inside_client_at_low_dpi() {
        // 96 DPI 不缩放；离谱的 y_from_bottom 被夹进客户区内（8px 边距），
        // 画像写错值也点不出窗口。
        let point = click_point_in_client(
            1000,
            300,
            96,
            AnchorGeometry::BottomUp(BottomUpAnchor {
                x_ratio: 0.5,
                y_from_bottom: 900.0,
            }),
        );
        assert_eq!(point, (500, 8));
    }

    #[test]
    fn ratio_anchor_unchanged_for_legacy_profiles() {
        // 旧比例锚路径保持原行为：未声明 anchor_bottom 的画像走这里。
        let point = click_point_in_client(
            1000,
            800,
            144,
            AnchorGeometry::Ratio(FocusAnchor {
                x_ratio: 0.5,
                y_ratio: 0.5,
            }),
        );
        assert_eq!(point, (500, 400));
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

    /// 表达式点击点的求值面：四则/变量/括号/一元负号可用，垃圾一律拒绝。
    #[test]
    fn point_expr_supports_arithmetic_variables_and_rejects_garbage() {        assert_eq!(crate::eval_point_expr("8", 1920, 1080), Ok(8));
        assert_eq!(
            crate::eval_point_expr("WINDOW_WIDTH - 8", 1920, 1080),
            Ok(1912)
        );
        assert_eq!(
            crate::eval_point_expr("WINDOW_HEIGHT - 8", 1920, 1080),
            Ok(1072)
        );
        assert_eq!(crate::eval_point_expr("WINDOW_WIDTH / 2", 1920, 1080), Ok(960));
        assert_eq!(
            crate::eval_point_expr("(WINDOW_WIDTH - 16) / 2", 1920, 1080),
            Ok(952)
        );
        assert_eq!(
            crate::eval_point_expr("-8 + WINDOW_WIDTH", 1920, 1080),
            Ok(1912)
        );
        assert!(crate::eval_point_expr("WINDOW_DEPTH", 1920, 1080).is_err());
        assert!(crate::eval_point_expr("WINDOW_WIDTH / 0", 1920, 1080).is_err());
        assert!(crate::eval_point_expr("1 +", 1920, 1080).is_err());
        assert!(crate::eval_point_expr("(1", 1920, 1080).is_err());
        assert!(crate::eval_point_expr("1 2", 1920, 1080).is_err());
        assert!(crate::eval_point_expr("", 1920, 1080).is_err());
    }

    /// ExprPoint 几何：逻辑像素按实时 DPI 放大、越界夹进客户区（允许贴边）。
    #[test]
    fn expr_point_scales_logical_by_dpi_and_clamps_into_client() {
        let geometry = AnchorGeometry::ExprPoint {
            x_logical: 100,
            y_logical: 50,
        };
        assert_eq!(click_point_in_client(1000, 800, 144, geometry), (150, 75));
        assert_eq!(click_point_in_client(1000, 800, 96, geometry), (100, 50));
        let corner = AnchorGeometry::ExprPoint {
            x_logical: 0,
            y_logical: 0,
        };
        assert_eq!(click_point_in_client(1000, 800, 288, corner), (0, 0));
        let overflow = AnchorGeometry::ExprPoint {
            x_logical: 99_999,
            y_logical: 99_999,
        };
        assert_eq!(click_point_in_client(1000, 800, 96, overflow), (999, 799));
    }

    /// caret 身份谓词：缺省=千牛校准值（role 7/编辑）；画像自定义后按声明比对。
    #[test]
    fn caret_identity_matches_declared_role_and_name() {
        let default_identity = CaretSemanticIdentity::default();
        assert!(caret_identity_matches(
            &default_identity,
            Some(7),
            Some("编辑")
        ));
        assert!(!caret_identity_matches(
            &default_identity,
            Some(7),
            Some("买家账号")
        ));
        assert!(!caret_identity_matches(&default_identity, Some(10), Some("编辑")));
        assert!(!caret_identity_matches(&default_identity, None, Some("编辑")));
        assert!(!caret_identity_matches(&default_identity, Some(7), None));

        let custom = CaretSemanticIdentity {
            role: 42,
            name: "chat input".to_string(),
        };
        assert!(caret_identity_matches(&custom, Some(42), Some("chat input")));
        assert!(!caret_identity_matches(&custom, Some(7), Some("编辑")));
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

// ---------------------------------------------------------------------------
// D49 拖入兜底：IDropTarget（RegisterDragDrop）。spike S1 结论：Slint 1.17
// 不支持 OS 文件拖入（DropArea 仅应用内 DnD、winit 后端零处理、无注册冲突），
// 故经 OLE 注册自主 IDropTarget。回调在注册线程（UI 消息泵）派发。
// ---------------------------------------------------------------------------
pub mod dragdrop {
    use std::sync::{Arc, OnceLock};
    use windows::core::implement;
    use windows::Win32::Foundation::{HGLOBAL, HWND, POINTL};
    use windows::Win32::System::Com::{
        IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL,
    };
    use windows::Win32::System::Ole::{
        IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, ReleaseStgMedium,
        RevokeDragDrop, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY,
    };
    use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    use crate::{FileDropSink, PlatformError};

    /// OLE 初始化（RegisterDragDrop 前置；STA 上幂等）。失败则拖拽不可用，
    /// 由调用方降级（导入三入口仍在）。
    fn ensure_ole_initialized() -> Result<(), PlatformError> {
        static OLE_READY: OnceLock<Result<(), String>> = OnceLock::new();
        let ready = OLE_READY.get_or_init(|| unsafe {
            // 线程公寓在本进程已是 STA（CoInitializeEx），OleInitialize 返回
            // S_FALSE 表示已装；RPC_E_CHANGED_MODE 表示公寓冲突，拖拽放弃。
            match OleInitialize(None) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == windows::core::HRESULT(0x0001_0101u32 as i32) => Ok(()), // S_FALSE（已初始化）
                Err(e) => Err(format!("OleInitialize 失败: {e}")),
            }
        });
        ready.clone().map_err(PlatformError::Window).and(Ok(()))
    }

    #[implement(IDropTarget)]
    struct FileDropTarget {
        sink: Arc<dyn FileDropSink>,
    }

    impl IDropTarget_Impl for FileDropTarget_Impl {
        fn DragEnter(
            &self,
            _pdataobj: windows::core::Ref<'_, IDataObject>,
            _grfkeystate: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            pdweffect: *mut DROPEFFECT,
        ) -> windows::core::Result<()> {
            // 仅接受复制语义；不接受时 effect 置 0，OS 自行显示禁止光标。
            unsafe { *pdweffect = DROPEFFECT_COPY };
            Ok(())
        }

        fn DragOver(
            &self,
            _grfkeystate: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            pdweffect: *mut DROPEFFECT,
        ) -> windows::core::Result<()> {
            unsafe { *pdweffect = DROPEFFECT_COPY };
            Ok(())
        }

        fn DragLeave(&self) -> windows::core::Result<()> {
            Ok(())
        }

        fn Drop(
            &self,
            pdataobj: windows::core::Ref<'_, IDataObject>,
            _grfkeystate: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            pdweffect: *mut DROPEFFECT,
        ) -> windows::core::Result<()> {
            unsafe { *pdweffect = DROPEFFECT_COPY };
            let Ok(data) = pdataobj.ok() else {
                return Ok(());
            };
            let format = FORMATETC {
                cfFormat: CF_HDROP.0,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            let Ok(mut medium) = (unsafe { data.GetData(&format) }) else {
                // 非 CF_HDROP 拖入（纯文本等）：不处理，OS 按不接受呈现。
                return Ok(());
            };
            let paths = unsafe { extract_hdrop_paths(&medium) };
            unsafe { ReleaseStgMedium(&mut medium) };
            if !paths.is_empty() {
                self.sink.files_dropped(paths);
            }
            Ok(())
        }
    }

    /// HDROP → 路径列表（DragQueryFileW 计数 + 逐条取名）。
    unsafe fn extract_hdrop_paths(medium: &STGMEDIUM) -> Vec<std::path::PathBuf> {
        let hglobal: HGLOBAL = medium.u.hGlobal;
        if hglobal.0.is_null() {
            return Vec::new();
        }
        let hdrop = HDROP(hglobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None) as usize;
        let mut paths = Vec::with_capacity(count);
        for index in 0..count as u32 {
            let len = DragQueryFileW(hdrop, index, None) as usize;
            if len == 0 {
                continue;
            }
            // 首次调用返回的长度不含结尾 NUL；缓冲区必须留出 NUL 位
            // （len+1），否则 API 截断成 len-1 个字符——末字符被吃掉，
            // "walrus.jpg" 变 "walrus.jp"，下游按扩展名过滤全部落空。
            let mut buffer = vec![0u16; len + 1];
            let written = DragQueryFileW(hdrop, index, Some(&mut buffer)) as usize;
            if written == 0 {
                continue;
            }
            let text = String::from_utf16_lossy(&buffer[..written]);
            if !text.is_empty() {
                paths.push(std::path::PathBuf::from(text));
            }
        }
        paths
    }

    /// 在主窗口 HWND 上注册拖入接收。重复注册会失败（先 Revoke 再注册）。
    pub fn register_file_drop(
        hwnd: isize,
        sink: Arc<dyn FileDropSink>,
    ) -> Result<(), PlatformError> {
        if hwnd == 0 {
            return Err(PlatformError::Window("窗口句柄为空，拖拽导入不可用".into()));
        }
        ensure_ole_initialized()?;
        let target: IDropTarget = FileDropTarget { sink }.into();
        let hwnd = HWND(hwnd as *mut core::ffi::c_void);
        unsafe {
            // 同一 HWND 重复注册返回 E_FAIL；先撤销再装，幂等。
            let _ = RevokeDragDrop(hwnd);
            RegisterDragDrop(hwnd, &target)
                .map_err(|error| PlatformError::Window(format!("注册拖入目标失败: {error}")))
        }
    }
}

/// 恢复重绘守卫：Windows 上 winit 不派发 Occluded 事件，glutin 对 WGL 表面
/// 的 resize 是 no-op（"not supported with WGL"）——最小化→唤出后 GPU 交换链
/// 缓冲内容未定义（黑），客户端区能否恢复全靠系统补发 WM_PAINT（winit 以它
/// 驱动 RedrawRequested，femtovg 由此重铺整帧）。
///
/// 软件渲染档（winit+softbuffer）的黑屏是另一条失效链（2026-08-30 真机复发，
/// 源码级定位）：① slint 收到最小化的 Resized(0×0) 直接丢弃（winitwindowadapter
/// resize_event 过滤零尺寸，防渲染器炸）——恢复时场景零变化；② sw 渲染器的
/// `render()` 每帧用 softbuffer 的 buffer age 选重绘档，Windows 上 age 恒为 1
/// ⇒ `ReusedBuffer` 部分重绘，把 `occluded(true)` 设的全量档直接覆写；③ 部分
/// 重绘算出空脏区 ⇒ softbuffer 一像素不上屏，但 `present_with_damage` 收尾的
/// `ValidateRect(整窗)` 让系统从此不再补发 WM_PAINT；④ DWM 在最小化/完全遮挡
/// 期间可能丢弃窗口重定表面 ⇒ 黑区定格，直到鼠标悬浮把悬浮件区域标脏。
///
/// 两链的公共兜底：子类化主窗口 proc，钩 WM_PAINT——更新区几乎覆盖整个客户区
/// （≥90%，= 系统判定表面内容不可信）时调用应用层回调把整窗标脏，强制一帧
/// 全量重绘。「最小化→非最小化」转换仍补发 RDW_INTERNALPAINT（femtovg 档靠它
/// 触发重绘；系统侧合并去重，不产生重绘风暴）。
pub mod paint_guard {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{GetUpdateRect, RedrawWindow, RDW_INTERNALPAINT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, GetClientRect, SetWindowLongPtrW, GWLP_WNDPROC,
        SIZE_MINIMIZED, WM_NCDESTROY, WM_PAINT, WM_SIZE, WNDPROC,
    };

    use crate::PlatformError;

    static ORIGINAL_PROC: AtomicIsize = AtomicIsize::new(0);
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    static WAS_MINIMIZED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        /// 整窗标脏回调（应用层翻转 repaint-nudge）。WM_PAINT 投递在窗口所属
        /// 线程 = Slint UI 线程（与 window_ready 模块同理），thread_local 存
        /// 非 Send 闭包即可。
        static ON_FULL_INVALIDATE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn guard_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_SIZE => {
                let minimized = wparam == SIZE_MINIMIZED as usize;
                let was = WAS_MINIMIZED.swap(minimized, Ordering::Relaxed);
                if was && !minimized {
                    // 恢复帧：补发内部绘制。winit 的 WM_PAINT 分支把它转成
                    // RedrawRequested；软件档的整窗标脏由下面 WM_PAINT 臂的
                    // 更新区判定接管。
                    unsafe {
                        RedrawWindow(
                            hwnd,
                            std::ptr::null(),
                            std::ptr::null_mut(),
                            RDW_INTERNALPAINT,
                        )
                    };
                }
            }
            WM_PAINT => {
                // 在转发给 winit（其 BeginPaint/ValidateRect 会清掉更新区）之前
                // 先看一眼：整窗失效 ⇒ 表面内容已不可信，部分重绘必漏画。
                if unsafe { full_client_invalidate(hwnd) } {
                    ON_FULL_INVALIDATE.with(|slot| {
                        if let Some(callback) = slot.borrow().as_ref() {
                            callback();
                        }
                    });
                }
            }
            WM_NCDESTROY => {
                // 还原子类化，避免销毁路径上 proc 悬垂；顺带释放回调。
                let original = ORIGINAL_PROC.load(Ordering::Relaxed);
                if original != 0 {
                    unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, original) };
                }
                ON_FULL_INVALIDATE.with(|slot| *slot.borrow_mut() = None);
            }
            _ => {}
        }
        let original = ORIGINAL_PROC.load(Ordering::Relaxed);
        if original == 0 {
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        } else {
            let proc: WNDPROC = std::mem::transmute(original);
            unsafe { CallWindowProcW(proc, hwnd, msg, wparam, lparam) }
        }
    }

    /// 当前更新区是否几乎覆盖整个客户区（≥90%）。在 winit 校验窗口之前调用，
    /// 更新区仍完好。最小化恢复/完全遮挡重现时 DWM 丢弃重定表面，系统以整窗
    /// 失效请求重画；拖拽改尺寸等局部失效（缓冲仍可信）不触发整窗标脏。
    unsafe fn full_client_invalidate(hwnd: HWND) -> bool {
        let mut update = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if unsafe { GetUpdateRect(hwnd, &mut update, 0) } == 0 {
            // 无更新区（RDW_INTERNALPAINT 之类的内部绘制请求）：交给常规重绘。
            return false;
        }
        let mut client = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
            return false;
        }
        let client_w = (client.right - client.left) as i64;
        let client_h = (client.bottom - client.top) as i64;
        if client_w <= 0 || client_h <= 0 {
            return false;
        }
        let update_w = (update.right - update.left).clamp(0, client_w as i32) as i64;
        let update_h = (update.bottom - update.top).clamp(0, client_h as i32) as i64;
        update_w * update_h * 10 >= client_w * client_h * 9
    }

    /// 在主窗口 HWND 上安装守卫（幂等）。`on_full_invalidate` 在 UI 线程被调
    /// （WM_PAINT 与窗口同线程），应把 slint 侧整窗标脏。UI 线程 STA 单线程
    /// 泵消息，安装期间不会有并发 proc 调用，ORIGINAL_PROC 的落库顺序因此
    /// 安全。失败仅告警，不阻断主流程（守卫是兜底，常态路径是系统自发的
    /// WM_PAINT）。
    pub fn install(hwnd: isize, on_full_invalidate: Box<dyn Fn()>) -> Result<(), PlatformError> {
        if hwnd == 0 {
            return Err(PlatformError::Window(
                "窗口句柄为空，恢复重绘守卫不可用".into(),
            ));
        }
        if INSTALLED.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        ON_FULL_INVALIDATE.with(|slot| *slot.borrow_mut() = Some(on_full_invalidate));
        let previous = unsafe {
            SetWindowLongPtrW(
                hwnd as HWND,
                GWLP_WNDPROC,
                guard_proc as *const () as usize as isize,
            )
        };
        if previous == 0 {
            INSTALLED.store(false, Ordering::Release);
            ON_FULL_INVALIDATE.with(|slot| *slot.borrow_mut() = None);
            return Err(PlatformError::Window("SetWindowLongPtrW 子类化失败".into()));
        }
        ORIGINAL_PROC.store(previous, Ordering::Release);
        Ok(())
    }
}

/// 事件驱动的「主窗口就绪」通知：winit 窗口在事件循环首轮才创建，业务侧
/// （拖拽注册、渲染守卫）需要一个不轮询的挂载时机。这里用 WinEvent 钩子
/// （EVENT_OBJECT_SHOW，按本进程过滤）实现：首个可见、无属主的顶层窗口
/// 出现即回调一次并自摘钩子。替代旧「100ms Timer 退避重试」——临时
/// slint::Timer 出语句即被 Drop 取消，轮询从未真正发生过（D49 拖拽注册
/// 因此一直是死代码），且轮询本身违反项目「事件驱动优先」约束。
pub mod window_ready {
    use std::cell::RefCell;

    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetClientRect, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible,
        EVENT_OBJECT_SHOW, GA_ROOT, GWLP_HWNDPARENT, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
    };

    use crate::PlatformError;

    // WinEvent 回调投递在注册线程（Slint 主线程）上，thread_local 即可；
    // 闭包捕获 slint::Weak（非 Send），不能进全局静态。
    type PendingReadyCallback = Option<Box<dyn FnOnce(isize)>>;
    thread_local! {
        static PENDING: RefCell<PendingReadyCallback> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn win_event_proc(
        hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_object: i32,
        id_child: i32,
        _id_thread: u32,
        _dwms_event_time: u32,
    ) {
        if event != EVENT_OBJECT_SHOW
            || id_object != OBJID_WINDOW
            || id_child != 0
            || hwnd.is_null()
        {
            return;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != GetCurrentProcessId() {
            return;
        }
        // 只认本进程的可见、自 rooted、无属主顶层窗口（winit 主窗口；弹层与
        // 消息专用窗口在此被滤掉）。
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let (visible, root, client_ok, owned) = unsafe {
            (
                IsWindowVisible(hwnd) != 0,
                GetAncestor(hwnd, GA_ROOT),
                GetClientRect(hwnd, &mut rect) != 0,
                GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT) != 0,
            )
        };
        let client = client_ok && rect.right - rect.left > 0 && rect.bottom - rect.top > 0;
        if !visible || root != hwnd || owned || !client {
            return;
        }
        let callback = PENDING.with(|pending| pending.borrow_mut().take());
        if let Some(callback) = callback {
            unsafe { UnhookWinEvent(hook) };
            callback(hwnd as isize);
        }
    }

    /// 挂钩（run 事件循环前调用；钩子回调依赖调用线程泵消息，Slint 主线程
    /// 满足）。窗口就绪即触发一次回调；失败仅返回 Err，由调用方降级。
    pub fn on_first_visible_window(
        callback: Box<dyn FnOnce(isize) + 'static>,
    ) -> Result<(), PlatformError> {
        let already_pending = PENDING.with(|pending| {
            let mut pending = pending.borrow_mut();
            let was = pending.is_some();
            *pending = Some(callback);
            was
        });
        if already_pending {
            return Err(PlatformError::Window("窗口就绪钩子已挂号".into()));
        }
        let hook = unsafe {
            SetWinEventHook(
                EVENT_OBJECT_SHOW,
                EVENT_OBJECT_SHOW,
                std::ptr::null_mut(),
                Some(win_event_proc),
                GetCurrentProcessId(),
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        if hook.is_null() {
            PENDING.with(|pending| *pending.borrow_mut() = None);
            return Err(PlatformError::Window("SetWinEventHook 安装失败".into()));
        }
        Ok(())
    }
}

/// HTTP 文本拉取（D56 更新检查）与「系统默认方式打开 URL」。
///
/// 为什么是 WinHTTP 而不是 reqwest/ureq：零新依赖（D56）——windows-sys
/// 已在编译树里，schannel 出 TLS 不新增任何 crate；更新检查是 24h 一次的
/// 低频动作，不值得为它背一整套 HTTP 客户端栈。AUTOMATIC_PROXY 跟随用户
/// 系统代理——国内直连 GitHub 常需代理，这是「镜像顺序回落」之外的生命线。
///
/// 每次拉取独立建会话（session→connect→request，RAII 逆声明序回收）：
/// 频率极低，不为复用句柄引入跨线程生命周期管理。
pub mod http {
    use std::ptr;

    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
        WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
        WinHttpSendRequest, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
        WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER,
        WINHTTP_QUERY_STATUS_CODE,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use crate::{
        DownloadProgress, HttpFileDownloader, HttpTextFetcher, PlatformError, Result, UrlOpener,
    };

    /// 响应体上限：release 清单 JSON 是 KB 量级（超长 notes 也远够），对端
    /// 异常时不得无界吃内存。
    const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

    /// WinHTTP 句柄 RAII：任一步失败自动关闭已建句柄，不泄漏。
    struct HttpHandle(*mut core::ffi::c_void);
    impl Drop for HttpHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error() -> u32 {
        unsafe { GetLastError() }
    }

    /// 拆 URL 为 (是否 https, 主机, 端口, 路径)。仅接受 http(s)——更新源清单
    /// 是静态配置而非用户输入，更宽松的协议没有服务对象。
    fn split_url(url: &str) -> Result<(bool, String, u16, String)> {
        let (secure, rest) = match url.split_once("://") {
            Some(("https", rest)) => (true, rest),
            Some(("http", rest)) => (false, rest),
            _ => return Err(PlatformError::Network(format!("不支持的 URL: {url}"))),
        };
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            // 末段全数字才当端口（避免吃掉 IPv6 裸地址的冒号段）。
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (
                h,
                p.parse::<u16>()
                    .map_err(|_| PlatformError::Network(format!("端口非法: {url}")))?,
            ),
            _ => (authority, if secure { 443 } else { 80 }),
        };
        if host.is_empty() {
            return Err(PlatformError::Network(format!("URL 缺主机名: {url}")));
        }
        Ok((secure, host.to_string(), port, path.to_string()))
    }

    pub struct Win32HttpFetcher;

    impl HttpTextFetcher for Win32HttpFetcher {
        fn fetch_text(&self, url: &str, timeout_ms: u64) -> Result<String> {
            let (secure, host, port, path) = split_url(url)?;
            let host_w = wide(&host);
            let path_w = wide(&path);
            let verb_w = wide("GET");
            // api.github.com 无 User-Agent 直接拒绝请求；Accept 按 GitHub JSON 惯例带。
            let headers_w = wide(concat!(
                "User-Agent: assetdeck-updater/",
                env!("CARGO_PKG_VERSION"),
                "\r\nAccept: application/vnd.github+json\r\n"
            ));
            // 各相位同一上限：解析/连接/发送/接收合计最坏 ~4×timeout，
            // 后台线程执行不阻塞 UI，语义是「单源最多拖这么久」。
            let timeout = timeout_ms.clamp(1_000, 60_000) as i32;

            unsafe {
                let session = HttpHandle(WinHttpOpen(
                    wide(concat!("assetdeck-updater/", env!("CARGO_PKG_VERSION"))).as_ptr(),
                    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                    ptr::null(),
                    ptr::null(),
                    0,
                ));
                if session.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "WinHttpOpen 失败 (错误码 {})",
                        last_error()
                    )));
                }
                let _ = WinHttpSetTimeouts(session.0, timeout, timeout, timeout, timeout);

                let connection = HttpHandle(WinHttpConnect(session.0, host_w.as_ptr(), port, 0));
                if connection.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "连接 {host} 失败 (错误码 {})",
                        last_error()
                    )));
                }

                let flags = if secure { WINHTTP_FLAG_SECURE } else { 0 };
                let request = HttpHandle(WinHttpOpenRequest(
                    connection.0,
                    verb_w.as_ptr(),
                    path_w.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    flags,
                ));
                if request.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "构造请求失败 (错误码 {})",
                        last_error()
                    )));
                }

                if WinHttpSendRequest(
                    request.0,
                    headers_w.as_ptr(),
                    // 头串长度按字符计（全 ASCII，UTF-16 长度与字节数一致）。
                    headers_w.len() as u32 - 1,
                    ptr::null(),
                    0,
                    0,
                    0,
                ) == 0
                {
                    return Err(PlatformError::Network(format!(
                        "发送请求失败 (错误码 {})",
                        last_error()
                    )));
                }
                if WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0 {
                    return Err(PlatformError::Network(format!(
                        "接收响应失败 (错误码 {})",
                        last_error()
                    )));
                }

                let mut status: u32 = 0;
                let mut status_size = std::mem::size_of::<u32>() as u32;
                let mut index: u32 = 0;
                if WinHttpQueryHeaders(
                    request.0,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    ptr::null(),
                    &mut status as *mut u32 as *mut core::ffi::c_void,
                    &mut status_size,
                    &mut index,
                ) == 0
                {
                    return Err(PlatformError::Network(format!(
                        "读状态码失败 (错误码 {})",
                        last_error()
                    )));
                }
                if !(200..300).contains(&status) {
                    return Err(PlatformError::Network(format!("HTTP {status}（{host}）")));
                }

                let mut body: Vec<u8> = Vec::new();
                loop {
                    let mut available: u32 = 0;
                    if WinHttpQueryDataAvailable(request.0, &mut available) == 0 {
                        return Err(PlatformError::Network(format!(
                            "探测数据量失败 (错误码 {})",
                            last_error()
                        )));
                    }
                    if available == 0 {
                        break;
                    }
                    if body.len() + available as usize > MAX_BODY_BYTES {
                        return Err(PlatformError::Network(
                            "响应体超出 2 MiB 上限，疑似异常响应".into(),
                        ));
                    }
                    let start = body.len();
                    body.resize(start + available as usize, 0);
                    let mut read: u32 = 0;
                    if WinHttpReadData(
                        request.0,
                        body[start..].as_mut_ptr() as *mut core::ffi::c_void,
                        available,
                        &mut read,
                    ) == 0
                    {
                        return Err(PlatformError::Network(format!(
                            "读取响应失败 (错误码 {})",
                            last_error()
                        )));
                    }
                    if read == 0 {
                        break;
                    }
                    body.truncate(start + read as usize);
                }

                String::from_utf8(body)
                    .map_err(|_| PlatformError::Network("响应体不是合法 UTF-8".into()))
            }
        }
    }

    /// 流式文件下载器（D70）：与 [`Win32HttpFetcher`] 同一条 WinHTTP 栈
    /// （AUTOMATIC_PROXY 跟系统代理、每相位超时），差异只在消费端——
    /// 响应体按 64 KiB 块边收边写盘，不驻留内存。
    pub struct Win32FileDownloader;

    /// 读块大小。64 KiB：对三十 MB 量级的安装包，这是「进度回调足够密」
    /// 与「系统调用不碎」的折中。
    const DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;

    /// 测速取样（D71）：与 download_to_file 同一套请求机械的第三份展开——
    /// 故意不抽公共助手：三处差异都在中途行为上（收进内存/写盘/取样即止），
    /// 参数化会把 D56 已验证的路径卷进重构风险，收益不抵。
    impl Win32FileDownloader {
        fn probe_sample_impl(
            &self,
            url: &str,
            timeout_ms: u64,
            sample_bytes: u32,
            cancel: &std::sync::atomic::AtomicBool,
        ) -> Result<u64> {
            let (secure, host, port, path) = split_url(url)?;
            let host_w = wide(&host);
            let path_w = wide(&path);
            let verb_w = wide("GET");
            // Range 只当提速与取样截断用：不支持 Range 的对端回 200 全量，
            // 下面的读取循环按 sample_bytes 上限自然截断，无需区分 206/200。
            let headers_w = wide(&format!(
                concat!(
                    "User-Agent: assetdeck-updater/",
                    env!("CARGO_PKG_VERSION"),
                    "\r\nAccept: */*\r\nRange: bytes=0-{}\r\n"
                ),
                sample_bytes.saturating_sub(1)
            ));
            let timeout = timeout_ms.clamp(1_000, 60_000) as i32;
            let started = std::time::Instant::now();

            unsafe {
                let session = HttpHandle(WinHttpOpen(
                    wide(concat!("assetdeck-updater/", env!("CARGO_PKG_VERSION"))).as_ptr(),
                    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                    ptr::null(),
                    ptr::null(),
                    0,
                ));
                if session.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "WinHttpOpen 失败 (错误码 {})",
                        last_error()
                    )));
                }
                let _ = WinHttpSetTimeouts(session.0, timeout, timeout, timeout, timeout);

                let connection = HttpHandle(WinHttpConnect(session.0, host_w.as_ptr(), port, 0));
                if connection.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "连接 {host} 失败 (错误码 {})",
                        last_error()
                    )));
                }

                let flags = if secure { WINHTTP_FLAG_SECURE } else { 0 };
                let request = HttpHandle(WinHttpOpenRequest(
                    connection.0,
                    verb_w.as_ptr(),
                    path_w.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    flags,
                ));
                if request.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "构造请求失败 (错误码 {})",
                        last_error()
                    )));
                }

                if WinHttpSendRequest(
                    request.0,
                    headers_w.as_ptr(),
                    headers_w.len() as u32 - 1,
                    ptr::null(),
                    0,
                    0,
                    0,
                ) == 0
                {
                    return Err(PlatformError::Network(format!(
                        "发送请求失败 (错误码 {})",
                        last_error()
                    )));
                }
                if WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0 {
                    return Err(PlatformError::Network(format!(
                        "接收响应失败 (错误码 {})",
                        last_error()
                    )));
                }

                let mut status: u32 = 0;
                let mut status_size = std::mem::size_of::<u32>() as u32;
                let mut index: u32 = 0;
                if WinHttpQueryHeaders(
                    request.0,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    ptr::null(),
                    &mut status as *mut u32 as *mut core::ffi::c_void,
                    &mut status_size,
                    &mut index,
                ) == 0
                {
                    return Err(PlatformError::Network(format!(
                        "读状态码失败 (错误码 {})",
                        last_error()
                    )));
                }
                // 2xx 之外（含 416 Range 拒绝）按探测失败处理，候选顺延。
                if !(200..300).contains(&status) {
                    return Err(PlatformError::Network(format!("HTTP {status}（{host}）")));
                }

                let mut buf = vec![0u8; DOWNLOAD_CHUNK_BYTES];
                let mut received: u64 = 0;
                'sample: loop {
                    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err(PlatformError::Network("测速已取消".into()));
                    }
                    let mut available: u32 = 0;
                    if WinHttpQueryDataAvailable(request.0, &mut available) == 0 {
                        return Err(PlatformError::Network(format!(
                            "探测数据量失败 (错误码 {})",
                            last_error()
                        )));
                    }
                    if available == 0 {
                        break;
                    }
                    let mut pending = available;
                    while pending > 0 {
                        let want = pending.min(buf.len() as u32);
                        let mut read: u32 = 0;
                        if WinHttpReadData(
                            request.0,
                            buf.as_mut_ptr() as *mut core::ffi::c_void,
                            want,
                            &mut read,
                        ) == 0
                        {
                            return Err(PlatformError::Network(format!(
                                "读取响应失败 (错误码 {})",
                                last_error()
                            )));
                        }
                        if read == 0 {
                            break 'sample;
                        }
                        received += read as u64;
                        if received >= sample_bytes as u64 {
                            break 'sample;
                        }
                        pending -= read;
                    }
                }
                Ok(started.elapsed().as_millis() as u64)
            }
        }
    }

    impl HttpFileDownloader for Win32FileDownloader {
        fn probe_sample(
            &self,
            url: &str,
            timeout_ms: u64,
            sample_bytes: u32,
            cancel: &std::sync::atomic::AtomicBool,
        ) -> Result<u64> {
            self.probe_sample_impl(url, timeout_ms, sample_bytes, cancel)
        }

        fn download_to_file(
            &self,
            url: &str,
            dest: &std::path::Path,
            timeout_ms: u64,
            max_bytes: u64,
            progress: DownloadProgress<'_>,
            cancel: &std::sync::atomic::AtomicBool,
        ) -> Result<u64> {
            use std::io::Write;

            let (secure, host, port, path) = split_url(url)?;
            let host_w = wide(&host);
            let path_w = wide(&path);
            let verb_w = wide("GET");
            // 与 fetch_text 同一头串：api.github.com 系镜像与 objects.githubusercontent.com
            // 都不吃无 User-Agent 的裸请求。
            let headers_w = wide(concat!(
                "User-Agent: assetdeck-updater/",
                env!("CARGO_PKG_VERSION"),
                "\r\nAccept: */*\r\n"
            ));
            let timeout = timeout_ms.clamp(1_000, 60_000) as i32;

            unsafe {
                let session = HttpHandle(WinHttpOpen(
                    wide(concat!("assetdeck-updater/", env!("CARGO_PKG_VERSION"))).as_ptr(),
                    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                    ptr::null(),
                    ptr::null(),
                    0,
                ));
                if session.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "WinHttpOpen 失败 (错误码 {})",
                        last_error()
                    )));
                }
                let _ = WinHttpSetTimeouts(session.0, timeout, timeout, timeout, timeout);

                let connection = HttpHandle(WinHttpConnect(session.0, host_w.as_ptr(), port, 0));
                if connection.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "连接 {host} 失败 (错误码 {})",
                        last_error()
                    )));
                }

                let flags = if secure { WINHTTP_FLAG_SECURE } else { 0 };
                let request = HttpHandle(WinHttpOpenRequest(
                    connection.0,
                    verb_w.as_ptr(),
                    path_w.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    flags,
                ));
                if request.0.is_null() {
                    return Err(PlatformError::Network(format!(
                        "构造请求失败 (错误码 {})",
                        last_error()
                    )));
                }

                if WinHttpSendRequest(
                    request.0,
                    headers_w.as_ptr(),
                    headers_w.len() as u32 - 1,
                    ptr::null(),
                    0,
                    0,
                    0,
                ) == 0
                {
                    return Err(PlatformError::Network(format!(
                        "发送请求失败 (错误码 {})",
                        last_error()
                    )));
                }
                if WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0 {
                    return Err(PlatformError::Network(format!(
                        "接收响应失败 (错误码 {})",
                        last_error()
                    )));
                }

                let mut status: u32 = 0;
                let mut status_size = std::mem::size_of::<u32>() as u32;
                let mut index: u32 = 0;
                if WinHttpQueryHeaders(
                    request.0,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    ptr::null(),
                    &mut status as *mut u32 as *mut core::ffi::c_void,
                    &mut status_size,
                    &mut index,
                ) == 0
                {
                    return Err(PlatformError::Network(format!(
                        "读状态码失败 (错误码 {})",
                        last_error()
                    )));
                }
                if !(200..300).contains(&status) {
                    return Err(PlatformError::Network(format!("HTTP {status}（{host}）")));
                }

                // Content-Length 查不到（chunked/代理剥离）不致命：total=0 走
                // 不确定态，字节数照常上报，上限封顶由 max_bytes 兜底。
                let mut total: u64 = 0;
                let mut total_size = std::mem::size_of::<u32>() as u32;
                let mut header_index: u32 = 0;
                if WinHttpQueryHeaders(
                    request.0,
                    WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
                    ptr::null(),
                    &mut total as *mut u64 as *mut core::ffi::c_void,
                    &mut total_size,
                    &mut header_index,
                ) == 0
                {
                    total = 0;
                }

                // 覆盖式打开：上次取消/失败留下的半成品直接截断重写，
                // 不做临时文件+改名的体操（失败路径本来就是重下自愈）。
                let file = std::fs::File::create(dest).map_err(PlatformError::Io)?;
                let mut writer = std::io::BufWriter::with_capacity(DOWNLOAD_CHUNK_BYTES, file);
                let mut buf = vec![0u8; DOWNLOAD_CHUNK_BYTES];
                let mut received: u64 = 0;

                'download: loop {
                    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err(PlatformError::Network("下载已取消".into()));
                    }
                    let mut available: u32 = 0;
                    if WinHttpQueryDataAvailable(request.0, &mut available) == 0 {
                        return Err(PlatformError::Network(format!(
                            "探测数据量失败 (错误码 {})",
                            last_error()
                        )));
                    }
                    if available == 0 {
                        break;
                    }
                    let mut pending = available;
                    while pending > 0 {
                        let want = pending.min(buf.len() as u32);
                        let mut read: u32 = 0;
                        if WinHttpReadData(
                            request.0,
                            buf.as_mut_ptr() as *mut core::ffi::c_void,
                            want,
                            &mut read,
                        ) == 0
                        {
                            return Err(PlatformError::Network(format!(
                                "读取响应失败 (错误码 {})",
                                last_error()
                            )));
                        }
                        if read == 0 {
                            // 连接关闭：必须整体收尾，回到外层会在
                            // 「available>0 但 read==0」上原地打转。
                            break 'download;
                        }
                        writer
                            .write_all(&buf[..read as usize])
                            .map_err(PlatformError::Io)?;
                        received += read as u64;
                        if received > max_bytes {
                            return Err(PlatformError::Network(format!(
                                "下载超出 {max_bytes} 字节上限，疑似异常响应"
                            )));
                        }
                        progress(received, total);
                        pending -= read;
                    }
                }
                writer.flush().map_err(PlatformError::Io)?;
                Ok(received)
            }
        }
    }

    pub struct Win32UrlOpener;

    impl UrlOpener for Win32UrlOpener {
        fn open_url(&self, url: &str) -> Result<()> {
            let operation = wide("open");
            let file = wide(url);
            let instance = unsafe {
                ShellExecuteW(
                    ptr::null_mut(),
                    operation.as_ptr(),
                    file.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                    SW_SHOWNORMAL,
                )
            };
            // ShellExecuteW 约定：返回值 > 32 为成功。
            if instance as usize > 32 {
                Ok(())
            } else {
                Err(PlatformError::External(format!(
                    "ShellExecuteW 返回 {}（打开 {url}）",
                    instance as usize
                )))
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::split_url;

        #[test]
        fn split_url_breaks_down_https_default_port() {
            let parsed = split_url("https://api.github.com/repos/x/y/releases/latest").unwrap();
            assert_eq!(
                parsed,
                (
                    true,
                    "api.github.com".to_string(),
                    443u16,
                    "/repos/x/y/releases/latest".to_string()
                )
            );
        }

        #[test]
        fn split_url_keeps_explicit_port_and_bare_host() {
            assert_eq!(
                split_url("http://mirror.example:8080").unwrap(),
                (
                    false,
                    "mirror.example".to_string(),
                    8080u16,
                    "/".to_string()
                )
            );
        }

        #[test]
        fn split_url_rejects_non_http_and_hostless() {
            assert!(split_url("ftp://mirror.example/file").is_err());
            assert!(split_url("api.github.com/x").is_err());
        }
    }
}

/// SHA-256 文件摘要（D70 更新包校验）。走 BCrypt CNG：密码学原语绝不手写，
/// 也零新 crate（windows-sys 已在编译树）。流式分块喂数据，大文件不进内存。
pub mod crypto {
    use std::path::Path;

    use windows_sys::Win32::Security::Cryptography::{
        BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
        BCryptGetProperty, BCryptHashData, BCryptOpenAlgorithmProvider, BCRYPT_HASH_HANDLE,
        BCRYPT_OBJECT_LENGTH, BCRYPT_SHA256_ALGORITHM,
    };

    use crate::{PlatformError, Result};

    /// 读块大小：256 KiB。校验只跑一次每更新，内存增量瞬态且远低于预算。
    const READ_CHUNK_BYTES: usize = 256 * 1024;
    /// SHA-256 摘要长度是 FIPS 固定值，不是可查询的实现细节。
    const SHA256_DIGEST_BYTES: usize = 32;

    fn nt_success(status: i32) -> bool {
        status == 0
    }

    /// 64 字符小写十六进制串。文件不存在/读失败返回 Io，BCrypt 侧失败返回 Crypto。
    pub fn sha256_file_hex(path: &Path) -> Result<String> {
        use std::io::Read;

        let mut file = std::fs::File::open(path).map_err(PlatformError::Io)?;
        unsafe {
            let mut alg: *mut core::ffi::c_void = std::ptr::null_mut();
            if !nt_success(BCryptOpenAlgorithmProvider(
                &mut alg,
                BCRYPT_SHA256_ALGORITHM,
                std::ptr::null(),
                0,
            )) {
                return Err(PlatformError::Crypto("打开 SHA256 算法提供方失败".into()));
            }
            struct AlgGuard(*mut core::ffi::c_void);
            impl Drop for AlgGuard {
                fn drop(&mut self) {
                    unsafe { BCryptCloseAlgorithmProvider(self.0, 0) };
                }
            }
            let _alg = AlgGuard(alg);

            // 哈希对象长度是实现相关值，按属性查询后开缓冲——不写魔法数。
            let mut cb_object: u32 = 0;
            let mut cb_result: u32 = 0;
            if !nt_success(BCryptGetProperty(
                alg,
                BCRYPT_OBJECT_LENGTH,
                &mut cb_object as *mut u32 as *mut u8,
                std::mem::size_of::<u32>() as u32,
                &mut cb_result,
                0,
            )) {
                return Err(PlatformError::Crypto("查询哈希对象长度失败".into()));
            }
            let mut hash_object = vec![0u8; cb_object as usize];

            let mut hash: BCRYPT_HASH_HANDLE = std::ptr::null_mut();
            if !nt_success(BCryptCreateHash(
                alg,
                &mut hash,
                hash_object.as_mut_ptr(),
                cb_object,
                std::ptr::null(),
                0,
                0,
            )) {
                return Err(PlatformError::Crypto("创建哈希对象失败".into()));
            }
            struct HashGuard(BCRYPT_HASH_HANDLE);
            impl Drop for HashGuard {
                fn drop(&mut self) {
                    unsafe { BCryptDestroyHash(self.0) };
                }
            }
            let _hash = HashGuard(hash);

            let mut buf = vec![0u8; READ_CHUNK_BYTES];
            loop {
                let read = file.read(&mut buf).map_err(PlatformError::Io)?;
                if read == 0 {
                    break;
                }
                if !nt_success(BCryptHashData(hash, buf.as_mut_ptr(), read as u32, 0)) {
                    return Err(PlatformError::Crypto("喂数据进哈希失败".into()));
                }
            }

            let mut digest = [0u8; SHA256_DIGEST_BYTES];
            if !nt_success(BCryptFinishHash(
                hash,
                digest.as_mut_ptr(),
                SHA256_DIGEST_BYTES as u32,
                0,
            )) {
                return Err(PlatformError::Crypto("取摘要失败".into()));
            }
            Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::sha256_file_hex;

        #[test]
        fn sha256_matches_known_vectors() {
            let dir = std::env::temp_dir().join("assetdeck-platform-sha256-test");
            std::fs::create_dir_all(&dir).unwrap();

            let empty = dir.join("empty.bin");
            std::fs::write(&empty, b"").unwrap();
            assert_eq!(
                sha256_file_hex(&empty).unwrap(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );

            // FIPS 180-2 "abc" 向量 + 跨块输入（> 1 读块）验证分块喂送正确。
            let abc = dir.join("abc.bin");
            std::fs::write(&abc, b"abc").unwrap();
            assert_eq!(
                sha256_file_hex(&abc).unwrap(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            );

            let big = dir.join("big.bin");
            let chunk = [0xABu8; 1024];
            let mut big_bytes = Vec::with_capacity(1024 * 1024);
            for _ in 0..1024 {
                big_bytes.extend_from_slice(&chunk);
            }
            std::fs::write(&big, &big_bytes).unwrap();
            let hex = sha256_file_hex(&big).unwrap();
            assert_eq!(hex.len(), 64);
            assert!(hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }

        #[test]
        fn sha256_missing_file_is_io_error() {
            let missing = std::env::temp_dir().join("assetdeck-sha256-不存在.bin");
            assert!(matches!(
                sha256_file_hex(&missing),
                Err(crate::PlatformError::Io(_))
            ));
        }
    }
}

pub use crypto::sha256_file_hex;
pub use http::{Win32FileDownloader, Win32HttpFetcher, Win32UrlOpener};
