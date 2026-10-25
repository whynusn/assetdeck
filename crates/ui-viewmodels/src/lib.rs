//! ViewModel 层：桥接 UI 与核心 crates，纯 Rust 可全量单测（禁 slint 依赖，TDD 第一原则落点）。

pub mod catalog_loader;
pub mod category_rows;
pub mod classify;
pub mod grid_vm;
pub mod layout;
pub mod legacy_migration;
pub mod notifications;
pub mod search;
pub mod selection;
pub mod settings;
pub mod share_control;
pub mod share_vm;
pub mod space_vm;
pub mod target_bar_vm;
// 运行时门面只依赖 platform 的 trait 层，具体平台实现由二进制入口注入，故无平台门。
pub mod target_runtime;
pub mod theme;
pub mod update_apply;
pub mod update_check;

pub use catalog_loader::{
    content_paths, ensure_library_skeleton, load_library_catalog, load_real_library, CatalogError,
    FacetEntry, LibraryFacets, MaterializedAsset, RealAssetResolver,
};
pub use category_rows::CategoryRows;
pub use grid_vm::{LibraryGridVm, ThumbnailProvider, VmEvent, TEXT_TILE_HEIGHT};
pub use layout::{
    column_width, masonry_layout, masonry_layout_mixed, LayoutItem, Rect, FULL_ROW_GAP,
};
pub use search::{
    fit_search_snippet, fts_query_eligible, name_zone_px, split_search_snippet, FtsNameSource,
    HybridSearchProvider, SearchError, SearchProvider, SearchScope, SNIPPET_QUERY_LIMIT,
    TEXT_NAME_ZONE_FRACTION, TEXT_NAME_ZONE_MAX, TEXT_NAME_ZONE_MIN,
};
pub use settings::{
    settings_path, AppSettings, InputPointOverride, SettingKind, SettingSpec, SettingView,
    SETTING_SPECS, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH,
};
pub use share_vm::{
    delta_sync_lines, group_roster_of, human_size, roster_send_lines, sync_state_line,
    PendingRequest, ShareAction, ShareBadge, ShareBatch, ShareFinal, SharePhase, ShareVm,
};
// D80-M1-a 控制面：持久化契约与域形态枚举经 share 转发；app-ui 依赖白名单
// 不含 share（deps_guard EXACT 纪律），一律走本 crate。
pub use share::{DomainKind, ShareRegistry};
// D80-M2 邀请机制：邀请码编解码与名册模型经此转发（app-ui 白名单不含 share）。
pub use share::{group_invite_code, parse_invite_code, GroupRoster, InviteCode, RosterMember};
pub use share_control::{ControlError, ShareControlVm};
// D80 共享通道装配所需的传输类型：app-ui 依赖白名单不含 share（deps_guard
// EXACT 纪律），SendItem/DeviceEntry/ShareOffer 经此转发；uuid 同理（角标键与批次 id）。
pub use share::{DeviceEntry, SendItem, ShareOffer};
// D80-M1-c 配对码指纹（share pairing 模块）：app-ui 走本 crate。
pub use share::pairing_code;
// D90 共享空间：模型与 VM 经此转发（app-ui 白名单不含 share，同上）。
pub use share::{
    parse_space_invite_code, space_invite_code, SpaceError, SpaceInviteCode, SpaceMeta, SpaceOp,
    SpaceOpKind, SpaceRegistry, SpaceRole,
};
// D90-P2：空间同步的报文/服务册/资产元数据/上限（app-ui 白名单不含 share）。
pub use share::{
    merge_lamport, ServeItem, SpaceAssetKind, SpaceAssetMeta, SpaceBookEntry, SpaceBookState,
    SpaceMessage, MAX_FETCH_ITEMS,
};
pub use space_vm::{
    parse_staged_payload, parse_transfer_manifest, sort_and_dedupe_ops, space_book_json,
    space_ops_from_lines, space_ops_to_lines, space_send_line, SpaceVm,
};
pub use target_bar_vm::{
    TargetBarMode, TargetBarSnapshot, TargetBarVm, TargetChoice, TargetNoticeTone,
    TargetPasteNotice, TargetRoutingVm, TuningTarget,
};
pub use theme::{DarkThemeProvider, LightThemeProvider, ThemeProvider, ThemeTokens};
pub use update_apply::{
    hash_matches, installer_asset_name, load_download_mirrors, mirror_candidates, mirror_label,
    parse_sha256_sums, pick_asset, rank_by_probe, UpdateApplyVm, DEFAULT_DOWNLOAD_MIRRORS,
    DOWNLOAD_TIMEOUT_MS, INSTALLER_ASSET_PREFIX, MAX_DOWNLOAD_BYTES, SUMS_ASSET_NAME,
};
pub use update_check::{
    check_update, load_feeds, relative_time, silent_check_due, unix_now_secs, CheckOutcome,
    ReleaseAsset, ReleaseInfo, UpdateCheckVm, UpdateUiAction, CHECK_INTERVAL_SECS, DEFAULT_FEEDS,
    FETCH_TIMEOUT_MS,
};
pub use uuid;

pub use pipeline::{AssetPayload, PayloadKind, TargetPipelineDeps};
pub use target_runtime::{TargetRoutingRuntime, TargetRuntimeDeps};
pub use targets::{AliasMap, Health as TargetHealth, ProfileError as TargetProfileError};

/// 壳层（app-ui）依赖白名单只有本 crate + slint，装配所需的领域类型经此转发。
pub use domain::{
    Asset, AssetId, AssetKind, CategoryId, Filter, SortDirection, SortField, SortSpec, Sorter,
    TagId,
};
pub use index::FacetIndex;
