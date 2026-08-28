//! 运行时装配契约：`TargetRoutingRuntime` 只依赖 platform 的 trait 层。
//!
//! 背景：此前本 crate 直接 use platform::win32 具体实现，绕过了「Win32 由二进制
//! 入口装配」这条分层红线，也使运行时无法脱离桌面会话测试。改成注入
//! `TargetRuntimeDeps` 后，本测试用替身跑通「枚举 → 热目标 → 上框」，并断言全程无 Enter。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use platform::{
    ClipboardPayload, ClipboardSink, FileDialogs, FocusOutcome, FocusPlan, FocusStep, FocusWatcher,
    ForegroundObserver, InputFocuser, KeyInjector, ReadinessProbe, ReadinessSignal, Result,
    WindowActivator, WindowEnumerator, WindowHandle, WindowRect, WindowSnapshot,
};
use ui_viewmodels::{
    AssetKind, AssetPayload, TargetBarMode, TargetNoticeTone, TargetRoutingRuntime,
    TargetRuntimeDeps,
};

const PROFILES: &str = r#"
[[profiles]]
id = "wechat"
label = "微信"
exe_names = ["Weixin.exe"]
readiness = "uia_shallow"
settle_ms = 10

[profiles.formats]
image = ["png", "files"]
video = ["files"]
text = ["text"]
other = []
"#;

const TARGET: WindowHandle = WindowHandle(4242);

#[derive(Default)]
struct Recorder {
    payloads: Vec<ClipboardPayload<'static>>,
    keys: Vec<Vec<u16>>,
    activations: Vec<WindowHandle>,
    focuses: Vec<(WindowHandle, FocusPlan)>,
}

type Shared = Arc<Mutex<Recorder>>;

struct Sink(Shared);
impl ClipboardSink for Sink {
    fn write(&mut self, payload: &ClipboardPayload<'_>) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .payloads
            .push(payload.clone().into_owned());
        Ok(())
    }
}

struct Injector(Shared);
impl KeyInjector for Injector {
    fn inject(&mut self, keys: &[u16]) -> Result<()> {
        self.0.lock().unwrap().keys.push(keys.to_vec());
        Ok(())
    }
}

struct Activator(Shared);
impl WindowActivator for Activator {
    fn activate(&self, window: WindowHandle, _: u64, _: u64) -> Result<bool> {
        self.0.lock().unwrap().activations.push(window);
        Ok(true)
    }
}

struct Focus;
impl FocusWatcher for Focus {
    fn foreground(&self) -> WindowHandle {
        TARGET
    }

    fn is_alive(&self, _: WindowHandle) -> bool {
        true
    }
}

/// 前台停在无关第三方窗口（用户已切走）：注入前校验必然否证、降级仅复制。
struct ForeignFocus;
impl FocusWatcher for ForeignFocus {
    fn foreground(&self) -> WindowHandle {
        WindowHandle(9999)
    }

    fn is_alive(&self, _: WindowHandle) -> bool {
        true
    }
}

struct Enumerator;
impl WindowEnumerator for Enumerator {
    fn windows(&self) -> Result<Vec<WindowSnapshot>> {
        Ok(vec![WindowSnapshot {
            hwnd: TARGET,
            exe_name: "Weixin.exe".to_string(),
            class_name: "Qt51514QWindowIcon".to_string(),
            title: "微信".to_string(),
            visible: true,
            minimized: false,
            rect: WindowRect {
                left: 0,
                top: 0,
                right: 900,
                bottom: 700,
            },
            process_id: 7,
        }])
    }
}

/// 真实微信/千牛都探测不到输入框（DECISIONS D15），替身照实返回 Inconclusive。
struct Probe;
impl ReadinessProbe for Probe {
    fn probe(&self, _: WindowHandle, _: u64) -> ReadinessSignal {
        ReadinessSignal::Inconclusive
    }

    fn blockers(&self, _: WindowHandle) -> ReadinessSignal {
        ReadinessSignal::Inconclusive
    }
}

struct NoObserver;
impl ForegroundObserver for NoObserver {
    fn next_foreground(&mut self) -> Result<Option<WindowSnapshot>> {
        Ok(None)
    }
}

/// 千牛 CEF / 微信 Qt 都不暴露可 SetFocus 的输入框，替身照实走「锚点单击」这一级。
struct Focuser(Shared);
impl InputFocuser for Focuser {
    fn focus_input(&self, window: WindowHandle, plan: &FocusPlan) -> FocusOutcome {
        self.0.lock().unwrap().focuses.push((window, plan.clone()));
        FocusOutcome::FocusedByAnchor
    }
}

