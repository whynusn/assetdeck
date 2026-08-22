//! 红灯测试 4：`closed_loop_doubleclick_to_input_box_under_500ms`
//! （PRD 需求 4 / 行动项 A2 自动化部分，常规测试，cfg(windows)——CI 恒 windows-latest）。
//!
//! 诚实边界（PRD）：真实「双击→输入框」含 IM 目标窗口与 SendInput，
//! 无法诚实自动化。本测试覆盖自动化段：
//! `VM.double_click → VmEvent::OpenAsset → negotiate → 真实 Win32 剪贴板写入
//! + 读回校验 → 焦点死降级 CopiedOnly`（探针内部用 alive 恒 false 的包装
//! FocusWatcher 模拟 CI 无 IM 目标）。
//! 真实 SendInput 进输入框由 `real_sendinput_into_notepad`（#[ignore]）人工补全。

#![cfg(windows)]

use bench_harness::closed_loop::{run_closed_loop_probe, PROBE_TEXT};

#[test]
fn closed_loop_doubleclick_to_input_box_under_500ms() {
    let report = run_closed_loop_probe().expect("闭环探针执行失败");

    // 门 1：CopiedOnly 语义断言——焦点失活降级路径必须发生（而非 Injected/Failed）
    assert_eq!(
        report.copied_only_reason, "前一前台窗口已失活",
        "降级原因应来自焦点校验失败路径"
    );

    // 门 1：读回校验真实发生——run_closed_loop_probe 内部已做 CF_UNICODETEXT
    // 逐字比对并以此为准返回 Err；此处再锁一次载荷常量防漂移。
    assert!(!PROBE_TEXT.is_empty());

    // best-effort 时延断言（D10/A2）：仅约束自动化管线段
    // （VM 事件 → 协商 → 剪贴板写+读回），不含真实 IM 窗口接收侧。
    // CI 共享宿主的剪贴板/调度抖动可能偶发放大，超预算即红、不静默放宽。
    const AUTOMATED_SEGMENT_BUDGET_MS: u128 = 500;
    assert!(
        report.elapsed_ms < AUTOMATED_SEGMENT_BUDGET_MS,
        "闭环自动化段 {}ms ≥ {}ms 预算（best-effort，D10/A2）",
        report.elapsed_ms,
        AUTOMATED_SEGMENT_BUDGET_MS
    );
}
