//! 多角度「取输入框焦点」API 真机探测（开发用，不进产品路径）。
//!
//! 红线守恒：本工具**绝不合成任何键盘事件**（尤其 0x0D），不写剪贴板；
//! 只做只读探测（枚举/caret/焦点查询）与 UIA SetFocus / 锚点单击两类落焦动作。
//! 例外：`--paste-element`（D75 实验）额外合成**且仅合成** Ctrl+V——
//! 验证「元素级设焦 → 系统级粘贴」全链路，0x0D 红线不变。
//!
//! 覆盖的角度（每个都实测耗时并验证「焦点真的落进目标进程的可写控件」）：
//! - `p0`      产品现状的 AlreadyEditable 判定：UIA GetFocusedElement + 进程/可写复核
//! - `gti`     GetGUIThreadInfo：目标线程的 hwndFocus / hwndCaret / rcCaret（微秒级）
//! - `ati`     AttachThreadInput + GetFocus：跨进程直读输入队列焦点
//! - `uia_true` 产品基线：TrueCondition FindAll 全子树 + 线性扫描 ≤200 + SetFocus
//! - `uia_prop` 属性条件变体：CreatePropertyCondition(ControlType) 直查 Edit/Document
//! - `children`/`wake`/`msaa`（跑批后一次）：原生子 HWND 清点；WM_GETOBJECT 无障碍
//!   唤醒（CEF 类目标的关键实验）前后对比 UIA/MSAA 可见度
//! - `--click` 可选：复用产品 Win32InputFocuser 的锚点单击（全部防呆守卫保留），
//!   点击后复测 caret/焦点信号——验证「点完再复核」的可行性
//! - `--a11y` 激活后深探（D74）：先在 Chrome_RenderWidgetHostHWND（无则顶层）发
//!   WM_GETOBJECT 激活无障碍树，事件驱动等建树后做 UIA 深枚举 + MSAA 递归走树，
//!   并用画像锚点做命中判定。既有电池的盲区正是「UIA 枚举全部跑在激活之前」。
//!
//! 用法：
//!   focus_probe --list
//!   focus_probe --hwnd <N> [--runs 3] [--click "0.66,0.85"]
//!   focus_probe --a11y <N> [--anchor "0.49,0.92"] [--a11y-click <候选序号>]
//!   focus_probe --semantic-probe <N>   (guarded: strict semantic evidence only)
//!   focus_probe --paste-element <N> [--marker <串>] [--no-setfocus|--focus-rwh]

#[path = "../focus_probe_raw.rs"]
mod raw_snapshot;

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use windows::core::{Interface, BOOL, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
    UIA_ControlTypePropertyId, UIA_DocumentControlTypeId, UIA_EditControlTypeId,
    UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClassNameW, GetClientRect, GetForegroundWindow,
    GetGUIThreadInfo, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, SendMessageTimeoutW, SetForegroundWindow, GUITHREADINFO, GUI_CARETBLINKING,
    SMTO_ABORTIFHUNG, WM_GETOBJECT,
};

thread_local! {
    static UIA: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
}

fn uia() -> IUIAutomation {
    UIA.with(|slot| {
        if let Some(existing) = slot.borrow().as_ref() {
            return existing.clone();
        }
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
        assert!(
            matches!(hr, S_OK | S_FALSE | RPC_E_CHANGED_MODE),
            "CoInitializeEx failed: {hr:?}"
        );
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .expect("CoCreateInstance(CUIAutomation)");
        *slot.borrow_mut() = Some(automation.clone());
        automation
    })
}

fn hwnd_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn hwnd_title(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..copied.max(0) as usize])
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn window_thread(hwnd: HWND) -> u32 {
    unsafe { GetWindowThreadProcessId(hwnd, None) }
}

fn exe_name_of(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return format!("pid{pid}");
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let name = if QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
        {
            String::from_utf16_lossy(&buf[..len as usize])
                .rsplit('\\')
                .next()
                .unwrap_or("")
                .to_string()
        } else {
            format!("pid{pid}")
        };
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        name
    }
}

// ---------------------------------------------------------------------------
// 验证：焦点真的落在目标进程的可写控件上（产品同一标准）
// ---------------------------------------------------------------------------

