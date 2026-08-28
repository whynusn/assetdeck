use std::collections::{HashMap, HashSet};

use platform::{FocusAnchor, FocusPlan, FocusStep};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::TargetId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardFormat {
    Png,
    Files,
    Dib,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatKind {
    Image,
    Video,
    Text,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FormatPolicy {
    pub image: Vec<ClipboardFormat>,
    pub video: Vec<ClipboardFormat>,
    pub text: Vec<ClipboardFormat>,
    pub other: Vec<ClipboardFormat>,
}

impl Default for FormatPolicy {
    fn default() -> Self {
        Self {
            image: vec![ClipboardFormat::Png, ClipboardFormat::Files],
            video: vec![ClipboardFormat::Files],
            text: vec![ClipboardFormat::Text],
            other: Vec::new(),
        }
    }
}

impl FormatPolicy {
    pub fn for_kind(&self, kind: FormatKind) -> &[ClipboardFormat] {
        match kind {
            FormatKind::Image => &self.image,
            FormatKind::Video => &self.video,
            FormatKind::Text => &self.text,
            FormatKind::Other => &self.other,
        }
    }
}

/// 按素材类别声明的格式集合（各行缺省为空，与 [`FormatPolicy`] 的非空缺省不同）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KindFormats {
    pub image: Vec<ClipboardFormat>,
    pub video: Vec<ClipboardFormat>,
    pub text: Vec<ClipboardFormat>,
    pub other: Vec<ClipboardFormat>,
}

impl KindFormats {
    pub fn for_kind(&self, kind: FormatKind) -> &[ClipboardFormat] {
        match kind {
            FormatKind::Image => &self.image,
            FormatKind::Video => &self.video,
            FormatKind::Text => &self.text,
            FormatKind::Other => &self.other,
        }
    }
}

/// 「粘贴即发送」的声明形态。
///
/// 为什么不是单纯的格式集合：2026-08-25 受控实验证明千牛接待中心对 `CF_HDROP`
/// 的语义**按素材类别分叉**——视频粘进去立刻作为消息发出，图片却停在输入框并
/// 渲染真缩略图（取证 `Default_Project_probe/q3-our-hdrop-image.png`）。把即发
/// 事实建模成纯格式维度会把图片一起误判为即发，白白让图片走高成本的 CF_PNG。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SendPolicy {
    /// 数组写法：该格式对所有素材类别都即发。保留它是为了让既有用户画像
    /// （`paste_sends = ["files"]`）继续可读，而不是静默解析失败。
    AllKinds(Vec<ClipboardFormat>),
    /// 表写法：逐类别声明，例如千牛只在 `video` 行放 `files`。
    PerKind(KindFormats),
}

impl Default for SendPolicy {
    fn default() -> Self {
        SendPolicy::PerKind(KindFormats::default())
    }
}

