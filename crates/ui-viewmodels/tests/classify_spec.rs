//! D66 归类弹窗 VM 红灯规格：批次级三操作 + 可解析包静默直通 + 混选
//! 分流 + 批次记忆 + 归入检索 + 只读清单行。
//!
//! 词汇（D66）：归入… / 新建… / 待分类；「按包内分类」只作为静默直通语义
//! 存在，不再是弹窗操作项。批次级 = 一批素材只做一次决策，不逐组问。

use std::path::{Path, PathBuf};

use ui_viewmodels::classify::{
    decision_to_mode_field, filter_category_matches, plan_import, resolve_target, ClassifyTarget,
    EntryKind, GroupKind, GroupMode, ImportEntry,
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

// ----- D66：可解析包静默直通（不进弹窗） -----

#[test]
fn emo_with_resolvable_categories_is_silent_auto() {
    let plan = plan_import(&[entry("C:/p.emo", EntryKind::EmoPackage, Some(3))]);
    assert!(plan.groups.is_empty(), "可解析包不出弹窗行");
    assert_eq!(plan.silent_packages, vec![PathBuf::from("C:/p.emo")]);
}

#[test]
fn qianniu_directory_with_categories_is_silent_too() {
    // 千牛结构目录与 .emo 同一包语义：probe 探得分类 → 静默按包内分类。
    let plan = plan_import(&[entry("C:/包", EntryKind::Directory, Some(2))]);
    assert!(plan.groups.is_empty());
    assert_eq!(plan.silent_packages.len(), 1);
}

#[test]
fn degenerate_packages_still_ask() {
    // probe 失败（None）与探得 0 分类（扁平 zip）= 结构可疑，交用户裁决。
    let corrupt = entry("C:/broken.emo", EntryKind::EmoPackage, None);
    let flat = entry("C:/flat.emo", EntryKind::EmoPackage, Some(0));
    let plan = plan_import(&[corrupt, flat]);
    assert!(plan.silent_packages.is_empty());
    assert_eq!(plan.groups.len(), 1, "同组残包合一行");
    assert_eq!(plan.groups[0].kind, GroupKind::Package);
    assert_eq!(plan.groups[0].paths.len(), 2);
    assert_eq!(
        plan.groups[0].default_mode,
        GroupMode::Inbox,
        "残包默认待分类（读取可能失败，不预设归入）"
    );
    assert_eq!(
        plan.groups[0].suggested_name.as_deref(),
        Some("broken"),
        "预填名取组内首条目的包名 stem"
    );
}

// ----- D66 表：三类来源的默认项 + 三操作选项集 -----

#[test]
fn folder_group_defaults_to_create_with_folder_name() {
    let plan = plan_import(&[entry("C:/相册", EntryKind::Directory, None)]);
    assert_eq!(plan.groups.len(), 1);
    let g = &plan.groups[0];
    assert_eq!(g.kind, GroupKind::Folder);
    assert_eq!(
        g.default_mode,
        GroupMode::Create,
        "文件夹组默认新建（预填目录名）"
    );
    assert_eq!(g.suggested_name.as_deref(), Some("相册"));
}

#[test]
fn loose_group_defaults_to_into() {
    let png = std::env::temp_dir().join("classify_loose_a.png");
    touch(&png);
    let plan = plan_import(&[entry(
        &png.display().to_string(),
        EntryKind::LooseFile,
        None,
    )]);
    let g = &plan.groups[0];
    assert_eq!(g.kind, GroupKind::Loose);
    assert_eq!(g.default_mode, GroupMode::Into, "散文件默认归入现有分类");
    assert_eq!(g.suggested_name, None, "散文件不预填新建名");
    std::fs::remove_file(png).unwrap();
}

// ----- 混选：静默与询问同批分流 -----

#[test]
fn mixed_selection_splits_silent_and_ask_groups() {
    let png = std::env::temp_dir().join("classify_mixed_a.png");
    touch(&png);
    let entries = vec![
        entry(&png.display().to_string(), EntryKind::LooseFile, None),
        entry("C:/pack.emo", EntryKind::EmoPackage, Some(4)),
        entry("C:/相册", EntryKind::Directory, None),
    ];
    let plan = plan_import(&entries);
    assert_eq!(plan.silent_packages, vec![PathBuf::from("C:/pack.emo")]);
    assert_eq!(plan.groups.len(), 2, "弹窗只剩散文件与文件夹两行");
    assert_eq!(
        plan.groups[0].kind,
        GroupKind::Loose,
        "首现序：散文件行在前"
    );
    assert_eq!(plan.groups[1].kind, GroupKind::Folder);
    std::fs::remove_file(png).unwrap();
}

#[test]
fn multiple_silent_packages_keep_own_paths() {
    let plan = plan_import(&[
        entry("C:/a.emo", EntryKind::EmoPackage, Some(2)),
        entry("C:/b.emo", EntryKind::EmoPackage, Some(3)),
    ]);
    assert!(plan.groups.is_empty(), "全静默批次不弹窗");
    assert_eq!(
        plan.silent_packages.len(),
        2,
        "各包独立成行、各自按包内分类"
    );
}

// ----- R4：拖入含不支持类型 = 该文件跳过 -----

#[test]
fn unsupported_loose_files_are_filtered_out() {
    let txt = std::env::temp_dir().join("classify_unsupported.exe");
    std::fs::write(&txt, b"hello").unwrap();
    let png = std::env::temp_dir().join("classify_supported.png");
    touch(&png);
    let plan = plan_import(&[
        entry(&txt.display().to_string(), EntryKind::LooseFile, None),
        entry(&png.display().to_string(), EntryKind::LooseFile, None),
    ]);
    assert_eq!(plan.groups.len(), 1, "纯不支持文件不产生分组行");
    assert_eq!(plan.groups[0].paths, vec![png.clone()], "支持的照常进组");
    std::fs::remove_file(txt).unwrap();
    std::fs::remove_file(png).unwrap();
}

#[test]
fn all_entries_filtered_yields_nothing() {
    let txt = std::env::temp_dir().join("classify_all_unsupported.exe");
    std::fs::write(&txt, b"MZ").unwrap();
    let plan = plan_import(&[entry(
        &txt.display().to_string(),
        EntryKind::LooseFile,
        None,
    )]);
    assert!(plan.groups.is_empty());
    assert!(plan.silent_packages.is_empty());
    std::fs::remove_file(txt).unwrap();
}

// ----- R8 批次记忆（D66）：整批一对（方式/目标）+ 批次默认 -----

fn settings_with_memory(mode: &str, category: &str) -> AppSettings {
    AppSettings {
        ask_classify_on_import: false,
        remember_mode: mode.into(),
        remember_category: category.into(),
        ..AppSettings::default()
    }
}

#[test]
fn remembered_decision_skips_the_dialog() {
    for mode_str in ["category", "into", "create"] {
        // 单输入框时代归入/新建指令同形（category:X），新旧串都认。
        let s = settings_with_memory(mode_str, "相册");
        assert_eq!(
            ui_viewmodels::classify::remembered_decision(&s),
            Some((GroupMode::Into, Some("相册".into()))),
            "记忆成对落盘，批次级直接套用（串 = {mode_str}）"
        );
    }
    let inbox = settings_with_memory("inbox", "");
    assert_eq!(
        ui_viewmodels::classify::remembered_decision(&inbox),
        Some((GroupMode::Inbox, None))
    );
}

#[test]
fn legacy_memory_strings_are_treated_as_unremembered() {
    // D50 时代的 per_source/unified 与空串/乱值 = 没记（弹窗重新问）。
    for legacy in ["per_source", "unified", "nonsense", ""] {
        let s = settings_with_memory(legacy, "相册");
        assert_eq!(ui_viewmodels::classify::remembered_decision(&s), None);
    }
}

#[test]
fn dialog_prefill_follows_memory_then_single_group_name() {
    let folder = plan_import(&[entry("C:/相册", EntryKind::Directory, None)]);
    assert_eq!(
        ui_viewmodels::classify::dialog_prefill(&folder.groups, &AppSettings::default()),
        Some("相册".into()),
        "单文件夹预填目录名"
    );
    let remembered = settings_with_memory("category", "表情");
    assert_eq!(
        ui_viewmodels::classify::dialog_prefill(&folder.groups, &remembered),
        Some("表情".into()),
        "有记忆用记忆分类，盖过目录名建议"
    );
    let remembered_inbox = settings_with_memory("inbox", "");
    assert_eq!(
        ui_viewmodels::classify::dialog_prefill(&folder.groups, &remembered_inbox),
        Some("相册".into()),
        "inbox 记忆不预填，退回组建议名"
    );
    let broken = plan_import(&[entry("C:/broken.emo", EntryKind::EmoPackage, None)]);
    assert_eq!(
        ui_viewmodels::classify::dialog_prefill(&broken.groups, &AppSettings::default()),
        Some("broken".into()),
        "残包预填包名 stem"
    );
}

#[test]
fn mixed_batch_prefills_nothing() {
    let png = std::env::temp_dir().join("classify_batch_mixed.png");
    touch(&png);
    let plan = plan_import(&[
        entry(&png.display().to_string(), EntryKind::LooseFile, None),
        entry("C:/相册", EntryKind::Directory, None),
    ]);
    assert_eq!(
        ui_viewmodels::classify::dialog_prefill(&plan.groups, &AppSettings::default()),
        None,
        "混合组不预填（批次级只做一次决策，输入框留给用户）"
    );
    std::fs::remove_file(png).unwrap();
}

#[test]
fn restoring_the_ask_toggle_discards_memory_effect() {
    let mut s = settings_with_memory("inbox", "");
    // 设置面板「导入时询问归类」重新打开（D28 toggle 机制）。
    assert!(s.toggle("ask_classify_on_import"));
    assert!(s.ask_classify_on_import);
    assert_eq!(
        ui_viewmodels::classify::remembered_decision(&s),
        None,
        "开着询问 = 记忆一律不生效"
    );
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

// ----- D66：批次清单行（弹窗只读首行，替代逐组重复卡片） -----

#[test]
fn manifest_summary_lists_groups_in_first_seen_order() {
    let png = std::env::temp_dir().join("classify_manifest.png");
    touch(&png);
    let plan = plan_import(&[
        entry(&png.display().to_string(), EntryKind::LooseFile, None),
        entry("C:/相册", EntryKind::Directory, None),
        entry("C:/broken.emo", EntryKind::EmoPackage, None),
    ]);
    assert_eq!(
        ui_viewmodels::classify::manifest_summary(&plan.groups),
        "散文件 1 个 · 文件夹「相册」 · 素材包「broken」（结构未识别）",
        "单项组带名，残包组标注结构未识别"
    );
    let merged = plan_import(&[
        entry("C:/broken.emo", EntryKind::EmoPackage, None),
        entry("C:/flat.emo", EntryKind::EmoPackage, Some(0)),
    ]);
    assert_eq!(
        ui_viewmodels::classify::manifest_summary(&merged.groups),
        "素材包 2 个（结构未识别）",
        "同组多包退回计数形式"
    );
    std::fs::remove_file(png).unwrap();
}

// ----- 决策 → 清单 mode 字段（与 sample-library --import-paths 协议对齐） -----

#[test]
fn decisions_map_to_import_directives() {
    use ui_viewmodels::classify::decision_to_mode_field as d;
    assert_eq!(d(GroupMode::Inbox, None), "inbox");
    assert_eq!(d(GroupMode::Into, Some("相册")), "category:相册");
    assert_eq!(d(GroupMode::Create, Some("新分类")), "category:新分类");
    // 归入未选 / 新建留空 = inbox 语义；「待分类」名同义。
    assert_eq!(d(GroupMode::Into, None), "inbox");
    assert_eq!(d(GroupMode::Create, Some("  ")), "inbox");
    assert_eq!(d(GroupMode::Create, Some("待分类")), "inbox");
}

// ----- D66.1：输入框 → 目标解析（归入已有 / 新建 / 待分类） -----

#[test]
fn resolve_target_matches_existing_case_insensitively() {
    let categories: Vec<String> = vec!["表情包".into(), "Screenshots".into()];
    assert_eq!(
        resolve_target(&categories, "screenshots"),
        ClassifyTarget::Existing("Screenshots".into()),
        "命中取列表规范名，防大小写重复项"
    );
    assert_eq!(
        resolve_target(&categories, " 表情包 "),
        ClassifyTarget::Existing("表情包".into()),
        "首尾空白不参与判定"
    );
    assert_eq!(
        resolve_target(&categories, "风景"),
        ClassifyTarget::Fresh("风景".into()),
        "无同名 = 导入时新建（toast 点名）"
    );
    assert_eq!(resolve_target(&categories, ""), ClassifyTarget::Inbox);
    assert_eq!(resolve_target(&categories, "  "), ClassifyTarget::Inbox);
    // 「待分类」不作命中目标，落 Fresh 后由 mode 字段归 inbox 语义。
    assert_eq!(
        resolve_target(&categories, "待分类"),
        ClassifyTarget::Fresh("待分类".into())
    );
    assert_eq!(
        decision_to_mode_field(GroupMode::Create, Some("待分类")),
        "inbox",
        "输入待分类 = inbox 指令，不会真的建重名分类"
    );
}

#[test]
fn target_hint_states_the_outcome_before_confirm() {
    assert!(ClassifyTarget::Inbox.hint().contains("待分类"));
    assert_eq!(
        ClassifyTarget::Existing("风景".into()).hint(),
        "将导入到已有分类「风景」。"
    );
    assert_eq!(
        ClassifyTarget::Fresh("风景".into()).hint(),
        "没有同名分类，点「导入」将新建「风景」。"
    );
    // 形态码：0 待分类 / 1 已有 / 2 新建（slint 侧 UiEnums.classify-hint-*）。
    assert_eq!(ClassifyTarget::Inbox.hint_kind(), 0);
    assert_eq!(ClassifyTarget::Existing("x".into()).hint_kind(), 1);
    assert_eq!(ClassifyTarget::Fresh("x".into()).hint_kind(), 2);
}

// ----- D66：归入检索 -----

#[test]
fn category_matches_filter_case_insensitive_and_excludes_inbox() {
    // 「全部」表头由调用方（壳层 skip(1)）先滤掉，过滤器只管排除待分类。
    let categories: Vec<String> = vec![
        "待分类".into(),
        "表情包".into(),
        "Screenshots".into(),
        "壁纸".into(),
    ];
    let hits = filter_category_matches(&categories, "表情", 8);
    assert_eq!(hits, vec!["表情包".to_string()]);
    let hits = filter_category_matches(&categories, "SCREEN", 8);
    assert_eq!(hits, vec!["Screenshots".to_string()], "大小写不敏感");
    let all = filter_category_matches(&categories, "", 8);
    assert!(
        !all.contains(&"待分类".to_string()),
        "候选不含待分类（第三操作兜底）"
    );
    assert_eq!(all.len(), 3);
    let capped = filter_category_matches(&categories, "", 2);
    assert_eq!(capped.len(), 2, "封顶生效");
}
