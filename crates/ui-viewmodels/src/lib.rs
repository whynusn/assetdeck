//! ViewModel 层：桥接 UI 与核心 crates，纯 Rust 可全量单测（禁 slint 依赖，TDD 第一原则落点）。

pub mod catalog_loader;
pub mod grid_vm;
pub mod layout;

pub use catalog_loader::{load_library_catalog, CatalogError};
pub use grid_vm::{LibraryGridVm, ThumbnailProvider, VmEvent};
pub use layout::{masonry_layout, Rect};

/// 壳层（app-ui）依赖白名单只有本 crate + slint，装配所需的领域类型经此转发。
pub use domain::{
    Asset, AssetId, CategoryId, Filter, SortDirection, SortField, SortSpec, Sorter, TagId,
};
pub use index::FacetIndex;
