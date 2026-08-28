//! D50 归类弹窗 VM 红灯规格：D50 选项表穷举 + 混选分组 + 记忆决策。
//!
//! 词汇对齐 CONTEXT.md：按包内分类 / 按文件夹名归类 / 统一归入 / 放入待分类。

use std::path::{Path, PathBuf};

use ui_viewmodels::classify::{
    mode_label, plan_groups, EntryKind, GroupKind, GroupMode, ImportEntry,
};
use ui_viewmodels::settings::AppSettings;

fn entry(path: &str, kind: EntryKind, count: Option<usize>) -> ImportEntry {
    ImportEntry {
        path: PathBuf::from(path),
        kind,
        category_count: count,
    }
}

fn touch(path: &Path) {
    std::fs::write(path, b"placeholder").unwrap();
}

// ----- D50 表：三种来源的默认项 + 选项集 -----

#[test]
fn package_group_defaults_to_per_source_with_n_annotation() {
    let groups = plan_groups(&[entry("C:/p.emo", EntryKind::EmoPackage, Some(3))]);
    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(g.kind, GroupKind::Package);
    assert_eq!(g.default_mode, GroupMode::PerSource);
    assert_eq!(
        g.options,
        &[GroupMode::PerSource, GroupMode::Unified, GroupMode::Inbox]
    );
    assert_eq!(g.category_count, Some(3), "「含 N 个分类」标注透传");
    assert_eq!(mode_label(g.kind, GroupMode::PerSource), "按包内分类");
    assert_eq!(mode_label(g.kind, GroupMode::Unified), "统一归入…");
    assert_eq!(mode_label(g.kind, GroupMode::Inbox), "放入待分类");
}

#[test]
fn qianniu_directory_with_probe_count_is_package_kind() {
    // 千牛结构目录：目录 + probe Some(n) → 包组（D50 表第一行同样适用）。
    let groups = plan_groups(&[entry("C:/包", EntryKind::Directory, Some(2))]);
    assert_eq!(groups[0].kind, GroupKind::Package);
    assert_eq!(groups[0].default_mode, GroupMode::PerSource);
    assert_eq!(groups[0].category_count, Some(2));
}

#[test]
fn folder_group_defaults_to_per_folder_without_annotation() {
    let groups = plan_groups(&[entry("C:/相册", EntryKind::Directory, None)]);
    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(g.kind, GroupKind::Folder);
    assert_eq!(g.default_mode, GroupMode::PerSource);
    assert_eq!(
        g.options,
        &[GroupMode::PerSource, GroupMode::Unified, GroupMode::Inbox]
    );
    assert_eq!(g.category_count, None, "普通文件夹不给 N 标注");
    assert_eq!(mode_label(g.kind, GroupMode::PerSource), "按文件夹名归类");
}

#[test]
fn loose_group_defaults_to_unified() {
    let png = std::env::temp_dir().join("classify_loose_a.png");
    touch(&png);
    let groups = plan_groups(&[entry(
        &png.display().to_string(),
        EntryKind::LooseFile,
        None,
    )]);
    let g = &groups[0];
    assert_eq!(g.kind, GroupKind::Loose);
    assert_eq!(g.default_mode, GroupMode::Unified, "散文件默认「统一归入」");
    assert_eq!(
        g.options,
        &[GroupMode::Unified, GroupMode::Inbox],
        "散文件没有按来源规则选项"
    );
    assert_eq!(mode_label(g.kind, GroupMode::Unified), "统一归入…");
    std::fs::remove_file(png).unwrap();
}

// ----- 混选分组：按来源分组各给一行，顺序 = 首现序 -----

#[test]
fn mixed_selection_buckets_by_kind_in_first_seen_order() {
    let png = std::env::temp_dir().join("classify_mixed_a.png");
    touch(&png);
    let entries = vec![
        entry(&png.display().to_string(), EntryKind::LooseFile, None),
        entry("C:/pack.emo", EntryKind::EmoPackage, Some(4)),
        entry("C:/相册", EntryKind::Directory, None),
    ];
    let groups = plan_groups(&entries);
    assert_eq!(groups.len(), 3, "混选 = 三组三行，弹窗仍一次");
    assert_eq!(groups[0].kind, GroupKind::Loose, "首现序：散文件行在最前");
    assert_eq!(groups[1].kind, GroupKind::Package);
    assert_eq!(groups[1].paths, vec![PathBuf::from("C:/pack.emo")]);
    assert_eq!(groups[2].kind, GroupKind::Folder);
    std::fs::remove_file(png).unwrap();
}

#[test]
fn package_group_aggregates_counts_across_entries() {
    let entries = vec![
        entry("C:/a.emo", EntryKind::EmoPackage, Some(2)),
        entry("C:/b.emo", EntryKind::EmoPackage, Some(3)),
    ];
    let groups = plan_groups(&entries);
    assert_eq!(groups.len(), 1, "同组合一行");
    assert_eq!(groups[0].category_count, Some(5), "N = 各包计数之和");
    assert_eq!(groups[0].paths.len(), 2);
}

// ----- R4：拖入含不支持类型 = 该文件跳过 -----

