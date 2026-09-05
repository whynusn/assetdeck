//! 真实 IM 上框验证程序。
//!
//! 复用产品路径 `TargetRoutingRuntime::paste`，不调用 `send()`/Enter。
//! 目标：证明“真实素材 -> 剪贴板 -> 真实 IM 窗口 -> Ctrl+V”这一环，
//! 而不是替换产品中的目标选择/注入逻辑。
//!
//! 所有等待都走 WinEvent 订阅（`WindowEventSource`），时间只作为兜底上限；
//! `--timings` 会把各阶段的事件等待结果和墙钟耗时打印出来，方便定位延迟来源。

use std::path::Path;
use std::time::Instant;

use platform::win32::{uia_focus_debug, uia_read_visible_text};
use platform::win32::{
    Win32Clipboard, Win32Focus, Win32ForegroundObserver, Win32Injector, Win32InputFocuser,
    Win32Readiness, Win32WindowActivator, Win32WindowEnumerator, Win32WindowEvents,
};
use platform::{
    ForegroundObserver, KeyInjector, WaitOutcome, WindowActivator, WindowEventSource, WindowHandle,
    KEY_UP,
};
use ui_viewmodels::{
    load_real_library, AssetId, TargetChoice, TargetNoticeTone, TargetRoutingRuntime,
    TargetRuntimeDeps,
};

const BUILTIN_PROFILES: &str = include_str!("../../../profiles/profiles.builtin.toml");

/// 快捷键切到目标 IM 会话后，等输入区出现/重建的兜底上限。
const OPEN_SHORTCUT_SETTLE_CAP_MS: u64 = 700;
/// 清空输入框后等目标应用处理完的兜底上限。
const CLEANUP_SETTLE_CAP_MS: u64 = 250;
/// `--tail-probe` 的静默判定：连续这么久收不到目标进程事件就认为渲染结束。
const TAIL_QUIET_MS: u64 = 300;