fn focused_element_info(automation: &IUIAutomation, target_pid: u32) -> (bool, String) {
    let Ok(focused) = (unsafe { automation.GetFocusedElement() }) else {
        return (false, "getfocused-failed".into());
    };
    let pid = unsafe { focused.CurrentProcessId() }.unwrap_or(-1) as u32;
    let control = unsafe { focused.CurrentControlType() }.unwrap_or(UIA_CONTROLTYPE_ID_ZERO);
    let class = unsafe { focused.CurrentClassName() }
        .map(|v| v.to_string())
        .unwrap_or_default();
    let name = unsafe { focused.CurrentName() }
        .map(|v| v.to_string())
        .unwrap_or_default();
    let editable = pid == target_pid
        && (control == UIA_EditControlTypeId || control == UIA_DocumentControlTypeId)
        && unsafe {
            focused
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .map(|p| {
                    p.CurrentIsReadOnly()
                        .map(|ro| !ro.as_bool())
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        };
    (
        editable,
        format!("pid={pid} control={control:?} class={class:?} name={name:?}"),
    )
}

const UIA_CONTROLTYPE_ID_ZERO: windows::Win32::UI::Accessibility::UIA_CONTROLTYPE_ID =
    windows::Win32::UI::Accessibility::UIA_CONTROLTYPE_ID(0);

// ---------------------------------------------------------------------------
// 策略实现
// ---------------------------------------------------------------------------

/// 产品基线：TrueCondition 全子树 FindAll + 线性扫描 + SetFocus + 复核。
fn probe_uia_true(automation: &IUIAutomation, hwnd: HWND, target_pid: u32) {
    use windows::Win32::UI::Accessibility::TreeScope_Descendants;
    let started = Instant::now();
    let root = match unsafe { automation.ElementFromHandle(hwnd) } {
        Ok(root) => root,
        Err(error) => {
            println!("strat=uia_true error={error}");
            return;
        }
    };
    let element_us = started.elapsed().as_micros();
    let condition = match unsafe { automation.CreateTrueCondition() } {
        Ok(condition) => condition,
        Err(error) => {
            println!("strat=uia_true error={error}");
            return;
        }
    };
    let all = match unsafe { root.FindAll(TreeScope_Descendants, &condition) } {
        Ok(all) => all,
        Err(error) => {
            println!("strat=uia_true error={error}");
            return;
        }
    };
    let length = unsafe { all.Length() }.unwrap_or(0);

    let scan_start = Instant::now();
    let mut editable = 0usize;
    let mut set_focus_us = 0u128;
    let mut focused_name = String::new();
    for index in 0..length.min(200) {
        let Ok(element) = (unsafe { all.GetElement(index) }) else {
            continue;
        };
        let control = unsafe { element.CurrentControlType() }.unwrap_or(UIA_CONTROLTYPE_ID_ZERO);
        if control != UIA_EditControlTypeId && control != UIA_DocumentControlTypeId {
            continue;
        }
        if !unsafe { element.CurrentIsEnabled() }
            .map(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let read_only = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .map(|p| p.CurrentIsReadOnly().map(|ro| ro.as_bool()).unwrap_or(true))
                .unwrap_or(true)
        };
        if read_only {
            continue;
        }
        editable += 1;
        if set_focus_us == 0 {
            focused_name = unsafe { element.CurrentName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            let sf = Instant::now();
            unsafe { element.SetFocus() }.ok();
            set_focus_us = sf.elapsed().as_micros();
        }
    }
    let scan_us = scan_start.elapsed().as_micros();
    let (verified, info) = focused_element_info(automation, target_pid);
    println!(
        "strat=uia_true element_us={element_us} findall_elems={length} scan_us={scan_us} editable={editable} setfocus_us={set_focus_us} verified={verified} focus_name={focused_name:?} {info}"
    );
}

/// 属性条件变体：ControlType 直查，让 COM 侧做过滤。
fn probe_uia_prop(automation: &IUIAutomation, hwnd: HWND, target_pid: u32) {
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{TreeScope_Children, TreeScope_Descendants};
    let started = Instant::now();
    let root = match unsafe { automation.ElementFromHandle(hwnd) } {
        Ok(root) => root,
        Err(error) => {
            println!("strat=uia_prop error={error}");
            return;
        }
    };
    let mut found: Option<IUIAutomationElement> = None;
    let mut counts = String::new();
    for (label, control_id) in [
        ("edit", UIA_EditControlTypeId),
        ("document", UIA_DocumentControlTypeId),
    ] {
        let Ok(condition) = (unsafe {
            automation
                .CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(control_id.0))
        }) else {
            continue;
        };
        // 全子树为主口径；直接子级作为 CEF 浅层挂载的对照。
        let descendants = unsafe { root.FindAll(TreeScope_Descendants, &condition) };
        let children = unsafe { root.FindAll(TreeScope_Children, &condition) };
        let desc_len = descendants
            .as_ref()
            .map(|a| unsafe { a.Length() })
            .unwrap_or(Ok(0))
            .unwrap_or(0);
        let child_len = children
            .as_ref()
            .map(|a| unsafe { a.Length() })
            .unwrap_or(Ok(0))
            .unwrap_or(0);
        counts.push_str(&format!("{label}={desc_len}(child {child_len}) "));
        if found.is_none() {
            if let Ok(pool) = descendants.as_ref() {
                for index in 0..desc_len {
                    let Ok(element) = (unsafe { pool.GetElement(index) }) else {
                        continue;
                    };
                    if !unsafe { element.CurrentIsEnabled() }
                        .map(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let read_only = unsafe {
                        element
                            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                            .map(|p| p.CurrentIsReadOnly().map(|ro| ro.as_bool()).unwrap_or(true))
                            .unwrap_or(true)
                    };
                    if read_only {
                        continue;
                    }
                    found = Some(element);
                    break;
                }
            }
        }
    }
    let total_us = started.elapsed().as_micros();
    match found {
        Some(element) => {
            let name = unsafe { element.CurrentName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            let class = unsafe { element.CurrentClassName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            let sf = Instant::now();
            let set_ok = unsafe { element.SetFocus() }.is_ok();
            let set_focus_us = sf.elapsed().as_micros();
            let (verified, info) = focused_element_info(automation, target_pid);
            println!(
                "strat=uia_prop total_us={total_us} {counts}setfocus_us={set_focus_us} set_ok={set_ok} target={name:?}/{class:?} verified={verified} {info}"
            );
        }
        None => {
            let (verified, info) = focused_element_info(automation, target_pid);
            println!(
                "strat=uia_prop total_us={total_us} {counts}candidate=0 verified={verified} {info}"
            );
        }
    }
}

/// FindFirst 单发：属性条件 + 找到即停，不取全量数组。
fn probe_uia_first(automation: &IUIAutomation, hwnd: HWND, target_pid: u32) {
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::TreeScope_Descendants;
    let started = Instant::now();
    let Ok(root) = (unsafe { automation.ElementFromHandle(hwnd) }) else {
        println!("strat=uia_first error=elementfromhandle");
        return;
    };
    let mut first_us = 0u128;
    let mut found: Option<IUIAutomationElement> = None;
    for control_id in [UIA_EditControlTypeId, UIA_DocumentControlTypeId] {
        let Ok(condition) = (unsafe {
            automation
                .CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(control_id.0))
        }) else {
            continue;
        };
        let probe = Instant::now();
        let hit = unsafe { root.FindFirst(TreeScope_Descendants, &condition) };
        let elapsed = probe.elapsed().as_micros();
        first_us += elapsed;
        if let Ok(element) = hit {
            let enabled = unsafe { element.CurrentIsEnabled() }
                .map(|v| v.as_bool())
                .unwrap_or(false);
            let read_only = unsafe {
                element
                    .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    .map(|p| p.CurrentIsReadOnly().map(|ro| ro.as_bool()).unwrap_or(true))
                    .unwrap_or(true)
            };
            if enabled && !read_only {
                found = Some(element);
                break;
            }
        }
    }
    let total_us = started.elapsed().as_micros();
    match found {
        Some(element) => {
            let class = unsafe { element.CurrentClassName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            let name = unsafe { element.CurrentName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            let sf = Instant::now();
            let set_ok = unsafe { element.SetFocus() }.is_ok();
            let set_focus_us = sf.elapsed().as_micros();
            let (verified, info) = focused_element_info(automation, target_pid);
            println!(
                "strat=uia_first total_us={total_us} findfirst_us={first_us} setfocus_us={set_focus_us} set_ok={set_ok} target={name:?}/{class:?} verified={verified} {info}"
            );
        }
        None => println!("strat=uia_first total_us={total_us} findfirst_us={first_us} candidate=0"),
    }
}

/// 子 HWND 限域搜索：把 UIA 查询钉在每个原生子窗口上，验证「缩小搜索树」的收益。
fn probe_uia_children_scope(automation: &IUIAutomation, hwnd: HWND, target_pid: u32) {
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::TreeScope_Descendants;
    let started = Instant::now();
    let mut children: Vec<HWND> = Vec::new();
    collect_children(hwnd, &mut children);
    children.truncate(6);
    let mut log = String::new();
    let mut found: Option<IUIAutomationElement> = None;
    for child in children {
        let child_us = Instant::now();
        let Ok(root) = (unsafe { automation.ElementFromHandle(child) }) else {
            log.push_str(&format!("[{:?}=err] ", hwnd_class(child)));
            continue;
        };
        for control_id in [UIA_EditControlTypeId, UIA_DocumentControlTypeId] {
            let Ok(condition) = (unsafe {
                automation.CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &VARIANT::from(control_id.0),
                )
            }) else {
                continue;
            };
            let Ok(element) = (unsafe { root.FindFirst(TreeScope_Descendants, &condition) }) else {
                continue;
            };
            let enabled = unsafe { element.CurrentIsEnabled() }
                .map(|v| v.as_bool())
                .unwrap_or(false);
            let read_only = unsafe {
                element
                    .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    .map(|p| p.CurrentIsReadOnly().map(|ro| ro.as_bool()).unwrap_or(true))
                    .unwrap_or(true)
            };
            if enabled && !read_only {
                log.push_str(&format!(
                    "[{:?} hit_us={}] ",
                    hwnd_class(child),
                    child_us.elapsed().as_micros()
                ));
                found = Some(element);
                break;
            }
        }
        if found.is_some() {
            break;
        }
        log.push_str(&format!(
            "[{:?} miss_us={}] ",
            hwnd_class(child),
            child_us.elapsed().as_micros()
        ));
    }
    let total_us = started.elapsed().as_micros();
    match found {
        Some(element) => {
            let class = unsafe { element.CurrentClassName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            let name = unsafe { element.CurrentName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            let sf = Instant::now();
            let set_ok = unsafe { element.SetFocus() }.is_ok();
            let set_focus_us = sf.elapsed().as_micros();
            let (verified, info) = focused_element_info(automation, target_pid);
            println!(
                "strat=uia_scope total_us={total_us} {log}setfocus_us={set_focus_us} set_ok={set_ok} target={name:?}/{class:?} verified={verified} {info}"
            );
        }
        None => println!("strat=uia_scope total_us={total_us} {log}candidate=0"),
    }
}

/// GetGUIThreadInfo：目标线程的焦点 HWND 与 caret。
fn probe_guithreadinfo(hwnd: HWND, label: &str) {
    let started = Instant::now();
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let tid = window_thread(hwnd);
    let ok = unsafe { GetGUIThreadInfo(tid, &mut info) }.is_ok();
    let elapsed = started.elapsed().as_micros();
    if !ok {
        println!("probe=gti{label} us={elapsed} ok=false");
        return;
    }
    let focus_class = hwnd_class(info.hwndFocus);
    let caret_class = hwnd_class(info.hwndCaret);
    let caret_blink = (info.flags & GUI_CARETBLINKING) == GUI_CARETBLINKING;
    let r = info.rcCaret;
    println!(
        "probe=gti{label} us={elapsed} focus_class={focus_class:?} focus_is_target={} caret_class={caret_class:?} caret_rc=({},{},{},{}) caret_blink={caret_blink}",
        window_pid(info.hwndFocus) == window_pid(hwnd),
        r.left, r.top, r.right, r.bottom
    );
}

/// AttachThreadInput + GetFocus：跨进程直读输入队列焦点。
fn probe_attach_thread_input(hwnd: HWND, label: &str) {
    let started = Instant::now();
    let tid = window_thread(hwnd);
    let my_tid = unsafe { GetCurrentThreadId() };
    let attached = unsafe { AttachThreadInput(my_tid, tid, true) }.as_bool();
    if !attached {
        println!(
            "probe=ati{label} us={} attach=false",
            started.elapsed().as_micros()
        );
        return;
    }
    let focus = unsafe { GetFocus() };
    let class = hwnd_class(focus);
    let pid = window_pid(focus);
    unsafe {
        let _ = AttachThreadInput(my_tid, tid, false);
    };
    println!(
        "probe=ati{label} us={} focus_class={class:?} focus_pid={pid} focus_is_target={}",
        started.elapsed().as_micros(),
        pid == window_pid(hwnd)
    );
}

/// 子 HWND 清点：目标窗口里到底有哪些原生窗口。
fn probe_children(hwnd: HWND) {
    let mut lines: Vec<String> = Vec::new();
    collect_child_tree(hwnd, 0, &mut lines);
    println!("probe=children hwnd={}", hwnd.0 as isize);
    for line in lines {
        println!("  {line}");
    }
}

fn collect_child_tree(hwnd: HWND, depth: usize, lines: &mut Vec<String>) {
    if depth > 2 || lines.len() > 40 {
        return;
    }
    let mut ctx = ChildCtx { depth, lines };
    unsafe {
        let _ = EnumChildWindows(
            Some(hwnd),
            Some(enum_child),
            LPARAM(&mut ctx as *mut ChildCtx as isize),
        );
    }
}

struct ChildCtx<'a> {
    depth: usize,
    lines: &'a mut Vec<String>,
}

unsafe extern "system" fn enum_child(child: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut ChildCtx);
    ctx.lines.push(format!(
        "{}hwnd={} class={:?} title={:?}",
        "  ".repeat(ctx.depth + 1),
        child.0 as isize,
        hwnd_class(child),
        hwnd_title(child)
    ));
    collect_child_tree(child, ctx.depth + 1, ctx.lines);
    BOOL(1)
}

/// WM_GETOBJECT 无障碍唤醒（CEF/Chromium 目标的关键实验）。
fn probe_wake(hwnd: HWND, label: &str) {
    let mut targets = vec![hwnd];
    collect_children(hwnd, &mut targets);
    for target in targets {
        let mut result: usize = 0;
        unsafe {
            SendMessageTimeoutW(
                target,
                WM_GETOBJECT,
                WPARAM(0),
                LPARAM(0xFFFFFFFCu32 as i32 as isize), // OBJID_CLIENT
                SMTO_ABORTIFHUNG,
                500,
                Some(&mut result),
            )
        };
        println!(
            "probe=wake{label} hwnd={} class={:?} lresult_nonzero={}",
            target.0 as isize,
            hwnd_class(target),
            result != 0
        );
    }
}

fn collect_children(hwnd: HWND, out: &mut Vec<HWND>) {
    unsafe {
        let _ = EnumChildWindows(
            Some(hwnd),
            Some(collect_child),
            LPARAM(out as *mut Vec<HWND> as isize),
        );
    }
}

unsafe extern "system" fn collect_child(child: HWND, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<HWND>);
    out.push(child);
    BOOL(1)
}

/// MSAA 视角：accFocus + 一层子树里 ROLE_SYSTEM_TEXT 计数。
fn probe_msaa(hwnd: HWND, label: &str) {
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{AccessibleChildren, IAccessible};
    const ROLE_SYSTEM_TEXT: i32 = 0x2A;
    const OBJID_CLIENT: u32 = 0xFFFF_FFFC;

    let started = Instant::now();
    let acc: IAccessible = match access_client_accessible(hwnd, OBJID_CLIENT) {
        Ok(acc) => acc,
        Err(error) => {
            println!(
                "probe=msaa{label} us={} error={error}",
                started.elapsed().as_micros()
            );
            return;
        }
    };
    let count = unsafe { acc.accChildCount() }.unwrap_or(-1);
    let focus_desc = if count > 0 {
        match unsafe { acc.accFocus() } {
            Ok(focus) => describe_variant(&focus),
            Err(error) => format!("accFocus err={error}"),
        }
    } else {
        "no_children".into()
    };
    let mut text_roles = 0usize;
    if count > 0 {
        let mut variants: Vec<VARIANT> = (0..count.clamp(1, 64))
            .map(|_| unsafe { std::mem::zeroed() })
            .collect();
        let mut obtained: i32 = 0;
        if unsafe { AccessibleChildren(&acc, 0, &mut variants, &mut obtained) }.is_ok() {
            for variant in variants.iter().take(obtained as usize) {
                if variant_role_is_text(variant, ROLE_SYSTEM_TEXT) {
                    text_roles += 1;
                }
            }
        }
    }
    println!(
        "probe=msaa{label} us={} childcount={count} text_roles_level1={text_roles} {focus_desc}",
        started.elapsed().as_micros()
    );
}

fn access_client_accessible(
    hwnd: HWND,
    objid: u32,
) -> windows::core::Result<windows::Win32::UI::Accessibility::IAccessible> {
    use windows::core::Type;
    use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};
    let mut ppv: *mut core::ffi::c_void = std::ptr::null_mut();
    unsafe {
        AccessibleObjectFromWindow(hwnd, objid, &IAccessible::IID, &mut ppv)?;
        IAccessible::from_abi(ppv)
    }
}

fn describe_variant(variant: &windows::Win32::System::Variant::VARIANT) -> String {
    use windows::Win32::System::Variant::{VT_DISPATCH, VT_EMPTY, VT_I4};
    let vt = unsafe { variant.Anonymous.Anonymous.vt };
    if vt == VT_EMPTY {
        return "accFocus=empty".into();
    }
    if vt == VT_DISPATCH {
        return "accFocus=dispatch(accessible)".into();
    }
    if vt == VT_I4 {
        let l = unsafe { variant.Anonymous.Anonymous.Anonymous.lVal };
        return format!("accFocus=child_id({l})");
    }
    format!("accFocus=vt({vt:?})")
}

/// --a11y-activate：Chromium 渐进式无障碍的「强制升级」探针（调研验证用）。
///
/// 依据（2026-09-04 源码调研，Chromium main + M87~M139 对照）：
/// - OBJID_CLIENT 只触发 kNativeAPIs（返回根 IAccessible，不建 DOM 树）——此前
///   「CEF 树不物化」的结论止步于此档。
/// - 升级钩子在 ax_platform_node_win.cc 的属性 getter 内部：真实调用
///   get_accName（置 is_name_used_）→ get_accDefaultAction（直接升
///   kAXModeBasic+kExtendedProperties）→ WM_GETOBJECT(objid=1) 蜜罐
///   （幂等兜底）。必须先 Name 后蜜罐，顺序不能反。
/// - renderer 建树是异步的，等待后重枚举 UIA 判定（Edit/Document 是否出现）。
///
/// 激活协议本体：RWH 发现 → 真实 COM 属性调用（Name→DefaultAction→Role）→ 蜜罐。
/// 返回触达的 RWH hwnd 列表（空 = 该目标无 CEF 渲染面或未触达）。
fn a11y_activate_protocol(root: HWND) -> Vec<HWND> {
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

    const OBJID_CLIENT: u32 = 0xFFFF_FFFC;
    const WM_GETOBJECT: u32 = 0x003D;
    let child_list = {
        let mut children: Vec<HWND> = Vec::new();
        collect_children(root, &mut children);
        children
            .into_iter()
            .filter(|child| hwnd_class(*child) == "Chrome_RenderWidgetHostHWND")
            .collect::<Vec<_>>()
    };
    println!(
        "a11y-activate: render widget hwnds = {:?}",
        child_list.iter().map(|h| h.0 as isize).collect::<Vec<_>>()
    );
    if child_list.is_empty() {
        println!("a11y-activate: no Chrome_RenderWidgetHostHWND under root");
        return child_list;
    }

    let child_self = VARIANT::from(0i32);
    for hwnd in &child_list {
        let acc = match access_client_accessible(*hwnd, OBJID_CLIENT) {
            Ok(acc) => acc,
            Err(error) => {
                println!(
                    "a11y-activate hwnd=0x{:X}: ObjectFromLresult err={error}",
                    hwnd.0 as isize
                );
                continue;
            }
        };
        let count = unsafe { acc.accChildCount() }.unwrap_or(-1);
        let name = unsafe { acc.get_accName(&child_self) }
            .map(|v| {
                let s: String = v.to_string().chars().take(40).collect();
                s
            })
            .unwrap_or_else(|e| format!("err={e}"));
        let default_action = unsafe { acc.get_accDefaultAction(&child_self) }
            .map(|v| v.to_string())
            .unwrap_or_else(|e| format!("err={e}"));
        let role = unsafe { acc.get_accRole(&child_self) }
            .ok()
            .and_then(|v| msaa_role_i4(&v))
            .map(|r| format!("0x{r:X}"))
            .unwrap_or_else(|| "none".into());
        println!(
            "a11y-activate hwnd={:p}: root accChildCount={count} accName={name:?} accDefaultAction={default_action:?} accRole={role}",
            hwnd.0
        );
        // 蜜罐：必须在真实读 Name 之后（is_name_used_ 前置）。
        let honey = unsafe {
            SendMessageW(
                *hwnd,
                WM_GETOBJECT,
                Some(windows::Win32::Foundation::WPARAM(0)),
                Some(windows::Win32::Foundation::LPARAM(1)),
            )
        };
        println!(
            "a11y-activate: honey pot WM_GETOBJECT(objid=1) -> 0x{:X}",
            honey.0
        );
    }
    child_list
}

fn run_a11y_activation(root: HWND) {
    let _ = a11y_activate_protocol(root);

    println!("a11y-activate: waiting 4s for renderer tree build...");
    std::thread::sleep(std::time::Duration::from_millis(4000));

    let automation = uia();
    let mut candidates: Vec<A11yCandidate> = Vec::new();
    let total = uia_collect_editables(&automation, root, &mut candidates, None);
    println!(
        "a11y-activate: AFTER descendants={total} edit_candidates={}",
        candidates.len()
    );
    for (index, cand) in candidates.iter().enumerate() {
        let (l, t, r, b) = cand.rect;
        println!(
            "  cand[{index}] {} rect=({l},{t})-({r},{b}) {}",
            cand.source, cand.detail
        );
    }
}

/// --paste-element 的落焦模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PasteFocusMode {
    /// UIA element.SetFocus()（对 CEF 走 UIA↔MSAA 桥的 accSelect TAKEFOCUS）
    Uia,
    /// AttachThreadInput + SetFocus(RWH legacy hwnd)（键盘消息转发链）
    Rwh,
    /// UIA SetFocus 到「消息聊天」Document 本体（accSelect TAKEFOCUS 路径，
    /// 验证 Blink 是否把焦点恢复到文档内最后聚焦的 DOM 节点/composer）
    Doc,
    /// 完全不设焦点——验证「激活后焦点自恢复进 composer」假设
    None,
}

/// --paste-element：D75 元素级粘贴全链路实验（零坐标）。
///
/// 前置：剪贴板里已放验证标记（外部 Set-Clipboard）。
/// 流程：产品激活器拉前台 → a11y 激活协议 → UIA 枚举 Edit 候选 → 选 composer
/// （非只读 + 名称提示，面积兜底——选择靠元素属性不靠坐标）→ 按模式设焦点 →
/// SendInput Ctrl+V → 重枚举读候选 Value 验证标记落框。全程不点击。
fn run_paste_element(
    root: HWND,
    root_value: isize,
    automation: &IUIAutomation,
    mode: PasteFocusMode,
    marker: &str,
    nav: Option<&str>,
    tabs: u32,
) {
    use platform::win32::Win32WindowActivator;
    use platform::{WindowActivator, WindowHandle};

    let pid = window_pid(root);
    println!("=== paste-element hwnd={root_value} pid={pid} mode={mode:?} marker={marker:?} ===");

    // 0) 前台：产品激活器（带确认循环与线程附加）。
    let started = Instant::now();
    let activated = Win32WindowActivator
        .activate(WindowHandle(root_value), 200, 120)
        .unwrap_or(false);
    println!(
        "activate(product) ms={} ok={activated} foreground_now={}",
        started.elapsed().as_millis(),
        unsafe { GetForegroundWindow() } == root
    );

    // 1) a11y 激活协议 + 等建树。
    let touched = a11y_activate_protocol(root);
    println!("paste-element: waiting 4s for renderer tree build...");
    std::thread::sleep(std::time::Duration::from_millis(4000));

    // 2) 枚举 Edit 候选（保留元素本体）。
    let mut candidates: Vec<A11yCandidate> = Vec::new();
    let mut elements = Vec::new();
    let total = uia_collect_editables(automation, root, &mut candidates, Some(&mut elements));
    println!(
        "paste-element: descendants={total} edit_candidates={}",
        candidates.len()
    );
    for (index, cand) in candidates.iter().enumerate() {
        let (l, t, r, b) = cand.rect;
        println!("  cand[{index}] rect=({l},{t})-({r},{b}) {}", cand.detail);
    }

    // 2.5) 可选元素级导航：在已物化树里按名称找导航项触发（无坐标），
    //      等新面板建树后重跑激活协议 + 重枚举 Edit。
    if let Some(nav) = nav {
        if candidates.is_empty() {
            println!("paste-element: nav {nav:?} skipped (no tree to search)");
        } else {
            let ok = invoke_by_name(automation, root, nav);
            println!("paste-element: nav {nav:?} invoked={ok}");
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let _ = a11y_activate_protocol(root);
            println!("paste-element: waiting 4s after nav...");
            std::thread::sleep(std::time::Duration::from_millis(4000));
            candidates.clear();
            elements.clear();
            let total =
                uia_collect_editables(automation, root, &mut candidates, Some(&mut elements));
            println!(
                "paste-element: after-nav descendants={total} edit_candidates={}",
                candidates.len()
            );
            for (index, cand) in candidates.iter().enumerate() {
                let (l, t, r, b) = cand.rect;
                println!("  cand[{index}] rect=({l},{t})-({r},{b}) {}", cand.detail);
            }
        }
    }

    if candidates.is_empty() {
        println!("PASTE_ELEMENT: FAIL no-edit-candidates (activation did not materialize tree)");
        return;
    }

    // 3) composer 选择：非只读优先，名称/aid 提示（chat/input/message/聊天/输入），
    //    都不中则取面积最大的可写 Edit。选择依据全是元素属性，不碰屏幕坐标。
    let hint = |detail: &str| {
        let lower = detail.to_lowercase();
        lower.contains("chat")
            || lower.contains("input")
            || lower.contains("message")
            || detail.contains("聊天")
            || detail.contains("输入")
    };
    // Historical experiment retained for provenance, unreachable from CLI.
    // No area-based fallback: geometry is not composer identity.
    let writable: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.detail.contains("readonly=Some(false)"))
        .map(|(i, _)| i)
        .collect();
    let chosen = writable
        .iter()
        .copied()
        .find(|i| hint(&candidates[*i].detail));
    let chosen_index = match chosen {
        Some(index) => index,
        None => {
            println!("PASTE_ELEMENT: FAIL no-writable-edit (readonly all true)");
            return;
        }
    };
    println!(
        "paste-element: chosen cand[{chosen_index}] {}",
        candidates[chosen_index].detail
    );

    // 4) 按模式设焦点。
    match mode {
        PasteFocusMode::Uia => {
            let focus_result = unsafe { elements[chosen_index].SetFocus() };
            println!("paste-element: UIA SetFocus err={:?}", focus_result.err());
        }
        PasteFocusMode::Rwh => {
            let target_thread = window_thread(root);
            let own_thread = unsafe { GetCurrentThreadId() };
            let attached = unsafe { AttachThreadInput(own_thread, target_thread, true) }.as_bool();
            let focus_result = unsafe {
                windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(
                    touched.last().copied().or(Some(root)),
                )
            };
            println!(
                "paste-element: AttachThreadInput ok={attached} SetFocus(RWH) prev={focus_result:?}"
            );
            unsafe {
                let _ = AttachThreadInput(own_thread, target_thread, false);
            }
        }
        PasteFocusMode::Doc => {
            // 「消息聊天」Document 元素本体走 UIA SetFocus（accSelect TAKEFOCUS），
            // 验证 Blink 是否把焦点恢复到文档内最后聚焦的 DOM 节点（composer）。
            let doc = candidates.iter().position(|c| {
                c.detail.contains("Chrome_RenderWidgetHostHWND") && c.detail.contains("消息聊天")
            });
            match doc.map(|index| (index, &elements[index])) {
                Some((index, element)) => {
                    let focus_result = unsafe { element.SetFocus() };
                    println!(
                        "paste-element: UIA SetFocus(doc cand[{index}]) err={:?}",
                        focus_result.err()
                    );
                }
                None => println!("paste-element: doc candidate not found (no 消息聊天 Document)"),
            }
        }
        PasteFocusMode::None => {
            println!(
                "paste-element: mode=None, skipping explicit focus (focus-restore hypothesis)"
            );
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (already, info) = focused_element_info(automation, pid);
    println!("paste-element: focus_after_set already_editable={already} {info}");

    // 4.5) Tab 键盘导航（--tabs N）：焦点在消息文档内逐个遍历 focusable，
    //      每按一次枚举一次——composer 首次获焦即物化为可写 Edit，枚举能看见。
    //      途中一旦出现带提示词的可写 Edit 就提前收工（无需按满 N 次）。
    if tabs > 0 {
        for step in 1..=tabs {
            let sent = send_tab();
            std::thread::sleep(std::time::Duration::from_millis(400));
            let mut tab_cands: Vec<A11yCandidate> = Vec::new();
            let mut tab_elems: Vec<windows::Win32::UI::Accessibility::IUIAutomationElement> =
                Vec::new();
            let _ = uia_collect_editables(automation, root, &mut tab_cands, Some(&mut tab_elems));
            let writable: Vec<usize> = tab_cands
                .iter()
                .enumerate()
                .filter(|(_, c)| c.detail.contains("readonly=Some(false)"))
                .map(|(i, _)| i)
                .collect();
            let focus_now = tab_elems
                .get(writable.first().copied().unwrap_or(0))
                .map(|e| {
                    let f = unsafe { e.CurrentHasKeyboardFocus() };
                    f.map(|v| v.as_bool()).unwrap_or(false)
                })
                .unwrap_or(false);
            println!(
                "paste-element: tab[{step}] sent={sent} edit_candidates={} focus_now={}",
                tab_cands.len(),
                focus_now,
            );
            for (index, cand) in tab_cands.iter().enumerate() {
                let (l, t, r, b) = cand.rect;
                println!(
                    "    tabcand[{index}] rect=({l},{t})-({r},{b}) {}",
                    cand.detail
                );
            }
            if let Some(index) = writable.iter().copied().find(|i| {
                let d = &tab_cands[*i].detail;
                !d.contains("买家")
                    && (d.contains("chat")
                        || d.contains("input")
                        || d.contains("message")
                        || d.contains("聊天")
                        || d.contains("输入"))
            }) {
                println!(
                    "paste-element: tab[{step}] composer materialized as cand[{index}], taking it"
                );
                elements.clear();
                elements.extend(tab_elems);
                candidates.clear();
                candidates.extend(tab_cands);
                break;
            }
        }
    }

    // 5) SendInput Ctrl+V（scancode 路径；0x0D 红线不动：本探针从不合成回车）。
    send_ctrl_v();
    std::thread::sleep(std::time::Duration::from_millis(1200));

    // 6) 重枚举读 Value 验证标记落框。
    let mut after: Vec<A11yCandidate> = Vec::new();
    let mut after_elements = Vec::new();
    let _ = uia_collect_editables(automation, root, &mut after, Some(&mut after_elements));
    let mut hit = false;
    for (index, (element, cand)) in after_elements.iter().zip(after.iter()).enumerate() {
        let value = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()
                .and_then(|p| p.CurrentValue().ok())
                .map(|v| v.to_string())
        };
        let Some(value) = value else {
            println!("  after[{index}] no-value-pattern {}", cand.detail);
            continue;
        };
        let contains = value.contains(marker);
        hit |= contains;
        let brief: String = value.chars().take(60).collect();
        println!("  after[{index}] value={brief:?} hit={contains}");
    }
    if hit {
        println!(
            "PASTE_ELEMENT: NON_COMPOSER_EVIDENCE marker matched an arbitrary Edit; not success"
        );
    } else {
        println!("PASTE_ELEMENT: MISS marker not found in any edit value");
    }
}

/// 元素级导航：在已物化树里按名称找元素并触发（Invoke → DoDefaultAction → Select）。
/// 纯 UIA 元素动作，零坐标。返回是否成功触发。
fn invoke_by_name(automation: &IUIAutomation, root: HWND, text: &str) -> bool {
    use windows::Win32::UI::Accessibility::{
        IUIAutomationInvokePattern, IUIAutomationLegacyIAccessiblePattern,
        IUIAutomationSelectionItemPattern, TreeScope_Descendants, UIA_InvokePatternId,
        UIA_LegacyIAccessiblePatternId, UIA_SelectionItemPatternId,
    };
    let Ok(root_el) = (unsafe { automation.ElementFromHandle(root) }) else {
        return false;
    };
    let Ok(condition) = (unsafe { automation.CreateTrueCondition() }) else {
        return false;
    };
    let Ok(all) = (unsafe { root_el.FindAll(TreeScope_Descendants, &condition) }) else {
        return false;
    };
    let length = unsafe { all.Length() }.unwrap_or(0);
    for index in 0..length.min(1500) {
        let Ok(element) = (unsafe { all.GetElement(index) }) else {
            continue;
        };
        let name = unsafe { element.CurrentName() }
            .map(|v| v.to_string())
            .unwrap_or_default();
        if !name.contains(text) {
            continue;
        }
        let control = unsafe { element.CurrentControlType() }.unwrap_or(UIA_CONTROLTYPE_ID_ZERO);
        let invoked = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                .ok()
                .and_then(|p| p.Invoke().ok())
        }
        .is_some();
        if invoked {
            println!("invoke_by_name: Invoke ok name={name:?} control={control:?}");
            return true;
        }
        let legacy = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                    UIA_LegacyIAccessiblePatternId,
                )
                .ok()
                .and_then(|p| p.DoDefaultAction().ok())
        }
        .is_some();
        if legacy {
            println!("invoke_by_name: DoDefaultAction ok name={name:?} control={control:?}");
            return true;
        }
        let selected = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                    UIA_SelectionItemPatternId,
                )
                .ok()
                .and_then(|p| p.Select().ok())
        }
        .is_some();
        if selected {
            println!("invoke_by_name: Select ok name={name:?} control={control:?}");
            return true;
        }
    }
    false
}

