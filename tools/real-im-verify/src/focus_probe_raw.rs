//! Isolated diagnostic path. No foreground activation, focus setters, input or clipboard.
//! Provider reads may lazily activate accessibility. Budgets are cooperative, not COM cancellation.
use std::time::{Duration, Instant};
use windows::core::{Result, BSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GUITHREADINFO,
};

const MAX_NODES: usize = 512;
const MAX_DEPTH: usize = 24;
const MAX_TEXT: usize = 256;

pub fn paste_rejection(marker: Option<&str>) -> &'static str {
    if marker.is_some_and(|s| s.trim().is_empty()) {
        "PASTE_ELEMENT: REJECT empty marker"
    } else {
        "PASTE_ELEMENT: DISABLED unidentified composer; arbitrary Edit marker match is NOT composer success"
    }
}

fn brief(value: Result<BSTR>) -> String {
    match value {
        Ok(s) => {
            let units: &[u16] = &s;
            let n = units.len().min(MAX_TEXT);
            format!(
                "{:?} truncated={}",
                String::from_utf16_lossy(&units[..n]),
                units.len() > n
            )
        }
        Err(e) => format!("ERROR({e})"),
    }
}
fn intersection(a: RECT, b: RECT) -> i64 {
    (i64::from(a.right.min(b.right)) - i64::from(a.left.max(b.left))).max(0)
        * (i64::from(a.bottom.min(b.bottom)) - i64::from(a.top.max(b.top))).max(0)
}
fn parse_region(text: &str) -> Option<RECT> {
    let v: Vec<i32> = text
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    if v.len() != 4 || v[2] <= v[0] || v[3] <= v[1] {
        return None;
    }
    Some(RECT {
        left: v[0],
        top: v[1],
        right: v[2],
        bottom: v[3],
    })
}

