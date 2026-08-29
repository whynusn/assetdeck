//! 库目录装载：Store 门面 → [`FacetIndex`] 装配。
//!
//! 分层依据（spec ui-viewmodels database-guidelines）：VM 不直接持有 Connection，
//! 经 Store 门面访问；app-ui 依赖白名单只有本 crate + slint，故 `--bench` 内存
//! 守卫路径（design.md 契约：`Store::open(root/meta.db) → 读全量 AssetMeta →
//! 建 FacetIndex`）的组装收拢在本模块。
//!
//! 峰值驻留纪律（D3/D4）：经 [`store::Store::for_each_asset`] 流式遍历，
//! 边读边装配，不物化全量 AssetMeta Vector。

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use domain::{Asset, AssetId, AssetKind, CategoryId, Filter, TagId};
use index::FacetIndex;
use pipeline::AssetPayload;
use store::Store;

#[derive(Debug)]
pub enum CatalogError {
    /// meta.db 不存在或不是文件。
    MissingDatabase(String),
    Store(store::StoreError),
    Io(std::io::Error),
    /// 素材存在但不可上框：已删除（回收站）/对象文件缺失。
    PasteBlocked(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::MissingDatabase(msg) => write!(f, "库目录无效: {msg}"),
            CatalogError::Store(e) => write!(f, "存储错误: {e}"),
            CatalogError::Io(e) => write!(f, "IO 错误: {e}"),
            CatalogError::PasteBlocked(msg) => write!(f, "{msg}"),
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

/// 一个分类/标签条目：稳定数字 id、显示名与命中计数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetEntry {
    pub id: u32,
    pub name: String,
    pub count: u32,
}

/// 分类/标签名称注册表：检索器在内存里对名称做子串匹配组装 [`Filter`]，
/// 不受 FTS5 trigram（≥3 连续字符）限制。id 与 `load_real_library` 分配给
/// `FacetIndex` 的 [`CategoryId`]/[`TagId`] 一致。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryFacets {
    categories: Vec<FacetEntry>,
    tags: Vec<FacetEntry>,
    category_by_name: HashMap<String, CategoryId>,
}

impl LibraryFacets {
    /// 全部分类（按名升序，含计数）。
    pub fn categories(&self) -> &[FacetEntry] {
        &self.categories
    }

    /// 全部标签（按名升序，含计数）。
    pub fn tags(&self) -> &[FacetEntry] {
        &self.tags
    }

    /// 按精确分类名取 id（工具栏点击某分类时用）。
    pub fn category_id(&self, name: &str) -> Option<CategoryId> {
        self.category_by_name.get(name).copied()
    }

    /// 分类名列表，下标即 [`CategoryId`]（保留旧 `category_names()` 语义）。
    pub fn category_names(&self) -> Vec<String> {
        let mut names = vec![String::new(); self.categories.len()];
        for entry in &self.categories {
            if let Some(slot) = names.get_mut(entry.id as usize) {
                *slot = entry.name.clone();
            }
        }
        names
    }

    /// 对分类名 + 标签名做子串模糊匹配，命中即组装 `Filter::AnyOf`。
    ///
    /// 空/纯空白查询返回 `None`（调用方回落当前分类视图）；有查询但无命中
    /// 返回 `Some(AnyOf(vec![]))`（空集，瀑布流清空）。
    pub fn fuzzy_filter(&self, query: &str) -> Option<Filter> {
        if query.trim().is_empty() {
            return None;
        }
        let mut clauses: Vec<Filter> = self
            .category_matches(query)
            .into_iter()
            .map(Filter::InCategory)
            .collect();
        clauses.extend(self.tag_matches(query).into_iter().map(Filter::HasTag));
        Some(Filter::AnyOf(clauses))
    }

    /// 分类名折叠命中（D51：大小写统一——ASCII 与 Unicode 同 domain::text 语义）。
    pub fn category_matches(&self, query: &str) -> Vec<CategoryId> {
        let needle = domain::text::fold_lower(query);
        if needle.is_empty() {
            return Vec::new();
        }
        let mut ring = Vec::with_capacity(needle.len());
        self.categories
            .iter()
            .filter(|entry| domain::text::contains_case_fold(&entry.name, &needle, &mut ring))
            .map(|entry| CategoryId(entry.id))
            .collect()
    }

    /// 标签名折叠命中（同上）。
    pub fn tag_matches(&self, query: &str) -> Vec<TagId> {
        let needle = domain::text::fold_lower(query);
        if needle.is_empty() {
            return Vec::new();
        }
        let mut ring = Vec::with_capacity(needle.len());
        self.tags
            .iter()
            .filter(|entry| domain::text::contains_case_fold(&entry.name, &needle, &mut ring))
            .map(|entry| TagId(entry.id))
            .collect()
    }
}

