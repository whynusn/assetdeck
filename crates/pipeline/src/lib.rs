//! 粘贴管线：格式协商 → 剪贴板 → 焦点校验 → 注入（绝不合成 Enter）。
//!
//! D8 红线（2026-09-15 清理后由**结构**保证）：「双击 = 素材进输入框」止步于此；
//! 注入序列只含 Ctrl+V 和弦，不含回车——M6 的 auto_send 布尔开关整代删除
//! （生产零调用者），任何改动不得把 0x0D 放回注入路径。
//!
//! 失败语义是降级而非中断：除剪贴板写入失败（唯一硬失败 → [`TargetPasteOutcome::Failed`]）
//! 外，其余一律返回可呈现的降级结果，交由调用方 UI 弹 toast。

pub mod feedback;
pub mod negotiate;

use std::time::Instant;

use platform::{
    ClipboardPayload, ClipboardSink, FocusOutcome, FocusWatcher, ForegroundRelation, InputFocuser,
    KeyInjector, ReadinessBlocker, ReadinessProbe, ReadinessSignal, WindowActivator,
};
use targets::{NotReadyReason, Profile, ReadinessMode, TargetBinding, TargetTracker};

pub use feedback::{FeedbackAction, FeedbackSeverity, PasteFeedback};
pub use negotiate::{
    negotiate_detailed, AssetPayload, Negotiated, NegotiationProfile, PayloadKind,
};

/// 虚拟键码：Ctrl（与 Win32 VK_CONTROL 同值；D12 平台事实）。
pub const VK_CONTROL: u16 = 0x11;
/// 虚拟键码：字母 V。Win32 头文件对字母键无常量，惯例取 ASCII 大写码。
pub const VK_V: u16 = 0x56;
/// 虚拟键码：回车。产品注入路径绝不出现——注入序列在结构上只含
/// Ctrl+V 和弦（守卫测试 core_upload_path_never_synthesizes_enter 锁定）。
pub const VK_RETURN: u16 = 0x0D;

/// Ctrl+V 和弦序列：Ctrl↓ V↓ V↑ Ctrl↑。
///
/// 按下/释放相位以 [`platform::KEY_UP`] 位标记；顺序不可重排——
/// Ctrl 必须在 V 之后才释放，否则和弦断裂。
fn chord_paste() -> Vec<u16> {
    vec![
        VK_CONTROL,
        VK_V,
        VK_V | platform::KEY_UP,
        VK_CONTROL | platform::KEY_UP,
    ]
}

/// 产品上框路径的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetPasteOutcome {
    Injected { verified: bool },
    CopiedOnly { feedback: PasteFeedback },
    Failed { feedback: PasteFeedback },
}

/// 管线的平台依赖集合：六个 trait 对象由装配层（app-ui）注入；
/// 测试以带共享操作日志的 mock 实现替换。
pub struct TargetPipelineDeps<'a> {
    pub clipboard: &'a mut dyn ClipboardSink,
    pub focus: &'a dyn FocusWatcher,
    pub injector: &'a mut dyn KeyInjector,
    pub activator: &'a dyn WindowActivator,
    pub readiness: &'a dyn ReadinessProbe,
    /// 把键盘焦点送进聊天输入框。仅激活窗口不足以让输入框拿到焦点，
    /// 缺了这一步 Ctrl+V 会落到窗口根控件上（真机实测的主要失败模式）。
    pub focuser: &'a dyn InputFocuser,
}

/// 一次「锁定目标 → 上框」的会话状态：持有常驻追踪器锁定的热目标。
///
/// 编排顺序（操作日志断言对象，见 target_routing_spec）：
/// `negotiate_detailed → write_clipboard → would_send 红线 → activate →
/// focus → readiness → 前台漂移复检 → inject Ctrl+V`
#[derive(Default)]
pub struct PasteSession {
    target: Option<TargetBinding>,
}

impl PasteSession {
    pub fn new() -> Self {
        Self { target: None }
    }

    /// 从常驻追踪器锁定本次上框目标。面板前台本身不会参与目标选择。
    pub fn begin_targeted(&mut self, tracker: &TargetTracker) {
        self.target = tracker.hot().cloned();
    }

