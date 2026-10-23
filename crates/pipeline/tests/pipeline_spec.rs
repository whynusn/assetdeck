//! M8 入口守卫测试集（原 M6 六测的收敛改写，TDD_PLAN 红线方向保留）。
//!
//! 全部基于 mock + 共享操作日志（Op）断言顺序与内容，零真实 Win32。
//! M6 的旧入口（PasteSession::paste + PipelineDeps + auto_send 开关）已整代
//! 删除：完整产品路径的操作顺序、注入和弦与「结构上无回车」红线由
//! target_routing_spec.rs 守卫；本文件守协商表基线与「无热目标」降级路径。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pipeline::{
    negotiate_detailed, AssetPayload, Negotiated, PasteSession, PayloadKind, TargetPasteOutcome,
    TargetPipelineDeps,
};
use platform::{ClipboardPayload, ClipboardSink, FocusWatcher, KeyInjector, Result, WindowHandle};
use targets::Profile;

// ---------------------------------------------------------------------------
// 操作日志与 mock 基建（design.md：单一 Vec<Op> 共享给 mock）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    WriteClipboard(ClipboardPayload<'static>),
}

/// Arc<Mutex<Vec<Op>>> 薄封装：mock 各持 clone，写入同一份时序日志。
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Op>>>);

impl Log {
    fn push(&self, op: Op) {
        self.0.lock().unwrap().push(op);
    }

    fn snapshot(&self) -> Vec<Op> {
        self.0.lock().unwrap().clone()
    }
}

struct MockSink(Log);

impl ClipboardSink for MockSink {
    fn write(&mut self, payload: &ClipboardPayload<'_>) -> Result<()> {
        self.0
            .push(Op::WriteClipboard(payload.clone().into_owned()));
        Ok(())
    }
}

/// 无目标场景替身：不会参与任何调用（会话未持目标时管线在写剪贴板后即降级）。
struct DeadFocus;
impl FocusWatcher for DeadFocus {
    fn foreground(&self) -> WindowHandle {
        WindowHandle(0)
    }

    fn is_alive(&self, _: WindowHandle) -> bool {
        false
    }
}

struct NoopInjector;
impl KeyInjector for NoopInjector {
    fn inject(&mut self, _keys: &[u16]) -> Result<()> {
        Ok(())
    }
}

struct NoopActivator;
impl platform::WindowActivator for NoopActivator {
    fn activate(&self, _: WindowHandle, _: u64, _: u64) -> Result<bool> {
        Ok(false)
    }
}

struct NoopProbe;
impl platform::ReadinessProbe for NoopProbe {
    fn probe(&self, _: WindowHandle, _: u64) -> platform::ReadinessSignal {
        platform::ReadinessSignal::Inconclusive
    }

    fn blockers(&self, _: WindowHandle) -> platform::ReadinessSignal {
        platform::ReadinessSignal::Inconclusive
    }
}