fn main() {
    if let Err(error) = run() {
        eprintln!("real-im-verify 失败: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    // 先收集参数，避免顺序敏感的迭代器解析：缺失的可选参数不再吞掉后面的实参。
    let args: Vec<String> = std::env::args().skip(1).collect();
    let library_root = required_arg(&args, "--library")?;
    let profile_id = required_arg(&args, "--profile")?;
    // 用户画像覆盖：验证「字段级覆盖」链路，也用于探测目标 IM 对某格式的粘贴行为。
    let user_profiles = match optional_arg(&args, "--user-profiles") {
        Some(path) => Some(
            std::fs::read_to_string(&path)
                .map_err(|error| format!("用户画像 {path} 读取失败: {error}"))?,
        ),
        None => None,
    };
    let hwnd = required_arg(&args, "--hwnd")?
        .parse::<isize>()
        .map_err(|_| "--hwnd 必须是窗口句柄数字".to_string())?;
    let inspect_only = args.iter().any(|arg| arg == "--inspect-only");
    let quiet = args.iter().any(|arg| arg == "--quiet");
    let cleanup_input = args.iter().any(|arg| arg == "--cleanup-input");
    let timings = args.iter().any(|arg| arg == "--timings");
    let tail_probe = args.iter().any(|arg| arg == "--tail-probe");
    // 纯巡检/清场模式不需要素材，缺省取第 0 条，避免为了清空输入框而被迫指定素材。
    let asset_index = match optional_arg(&args, "--asset-index") {
        Some(value) => value
            .parse::<u32>()
            .map_err(|_| "--asset-index 必须是数字".to_string())?,
        None if inspect_only => 0,
        None => return Err("缺少 --asset-index 参数".into()),
    };
    let asset_file = optional_arg(&args, "--asset-file");
    let probe_hwnd = optional_arg(&args, "--probe-hwnd")
        .map(|value| {
            value
                .parse::<isize>()
                .map_err(|_| "--probe-hwnd 必须是窗口句柄数字".to_string())
        })
        .transpose()?;
    let open_wechat_file_transfer = args.iter().any(|arg| arg == "--wechat-open-file-transfer");
    let wechat_safe_session = args.iter().any(|arg| arg == "--wechat-safe-session");
    let open_qianniu_chat = args.iter().any(|arg| arg == "--qianniu-open-chat");
    let open_qianniu_message = args.iter().any(|arg| arg == "--qianniu-open-message");

    let (_, resolver) = load_real_library(Path::new(&library_root))
        .map_err(|error| format!("真实库装载失败: {error}"))?;
    if asset_index as usize >= resolver.len() {
        return Err(format!(
            "asset-index {asset_index} 越界，当前库只有 {} 条",
            resolver.len()
        ));
    }
    let materialized = if let Some(file_name) = asset_file.as_deref() {
        resolver
            .materialize_by_file_name(file_name)
            .map_err(|error| format!("素材读取失败: {error}"))?
            .ok_or_else(|| format!("素材 {file_name} 不存在"))?
    } else {
        resolver
            .materialize(AssetId(asset_index))
            .map_err(|error| format!("素材读取失败: {error}"))?
            .ok_or_else(|| format!("素材 {asset_index} 不存在"))?
    };
    println!(
        "asset={} kind={:?} path={} bytes={}",
        asset_index,
        materialized.kind,
        materialized.source_path.display(),
        materialized.png_bytes.len()
    );

    let mut runtime = TargetRoutingRuntime::new(
        BUILTIN_PROFILES,
        user_profiles.as_deref(),
        win32_runtime_deps(timings),
    )
    .map_err(|e| e.to_string())?;
    runtime.poll().map_err(|e| e.to_string())?;
    runtime.open_picker();

    let available = runtime.snapshot().choices;
    let choice = available
        .iter()
        .find(|choice| {
            choice.binding.id.as_str() == profile_id
                && choice.binding.hwnd == Some(WindowHandle(hwnd))
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "未找到 {profile_id}@{hwnd}，当前候选:\n{}",
                list_choices(&available)
            )
        })?;
    let selection_key = selection_key_for(&choice);
    if !runtime.choose(selection_key.as_str()) {
        return Err(format!("目标选择失败: {selection_key}"));
    }
    println!(
        "target={} label={} hwnd={}",
        selection_key, choice.binding.label, hwnd
    );
    let activator = TimedWindowActivator {
        inner: Win32WindowActivator,
        timings,
    };
    match activator.activate(WindowHandle(hwnd), 200, 120) {
        Ok(true) => println!("activate ok"),
        Ok(false) => println!("activate timeout"),
        Err(error) => println!("activate error={error}"),
    }
    print_focus_debug(uia_focus_debug(WindowHandle(hwnd)), quiet);
    if let Some(probe_hwnd) = probe_hwnd {
        println!("probe native={probe_hwnd}");
        print_focus_debug(uia_focus_debug(WindowHandle(probe_hwnd)), false);
    }

    let events = Win32WindowEvents;
    if open_wechat_file_transfer {
        wait_for_surface_after(
            &events,
            WindowHandle(hwnd),
            OPEN_SHORTCUT_SETTLE_CAP_MS,
            "open-wechat",
            timings,
            || {
                inject_shortcut(&[
                    0x11,
                    0x12,
                    0x57,
                    0x57 | KEY_UP,
                    0x12 | KEY_UP,
                    0x11 | KEY_UP,
                ])
            },
        )?;
        println!("wechat shortcut=Ctrl+Alt+W");
        print_focus_debug(uia_focus_debug(WindowHandle(hwnd)), quiet);
    }
    if wechat_safe_session {
        match uia_focus_wechat_input(WindowHandle(hwnd)) {
            Ok(detail) => println!("wechat uia session={detail}"),
            Err(error) => println!("wechat uia session error={error}"),
        }
    }
    if open_qianniu_chat {
        wait_for_surface_after(
            &events,
            WindowHandle(hwnd),
            OPEN_SHORTCUT_SETTLE_CAP_MS,
            "open-qianniu-chat",
            timings,
            || {
                inject_shortcut(&[
                    0x11,
                    0x12,
                    0x58,
                    0x58 | KEY_UP,
                    0x12 | KEY_UP,
                    0x11 | KEY_UP,
                ])
            },
        )?;
        println!("qianniu shortcut=Ctrl+Alt+X");
        print_focus_debug(uia_focus_debug(WindowHandle(hwnd)), quiet);
    }
    if open_qianniu_message {
        wait_for_surface_after(
            &events,
            WindowHandle(hwnd),
            OPEN_SHORTCUT_SETTLE_CAP_MS,
            "open-qianniu-message",
            timings,
            || {
                inject_shortcut(&[
                    0x11,
                    0x12,
                    0x4D,
                    0x4D | KEY_UP,
                    0x12 | KEY_UP,
                    0x11 | KEY_UP,
                ])
            },
        )?;
        println!("qianniu shortcut=Ctrl+Alt+M");
        print_focus_debug(uia_focus_debug(WindowHandle(hwnd)), quiet);
    }

    if inspect_only {
        // 巡检模式也允许清场：把输入框恢复到干净状态而不注入任何素材。
        if cleanup_input {
            cleanup_target_input(&events, WindowHandle(hwnd), timings)?;
        }
        return Ok(());
    }

    let payload = materialized.as_payload();
    let paste_started = Instant::now();
    let notice = runtime.paste(&payload);
    let paste_ms = paste_started.elapsed().as_millis();
    if timings {
        println!("timing[paste] {paste_ms}ms");
    }
    print_notice(&notice);

    if tail_probe {
        let mut last_event_after_inject_ms: Option<u128> = None;
        loop {
            let mut activity = events.await_process_activity(WindowHandle(hwnd));
            match activity.wait(TAIL_QUIET_MS) {
                WaitOutcome::Observed { .. } => {
                    last_event_after_inject_ms = Some(paste_started.elapsed().as_millis());
                }
                _ => break,
            }
        }
        println!(
            "tail_probe payload_bytes={} last_event_after_inject_ms={:?} quiet_after_ms={TAIL_QUIET_MS}",
            materialized.png_bytes.len(),
            last_event_after_inject_ms
        );
    }
    match uia_read_visible_text(WindowHandle(hwnd)) {
        Ok(text) => {
            println!("readback:\n{text}");
            if !materialized.text.is_empty() {
                let sentinel = materialized.text.trim();
                if text.contains(sentinel) {
                    println!("READBACK_OK sentinel={sentinel}");
                } else {
                    println!("READBACK_MISSING sentinel={sentinel}");
                }
            }
        }
        Err(error) => println!("readback error={error}"),
    }

    if cleanup_input {
        cleanup_target_input(&events, WindowHandle(hwnd), timings)?;
    }

    match notice.tone {
        TargetNoticeTone::Error => Err("上框结果失败".into()),
        TargetNoticeTone::Warning if notice.text.contains("已复制") => Ok(()),
        _ => Ok(()),
    }
}

fn required_arg(args: &[String], name: &str) -> Result<String, String> {
    optional_arg(args, name).ok_or_else(|| format!("缺少 {name} 参数"))
}

/// 给真实验证程序的 `WindowActivator` 包一层墙钟计时，便于 `--timings` 定位延迟。
struct TimedWindowActivator {
    inner: Win32WindowActivator,
    timings: bool,
}

impl WindowActivator for TimedWindowActivator {
    fn activate(
        &self,
        window: WindowHandle,
        confirm_timeout_ms: u64,
        settle_ms: u64,
    ) -> platform::Result<bool> {
        let started = Instant::now();
        let result = self.inner.activate(window, confirm_timeout_ms, settle_ms);
        if self.timings {
            println!(
                "timing[activate] {}ms result={result:?}",
                started.elapsed().as_millis()
            );
        }
        result
    }
}

/// Win32 平台装配点：验证程序与产品二进制走同一套注入方式。
fn win32_runtime_deps(timings: bool) -> TargetRuntimeDeps {
    TargetRuntimeDeps {
        observer: Win32ForegroundObserver::new()
            .ok()
            .map(|observer| Box::new(observer) as Box<dyn ForegroundObserver>),
        enumerator: Box::new(Win32WindowEnumerator),
        clipboard: Box::new(Win32Clipboard),
        focus: Box::new(Win32Focus),
        injector: Box::new(Win32Injector),
        activator: Box::new(TimedWindowActivator {
            inner: Win32WindowActivator,
            timings,
        }),
        readiness: Box::new(Win32Readiness),
        focuser: Box::new(Win32InputFocuser),
        // 验证工具无对话框场景：占位实现恒「取消」（返回 None）。
        dialogs: Box::new(NoDialogs),
    }
}

/// 验证工具用占位对话框：任何选择都视同取消（工具不需要文件对话框）。
struct NoDialogs;

impl platform::FileDialogs for NoDialogs {
    fn pick_folder(&self, _title: &str) -> platform::Result<Option<std::path::PathBuf>> {
        Ok(None)
    }

    fn pick_open_file(
        &self,
        _title: &str,
        _filter: &str,
    ) -> platform::Result<Option<std::path::PathBuf>> {
        Ok(None)
    }

    fn pick_save_path(
        &self,
        _title: &str,
        _default_name: &str,
        _filter: &str,
    ) -> platform::Result<Option<std::path::PathBuf>> {
        Ok(None)
    }
}

fn optional_arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .filter(|value| !value.starts_with("--"))
        .cloned()
}