/// --wx-uia：微信 4.x（Qt mmui）条件式 UIA 触发实验。
/// 基线枚举只有窗口壳（Weixin + MMUIRenderSubWindowHW）。逐个试触发器，
/// 每步之间重新枚举计数，找出唤醒完整 mmui 树的最小动作：
///   1) 顶层 WM_GETOBJECT(OBJID_CLIENT)（Qt QAccessible 安装钩子）
///   2) MMUIRenderSubWindowHW 子窗 WM_GETOBJECT(OBJID_CLIENT)
///   3) UIA GetFocusedElement（焦点查询是否触发 provider 构建）
fn run_wx_uia(automation: &IUIAutomation, root: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_GETOBJECT};

    const OBJID_CLIENT: u32 = 0xFFFF_FFFC;
    let count = |automation: &IUIAutomation| -> i32 {
        let mut candidates: Vec<A11yCandidate> = Vec::new();
        uia_collect_editables(automation, root, &mut candidates, None)
    };

    println!("wx-uia: baseline descendants={}", count(automation));

    // 1) 顶层 WM_GETOBJECT。
    let honey_top = unsafe {
        SendMessageW(
            root,
            WM_GETOBJECT,
            Some(WPARAM(0)),
            Some(LPARAM(OBJID_CLIENT as i32 as isize)),
        )
    };
    println!(
        "wx-uia: top WM_GETOBJECT -> 0x{:X}, descendants={}",
        honey_top.0,
        count(automation)
    );
    std::thread::sleep(std::time::Duration::from_millis(1000));

    // 2) MMUIRenderSubWindowHW 子窗。
    let mut children = Vec::new();
    collect_children(root, &mut children);
    let sub: Vec<HWND> = children
        .into_iter()
        .filter(|child| hwnd_class(*child).contains("MMUI"))
        .collect();
    println!(
        "wx-uia: mmui child hwnds = {:?}",
        sub.iter().map(|h| h.0 as isize).collect::<Vec<_>>()
    );
    for child in &sub {
        let honey = unsafe {
            SendMessageW(
                *child,
                WM_GETOBJECT,
                Some(WPARAM(0)),
                Some(LPARAM(OBJID_CLIENT as i32 as isize)),
            )
        };
        println!(
            "wx-uia: mmui WM_GETOBJECT 0x{:X} -> 0x{:X}",
            child.0 as isize, honey.0
        );
    }
    println!("wx-uia: after mmui wake descendants={}", count(automation));
    std::thread::sleep(std::time::Duration::from_millis(1000));

    // 3) GetFocusedElement 焦点查询。
    let focused = unsafe { automation.GetFocusedElement() };
    match focused {
        Ok(element) => {
            let name = unsafe { element.CurrentName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            let class = unsafe { element.CurrentClassName() }
                .map(|v| v.to_string())
                .unwrap_or_default();
            println!("wx-uia: focused name={name:?} class={class:?}");
        }
        Err(error) => println!("wx-uia: GetFocusedElement err={error}"),
    }
    let final_total = count(automation);
    println!("wx-uia: final descendants={final_total}");
    if final_total > 2 {
        println!("wx-uia: materialized! dumping full tree...");
        dump_tree(automation, root);
    }
}

