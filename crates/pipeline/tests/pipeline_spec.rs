//! M6 守卫测试集：六个自动化测试，TDD_PLAN 清单逐字命名。
//!
//! 全部基于 mock + 共享操作日志（Op）断言顺序与内容，零真实 Win32。
//! 断言纪律（implement.md 门 2）：红线顺序用**下标精确比较**而非宽松 contains；
//! 唯一例外是回车检测——序列元素的释放相位带 [`KEY_UP`] 位，
//! 掩码后比对 VK_RETURN 属语义正确的例外。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pipeline::{
    negotiate, AssetKind, AssetPayload, PasteConfig, PasteOutcome, PasteSession, PipelineDeps,
    TargetProfile, VK_CONTROL, VK_RETURN, VK_V,
};
use platform::{
    ClipboardPayload, ClipboardSink, FocusWatcher, KeyInjector, Result, WindowHandle, KEY_UP,
};

// ---------------------------------------------------------------------------
// 操作日志与 mock 基建（design.md：单一 Vec<Op> 共享给三个 mock）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    WriteClipboard(ClipboardPayload),
    CheckAlive(WindowHandle),
    Inject(Vec<u16>),
}

/// Arc<Mutex<Vec<Op>>> 薄封装：三个 mock 各持 clone，写入同一份时序日志。
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
    fn write(&mut self, payload: &ClipboardPayload) -> Result<()> {
        self.0.push(Op::WriteClipboard(payload.clone()));
        Ok(())
    }
}

struct MockFocus {
    fg: WindowHandle,
    alive: bool,
    log: Log,
}

impl FocusWatcher for MockFocus {
    fn foreground(&self) -> WindowHandle {
        self.fg
    }

    fn is_alive(&self, window: WindowHandle) -> bool {
        self.log.push(Op::CheckAlive(window));
        self.alive
    }
}

struct MockInjector(Log);

impl KeyInjector for MockInjector {
    fn inject(&mut self, keys: &[u16]) -> Result<()> {
        self.0.push(Op::Inject(keys.to_vec()));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

const FG: WindowHandle = WindowHandle(0x00AB);

fn image_payload<'a>(png: &'a [u8]) -> AssetPayload<'a> {
    AssetPayload {
        kind: AssetKind::Image,
        png_bytes: png,
        source_path: PathBuf::from("C:/library/objects/u1/raw.png"),
        text: String::new(),
    }
}

/// 组装三 mock 并执行一次 paste；返回结果，操作留在 log 里供断言。
fn run_paste(
    session: &mut PasteSession,
    log: &Log,
    fg: WindowHandle,
    alive: bool,
    req: &AssetPayload<'_>,
) -> PasteOutcome {
    let mut sink = MockSink(log.clone());
    let focus = MockFocus {
        fg,
        alive,
        log: log.clone(),
    };
    let mut injector = MockInjector(log.clone());
    let mut deps = PipelineDeps {
        clipboard: &mut sink,
        focus: &focus,
        injector: &mut injector,
    };
    session.paste(req, &mut deps)
}

/// 抽取全部注入序列（保持时序）。
fn inject_sequences(ops: &[Op]) -> Vec<&Vec<u16>> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Inject(seq) => Some(seq),
            _ => None,
        })
        .collect()
}

/// 序列中是否存在回车键事件（掩掉释放相位位后比对）。
fn contains_enter(seq: &[u16]) -> bool {
    seq.iter().any(|&event| event & !KEY_UP == VK_RETURN)
}

// ---------------------------------------------------------------------------
// 六个自动化测试（名字逐字对应 TDD_PLAN M6）
// ---------------------------------------------------------------------------

#[test]
fn format_negotiation_table_image_video_text() {
    // Image → PNG 字节透传（不重编码）。
    let png = vec![0x89, b'P', b'N', b'G'];
    let img = image_payload(&png);
    assert_eq!(
        negotiate(&img, TargetProfile::ImGeneric),
        Some(ClipboardPayload::Png(png))
    );

    // Video → Files 且携带源路径。
    let video_path = PathBuf::from("C:/library/objects/u2/raw.mp4");
    let video = AssetPayload {
        kind: AssetKind::Video,
        png_bytes: &[],
        source_path: video_path.clone(),
        text: String::new(),
    };
    assert_eq!(
        negotiate(&video, TargetProfile::ImGeneric),
        Some(ClipboardPayload::Files(vec![video_path]))
    );

    // Text → Text。
    let text = AssetPayload {
        kind: AssetKind::Text,
        png_bytes: &[],
        source_path: PathBuf::from("C:/library/objects/u3/raw.txt"),
        text: "你好 IM".to_string(),
    };
    assert_eq!(
        negotiate(&text, TargetProfile::ImGeneric),
        Some(ClipboardPayload::Text("你好 IM".to_string()))
    );

    // 未知组合（未路由的 Other 类资产）→ None，调用方降级处理。
    let other = AssetPayload {
        kind: AssetKind::Other,
        png_bytes: &[],
        source_path: PathBuf::from("C:/library/objects/u4/raw.zip"),
        text: String::new(),
    };
    assert_eq!(negotiate(&other, TargetProfile::ImGeneric), None);
}