    pub fn set_target(&mut self, target: Option<TargetBinding>) {
        self.target = target;
    }

    pub fn target(&self) -> Option<&TargetBinding> {
        self.target.as_ref()
    }

    /// M8 核心路径：只负责把素材放入已锁定目标的输入框，绝不自动发送。
    pub fn paste_targeted(
        &mut self,
        req: &AssetPayload<'_>,
        profile: &Profile,
        deps: &mut TargetPipelineDeps<'_>,
    ) -> TargetPasteOutcome {
        // 分段耗时瀑布（D41）：低配机的延迟归因不再靠猜——成功路径的每一段
        // 都进 Info 日志，一次上框就能看出大头在哪。微秒为段单位（事件驱动
        // 快路径各段常在 1ms 以下），总耗时用毫秒。
        let started = Instant::now();
        let target_label = self
            .target
            .as_ref()
            .map(|target| target.label.clone())
            .unwrap_or_else(|| profile.label.clone());
        // 「只上框不发送」红线在此收口：协商区分「安全格式」与「粘贴即发送格式」，
        // 后者只复制、绝不注入（否则 Ctrl+V 在千牛这类目标上等于直接发出消息）。
        let (payload, format, would_send) = match negotiate_detailed(req, profile) {
            Negotiated::Safe {
                payload, format, ..
            } => (payload, format, false),
            Negotiated::WouldSend {
                payload, format, ..
            } => (payload, format, true),
            Negotiated::Unsupported => {
                // D82 文件载荷的降级语义：目标未声明 file 行（或尚无热目标，
                // 落到通用画像）时不注入，但文件仍写剪贴板供用户手动粘贴。
                // 这是「日志导出」在未实测目标上的唯一安全落点——既不放行
                // 可能落空的 Ctrl+V，也不让用户拿不到日志文件。
                if req.kind == PayloadKind::File && !req.source_path.as_os_str().is_empty() {
                    let payload = ClipboardPayload::Files(vec![req.source_path.clone()]);
                    if let Err(error) = deps.clipboard.write(&payload) {
                        return TargetPasteOutcome::Failed {
                            feedback: PasteFeedback::failed(target_label, error.to_string()),
                        };
                    }
                    return copied_only(
                        target_label,
                        NotReadyReason::PayloadUnsupported,
                        "目标画像未声明 file 行，已复制文件供手动粘贴",
                    );
                }
                return TargetPasteOutcome::Failed {
                    feedback: PasteFeedback::failed(
                        target_label,
                        "画像声明的剪贴板格式均无法承载当前素材",
                    ),
                };
            }
        };
        if let Err(error) = deps.clipboard.write(&payload) {
            return TargetPasteOutcome::Failed {
                feedback: PasteFeedback::failed(target_label, error.to_string()),
            };
        }
        let after_write = Instant::now();
        if would_send {
            return copied_only(
                target_label,
                NotReadyReason::PasteWouldSend,
                "该目标对此格式的粘贴等于直接发送，按只上框红线中止注入",
            );
        }

        let Some(target) = self.target.as_ref() else {
            return copied_only(target_label, NotReadyReason::NoTarget, "没有已锁定的热目标");
        };
        let Some(hwnd) = target.hwnd else {
            return copied_only(
                target.label.clone(),
                NotReadyReason::WindowGone,
                "目标身份存在但当前没有窗口绑定",
            );
        };
        match deps.activator.activate(hwnd, 200, profile.settle_ms) {
            Ok(true) => {}
            Ok(false) => {
                return copied_only(
                    target.label.clone(),
                    NotReadyReason::Starting,
                    "目标窗口未在 200ms 内成为前台",
                )
            }
            Err(error) => {
                return copied_only(
                    target.label.clone(),
                    NotReadyReason::WindowGone,
                    error.to_string(),
                )
            }
        }
        let after_activate = Instant::now();

        // 激活只把窗口提到前台，键盘焦点仍停在窗口根控件（微信/千牛实测）。
        // 必须在探测与注入之前显式把焦点送进输入框，否则 Ctrl+V 会落空：
        // 素材进了剪贴板，却没有进任何输入框。聚焦动作只允许 UIA SetFocus 或
        // 画像锚点单击，绝不含键盘事件——「只上框不发送」的红线在此仍然成立。
        let focus_report = deps.focuser.focus_input(hwnd, &profile.focus_plan());
        let after_focus = Instant::now();
        if focus_report.outcome == FocusOutcome::Unavailable
            && profile.readiness == ReadinessMode::UiaStrict
        {
            return copied_only(
                target.label.clone(),
                NotReadyReason::ProbeUnavailable,
                "未能把键盘焦点送进输入框，按严格就绪模式中止注入",
            );
        }
        // 聚焦现场进 Info：锚点几何形态、实际点击点、客户区尺寸、DPI、事件等待
        // 结局一次记全。「报成功但没落框」的远程排障只需 diff 两台机器的这两行。
        log::info!(
            "上框聚焦现场 target={} outcome={:?} attempts={:?}",
            target_label,
            focus_report.outcome,
            focus_report.attempts
        );
        // 聚焦成功会让随后的 UIA 探测更可能给出 Ready；聚焦不确定则沿用
        // 「否证阻塞才不注入」的既有语义，注入并标 verified=false。
        // 锚点路径的正证据升级：单击后等到「输入迹象」事件（焦点/插入符）才把
        // verified 升为 true——旧实现 FocusedByAnchor 恒为 false，把唯一的
        // 事后可证路径埋进了 debug 日志。
        let focus_verified = matches!(
            focus_report.outcome,
            FocusOutcome::AlreadyEditable | FocusOutcome::FocusedByUia
        ) || focus_report.anchor_click_observed();

        // 只有严格档才值得为 UIA 往返付 3~30ms：按 D15，浅探测在微信/千牛上
        // 从未返回 Ready，其结论恒为 Inconclusive，`verified` 实际由 FocusOutcome 决定。
        // 因此非严格档改走 blockers() —— 保留全部否证能力（窗口消失 / 模态阻塞），
        // 去掉不产生信息量的开销。`verified` 只影响提示文案，不影响是否上框。
        let signal = match profile.readiness {
            ReadinessMode::UiaStrict => deps.readiness.probe(hwnd, 50),
            ReadinessMode::UiaShallow | ReadinessMode::P0Only => deps.readiness.blockers(hwnd),
        };
        let verified = match signal {
            ReadinessSignal::Ready => true,
            ReadinessSignal::Inconclusive if profile.readiness == ReadinessMode::UiaStrict => {
                return copied_only(
                    target.label.clone(),
                    NotReadyReason::ProbeUnavailable,
                    "UIA 未证明当前存在可写输入框，按严格就绪模式中止注入",
                )
            }
            ReadinessSignal::Inconclusive => focus_verified,
            ReadinessSignal::Blocked(blocker) => {
                return copied_only(
                    target.label.clone(),
                    map_blocker(blocker),
                    "输入框就绪度探测明确否证",
                )
            }
        };

        let alive = deps.focus.is_alive(hwnd);
        let fg = deps.focus.foreground();
        let after_preinject = Instant::now();
        // 注入前最后一道门的状态快照（D44 前台归属 / D21 焦点结果 / verified）。
        // Debug 级 + paste_trace::pipeline target：与 settle/事件明细同一 grep 域，
        // 默认 Info 下零格式化开销，verbose_diagnostics 开启后进文件可回溯。
        log::debug!(
            target: "paste_trace::pipeline",
            "preinject alive={alive} fg={:?} target_hwnd={:?} focus_outcome={:?} signal_verified={verified}",
            fg.0,
            hwnd.0,
            focus_report.outcome
        );
        if !alive {
            return copied_only(
                target.label.clone(),
                NotReadyReason::WindowGone,
                "注入前目标窗口已失活",
            );
        }
        if fg != hwnd {
            // 前台漂移 ≠ 目标关闭（D44，低配机日志实测约半数上框止步于此）：
            // activate 刚确认过前台，到注入前又被挪走——大头是用户连点素材面板
            // （前台回到自己进程）或目标内部多顶层表面抖动（前台仍在目标进程）。
            // 这两类「上框意图未变」，用一次快速再激活复检前台后照常注入；只有
            // 第三方前台才是用户真的切走了，按红线降级为仅复制、绝不抢回前台。
            let relation = deps.focus.foreground_relation(fg, hwnd);
            if !matches!(
                relation,
                ForegroundRelation::OwnProcess | ForegroundRelation::SameAsTarget
            ) {
                return copied_only(
                    target.label.clone(),
                    NotReadyReason::ForegroundLost,
                    "注入前前台被第三方窗口占据",
                );
            }
            log::info!(
                "注入前前台漂移 relation={relation:?} focus_outcome={:?}，尝试再断言",
                focus_report.outcome
            );
            let reasserted = matches!(
                deps.activator
                    .activate(hwnd, FOREGROUND_REASSERT_BUDGET_MS, 0),
                Ok(true)
            ) && deps.focus.foreground() == hwnd;
            if !reasserted {
                return copied_only(
                    target.label.clone(),
                    NotReadyReason::ForegroundLost,
                    "前台再断言后仍未回到目标窗口",
                );
            }
        }
        if let Err(error) = deps.injector.inject(&chord_paste()) {
            return TargetPasteOutcome::CopiedOnly {
                feedback: PasteFeedback::copied(
                    target.label.clone(),
                    "系统拒绝向目标输入，请在目标应用中手动粘贴",
                    FeedbackAction::Retry,
                )
                .with_diagnostic(error.to_string()),
            };
        }
        let finished = Instant::now();
        log::info!(
            "上框耗时分布 target={} format={:?} write={}us activate={}us focus={}us \
             readiness={}us inject={}us total={}ms verified={}",
            target_label,
            format,
            after_write.duration_since(started).as_micros(),
            after_activate.duration_since(after_write).as_micros(),
            after_focus.duration_since(after_activate).as_micros(),
            after_preinject.duration_since(after_focus).as_micros(),
            finished.duration_since(after_preinject).as_micros(),
            finished.duration_since(started).as_millis(),
            verified,
        );
        TargetPasteOutcome::Injected { verified }
    }
}