struct NoopFocuser;
impl platform::InputFocuser for NoopFocuser {
    fn focus_input(&self, _: WindowHandle, _: &platform::FocusPlan) -> platform::FocusReport {
        platform::FocusReport {
            outcome: platform::FocusOutcome::Unavailable,
            attempts: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

fn text_payload<'a>(text: &'a str) -> AssetPayload<'a> {
    AssetPayload {
        kind: PayloadKind::Text,
        png_bytes: &[],
        source_path: PathBuf::from("C:/library/objects/u3/raw.txt"),
        text: text.to_string(),
    }
}

/// 组装 mock 并对**未持目标**的会话执行一次 paste_targeted。
fn run_paste_without_target(req: &AssetPayload<'_>, log: &Log) -> TargetPasteOutcome {
    let mut sink = MockSink(log.clone());
    let mut injector = NoopInjector;
    let mut deps = TargetPipelineDeps {
        clipboard: &mut sink,
        focus: &DeadFocus,
        injector: &mut injector,
        activator: &NoopActivator,
        readiness: &NoopProbe,
        focuser: &NoopFocuser,
    };
    let mut session = PasteSession::new();
    session.paste_targeted(req, &Profile::generic(), &mut deps)
}

// ---------------------------------------------------------------------------
// M8 入口守卫
// ---------------------------------------------------------------------------

/// 协商表基线（经通用 Profile 的缺省 formats）：Image → PNG 透传、
/// Video → 源路径 HDROP、Text → Unicode 文本、未知组合 → Unsupported。
#[test]
fn format_negotiation_table_image_video_text() {
    let profile = Profile::generic();

    // Image → PNG 字节透传（不重编码）。
    let png = vec![0x89, b'P', b'N', b'G'];
    let img = AssetPayload {
        kind: PayloadKind::Image,
        png_bytes: &png,
        source_path: PathBuf::from("C:/library/objects/u1/raw.png"),
        text: String::new(),
    };
    assert_eq!(
        negotiate_detailed(&img, &profile),
        Negotiated::Safe {
            format: targets::ClipboardFormat::Png,
            payload: ClipboardPayload::Png(png.as_slice().into())
        }
    );

    // Video → Files 且携带源路径。
    let video_path = PathBuf::from("C:/library/objects/u2/raw.mp4");
    let video = AssetPayload {
        kind: PayloadKind::Video,
        png_bytes: &[],
        source_path: video_path.clone(),
        text: String::new(),
    };
    assert_eq!(
        negotiate_detailed(&video, &profile),
        Negotiated::Safe {
            format: targets::ClipboardFormat::Files,
            payload: ClipboardPayload::Files(vec![video_path])
        }
    );

    // Text → Text。
    let text = text_payload("你好 IM");
    assert_eq!(
        negotiate_detailed(&text, &profile),
        Negotiated::Safe {
            format: targets::ClipboardFormat::Text,
            payload: ClipboardPayload::Text("你好 IM".into())
        }
    );

    // 未知组合（未路由的 Other 类资产）→ Unsupported，调用方报不支持。
    let other = AssetPayload {
        kind: PayloadKind::Other,
        png_bytes: &[],
        source_path: PathBuf::from("C:/library/objects/u4/raw.zip"),
        text: String::new(),
    };
    assert_eq!(
        negotiate_detailed(&other, &profile),
        Negotiated::Unsupported
    );
}

/// 红线顺序（原 M6 paste_writes_clipboard_before_focus_switch 的 M8 等价）：
/// 未持目标的会话在**写完剪贴板之后**才降级——复制必须先完成，
/// 注入绝不能发生（禁止重试后强注）。
#[test]
fn paste_without_target_writes_clipboard_then_degrades_to_copy_only() {
    let log = Log::default();
    let req = text_payload("你好 IM");
    let outcome = run_paste_without_target(&req, &log);
    assert!(
        matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }),
        "无热目标应得 CopiedOnly 降级，实际 {outcome:?}"
    );

    let ops = log.snapshot();
    assert_eq!(ops.len(), 1, "降级前应恰好发生一次剪贴板写入: {ops:?}");
    assert!(
        matches!(ops.first(), Some(Op::WriteClipboard(_))),
        "复制必须已完成(剪贴板已写入): {ops:?}"
    );
}

/// D82 日志导出：没有热目标（通用画像未声明 file 行）时不能报硬失败——
/// 文件仍写剪贴板，用户可手动粘贴；注入绝不发生。
#[test]
fn file_payload_without_target_copies_instead_of_failing() {
    let log = Log::default();
    let file = AssetPayload {
        kind: PayloadKind::File,
        png_bytes: &[],
        source_path: PathBuf::from("C:/logs/app-1-2.log"),
        text: String::new(),
    };
    let outcome = run_paste_without_target(&file, &log);
    assert!(
        matches!(outcome, TargetPasteOutcome::CopiedOnly { .. }),
        "无热目标的文件应降级为仅复制，实际 {outcome:?}"
    );
    assert_eq!(
        log.snapshot(),
        vec![Op::WriteClipboard(ClipboardPayload::Files(vec![
            PathBuf::from("C:/logs/app-1-2.log")
        ]))],
        "应恰好写入一次文件剪贴板，且不注入"
    );
}

/// 「无法承载」与「降级」是两种不同结论：非 File 的协商 Unsupported 仍是
/// 硬失败，连剪贴板都不写（video 行置空同款纪律，见 target_routing_spec）。
#[test]
fn unsupported_payload_never_touches_clipboard() {
    let log = Log::default();
    let other = AssetPayload {
        kind: PayloadKind::Other,
        png_bytes: &[],
        source_path: PathBuf::from("C:/library/objects/u4/raw.zip"),
        text: String::new(),
    };
    let outcome = run_paste_without_target(&other, &log);
    assert!(
        matches!(outcome, TargetPasteOutcome::Failed { .. }),
        "未路由组合应得硬失败，实际 {outcome:?}"
    );
    assert!(
        log.snapshot().is_empty(),
        "协商失败时连剪贴板都不得触碰: {:?}",
        log.snapshot()
    );
}
