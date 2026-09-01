//! UI 枚举 → 整数的**单一映射层**（综合分析报告「三.9」）。
//!
//! .slint 侧所有 0/1/2/3 魔法数字已收口到 UiEnums 全局常量；本文件的映射是
//! Rust 侧唯一写这些数字的地方。新增 target 模式 / notice 类型 / 卡片类别时，
//! 只需同步这里 + appwindow.slint 的 UiEnums。

use ui_viewmodels::selection::MenuAction;
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

/// 右键菜单动作 id → [`MenuAction`]（与 appwindow.slint menu-action 的整数编码严格对应）。
pub fn menu_action(id: i32) -> Option<MenuAction> {
    match id {
        0 => Some(MenuAction::Copy),
        1 => Some(MenuAction::MoveToCategory),
        2 => Some(MenuAction::Rename),
        3 => Some(MenuAction::Properties),
        4 => Some(MenuAction::Delete),
        _ => None,
    }
}

/// D51 搜索范围档位 → UiEnums.scope-*（全部/文件名/分类/标签）。
pub const SCOPE_ALL: i32 = 0;
pub const SCOPE_FILE_NAME: i32 = 1;
pub const SCOPE_CATEGORY: i32 = 2;
pub const SCOPE_TAG: i32 = 3;

/// 档位码 → 混合路由枚举（未知码回落 All，UiEnums 收口纪律）。
pub fn search_scope(code: i32) -> ui_viewmodels::SearchScope {
    match code {
        SCOPE_FILE_NAME => ui_viewmodels::SearchScope::FileName,
        SCOPE_CATEGORY => ui_viewmodels::SearchScope::Category,
        SCOPE_TAG => ui_viewmodels::SearchScope::Tag,
        _ => ui_viewmodels::SearchScope::All,
    }
}

/// 底部操作条形态 → UiEnums.bar-*（隐藏 / 多选 / 回收站）。
pub const BAR_HIDDEN: i32 = 0;
pub const BAR_MULTI: i32 = 1;
pub const BAR_TRASH: i32 = 2;

/// D65 导入结果行形态 → UiEnums.result-row-*（完全相同 / 相似 / 失败）。
pub const IMPORT_RESULT_EXACT: i32 = 0;
pub const IMPORT_RESULT_SIMILAR: i32 = 1;
pub const IMPORT_RESULT_FAILED: i32 = 2;

/// D66 归类弹窗预告行形态 → UiEnums.classify-hint-*（待分类 / 已有 / 新建）。
/// 数值与 ui_viewmodels::classify::ClassifyTarget::hint_kind 的产出约定一致。
pub const CLASSIFY_HINT_INBOX: i32 = 0;
pub const CLASSIFY_HINT_EXISTING: i32 = 1;
pub const CLASSIFY_HINT_NEW: i32 = 2;

/// D66「导入」按钮语义 → UiEnums.classify-confirm-*（按输入解析 / 直入待分类）。
pub const CLASSIFY_CONFIRM_RESOLVE: i32 = 0;
pub const CLASSIFY_CONFIRM_INBOX: i32 = 1;

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
        assert_eq!(IMPORT_RESULT_EXACT, 0);
        assert_eq!(IMPORT_RESULT_SIMILAR, 1);
        assert_eq!(IMPORT_RESULT_FAILED, 2);
        assert_eq!(CLASSIFY_HINT_INBOX, 0);
        assert_eq!(CLASSIFY_HINT_EXISTING, 1);
        assert_eq!(CLASSIFY_HINT_NEW, 2);
        assert_eq!(CLASSIFY_CONFIRM_RESOLVE, 0);
        assert_eq!(CLASSIFY_CONFIRM_INBOX, 1);
    }
}