/// SendInput Ctrl+V（scancode 路径，仅此四事件，绝无回车）。
/// 合成单个 Tab 键（scancode 0x0F，非 0x0D，红线不动）。
/// --tabs 模式用：RWH 设焦后用键盘导航遍历 focusable 元素，促使 CEF
/// 为新获焦子树物化 a11y 节点（composer 首次获焦才会进树）。
fn send_tab() -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
        VIRTUAL_KEY,
    };
    const VK_TAB: VIRTUAL_KEY = VIRTUAL_KEY(0x09);
    const SC_TAB: u16 = 0x0F;
    let make = |flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_TAB,
                wScan: SC_TAB,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let down = KEYEVENTF_SCANCODE;
    let up = KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP;
    let events = [make(down), make(up)];
    unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) }
}

fn send_ctrl_v() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
        VIRTUAL_KEY, VK_CONTROL,
    };
    const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);
    const SC_CTRL: u16 = 0x1D;
    const SC_V: u16 = 0x2F;
    let make = |flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
                vk: VIRTUAL_KEY,
                sc: u16| {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: sc,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    };
    let down = KEYEVENTF_SCANCODE;
    let up = KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP;
    let events = [
        make(down, VK_CONTROL, SC_CTRL),
        make(down, VK_V, SC_V),
        make(up, VK_V, SC_V),
        make(up, VK_CONTROL, SC_CTRL),
    ];
    let sent = unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
    println!(
        "paste-element: sendinput ctrl+v sent={sent}/{}",
        events.len()
    );
}

fn variant_role_is_text(
    variant: &windows::Win32::System::Variant::VARIANT,
    role_system_text: i32,
) -> bool {
    use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VT_I4};
    let vt = unsafe { variant.Anonymous.Anonymous.vt };
    if vt != VT_DISPATCH {
        return false;
    }
    let disp = unsafe { variant.Anonymous.Anonymous.Anonymous.pdispVal.clone() };
    let Some(ref disp) = *disp else { return false };
    let Ok(acc) = disp.cast::<windows::Win32::UI::Accessibility::IAccessible>() else {
        return false;
    };
    // CHILDID_SELF：i4=0 的 VARIANT。
    let child = VARIANT::from(0i32);
    matches!(
        unsafe { acc.get_accRole(&child) },
        Ok(role) if unsafe { role.Anonymous.Anonymous.vt } == VT_I4
            && unsafe { role.Anonymous.Anonymous.Anonymous.lVal } == role_system_text
    )
}

// ---------------------------------------------------------------------------
// --a11y：无障碍激活后的元素级输入框巡检（D74 探针）
//
// 与既有电池的分工差异：既有 wake/msaa 只在最后做一层浅探，且所有 UIA 枚举
// 都跑在激活**之前**——「editable=0」可能是未激活的假阴性。本模式把顺序倒过来：
// 先激活、事件驱动等建树、再深枚举，并用画像锚点对找到的候选做命中判定。
// ---------------------------------------------------------------------------

/// 一个「可能是输入框」的候选元素：UIA 与 MSAA 两个视角共用。
#[derive(Debug)]
struct A11yCandidate {
    source: &'static str,
    detail: String,
    /// 屏幕物理像素 (left, top, right, bottom)。
    rect: (i32, i32, i32, i32),
}

/// MSAA 角色常量（oleacc 头文件值）。
const ROLE_SYSTEM_TEXT: i32 = 0x2A;
const ROLE_SYSTEM_COMBOBOX: i32 = 0x2E;

fn msaa_role_i4(variant: &windows::Win32::System::Variant::VARIANT) -> Option<i32> {
    use windows::Win32::System::Variant::VT_I4;
    (unsafe { variant.Anonymous.Anonymous.vt } == VT_I4)
        .then(|| unsafe { variant.Anonymous.Anonymous.Anonymous.lVal })
}

/// 角色是文本/组合框且带有效屏幕位置 → 收为候选。
fn msaa_maybe_candidate(
    acc: &windows::Win32::UI::Accessibility::IAccessible,
    child: &windows::Win32::System::Variant::VARIANT,
    depth: usize,
    out: &mut Vec<A11yCandidate>,
) {
    let Ok(role) = (unsafe { acc.get_accRole(child) }) else {
        return;
    };
    let Some(role) = msaa_role_i4(&role) else {
        return;
    };
    if role != ROLE_SYSTEM_TEXT && role != ROLE_SYSTEM_COMBOBOX {
        return;
    }
    let (mut x, mut y, mut w, mut h) = (0, 0, 0, 0);
    if unsafe { acc.accLocation(&mut x, &mut y, &mut w, &mut h, child) }.is_err()
        || w <= 0
        || h <= 0
    {
        return;
    }
    let name = unsafe { acc.get_accName(child) }
        .map(|v| v.to_string())
        .unwrap_or_default();
    let state = unsafe { acc.get_accState(child) }
        .ok()
        .and_then(|v| msaa_role_i4(&v))
        .unwrap_or(-1);
    let name_brief: String = name.chars().take(40).collect();
    out.push(A11yCandidate {
        source: "msaa",
        detail: format!("depth={depth} role=0x{role:X} state=0x{state:X} name={name_brief:?}"),
        rect: (x, y, x + w, y + h),
    });
}

/// MSAA 递归走树：dispatch 子节点递归 + 简单子元素按 child id 取角色/位置。
fn msaa_collect(
    acc: &windows::Win32::UI::Accessibility::IAccessible,
    depth: usize,
    out: &mut Vec<A11yCandidate>,
    budget: &mut usize,
) {
    use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VT_I4};
    use windows::Win32::UI::Accessibility::AccessibleChildren;
    if depth > 10 || *budget == 0 {
        return;
    }
    let count = unsafe { acc.accChildCount() }.unwrap_or(0);
    if count <= 0 {
        return;
    }
    let mut variants: Vec<VARIANT> = (0..count.clamp(1, 96))
        .map(|_| unsafe { std::mem::zeroed() })
        .collect();
    let mut obtained: i32 = 0;
    if unsafe { AccessibleChildren(acc, 0, &mut variants, &mut obtained) }.is_err() {
        return;
    }
    for variant in variants.iter().take(obtained.max(0) as usize) {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        match unsafe { variant.Anonymous.Anonymous.vt } {
            VT_DISPATCH => {
                let disp = unsafe { variant.Anonymous.Anonymous.Anonymous.pdispVal.clone() };
                let Some(ref disp) = *disp else { continue };
                let Ok(child_acc) = disp.cast::<windows::Win32::UI::Accessibility::IAccessible>()
                else {
                    continue;
                };
                let self_child = VARIANT::from(0i32);
                msaa_maybe_candidate(&child_acc, &self_child, depth, out);
                msaa_collect(&child_acc, depth + 1, out, budget);
            }
            VT_I4 => msaa_maybe_candidate(acc, variant, depth, out),
            _ => {}
        }
    }
}

/// 绝对屏幕坐标单击（探针用；与平台层锚点单击同一套 SendInput 虚拟桌面归一化）。
fn click_screen_point(x: i32, y: i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    let (vx, vy) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        )
    };
    let (vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if vw <= 1 || vh <= 1 {
        println!("click abort: virtual desktop degenerate");
        return;
    }
    let norm = |value: i32, origin: i32, size: i32| {
        (((value - origin) as i64 * 65_535 / (size - 1) as i64) as i32).clamp(0, 65_535)
    };
    let (nx, ny) = (norm(x, vx, vw), norm(y, vy, vh));
    let absolute = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
    let make = |flags| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: nx,
                dy: ny,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let events = [
        make(absolute | MOUSEEVENTF_MOVE),
        make(absolute | MOUSEEVENTF_LEFTDOWN),
        make(absolute | MOUSEEVENTF_LEFTUP),
    ];
    let sent = unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
    println!("click sendinput sent={sent}/{} at=({x},{y})", events.len());
}

/// UIA 子树里的可编辑候选采集。返回子树元素总数（激活是否生效的直接证据）。
/// `elements` 传 Some 时同步收集元素本体（--paste-element 的 SetFocus 需要）。
fn uia_collect_editables(
    automation: &IUIAutomation,
    hwnd: HWND,
    out: &mut Vec<A11yCandidate>,
    mut elements: Option<&mut Vec<windows::Win32::UI::Accessibility::IUIAutomationElement>>,
) -> i32 {
    use windows::Win32::UI::Accessibility::TreeScope_Descendants;
    let Ok(root) = (unsafe { automation.ElementFromHandle(hwnd) }) else {
        println!("uia root error");
        return -1;
    };
    let Ok(condition) = (unsafe { automation.CreateTrueCondition() }) else {
        println!("uia create-condition error");
        return -1;
    };
    let Ok(all) = (unsafe { root.FindAll(TreeScope_Descendants, &condition) }) else {
        println!("uia findall error");
        return -1;
    };
    let total = unsafe { all.Length() }.unwrap_or(0);
    for index in 0..total.min(1500) {
        let Ok(element) = (unsafe { all.GetElement(index) }) else {
            continue;
        };
        let Ok(control) = (unsafe { element.CurrentControlType() }) else {
            continue;
        };
        if control != UIA_EditControlTypeId && control != UIA_DocumentControlTypeId {
            continue;
        }
        let name = unsafe { element.CurrentName() }
            .map(|v| v.to_string())
            .unwrap_or_default();
        let aid = unsafe { element.CurrentAutomationId() }
            .map(|v| v.to_string())
            .unwrap_or_default();
        let class = unsafe { element.CurrentClassName() }
            .map(|v| v.to_string())
            .unwrap_or_default();
        let rect = unsafe { element.CurrentBoundingRectangle() }
            .map(|r| (r.left, r.top, r.right, r.bottom))
            .unwrap_or_default();
        let readonly = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()
                .and_then(|p| p.CurrentIsReadOnly().ok())
                .map(|v| v.as_bool())
        };
        let focusable = unsafe { element.CurrentIsKeyboardFocusable() }
            .map(|v| v.as_bool())
            .unwrap_or(false);
        let has_focus = unsafe { element.CurrentHasKeyboardFocus() }
            .map(|v| v.as_bool())
            .unwrap_or(false);
        let el_pid = unsafe { element.CurrentProcessId() }.unwrap_or(-1);
        let name_brief: String = name.chars().take(30).collect();
        if let Some(slot) = elements.as_deref_mut() {
            slot.push(element.clone());
        }
        out.push(A11yCandidate {
            source: "uia",
            detail: format!(
                "control={control:?} focusable={focusable} readonly={readonly:?} \
                 has_focus={has_focus} pid={el_pid} aid={aid:?} class={class:?} name={name_brief:?}"
            ),
            rect,
        });
    }
    total
}