fn focus(a: &IUIAutomation, root: HWND, stage: &str) {
    let fg = unsafe { GetForegroundWindow() };
    println!("state={stage} foreground={:?} target={:?}", fg, root);
    for (label, tid) in [("foreground", 0), ("target", super::window_thread(root))] {
        let mut g = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let result = unsafe { GetGUIThreadInfo(tid, &mut g) };
        println!(
            "gti={label} result={result:?} active={:?} focus={:?} caret={:?} rect={:?} flags={:?}",
            g.hwndActive, g.hwndFocus, g.hwndCaret, g.rcCaret, g.flags
        );
    }
    match unsafe { a.GetFocusedElement() } {
        Ok(e) => describe(&e, "focus", None),
        Err(e) => println!("uia_focus ERROR({e})"),
    }
}
fn describe(e: &IUIAutomationElement, path: &str, region: Option<RECT>) {
    unsafe {
        println!("node path={path} control={:?} pid={:?} hwnd={:?} rect={:?} enabled={:?} focusable={:?} focused={:?} offscreen={:?}", e.CurrentControlType(), e.CurrentProcessId(), e.CurrentNativeWindowHandle(), e.CurrentBoundingRectangle(), e.CurrentIsEnabled(), e.CurrentIsKeyboardFocusable(), e.CurrentHasKeyboardFocus(), e.CurrentIsOffscreen());
        println!(
            "  name={} aid={} class={} framework={} provider={}",
            brief(e.CurrentName()),
            brief(e.CurrentAutomationId()),
            brief(e.CurrentClassName()),
            brief(e.CurrentFrameworkId()),
            brief(e.CurrentProviderDescription())
        );
        if let Some(region) = region {
            println!(
                "  composer_region_intersection_diagnostic_only={:?}",
                e.CurrentBoundingRectangle()
                    .map(|r| intersection(r, region))
            );
        }
        match e.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
            Ok(p) => println!(
                "  Value readonly={:?} text={}",
                p.CurrentIsReadOnly(),
                brief(p.CurrentValue())
            ),
            Err(err) => println!("  Value unavailable_or_error={err}"),
        }
        match e.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) {
            Ok(p) => println!(
                "  Text text={}",
                brief(p.DocumentRange().and_then(|r| r.GetText(MAX_TEXT as i32)))
            ),
            Err(err) => println!("  Text unavailable_or_error={err}"),
        }
        match e.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
            UIA_LegacyIAccessiblePatternId,
        ) {
            Ok(p) => println!(
                "  Legacy role={:?} state={:?} name={} value={} description={}",
                p.CurrentRole(),
                p.CurrentState(),
                brief(p.CurrentName()),
                brief(p.CurrentValue()),
                brief(p.CurrentDescription())
            ),
            Err(err) => println!("  Legacy unavailable_or_error={err}"),
        }
    }
}
struct Walk<'a> {
    walker: IUIAutomationTreeWalker,
    count: usize,
    start: Instant,
    region: Option<RECT>,
    label: &'a str,
}
impl Walk<'_> {
    fn visit(&mut self, e: IUIAutomationElement, path: String, depth: usize) {
        if self.count >= MAX_NODES || self.start.elapsed() >= Duration::from_secs(12) {
            println!("TRUNCATED {} path={path} node_or_time_budget", self.label);
            return;
        }
        self.count += 1;
        describe(&e, &path, self.region);
        if depth >= MAX_DEPTH {
            println!("TRUNCATED path={path} depth_budget");
            return;
        }
        let mut child = unsafe { self.walker.GetFirstChildElement(&e) };
        let mut i = 0;
        loop {
            let current = match child {
                Ok(c) => c,
                Err(err) => {
                    println!("walk_end_or_error parent={path} next={i} error={err}");
                    break;
                }
            };
            if self.count >= MAX_NODES || self.start.elapsed() >= Duration::from_secs(12) {
                println!("TRUNCATED parent={path} remaining_children");
                break;
            }
            self.visit(current.clone(), format!("{path}/{i}"), depth + 1);
            child = unsafe { self.walker.GetNextSiblingElement(&current) };
            i += 1;
        }
    }
}
// Use only the existing typed MSAA bindings on IA2 objects. No guessed IA2 text/attribute ABI.
// Bounded inherited-interface reads are not proof that the full IA2 tree was inspected.
fn inherited_msaa(
    acc: &IAccessible,
    child: &windows::Win32::System::Variant::VARIANT,
    path: &str,
    depth: usize,
    budget: &mut usize,
    start: Instant,
) {
    use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH, VT_I4};
    if *budget == 0 || start.elapsed() >= Duration::from_secs(6) {
        println!("TRUNCATED ia2_msaa path={path} node_or_time_budget");
        return;
    }
    *budget -= 1;
    unsafe {
        println!(
            "ia2_msaa path={path} role={:?} state={:?} name={} value={} description={}",
            acc.get_accRole(child).map(|v| super::msaa_role_i4(&v)),
            acc.get_accState(child).map(|v| super::msaa_role_i4(&v)),
            brief(acc.get_accName(child)),
            brief(acc.get_accValue(child)),
            brief(acc.get_accDescription(child))
        );
        if depth >= 12 {
            println!("TRUNCATED ia2_msaa path={path} depth_budget");
            return;
        }
        // Simple child IDs are properties on the parent, not independent tree roots.
        if super::msaa_role_i4(child) != Some(0) {
            return;
        }
        let count = match acc.accChildCount() {
            Ok(n) => n.max(0),
            Err(e) => {
                println!("ia2_msaa path={path} childcount_error={e}");
                return;
            }
        };
        if count == 0 {
            return;
        }
        let cap = (count as usize).min(64).min(*budget);
        if cap == 0 {
            println!("TRUNCATED ia2_msaa path={path} children");
            return;
        }
        let mut children = vec![VARIANT::default(); cap];
        let mut obtained = 0;
        let hr = AccessibleChildren(acc, 0, &mut children, &mut obtained);
        println!("ia2_msaa path={path} children={count} obtained={obtained} hr={hr:?}");
        if hr.is_err() {
            return;
        }
        if count as usize > cap {
            println!("TRUNCATED ia2_msaa path={path} child_cap={cap}");
        }
        for (i, item) in children.iter().take(obtained.max(0) as usize).enumerate() {
            if *budget == 0 || start.elapsed() >= Duration::from_secs(6) {
                println!("TRUNCATED ia2_msaa path={path} remaining_children");
                break;
            }
            let next = format!("{path}/{i}");
            match item.Anonymous.Anonymous.vt {
                VT_DISPATCH => {
                    use windows::core::Interface;
                    let dispatch = &*item.Anonymous.Anonymous.Anonymous.pdispVal;
                    if let Some(dispatch) = dispatch {
                        match dispatch.cast::<IAccessible>() {
                            Ok(a) => inherited_msaa(
                                &a,
                                &VARIANT::from(0i32),
                                &next,
                                depth + 1,
                                budget,
                                start,
                            ),
                            Err(e) => println!("ia2_msaa path={next} cast_error={e}"),
                        }
                    }
                }
                VT_I4 => inherited_msaa(acc, item, &next, depth + 1, budget, start),
                other => println!("ia2_msaa path={next} unsupported_variant={other:?}"),
            }
        }
    }
}