/// D52：FTS 名检索接缝的实现落点——resolver 同持 store 与升序 uuids，
/// 映射（含回收站彻底删除窗口期的未知 uuid 容错）内聚在此。
impl crate::search::FtsNameSource for RealAssetResolver {
    fn name_ids(
        &self,
        query: &str,
        limit: usize,
    ) -> std::result::Result<Vec<u32>, crate::search::SearchError> {
        RealAssetResolver::name_ids(self, query, limit)
    }
}

/// 从真实 `.library` 目录解析出的素材载荷。所有权归 resolver，调用方短暂借用。
pub struct MaterializedAsset {
    pub kind: AssetKind,
    pub png_bytes: Vec<u8>,
    pub source_path: std::path::PathBuf,
    pub text: String,
}

impl MaterializedAsset {
    pub fn as_payload(&self) -> AssetPayload<'_> {
        AssetPayload {
            kind: self.kind,
            png_bytes: &self.png_bytes,
            source_path: self.source_path.clone(),
            text: self.text.clone(),
        }
    }
}

/// 真实库素材解析：持有 Store 连接与 uuid→AssetId 映射。
///
/// 与 `load_library_catalog` 分开考虑内存：调用方完成索引后可以继续持有
/// 本 resolver，双击时才从 meta.db 取值并读取文件字节。
///
/// 上框热路径优化：最近物化的载荷按 LRU 缓存（见 MATERIALIZE_CACHE_BYTE_BUDGET），
/// 同一素材反复上框不重复读盘。库为「复制入库」模型（D3：导入即拷入库内目录），
/// raw 文件不会原地被改，缓存陈旧风险可接受（v1 已知取舍，见 D20 派生语义）。
pub struct RealAssetResolver {
    root: std::path::PathBuf,
    store: Store,
    uuids: Vec<String>,
    facets: LibraryFacets,
    /// (AssetId, 物化载荷) LRU：队首最旧、队尾最新，字节预算封顶。
    cache: RefCell<VecDeque<(u32, Arc<MaterializedAsset>)>>,
}

/// 物化缓存条目上限（LRU 淘汰阈值）。
const MATERIALIZE_CACHE_MAX_ENTRIES: usize = 4;
/// 物化缓存总字节预算：单素材超过预算不缓存；累计超预算按 LRU 淘汰最旧。
/// 上框载荷由 worker 以 4096 cap 派生（D20），典型几 MB；16MB 预算可容纳
/// 2~8 个素材，且远低于空闲内存预算（AC10 <100MB）的峰值冲击。
const MATERIALIZE_CACHE_BYTE_BUDGET: usize = 16 * 1024 * 1024;

impl RealAssetResolver {
    pub fn len(&self) -> usize {
        self.uuids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.uuids.is_empty()
    }

    /// 分类名列表，下标即 [`CategoryId`]（与 `load_real_library` 分配顺序一致）。
    ///
    /// 委托给 [`LibraryFacets::category_names`]；保留给现有 `apply_categories` 调用方。
    pub fn category_names(&self) -> Vec<String> {
        self.facets.category_names()
    }

    /// 分类/标签名称注册表（检索器与工具栏计数用）。
    pub fn facets(&self) -> &LibraryFacets {
        &self.facets
    }

    /// FTS5 全文检索直通（store 侧已 JOIN 过滤 `deleted=0`，回收站行不泄漏）。
    /// 检索 UI 的两条路之一：内存名扫描走 `FacetIndex::NameContains`，
    /// 升序 uuid 表的只读视图（binary_search 前提，守卫测试锁定升序不变量）。
    pub fn uuids(&self) -> &[String] {
        &self.uuids
    }

    /// uuid → 行号（D52 零常驻映射：持引用二分，无克隆）。
    pub fn uuid_rank(&self, uuid: &str) -> Option<u32> {
        self.uuids
            .binary_search_by(|probe| probe.as_str().cmp(uuid))
            .ok()
            .map(|index| index as u32)
    }

    /// FTS 名检索 → 行号升序 id 集（D52 混合路由的数据源）。
    /// 未知 uuid（回收站后彻底删除期间等窗口）跳过；库错误包装为
    /// FtsUnavailable，由 Provider 降级内存路。
    pub fn name_ids(
        &self,
        query: &str,
        limit: usize,
    ) -> std::result::Result<Vec<u32>, crate::search::SearchError> {
        let hits = self
            .store_search(query, limit)
            .map_err(|_| crate::search::SearchError::FtsUnavailable)?;
        Ok(hits
            .iter()
            .filter_map(|hit| self.uuid_rank(&hit.uuid))
            .collect())
    }