/// 清空目标输入框：Ctrl+A 全选后 Delete，绝不合成 Enter。
///
/// 也是「先订阅输入面事件，再注入动作，然后等到事件或兜底上限」，
/// 不再无条件睡满 250ms。
fn cleanup_target_input(
    events: &Win32WindowEvents,
    hwnd: WindowHandle,
    timings: bool,
) -> Result<(), String> {
    wait_for_surface_after(
        events,
        hwnd,
        CLEANUP_SETTLE_CAP_MS,
        "cleanup",
        timings,
        || {
            inject_shortcut(&[
                0x11,
                0x41,
                0x41 | KEY_UP,
                0x11 | KEY_UP,
                0x2E,
                0x2E | KEY_UP,
            ])
        },
    )?;
    println!("cleanup=Ctrl+A+Delete");
    Ok(())
}

/// 订阅目标窗口的输入面事件 → 执行动作 → 等到事件或兜底上限。
///
/// 这是事件驱动等待的标准形状：订阅必须先于动作，否则动作产生的早期事件会漏掉。
fn wait_for_surface_after(
    events: &Win32WindowEvents,
    hwnd: WindowHandle,
    cap_ms: u64,
    label: &str,
    timings: bool,
    action: impl FnOnce() -> Result<(), String>,
) -> Result<WaitOutcome, String> {
    let started = Instant::now();
    let mut surface = events.await_input_surface(hwnd);
    action()?;
    let outcome = surface.wait(cap_ms);
    if timings {
        println!(
            "timing[{label}] {outcome:?} total_ms={}",
            started.elapsed().as_millis()
        );
    }
    Ok(outcome)
}