// QueryService availability is NOT evidence of composer text access.
fn extended_interfaces(root: HWND) {
    use windows::core::{IUnknown, IUnknown_Vtbl, Interface, GUID, HRESULT};
    #[repr(C)]
    struct ServiceVtbl {
        base: IUnknown_Vtbl,
        query_service: unsafe extern "system" fn(
            *mut core::ffi::c_void,
            *const GUID,
            *const GUID,
            *mut *mut core::ffi::c_void,
        ) -> HRESULT,
    }
    const SERVICE: GUID = GUID::from_u128(0x6d5140c1_7436_11ce_8034_00aa006009fa);
    const IA2: GUID = GUID::from_u128(0xe89f726e_c4f4_4c19_bb19_b647d7fa8478);
    const SIMPLE_DOM: GUID = GUID::from_u128(0x1814ceeb_49e2_407f_af99_fa755a7d2607);
    let mut targets = vec![root];
    super::collect_children(root, &mut targets);
    let total = targets.len();
    for hwnd in targets.into_iter().take(32) {
        let acc = match super::access_client_accessible(hwnd, 0xFFFF_FFFC) {
            Ok(acc) => acc,
            Err(err) => {
                println!("extended hwnd={hwnd:?} accessible_error={err}");
                continue;
            }
        };
        unsafe {
            let mut service_raw = core::ptr::null_mut();
            let hr = acc.query(&SERVICE, &mut service_raw);
            println!("extended hwnd={hwnd:?} IServiceProvider={hr:?}");
            if hr.is_err() || service_raw.is_null() {
                continue;
            }
            let service = IUnknown::from_raw(service_raw);
            let vt = &**(service.as_raw() as *const *const ServiceVtbl);
            for (name, sid, iid) in [
                ("IA2", IAccessible::IID, IA2),
                ("ISimpleDOMNode", SIMPLE_DOM, SIMPLE_DOM),
            ] {
                let mut raw = core::ptr::null_mut();
                let hr = (vt.query_service)(service.as_raw(), &sid, &iid, &mut raw);
                println!("extended hwnd={hwnd:?} query={name} hr={hr:?} nonnull={} availability_only=true", !raw.is_null());
                if hr.is_ok() && !raw.is_null() {
                    let object = IUnknown::from_raw(raw);
                    if name == "IA2" {
                        match object.cast::<IAccessible>() {
                            Ok(a) => {
                                let mut budget = 128;
                                inherited_msaa(
                                    &a,
                                    &windows::Win32::System::Variant::VARIANT::from(0i32),
                                    &format!("{hwnd:?}"),
                                    0,
                                    &mut budget,
                                    Instant::now(),
                                );
                                println!("ia2_msaa hwnd={hwnd:?} visited={} complete_not_guaranteed=true ia2_specific_text_attributes_not_read=true", 128 - budget);
                            }
                            Err(e) => {
                                println!("ia2_msaa hwnd={hwnd:?} inherited_interface_error={e}")
                            }
                        }
                    }
                }
            }
        }
    }
    if total > 32 {
        println!("TRUNCATED extended native_hwnds total={total} cap=32");
    }
}

fn snapshot(a: &IUIAutomation, root: HWND, region: Option<RECT>, label: &str) {
    focus(a, root, &format!("{label}-before"));
    match (unsafe { a.RawViewWalker() }, unsafe {
        a.ElementFromHandle(root)
    }) {
        (Ok(walker), Ok(e)) => {
            let mut w = Walk {
                walker,
                count: 0,
                start: Instant::now(),
                region,
                label,
            };
            w.visit(e, "root".into(), 0);
            println!(
                "snapshot={label} visited={} elapsed_ms={} complete_not_guaranteed=true",
                w.count,
                w.start.elapsed().as_millis()
            );
        }
        (walker, root) => println!(
            "snapshot ERROR walker={:?} root={:?}",
            walker.err(),
            root.err()
        ),
    }
    focus(a, root, &format!("{label}-after"));
}

fn caret_client_point(rect: RECT) -> Option<windows::Win32::Foundation::POINT> {
    if rect.bottom <= rect.top || rect.right < rect.left {
        return None;
    }
    Some(windows::Win32::Foundation::POINT {
        x: rect.left,
        y: ((i64::from(rect.top) + i64::from(rect.bottom)) / 2) as i32,
    })
}

