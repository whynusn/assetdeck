//! RoaringBitmap 分面索引：Filter 求值、facet 计数缓存与失效。
//!
//! 百万级内存模型（综合分析报告「四.3」）：Asset 不再逐条持有堆分配
//! （String + Vec<TagId>），改为 SoA 紧凑行表——AssetId 直接作数组下标，
//! 标签成员关系只存在于 RoaringBitmap。100 万行固定开销约 37MB + 文件名字节，
//! 远低于 `HashMap<u32, Asset>` 的逐条堆分配。
//!
//! 已知边界：remove 只退位图成员关系、行数据留孔（v1 删除路径是整库重建，
//! 重载后自动回收）；`asset()` 为兼容视图重建（不含 tags，成员关系唯一真相在位图）。

use std::cmp::Ordering;
use std::collections::HashMap;

use domain::{Asset, AssetId, AssetKind, CategoryId, Filter, SortField, Sorter, TagId};
use roaring::RoaringBitmap;

/// 内存分面索引。id 为紧凑 u32（AssetId），与持久层标识的映射由 store 层负责。
#[derive(Default)]
pub struct FacetIndex {
    /// SoA 行表：下标即 AssetId。
    names: Vec<Box<str>>,
    categories: Vec<Option<u32>>,
    created_at: Vec<i64>,
    sizes: Vec<Option<u64>>,
    kinds: Vec<AssetKind>,
    by_category: HashMap<u32, RoaringBitmap>,
    by_tag: HashMap<u32, RoaringBitmap>,
    /// 活集合：contains(id) 恒等于「id 对应行可被查询」（remove 只退位图，行留孔）。
    all: RoaringBitmap,
    /// D46 回收站 tombstone：与 `all` 互斥的行集。mark 后行数据保留（属性
    /// 面板/恢复要读），evaluate/len/name 等一切查询路径视同不存在。
    deleted: RoaringBitmap,
    tag_counts_cache: Option<HashMap<TagId, u64>>,
}