/// 测试替身：无对话框能力（任何选择都返回「取消」）。
struct NoDialogs;
impl FileDialogs for NoDialogs {
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

fn runtime(recorder: &Shared) -> TargetRoutingRuntime {
    TargetRoutingRuntime::new(
        PROFILES,
        None,
        TargetRuntimeDeps {
            observer: Some(Box::new(NoObserver)),
            enumerator: Box::new(Enumerator),
            clipboard: Box::new(Sink(recorder.clone())),
            focus: Box::new(Focus),
            injector: Box::new(Injector(recorder.clone())),
            activator: Box::new(Activator(recorder.clone())),
            readiness: Box::new(Probe),
            focuser: Box::new(Focuser(recorder.clone())),
            dialogs: Box::new(NoDialogs),
        },
    )
    .expect("画像装载失败")
}

#[test]
fn runtime_accepts_injected_platform_deps_and_locks_hot_target() {
    let recorder: Shared = Arc::default();
    let runtime = runtime(&recorder);

    let snapshot = runtime.snapshot();
    assert_eq!(
        snapshot.mode,
        TargetBarMode::Ready,
        "唯一 eligible 前台窗口应直接成为热目标，无需用户选择"
    );
    assert_eq!(snapshot.label, "微信");
}

/// 泵启动慢（低配机冷启动握手超时，D40）的替身：第一次 `set_wakeup` 失败、
/// 之后成功——锁定「退路 Timer 每轮重试接管事件驱动，成功即停表」的契约。
#[derive(Clone, Default)]
struct LateObserver {
    ready: Arc<Mutex<bool>>,
    installs: Arc<Mutex<usize>>,
}

impl ForegroundObserver for LateObserver {
    fn next_foreground(&mut self) -> Result<Option<WindowSnapshot>> {
        Ok(None)
    }

