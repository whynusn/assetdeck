//! 导入归类弹窗的纯函数模型（D49/D50/D66）。
//!
//! 壳层把对话框/拖放拿到的路径 + probe 计数归纳成 [`ImportEntry`] 列表，
//! [`plan_import`] 把「probe 确认包内分类可解析」的包（.emo / 千牛结构目录
//! 且探得 ≥1 个分类）分流为**静默按包内分类直通**（D66：不弹窗），余下的
//! 产出弹窗分组行（D66 三操作：归入 / 新建 / 待分类）；记忆决策（R8）由
//! [`memory_defaults`] 依据 [`AppSettings`] 给出；确认后
//! [`decision_to_mode_field`] 把每行决策翻译成 `sample-library
//! --import-paths` 清单行的 mode 字段。
//!
//! 本模块零 IO（除可导入性判定读扩展名），全部可穷举测试。

use std::path::{Path, PathBuf};

use crate::settings::AppSettings;
use media::is_importable;
// 壳层（app-ui）不直接依赖 store：待分类名经此再导出，内部引用同用此名。
pub use store::INBOX_CATEGORY;

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
    /// None = 无标注（普通目录/散文件/探测失败）。
    pub category_count: Option<usize>,
}

/// 弹窗一行的归类方式（D66 三操作，所有来源同一组）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMode {
    /// 归入现有分类（检索后点选）。
    Into,
    /// 新建分类（自由命名；留空确认 = 待分类）。
    Create,
    /// 放入待分类。
    Inbox,
}

/// 来源组别（决定摘要文案、新建预填名与记忆槽位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// 结构异常的 .emo / 千牛目录（probe 没探得分类，无法静默直通）。
    Package,
    /// 普通文件夹。
    Folder,
    /// 散文件。
    Loose,
}

/// 弹窗一行 = 一组同源条目 + 一个归类决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGroup {
    pub kind: GroupKind,
    pub paths: Vec<PathBuf>,
    pub default_mode: GroupMode,
    /// 「新建」预填名：文件夹组 = 目录名、残包组 = 包名去扩展。None = 不预填。
    pub suggested_name: Option<String>,
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

/// probe 确认「包内分类可解析」：.emo / 千牛结构目录且探得 ≥1 个分类
/// （D66 静默直通门槛；探得 0 个或 probe 失败 = 结构可疑，仍交用户裁决）。
fn is_resolvable_package(entry: &ImportEntry) -> bool {
    let structured = matches!(entry.kind, EntryKind::EmoPackage | EntryKind::Directory)
        && entry.category_count.is_some();
    structured && entry.category_count >= Some(1)
}

/// 新建预填名：零 IO——目录名/包名 stem 直接取自路径，散文件不预填。
fn suggested_name_of(kind: GroupKind, path: &Path) -> Option<String> {
    match kind {
        GroupKind::Folder => path.file_name().map(|n| n.to_string_lossy().into_owned()),
        GroupKind::Package => path.file_stem().map(|n| n.to_string_lossy().into_owned()),
        GroupKind::Loose => None,
    }
}

/// D66 分流结果：静默直通的包路径（壳层发 `p\tauto\t` 行，不弹窗）+
/// 需要询问的分组行。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportPlan {
    pub silent_packages: Vec<PathBuf>,
    pub groups: Vec<SourceGroup>,
}

/// 归纳分流：可解析包静默直通；余下散文件按扩展名过滤（R4 不支持 = 静默
/// 跳过），按组别首现序分桶，各桶合并 paths（计数求和）。空输入/全被过滤
/// → 两组皆空。
pub fn plan_import(entries: &[ImportEntry]) -> ImportPlan {
    let mut plan = ImportPlan::default();
    for entry in entries {
        if is_resolvable_package(entry) {
            plan.silent_packages.push(entry.path.clone());
            continue;
        }
        let kind = group_kind_of(entry);
        if kind == GroupKind::Loose && !is_importable(&entry.path) {
            continue;
        }
        if let Some(group) = plan.groups.iter_mut().find(|g| g.kind == kind) {
            group.paths.push(entry.path.clone());
            continue;
        }
        let default_mode = match kind {
            GroupKind::Package => GroupMode::Inbox,
            GroupKind::Folder => GroupMode::Create,
            GroupKind::Loose => GroupMode::Into,
        };
        plan.groups.push(SourceGroup {
            kind,
            paths: vec![entry.path.clone()],
            default_mode,
            suggested_name: suggested_name_of(kind, &entry.path),
        });
    }
    plan
}

// ---------------------------------------------------------------------------
// R8 批次记忆（D66）：整批一个决策；「记住我的选择」只落一对（方式/目标）
// ---------------------------------------------------------------------------

/// 已记住的批次决策（None = 照常弹窗）。`ask_classify_on_import` 开着（默认）
/// = None；方式串未知（手改 TOML / D50 时代 per_source|unified 残留）= 没记。
/// 单输入框时代归入/新建在指令层同形（category:X），新串 "category" 与旧串
/// "into"/"create" 一并认作分类语义。
pub fn remembered_decision(settings: &AppSettings) -> Option<(GroupMode, Option<String>)> {
    if settings.ask_classify_on_import {
        return None;
    }
    let mode = match settings.remember_mode.as_str() {
        "inbox" => GroupMode::Inbox,
        "category" | "into" | "create" => GroupMode::Into,
        _ => return None,
    };
    let category = if settings.remember_category.is_empty() {
        None
    } else {
        Some(settings.remember_category.clone())
    };
    Some((mode, category))
}