impl SendPolicy {
    /// 该 (类别, 格式) 组合粘进目标是否等于直接发送。
    pub fn sends(&self, kind: FormatKind, format: ClipboardFormat) -> bool {
        match self {
            SendPolicy::AllKinds(formats) => formats.contains(&format),
            SendPolicy::PerKind(per_kind) => per_kind.for_kind(kind).contains(&format),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessMode {
    P0Only,
    #[default]
    UiaShallow,
    UiaStrict,
}

/// 聊天输入框在窗口客户区中的比例位置，画像级声明。
///
/// 为什么建模在画像里：每个 IM 的版式不同（微信输入框偏右下、千牛在中栏下部），
/// 但同一个 IM 内相对位置随窗口缩放保持稳定。比例而非绝对像素，才能跟随用户拖拽。
/// 本类型是 `platform::FocusAnchor` 的可反序列化镜像——`platform` trait 层零依赖，
/// 不能引入 serde，因此配置形态留在 `targets`。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InputAnchor {
    pub x_ratio: f32,
    pub y_ratio: f32,
}

impl From<InputAnchor> for FocusAnchor {
    fn from(value: InputAnchor) -> Self {
        FocusAnchor {
            x_ratio: value.x_ratio,
            y_ratio: value.y_ratio,
        }
    }
}

/// 聚焦级别的可反序列化镜像（`platform::FocusStep` 在 trait 层零依赖，不能引入 serde）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusStrategyStep {
    /// 已在可写控件上则直接采用。
    Already,
    /// UIA 子树搜索 + `SetFocus`。
    Uia,
    /// 画像锚点单次左键单击。
    Anchor,
}

impl From<FocusStrategyStep> for FocusStep {
    fn from(value: FocusStrategyStep) -> Self {
        match value {
            FocusStrategyStep::Already => FocusStep::AlreadyEditable,
            FocusStrategyStep::Uia => FocusStep::UiaSetFocus,
            FocusStrategyStep::Anchor => FocusStep::AnchorClick,
        }
    }
}

/// 未声明 `focus_strategy` 时的缺省顺序：与历史三级降级完全一致，
/// 因此新增字段对既有画像零行为变化。
fn default_focus_strategy() -> Vec<FocusStrategyStep> {
    vec![
        FocusStrategyStep::Already,
        FocusStrategyStep::Uia,
        FocusStrategyStep::Anchor,
    ]
}

// Eq 被 input_anchor 的 f32 排除：比例是连续量，等价比较用 PartialEq 即可。
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub id: TargetId,
    pub label: String,
    pub exe_names: Vec<String>,
    pub class_names: Vec<String>,
    pub title_regexes: Vec<String>,
    pub not_ready_title_regexes: Vec<String>,
    pub formats: FormatPolicy,
    /// 该目标上「粘贴即发送」的 (类别 × 格式) 组合：粘进去不会停在输入框，
    /// 而是直接发出消息。实测千牛接待中心对**视频**的 `CF_HDROP` 就是这个行为，
    /// 对图片的 `CF_HDROP` 则停在输入框显示缩略图；微信两者都停在输入框。
    /// 「只上框不发送」是红线，因此协商层必须跳过落在此集合里的组合。
    pub paste_sends: SendPolicy,
    pub readiness: ReadinessMode,
    pub settle_ms: u64,
    /// 聚焦级别顺序。声明它的意义是**跳过在该目标上注定失败的级别**——
    /// 微信/千牛的 UIA 可写候选实测为 0，把 `uia` 去掉即省下 22~83ms 纯损耗。
    /// 未声明时取三级缺省顺序；空数组是配置错误（见 `ProfileError::EmptyFocusStrategy`）。
    pub focus_strategy: Vec<FocusStrategyStep>,
    /// 输入框锚点：激活窗口后若拿不到键盘焦点，平台层允许在此处做一次左键单击。
    /// 未声明时不做任何点击（宁可不落框，也不点未声明的位置）。
    pub input_anchor: Option<InputAnchor>,
}

impl Profile {
    pub fn generic() -> Self {
        Self {
            id: TargetId::new("generic_im"),
            label: "通用 IM".to_string(),
            exe_names: Vec::new(),
            class_names: Vec::new(),
            title_regexes: Vec::new(),
            not_ready_title_regexes: Vec::new(),
            formats: FormatPolicy::default(),
            paste_sends: SendPolicy::default(),
            readiness: ReadinessMode::P0Only,
            settle_ms: 80,
            focus_strategy: default_focus_strategy(),
            input_anchor: None,
        }
    }

    /// 该 (类别, 格式) 组合粘进目标是否等于直接发送。
    pub fn paste_sends_format(&self, kind: FormatKind, format: ClipboardFormat) -> bool {
        self.paste_sends.sends(kind, format)
    }

    /// 传给平台聚焦端的锚点。未声明锚点时返回 None，平台层据此跳过点击。
    pub fn focus_anchor(&self) -> Option<FocusAnchor> {
        self.input_anchor.map(FocusAnchor::from)
    }