fn run_semantic_probe(hwnd: HWND, automation: &IUIAutomation) {
    use platform::win32::Win32WindowActivator;
    use platform::{WindowActivator, WindowHandle};
    let pid = window_pid(hwnd);
    println!("semantic-probe target={:?} pid={pid} action=activate", hwnd);
    let activated = Win32WindowActivator
        .activate(WindowHandle(hwnd.0 as isize), 200, 120)
        .unwrap_or(false);
    if !activated || unsafe { GetForegroundWindow() } != hwnd {
        println!("semantic-probe candidate=none action=fallback reason=activation_failed");
        return;
    }
    let _ = a11y_activate_protocol(hwnd);
    let mut candidates = Vec::new();
    let mut elements = Vec::new();
    uia_collect_editables(automation, hwnd, &mut candidates, Some(&mut elements));
    let strict: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let d = c.detail.to_ascii_lowercase();
            (c.source == "uia"
                && d.contains("focusable=true")
                && (d.contains("name=\"编辑\"") || d.contains("name=\"edit\""))
                && d.contains("pid="))
            .then_some(i)
        })
        .collect();
    println!(
        "semantic-probe candidates={} strict={}",
        candidates.len(),
        strict.len()
    );
    if strict.len() != 1 {
        println!("semantic-probe action=none fallback=ambiguous_or_weak_evidence");
        return;
    }
    let i = strict[0];
    println!(
        "semantic-probe candidate={} action=SetFocus evidence={:?}",
        i, candidates[i].detail
    );
    let _ = unsafe { elements[i].SetFocus() };
    let verified = focused_element_info(automation, pid).0;
    println!(
        "semantic-probe post-focus caret_role=7 caret_name=编辑 target_root={} verified={verified}",
        unsafe { GetForegroundWindow() } == hwnd
    );
    if !verified {
        println!("semantic-probe fallback=focus_not_verified");
    }
}

// ---------------------------------------------------------------------------
// --corner-matrix：失焦→激活→窗口角落极限点单击→同进程即时语义复核（批量）。
//
// 用户线索：点击窗口边缘空白区（避开控件）可能让 IM 聊天输入框直接恢复聚焦。
// 跨命令测不准（命令间前台漂移 + HWND 失效），本模式把整链收进单进程原子循环：
//   SW_MINIMIZE 失焦 → 产品激活器拉回 → 解析候选点(客户区物理像素) →
//   归属守卫 → SendInput 单击 → GTI caret + MSAA role/name + UIA 焦点判决。
// 红线：默认零输入合成；仅 --paste-marker 时合成 Ctrl+V（无 0x0D），
//       粘贴前检查 composer 基线非空则跳过（不覆盖用户草稿），
//       粘贴确认落框后 UIA SetValue 清理本轮标记。
// ---------------------------------------------------------------------------

/// 解析候选点：`bl{d}`/`br{d}`/`tl{d}`/`tr{d}` = 距对应角内缩 d 物理像素；
/// 或 `x,y` 字面客户区物理像素。返回 (x, y)，基于当前客户区尺寸。
fn resolve_corner_point(spec: &str, w: i32, h: i32) -> Option<(i32, i32)> {
    let clamp_x = |x: i32| x.clamp(0, w - 1);
    let clamp_y = |y: i32| y.clamp(0, h - 1);
    if let Some(d) = spec.strip_prefix("bl").and_then(|s| s.parse::<i32>().ok()) {
        return Some((clamp_x(d), clamp_y(h - 1 - d)));
    }
    if let Some(d) = spec.strip_prefix("br").and_then(|s| s.parse::<i32>().ok()) {
        return Some((clamp_x(w - 1 - d), clamp_y(h - 1 - d)));
    }
    if let Some(d) = spec.strip_prefix("tl").and_then(|s| s.parse::<i32>().ok()) {
        return Some((clamp_x(d), clamp_y(d)));
    }
    if let Some(d) = spec.strip_prefix("tr").and_then(|s| s.parse::<i32>().ok()) {
        return Some((clamp_x(w - 1 - d), clamp_y(d)));
    }
    let (x, y) = spec.split_once(',')?;
    Some((
        clamp_x(x.trim().parse().ok()?),
        clamp_y(y.trim().parse().ok()?),
    ))
}

struct CornerVerdict {
    outcome: &'static str,
    role: Option<i32>,
    name: String,
    gti_focus_class: String,
    caret_screen: Option<(i32, i32)>,
    uia_note: String,
}

/// 点击 settle 后的即时判决：前台 → GTI caret → MSAA point 语义 → UIA 焦点兜底。
unsafe fn classify_after_click(root: HWND, pid: u32, automation: &IUIAutomation) -> CornerVerdict {
    let empty = || CornerVerdict {
        outcome: "internal_error",
        role: None,
        name: String::new(),
        gti_focus_class: String::new(),
        caret_screen: None,
        uia_note: String::new(),
    };
    if unsafe { GetForegroundWindow() } != root {
        return CornerVerdict {
            outcome: "not_foreground",
            role: None,
            name: String::new(),
            gti_focus_class: String::new(),
            caret_screen: None,
            uia_note: String::new(),
        };
    }
    let mut g = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetGUIThreadInfo(window_thread(root), &mut g) }.is_err() {
        return empty();
    }
    let gti_focus_class = hwnd_class(g.hwndFocus);
    let caret_rect = g.rcCaret;
    let has_caret = !g.hwndCaret.0.is_null() && caret_rect.bottom > caret_rect.top;
    let mut caret_screen = None;
    if has_caret {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::ClientToScreen;
        use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};
        let mut point = POINT {
            x: caret_rect.left,
            y: (i64::from(caret_rect.top) + i64::from(caret_rect.bottom)) as i32 / 2,
        };
        if unsafe { ClientToScreen(g.hwndCaret, &mut point) }.as_bool() {
            let owner = unsafe { WindowFromPoint(point) };
            if unsafe { GetAncestor(owner, GA_ROOT) } == unsafe { GetAncestor(root, GA_ROOT) } {
                caret_screen = Some((point.x, point.y));
                use windows::Win32::UI::Accessibility::AccessibleObjectFromPoint;
                let mut acc = None;
                let mut child = windows::Win32::System::Variant::VARIANT::default();
                if unsafe { AccessibleObjectFromPoint(point, &mut acc, &mut child) }.is_ok() {
                    if let Some(acc) = acc {
                        let role = unsafe { acc.get_accRole(&child) }
                            .ok()
                            .and_then(|v| msaa_role_i4(&v));
                        let name = unsafe { acc.get_accName(&child) }
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        if role == Some(7) {
                            let outcome = if name == "编辑" {
                                "composer_caret"
                            } else {
                                "caret_text_role"
                            };
                            return finish_verdict(
                                outcome,
                                role,
                                name,
                                gti_focus_class,
                                caret_screen,
                                automation,
                                root,
                            );
                        }
                        return finish_verdict(
                            "caret_other_role",
                            role,
                            name,
                            gti_focus_class,
                            caret_screen,
                            automation,
                            root,
                        );
                    }
                }
                return finish_verdict(
                    "caret_msaa_unreadable",
                    None,
                    String::new(),
                    gti_focus_class,
                    caret_screen,
                    automation,
                    root,
                );
            }
            return finish_verdict(
                "caret_owner_mismatch",
                None,
                String::new(),
                gti_focus_class,
                Some((point.x, point.y)),
                automation,
                root,
            );
        }
    }
    // 无 caret：按 UIA 焦点分类（买家账号=错误目标；RWH 获焦=部分信号）。
    let (already, info) = focused_element_info(automation, pid);
    let lower = info.to_lowercase();
    let outcome = if already && info.contains("买家") {
        "wrong_editable_buyer"
    } else if already
        && (lower.contains("聊天") || lower.contains("消息") || lower.contains("chat"))
    {
        "editable_chat_no_caret"
    } else if already {
        // 微信 4.x composer 是名字=会话名的可写 textfield（如「文件传输助手」），
        // 不含聊天关键词；只要可写 Edit 获焦且非买家账号就是强信号。
        "editable_focused_other"
    } else if gti_focus_class.contains("Chrome_RenderWidgetHost") {
        "rwh_focus_no_caret"
    } else {
        "foreground_only"
    };
    finish_verdict(
        outcome,
        None,
        String::new(),
        gti_focus_class,
        caret_screen,
        automation,
        root,
    )
}

/// 附加 UIA 树注记：可写 Edit/Document 候选数 + 当前 has_focus 的名字（≤3 个）。
fn finish_verdict(
    outcome: &'static str,
    role: Option<i32>,
    name: String,
    gti_focus_class: String,
    caret_screen: Option<(i32, i32)>,
    automation: &IUIAutomation,
    root: HWND,
) -> CornerVerdict {
    let mut candidates: Vec<A11yCandidate> = Vec::new();
    let _ = uia_collect_editables(automation, root, &mut candidates, None);
    let focused_names: Vec<String> = candidates
        .iter()
        .filter(|c| c.detail.contains("has_focus=true"))
        .filter_map(|c| {
            c.detail
                .split("name=")
                .nth(1)
                .map(|n| n.trim_matches('"').to_string())
        })
        .take(3)
        .collect();
    // GetFocusedElement 详情：区分「焦点已到但 caret 未物化」与「根本没聚焦」。
    let (_, focused_info) = focused_element_info(automation, window_pid(root));
    CornerVerdict {
        outcome,
        role,
        name,
        gti_focus_class,
        caret_screen,
        uia_note: format!(
            "focused_element={focused_info} editables={} focused={focused_names:?}",
            candidates.len()
        ),
    }
}

/// 读取当前 UIA 焦点元素及其 Value（粘贴验证 / 基线防呆）。
fn focused_value(automation: &IUIAutomation) -> Option<(IUIAutomationElement, String)> {
    let focused = unsafe { automation.GetFocusedElement() }.ok()?;
    let value = unsafe {
        focused
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .ok()
            .and_then(|p| p.CurrentValue().ok())
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    Some((focused, value))
}

/// 合成 Ctrl+<vk>（scancode 路径）+ 单键。仅用于已确认 composer 的清理（Ctrl+A/Backspace）。
fn send_ctrl_chord_then_key(chord_vk: u16, chord_sc: u16, key_vk: u16, key_sc: u16) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
        VIRTUAL_KEY, VK_CONTROL,
    };
    let make = |flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
                vk: VIRTUAL_KEY,
                sc: u16| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: sc,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let down = KEYEVENTF_SCANCODE;
    let up = KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP;
    let events = [
        make(down, VK_CONTROL, 0x1D),
        make(down, VIRTUAL_KEY(chord_vk), chord_sc),
        make(up, VIRTUAL_KEY(chord_vk), chord_sc),
        make(up, VK_CONTROL, 0x1D),
        make(down, VIRTUAL_KEY(key_vk), key_sc),
        make(up, VIRTUAL_KEY(key_vk), key_sc),
    ];
    unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
}

/// WM_NCHITTEST(0x0084)：返回 Some(ht)。仅 HTCLIENT=1 表示点击会作为客户区输入送达；
/// 角落数像素属于尺寸调整边框（HTBOTTOMLEFT 等），点击会被非客户区吞掉。
unsafe fn hit_test_client(root: HWND, screen: (i32, i32)) -> Option<isize> {
    const WM_NCHITTEST: u32 = 0x0084;
    let (sx, sy) = screen;
    let packed = ((sy as u16 as u32) << 16) | sx as u16 as u32;
    let lresult = unsafe {
        SendMessageTimeoutW(
            root,
            WM_NCHITTEST,
            WPARAM(0),
            LPARAM(packed as isize),
            SMTO_ABORTIFHUNG,
            500,
            None,
        )
    };
    (lresult.0 != 0).then_some(lresult.0)
}

