//! 库目录装载：Store 门面 → [`FacetIndex`] 装配。
//!
//! 分层依据（spec ui-viewmodels database-guidelines）：VM 不直接持有 Connection，
//! 经 Store 门面访问；app-ui 依赖白名单只有本 crate + slint，故 `--bench` 内存
//! 守卫路径（design.md 契约：`Store::open(root/meta.db) → 读全量 AssetMeta →
//! 建 FacetIndex`）的组装收拢在本模块。
//!
//! 峰值驻留纪律（D3/D4）：经 [`store::Store::for_each_asset`] 流式遍历，
//! 边读边装配，不物化全量 AssetMeta Vector。

use std::fmt;
use std::path::Path;

use domain::{Asset, AssetId};
use index::FacetIndex;
use store::Store;

#[derive(Debug)]
pub enum CatalogError {
    /// meta.db 不存在或不是文件。
    MissingDatabase(String),
    Store(store::StoreError),
    Io(std::io::Error),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::MissingDatabase(msg) => write!(f, "库目录无效: {msg}"),
            CatalogError::Store(e) => write!(f, "存储错误: {e}"),
            CatalogError::Io(e) => write!(f, "IO 错误: {e}"),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<store::StoreError> for CatalogError {
    fn from(e: store::StoreError) -> Self {
        CatalogError::Store(e)
    }
}

impl From<std::io::Error> for CatalogError {
    fn from(e: std::io::Error) -> Self {
        CatalogError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, CatalogError>;

/// 打开 `<root>/meta.db` 并把全库资产装配为 [`FacetIndex`]。
///
/// id 映射契约（design.md）：uuid → 顺序 [`AssetId`]——按 uuid 升序遍历，
/// 第 i 行得 `AssetId(i)`。合成库 uuid 形如 `bench-{i:08}`（零填充字典序 ==
/// 数值序），故顺序 id 与生成器下标一一对应。
///
/// 分类/标签映射说明：字符串分类 → CategoryId 需注册表（后续里程碑）；
/// 合成库契约两者恒空，非空时保守降级丢弃（内存守卫对象是行数与驻留
/// 结构，非分类语义）。
pub fn load_library_catalog(root: &Path) -> Result<FacetIndex> {
    let db = root.join("meta.db");
    if !db.is_file() {
        return Err(CatalogError::MissingDatabase(format!(
            "{} 下无 meta.db",
            root.display()
        )));
    }
    let store = Store::open(&db)?;
    let mut idx = FacetIndex::new();
    let mut next_id: u32 = 0;
    store.for_each_asset(|meta| {
        // 字符串分类/tags → id 需注册表（后续里程碑）；非空值保守丢弃而非
        // 压缩到同一 id（那会伪造分桶语义）。内存守卫对象是行数与驻留结构。
        idx.insert(&Asset {
            id: AssetId(next_id),
            name: meta.file_name,
            category: None,
            tags: vec![],
            created_at: meta.created_at,
        });
        next_id += 1;
    })?;
    Ok(idx)
}