    /// 传给平台聚焦端的完整计划：级别顺序 + 锚点。
    pub fn focus_plan(&self) -> FocusPlan {
        FocusPlan {
            steps: self
                .focus_strategy
                .iter()
                .copied()
                .map(FocusStep::from)
                .collect(),
            anchor: self.focus_anchor(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProfileSet {
    profiles: Vec<Profile>,
    generic: Profile,
}

impl ProfileSet {
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn get(&self, id: &TargetId) -> Option<&Profile> {
        self.profiles.iter().find(|profile| &profile.id == id)
    }

    pub fn generic(&self) -> &Profile {
        &self.generic
    }
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("目标画像 TOML 无法解析: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("目标画像缺少 id")]
    MissingId,
    #[error("内置目标画像 {0} 缺少 label")]
    MissingLabel(String),
    #[error("同一目标画像文档内重复定义 id: {0}")]
    DuplicateId(String),
    #[error("目标画像 {profile} 的正则无效: {pattern}: {source}")]
    InvalidRegex {
        profile: String,
        pattern: String,
        source: regex::Error,
    },
    #[error("目标画像 {profile} 的输入框锚点比例越界: x={x} y={y}(须在 0.0..=1.0)")]
    InvalidAnchor { profile: String, x: f32, y: f32 },
    /// 空数组不能静默退化成「不聚焦」：那样 Ctrl+V 会落空且不留任何线索。
    #[error("目标画像 {0} 的 focus_strategy 为空数组(至少声明一个聚焦级别，或整体省略以取缺省)")]
    EmptyFocusStrategy(String),
}

#[derive(Debug, Default, Deserialize)]
struct ProfileDocument {
    #[serde(default)]
    profiles: Vec<ProfilePatch>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ProfilePatch {
    id: Option<String>,
    label: Option<String>,
    exe_names: Option<Vec<String>>,
    class_names: Option<Vec<String>>,
    title_regexes: Option<Vec<String>>,
    not_ready_title_regexes: Option<Vec<String>>,
    formats: Option<FormatPolicy>,
    paste_sends: Option<SendPolicy>,
    readiness: Option<ReadinessMode>,
    settle_ms: Option<u64>,
    focus_strategy: Option<Vec<FocusStrategyStep>>,
    input_anchor: Option<InputAnchor>,
}

/// 合并随版本发布的内置画像与用户画像。用户画像按 id 做字段级覆盖。
///
/// 此函数只接受字符串，文件选择、读取和原子保存由装配层负责。
pub fn load_profiles(builtin: &str, user: Option<&str>) -> Result<ProfileSet, ProfileError> {
    let builtin_doc: ProfileDocument = toml::from_str(builtin)?;
    let mut order = Vec::new();
    let mut merged: HashMap<String, ProfilePatch> = HashMap::new();
    let mut builtin_ids = HashSet::new();

    for patch in builtin_doc.profiles {
        let id = required_id(&patch)?;
        if !builtin_ids.insert(id.clone()) {
            return Err(ProfileError::DuplicateId(id));
        }
        if patch.label.is_none() {
            return Err(ProfileError::MissingLabel(id));
        }
        order.push(id.clone());
        merged.insert(id, patch);
    }

    if let Some(user) = user {
        let user_doc: ProfileDocument = toml::from_str(user)?;
        let mut user_ids = HashSet::new();
        for patch in user_doc.profiles {
            let id = required_id(&patch)?;
            if !user_ids.insert(id.clone()) {
                return Err(ProfileError::DuplicateId(id));
            }
            if let Some(existing) = merged.get_mut(&id) {
                merge_patch(existing, patch);
            } else {
                order.push(id.clone());
                merged.insert(id, patch);
            }
        }
    }

    let mut profiles = Vec::with_capacity(order.len());
    for id in order {
        let patch = merged.remove(&id).expect("order 与 merged 同源");
        profiles.push(resolve_patch(id, patch)?);
    }

    Ok(ProfileSet {
        profiles,
        generic: Profile::generic(),
    })
}

fn required_id(patch: &ProfilePatch) -> Result<String, ProfileError> {
    patch
        .id
        .as_ref()
        .filter(|id| !id.trim().is_empty())
        .cloned()
        .ok_or(ProfileError::MissingId)
}

fn merge_patch(base: &mut ProfilePatch, overlay: ProfilePatch) {
    if overlay.label.is_some() {
        base.label = overlay.label;
    }
    if overlay.exe_names.is_some() {
        base.exe_names = overlay.exe_names;
    }
    if overlay.class_names.is_some() {
        base.class_names = overlay.class_names;
    }
    if overlay.title_regexes.is_some() {
        base.title_regexes = overlay.title_regexes;
    }
    if overlay.not_ready_title_regexes.is_some() {
        base.not_ready_title_regexes = overlay.not_ready_title_regexes;
    }
    if overlay.formats.is_some() {
        base.formats = overlay.formats;
    }
    if overlay.paste_sends.is_some() {
        base.paste_sends = overlay.paste_sends;
    }
    if overlay.readiness.is_some() {
        base.readiness = overlay.readiness;
    }
    if overlay.settle_ms.is_some() {
        base.settle_ms = overlay.settle_ms;
    }
    if overlay.focus_strategy.is_some() {
        base.focus_strategy = overlay.focus_strategy;
    }
    if overlay.input_anchor.is_some() {
        base.input_anchor = overlay.input_anchor;
    }
}

fn resolve_patch(id: String, patch: ProfilePatch) -> Result<Profile, ProfileError> {
    let label = patch
        .label
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| id.clone());
    let title_regexes = patch.title_regexes.unwrap_or_default();
    let not_ready_title_regexes = patch.not_ready_title_regexes.unwrap_or_default();
    validate_regexes(&id, &title_regexes)?;
    validate_regexes(&id, &not_ready_title_regexes)?;
    if let Some(anchor) = patch.input_anchor {
        // 越界比例一律报错而不是夹紧：静默夹紧会让错配的画像看起来「能用」，
        // 却把点击落到输入框以外的控件上。
        if !(0.0..=1.0).contains(&anchor.x_ratio) || !(0.0..=1.0).contains(&anchor.y_ratio) {
            return Err(ProfileError::InvalidAnchor {
                profile: id,
                x: anchor.x_ratio,
                y: anchor.y_ratio,
            });
        }
    }
    if let Some(strategy) = patch.focus_strategy.as_ref() {
        if strategy.is_empty() {
            return Err(ProfileError::EmptyFocusStrategy(id));
        }
    }

    Ok(Profile {
        id: TargetId::new(id),
        label,
        exe_names: patch.exe_names.unwrap_or_default(),
        class_names: patch.class_names.unwrap_or_default(),
        title_regexes,
        not_ready_title_regexes,
        formats: patch.formats.unwrap_or_default(),
        paste_sends: patch.paste_sends.unwrap_or_default(),
        readiness: patch.readiness.unwrap_or_default(),
        settle_ms: patch.settle_ms.unwrap_or(80),
        focus_strategy: patch.focus_strategy.unwrap_or_else(default_focus_strategy),
        input_anchor: patch.input_anchor,
    })
}

fn validate_regexes(profile: &str, patterns: &[String]) -> Result<(), ProfileError> {
    for pattern in patterns {
        if let Err(source) = Regex::new(pattern) {
            return Err(ProfileError::InvalidRegex {
                profile: profile.to_string(),
                pattern: pattern.clone(),
                source,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILTIN: &str = r#"
[[profiles]]
id = "wechat"
label = "微信"
exe_names = ["WeChat.exe"]
class_names = ["WeChatMainWndForPC"]
settle_ms = 80

[profiles.formats]
image = ["png", "files"]
video = ["files"]
text = ["text"]
other = []
"#;

    #[test]
    fn user_profile_overrides_builtin_by_id() {
        let user = r#"
[[profiles]]
id = "wechat"
settle_ms = 140
title_regexes = ["微信.*"]
"#;
        let set = load_profiles(BUILTIN, Some(user)).unwrap();
        let profile = set.get(&TargetId::new("wechat")).unwrap();
        assert_eq!(profile.label, "微信");
        assert_eq!(profile.exe_names, ["WeChat.exe"]);
        assert_eq!(profile.settle_ms, 140);
        assert_eq!(profile.title_regexes, ["微信.*"]);
    }

    #[test]
    fn malformed_profile_is_rejected_not_silently_defaulted() {
        let malformed = r#"
[[profiles]]
id = "broken"
label = "坏画像"
title_regexes = ["["]
"#;
        assert!(matches!(
            load_profiles(malformed, None),
            Err(ProfileError::InvalidRegex { .. })
        ));
    }

    #[test]
    fn duplicate_profile_id_is_rejected_instead_of_panicking() {
        let duplicate = r#"
[[profiles]]
id = "wechat"
label = "微信"

[[profiles]]
id = "wechat"
label = "重复微信"
"#;
        assert!(matches!(
            load_profiles(duplicate, None),
            Err(ProfileError::DuplicateId(id)) if id == "wechat"
        ));
    }

    #[test]
    fn unknown_exe_falls_back_to_generic_profile() {
        let set = load_profiles(BUILTIN, None).unwrap();
        assert_eq!(set.generic().id.as_str(), "generic_im");
        assert_eq!(set.generic().formats.image[0], ClipboardFormat::Png);
    }

    #[test]
    fn input_anchor_is_parsed_and_exposed_as_focus_anchor() {
        let doc = r#"
[[profiles]]
id = "qianniu"
label = "千牛"

[profiles.input_anchor]
x_ratio = 0.394
y_ratio = 0.787
"#;
        let set = load_profiles(doc, None).unwrap();
        let profile = set.get(&TargetId::new("qianniu")).unwrap();
        let anchor = profile.focus_anchor().expect("画像声明了锚点");
        assert!((anchor.x_ratio - 0.394).abs() < f32::EPSILON);
        assert!((anchor.y_ratio - 0.787).abs() < f32::EPSILON);
    }

    #[test]
    fn profile_without_input_anchor_yields_no_click_target() {
        let set = load_profiles(BUILTIN, None).unwrap();
        let profile = set.get(&TargetId::new("wechat")).unwrap();
        assert!(profile.focus_anchor().is_none());
        assert!(Profile::generic().focus_anchor().is_none());
    }

    #[test]
    fn out_of_range_anchor_is_rejected_instead_of_clamped() {
        let doc = r#"
[[profiles]]
id = "broken"
label = "坏锚点"

[profiles.input_anchor]
x_ratio = 1.4
y_ratio = 0.5
"#;
        assert!(matches!(
            load_profiles(doc, None),
            Err(ProfileError::InvalidAnchor { profile, .. }) if profile == "broken"
        ));
    }

    #[test]
    fn user_profile_can_retune_input_anchor() {
        let builtin = r#"
[[profiles]]
id = "wechat"
label = "微信"

[profiles.input_anchor]
x_ratio = 0.66
y_ratio = 0.85
"#;
        let user = r#"
[[profiles]]
id = "wechat"

[profiles.input_anchor]
x_ratio = 0.5
y_ratio = 0.9
"#;
        let set = load_profiles(builtin, Some(user)).unwrap();
        let anchor = set
            .get(&TargetId::new("wechat"))
            .unwrap()
            .focus_anchor()
            .unwrap();
        assert!((anchor.x_ratio - 0.5).abs() < f32::EPSILON);
        assert!((anchor.y_ratio - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn profile_without_focus_strategy_keeps_three_level_default() {
        let set = load_profiles(BUILTIN, None).unwrap();
        let profile = set.get(&TargetId::new("wechat")).unwrap();
        assert_eq!(
            profile.focus_plan().steps,
            vec![
                FocusStep::AlreadyEditable,
                FocusStep::UiaSetFocus,
                FocusStep::AnchorClick
            ],
            "未声明 focus_strategy 的画像必须保持历史三级降级顺序"
        );
        assert_eq!(
            Profile::generic().focus_plan().steps,
            vec![
                FocusStep::AlreadyEditable,
                FocusStep::UiaSetFocus,
                FocusStep::AnchorClick
            ]
        );
    }

    #[test]
    fn declared_focus_strategy_can_skip_uia_scan() {
        let doc = r#"
[[profiles]]
id = "wechat"
label = "微信"
focus_strategy = ["already", "anchor"]
"#;
        let set = load_profiles(doc, None).unwrap();
        let steps = set
            .get(&TargetId::new("wechat"))
            .unwrap()
            .focus_plan()
            .steps;
        assert_eq!(
            steps,
            vec![FocusStep::AlreadyEditable, FocusStep::AnchorClick],
            "声明了策略就必须逐字生效：跳过 UIA 才能省下实测 22~83ms 的纯损耗"
        );
        assert!(!steps.contains(&FocusStep::UiaSetFocus));
    }

    #[test]
    fn empty_focus_strategy_is_rejected_instead_of_silently_defaulted() {
        let doc = r#"
[[profiles]]
id = "broken"
label = "空策略"
focus_strategy = []
"#;
        assert!(matches!(
            load_profiles(doc, None),
            Err(ProfileError::EmptyFocusStrategy(id)) if id == "broken"
        ));
    }

    #[test]
    fn user_profile_can_replace_focus_strategy_wholesale() {
        let builtin = r#"
[[profiles]]
id = "wechat"
label = "微信"
focus_strategy = ["already", "anchor"]
"#;
        let user = r#"
[[profiles]]
id = "wechat"
focus_strategy = ["already", "uia", "anchor"]
"#;
        let set = load_profiles(builtin, Some(user)).unwrap();
        assert_eq!(
            set.get(&TargetId::new("wechat")).unwrap().focus_strategy,
            vec![
                FocusStrategyStep::Already,
                FocusStrategyStep::Uia,
                FocusStrategyStep::Anchor
            ],
            "用户画像声明的策略整体覆盖内置，而不是追加"
        );
    }

    /// 即发声明的表写法：逐类别，允许同一格式在不同类别上得出相反结论。
    #[test]
    fn per_kind_paste_sends_is_parsed_per_kind() {
        let doc = r#"
[[profiles]]
id = "qianniu"
label = "千牛"
paste_sends = { video = ["files"] }
"#;
        let set = load_profiles(doc, None).unwrap();
        let profile = set.get(&TargetId::new("qianniu")).unwrap();
        assert!(profile.paste_sends_format(FormatKind::Video, ClipboardFormat::Files));
        assert!(
            !profile.paste_sends_format(FormatKind::Image, ClipboardFormat::Files),
            "未声明的类别不得继承别的类别的即发结论"
        );
    }

    /// 旧用户画像的数组写法必须继续解析，且语义是「所有类别都即发」——
    /// 静默解析失败或静默缩小范围都会悄悄拆掉用户自己装的发送保护。
    #[test]
    fn legacy_flat_paste_sends_array_still_parses_as_all_kinds() {
        let doc = r#"
[[profiles]]
id = "qianniu"
label = "千牛"
paste_sends = ["files"]
"#;
        let set = load_profiles(doc, None).unwrap();
        let profile = set.get(&TargetId::new("qianniu")).unwrap();
        for kind in [
            FormatKind::Image,
            FormatKind::Video,
            FormatKind::Text,
            FormatKind::Other,
        ] {
            assert!(profile.paste_sends_format(kind, ClipboardFormat::Files));
            assert!(!profile.paste_sends_format(kind, ClipboardFormat::Png));
        }
    }

    /// 未声明即发的画像对任何组合都不得判定为即发（缺省不是「保守拦一切」，
    /// 因为那会把正常上框全部降级为仅复制）。
    #[test]
    fn profile_without_paste_sends_never_reports_send() {
        let set = load_profiles(BUILTIN, None).unwrap();
        let profile = set.get(&TargetId::new("wechat")).unwrap();
        assert!(!profile.paste_sends_format(FormatKind::Image, ClipboardFormat::Files));
        assert!(!Profile::generic().paste_sends_format(FormatKind::Video, ClipboardFormat::Files));
    }
}