/// 合成单个 VK 键（VK 路径，无 scancode；End/Backspace 等 EV_DELETE 类键必须带
/// 扩展位语义，scancode 路径会把 0x4F 当成 NumPad1——只用于探针内清理）。
fn send_key_vk(vk: u16) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    let make = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(
            &[make(Default::default()), make(KEYEVENTF_KEYUP)],
            std::mem::size_of::<INPUT>() as i32,
        )
    };
}

#[allow(clippy::too_many_arguments)]
fn run_corner_probe(
    root_value: isize,
    points: &[String],
    rounds_per_point: usize,
    settle_ms: u64,
    paste_marker: Option<&str>,
    defocus_peer: bool,
    cleanup_backspaces: u32,
    product_point: Option<(String, String)>,
) {
    // 物理像素几何：必须先于 UIA/COM 初始化切 PMv2（同 raw_snapshot::dispatch）。
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let automation = uia();

    let root = HWND(root_value as *mut core::ffi::c_void);
    let pid = window_pid(root);
    println!(
        "CORNER target hwnd={root_value} pid={pid} title={:?} defocus={} points={points:?} rounds={rounds_per_point} settle={settle_ms}ms paste={}",
        hwnd_title(root),
        if defocus_peer { "peer" } else { "minimize" },
        paste_marker.map(str::len).unwrap_or(0)
    );

    // peer 失焦的供体窗口：探针启动时的前台（通常为终端），要求非目标窗口。
    let donor = if defocus_peer {
        let fg = unsafe { GetForegroundWindow() };
        (fg != root && !fg.0.is_null()).then_some(fg)
    } else {
        None
    };
    if defocus_peer && donor.is_none() {
        println!("CORNER note=peer_donor_unavailable fallback=minimize");
    }

    use platform::win32::Win32WindowActivator;
    use platform::{WindowActivator, WindowHandle};
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetClientRect, WindowFromPoint, GA_ROOT,
    };

    for spec in points {
        for round in 1..=rounds_per_point {
            let started = Instant::now();
            if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(root)) }.as_bool() {
                println!("CORNER fatal=window_gone pt={spec}");
                return;
            }
            // 1) 失焦：peer=切到供体窗口（目标保持可见，贴近用户观察场景）；
            //    minimize=最小化（更重的状态重置）。两种方式都验证「激活≠内部焦点恢复」。
            if unsafe { GetForegroundWindow() } == root {
                let peer_done = match donor {
                    Some(d) => {
                        let my_tid = unsafe { GetCurrentThreadId() };
                        let d_tid = window_thread(d);
                        let t_tid = window_thread(root);
                        unsafe {
                            let _ = AttachThreadInput(my_tid, d_tid, true);
                            let _ = AttachThreadInput(my_tid, t_tid, true);
                            let _ = SetForegroundWindow(d);
                            let _ = AttachThreadInput(my_tid, t_tid, false);
                            let _ = AttachThreadInput(my_tid, d_tid, false);
                        }
                        let deadline = Instant::now() + std::time::Duration::from_millis(600);
                        while Instant::now() < deadline && unsafe { GetForegroundWindow() } == root
                        {
                            std::thread::sleep(std::time::Duration::from_millis(40));
                        }
                        (unsafe { GetForegroundWindow() }) != root
                    }
                    None => false,
                };
                if !peer_done {
                    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_MINIMIZE};
                    let _ = unsafe { ShowWindow(root, SW_MINIMIZE) };
                    let deadline = Instant::now() + std::time::Duration::from_millis(800);
                    while Instant::now() < deadline && unsafe { GetForegroundWindow() } == root {
                        std::thread::sleep(std::time::Duration::from_millis(40));
                    }
                }
            }
            // 2) 产品激活器拉回前台。
            let activated = Win32WindowActivator
                .activate(WindowHandle(root_value), 200, 120)
                .unwrap_or(false);
            let fg_ok = unsafe { GetForegroundWindow() } == root;
            if !fg_ok {
                println!(
                    "CORNER pt={spec} round={round} outcome=activate_failed activated={activated} us={}",
                    started.elapsed().as_micros()
                );
                continue;
            }
            std::thread::sleep(std::time::Duration::from_millis(120));

            // 3) 产品路径（--via-product "x|y"）：表达式求值 / HTCLIENT 守卫 /
            //    锚点单击 / 事件等待全部走平台层 Win32InputFocuser；
            //    探针只负责失焦循环与事后语义判决。E2E 统计以此为准。
            if let Some((expr_x, expr_y)) = product_point.as_ref() {
                use platform::win32::Win32InputFocuser;
                use platform::{FocusPlan, FocusStep, InputFocuser, InputPointExpr};
                let plan = FocusPlan {
                    steps: vec![FocusStep::InputPointClick],
                    anchor: None,
                    anchor_bottom: None,
                    input_point_expr: Some(InputPointExpr {
                        x: expr_x.clone(),
                        y: expr_y.clone(),
                    }),
                    caret_identity: None,
                };
                let report = Win32InputFocuser.focus_input(WindowHandle(root_value), &plan);
                let verdict = unsafe { classify_after_click(root, pid, &automation) };
                println!(
                    "CORNER pt=product({expr_x}|{expr_y}) round={round} attempts={:?} outcome={} role={:?} name={:?} gti_focus={:?} caret_screen={:?} {} us={}",
                    report.attempts,
                    verdict.outcome,
                    verdict.role,
                    verdict.name,
                    verdict.gti_focus_class,
                    verdict.caret_screen,
                    verdict.uia_note,
                    started.elapsed().as_micros()
                );
                let paste_ok = verdict.caret_screen.is_some()
                    || matches!(
                        verdict.outcome,
                        "composer_caret" | "editable_chat_no_caret" | "editable_focused_other"
                    );
                corner_paste_and_cleanup(
                    &automation,
                    spec,
                    round,
                    paste_marker,
                    paste_ok,
                    verdict.caret_screen,
                    cleanup_backspaces,
                );
                std::thread::sleep(std::time::Duration::from_millis(150));
                continue;
            }

            // 3a) 解析候选点（客户区物理像素）。
            let mut rc = windows::Win32::Foundation::RECT::default();
            unsafe {
                let _ = GetClientRect(root, &mut rc);
            }
            let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
            let Some((bx, by)) = resolve_corner_point(spec, cw, ch) else {
                println!("CORNER pt={spec} outcome=bad_point_spec");
                break;
            };

            // 3.5) WM_NCHITTEST 守卫：角点常落在尺寸调整边框（用户实测：光标变
            //      resize、点击被吞）。仅 HTCLIENT 才点；否则沿角向内缩 4px 重试。
            let inward = if spec.starts_with("tl") {
                (1, 1)
            } else if spec.starts_with("tr") {
                (-1, 1)
            } else if spec.starts_with("br") {
                (-1, -1)
            } else {
                (1, -1) // bl 与字面坐标
            };
            let (mut cx, mut cy) = (bx, by);
            let mut screen = POINT { x: cx, y: cy };
            let mut walk = String::new();
            let mut clickable = false;
            for step in 0..=6 {
                screen = POINT { x: cx, y: cy };
                unsafe {
                    let _ = ClientToScreen(root, &mut screen);
                }
                match unsafe { hit_test_client(root, (screen.x, screen.y)) } {
                    Some(1) => {
                        clickable = true;
                        break;
                    }
                    other if step < 6 => {
                        walk = format!("walk+{}", (step + 1) * 4);
                        cx = (cx + inward.0 * 4).clamp(0, cw - 1);
                        cy = (cy + inward.1 * 4).clamp(0, ch - 1);
                        let _ = other;
                    }
                    other => {
                        walk = format!("ht={other:?}");
                        break;
                    }
                }
            }
            if !clickable {
                println!("CORNER pt={spec} round={round} outcome=resize_border {walk}");
                continue;
            }
            let owner = unsafe { WindowFromPoint(screen) };
            if unsafe { GetAncestor(owner, GA_ROOT) } != unsafe { GetAncestor(root, GA_ROOT) } {
                println!(
                    "CORNER pt={spec} round={round} outcome=occluded screen=({},{})",
                    screen.x, screen.y
                );
                continue;
            }

            // 4) 单击 + 即时判决。
            click_screen_point(screen.x, screen.y);
            std::thread::sleep(std::time::Duration::from_millis(settle_ms));
            let verdict = unsafe { classify_after_click(root, pid, &automation) };
            println!(
                "CORNER pt={spec} round={round} asked=({bx},{by}) final=({cx},{cy}) {walk} screen=({},{}) outcome={} role={:?} name={:?} gti_focus={:?} caret_screen={:?} {} us={}",
                screen.x,
                screen.y,
                verdict.outcome,
                verdict.role,
                verdict.name,
                verdict.gti_focus_class,
                verdict.caret_screen,
                verdict.uia_note,
                started.elapsed().as_micros()
            );

            let paste_ok = verdict.caret_screen.is_some()
                || matches!(
                    verdict.outcome,
                    "composer_caret" | "editable_chat_no_caret" | "editable_focused_other"
                );
            corner_paste_and_cleanup(
                &automation,
                spec,
                round,
                paste_marker,
                paste_ok,
                verdict.caret_screen,
                cleanup_backspaces,
            );
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
    println!("CORNER done");
}

