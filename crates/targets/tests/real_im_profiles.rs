//! 用真实桌面枚举（OpenDesktop("default")）抓到的窗口签名驱动匹配回归。
//!
//! 注意：这些签名来自 2026-08-23 对 Admin 交互桌面的真实窗口枚举，不是推演。
//! 值来源见 profiles/profiles.builtin.toml 与 DECISIONS.md A5 复核。

use platform::{WindowHandle, WindowRect, WindowSnapshot};
use targets::{
    load_profiles, matching_profile_windows, resolve_eligible_snapshot, ClipboardFormat,
    FormatKind, MatchResult,
};

const BUILTIN: &str = include_str!("../../../profiles/profiles.builtin.toml");

fn snapshot(
    hwnd: isize,
    exe: &str,
    class: &str,
    title: &str,
    visible: bool,
    minimized: bool,
) -> WindowSnapshot {
    WindowSnapshot {
        hwnd: WindowHandle(hwnd),
        exe_name: exe.to_string(),
        class_name: class.to_string(),
        title: title.to_string(),
        visible,
        minimized,
        rect: WindowRect {
            left: 0,
            top: 0,
            right: 960,
            bottom: 640,
        },
        process_id: hwnd as u32,
    }
}

#[test]
fn real_wechat_4_foreground_resolves_to_wechat_profile() {
    let set = load_profiles(BUILTIN, None).unwrap();
    // 真实微信 4.0 主窗口：Weixin.exe / Qt51514QWindowIcon / 标题微信
    let resolved = resolve_eligible_snapshot(
        &set,
        &snapshot(
            2163916,
            "Weixin.exe",
            "Qt51514QWindowIcon",
            "微信",
            true,
            true,
        ),
    )
    .expect("微信前台窗口应解析出目标");
    assert_eq!(resolved.profile.id.as_str(), "wechat");
    // matcher 会用 profile label + 真实窗口标题做会话级区分；标题恰为 "微信" 时拼接可读。
    assert_eq!(resolved.binding.label, "微信 (4.0) · 微信");
    assert!(resolved.binding.minimized, "最小化窗口应被标记而非丢弃");
}

#[test]
fn real_qianniu_foreground_resolves_to_qianniu_profile() {
    let set = load_profiles(BUILTIN, None).unwrap();
    let resolved = resolve_eligible_snapshot(
        &set,
        &snapshot(
            2098376,
            "AliWorkbench.exe",
            "Qt5152QWindowIcon",
            "千牛工作台",
            false,
            false,
        ),
    )
    .expect("千牛前台窗口应解析出目标");
    assert_eq!(resolved.profile.id.as_str(), "qianniu");
}

#[test]
fn real_pdd_foreground_resolves_to_pdd_profile() {
    let set = load_profiles(BUILTIN, None).unwrap();
    let resolved = resolve_eligible_snapshot(
        &set,
        &snapshot(
            656860,
            "PddWorkbench.exe",
            "g_wszPDDWindowClass",
            "拼多多工作台",
            false,
            false,
        ),
    )
    .expect("拼多多前台窗口应解析出目标");
    assert_eq!(resolved.profile.id.as_str(), "pdd");
}

#[test]
fn two_real_wechat_instances_are_ambiguous_not_silently_first() {
    let set = load_profiles(BUILTIN, None).unwrap();
    let profile = set.get(&"wechat".into()).unwrap();
    let result = matching_profile_windows(
        profile,
        &[
            snapshot(
                2163916,
                "Weixin.exe",
                "Qt51514QWindowIcon",
                "微信",
                true,
                true,
            ),
            snapshot(
                197440,
                "Weixin.exe",
                "Qt51514QWindowIcon",
                "微信",
                true,
                true,
            ),
        ],
    );
    // 两个窗口 exe/class/title 都命中，最高分并列，底层窗口匹配应保留两个候选；
    // 由上层 resolve_profile_windows / picker 决定用户选择。
    assert_eq!(result.len(), 2);
    let windows = targets::resolve_profile_windows(
        profile,
        &[
            snapshot(
                2163916,
                "Weixin.exe",
                "Qt51514QWindowIcon",
                "微信",
                true,
                true,
            ),
            snapshot(
                197440,
                "Weixin.exe",
                "Qt51514QWindowIcon",
                "微信",
                true,
                true,
            ),
        ],
    );
    assert!(
        matches!(windows, MatchResult::Ambiguous(items) if items.len() == 2),
        "同一 IM 多开不得静默选择第一个"
    );
}

/// 内置画像必须把 2026-08-25 的实测结论逐字带出来：图片优先交文件引用，
/// 因为对端只向外壳要缩略图（微信 436ms / 千牛 1027ms），而 CF_PNG 要对端
/// 在自己进程内全量解码同一张图（微信 2061ms / 千牛 3346ms）。
#[test]
fn builtin_image_route_prefers_file_reference_over_full_png() {
    let set = load_profiles(BUILTIN, None).unwrap();
    for id in ["wechat", "qianniu"] {
        let profile = set.get(&id.into()).unwrap();
        assert_eq!(
            profile.formats.for_kind(FormatKind::Image),
            [ClipboardFormat::Files, ClipboardFormat::Png],
            "{id} 的图片路由必须 files 优先、png 兜底"
        );
    }
}

/// 千牛的即发结论按类别分叉，这是画像里唯一允许「同格式不同结论」的地方：
/// 视频 HDROP 实测当场发出，图片 HDROP 实测停在输入框并显示真缩略图。
#[test]
fn builtin_qianniu_sends_only_video_hdrop_not_image_hdrop() {
    let set = load_profiles(BUILTIN, None).unwrap();
    let qianniu = set.get(&"qianniu".into()).unwrap();
    assert!(
        qianniu.paste_sends_format(FormatKind::Video, ClipboardFormat::Files),
        "千牛视频 HDROP 是粘贴即发送，必须保留 D18 保护"
    );
    assert!(
        !qianniu.paste_sends_format(FormatKind::Image, ClipboardFormat::Files),
        "千牛图片 HDROP 实测停在输入框，不得误判为即发而退回高成本 CF_PNG"
    );

    let wechat = set.get(&"wechat".into()).unwrap();
    for kind in [FormatKind::Image, FormatKind::Video] {
        assert!(
            !wechat.paste_sends_format(kind, ClipboardFormat::Files),
            "微信对 HDROP 一律停在输入框"
        );
    }
}
