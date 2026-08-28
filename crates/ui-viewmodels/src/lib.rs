//! ViewModel 层：桥接 UI 与核心 crates，纯 Rust 可全量单测（禁 slint 依赖，TDD 第一原则落点）。

pub mod catalog_loader;
pub mod grid_vm;
pub mod layout;
pub mod search;
pub mod settings;
pub mod target_bar_vm;
// 运行时门面只依赖 platform 的 trait 层，具体平台实现由二进制入口注入，故无平台门。
pub mod target_runtime;
pub mod theme;

pub use catalog_loader::{
    load_library_catalog, load_real_library, CatalogError, FacetEntry, LibraryFacets,
    MaterializedAsset, RealAssetResolver,
};
pub use grid_vm::{LibraryGridVm, ThumbnailProvider, VmEvent};
pub use layout::{masonry_layout, Rect};
pub use search::{FacetSearchProvider, SearchError, SearchProvider};
pub use settings::{
    settings_path, AppSettings, SettingKind, SettingSpec, SettingView, SETTING_SPECS,
    SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH,
};
pub use target_bar_vm::{
    TargetBarMode, TargetBarSnapshot, TargetBarVm, TargetChoice, TargetNoticeTone,
    TargetPasteNotice, TargetRoutingVm,
};
pub use theme::{DarkThemeProvider, LightThemeProvider, ThemeProvider, ThemeTokens};

pub use pipeline::{AssetKind, AssetPayload, TargetPipelineDeps};
pub use target_runtime::{TargetRoutingRuntime, TargetRuntimeDeps};
pub use targets::{Health as TargetHealth, ProfileError as TargetProfileError};

/// 壳层（app-ui）依赖白名单只有本 crate + slint，装配所需的领域类型经此转发。
pub use domain::{
    Asset, AssetId, CategoryId, Filter, SortDirection, SortField, SortSpec, Sorter, TagId,
};
pub use index::FacetIndex;