/// 粘贴验证 + 探针自清理（产品路径与手点路径共用的收尾段）。
/// 粘贴门禁由调用方按判决给出（caret 在目标内 / 可写 Edit 获焦非买家）。
#[allow(clippy::too_many_arguments)]
fn corner_paste_and_cleanup(
    automation: &IUIAutomation,
    spec: &str,
    round: usize,
    paste_marker: Option<&str>,
    paste_ok: bool,
    caret_screen: Option<(i32, i32)>,
    cleanup_backspaces: u32,
) {
    if let Some(marker) = paste_marker {
        if paste_ok {
            let Some((_element, baseline)) = focused_value(automation) else {
                println!("PASTE pt={spec} round={round} outcome=focused_unreadable");
                return;
            };
            if !baseline.is_empty() && !baseline.contains(marker) {
                println!(
                    "PASTE pt={spec} round={round} outcome=draft_present_skip value={}",
                    baseline.chars().take(40).collect::<String>()
                );
                return;
            }
            send_ctrl_v();
            std::thread::sleep(std::time::Duration::from_millis(900));
            let after = focused_value(automation);
            let hit = after
                .as_ref()
                .map(|(_, v)| v.contains(marker))
                .unwrap_or(false);
            println!(
                "PASTE pt={spec} round={round} outcome={} value={}",
                if hit { "hit_composer" } else { "miss" },
                after
                    .as_ref()
                    .map(|(_, v)| v.chars().take(60).collect::<String>())
                    .unwrap_or_default()
            );
            // 清理：确认落框才清。优先 UIA SetValue("")（零键盘事件），
            // 失败再退 Ctrl+A + Backspace（焦点已验证时选区只在 composer 内）。
            if hit {
                use windows::Win32::UI::Accessibility::IUIAutomationValuePattern;
                let mut cleaned = false;
                if let Some((element, _)) = after.as_ref() {
                    if let Ok(pattern) = unsafe {
                        element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    } {
                        cleaned = unsafe { pattern.SetValue(&windows::core::BSTR::new()) }.is_ok();
                    }
                }
                if !cleaned {
                    // Ctrl+A(0x41/0x1E) + Backspace(0x08/0x0E)
                    send_ctrl_chord_then_key(0x41, 0x1E, 0x08, 0x0E);
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
                let cleaned_now = focused_value(automation)
                    .map(|(_, v)| v.trim().is_empty())
                    .unwrap_or(false);
                println!("PASTE pt={spec} round={round} cleaned={cleaned_now}");
            }
            return;
        }
    }
    // 探针自清理：caret 在目标内时 End + N×Backspace（删粘贴实验残留的
    // 标记字符；用户原有草稿在前缀不受影响）。
    if cleanup_backspaces > 0 {
        if caret_screen.is_some() {
            const VK_END: u16 = 0x23;
            const VK_BACK: u16 = 0x08;
            send_key_vk(VK_END);
            for _ in 0..cleanup_backspaces {
                std::thread::sleep(std::time::Duration::from_millis(30));
                send_key_vk(VK_BACK);
            }
            println!("CLEANUP pt={spec} round={round} sent=end+{cleanup_backspaces}bs caret={caret_screen:?}");
        } else {
            println!("CLEANUP pt={spec} round={round} skipped=no_caret");
        }
    }
}

fn run_a11y_probe(
    hwnd: HWND,
    automation: &IUIAutomation,
    anchor: (f32, f32),
    click_index: Option<usize>,
) {
    use platform::win32::{Win32WindowActivator, Win32WindowEvents};
    use platform::{WindowActivator, WindowEventSource, WindowHandle};

    let pid = window_pid(hwnd);
    println!("=== a11y probe hwnd={} pid={pid} ===", hwnd.0 as isize);

    // 0) 先激活：最小化/后台窗口的客户区坐标没有意义（最小化态被挪到 -32000 附近），
    //    CEF 对不可见窗口也可能不建树。与产品顺序一致：激活 → 定位。
    let started = Instant::now();
    let activated = Win32WindowActivator
        .activate(WindowHandle(hwnd.0 as isize), 200, 120)
        .unwrap_or(false);
    println!(
        "activate ms={} ok={activated} foreground_now={}",
        started.elapsed().as_millis(),
        unsafe { GetForegroundWindow() } == hwnd
    );

    let mut rc = windows::Win32::Foundation::RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let client_w = rc.right - rc.left;
    let client_h = rc.bottom - rc.top;
    let mut origin = windows::Win32::Foundation::POINT { x: 0, y: 0 };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut origin);
    }
    println!(
        "client={client_w}x{client_h} origin=({},{})",
        origin.x, origin.y
    );

    // 1) 激活：CEF 渲染子窗口优先（web 内容的 a11y 树挂在渲染表面进程上），
    //    无渲染子窗口（Qt/自绘）则发顶层窗口。
    let mut children = Vec::new();
    collect_children(hwnd, &mut children);
    let render = children
        .iter()
        .copied()
        .find(|child| hwnd_class(*child) == "Chrome_RenderWidgetHostHWND");
    let wake_target = render.unwrap_or(hwnd);
    let mut wake_result: usize = 0;
    unsafe {
        SendMessageTimeoutW(
            wake_target,
            WM_GETOBJECT,
            WPARAM(0),
            LPARAM(0xFFFFFFFCu32 as i32 as isize), // OBJID_CLIENT，与 oleacc 同参
            SMTO_ABORTIFHUNG,
            800,
            Some(&mut wake_result),
        )
    };
    println!(
        "wake hwnd={} class={:?} is_render_child={} lresult_nonzero={}",
        wake_target.0 as isize,
        hwnd_class(wake_target),
        render.is_some(),
        wake_result != 0
    );

    // 2) 事件驱动等建树：渲染进程（或顶层进程）的任何已钩事件都会提前返回。
    //    泵只钩 FOREGROUND/FOCUS/LOCATIONCHANGE——建树可能一个都不发，
    //    CappedOut 不代表没建成，只代表「没等到证据」，枚举结果才是判决。
    let mut activity =
        Win32WindowEvents.await_process_activity(WindowHandle(wake_target.0 as isize));
    println!("build_wait outcome={:?}", activity.wait(900));

    // 3) UIA 深枚举（激活后）：总数是「激活是否生效」的直接证据。
    let mut candidates: Vec<A11yCandidate> = Vec::new();
    let total = uia_collect_editables(automation, hwnd, &mut candidates, None);
    println!(
        "uia total_descendants={total} editables={}",
        candidates.len()
    );
    if candidates.is_empty() {
        // 树可能还没建好：再给一个事件窗口重试一次（仍是事件驱动，非轮询）。
        let mut again =
            Win32WindowEvents.await_process_activity(WindowHandle(wake_target.0 as isize));
        println!("build_wait_retry outcome={:?}", again.wait(900));
        let total = uia_collect_editables(automation, hwnd, &mut candidates, None);
        println!(
            "uia retry total_descendants={total} editables={}",
            candidates.len()
        );
    }

    // 4) MSAA 递归走树（UIA 贫瘠时的后备视角；accLocation 同样是屏幕像素）。
    //    遍历全部 RWH（多渲染面的目标只走 wake_target 会漏另一个渲染面）。
    let mut rwh_walk: Vec<HWND> = {
        let mut children = Vec::new();
        collect_children(hwnd, &mut children);
        let list: Vec<HWND> = children
            .into_iter()
            .filter(|child| hwnd_class(*child) == "Chrome_RenderWidgetHostHWND")
            .collect();
        if list.is_empty() {
            vec![wake_target]
        } else {
            list
        }
    };
    for walk_root in rwh_walk.drain(..) {
        match access_client_accessible(walk_root, 0xFFFFFFFC) {
            Ok(acc) => {
                let mut budget = 400usize;
                msaa_collect(&acc, 0, &mut candidates, &mut budget);
                println!(
                    "msaa walk hwnd={:p} done, candidates_so_far={}",
                    walk_root.0,
                    candidates.len()
                );
            }
            Err(error) => println!("msaa root error={error}"),
        }
    }

    // 5) 画像锚点命中判定：当前锚点落在哪个候选里（或谁都没打中）。
    let point = (
        origin.x + (client_w as f32 * anchor.0).round() as i32,
        origin.y + (client_h as f32 * anchor.1).round() as i32,
    );
    println!(
        "anchor=({},{}) point=({},{}) y_from_bottom={}",
        anchor.0,
        anchor.1,
        point.0,
        point.1,
        client_h - (point.1 - origin.y)
    );
    for (index, candidate) in candidates.iter().enumerate() {
        let (l, t, r, b) = candidate.rect;
        let hit = point.0 >= l && point.0 < r && point.1 >= t && point.1 < b;
        println!(
            "  cand[{index}] {} rect=({l},{t})-({r},{b}) anchor_hit={hit} {}",
            candidate.source, candidate.detail
        );
    }

    // 6) 可选：点击候选中心做端到端验证（订阅先行，点击后等插入符事件）。
    if let Some(index) = click_index {
        let Some(candidate) = candidates.get(index) else {
            println!(
                "a11y-click index={index} 越界（共 {} 个候选）",
                candidates.len()
            );
            return;
        };
        let (l, t, r, b) = candidate.rect;
        let (cx, cy) = ((l + r) / 2, (t + b) / 2);
        let started = Instant::now();
        let activated = Win32WindowActivator
            .activate(WindowHandle(hwnd.0 as isize), 200, 120)
            .unwrap_or(false);
        println!(
            "a11y-click activate ms={} ok={activated}",
            started.elapsed().as_millis()
        );
        let mut surface = Win32WindowEvents.await_input_surface(WindowHandle(hwnd.0 as isize));
        click_screen_point(cx, cy);
        println!(
            "a11y-click cand[{index}] center=({cx},{cy}) caret_wait={:?}",
            surface.wait(500)
        );
        let (verified, info) = focused_element_info(automation, pid);
        println!("a11y-click after verified={verified} {info}");
        probe_guithreadinfo(hwnd, "_a11y_click");
    }
}

static TOP_WINDOW_COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn list_top_window(window: HWND, _lparam: LPARAM) -> BOOL {
    if !unsafe { IsWindowVisible(window) }.as_bool() {
        return BOOL(1);
    }
    let pid = window_pid(window);
    let exe = exe_name_of(pid);
    if !matches!(
        exe.as_str(),
        "Weixin.exe" | "WeChat.exe" | "AliWorkbench.exe" | "PddWorkbench.exe" | "Telegram.exe"
    ) {
        return BOOL(1);
    }
    TOP_WINDOW_COUNT.fetch_add(1, Ordering::Relaxed);
    println!(
        "hwnd={} pid={pid} exe={exe} class={:?} title={:?}",
        window.0 as isize,
        hwnd_class(window),
        hwnd_title(window)
    );
    BOOL(1)
}

/// 只做「激活 → 锚点点击 → 信号复测」，跳过探测电池（避免 UIA 误聚焦污染现场）。
/// 打印客户区尺寸与锚点的屏幕坐标，供截图比对定位是否准确。
/// `--anchor-bottom x_ratio,y_from_bottom` 走产品 anchor_bottom 代码路径。
fn run_click_only(
    hwnd: HWND,
    hwnd_value: isize,
    pid: u32,
    automation: &IUIAutomation,
    click: Option<(f32, f32)>,
    bottom: Option<(f32, f32)>,
) {
    use platform::win32::{Win32InputFocuser, Win32WindowActivator};
    use platform::{
        BottomUpAnchor, FocusAnchor, FocusPlan, FocusStep, InputFocuser, WindowActivator,
        WindowHandle,
    };

    let (x_ratio, y_ratio) = click.unwrap_or((0.5, 0.5));

    let started = Instant::now();
    let activated = Win32WindowActivator
        .activate(WindowHandle(hwnd_value), 200, 120)
        .unwrap_or(false);
    println!(
        "activate(product) ms={} activated={activated} foreground_now={}",
        started.elapsed().as_millis(),
        unsafe { GetForegroundWindow() } == hwnd
    );

    // 产品激活被前台锁拒绝时（实测：千牛钉住前台时激活微信失败），用
    // AttachThreadInput + SetForegroundWindow 解锁后重试一次。
    if unsafe { GetForegroundWindow() } != hwnd {
        use windows::Win32::System::Threading::GetCurrentThreadId;
        let my_tid = unsafe { GetCurrentThreadId() };
        let mut fg_pid = 0u32;
        let fg = unsafe { GetForegroundWindow() };
        let fg_tid = unsafe { GetWindowThreadProcessId(fg, Some(&mut fg_pid)) };
        let mut target_pid = 0u32;
        let target_tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut target_pid)) };
        unsafe {
            let _ = AttachThreadInput(my_tid, fg_tid, true);
            let _ = AttachThreadInput(my_tid, target_tid, true);
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(my_tid, fg_tid, false);
            let _ = AttachThreadInput(my_tid, target_tid, false);
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
        println!(
            "force-foreground(attach) now={}",
            unsafe { GetForegroundWindow() } == hwnd
        );
    }

    let mut rc = windows::Win32::Foundation::RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    };
    let client_w = rc.right - rc.left;
    let client_h = rc.bottom - rc.top;
    let clamp = |ratio: f32| ratio.clamp(0.02, 0.98);
    let plan = if let Some((bx, y_from_bottom)) = bottom {
        // 底部锚预览与产品 click_point_in_client 同式（物理像素 → 屏幕点）。
        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
        let scale = dpi.max(1) as f32 / 96.0;
        // 与产品 click_point_in_client 逐式对齐（偏移先取整），预览即落点。
        let offset = (y_from_bottom * scale).round() as i32;
        let mut point = windows::Win32::Foundation::POINT {
            x: rc.left + (client_w as f32 * clamp(bx)).round() as i32,
            y: rc.top + (client_h as f32 - offset as f32).round() as i32,
        };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut point);
        }
        println!(
            "click_point bottom_up=({bx},{y_from_bottom}) dpi={dpi} client={client_w}x{client_h} screen=({},{})",
            point.x, point.y
        );
        FocusPlan {
            steps: vec![FocusStep::AnchorClick],
            anchor: None,
            anchor_bottom: Some(BottomUpAnchor {
                x_ratio: bx,
                y_from_bottom,
            }),
            input_point_expr: None,
            caret_identity: None,
        }
    } else {
        let mut point = windows::Win32::Foundation::POINT {
            x: rc.left + (client_w as f32 * clamp(x_ratio)).round() as i32,
            y: rc.top + (client_h as f32 * clamp(y_ratio)).round() as i32,
        };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut point);
        }
        println!(
            "click_point ratio=({x_ratio},{y_ratio}) client={client_w}x{client_h} screen=({},{})",
            point.x, point.y
        );
        FocusPlan {
            steps: vec![FocusStep::AnchorClick],
            anchor: Some(FocusAnchor { x_ratio, y_ratio }),
            anchor_bottom: None,
            input_point_expr: None,
            caret_identity: None,
        }
    };
    let started = Instant::now();
    let report = Win32InputFocuser.focus_input(WindowHandle(hwnd_value), &plan);
    println!(
        "probe=anchor_click outcome={:?} attempts={:?} us={}",
        report.outcome,
        report.attempts,
        started.elapsed().as_micros()
    );

    for delay_ms in [0u64, 300, 700] {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        let (verified, info) = focused_element_info(automation, pid);
        println!("probe=p0_after_click t=+{delay_ms}ms verified={verified} {info}");
        probe_guithreadinfo(hwnd, "_after_click");
    }
    probe_attach_thread_input(hwnd, "_after_click");
}

