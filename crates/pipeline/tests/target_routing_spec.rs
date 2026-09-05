use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pipeline::{
    negotiate, negotiate_detailed, AssetKind, AssetPayload, Negotiated, PasteConfig, PasteSession,
    TargetPasteOutcome, TargetPipelineDeps, VK_RETURN,
};
use platform::{
    ClipboardPayload, ClipboardSink, FocusOutcome, FocusPlan, FocusStep, FocusWatcher,
    ForegroundRelation, InputFocuser, KeyInjector, ReadinessBlocker, ReadinessProbe,
    ReadinessSignal, Result, WindowActivator, WindowHandle, KEY_UP,
};
use targets::ReadinessMode;
use targets::{
    ClipboardFormat, FocusStrategyStep, FormatPolicy, InputAnchor, KindFormats, Profile,
    SendPolicy, TargetBinding, TargetId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Write,
    Activate,
    Focus,
    Probe,
    Blockers,
    Alive,
    Foreground,
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
    fn write(&mut self, _: &ClipboardPayload<'_>) -> Result<()> {
        self.0.push(Op::Write);
        Ok(())
    }
}

struct Focus {
    log: Log,
    foreground: WindowHandle,
    alive: bool,
}
impl FocusWatcher for Focus {
    fn foreground(&self) -> WindowHandle {
        self.log.push(Op::Foreground);
        self.foreground
    }

    fn is_alive(&self, _: WindowHandle) -> bool {
        self.log.push(Op::Alive);
        self.alive
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
    fn activate(&self, _: WindowHandle, _: u64, _: u64) -> Result<bool> {
        self.0.push(Op::Activate);
        Ok(true)
    }
}

struct Probe {
    log: Log,
    signal: ReadinessSignal,
}
impl ReadinessProbe for Probe {
    fn probe(&self, _: WindowHandle, _: u64) -> ReadinessSignal {
        self.log.push(Op::Probe);
        self.signal
    }

    fn blockers(&self, _: WindowHandle) -> ReadinessSignal {
        self.log.push(Op::Blockers);
        self.signal
    }
}

struct Focuser {
    log: Log,
    outcome: FocusOutcome,
    seen_plan: Arc<Mutex<Option<FocusPlan>>>,
}

impl Focuser {
    fn new(log: Log, outcome: FocusOutcome) -> Self {
        Self {
            log,
            outcome,
            seen_plan: Arc::new(Mutex::new(None)),
        }
    }
}

impl InputFocuser for Focuser {
    fn focus_input(&self, _: WindowHandle, plan: &FocusPlan) -> platform::FocusReport {
        self.log.push(Op::Focus);
        *self.seen_plan.lock().unwrap() = Some(plan.clone());
        platform::FocusReport {
            outcome: self.outcome,
            attempts: Vec::new(),
        }
    }
}

const HWND: WindowHandle = WindowHandle(88);

fn profile() -> Profile {
    Profile {
        id: TargetId::new("wechat"),
        label: "微信".to_string(),
        exe_names: vec!["WeChat.exe".to_string()],
        class_names: Vec::new(),
        title_regexes: Vec::new(),
        not_ready_title_regexes: Vec::new(),
        require_title: false,
        formats: FormatPolicy::default(),
        paste_sends: SendPolicy::default(),
        readiness: targets::ReadinessMode::UiaShallow,
        settle_ms: 80,
        focus_strategy: vec![
            FocusStrategyStep::Already,
            FocusStrategyStep::Uia,
            FocusStrategyStep::Anchor,
        ],
        input_anchor: None,
        input_anchor_bottom: None,
        input_point: None,
        caret_semantic: None,
    }
}

fn payload<'a>(png: &'a [u8]) -> AssetPayload<'a> {
    AssetPayload {
        kind: AssetKind::Image,
        png_bytes: png,
        source_path: PathBuf::from("C:/asset.png"),
        text: String::new(),
    }
}

fn run(
    signal: ReadinessSignal,
    foreground: WindowHandle,
    auto_send: bool,
) -> (TargetPasteOutcome, Vec<Op>) {
    run_with_mode(signal, foreground, auto_send, ReadinessMode::UiaShallow)
}