#[test]
fn auto_send_flag_defaults_off() {
    // 快照断言（D8 红线）：默认配置的序列化形态必须恰为字面量。
    assert_eq!(
        serde_json::to_string(&PasteConfig::default()).unwrap(),
        r#"{"auto_send":false}"#
    );
    // 反序列化同一形态必须无损还原（配置持久化边界契约）。
    assert_eq!(
        serde_json::from_str::<PasteConfig>(r#"{"auto_send":false}"#).unwrap(),
        PasteConfig::default()
    );
}

#[test]
fn previous_foreground_window_recorded_on_panel_invoke() {
    let log = Log::default();
    let focus = MockFocus {
        fg: FG,
        alive: true,
        log: log.clone(),
    };
    let mut session = PasteSession::new(PasteConfig::default());
    // 记录前无锚点：后续 paste 会因此降级（另一测试覆盖）。
    assert_eq!(session.previous_foreground(), None);

    session.begin_panel(&focus);
    assert_eq!(session.previous_foreground(), Some(focus.foreground()));
    assert_eq!(session.previous_foreground(), Some(FG));
}

#[test]
fn paste_writes_clipboard_before_focus_switch() {
    let log = Log::default();
    let png = vec![1, 2, 3];
    let req = image_payload(&png);
    let mut session = PasteSession::new(PasteConfig::default());
    session.begin_panel(&MockFocus {
        fg: FG,
        alive: true,
        log: log.clone(),
    });

    let outcome = run_paste(&mut session, &log, FG, true, &req);
    assert_eq!(outcome, PasteOutcome::Injected);

    let ops = log.snapshot();
    // 下标精确断言：写剪贴板严格先于焦点校验与注入。
    let write_idx = ops
        .iter()
        .position(|op| matches!(op, Op::WriteClipboard(_)))
        .expect("应有 WriteClipboard 操作");
    let check_idx = ops
        .iter()
        .position(|op| matches!(op, Op::CheckAlive(_)))
        .expect("应有 CheckAlive 操作");
    let inject_idx = ops
        .iter()
        .position(|op| matches!(op, Op::Inject(_)))
        .expect("应有 Inject 操作");
    assert!(write_idx < check_idx, "写剪贴板必须先于焦点校验: {ops:?}");
    assert!(write_idx < inject_idx, "写剪贴板必须先于注入: {ops:?}");
}

#[test]
fn focus_check_failure_degrades_to_copy_only() {
    let log = Log::default();
    let png = vec![7, 8, 9];
    let req = image_payload(&png);
    let mut session = PasteSession::new(PasteConfig::default());
    session.begin_panel(&MockFocus {
        fg: FG,
        alive: true, // 记录锚点时窗口还活着
        log: log.clone(),
    });

    // 注入时刻目标已死：降级为仅复制，而不是 Err。
    let outcome = run_paste(&mut session, &log, FG, false, &req);
    match outcome {
        PasteOutcome::CopiedOnly { reason } => {
            assert!(!reason.is_empty(), "降级原因应可呈现给 toast");
        }
        other => panic!("死窗口应得 CopiedOnly，实际 {other:?}"),
    }

    let ops = log.snapshot();
    // 复制已完成：剪贴板已写入。
    assert!(
        matches!(ops.first(), Some(Op::WriteClipboard(_))),
        "剪贴板应已写入(复制完成): {ops:?}"
    );
    // 红线：零注入，禁止重试后强注。
    assert!(
        ops.iter().all(|op| !matches!(op, Op::Inject(_))),
        "焦点校验失败后不得有任何注入: {ops:?}"
    );
}

#[test]
fn auto_send_off_never_synththesizes_enter() {
    let png = vec![4, 5, 6];
    let req = image_payload(&png);

    // —— 关（默认主路径）：所有注入序列均不得含回车。——
    let log_off = Log::default();
    let mut session_off = PasteSession::new(PasteConfig::default());
    session_off.begin_panel(&MockFocus {
        fg: FG,
        alive: true,
        log: log_off.clone(),
    });
    let outcome = run_paste(&mut session_off, &log_off, FG, true, &req);
    assert_eq!(outcome, PasteOutcome::Injected);

    let ops_off = log_off.snapshot();
    let off_seqs = inject_sequences(&ops_off);
    assert_eq!(off_seqs.len(), 1, "关开关应恰好一次注入: {off_seqs:?}");
    // 结构精确断言：Ctrl↓ V↓ V↑ Ctrl↑（和弦顺序不可重排）。
    assert_eq!(
        off_seqs[0].as_slice(),
        &[VK_CONTROL, VK_V, VK_V | KEY_UP, VK_CONTROL | KEY_UP],
        "关状态注入序列应为纯 Ctrl+V 和弦"
    );
    assert!(
        off_seqs.iter().all(|seq| !contains_enter(seq)),
        "auto-send 关闭时任何序列都不得合成回车"
    );

    // —— 开（对照组）：证明差异确实来自开关本身。——
    let log_on = Log::default();
    let mut session_on = PasteSession::new(PasteConfig { auto_send: true });
    session_on.begin_panel(&MockFocus {
        fg: FG,
        alive: true,
        log: log_on.clone(),
    });
    let outcome_on = run_paste(&mut session_on, &log_on, FG, true, &req);
    assert_eq!(outcome_on, PasteOutcome::Injected);

    let ops_on = log_on.snapshot();
    let on_seqs = inject_sequences(&ops_on);
    assert!(
        on_seqs.iter().any(|seq| contains_enter(seq)),
        "对照组(开)应存在含回车的序列: {on_seqs:?}"
    );
}
