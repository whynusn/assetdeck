//! RoaringBitmap 分面索引：Filter 求值、facet 计数缓存与失效。

use std::collections::HashMap;

use domain::{Asset, AssetId, CategoryId, Filter, TagId};
use roaring::RoaringBitmap;

/// 内存分面索引。id 为紧凑 u32（AssetId），与持久层标识的映射由 store 层负责。
#[derive(Default)]
pub struct FacetIndex {
    assets: HashMap<u32, Asset>,
    by_category: HashMap<CategoryId, RoaringBitmap>,
    by_tag: HashMap<TagId, RoaringBitmap>,
    all: RoaringBitmap,
    tag_counts_cache: Option<HashMap<TagId, u64>>,
}

impl FacetIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入或覆盖（upsert 语义）：同 id 旧记录的所有 facet 成员关系先被撤销。
    pub fn insert(&mut self, asset: &Asset) {
        self.remove(asset.id);
        let id = asset.id.0;
        self.assets.insert(id, asset.clone());
        self.all.insert(id);
        if let Some(cat) = asset.category {
            self.by_category.entry(cat).or_default().insert(id);
        }
        for tag in &asset.tags {
            self.by_tag.entry(*tag).or_default().insert(id);
        }
        self.tag_counts_cache = None;
    }

    pub fn remove(&mut self, id: AssetId) {
        if !self.all.remove(id.0) {
            return;
        }
        self.assets.remove(&id.0);
        for bitmap in self.by_category.values_mut() {
            bitmap.remove(id.0);
        }
        for bitmap in self.by_tag.values_mut() {
            bitmap.remove(id.0);
        }
        self.tag_counts_cache = None;
    }

    pub fn len(&self) -> u64 {
        self.all.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn asset(&self, id: u32) -> Option<&Asset> {
        self.assets.get(&id)
    }

    /// 全集位图（`Filter::All` 的求值结果，免克隆引用版）。
    pub fn all_ids(&self) -> &RoaringBitmap {
        &self.all
    }

    /// 对过滤器树做位图求值：组合谓词 → 交集/并集/补集。
    pub fn evaluate(&self, filter: &Filter) -> RoaringBitmap {
        match filter {
            Filter::All => self.all.clone(),
            Filter::InCategory(cat) => self.by_category.get(cat).cloned().unwrap_or_default(),
            Filter::HasTag(tag) => self.by_tag.get(tag).cloned().unwrap_or_default(),
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

    /// 各标签的资产计数（带缓存；任何 insert/remove 自动失效）。
    pub fn tag_counts(&mut self) -> HashMap<TagId, u64> {
        if let Some(cached) = &self.tag_counts_cache {
            return cached.clone();
        }
        let counts: HashMap<TagId, u64> = self
            .by_tag
            .iter()
            .filter(|(_, bm)| !bm.is_empty())
            .map(|(tag, bm)| (*tag, bm.len()))
            .collect();
        self.tag_counts_cache = Some(counts.clone());
        counts
    }
}