// Query the real native insertion point rather than deriving a point from window size.
// This is identity evidence only: a caret can belong to search, not the composer.
fn native_caret(a: &IUIAutomation, root: HWND) {
    use windows::core::Interface;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH};
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};
    unsafe {
        if GetForegroundWindow() != root {
            println!("NATIVE_CARET REJECT target_not_foreground");
            return;
        }
        let mut g = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if let Err(e) = GetGUIThreadInfo(super::window_thread(root), &mut g) {
            println!("NATIVE_CARET REJECT gti={e}");
            return;
        }
        println!(
            "native focus={:?} caret={:?} client_rect={:?} flags={:?}",
            g.hwndFocus, g.hwndCaret, g.rcCaret, g.flags
        );
        for (label, hwnd, object_id) in [
            ("root_client", root, 0xFFFF_FFFC),
            ("root_window", root, 0),
            ("native_caret", g.hwndCaret, 0xFFFF_FFF8),
        ] {
            if hwnd.0.is_null() {
                continue;
            }
            match super::access_client_accessible(hwnd, object_id) {
                Ok(acc) => {
                    let mut budget = 256;
                    inherited_msaa(
                        &acc,
                        &VARIANT::from(0i32),
                        label,
                        0,
                        &mut budget,
                        Instant::now(),
                    );
                    match acc.accFocus() {
                        Ok(v) if v.Anonymous.Anonymous.vt == VT_DISPATCH => {
                            if let Some(d) = &*v.Anonymous.Anonymous.Anonymous.pdispVal {
                                if let Ok(focused) = d.cast::<IAccessible>() {
                                    inherited_msaa(
                                        &focused,
                                        &VARIANT::from(0i32),
                                        &format!("{label}/accFocus"),
                                        0,
                                        &mut 32,
                                        Instant::now(),
                                    );
                                }
                            }
                        }
                        Ok(v) => println!(
                            "{label} accFocus child_id={:?} vt={:?}",
                            super::msaa_role_i4(&v),
                            v.Anonymous.Anonymous.vt
                        ),
                        Err(e) => println!("{label} accFocus_error={e}"),
                    }
                }
                Err(e) => println!("{label} accessible_error={e}"),
            }
        }
        let Some(mut point) = caret_client_point(g.rcCaret).filter(|_| !g.hwndCaret.0.is_null())
        else {
            println!("NATIVE_CARET REJECT no_live_caret_rectangle");
            return;
        };
        if !ClientToScreen(g.hwndCaret, &mut point).as_bool() {
            println!("NATIVE_CARET REJECT coordinate_conversion_failed");
            return;
        }
        let owner = WindowFromPoint(point);
        println!("native caret_screen={point:?} point_owner={owner:?}");
        if GetForegroundWindow() != root || GetAncestor(owner, GA_ROOT) != root {
            println!("NATIVE_CARET REJECT foreground_or_point_owner_changed");
            return;
        }
        match a.ElementFromPoint(point) {
            Ok(e) => describe(&e, "native-caret-hit", None),
            Err(e) => println!("native uia_point_error={e}"),
        }
        let mut acc = None;
        let mut child = VARIANT::default();
        match AccessibleObjectFromPoint(point, &mut acc, &mut child) {
            Ok(()) => {
                if let Some(acc) = acc {
                    inherited_msaa(
                        &acc,
                        &child,
                        "native-caret-msaa-hit",
                        0,
                        &mut 64,
                        Instant::now(),
                    );
                }
            }
            Err(e) => println!("native msaa_point_error={e}"),
        }
        if let Ok(acc) = super::access_client_accessible(root, 0xFFFF_FFFC) {
            match acc.accHitTest(point.x, point.y) {
                Ok(v) if v.Anonymous.Anonymous.vt == VT_DISPATCH => {
                    if let Some(d) = &*v.Anonymous.Anonymous.Anonymous.pdispVal {
                        if let Ok(hit) = d.cast::<IAccessible>() {
                            inherited_msaa(
                                &hit,
                                &VARIANT::from(0i32),
                                "root-accHitTest",
                                0,
                                &mut 64,
                                Instant::now(),
                            );
                        }
                    }
                }
                Ok(v) => inherited_msaa(
                    &acc,
                    &v,
                    "root-accHitTest-child",
                    0,
                    &mut 64,
                    Instant::now(),
                ),
                Err(e) => println!("native accHitTest_error={e}"),
            }
        }
        println!("NATIVE_CARET complete read_only=true composer_identity_not_assumed=true");
    }
}