    /// 全文/标签语义走这里，结果以 uuid 二分回 `uuids` 行号（D52）。
    pub fn store_search(&self, query: &str, limit: usize) -> Result<Vec<store::SearchHit>> {
        Ok(self.store.search(query, limit)?)
    }

    /// 行号 → uuid（子命令派发 `--cmd trash --uuid …` 与属性面板寻址用）。
    pub fn uuid_of(&self, id: AssetId) -> Option<&str> {
        self.uuids.get(id.0 as usize).map(|s| s.as_str())
    }

    /// 行号对应素材的元数据（属性面板数据源）。行被彻底删除后返回 None。
    pub fn meta_of(&self, id: AssetId) -> Result<Option<store::AssetMeta>> {
        match self.uuid_of(id) {
            Some(uuid) => Ok(self.store.get_asset(uuid)?),
            None => Ok(None),
        }
    }

    /// 素材正本的绝对路径（属性面板「路径」字段，D14 教训：展示绝对路径）。
    pub fn absolute_path(&self, id: AssetId) -> Option<PathBuf> {
        let uuid = self.uuid_of(id)?;
        let meta = self.store.get_asset(uuid).ok().flatten()?;
        let joined = self.root.join(meta.rel_path);
        Some(std::path::absolute(&joined).unwrap_or(joined))
    }

    /// 浏览用缩略图的绝对路径；文件不存在时返回 `None`（瓦片回落纯色）。
    ///
    /// 只回路径不读字节：渲染端按需装载并自己管住驻留，避免「VM 存编码字节 +
    /// 渲染端存解码像素」两份常驻。派生工序见 `tools/derive-thumbs`。
    pub fn thumbnail_path(&self, id: AssetId) -> Option<PathBuf> {
        let uuid = self.uuids.get(id.0 as usize)?;
        let path = self.root.join(Store::thumbnail_cache_path(uuid, "png"));
        let path = std::path::absolute(&path).unwrap_or(path);
        path.is_file().then_some(path)
    }

    /// 真实宽高比表（`AssetId → w/h`），供 [`crate::LibraryGridVm::set_aspects`] 驱动版式。
    ///
    /// 遍历顺序与 [`load_real_library`] 分配 id 的顺序一致（同一 `for_each_asset`
    /// 契约），所以这里用行序计数而不是逐 uuid 查询。缺尺寸的行直接不入表，
    /// VM 侧回落占位比例。
    pub fn aspects(&self) -> Result<HashMap<AssetId, f32>> {
        let mut aspects = HashMap::new();
        let mut next_id: u32 = 0;
        self.store.for_each_asset(|meta| {
            if let Some(aspect) = meta.aspect() {
                aspects.insert(AssetId(next_id), aspect);
            }
            next_id += 1;
        })?;
        Ok(aspects)
    }

    pub fn materialize_by_file_name(
        &self,
        file_name: &str,
    ) -> Result<Option<Arc<MaterializedAsset>>> {
        for (index, uuid) in self.uuids.iter().enumerate() {
            // 缓存命中优先：热路径不碰 meta.db。库为复制入库模型，id→文件名
            // 关系稳定，命中缓存不必复核文件名。
            if let Some(asset) = self.cache_take(index as u32) {
                return Ok(Some(asset));
            }
            let Some(meta) = self.store.get_asset(uuid)? else {
                continue;
            };
            if meta.file_name == file_name {
                return self.materialize_and_cache(index as u32, meta);
            }
        }
        Ok(None)
    }

    pub fn materialize(&self, id: AssetId) -> Result<Option<Arc<MaterializedAsset>>> {
        // 缓存命中优先：上框热路径完全不读盘、不碰 meta.db。
        if let Some(asset) = self.cache_take(id.0) {
            return Ok(Some(asset));
        }
        let Some(uuid) = self.uuids.get(id.0 as usize) else {
            return Ok(None);
        };
        let Some(meta) = self.store.get_asset(uuid)? else {
            return Ok(None);
        };
        self.materialize_and_cache(id.0, meta)
    }

    /// 物化并把结果放入 LRU 缓存（缓存命中时提前返回，不会走到这里）。
    fn materialize_and_cache(
        &self,
        id: u32,
        meta: store::AssetMeta,
    ) -> Result<Option<Arc<MaterializedAsset>>> {
        let Some(asset) = self.materialize_debug(id, meta)? else {
            return Ok(None);
        };
        let asset = Arc::new(asset);
        self.cache_insert(id, Arc::clone(&asset));
        Ok(Some(asset))
    }