fn inject_shortcut(keys: &[u16]) -> Result<(), String> {
    let mut injector = Win32Injector;
    injector
        .inject(keys)
        .map_err(|error| format!("快捷键注入失败: {error}"))
}

fn print_focus_debug(debug: String, quiet: bool) {
    if !quiet {
        println!("{debug}");
        return;
    }
    println!("{}", debug.lines().take(7).collect::<Vec<_>>().join("\n"));
}

// ---------------------------------------------------------------------------
// 微信会话切换（dev 电池专用，自 `platform` 迁入）：
// 平台层只保留通用能力；「文件传输助手」是开发验证的专属约定，不属于产品
// 目标路由，因此连同伴搜索 helper 一起收进本工具。
// ---------------------------------------------------------------------------

/// 切换微信会话后**最多**等输入区重建多久。
const WECHAT_SESSION_SWITCH_CAP_MS: u64 = 400;

/// 在目标窗口内选择「文件传输助手」并把焦点移到微信聊天输入框。
fn uia_focus_wechat_input(window: WindowHandle) -> Result<String, String> {
    use windows::Win32::UI::Accessibility::{
        IUIAutomationInvokePattern, IUIAutomationSelectionItemPattern, UIA_EditControlTypeId,
        UIA_InvokePatternId, UIA_SelectionItemPatternId,
    };
    let automation = platform_win32_automation()?;
    let mut raw_process_id = 0u32;
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
            windows::Win32::Foundation::HWND(window.0 as *mut core::ffi::c_void),
            Some(&mut raw_process_id),
        )
    };
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

/// 探针侧独立的 UIA 实例获取（与 platform 内缓存互不影响）。
fn platform_win32_automation() -> Result<
    windows::Win32::UI::Accessibility::IUIAutomation,
    String,
> {
    use windows::Win32::UI::Accessibility::CUIAutomation;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
    if !matches!(hr, S_OK | S_FALSE | RPC_E_CHANGED_MODE) {
        return Err(format!("CoInitializeEx failed: {hr:?}"));
    }
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.map_err(|e| e.to_string())
}

fn uia_search_from_focus(
    automation: &windows::Win32::UI::Accessibility::IUIAutomation,
    target_process_id: i32,
    predicate: impl Fn(&windows::Win32::UI::Accessibility::IUIAutomationElement) -> bool,
) -> Result<Option<windows::Win32::UI::Accessibility::IUIAutomationElement>, String> {
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
    walker: &windows::Win32::UI::Accessibility::IUIAutomationTreeWalker,
    current: &windows::Win32::UI::Accessibility::IUIAutomationElement,
    depth: u8,
    predicate: &impl Fn(&windows::Win32::UI::Accessibility::IUIAutomationElement) -> bool,
) -> Result<Option<windows::Win32::UI::Accessibility::IUIAutomationElement>, String> {
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

fn selection_key_for(choice: &TargetChoice) -> String {
    let window = choice
        .binding
        .hwnd
        .map_or_else(|| "dormant".to_string(), |hwnd| hwnd.0.to_string());
    format!("{}@{window}", choice.binding.id)
}

fn list_choices(choices: &[TargetChoice]) -> String {
    if choices.is_empty() {
        return "  (无候选：没有任何内置画像匹配到运行中的窗口)".to_string();
    }
    choices
        .iter()
        .map(|choice| {
            format!(
                "  {} hwnd={:?} label={}",
                choice.binding.id.as_str(),
                choice.binding.hwnd.map(|h| h.0),
                choice.binding.label
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_notice(notice: &ui_viewmodels::TargetPasteNotice) {
    match notice.tone {
        TargetNoticeTone::Success => println!("notice[success] {}", notice.text),
        TargetNoticeTone::Warning => println!("notice[warning] {}", notice.text),
        TargetNoticeTone::Error => println!("notice[error] {}", notice.text),
    }
}