impl FacetIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入或覆盖（upsert 语义）：同 id 旧记录的所有 facet 成员关系先被撤销。
    /// 覆盖写会把被 D46 标删的行重新复活——「重写即活行」是 store 侧 upsert
    /// 的既有语义，索引必须一致，否则删后又改的素材会隐身。
    /// 例外：[`insert_as_deleted`] 以「装载期回收站回填」为目的，保留标删态。
    pub fn insert(&mut self, asset: &Asset) {
        self.insert_impl(asset, false);
    }

    /// 装载期专用：按 upsert 语义写行数据，但目标行落点在回收站（不进活集、
    /// 不进 facet 位图、不占标签计数缓存）。`load_real_library` 用它保持
    /// 「行号 = uuids 下标」的位置对齐契约不破（D52 二分映射的地基）。
    pub fn insert_as_deleted(&mut self, asset: &Asset) {
        self.insert_impl(asset, true);
    }

    fn insert_impl(&mut self, asset: &Asset, deleted: bool) {
        let id = asset.id.0;
        let row = id as usize;
        if deleted {
            // 标删行先彻底退位（可能曾是活行被覆盖写），再只写行表。
            self.all.remove(id);
            for bitmap in self.by_category.values_mut() {
                bitmap.remove(id);
            }
            for bitmap in self.by_tag.values_mut() {
                bitmap.remove(id);
            }
            self.deleted.insert(id);
        } else {
            self.deleted.remove(id);
        }
        if deleted {
            // 标删行只写行表：不进活集、不进 facet 位图。
            self.ensure_row(row);
            self.names[row] = asset.name.clone().into_boxed_str();
            self.categories[row] = asset.category.map(|c| c.0);
            self.created_at[row] = asset.created_at;
            self.sizes[row] = asset.size_bytes;
            self.kinds[row] = asset.kind;
            return;
        }
        if self.all.contains(id) {
            // 覆盖写既有行：先撤销旧的 facet 成员关系。
            for bitmap in self.by_category.values_mut() {
                bitmap.remove(id);
            }
            for bitmap in self.by_tag.values_mut() {
                bitmap.remove(id);
            }
        } else {
            self.all.insert(id);
        }
        // 行数据（SoA）：新行先扩表，覆盖写则行已存在。
        self.ensure_row(row);
        self.names[row] = asset.name.clone().into_boxed_str();
        self.categories[row] = asset.category.map(|c| c.0);
        self.created_at[row] = asset.created_at;
        self.sizes[row] = asset.size_bytes;
        self.kinds[row] = asset.kind;
        if let Some(cat) = asset.category {
            self.by_category.entry(cat.0).or_default().insert(id);
        }
        for tag in &asset.tags {
            self.by_tag.entry(tag.0).or_default().insert(id);
        }
        self.tag_counts_cache = None;
    }

    pub fn remove(&mut self, id: AssetId) {
        self.deleted.remove(id.0);
        if !self.all.remove(id.0) {
            return;
        }
        for bitmap in self.by_category.values_mut() {
            bitmap.remove(id.0);
        }
        for bitmap in self.by_tag.values_mut() {
            bitmap.remove(id.0);
        }
        self.tag_counts_cache = None;
    }

    /// D46：把行标进回收站——从活集与 facet 位图摘除，但**保留行数据**
    /// （属性面板与恢复路径要读；恢复后类目直接按行表回填）。返回是否发生
    /// 迁移（已删/不存在的行返回 false，幂等）。
    ///
    /// 已知 v1 边界：tag 成员关系不回填（标签真相在 store，恢复后走一次
    /// 库重载即一致）；类目成员在 unmark 时从行表恢复。
    pub fn mark_deleted(&mut self, id: AssetId) -> bool {
        if !self.all.remove(id.0) {
            return false;
        }
        for bitmap in self.by_category.values_mut() {
            bitmap.remove(id.0);
        }
        for bitmap in self.by_tag.values_mut() {
            bitmap.remove(id.0);
        }
        self.deleted.insert(id.0);
        self.tag_counts_cache = None;
        true
    }

    /// 恢复出回收站：回到活集，类目成员按行表回填；tags 不回填（见上边界）。
    /// 返回是否发生迁移（非回收站行返回 false，幂等）。
    pub fn unmark_deleted(&mut self, id: AssetId) -> bool {
        if !self.deleted.remove(id.0) {
            return false;
        }
        self.all.insert(id.0);
        if let Some(Some(cat)) = self.categories.get(id.0 as usize).copied() {
            self.by_category.entry(cat).or_default().insert(id.0);
        }
        self.tag_counts_cache = None;
        true
    }

    /// 行是否处于回收站（区分「留孔的 remove」与「软删」两种不可见）。
    pub fn is_deleted(&self, id: AssetId) -> bool {
        self.deleted.contains(id.0)
    }

    pub fn len(&self) -> u64 {
        self.all.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    /// 兼容视图重建：返回自持的 Asset 副本（不含 tags）。活行与回收站行都可
    /// 读（后者供回收站视图）。仅供测试/演示路径使用；热路径请用
    /// [`Self::name`] / [`Self::kind`] 等窄接口。
    pub fn asset(&self, id: u32) -> Option<Asset> {
        if !self.visible_row(id) {
            return None;
        }
        let row = id as usize;
        Some(Asset {
            id: AssetId(id),
            name: self
                .names
                .get(row)
                .map(|s| s.to_string())
                .unwrap_or_default(),
            category: self.categories.get(row).copied().flatten().map(CategoryId),
            tags: Vec::new(),
            created_at: self.created_at.get(row).copied().unwrap_or(0),
            size_bytes: self.sizes.get(row).copied().flatten(),
            kind: self.kind(id),
        })
    }

    /// 行数据可见性判定：活行与回收站行都**可**读——回收站视图与属性面板
    /// 要显示它们；「查询不可见」由各过滤器求值走 `all`/facet 位图保证。
    fn visible_row(&self, id: u32) -> bool {
        self.all.contains(id) || self.deleted.contains(id)
    }

    /// 活集合内某资产的文件名（渲染热路径，零分配借引用）；回收站行同样可读。
    pub fn name(&self, id: u32) -> Option<&str> {
        if !self.visible_row(id) {
            return None;
        }
        self.names.get(id as usize).map(|s| s.as_ref())
    }

    /// 行数据所属类别（活行与回收站行都可读）；未知行回落 Other。
    pub fn kind(&self, id: u32) -> AssetKind {
        if !self.visible_row(id) {
            return AssetKind::Other;
        }
        self.kinds
            .get(id as usize)
            .copied()
            .unwrap_or(AssetKind::Other)
    }

    /// 文件名子串匹配（大小写不敏感）命中的 id 集合。
    ///
    /// v1 实现为内存线性扫描（百万级每键一次可接受；≥3 字符查全量走 Store FTS5）；
    /// 索引层只回答集合，不参与搜索策略编排（策略在 SearchProvider）。
    pub fn search_names(&self, needle: &str) -> RoaringBitmap {
        let needle = needle.trim().to_lowercase();
        let mut hits = RoaringBitmap::new();
        if needle.is_empty() {
            return hits;
        }
        for (index, name) in self.names.iter().enumerate() {
            if self.all.contains(index as u32) && name.to_lowercase().contains(&needle) {
                hits.insert(index as u32);
            }
        }
        hits
    }

    /// 全集位图（`Filter::All` 的求值结果，免克隆引用版）。
    pub fn all_ids(&self) -> &RoaringBitmap {
        &self.all
    }

    /// 对过滤器树做位图求值：组合谓词 → 交集/并集/补集。
    pub fn evaluate(&self, filter: &Filter) -> RoaringBitmap {
        match filter {
            Filter::All => self.all.clone(),
            Filter::InCategory(cat) => self.by_category.get(&cat.0).cloned().unwrap_or_default(),
            Filter::HasTag(tag) => self.by_tag.get(&tag.0).cloned().unwrap_or_default(),
            Filter::NameContains(needle) => self.search_names(needle),
            Filter::Not(inner) => &self.all - &self.evaluate(inner),
            Filter::AllOf(filters) => {
                let mut acc: Option<RoaringBitmap> = None;
                for f in filters {
                    let cur = self.evaluate(f);
                    acc = Some(match acc {
                        Some(a) => &a & &cur,
                        None => cur,
                    });
                }
                acc.unwrap_or_else(|| self.all.clone())
            }
            Filter::AnyOf(filters) => {
                let mut acc = RoaringBitmap::new();
                for f in filters {
                    acc |= &self.evaluate(f);
                }
                acc
            }
        }
    }

    /// 对候选集按 sorter 多键稳定排序，返回有序 id 序列（SoA 直排，不物化 Asset）。
    pub fn sorted_ids(&self, sorter: &Sorter, base: &RoaringBitmap) -> Vec<u32> {
        let mut ids: Vec<u32> = base.iter().collect();
        if sorter.keys.is_empty() {
            return ids;
        }
        ids.sort_by(|&a, &b| self.compare_keys(sorter, a, b));
        ids
    }

    fn compare_keys(&self, sorter: &Sorter, a: u32, b: u32) -> Ordering {
        for key in &sorter.keys {
            let raw = match key.field {
                SortField::CreatedAt => self
                    .created_at
                    .get(a as usize)
                    .copied()
                    .unwrap_or(0)
                    .cmp(&self.created_at.get(b as usize).copied().unwrap_or(0)),
                SortField::Name => self.name(a).unwrap_or("").cmp(self.name(b).unwrap_or("")),
                // 未知尺寸恒排后，不随方向翻转（语义与 domain::Sorter 一致）。
                SortField::Size => match (
                    self.sizes.get(a as usize).copied().flatten(),
                    self.sizes.get(b as usize).copied().flatten(),
                ) {
                    (Some(x), Some(y)) => match key.direction {
                        domain::SortDirection::Asc => x.cmp(&y),
                        domain::SortDirection::Desc => y.cmp(&x),
                    },
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                },
                SortField::Kind => self.kind(a).cmp(&self.kind(b)),
            };
            let ord = match key.direction {
                domain::SortDirection::Asc => raw,
                domain::SortDirection::Desc => raw.reverse(),
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    }

    /// 各标签的资产计数（带缓存；任何 insert/remove 自动失效）。
    pub fn tag_counts(&mut self) -> HashMap<TagId, u64> {
        if let Some(cached) = &self.tag_counts_cache {
            return cached.clone();
        }
        let counts: HashMap<TagId, u64> = self
            .by_tag
            .iter()
            .filter(|(_, bm)| !bm.is_empty())
            .map(|(tag, bm)| (TagId(*tag), bm.len()))
            .collect();
        self.tag_counts_cache = Some(counts.clone());
        counts
    }

    /// 把行表扩展到能容纳 (id+1) 行（默认值填充，便于任意顺序 insert）。
    fn ensure_row(&mut self, row: usize) {
        let grow = |v: &mut Vec<Box<str>>, n: usize| {
            while v.len() <= n {
                v.push(String::new().into_boxed_str());
            }
        };
        grow(&mut self.names, row);
        while self.categories.len() <= row {
            self.categories.push(None);
        }
        while self.created_at.len() <= row {
            self.created_at.push(0);
        }
        while self.sizes.len() <= row {
            self.sizes.push(None);
        }
        while self.kinds.len() <= row {
            self.kinds.push(AssetKind::Other);
        }
    }
}