    /// LRU 命中：取出并把该条目移到队尾（最近使用），返回同一 Arc 实例。
    fn cache_take(&self, id: u32) -> Option<Arc<MaterializedAsset>> {
        let mut cache = self.cache.borrow_mut();
        let pos = cache.iter().position(|(cached_id, _)| *cached_id == id)?;
        let (_, asset) = cache.remove(pos).expect("position 已由上面的查找校验");
        cache.push_back((id, Arc::clone(&asset)));
        Some(asset)
    }

    /// LRU 插入：超单素材预算不缓存；累计超预算或超条目数时从队首逐出最旧。
    fn cache_insert(&self, id: u32, asset: Arc<MaterializedAsset>) {
        let bytes = asset.png_bytes.len();
        if bytes > MATERIALIZE_CACHE_BYTE_BUDGET {
            // 单素材超预算：不缓存（读盘成本已由 worker 的 4096 cap 压住）。
            return;
        }
        let mut cache = self.cache.borrow_mut();
        cache.push_back((id, asset));
        let mut total: usize = cache.iter().map(|(_, a)| a.png_bytes.len()).sum();
        while cache.len() > MATERIALIZE_CACHE_MAX_ENTRIES || total > MATERIALIZE_CACHE_BYTE_BUDGET {
            if let Some((_, evicted)) = cache.pop_front() {
                total = total.saturating_sub(evicted.png_bytes.len());
            }
        }
    }

    fn materialize_debug(
        &self,
        _id: u32,
        meta: store::AssetMeta,
    ) -> Result<Option<MaterializedAsset>> {
        // rel_path 以 '/' 分隔存储；HDROP 载荷要求真正的绝对 Windows 路径，
        // 否则 IM 收到相对路径会静默丢弃粘贴（真实微信实测：输入框无任何变化）。
        let mut joined = self.root.clone();
        for segment in meta.rel_path.split('/').filter(|s| !s.is_empty()) {
            joined.push(segment);
        }
        let source_path = std::path::absolute(&joined)?;
        // 上框域 = 活动素材（D60）：回收站行（对象已移 trash/）与文件缺失在
        // 此拒绝——旧实现照常产出载荷，HDROP 指向不存在文件，IM 端静默丢弃
        // 整次粘贴（提示「成功」、实际贴了空气）。
        if self.store.is_deleted(&meta.uuid)? {
            return Err(CatalogError::PasteBlocked(format!(
                "素材已在回收站：{}",
                meta.file_name
            )));
        }
        if !source_path.is_file() {
            return Err(CatalogError::PasteBlocked(format!(
                "素材对象文件缺失：{}",
                source_path.display()
            )));
        }
        // 类别判定统一走 media 注册表（综合分析报告「扩展性缺口 #2」）。
        let kind = media::kind_of(&source_path);

        // D41：图片不再预读 png_bytes。内联 PNG 只有在协商落到**末位兜底**时才被
        // 消费——即 source_path 缺失的库外素材；而 v1 素材恒来自库内（source_path
        // 必在，files 永远可承载），预读是 100% 浪费：低配机上缓存（仅 4 条 LRU）
        // 每 miss 一次，就是几十~几百 ms 的 **UI 线程同步读盘**，是「点击素材到
        // 出结果肉眼可见地慢」的主要确定性来源（时快时慢 = 缓存命中与否）。
        // png_bytes 因此恒空，协商自然回落 CF_HDROP（D22 首选路径）。未来引入
        // 库外素材时应改为「协商确认需要 CF_PNG 后惰性读」，不得回到物化期预读。
        let png_bytes = Vec::new();
        let text = if matches!(kind, AssetKind::Text) {
            fs::read_to_string(&source_path)?
        } else {
            String::new()
        };

        Ok(Some(MaterializedAsset {
            kind,
            png_bytes,
            source_path,
            text,
        }))
    }
}