    fn set_wakeup(&mut self, _wakeup: Box<dyn Fn() + Send + Sync>) -> bool {
        *self.installs.lock().unwrap() += 1;
        *self.ready.lock().unwrap()
    }
}

/// 可编排枚举替身（D42）：脚本队列非空时每次弹出队首，空了回落到「微信还在」的
/// 固定快照，并记录调用次数。脚本由测试在外部推入，用于编排
/// 「窗口从枚举中消失 → 解绑 → 恢复」序列。
struct ScriptedEnumerator {
    script: Arc<Mutex<Vec<Vec<WindowSnapshot>>>>,
    calls: Arc<Mutex<usize>>,
}

fn weixin_window(hwnd: WindowHandle, process_id: u32) -> WindowSnapshot {
    WindowSnapshot {
        hwnd,
        exe_name: "Weixin.exe".to_string(),
        class_name: "Qt51514QWindowIcon".to_string(),
        title: "微信".to_string(),
        visible: true,
        minimized: false,
        rect: WindowRect {
            left: 0,
            top: 0,
            right: 900,
            bottom: 700,
        },
        process_id,
    }
}

impl WindowEnumerator for ScriptedEnumerator {
    fn windows(&self) -> Result<Vec<WindowSnapshot>> {
        *self.calls.lock().unwrap() += 1;
        let mut script = self.script.lock().unwrap();
        Ok(if script.is_empty() {
            vec![weixin_window(TARGET, 7)]
        } else {
            script.remove(0)
        })
    }
}

/// D42 解绑自愈：窗口从枚举中短暂消失（最小化/恢复动画中被快照过滤是真实低配机
/// 观测到的诱因）会让热目标解绑，随后上框直接降级「目标窗口已关闭」——旧路径要
/// 等下一轮轮询（退路 Timer 最长 2s）才重绑，用户只能连续重试硬扛。现在解绑态的
/// paste 必须先强制重绑，把「等轮询」变成「当次请求内自愈」。
#[test]
fn paste_heals_dormant_hot_target_by_forced_rebind() {
    let recorder: Shared = Arc::default();
    let calls = Arc::new(Mutex::new(0usize));
    let script: Arc<Mutex<Vec<Vec<WindowSnapshot>>>> = Arc::new(Mutex::new(Vec::new()));
    let enumerator = ScriptedEnumerator {
        script: Arc::clone(&script),
        calls: Arc::clone(&calls),
    };
    let mut runtime = TargetRoutingRuntime::new(
        PROFILES,
        None,
        TargetRuntimeDeps {
            observer: Some(Box::new(NoObserver)),
            enumerator: Box::new(enumerator),
            clipboard: Box::new(Sink(recorder.clone())),
            focus: Box::new(Focus),
            injector: Box::new(Injector(recorder.clone())),
            activator: Box::new(Activator(recorder.clone())),
            readiness: Box::new(Probe),
            focuser: Box::new(Focuser(recorder.clone())),
            dialogs: Box::new(NoDialogs),
        },
    )
    .expect("画像装载失败");

    // new() 内的首次 poll（脚本为空 → 固定快照）已锁定热目标。
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(
        runtime.selected().and_then(|binding| binding.hwnd),
        Some(TARGET)
    );

    // 推入一次「微信窗口消失」的快照：下一次 poll 解绑进入休眠态。
    script.lock().unwrap().push(vec![]);
    runtime.poll().expect("poll 失败");
    assert_eq!(
        runtime.selected().and_then(|binding| binding.hwnd),
        None,
        "窗口从枚举中消失后热目标必须解绑"
    );

    // 解绑态直接 paste（脚本已耗尽 → 回落「窗口还在」快照）：
    // 必须先强制重绑再上框，而不是降级「目标窗口已关闭」。
    let notice = runtime.paste(&AssetPayload {
        kind: AssetKind::Video,
        png_bytes: &[],
        source_path: PathBuf::from("C:/library/objects/clip.mp4"),
        text: String::new(),
    });
    assert_eq!(
        runtime.selected().and_then(|binding| binding.hwnd),
        Some(TARGET),
        "解绑自愈必须当场重绑，不等下一轮轮询"
    );
    assert_eq!(*calls.lock().unwrap(), 3, "自愈恰好触发一次全枚举");
    assert!(
        !recorder.lock().unwrap().keys.is_empty(),
        "重绑后上框必须走到注入而不是降级仅复制，notice={}",
        notice.text
    );
}

#[test]
fn install_wakeup_takes_over_once_observer_becomes_ready() {
    let recorder: Shared = Arc::default();
    let observer = LateObserver::default();
    let ready = observer.ready.clone();
    let installs = observer.installs.clone();
    let mut runtime = TargetRoutingRuntime::new(
        PROFILES,
        None,
        TargetRuntimeDeps {
            observer: Some(Box::new(observer)),
            enumerator: Box::new(Enumerator),
            clipboard: Box::new(Sink(recorder.clone())),
            focus: Box::new(Focus),
            injector: Box::new(Injector(recorder.clone())),
            activator: Box::new(Activator(recorder.clone())),
            readiness: Box::new(Probe),
            focuser: Box::new(Focuser(recorder.clone())),
            dialogs: Box::new(NoDialogs),
        },
    )
    .expect("画像装载失败");

    assert!(
        !runtime.install_wakeup(Box::new(|| {})),
        "泵未就绪时接管必须失败——调用方据此保留退路 Timer（时序驱动）"
    );
    // 泵自愈（钩子装好）后：退路 Timer 的下一轮重试必须能接管成功。
    *ready.lock().unwrap() = true;
    assert!(
        runtime.install_wakeup(Box::new(|| {})),
        "观察器就绪后重试接管必须成功，否则进程永久停在退路 Timer 上"
    );
    // 已接管后重复调用保持幂等成功。
    assert!(runtime.install_wakeup(Box::new(|| {})));
    assert_eq!(*installs.lock().unwrap(), 3);
}

// ---------------------------------------------------------------------------
// 连击合并（D45）：Slint clicked 在双击的第二次抬笔同样触发（items.rs Release
// 分支先无条件发 clicked 再追加 double_clicked），单击模式下一次双击 = 两次
// 完整上框请求，且尾随点击的 mouse-down 会当场抢走首次注入前校验的前台。
// ---------------------------------------------------------------------------

fn coalesce_runtime(recorder: &Shared, focus: Box<dyn FocusWatcher>) -> TargetRoutingRuntime {
    TargetRoutingRuntime::new(
        PROFILES,
        None,
        TargetRuntimeDeps {
            observer: Some(Box::new(NoObserver)),
            enumerator: Box::new(Enumerator),
            clipboard: Box::new(Sink(recorder.clone())),
            focus,
            injector: Box::new(Injector(recorder.clone())),
            activator: Box::new(Activator(recorder.clone())),
            readiness: Box::new(Probe),
            focuser: Box::new(Focuser(recorder.clone())),
            dialogs: Box::new(NoDialogs),
        },
    )
    .expect("画像装载失败")
}

fn video_payload_at(path: &str) -> AssetPayload<'static> {
    AssetPayload {
        kind: AssetKind::Video,
        png_bytes: &[],
        source_path: PathBuf::from(path),
        text: String::new(),
    }
}

