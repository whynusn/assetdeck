//! 多角度「取输入框焦点」API 真机探测（开发用，不进产品路径）。
//!
//! 红线守恒：本工具**绝不合成任何键盘事件**（尤其 0x0D），不写剪贴板；
//! 只做只读探测（枚举/caret/焦点查询）与 UIA SetFocus / 锚点单击两类落焦动作。
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
//!
//! 用法：
//!   focus_probe --list
//!   focus_probe --hwnd <N> [--runs 3] [--click "0.66,0.85"]

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use windows::core::{BOOL, Interface, PWSTR};
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
    IsWindowVisible, SendMessageTimeoutW, SetForegroundWindow, GUI_CARETBLINKING, GUITHREADINFO,
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
                .map(|p| p.CurrentIsReadOnly().map(|ro| !ro.as_bool()).unwrap_or(false))
                .unwrap_or(false)
        };
    (editable, format!("pid={pid} control={control:?} class={class:?} name={name:?}"))
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
            automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(control_id.0),
            )
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
            automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(control_id.0),
            )
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
    let mut info = GUITHREADINFO::default();
    info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
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
        let mut variants: Vec<VARIANT> =
            (0..count.clamp(1, 64)).map(|_| unsafe { std::mem::zeroed() }).collect();
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

fn variant_role_is_text(
    variant: &windows::Win32::System::Variant::VARIANT,
    role_system_text: i32,
) -> bool {
    use windows::Win32::System::Variant::{VT_DISPATCH, VT_I4, VARIANT};
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
// 驱动
// ---------------------------------------------------------------------------

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
fn run_click_only(
    hwnd: HWND,
    hwnd_value: isize,
    pid: u32,
    automation: &IUIAutomation,
    click: Option<(f32, f32)>,
) {
    use platform::win32::{Win32InputFocuser, Win32WindowActivator};
    use platform::{FocusAnchor, FocusPlan, FocusStep, InputFocuser, WindowActivator, WindowHandle};

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

    let plan = FocusPlan {
        steps: vec![FocusStep::AnchorClick],
        anchor: Some(FocusAnchor {
            x_ratio,
            y_ratio,
        }),
    };
    let started = Instant::now();
    let outcome = Win32InputFocuser.focus_input(WindowHandle(hwnd_value), &plan);
    println!(
        "probe=anchor_click outcome={outcome:?} us={}",
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
        unsafe { root.CurrentName() }.map(|v| v.to_string()).unwrap_or_default(),
        unsafe { root.CurrentClassName() }.map(|v| v.to_string()).unwrap_or_default(),
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
    let automation = uia();

    if args.iter().any(|arg| arg == "--list") {
        unsafe {
            let _ = EnumWindows(Some(list_top_window), LPARAM(0));
        }
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
        run_click_only(hwnd, hwnd_value, pid, &automation, click);
        return;
    }

    for run in 1..=runs {
        println!("--- run {run} ---");
        // 用产品激活器（带确认循环与线程附加）把目标拉到前台——裸 SetForegroundWindow
        // 会被前台锁静默拒绝，探测结果会失真。
        if unsafe { GetForegroundWindow() } != hwnd {
            use platform::{WindowActivator, WindowHandle};
            use platform::win32::Win32WindowActivator;
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
        };
        let started = Instant::now();
        let outcome = Win32InputFocuser.focus_input(WindowHandle(hwnd_value), &plan);
        println!(
            "probe=anchor_click outcome={outcome:?} us={}",
            started.elapsed().as_micros()
        );
        let (verified, info) = focused_element_info(&automation, pid);
        println!("probe=p0_after_click verified={verified} {info}");
        probe_guithreadinfo(hwnd, "_after_click");
        probe_attach_thread_input(hwnd, "_after_click");
    }
}