/// 打开真实库目录并同时返回 `FacetIndex` 与素材 resolver。
///
/// `index` 由 meta.db 流式装载；`uuids` 按同样的 `AssetId` 顺序保存，确保
/// 双击拿到的 `AssetId` 与 `Store::get_asset` 的 uuid 地址一致。
pub fn load_real_library(root: &Path) -> Result<(FacetIndex, RealAssetResolver)> {
    let db = root.join("meta.db");
    if !db.is_file() {
        return Err(CatalogError::MissingDatabase(format!(
            "{} 下无 meta.db",
            root.display()
        )));
    }
    let store = Store::open(&db)?;
    let mut idx = FacetIndex::new();
    let mut uuids = Vec::new();
    let mut category_ids: HashMap<String, CategoryId> = HashMap::new();
    let mut tag_ids: HashMap<String, TagId> = HashMap::new();
    // 名称 → 命中计数（按 id 累加），装配 LibraryFacets 时排序为条目表。
    let mut category_counts: HashMap<String, u32> = HashMap::new();
    let mut tag_counts: HashMap<String, u32> = HashMap::new();
    // D46：回收站清单先行（遍历回调内不可再查 store——连接锁自锁）。
    // 行号必须与 uuids 位置对齐（D52 的 FTS uuid 二分地基），所以回收站行
    // 也占号：走 insert_as_deleted（写行表、不进活集/facet、不计入分面计数）。
    let deleted: std::collections::HashSet<String> = store.deleted_uuids()?.into_iter().collect();
    let mut next_id: u32 = 0;
    store.for_each_asset(|mut meta| {
        let is_deleted = deleted.contains(&meta.uuid);
        let (category, tags) = if is_deleted {
            // 回收站行：只占行号保位置对齐，分面注册表与计数都不碰——
            // 恢复语义走整库重载（与 FacetIndex::unmark_deleted 的 v1 边界一致）。
            (None, Vec::new())
        } else {
            let next_category_id = category_ids.len() as u32;
            let category = meta.category.as_deref().map(|name| {
                *category_counts.entry(name.to_string()).or_insert(0) += 1;
                *category_ids
                    .entry(name.to_string())
                    .or_insert_with(|| CategoryId(next_category_id))
            });
            let next_tag_id = tag_ids.len() as u32;
            let tags = meta
                .tags
                .iter()
                .map(|name| {
                    *tag_counts.entry(name.clone()).or_insert(0) += 1;
                    *tag_ids
                        .entry(name.clone())
                        .or_insert_with(|| TagId(next_tag_id))
                })
                .collect();
            (category, tags)
        };
        let kind = media::kind_of(std::path::Path::new(&meta.rel_path));
        let created_at = meta.created_at;
        let size_bytes = std::mem::take(&mut meta.size_bytes);
        let asset = Asset {
            id: AssetId(next_id),
            name: std::mem::take(&mut meta.file_name),
            category,
            tags,
            created_at,
            size_bytes: Some(size_bytes as u64),
            kind,
        };
        if is_deleted {
            idx.insert_as_deleted(&asset);
        } else {
            idx.insert(&asset);
        }
        uuids.push(meta.uuid);
        next_id += 1;
    })?;

    let facets = assemble_facets(category_ids, tag_ids, &category_counts, &tag_counts);

    Ok((
        idx,
        RealAssetResolver {
            root: root.to_path_buf(),
            store,
            uuids,
            facets,
            cache: RefCell::new(VecDeque::new()),
        },
    ))
}

/// 把 `名称→id` 映射与 `名称→计数` 汇成按名升序的条目表 + 反查映射。
fn assemble_facets(
    category_ids: HashMap<String, CategoryId>,
    tag_ids: HashMap<String, TagId>,
    category_counts: &HashMap<String, u32>,
    tag_counts: &HashMap<String, u32>,
) -> LibraryFacets {
    let mut category_by_name = HashMap::new();
    let mut categories: Vec<FacetEntry> = category_ids
        .into_iter()
        .map(|(name, id)| {
            category_by_name.insert(name.clone(), id);
            FacetEntry {
                id: id.0,
                count: category_counts.get(&name).copied().unwrap_or(0),
                name,
            }
        })
        .collect();
    categories.sort_by(|a, b| a.name.cmp(&b.name));

    let mut tags: Vec<FacetEntry> = tag_ids
        .into_iter()
        .map(|(name, id)| FacetEntry {
            id: id.0,
            count: tag_counts.get(&name).copied().unwrap_or(0),
            name,
        })
        .collect();
    tags.sort_by(|a, b| a.name.cmp(&b.name));

    LibraryFacets {
        categories,
        tags,
        category_by_name,
    }
}

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
        // 字符串分类/tags → id 由本库注册表装配（load_real_library 走完整路径）；此处非空值保守丢弃而非
        // 压缩到同一 id（那会伪造分桶语义）。内存守卫对象是行数与驻留结构。
        idx.insert(&Asset {
            id: AssetId(next_id),
            name: meta.file_name,
            category: None,
            tags: vec![],
            created_at: meta.created_at,
            size_bytes: Some(meta.size_bytes as u64),
            kind: media::kind_of(std::path::Path::new(&meta.rel_path)),
        });
        next_id += 1;
    })?;
    Ok(idx)
}
