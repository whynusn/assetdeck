//! 导入归类弹窗的纯函数模型（D49/D50）。
//!
//! 壳层把对话框/拖放拿到的路径 + probe 计数归纳成 [`ImportEntry`] 列表，
//! [`plan_groups`] 产出弹窗的分组行（D50 选项表）；记忆决策（R8）由
//! [`memory_defaults`] 依据 [`AppSettings`] 给出；确认后
//! [`decision_to_mode_field`] 把每行决策翻译成 `sample-library
//! --import-paths` 清单行的 mode 字段。
//!
//! 本模块零 IO（除可导入性判定读扩展名），全部可穷举测试。

use std::path::PathBuf;

use crate::settings::AppSettings;
use media::is_importable;
use store::INBOX_CATEGORY;

/// 待导入条目的种类（壳层归纳：扩展名 + 是否目录 + probe 结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// 散素材文件（.emo 之外的文件）。
    LooseFile,
    /// .emo 归档。
    EmoPackage,
    /// 目录。
    Directory,
}

/// 一个待导入条目的弹窗前最小元数据（C2：只读结构，零解码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
    /// probe 结果（`--probe-categories`）：Some(n) = 千牛包/.emo 可归类数；
    /// None = 无标注（普通目录/散文件）。
    pub category_count: Option<usize>,
}

/// 弹窗一行的归类方式（D50 三方式；[`GroupMode::PerSource`] 的文案随组别变）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMode {
    /// 按包内分类（包组）/ 按文件夹名归类（文件夹组）。
    PerSource,
    /// 统一归入（分类名是弹窗行状态，None = 待分类）。
    Unified,
    /// 放入待分类。
    Inbox,
}

/// 来源组别（决定 PerSource 的文案与选项集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// .emo 包 / 千牛结构目录。
    Package,
    /// 普通文件夹。
    Folder,
    /// 散文件。
    Loose,
}

/// D50 选项表（选项顺序即 UI 显示顺序，默认项打头）。
const PACKAGE_FOLDER_OPTIONS: &[GroupMode] =
    &[GroupMode::PerSource, GroupMode::Unified, GroupMode::Inbox];
const LOOSE_OPTIONS: &[GroupMode] = &[GroupMode::Unified, GroupMode::Inbox];

/// 文案（CONTEXT.md 词汇锁定；穷举测试钉死）。
pub fn mode_label(kind: GroupKind, mode: GroupMode) -> &'static str {
    match (kind, mode) {
        (GroupKind::Package, GroupMode::PerSource) => "按包内分类",
        (GroupKind::Folder, GroupMode::PerSource) => "按文件夹名归类",
        (_, GroupMode::Unified) => "统一归入…",
        (_, GroupMode::Inbox) => "放入待分类",
        // 包组不存在「按文件夹名」文案分支；Folder 的 PerSource 已单列。
        (GroupKind::Loose, GroupMode::PerSource) => "按来源规则",
    }
}

/// 弹窗一行 = 一组同源条目 + 一个归类决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGroup {
    pub kind: GroupKind,
    pub paths: Vec<PathBuf>,
    /// 「含 N 个分类」标注（包组专属；多包 = 各包计数之和）。
    pub category_count: Option<usize>,
    pub default_mode: GroupMode,
    pub options: &'static [GroupMode],
}

/// 条目 → 组别：.emo 与「目录 + probe 有计数」都是包（千牛结构目录）。
fn group_kind_of(entry: &ImportEntry) -> GroupKind {
    match entry.kind {
        EntryKind::EmoPackage => GroupKind::Package,
        EntryKind::Directory if entry.category_count.is_some() => GroupKind::Package,
        EntryKind::Directory => GroupKind::Folder,
        EntryKind::LooseFile => GroupKind::Loose,
    }
}

/// 归纳分组行：散文件按扩展名过滤（R4 不支持 = 静默跳过），余下按组别
/// 首现序分桶，各桶合并 paths（计数求和）。空输入/全被过滤 → 空表。
pub fn plan_groups(entries: &[ImportEntry]) -> Vec<SourceGroup> {
    let mut groups: Vec<SourceGroup> = Vec::new();
    for entry in entries {
        let kind = group_kind_of(entry);
        if kind == GroupKind::Loose && !is_importable(&entry.path) {
            continue;
        }
        if let Some(group) = groups.iter_mut().find(|g| g.kind == kind) {
            group.paths.push(entry.path.clone());
            if let Some(n) = entry.category_count {
                group.category_count = Some(group.category_count.unwrap_or(0) + n);
            }
            continue;
        }
        let (default_mode, options, count) = match kind {
            GroupKind::Package => (
                GroupMode::PerSource,
                PACKAGE_FOLDER_OPTIONS,
                entry.category_count,
            ),
            GroupKind::Folder => (GroupMode::PerSource, PACKAGE_FOLDER_OPTIONS, None),
            GroupKind::Loose => (GroupMode::Unified, LOOSE_OPTIONS, None),
        };
        groups.push(SourceGroup {
            kind,
            paths: vec![entry.path.clone()],
            category_count: count,
            default_mode,
            options,
        });
    }
    groups
}

// ---------------------------------------------------------------------------
// R8 记忆：同来源类型不再弹窗直接套用；设置面板恢复询问
// ---------------------------------------------------------------------------

/// 每组的记忆默认值（None = 该组没有可用记忆，弹窗时用组默认项）。
/// `ask_classify_on_import` 开着（默认）= 一律 None（照常询问）。
pub fn memory_defaults(
    groups: &[SourceGroup],
    settings: &AppSettings,
) -> Vec<Option<(GroupMode, Option<String>)>> {
    groups
        .iter()
        .map(|group| {
            if settings.ask_classify_on_import {
                return None;
            }
            let (mode, category) = match group.kind {
                GroupKind::Package => (
                    settings.remember_package_mode.as_str(),
                    settings.remember_package_category.as_str(),
                ),
                GroupKind::Folder => (
                    settings.remember_folder_mode.as_str(),
                    settings.remember_folder_category.as_str(),
                ),
                GroupKind::Loose => (
                    settings.remember_loose_mode.as_str(),
                    settings.remember_loose_category.as_str(),
                ),
            };
            let mode = match mode {
                "per_source" => GroupMode::PerSource,
                "unified" => GroupMode::Unified,
                "inbox" => GroupMode::Inbox,
                _ => return None, // 未知串（手改 TOML）= 当作没记
            };
            let category = if category.is_empty() {
                None
            } else {
                Some(category.to_string())
            };
            Some((mode, category))
        })
        .collect()
}

/// 一行决策 → `--import-paths` 清单行的 mode 字段。
/// 统一归入选「待分类」（或空名）= inbox 语义，不产 `category:` 指令。
pub fn decision_to_mode_field(mode: GroupMode, unified_category: Option<&str>) -> String {
    match mode {
        GroupMode::PerSource => "auto".to_string(),
        GroupMode::Inbox => "inbox".to_string(),
        GroupMode::Unified => match unified_category.map(str::trim) {
            Some(name) if !name.is_empty() && name != INBOX_CATEGORY => {
                format!("category:{name}")
            }
            _ => "inbox".to_string(),
        },
    }
}
