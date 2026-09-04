use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pipeline::{AssetKind, AssetPayload, TargetPipelineDeps, VK_RETURN};
use platform::{
    ClipboardPayload, ClipboardSink, FocusOutcome, FocusPlan, FocusWatcher, InputFocuser,
    KeyInjector, ReadinessProbe, ReadinessSignal, Result, WindowActivator, WindowHandle,
    WindowRect, WindowSnapshot, KEY_UP,
};
use ui_viewmodels::{TargetNoticeTone, TargetRoutingVm};

const WECHAT: WindowHandle = WindowHandle(101);
const TELEGRAM: WindowHandle = WindowHandle(202);

const PROFILES: &str = r#"
[[profiles]]
id = "wechat"
label = "微信"
exe_names = ["WeChat.exe"]

[[profiles]]
id = "telegram"
label = "Telegram"
exe_names = ["Telegram.exe"]
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Write(ClipboardPayload<'static>),
    Activate(WindowHandle),
    Focus(WindowHandle),
    Inject(Vec<u16>),
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Op>>>);

impl Log {
    fn push(&self, op: Op) {
        self.0.lock().unwrap().push(op);
    }

    fn read(&self) -> Vec<Op> {
        self.0.lock().unwrap().clone()
    }
}

struct Sink(Log);

impl ClipboardSink for Sink {
    fn write(&mut self, payload: &ClipboardPayload<'_>) -> Result<()> {
        self.0.push(Op::Write(payload.clone().into_owned()));
        Ok(())
    }
}

struct Focus;

impl FocusWatcher for Focus {
    fn foreground(&self) -> WindowHandle {
        TELEGRAM
    }

    fn is_alive(&self, window: WindowHandle) -> bool {
        window == TELEGRAM
    }
}

struct Injector(Log);

impl KeyInjector for Injector {
    fn inject(&mut self, keys: &[u16]) -> Result<()> {
        self.0.push(Op::Inject(keys.to_vec()));
        Ok(())
    }
}

struct Activator(Log);

impl WindowActivator for Activator {
    fn activate(
        &self,
        window: WindowHandle,
        _confirm_timeout_ms: u64,
        _settle_ms: u64,
    ) -> Result<bool> {
        self.0.push(Op::Activate(window));
        Ok(window == TELEGRAM)
    }
}

struct Probe;

impl ReadinessProbe for Probe {
    fn probe(&self, window: WindowHandle, _timeout_ms: u64) -> ReadinessSignal {
        assert_eq!(window, TELEGRAM);
        ReadinessSignal::Ready
    }

    fn blockers(&self, window: WindowHandle) -> ReadinessSignal {
        assert_eq!(window, TELEGRAM);
        ReadinessSignal::Inconclusive
    }
}

struct Focuser(Log);

impl InputFocuser for Focuser {
    fn focus_input(&self, window: WindowHandle, _plan: &FocusPlan) -> platform::FocusReport {
        self.0.push(Op::Focus(window));
        // Telegram 是常规控件应用，UIA 能真的把焦点送进输入框（不同于微信/千牛，见 D15）。
        // 替身照此表态，于是非严格就绪档下 verified 由聚焦结论给出 true。
        platform::FocusReport {
            outcome: FocusOutcome::FocusedByUia,
            attempts: Vec::new(),
        }
    }
}

fn window(hwnd: WindowHandle, exe: &str, title: &str) -> WindowSnapshot {
    WindowSnapshot {
        hwnd,
        exe_name: exe.to_string(),
        class_name: String::new(),
        title: title.to_string(),
        visible: true,
        minimized: false,
        rect: WindowRect {
            left: 0,
            top: 0,
            right: 800,
            bottom: 600,
        },
        process_id: hwnd.0 as u32,
    }
}

#[test]
fn selected_cold_target_reaches_exact_hwnd_and_never_synthesizes_enter() {
    let mut routing = TargetRoutingVm::from_profiles(PROFILES, None).unwrap();
    routing.refresh_windows(&[
        window(WECHAT, "WeChat.exe", "微信"),
        window(TELEGRAM, "Telegram.exe", "Saved Messages"),
    ]);
    assert!(routing.open_picker());

    let telegram_key = routing
        .snapshot()
        .choices
        .iter()
        .find(|choice| choice.binding.hwnd == Some(TELEGRAM))
        .unwrap()
        .selection_key();
    assert!(routing.choose(&telegram_key));

    let log = Log::default();
    let mut sink = Sink(log.clone());
    let mut injector = Injector(log.clone());
    let activator = Activator(log.clone());
    let focuser = Focuser(log.clone());
    let mut deps = TargetPipelineDeps {
        clipboard: &mut sink,
        focus: &Focus,
        injector: &mut injector,
        activator: &activator,
        readiness: &Probe,
        focuser: &focuser,
    };
    let payload = AssetPayload {
        kind: AssetKind::Text,
        png_bytes: &[],
        source_path: PathBuf::new(),
        text: "target-routing-probe".to_string(),
    };

    let notice = routing.paste(&payload, &mut deps);
    assert_eq!(notice.tone, TargetNoticeTone::Success);

    let ops = log.read();
    assert_eq!(
        ops.first(),
        Some(&Op::Write(ClipboardPayload::Text(
            "target-routing-probe".into()
        )))
    );
    assert!(ops.contains(&Op::Activate(TELEGRAM)));
    assert!(!ops.contains(&Op::Activate(WECHAT)));
    // 聚焦也必须精确落在被选目标上：点错窗口等于把素材塞进别人的输入框。
    assert!(ops.contains(&Op::Focus(TELEGRAM)));
    assert!(!ops.contains(&Op::Focus(WECHAT)));
    let injected = ops.iter().find_map(|op| match op {
        Op::Inject(keys) => Some(keys),
        _ => None,
    });
    let injected = injected.expect("目标链路必须注入 Ctrl+V");
    assert!(injected.iter().all(|key| key & !KEY_UP != VK_RETURN));
}

#[test]
fn no_selected_target_still_copies_before_friendly_feedback() {
    let routing = TargetRoutingVm::from_profiles(PROFILES, None).unwrap();
    let log = Log::default();
    let mut sink = Sink(log.clone());
    let mut injector = Injector(log.clone());
    let activator = Activator(log.clone());
    let focuser = Focuser(log.clone());
    let mut deps = TargetPipelineDeps {
        clipboard: &mut sink,
        focus: &Focus,
        injector: &mut injector,
        activator: &activator,
        readiness: &Probe,
        focuser: &focuser,
    };
    let payload = AssetPayload {
        kind: AssetKind::Text,
        png_bytes: &[],
        source_path: PathBuf::new(),
        text: "copy-first".to_string(),
    };

    let notice = routing.paste(&payload, &mut deps);
    assert_eq!(notice.tone, TargetNoticeTone::Warning);
    assert!(notice.text.contains("已复制"));
    assert!(matches!(log.read().first(), Some(Op::Write(_))));
    assert!(!log
        .read()
        .iter()
        .any(|op| matches!(op, Op::Activate(_) | Op::Inject(_))));
}