/// 用指定画像跑一次上框，返回结果与操作日志。就绪度恒 Ready、前台恒命中，
/// 让断言只反映画像本身的决策（例如 paste_sends 是否阻断注入）。
fn run_with_profile(profile: Profile, req: &AssetPayload<'_>) -> (TargetPasteOutcome, Vec<Op>) {
    let log = Log::default();
    let mut sink = Sink(log.clone());
    let focus = Focus {
        log: log.clone(),
        foreground: HWND,
        alive: true,
    };
    let mut injector = Injector(log.clone());
    let activator = Activator(log.clone());
    let probe = Probe {
        log: log.clone(),
        signal: ReadinessSignal::Ready,
    };
    let focuser = Focuser::new(log.clone(), FocusOutcome::FocusedByAnchor);
    let mut deps = TargetPipelineDeps {
        clipboard: &mut sink,
        focus: &focus,
        injector: &mut injector,
        activator: &activator,
        readiness: &probe,
        focuser: &focuser,
    };
    let mut session = PasteSession::new(PasteConfig::default());
    session.set_target(Some(TargetBinding::new(
        profile.id.clone(),
        HWND,
        profile.label.clone(),
    )));
    let outcome = session.paste_targeted(req, &profile, &mut deps);
    (outcome, log.read())
}

fn video_payload<'a>() -> AssetPayload<'a> {
    AssetPayload {
        kind: AssetKind::Video,
        png_bytes: &[],
        source_path: PathBuf::from("C:/asset.mp4"),
        text: String::new(),
    }
}

/// 千牛的实测即发画像：只有视频的 CF_HDROP 会被当场发出。
fn video_files_sends() -> SendPolicy {
    SendPolicy::PerKind(KindFormats {
        video: vec![ClipboardFormat::Files],
        ..KindFormats::default()
    })
}

fn run_with_mode(
    signal: ReadinessSignal,
    foreground: WindowHandle,
    auto_send: bool,
    readiness: ReadinessMode,
) -> (TargetPasteOutcome, Vec<Op>) {
    run_with_focus(
        signal,
        foreground,
        auto_send,
        readiness,
        FocusOutcome::FocusedByAnchor,
    )
}

fn run_with_focus(
    signal: ReadinessSignal,
    foreground: WindowHandle,
    auto_send: bool,
    readiness: ReadinessMode,
    focus_outcome: FocusOutcome,
) -> (TargetPasteOutcome, Vec<Op>) {
    let log = Log::default();
    let mut sink = Sink(log.clone());
    let focus = Focus {
        log: log.clone(),
        foreground,
        alive: true,
    };
    let mut injector = Injector(log.clone());
    let activator = Activator(log.clone());
    let probe = Probe {
        log: log.clone(),
        signal,
    };
    let focuser = Focuser::new(log.clone(), focus_outcome);
    let mut deps = TargetPipelineDeps {
        clipboard: &mut sink,
        focus: &focus,
        injector: &mut injector,
        activator: &activator,
        readiness: &probe,
        focuser: &focuser,
    };
    let mut profile = profile();
    profile.readiness = readiness;
    let mut session = PasteSession::new(PasteConfig { auto_send });
    session.set_target(Some(TargetBinding::new(
        TargetId::new("wechat"),
        HWND,
        "微信",
    )));
    let outcome = session.paste_targeted(&payload(&[1, 2, 3]), &profile, &mut deps);
    (outcome, log.read())
}

fn has_enter(ops: &[Op]) -> bool {
    ops.iter().any(|op| match op {
        Op::Inject(keys) => keys.iter().any(|key| key & !KEY_UP == VK_RETURN),
        _ => false,
    })
}

#[test]
fn negotiate_honors_profile_ordered_format_fallback() {
    let mut profile = profile();
    profile.formats.image = vec![
        ClipboardFormat::Dib,
        ClipboardFormat::Files,
        ClipboardFormat::Png,
    ];
    let req = payload(&[1, 2, 3]);
    assert_eq!(
        negotiate(&req, &profile),
        Some(ClipboardPayload::Files(vec![PathBuf::from("C:/asset.png")]))
    );
}

