//! 实体与查询模型：Asset、Category、Filter、Sorter（纯数据与纯函数，零 IO）。

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssetId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CategoryId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TagId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub name: String,
    pub category: Option<CategoryId>,
    pub tags: Vec<TagId>,
    pub created_at: i64,
}

/// 过滤器：组合谓词树。求值在 index 层（位图运算），此处仅承载结构。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    All,
    InCategory(CategoryId),
    HasTag(TagId),
    Not(Box<Filter>),
    AllOf(Vec<Filter>),
    AnyOf(Vec<Filter>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortField {
    CreatedAt,
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortSpec {
    pub field: SortField,
    pub direction: SortDirection,
}

/// 排序器：与过滤器解耦的多键稳定排序规格。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Sorter {
    pub keys: Vec<SortSpec>,
}

impl Sorter {
    /// 多键稳定排序：所有键相等的元素保持原有相对顺序。
    pub fn sort_assets(&self, assets: &mut [Asset]) {
        assets.sort_by(|a, b| {
            for key in &self.keys {
                let raw = match key.field {
                    SortField::CreatedAt => a.created_at.cmp(&b.created_at),
                    SortField::Name => a.name.cmp(&b.name),
                };
                let ord = match key.direction {
                    SortDirection::Asc => raw,
                    SortDirection::Desc => raw.reverse(),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            Ordering::Equal
        });
    }
}

/// 智能文件夹：序列化的 (filter, sorter) 二元组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartFolder {
    pub name: String,
    pub filter: Filter,
    pub sorter: Sorter,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(id: u32, name: &str, created_at: i64) -> Asset {
        Asset {
            id: AssetId(id),
            name: name.to_string(),
            category: None,
            tags: vec![],
            created_at,
        }
    }

    #[test]
    fn sorter_recency_then_name_is_stable_multisort() {
        let mut items = vec![
            asset(1, "beta", 100),
            asset(2, "dup", 100),
            asset(3, "alpha", 100),
            asset(4, "zeta", 50),
            asset(5, "dup", 100),
            asset(6, "yak", 50),
        ];
        let sorter = Sorter {
            keys: vec![
                SortSpec {
                    field: SortField::CreatedAt,
                    direction: SortDirection::Desc,
                },
                SortSpec {
                    field: SortField::Name,
                    direction: SortDirection::Asc,
                },
            ],
        };
        sorter.sort_assets(&mut items);
        let ids: Vec<u32> = items.iter().map(|a| a.id.0).collect();
        assert_eq!(ids, vec![3, 1, 2, 5, 6, 4]);
    }

    #[test]
    fn smart_folder_serde_roundtrip_preserves_filter_sorter() {
        let folder = SmartFolder {
            name: "近期促销图".to_string(),
            filter: Filter::AllOf(vec![
                Filter::InCategory(CategoryId(7)),
                Filter::Not(Box::new(Filter::HasTag(TagId(2)))),
            ]),
            sorter: Sorter {
                keys: vec![SortSpec {
                    field: SortField::Name,
                    direction: SortDirection::Asc,
                }],
            },
        };
        let json = serde_json::to_string(&folder).unwrap();
        let back: SmartFolder = serde_json::from_str(&json).unwrap();
        assert_eq!(back, folder);
    }
}