/// 注入前前台再断言的确认预算（D44）。漂移修复只补一次快速激活：前台通常
/// 已在目标或本进程之间来回，SetForegroundWindow 立即可达，100ms 足够覆盖
/// 低配机的一轮事件确认；再失败就按 ForegroundLost 降级，不无限纠缠。
const FOREGROUND_REASSERT_BUDGET_MS: u64 = 100;

fn copied_only(
    target_label: String,
    reason: NotReadyReason,
    diagnostic: impl Into<String>,
) -> TargetPasteOutcome {
    let diagnostic = diagnostic.into();
    // 真实低配机日志（2026-08-27）显示约 30% 的上框降级只留下「目标窗口已关闭」
    // 这个 headline，无法区分「无窗口绑定 / 激活失败 / 注入前前台漂移」三个分支，
    // diagnostic 必须进日志文件（D42）；耗时由壳层「上框请求/完成」两条日志的
    // 时间戳差值给出。
    log::info!("上框降级 reason={reason:?} target={target_label} diagnostic={diagnostic}");
    TargetPasteOutcome::CopiedOnly {
        feedback: PasteFeedback::not_ready(target_label, reason).with_diagnostic(diagnostic),
    }
}

fn map_blocker(blocker: ReadinessBlocker) -> NotReadyReason {
    match blocker {
        ReadinessBlocker::NotLoggedIn => NotReadyReason::NotLoggedIn,
        ReadinessBlocker::NoConversation => NotReadyReason::NoConversation,
        ReadinessBlocker::ReadOnly => NotReadyReason::ReadOnly,
        ReadinessBlocker::ModalBlocking => NotReadyReason::ModalBlocking,
        ReadinessBlocker::Starting => NotReadyReason::Starting,
        ReadinessBlocker::WindowGone => NotReadyReason::WindowGone,
    }
}