#[test]
fn trailing_click_of_double_click_merges_into_first_paste() {
    let recorder: Shared = Arc::default();
    let mut runtime = coalesce_runtime(&recorder, Box::new(Focus));
    let payload = video_payload_at("C:/library/objects/clip.mp4");

    let first = runtime.paste(&payload);
    assert!(first.injected, "首次上框必须注入，notice={}", first.text);
    let keys_after_first = recorder.lock().unwrap().keys.len();
    assert!(keys_after_first >= 1);

    // 合并窗口内的同素材第二次请求（双击的尾随半程）：不得再次注入。
    let second = runtime.paste(&payload);
    assert_eq!(
        recorder.lock().unwrap().keys.len(),
        keys_after_first,
        "合并窗口内的同素材请求不得再次注入，notice={}",
        second.text
    );
    assert!(second.text.contains("连击已合并"));
}

#[test]
fn degraded_paste_does_not_block_immediate_retry() {
    let recorder: Shared = Arc::default();
    let mut runtime = coalesce_runtime(&recorder, Box::new(ForeignFocus));
    let payload = video_payload_at("C:/library/objects/clip.mp4");

    let first = runtime.paste(&payload);
    assert!(
        !first.injected,
        "第三方前台必须降级仅复制，notice={}",
        first.text
    );
    let writes_after_first = recorder.lock().unwrap().payloads.len();

    // 首次没有真正注入：同素材的立即重试必须完整再走管线（重新写剪贴板），
    // 而不是被合并窗口吞掉。
    let second = runtime.paste(&payload);
    assert!(!second.injected);
    assert_eq!(
        recorder.lock().unwrap().payloads.len(),
        writes_after_first + 1,
        "降级后的同素材重试必须再次走完整管线"
    );
}

#[test]
fn different_asset_within_coalesce_window_still_pastes() {
    let recorder: Shared = Arc::default();
    let mut runtime = coalesce_runtime(&recorder, Box::new(Focus));

    let first = runtime.paste(&video_payload_at("C:/library/objects/a.mp4"));
    assert!(first.injected, "notice={}", first.text);
    let keys_after_first = recorder.lock().unwrap().keys.len();

    // 合并只针对同素材：窗口内连点不同素材是切换意图，两次都要上框。
    let second = runtime.paste(&video_payload_at("C:/library/objects/b.mp4"));
    assert!(second.injected, "notice={}", second.text);
    assert_eq!(recorder.lock().unwrap().keys.len(), keys_after_first + 1);
}

#[test]
fn runtime_pastes_video_by_file_path_without_enter() {
    let recorder: Shared = Arc::default();
    let mut runtime = runtime(&recorder);

    let notice = runtime.paste(&AssetPayload {
        kind: AssetKind::Video,
        png_bytes: &[],
        source_path: PathBuf::from("C:/library/objects/clip.mp4"),
        text: String::new(),
    });

    assert_eq!(
        notice.tone,
        TargetNoticeTone::Warning,
        "探测不确定时照常注入但标 unverified，文案提示用户确认：{}",
        notice.text
    );

    let log = recorder.lock().unwrap();
    assert_eq!(log.activations, vec![TARGET], "上框前必须激活目标窗口");
    assert_eq!(
        log.focuses,
        vec![(
            TARGET,
            FocusPlan {
                steps: vec![
                    FocusStep::AlreadyEditable,
                    FocusStep::UiaSetFocus,
                    FocusStep::AnchorClick,
                ],
                anchor: None,
            }
        )],
        "激活后必须对同一目标做一次聚焦；该画像未声明策略故取三级缺省、未声明锚点故 anchor 为 None"
    );
    assert_eq!(
        log.payloads,
        vec![ClipboardPayload::Files(vec![PathBuf::from(
            "C:/library/objects/clip.mp4"
        )])],
        "视频走 HDROP 文件路径"
    );
    assert_eq!(
        log.keys,
        vec![vec![0x11u16, 0x56, 0x8056, 0x8011]],
        "只允许 Ctrl+V 一个和弦"
    );
    assert!(
        log.keys.iter().all(|chord| !chord.contains(&0x0D)),
        "红线：上框绝不合成 Enter"
    );
}