pub fn dispatch(args: &[String]) -> bool {
    let Some(i) = args.iter().position(|s| {
        s == "--raw-snapshot" || (s == "--hwnd" && !args.iter().any(|a| a == "--legacy-mutations"))
    }) else {
        return false;
    };
    // Must precede UIA/COM initialization: otherwise HWND/MSAA rectangles are DPI virtualized.
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    let previous_dpi =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if previous_dpi.0.is_null() {
        eprintln!("raw snapshot rejected: could not establish physical-screen DPI context");
        return true;
    }
    println!("dpi_context=PER_MONITOR_AWARE_V2 geometry=physical_screen; GTI rcCaret remains caret-window client coordinates");
    let value = args
        .get(i + 1)
        .and_then(|v| v.parse::<isize>().ok())
        .filter(|v| *v != 0)
        .expect("snapshot requires nonzero decimal HWND");
    let root = HWND(value as *mut core::ffi::c_void);
    if super::exe_name_of(super::window_pid(root)) != "AliWorkbench.exe" {
        eprintln!("raw snapshot rejected: target must be live Qianniu AliWorkbench.exe");
        return true;
    }
    let region = args.iter().position(|a| a == "--composer-region").map(|i| {
        parse_region(args.get(i + 1).expect("region missing"))
            .expect("region requires left,top,right,bottom in UIA screen coordinates")
    });
    println!("RAW read_only=true max_nodes={MAX_NODES} max_depth={MAX_DEPTH} text_utf16_cap={MAX_TEXT} cooperative_budget_ms=12000; provider COM calls may exceed budget; text at cap may be truncated; values may contain private data");
    let a = super::uia();
    if args.iter().any(|s| s == "--native-caret") {
        native_caret(&a, root);
        return true;
    }
    snapshot(&a, root, region, "baseline");
    if args
        .iter()
        .any(|s| s == "--qt-native-access" || s == "--cef-access" || s == "--extended-interfaces")
    {
        use platform::win32::Win32WindowEvents;
        use platform::WindowHandle;
        // Subscribe BEFORE any trigger. Events are activity, not composer or tree-completeness proof.
        let mut wait = Win32WindowEvents.await_process_activity(WindowHandle(value));
        println!("subscription_created_before_trigger=true event_scope=target_process focus_foreground_location_only");
        if args.iter().any(|s| s == "--qt-native-access") {
            match super::access_client_accessible(root, 0xFFFF_FFFC) {
                Ok(acc) => {
                    let child = windows::Win32::System::Variant::VARIANT::from(0i32);
                    println!(
                        "qt_native name={} role={:?} children={:?}",
                        brief(unsafe { acc.get_accName(&child) }),
                        unsafe { acc.get_accRole(&child) }.map(|r| super::msaa_role_i4(&r)),
                        unsafe { acc.accChildCount() }
                    );
                }
                Err(e) => println!("qt_native ERROR({e})"),
            }
        }
        if args.iter().any(|s| s == "--extended-interfaces") {
            extended_interfaces(root);
        }
        if args.iter().any(|s| s == "--cef-access") {
            super::a11y_activate_protocol(root);
        }
        println!(
            "trigger_wait={:?} (timeout does not imply missing tree)",
            wait.wait(1500)
        );
        snapshot(&a, root, region, "after-explicit-access");
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caret_point_uses_only_live_native_rectangle() {
        let p = caret_client_point(RECT {
            left: -100,
            top: -20,
            right: -100,
            bottom: 20,
        })
        .unwrap();
        assert_eq!((p.x, p.y), (-100, 0));
        assert!(caret_client_point(RECT::default()).is_none());
        assert!(caret_client_point(RECT {
            left: 1,
            right: 0,
            top: 0,
            bottom: 10
        })
        .is_none());
        let p = caret_client_point(RECT {
            left: 0,
            right: 1,
            top: i32::MIN,
            bottom: i32::MAX,
        })
        .unwrap();
        assert_eq!(p.y, 0);
    }
    #[test]
    fn empty_marker_rejected() {
        assert!(paste_rejection(Some(" ")).contains("REJECT"));
    }
    #[test]
    fn arbitrary_marker_never_success() {
        assert!(paste_rejection(Some("buyer-marker")).contains("DISABLED"));
        assert!(paste_rejection(None).contains("DISABLED"));
    }
    #[test]
    fn rectangles_diagnostic_only() {
        let a = parse_region("0,0,10,10").unwrap();
        assert_eq!(intersection(a, parse_region("5,5,20,20").unwrap()), 25);
        assert_eq!(intersection(a, parse_region("10,0,20,10").unwrap()), 0);
        assert!(parse_region("0,0,0,10").is_none());
        assert!(parse_region("1,2,3").is_none());
    }
    #[test]
    fn text_is_bounded_and_errors_explicit() {
        assert!(brief(Ok(BSTR::from("x".repeat(300)))).ends_with("truncated=true"));
        assert!(brief(Ok(BSTR::from("ok"))).ends_with("truncated=false"));
    }
}
