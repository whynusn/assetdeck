//! 闭环计时探针：`double_click → OpenAsset → negotiate → 真实 Win32 剪贴板写+读回校验
//! → 焦点死降级 CopiedOnly`。
//!
//! 诚实边界（PRD/A2）：「双击→输入框」的真实端到端含 IM 目标窗口与 SendInput，
//! 无法诚实自动化。本探针覆盖**自动化段**；真实 SendInput 进输入框由
//! `real_sendinput_into_notepad`（#[ignore]）人工补全。

use std::path::PathBuf;
use std::time::Instant;

use pipeline::{
    negotiate, AssetKind, AssetPayload, PasteConfig, PasteOutcome, PasteSession, PipelineDeps,
    TargetProfile,
};
use platform::{
    win32::{Win32Clipboard, Win32Focus},
    ClipboardPayload, ClipboardSink, FocusWatcher, KeyInjector, WindowHandle,
};
use ui_viewmodels::{
    Asset, AssetId, CategoryId, FacetIndex, LibraryGridVm, Sorter, TagId, VmEvent,
};

/// 探针载荷文本（读回校验的逐字比对目标）。
pub const PROBE_TEXT: &str = "closed-loop-probe";

/// 探针库规模：小合成库足以驱动完整管线（规模守卫属 RSS 测试职责）。
const PROBE_ASSETS: u32 = 50;
/// 被双击的资产 id。
const PROBE_ID: u32 = 23;

/// 探针结果。到达 Ok 即代表：CopiedOnly 降级发生且剪贴板读回逐字一致。
#[derive(Debug)]
pub struct ClosedLoopReport {
    /// 双击 → 读回校验完成的全耗时。best-effort 断言用（D10/A2 自动化段；
    /// 真实 SendInput 段不在内——见模块注释诚实边界）。
    pub elapsed_ms: u128,
    /// CopiedOnly 的降级原因（预期为焦点失活路径），供 CI 日志呈现。
    pub copied_only_reason: String,
}

/// 包装 FocusWatcher：foreground 走真实 [`Win32Focus`]（begin_panel 锚定此刻真前台，
/// 通常为测试宿主控制台窗口）；is_alive 恒 false——模拟 CI 无 IM 目标窗口，
/// 驱动管线走「前一前台窗口已失活 → CopiedOnly」降级分支而非 Injected。
struct DeadTargetFocus;

impl FocusWatcher for DeadTargetFocus {
    fn foreground(&self) -> WindowHandle {
        Win32Focus.foreground()
    }

    fn is_alive(&self, _window: WindowHandle) -> bool {
        false
    }
}

/// 测试内 Noop 注入器：CopiedOnly 分支不会触达注入，仅为满足 PipelineDeps 形参。
struct NoopInjector;

impl KeyInjector for NoopInjector {
    fn inject(&mut self, _keys: &[u16]) -> platform::Result<()> {
        Ok(())
    }
}

/// 跑一次闭环探针。函数体内 paste 之后不再使用 `?`——清理剪贴板的路径无条件执行。
pub fn run_closed_loop_probe() -> Result<ClosedLoopReport, String> {
    // 小合成库进 FacetIndex → VM
    let mut idx = FacetIndex::new();
    for i in 0..PROBE_ASSETS {
        idx.insert(&Asset {
            id: AssetId(i),
            name: format!("probe-{i}.png"),
            category: Some(CategoryId(i % 3)),
            tags: vec![TagId(i % 5)],
            created_at: i as i64,
        });
    }
    let mut vm = LibraryGridVm::new(idx, Sorter::default(), 16);

    // 双击语义止步于 OpenAsset 事件（D8 红线）
    vm.double_click(AssetId(PROBE_ID));
    let started = Instant::now();

    let opened = vm
        .take_events()
        .into_iter()
        .find(|e| *e == VmEvent::OpenAsset(AssetId(PROBE_ID)));
    let Some(VmEvent::OpenAsset(_)) = opened else {
        return Err(format!(
            "take_events 未产出 OpenAsset({PROBE_ID})，事件队列异常"
        ));
    };

    // 会话：auto_send 默认关（D8 快照测试锁定）；锚定真前台
    let mut session = PasteSession::new(PasteConfig::default());
    session.begin_panel(&DeadTargetFocus);

    // 载荷走协商表 Text 行（Image/Video 行的剪贴板形态不属本探针职责）
    let payload = AssetPayload {
        kind: AssetKind::Text,
        png_bytes: &[],
        source_path: PathBuf::new(),
        text: PROBE_TEXT.to_string(),
    };
    let Some(negotiated) = negotiate(&payload, TargetProfile::ImGeneric) else {
        return Err("negotiate 未映射 (Text × ImGeneric) 行".into());
    };
    debug_assert!(matches!(negotiated, ClipboardPayload::Text(_)));

    let mut sink = Win32Clipboard;
    let mut injector = NoopInjector;
    let mut deps = PipelineDeps {
        clipboard: &mut sink,
        focus: &DeadTargetFocus,
        injector: &mut injector,
    };
    let outcome = session.paste(&payload, &mut deps);

    // 读回校验（真实系统剪贴板）：CF_UNICODETEXT 须逐字等于载荷文本。
    // 选直接读回而非 sequence-number 前后比对：内容级证据更硬（design 备选二）。
    let read_back = read_cf_unicode_text();

    let reason = match outcome {
        PasteOutcome::CopiedOnly { reason } => reason,
        other => {
            cleanup_clipboard(&mut sink);
            return Err(format!("期望 CopiedOnly（焦点死降级），实际 {other:?}"));
        }
    };

    let read_ok = read_back.as_deref() == Some(PROBE_TEXT);
    // 无条件清理：覆盖写空串（Text("") 编码为单 NUL 字节载荷，合法写入）
    cleanup_clipboard(&mut sink);
    let elapsed_ms = started.elapsed().as_millis();

    if !read_ok {
        return Err(format!(
            "剪贴板读回校验失败: {read_back:?} != {PROBE_TEXT:?}"
        ));
    }
    Ok(ClosedLoopReport {
        elapsed_ms,
        copied_only_reason: reason,
    })
}

fn cleanup_clipboard(sink: &mut Win32Clipboard) {
    let _ = sink.write(&ClipboardPayload::Text(String::new()));
}

/// 直调 Win32 读回 CF_UNICODETEXT 文本（platform crate 未暴露读 API，design 允许）。
fn read_cf_unicode_text() -> Option<String> {
    #[cfg(windows)]
    unsafe {
        use std::ptr;
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, GetClipboardData, OpenClipboard,
        };
        use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
        use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

        // 安全：标准 Open→Lock→读→Unlock→Close 序列；句柄归系统所有，不释放。
        let mut opened = OpenClipboard(ptr::null_mut());
        if opened == 0 {
            // 与写入端同款竞争重试
            std::thread::sleep(std::time::Duration::from_millis(10));
            opened = OpenClipboard(ptr::null_mut());
        }
        if opened == 0 {
            return None;
        }
        let out = (|| {
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT));
            if handle.is_null() {
                return None;
            }
            let wide = GlobalLock(handle) as *const u16;
            if wide.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *wide.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(wide, len);
            let text = String::from_utf16_lossy(slice);
            GlobalUnlock(handle);
            Some(text)
        })();
        CloseClipboard();
        out
    }

    #[cfg(not(windows))]
    {
        None
    }
}