/// 弹窗打开时输入框的预填：有记忆用记忆分类（inbox 记忆 = 不预填）；
/// 否则单一来源组用其建议名（文件夹名/包 stem）；混合组留空。
pub fn dialog_prefill(groups: &[SourceGroup], settings: &AppSettings) -> Option<String> {
    match remembered_decision(settings) {
        Some((GroupMode::Inbox, _)) | None => {}
        Some((_, Some(category))) => return Some(category),
        Some((_, None)) => {}
    }
    if let [group] = groups {
        return group.suggested_name.clone();
    }
    None
}

/// 输入框内容解析出的导入目标（confirm 与实时预告行共用一个真源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifyTarget {
    /// 空输入 = 待分类。
    Inbox,
    /// 命中已有分类（大小写不敏感，取列表规范名——防大小写重复项）。
    Existing(String),
    /// 无同名 → 导入时新建（toast 点名「已自动创建」，不留无感知）。
    Fresh(String),
}

/// 解析输入 → 目标。`categories` 由调用方先 skip(1) 滤掉「全部」表头
/// （与 `filter_category_matches` 同契约）；「待分类」名不参与命中，
/// 落到 Fresh 后由 `decision_to_mode_field` 归 inbox 语义。
pub fn resolve_target(categories: &[String], typed: &str) -> ClassifyTarget {
    let name = typed.trim();
    if name.is_empty() {
        return ClassifyTarget::Inbox;
    }
    let needle = name.to_lowercase();
    categories
        .iter()
        .filter(|c| c.as_str() != INBOX_CATEGORY)
        .find(|c| c.to_lowercase() == needle)
        .map(|c| ClassifyTarget::Existing(c.clone()))
        .unwrap_or_else(|| ClassifyTarget::Fresh(name.to_string()))
}

impl ClassifyTarget {
    /// 实时预告行文案：导入前就把结果说清楚。
    pub fn hint(&self) -> String {
        match self {
            ClassifyTarget::Inbox => "留空 = 放入待分类，之后可随时移动。".to_string(),
            ClassifyTarget::Existing(name) => format!("将导入到已有分类「{name}」。"),
            ClassifyTarget::Fresh(name) => format!("没有同名分类，点「导入」将新建「{name}」。"),
        }
    }

    /// 预告行着色形态：0 待分类（弱化）/ 1 已有（accent）/ 2 新建（醒目）。
    pub fn hint_kind(&self) -> i32 {
        match self {
            ClassifyTarget::Inbox => 0,
            ClassifyTarget::Existing(_) => 1,
            ClassifyTarget::Fresh(_) => 2,
        }
    }
}

/// 弹窗只读清单行：本次导入的来源构成（按首现序）。单项组带名（文件夹原名/
/// 包名 stem），残包组附「结构未识别」——probe 探得 ≥1 分类的不进弹窗。
pub fn manifest_summary(groups: &[SourceGroup]) -> String {
    let count_of = |kind: &str, group: &SourceGroup| format!("{kind} {} 个", group.paths.len());
    groups
        .iter()
        .map(|group| {
            let kind_label = match group.kind {
                GroupKind::Package => "素材包",
                GroupKind::Folder => "文件夹",
                GroupKind::Loose => "散文件",
            };
            let mut label = match (group.kind, group.paths.as_slice()) {
                (GroupKind::Folder, [only]) => match only.file_name() {
                    Some(name) => format!("文件夹「{}」", name.to_string_lossy()),
                    None => count_of(kind_label, group),
                },
                (GroupKind::Package, [only]) => match only.file_stem() {
                    Some(stem) => format!("素材包「{}」", stem.to_string_lossy()),
                    None => count_of(kind_label, group),
                },
                _ => count_of(kind_label, group),
            };
            if group.kind == GroupKind::Package {
                label.push_str("（结构未识别）");
            }
            label
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 一行决策 → `--import-paths` 清单行的 mode 字段。
/// 归入未选 / 新建留空（或名即「待分类」）= inbox 语义，不产 `category:` 指令。
pub fn decision_to_mode_field(mode: GroupMode, category: Option<&str>) -> String {
    match mode {
        GroupMode::Inbox => "inbox".to_string(),
        GroupMode::Into | GroupMode::Create => match category.map(str::trim) {
            Some(name) if !name.is_empty() && name != INBOX_CATEGORY => {
                format!("category:{name}")
            }
            _ => "inbox".to_string(),
        },
    }
}

/// 归入检索（D66）：大小写不敏感子串匹配；排除「待分类」（有专属第三
/// 操作，不该出现在候选里）；封顶 `cap` 条（弹窗列表限高）。
pub fn filter_category_matches(categories: &[String], query: &str, cap: usize) -> Vec<String> {
    let needle = query.trim().to_lowercase();
    categories
        .iter()
        .filter(|c| c.as_str() != INBOX_CATEGORY)
        .filter(|c| needle.is_empty() || c.to_lowercase().contains(&needle))
        .take(cap)
        .cloned()
        .collect()
}
