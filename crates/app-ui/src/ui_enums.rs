//! UI 枚举 → 整数的**单一映射层**（综合分析报告「三.9」）。
//!
//! .slint 侧所有 0/1/2/3 魔法数字已收口到 UiEnums 全局常量；本文件的映射是
//! Rust 侧唯一写这些数字的地方。新增 target 模式 / notice 类型 / 卡片类别时，
//! 只需同步这里 + appwindow.slint 的 UiEnums。

use ui_viewmodels::{AssetKind, TargetBarMode, TargetNoticeTone};

/// 目标条模式 → UiEnums.target-mode-*（与 slint 常量严格对应）。
pub fn target_bar_mode(mode: TargetBarMode) -> i32 {
    match mode {
        TargetBarMode::Empty => 0,
        TargetBarMode::Ready => 1,
        TargetBarMode::NeedsConfirmation => 2,
        TargetBarMode::ChooseTarget => 3,
    }
}

/// 提示语气 → UiEnums.notice-tone-*（与 slint 常量严格对应）。
pub fn notice_tone(tone: TargetNoticeTone) -> i32 {
    match tone {
        TargetNoticeTone::Success => 0,
        TargetNoticeTone::Warning => 1,
        TargetNoticeTone::Error => 2,
    }
}

/// 素材类别 → UiEnums.card-kind-*（与 slint 常量严格对应）。
pub fn card_kind(kind: AssetKind) -> i32 {
    match kind {
        AssetKind::Image => 0,
        AssetKind::Video => 1,
        AssetKind::Text => 2,
        AssetKind::Other => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_match_slint_ui_enums_constants() {
        // appwindow.slint 的 UiEnums 常量值；此处锁定，改动需两处同步。
        assert_eq!(target_bar_mode(TargetBarMode::Empty), 0);
        assert_eq!(target_bar_mode(TargetBarMode::Ready), 1);
        assert_eq!(target_bar_mode(TargetBarMode::NeedsConfirmation), 2);
        assert_eq!(target_bar_mode(TargetBarMode::ChooseTarget), 3);
        assert_eq!(notice_tone(TargetNoticeTone::Success), 0);
        assert_eq!(notice_tone(TargetNoticeTone::Warning), 1);
        assert_eq!(notice_tone(TargetNoticeTone::Error), 2);
        assert_eq!(card_kind(AssetKind::Image), 0);
        assert_eq!(card_kind(AssetKind::Video), 1);
        assert_eq!(card_kind(AssetKind::Text), 2);
        assert_eq!(card_kind(AssetKind::Other), 3);
    }
}