#[test]
fn unsupported_loose_files_are_filtered_out() {
    let txt = std::env::temp_dir().join("classify_unsupported.exe");
    std::fs::write(&txt, b"hello").unwrap();
    let png = std::env::temp_dir().join("classify_supported.png");
    touch(&png);
    let groups = plan_groups(&[
        entry(&txt.display().to_string(), EntryKind::LooseFile, None),
        entry(&png.display().to_string(), EntryKind::LooseFile, None),
    ]);
    assert_eq!(groups.len(), 1, "纯不支持文件不产生分组行");
    assert_eq!(groups[0].paths, vec![png.clone()], "支持的照常进组");
    std::fs::remove_file(txt).unwrap();
    std::fs::remove_file(png).unwrap();
}

#[test]
fn all_entries_filtered_yields_no_groups() {
    let txt = std::env::temp_dir().join("classify_all_unsupported.exe");
    std::fs::write(&txt, b"MZ").unwrap();
    assert!(plan_groups(&[entry(
        &txt.display().to_string(),
        EntryKind::LooseFile,
        None
    )])
    .is_empty());
    std::fs::remove_file(txt).unwrap();
}

// ----- R8：记住我的选择（同来源不再弹窗） -----

fn settings_with_memory(
    package: (&str, &str),
    folder: (&str, &str),
    loose: (&str, &str),
) -> AppSettings {
    AppSettings {
        ask_classify_on_import: false,
        remember_package_mode: package.0.into(),
        remember_package_category: package.1.into(),
        remember_folder_mode: folder.0.into(),
        remember_folder_category: folder.1.into(),
        remember_loose_mode: loose.0.into(),
        remember_loose_category: loose.1.into(),
        ..AppSettings::default()
    }
}

#[test]
fn remembered_modes_skip_the_dialog() {
    let s = settings_with_memory(("per_source", ""), ("unified", "相册"), ("inbox", ""));
    let groups = plan_groups(&[
        entry("C:/p.emo", EntryKind::EmoPackage, Some(1)),
        entry("C:/d", EntryKind::Directory, None),
        entry("C:/f.png", EntryKind::LooseFile, None),
    ]);
    let remembered = ui_viewmodels::classify::memory_defaults(&groups, &s);
    assert!(
        remembered.iter().all(Option::is_some),
        "全部组有记忆 → 不弹窗"
    );
    assert_eq!(remembered[0], Some((GroupMode::PerSource, None)));
    assert_eq!(
        remembered[1],
        Some((GroupMode::Unified, Some("相册".into()))),
        "统一归入的记忆连分类名一并记住"
    );
    assert_eq!(remembered[2], Some((GroupMode::Inbox, None)));
}

#[test]
fn partial_memory_still_asks_with_preselection() {
    let s = settings_with_memory(("per_source", ""), ("", ""), ("", ""));
    let groups = plan_groups(&[
        entry("C:/p.emo", EntryKind::EmoPackage, Some(1)),
        entry("C:/f.png", EntryKind::LooseFile, None),
    ]);
    let remembered = ui_viewmodels::classify::memory_defaults(&groups, &s);
    assert!(remembered[0].is_some());
    assert!(!remembered[1].is_some(), "未记忆的组仍需弹窗确认");
}

#[test]
fn restoring_the_ask_toggle_discards_memory_effect() {
    let s = settings_with_memory(("inbox", ""), ("", ""), ("", ""));
    // 设置面板「导入时询问归类」重新打开（D28 toggle 机制）。
    let mut s = s;
    assert!(s.toggle("ask_classify_on_import"));
    assert!(s.ask_classify_on_import);
    let groups = plan_groups(&[entry("C:/p.emo", EntryKind::EmoPackage, Some(1))]);
    assert!(ui_viewmodels::classify::memory_defaults(&groups, &s)
        .iter()
        .all(Option::is_none));
}

#[test]
fn ask_toggle_is_described_in_settings_panel() {
    // D28 机制：describe 覆盖、toggle 翻转、文案非空。
    let s = AppSettings::default();
    let views = s.describe();
    let view = views
        .iter()
        .find(|v| v.key == "ask_classify_on_import")
        .expect("设置面板应有「导入时询问归类」行");
    assert!(view.checked, "默认询问");
    assert!(!view.detail.is_empty());
}

// ----- 决策 → 清单 mode 字段（与 sample-library --import-paths 协议对齐） -----

#[test]
fn decisions_map_to_import_directives() {
    use ui_viewmodels::classify::decision_to_mode_field;
    assert_eq!(decision_to_mode_field(GroupMode::PerSource, None), "auto");
    assert_eq!(decision_to_mode_field(GroupMode::Inbox, None), "inbox");
    assert_eq!(
        decision_to_mode_field(GroupMode::Unified, Some("相册")),
        "category:相册"
    );
    // 统一归入选中「待分类」= inbox 语义（R6 默认选中项）。
    assert_eq!(decision_to_mode_field(GroupMode::Unified, None), "inbox");
    assert_eq!(
        decision_to_mode_field(GroupMode::Unified, Some("待分类")),
        "inbox"
    );
}