fn dump_tree(automation: &IUIAutomation, hwnd: HWND) {
    use windows::Win32::UI::Accessibility::TreeScope_Descendants;
    let Ok(root) = (unsafe { automation.ElementFromHandle(hwnd) }) else {
        println!("dump error=elementfromhandle");
        return;
    };
    println!(
        "root name={:?} class={:?}",
        unsafe { root.CurrentName() }
            .map(|v| v.to_string())
            .unwrap_or_default(),
        unsafe { root.CurrentClassName() }
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    let Ok(condition) = (unsafe { automation.CreateTrueCondition() }) else {
        return;
    };
    let Ok(all) = (unsafe { root.FindAll(TreeScope_Descendants, &condition) }) else {
        return;
    };
    let length = unsafe { all.Length() }.unwrap_or(0);
    println!("dump elems={length}");
    for index in 0..length.min(300) {
        let Ok(element) = (unsafe { all.GetElement(index) }) else {
            continue;
        };
        let control = unsafe { element.CurrentControlType() }.unwrap_or(UIA_CONTROLTYPE_ID_ZERO);
        let name = unsafe { element.CurrentName() }
            .map(|v| v.to_string())
            .unwrap_or_default();
        let class = unsafe { element.CurrentClassName() }
            .map(|v| v.to_string())
            .unwrap_or_default();
        let value = unsafe {
            element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()
                .and_then(|p| p.CurrentValue().ok().map(|v| v.to_string()))
        }
        .unwrap_or_default();
        let brief: String = name.chars().take(40).collect();
        println!("  [{index}] control={control:?} class={class:?} name={brief:?} value={value:?}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--paste-element") {
        let marker = args
            .iter()
            .position(|a| a == "--marker")
            .and_then(|i| args.get(i + 1));
        eprintln!(
            "{}",
            raw_snapshot::paste_rejection(marker.map(String::as_str))
        );
        std::process::exit(2);
    }
    // Semantic restore is an explicit action mode and must bypass the raw snapshot dispatcher.
    if let Some(index) = args.iter().position(|arg| arg == "--semantic-probe") {
        let value = args
            .get(index + 1)
            .and_then(|v| v.parse::<isize>().ok())
            .expect("--semantic-probe 需要窗口句柄数字");
        let automation = uia();
        run_semantic_probe(HWND(value as *mut core::ffi::c_void), &automation);
        return;
    }
    // Corner matrix is an explicit action mode: activation + border-area click +
    // in-process semantic verdict. Optional paste stage requires --paste-marker.
    if let Some(index) = args.iter().position(|arg| arg == "--corner-matrix") {
        let value = args
            .get(index + 1)
            .and_then(|v| v.parse::<isize>().ok())
            .expect("--corner-matrix 需要窗口句柄数字");
        let points: Vec<String> = args
            .iter()
            .position(|arg| arg == "--points")
            .and_then(|i| args.get(i + 1))
            .map(|s| {
                s.split([' ', ';'])
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| {
                ["bl0", "bl2", "bl4", "tl0", "tl1", "tr0", "br0", "br2"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            });
        let rounds = args
            .iter()
            .position(|arg| arg == "--rounds")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(3);
        let settle = args
            .iter()
            .position(|arg| arg == "--settle")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(400);
        let marker = args
            .iter()
            .position(|arg| arg == "--paste-marker")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let defocus_peer = args
            .iter()
            .position(|arg| arg == "--defocus")
            .and_then(|i| args.get(i + 1))
            .map(|v| v == "peer")
            .unwrap_or(false);
        let cleanup_backspaces = args
            .iter()
            .position(|arg| arg == "--cleanup-backspaces")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        let product_point = args
            .iter()
            .position(|arg| arg == "--via-product")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.split_once('|'))
            .map(|(x, y)| (x.to_string(), y.to_string()));
        run_corner_probe(
            value,
            &points,
            rounds,
            settle,
            marker.as_deref(),
            defocus_peer,
            cleanup_backspaces,
            product_point,
        );
        return;
    }
    if raw_snapshot::dispatch(&args) {
        return;
    }
    // Old mutating batteries require an additional explicit opt-in. --hwnd now defaults to raw.
    if !args.iter().any(|arg| arg == "--legacy-mutations")
        && !args.iter().any(|arg| arg == "--list")
    {
        eprintln!("Use --raw-snapshot HWND (read-only), or explicitly opt into --legacy-mutations. Paste experiment is disabled.");
        std::process::exit(2);
    }
    let automation = uia();

    if args.iter().any(|arg| arg == "--list") {
        unsafe {
            let _ = EnumWindows(Some(list_top_window), LPARAM(0));
        }
        return;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--semantic-probe") {
        let value = args
            .get(index + 1)
            .and_then(|v| v.parse::<isize>().ok())
            .expect("--semantic-probe 需要窗口句柄数字");
        run_semantic_probe(HWND(value as *mut core::ffi::c_void), &automation);
        return;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--dump") {
        // 树转储：打印全子树的 control/name/class/value，用于寻找可稳定提取的
        // 目标身份元素（如微信多开时的账号昵称）。开发用，不进产品路径。
        let hwnd = args
            .get(index + 1)
            .and_then(|value| value.parse::<isize>().ok())
            .expect("--dump 需要窗口句柄数字");
        let hwnd = HWND(hwnd as *mut core::ffi::c_void);
        dump_tree(&automation, hwnd);
        return;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--a11y-activate") {
        // Chromium 渐进式 a11y 强制升级探针：真实 COM 属性调用 + 蜜罐 → 重枚举。
        let hwnd = args
            .get(index + 1)
            .and_then(|value| value.parse::<isize>().ok())
            .expect("--a11y-activate 需要窗口句柄数字（--list 先枚举）");
        run_a11y_activation(HWND(hwnd as *mut core::ffi::c_void));
        return;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--wx-uia") {
        // 微信 4.x（Qt mmui）条件式 UIA 触发实验：逐个发 WM_GETOBJECT（顶层 +
        // MMUIRenderSubWindowHW 子窗）再枚举计数，找出能唤醒完整 mmui 树的触发器。
        let hwnd = args
            .get(index + 1)
            .and_then(|value| value.parse::<isize>().ok())
            .expect("--wx-uia 需要窗口句柄数字");
        let root = HWND(hwnd as *mut core::ffi::c_void);
        run_wx_uia(&automation, root);
        return;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--activate-dump") {
        // 先拉前台 + 跑 a11y 激活协议再转储全树（后台窗口 renderer 不建树，
        // 且树失前台 ~10s 塌回，独立 --dump 看不到 web 树）。
        let hwnd = args
            .get(index + 1)
            .and_then(|value| value.parse::<isize>().ok())
            .expect("--activate-dump 需要窗口句柄数字");
        let root = HWND(hwnd as *mut core::ffi::c_void);
        {
            use platform::win32::Win32WindowActivator;
            use platform::{WindowActivator, WindowHandle};
            let ok = Win32WindowActivator
                .activate(WindowHandle(hwnd), 200, 120)
                .unwrap_or(false);
            println!(
                "activate-dump: activate ok={ok} foreground_now={}",
                unsafe { GetForegroundWindow() == root }
            );
        }
        let _ = a11y_activate_protocol(root);
        println!("activate-dump: waiting 4s for renderer tree build...");
        std::thread::sleep(std::time::Duration::from_millis(4000));
        dump_tree(&automation, root);
        return;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--paste-element") {
        // D75 元素级粘贴全链路（零坐标）：激活 → 选 composer → 设焦 → Ctrl+V → 读回验证。
        let hwnd = args
            .get(index + 1)
            .and_then(|value| value.parse::<isize>().ok())
            .expect("--paste-element 需要窗口句柄数字（--list 先枚举）");
        let mode = if args.iter().any(|arg| arg == "--no-setfocus") {
            PasteFocusMode::None
        } else if args.iter().any(|arg| arg == "--focus-rwh") {
            PasteFocusMode::Rwh
        } else if args.iter().any(|arg| arg == "--focus-doc") {
            PasteFocusMode::Doc
        } else {
            PasteFocusMode::Uia
        };
        let marker = args
            .iter()
            .position(|arg| arg == "--marker")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| "D75MARKER".into());
        let nav = args
            .iter()
            .position(|arg| arg == "--nav")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let tabs = args
            .iter()
            .position(|arg| arg == "--tabs")
            .and_then(|i| args.get(i + 1))
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        run_paste_element(
            HWND(hwnd as *mut core::ffi::c_void),
            hwnd,
            &automation,
            mode,
            &marker,
            nav.as_deref(),
            tabs,
        );
        return;
    }

    if let Some(index) = args.iter().position(|arg| arg == "--a11y") {
        // 激活后深探：唤醒 → 建树等待 → UIA/MSAA 双视角候选 → 锚点命中判定。
        let hwnd = args
            .get(index + 1)
            .and_then(|value| value.parse::<isize>().ok())
            .expect("--a11y 需要窗口句柄数字（--list 先枚举）");
        let anchor = args
            .iter()
            .position(|arg| arg == "--anchor")
            .and_then(|i| args.get(i + 1))
            .map(|value| {
                let (x, y) = value.split_once(',').expect("--anchor 需要 \"x,y\" 比例");
                (x.parse().expect("x 比例"), y.parse().expect("y 比例"))
            })
            .unwrap_or((0.49, 0.92));
        let click_index = args
            .iter()
            .position(|arg| arg == "--a11y-click")
            .and_then(|i| args.get(i + 1))
            .map(|value| value.parse::<usize>().expect("--a11y-click 需要候选序号"));
        run_a11y_probe(
            HWND(hwnd as *mut core::ffi::c_void),
            &automation,
            anchor,
            click_index,
        );
        return;
    }

    let hwnd_value = match args.iter().position(|arg| arg == "--hwnd") {
        Some(index) => args
            .get(index + 1)
            .and_then(|value| value.parse::<isize>().ok())
            .unwrap_or_else(|| panic!("--hwnd 需要窗口句柄数字")),
        None => panic!("缺少 --hwnd（或用 --list 先枚举）"),
    };
    let hwnd = HWND(hwnd_value as *mut core::ffi::c_void);
    let runs: usize = args
        .iter()
        .position(|arg| arg == "--runs")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    let click = args
        .iter()
        .position(|arg| arg == "--click")
        .and_then(|index| args.get(index + 1))
        .map(|value| {
            let (x, y) = value
                .split_once(',')
                .unwrap_or_else(|| panic!("--click 需要 \"x,y\" 比例"));
            (
                x.parse::<f32>().expect("x 比例"),
                y.parse::<f32>().expect("y 比例"),
            )
        });
    let click_only = args.iter().any(|arg| arg == "--click-only");
    let anchor_bottom = args
        .iter()
        .position(|arg| arg == "--anchor-bottom")
        .and_then(|i| args.get(i + 1))
        .map(|value| {
            let (x, y) = value
                .split_once(',')
                .expect("--anchor-bottom 需要 \"x_ratio,y_from_bottom\"");
            (
                x.parse::<f32>().expect("x 比例"),
                y.parse::<f32>().expect("y_from_bottom 逻辑像素"),
            )
        });

    let pid = window_pid(hwnd);
    let foreground = unsafe { GetForegroundWindow() };
    println!(
        "target hwnd={hwnd_value} pid={pid} exe={} class={:?} title={:?} visible={} foreground={}",
        exe_name_of(pid),
        hwnd_class(hwnd),
        hwnd_title(hwnd),
        unsafe { IsWindowVisible(hwnd) }.as_bool(),
        foreground == hwnd,
    );

    if click_only {
        run_click_only(hwnd, hwnd_value, pid, &automation, click, anchor_bottom);
        return;
    }

    for run in 1..=runs {
        println!("--- run {run} ---");
        // 用产品激活器（带确认循环与线程附加）把目标拉到前台——裸 SetForegroundWindow
        // 会被前台锁静默拒绝，探测结果会失真。
        if unsafe { GetForegroundWindow() } != hwnd {
            use platform::win32::Win32WindowActivator;
            use platform::{WindowActivator, WindowHandle};
            let started = Instant::now();
            let activated = Win32WindowActivator
                .activate(WindowHandle(hwnd_value), 200, 120)
                .unwrap_or(false);
            println!(
                "activate(product) ms={} activated={activated} foreground_now={}",
                started.elapsed().as_millis(),
                unsafe { GetForegroundWindow() } == hwnd
            );
        }
        let started = Instant::now();
        let (already, info) = focused_element_info(&automation, pid);
        println!(
            "probe=p0 us={} already_editable={already} {info}",
            started.elapsed().as_micros()
        );
        probe_guithreadinfo(hwnd, "");
        probe_attach_thread_input(hwnd, "");
        probe_uia_true(&automation, hwnd, pid);
        probe_uia_prop(&automation, hwnd, pid);
        probe_uia_first(&automation, hwnd, pid);
        probe_uia_children_scope(&automation, hwnd, pid);
    }

    println!("--- one-shot probes ---");
    probe_children(hwnd);
    probe_wake(hwnd, "-pre");
    probe_msaa(hwnd, "-pre");
    probe_wake(hwnd, "-post");
    probe_msaa(hwnd, "-post");

    if let Some((x, y)) = click {
        println!("--- anchor click ---");
        use platform::win32::Win32InputFocuser;
        use platform::{FocusAnchor, FocusPlan, FocusStep, InputFocuser, WindowHandle};
        let plan = FocusPlan {
            steps: vec![FocusStep::AnchorClick],
            anchor: Some(FocusAnchor {
                x_ratio: x,
                y_ratio: y,
            }),
            anchor_bottom: None,
            input_point_expr: None,
            caret_identity: None,
        };
        let started = Instant::now();
        let report = Win32InputFocuser.focus_input(WindowHandle(hwnd_value), &plan);
        println!(
            "probe=anchor_click outcome={:?} attempts={:?} us={}",
            report.outcome,
            report.attempts,
            started.elapsed().as_micros()
        );
        let (verified, info) = focused_element_info(&automation, pid);
        println!("probe=p0_after_click verified={verified} {info}");
        probe_guithreadinfo(hwnd, "_after_click");
        probe_attach_thread_input(hwnd, "_after_click");
    }
}
