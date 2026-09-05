//! IM 目标路由的纯逻辑层。
//!
//! 本 crate 只消费 `platform` 的平台无关值类型，不执行 IO，也不调用任何
//! 操作系统 API。配置加载只接受字符串，文件读取由装配层负责。

pub mod alias;
pub mod health;
pub mod matcher;
pub mod model;
pub mod profile;
pub mod tracker;

pub use alias::AliasMap;
pub use health::{evaluate_health, HealthCheckInput, HealthLevel, HealthReport, SelfTestReport};
pub use matcher::{
    matching_profile_windows, resolve_eligible_snapshot, resolve_profile_windows, resolve_snapshot,
    MatchResult, ResolvedTarget,
};
pub use model::{Health, NotReadyReason, Readiness, TargetBinding, TargetId, TargetScope};
pub use platform::{WindowHandle, WindowRect, WindowSnapshot};
pub use profile::{
    load_profiles, ClipboardFormat, FocusStrategyStep, FormatKind, FormatPolicy, InputAnchor,
    InputPointConfig, KindFormats, Profile, ProfileError, ProfileSet, ReadinessMode, SendPolicy,
};
pub use tracker::TargetTracker;