#[test]
fn not_ready_no_conversation_never_injects() {
    let (outcome, ops) = run(
        ReadinessSignal::Blocked(ReadinessBlocker::NoConversation),
        HWND,
        false,
    );
    assert!(matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }));
    assert!(!ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

#[test]
fn unknown_readiness_injects_but_marks_unverified() {
    let (outcome, _) = run(ReadinessSignal::Inconclusive, HWND, false);
    assert_eq!(outcome, TargetPasteOutcome::Injected { verified: false });
}

#[test]
fn probe_timeout_falls_back_to_unknown_not_notready() {
    let (outcome, ops) = run(ReadinessSignal::Inconclusive, HWND, false);
    assert!(matches!(outcome, TargetPasteOutcome::Injected { .. }));
    assert!(ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

#[test]
fn uia_strict_inconclusive_copies_without_injecting() {
    let (outcome, ops) = run_with_mode(
        ReadinessSignal::Inconclusive,
        HWND,
        false,
        ReadinessMode::UiaStrict,
    );
    assert!(matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }));
    assert!(!ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

#[test]
fn foreground_drift_before_inject_aborts() {
    let (outcome, ops) = run(ReadinessSignal::Ready, WindowHandle(99), false);
    assert!(matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }));
    assert!(!ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

// ---------------------------------------------------------------------------
// 注入前前台漂移的分流（D44）：OwnProcess/SameAsTarget 再断言后注入，Foreign 降级
// ---------------------------------------------------------------------------

/// 前台按脚本变化的双打：第一次 `foreground()` 返回漂移前台，之后恒 `final_fg`。
struct DriftFocus {
    log: Log,
    script: Mutex<VecDeque<WindowHandle>>,
    final_fg: WindowHandle,
    alive: bool,
    relation: ForegroundRelation,
}

impl FocusWatcher for DriftFocus {
    fn foreground(&self) -> WindowHandle {
        self.log.push(Op::Foreground);
        let mut script = self.script.lock().unwrap();
        match script.pop_front() {
            Some(fg) => fg,
            None => self.final_fg,
        }
    }

    fn is_alive(&self, _: WindowHandle) -> bool {
        self.log.push(Op::Alive);
        self.alive
    }

    fn foreground_relation(&self, _: WindowHandle, _: WindowHandle) -> ForegroundRelation {
        self.relation
    }
}

/// 组装漂移场景并执行一次上框（就绪度恒 Ready）。
fn run_drift(
    relation: ForegroundRelation,
    drifted_fg: WindowHandle,
    final_fg: WindowHandle,
) -> (TargetPasteOutcome, Vec<Op>) {
    let log = Log::default();
    let mut sink = Sink(log.clone());
    let focus = DriftFocus {
        log: log.clone(),
        script: Mutex::new([drifted_fg].into_iter().collect()),
        final_fg,
        alive: true,
        relation,
    };
    let mut injector = Injector(log.clone());
    let activator = Activator(log.clone());
    let probe = Probe {
        log: log.clone(),
        signal: ReadinessSignal::Ready,
    };
    let focuser = Focuser::new(log.clone(), FocusOutcome::FocusedByAnchor);
    let mut deps = TargetPipelineDeps {
        clipboard: &mut sink,
        focus: &focus,
        injector: &mut injector,
        activator: &activator,
        readiness: &probe,
        focuser: &focuser,
    };
    let mut session = PasteSession::new(PasteConfig::default());
    session.set_target(Some(TargetBinding::new(
        TargetId::new("wechat"),
        HWND,
        "微信",
    )));
    let outcome = session.paste_targeted(&payload(&[1, 2, 3]), &profile(), &mut deps);
    (outcome, log.read())
}

fn activate_count(ops: &[Op]) -> usize {
    ops.iter().filter(|op| **op == Op::Activate).count()
}

/// 用户连点素材面板：注入前前台回到自己进程。上框意图未变，再断言一次前台
/// 后应照常注入——这正是低配机日志里近半数降级的补救路径（D44）。
#[test]
fn foreground_drift_to_own_panel_reasserts_and_injects() {
    let (outcome, ops) = run_drift(ForegroundRelation::OwnProcess, WindowHandle(0xAB), HWND);
    assert_eq!(outcome, TargetPasteOutcome::Injected { verified: true });
    // 首激活 + 再断言，恰好两次；素材最终被注入。
    assert_eq!(activate_count(&ops), 2);
    assert!(ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

/// 目标内部多顶层表面抖动（前台仍在目标进程）：同样再断言后注入。
#[test]
fn foreground_drift_within_target_process_reasserts_and_injects() {
    let (outcome, ops) = run_drift(ForegroundRelation::SameAsTarget, WindowHandle(77), HWND);
    assert_eq!(outcome, TargetPasteOutcome::Injected { verified: true });
    assert_eq!(activate_count(&ops), 2);
    assert!(ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

/// 前台是无关第三方应用：用户已切走，不得抢回前台，立即降级为仅复制。
#[test]
fn foreground_drift_to_foreign_window_degrades_without_reassert() {
    let (outcome, ops) = run_drift(
        ForegroundRelation::Foreign,
        WindowHandle(0xCC),
        WindowHandle(0xCC),
    );
    assert!(matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }));
    // 只发生首激活，绝不为第三方前台发起第二次激活。
    assert_eq!(activate_count(&ops), 1);
    assert!(!ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

#[test]
fn core_upload_path_never_synthesizes_enter() {
    let (outcome, ops) = run(ReadinessSignal::Ready, HWND, true);
    assert_eq!(outcome, TargetPasteOutcome::Injected { verified: true });
    assert!(!has_enter(&ops));
}

#[test]
fn targeted_pipeline_order_is_write_activate_probe_validate_inject() {
    let (_, ops) = run(ReadinessSignal::Ready, HWND, false);
    assert_eq!(
        ops,
        vec![
            Op::Write,
            Op::Activate,
            Op::Focus,
            // 非严格档的就绪步骤是 O(1) 否证而不是 UIA 往返（AC5），位置不变。
            Op::Blockers,
            Op::Alive,
            Op::Foreground,
            Op::Inject(vec![0x11, 0x56, 0x8056, 0x8011]),
        ]
    );
}

/// 焦点获取必须夹在「激活」与「探测」之间：更早没有前台窗口可点，
/// 更晚则探测拿到的是激活瞬间那个错误焦点（真机上就是窗口根控件）。
#[test]
fn focus_step_runs_between_activate_and_probe() {
    let (_, ops) = run(ReadinessSignal::Ready, HWND, false);
    let activate = ops.iter().position(|op| op == &Op::Activate).unwrap();
    let focus = ops.iter().position(|op| op == &Op::Focus).unwrap();
    let readiness = ops
        .iter()
        .position(|op| matches!(op, Op::Probe | Op::Blockers))
        .unwrap();
    assert!(activate < focus && focus < readiness, "实际顺序: {ops:?}");

    // 严格档走 UIA probe()，夹在中间的位置关系必须完全一致。
    let (_, strict) = run_with_mode(
        ReadinessSignal::Ready,
        HWND,
        false,
        ReadinessMode::UiaStrict,
    );
    let activate = strict.iter().position(|op| op == &Op::Activate).unwrap();
    let focus = strict.iter().position(|op| op == &Op::Focus).unwrap();
    let probe = strict.iter().position(|op| op == &Op::Probe).unwrap();
    assert!(activate < focus && focus < probe, "实际顺序: {strict:?}");
}

/// 聚焦是「移动焦点」，不是「发消息」：这条路径上不得出现任何按键注入，
/// 唯一允许的注入是随后的 Ctrl+V。
#[test]
fn focus_step_never_injects_keys_before_paste_chord() {
    let (_, ops) = run(ReadinessSignal::Ready, HWND, false);
    let injects: Vec<_> = ops
        .iter()
        .filter_map(|op| match op {
            Op::Inject(keys) => Some(keys.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(injects, vec![vec![0x11, 0x56, 0x8056, 0x8011]]);
    assert!(!has_enter(&ops));
}

/// 拿不到焦点是「没能证明」而不是「证明失败」：默认档仍然注入（用户看得见
/// 输入框里有没有内容），但结果标 unverified，UI 据此提示确认。
#[test]
fn focus_unavailable_still_injects_but_marks_unverified() {
    let (outcome, ops) = run_with_focus(
        ReadinessSignal::Inconclusive,
        HWND,
        false,
        ReadinessMode::UiaShallow,
        FocusOutcome::Unavailable,
    );
    assert_eq!(outcome, TargetPasteOutcome::Injected { verified: false });
    assert!(ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

/// 探测不确定但焦点已确凿落在可写控件上时，结果可以标 verified——
/// 「已上框」的判定不必被 Qt/CEF 的 UIA 空洞连坐。
#[test]
fn confirmed_focus_upgrades_inconclusive_probe_to_verified() {
    let (outcome, _) = run_with_focus(
        ReadinessSignal::Inconclusive,
        HWND,
        false,
        ReadinessMode::UiaShallow,
        FocusOutcome::AlreadyEditable,
    );
    assert_eq!(outcome, TargetPasteOutcome::Injected { verified: true });
}

/// 严格档的语义是「证明不了就不动手」：拿不到焦点直接降级仅复制，不注入。
#[test]
fn uia_strict_aborts_when_focus_unavailable() {
    let (outcome, ops) = run_with_focus(
        ReadinessSignal::Ready,
        HWND,
        false,
        ReadinessMode::UiaStrict,
        FocusOutcome::Unavailable,
    );
    assert!(matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }));
    assert!(!ops.iter().any(|op| matches!(op, Op::Inject(_))));
    assert!(
        !ops.iter().any(|op| op == &Op::Probe),
        "严格档应在探测前就止步"
    );
}

/// 画像声明的锚点必须原样传到平台层：夹紧与安全校验是平台层的职责，
/// 管线不得擅自改写或凭空补一个锚点。
#[test]
fn profile_anchor_is_forwarded_to_focuser_verbatim() {
    let log = Log::default();
    let mut sink = Sink(log.clone());
    let focus = Focus {
        log: log.clone(),
        foreground: HWND,
        alive: true,
    };
    let mut injector = Injector(log.clone());
    let activator = Activator(log.clone());
    let probe = Probe {
        log: log.clone(),
        signal: ReadinessSignal::Ready,
    };
    let focuser = Focuser::new(log.clone(), FocusOutcome::FocusedByAnchor);
    let seen = focuser.seen_plan.clone();
    let mut profile = profile();
    profile.input_anchor = Some(InputAnchor {
        x_ratio: 0.394,
        y_ratio: 0.787,
    });
    {
        let mut deps = TargetPipelineDeps {
            clipboard: &mut sink,
            focus: &focus,
            injector: &mut injector,
            activator: &activator,
            readiness: &probe,
            focuser: &focuser,
        };
        let mut session = PasteSession::new(PasteConfig::default());
        session.set_target(Some(TargetBinding::new(profile.id.clone(), HWND, "微信")));
        session.paste_targeted(&payload(&[1, 2, 3]), &profile, &mut deps);
    }
    let anchor = seen
        .lock()
        .unwrap()
        .clone()
        .expect("聚焦端应被调用")
        .anchor
        .expect("锚点应被传入");
    assert!((anchor.x_ratio - 0.394).abs() < f32::EPSILON);
    assert!((anchor.y_ratio - 0.787).abs() < f32::EPSILON);
}

/// 未声明锚点的画像不得被平台层点击：传入 None 是「不要点」的唯一表达。
#[test]
fn profile_without_anchor_forwards_none() {
    let log = Log::default();
    let mut sink = Sink(log.clone());
    let focus = Focus {
        log: log.clone(),
        foreground: HWND,
        alive: true,
    };
    let mut injector = Injector(log.clone());
    let activator = Activator(log.clone());
    let probe = Probe {
        log: log.clone(),
        signal: ReadinessSignal::Ready,
    };
    let focuser = Focuser::new(log.clone(), FocusOutcome::Unavailable);
    let seen = focuser.seen_plan.clone();
    let profile = profile();
    {
        let mut deps = TargetPipelineDeps {
            clipboard: &mut sink,
            focus: &focus,
            injector: &mut injector,
            activator: &activator,
            readiness: &probe,
            focuser: &focuser,
        };
        let mut session = PasteSession::new(PasteConfig::default());
        session.set_target(Some(TargetBinding::new(profile.id.clone(), HWND, "微信")));
        session.paste_targeted(&payload(&[1, 2, 3]), &profile, &mut deps);
    }
    let plan = seen.lock().unwrap().clone().expect("聚焦端应被调用");
    assert_eq!(plan.anchor, None);
}

/// 聚焦级别顺序是**画像数据**而不是平台层的代码分支：画像声明什么，
/// 平台层就按什么顺序试。这条测试锁住这个性质，防止有人把顺序写回代码里。
#[test]
fn profile_focus_strategy_is_forwarded_as_plan() {
    let log = Log::default();
    let mut sink = Sink(log.clone());
    let focus = Focus {
        log: log.clone(),
        foreground: HWND,
        alive: true,
    };
    let mut injector = Injector(log.clone());
    let activator = Activator(log.clone());
    let probe = Probe {
        log: log.clone(),
        signal: ReadinessSignal::Ready,
    };
    let focuser = Focuser::new(log.clone(), FocusOutcome::FocusedByAnchor);
    let seen = focuser.seen_plan.clone();
    let mut profile = profile();
    profile.focus_strategy = vec![FocusStrategyStep::Already, FocusStrategyStep::Anchor];
    {
        let mut deps = TargetPipelineDeps {
            clipboard: &mut sink,
            focus: &focus,
            injector: &mut injector,
            activator: &activator,
            readiness: &probe,
            focuser: &focuser,
        };
        let mut session = PasteSession::new(PasteConfig::default());
        session.set_target(Some(TargetBinding::new(profile.id.clone(), HWND, "微信")));
        session.paste_targeted(&payload(&[1, 2, 3]), &profile, &mut deps);
    }
    let plan = seen.lock().unwrap().clone().expect("聚焦端应被调用");
    assert_eq!(
        plan.steps,
        vec![FocusStep::AlreadyEditable, FocusStep::AnchorClick]
    );
}

/// 千牛 × 视频：画像唯一可承载格式 files 被标记为「粘贴即发送」→
/// 复制得做（用户还能手动粘），注入绝不能做（否则直接把素材发出去）。
#[test]
fn paste_sends_format_copies_without_injecting() {
    let mut profile = profile();
    profile.paste_sends = video_files_sends();
    let (outcome, ops) = run_with_profile(profile, &video_payload());
    assert!(matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }));
    assert!(ops.contains(&Op::Write));
    assert!(!ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

#[test]
fn paste_sends_feedback_tells_user_to_paste_manually() {
    let mut profile = profile();
    profile.paste_sends = video_files_sends();
    let (outcome, _) = run_with_profile(profile, &video_payload());
    let TargetPasteOutcome::CopiedOnly { feedback } = outcome else {
        panic!("粘贴即发送的格式必须降级为仅复制");
    };
    assert_eq!(feedback.action, pipeline::FeedbackAction::PasteManually);
    assert!(!feedback.hint.is_empty());
}

/// 有安全替代格式时不该降级：某目标若真把图片 HDROP 当即发，图片仍能回落 png 上框。
#[test]
fn paste_sends_falls_back_to_safe_format_when_available() {
    let mut profile = profile();
    profile.paste_sends = SendPolicy::PerKind(KindFormats {
        image: vec![ClipboardFormat::Files],
        ..KindFormats::default()
    });
    let req = payload(&[1, 2, 3]);
    assert_eq!(
        negotiate_detailed(&req, &profile),
        Negotiated::Safe {
            format: ClipboardFormat::Png,
            payload: ClipboardPayload::Png(vec![1, 2, 3].into())
        }
    );
    let (outcome, ops) = run_with_profile(profile, &req);
    assert_eq!(outcome, TargetPasteOutcome::Injected { verified: true });
    assert!(ops.iter().any(|op| matches!(op, Op::Inject(_))));
}

/// 「无法承载」与「粘进去会发送」是两种不同结论，不可混为一谈：
/// 前者是硬失败（连复制都没意义），后者仍要复制并给出手动粘贴指引。
#[test]
fn unsupported_and_would_send_are_distinct_results() {
    let mut profile = profile();
    profile.paste_sends = video_files_sends();
    assert_eq!(
        negotiate_detailed(&video_payload(), &profile),
        Negotiated::WouldSend {
            format: ClipboardFormat::Files,
            payload: ClipboardPayload::Files(vec![PathBuf::from("C:/asset.mp4")])
        }
    );
    let empty = AssetPayload {
        kind: AssetKind::Video,
        png_bytes: &[],
        source_path: PathBuf::new(),
        text: String::new(),
    };
    assert_eq!(
        negotiate_detailed(&empty, &profile),
        Negotiated::Unsupported
    );
}

/// 便捷入口 negotiate() 只返回安全格式，避免旧调用点无意中拿到即发格式。
#[test]
fn negotiate_skips_paste_sends_formats() {
    let mut profile = profile();
    profile.paste_sends = video_files_sends();
    assert_eq!(negotiate(&video_payload(), &profile), None);
}

/// 即发是 (类别 × 格式) 事实，不是纯格式事实：千牛画像只把 video × files 标为即发，
/// 因此同一份 CF_HDROP 对图片必须判定为 Safe（实测图片 HDROP 停在输入框显示缩略图），
/// 对视频仍必须判定为 WouldSend（实测视频 HDROP 当场发出）。
#[test]
fn paste_sends_is_per_kind_not_per_format() {
    let mut profile = profile();
    profile.formats.image = vec![ClipboardFormat::Files, ClipboardFormat::Png];
    profile.paste_sends = video_files_sends();

    let image = payload(&[1, 2, 3]);
    assert_eq!(
        negotiate_detailed(&image, &profile),
        Negotiated::Safe {
            format: ClipboardFormat::Files,
            payload: ClipboardPayload::Files(vec![PathBuf::from("C:/asset.png")])
        },
        "图片走 HDROP 不会触发发送，必须直接上框而不是退化成 CF_PNG"
    );
    assert!(matches!(
        negotiate_detailed(&video_payload(), &profile),
        Negotiated::WouldSend {
            format: ClipboardFormat::Files,
            ..
        }
    ));
}

/// 旧用户画像写的是 `paste_sends = ["files"]`（纯格式数组）。兼容形态必须仍然
/// 覆盖所有类别，否则升级会把用户显式声明的即发保护静默降级为「不保护」。
#[test]
fn legacy_flat_paste_sends_still_covers_every_kind() {
    let mut profile = profile();
    profile.formats.image = vec![ClipboardFormat::Files];
    profile.paste_sends = SendPolicy::AllKinds(vec![ClipboardFormat::Files]);
    assert!(matches!(
        negotiate_detailed(&payload(&[]), &profile),
        Negotiated::WouldSend { .. }
    ));
    assert!(matches!(
        negotiate_detailed(&video_payload(), &profile),
        Negotiated::WouldSend { .. }
    ));
}

/// 就绪探测的成本必须由画像档位决定：只有严格档才付 UIA 往返（实测 3~30ms），
/// 非严格档改走 `blockers()` 的两项 O(1) 否证检查。这是 AC5。
#[test]
fn only_strict_readiness_pays_for_uia_probe() {
    let count = |ops: &[Op], want: &Op| ops.iter().filter(|op| *op == want).count();

    for mode in [ReadinessMode::UiaShallow, ReadinessMode::P0Only] {
        let (outcome, ops) = run_with_mode(ReadinessSignal::Ready, HWND, false, mode);
        assert_eq!(
            count(&ops, &Op::Probe),
            0,
            "{mode:?} 档不得触发 UIA 往返 probe()"
        );
        assert_eq!(
            count(&ops, &Op::Blockers),
            1,
            "{mode:?} 档必须做且只做一次 blockers() 否证"
        );
        assert!(matches!(outcome, TargetPasteOutcome::Injected { .. }));
    }

    let (outcome, ops) = run_with_mode(
        ReadinessSignal::Ready,
        HWND,
        false,
        ReadinessMode::UiaStrict,
    );
    assert_eq!(count(&ops, &Op::Probe), 1, "严格档必须做一次 UIA probe()");
    assert_eq!(
        count(&ops, &Op::Blockers),
        0,
        "严格档不重复做否证：probe() 内部已含同样两项检查"
    );
    assert!(matches!(outcome, TargetPasteOutcome::Injected { .. }));
}

/// 换成 blockers() 之后，明确否证的中止语义必须一字不变：窗口消失仍然只复制不注入。
#[test]
fn shallow_readiness_still_aborts_on_proven_blocker() {
    let (outcome, ops) = run_with_mode(
        ReadinessSignal::Blocked(ReadinessBlocker::WindowGone),
        HWND,
        false,
        ReadinessMode::UiaShallow,
    );
    assert!(matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }));
    assert!(!ops.iter().any(|op| matches!(op, Op::Inject(_))));
}
