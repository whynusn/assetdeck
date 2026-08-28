//! 实体与查询模型：Asset、Category、Filter、Sorter（纯数据与纯函数，零 IO）。
//!
//! 扩展点契约（综合分析报告）：封闭世界枚举（AssetKind / SortField / Filter 变体）
//! 在此集中定义并用 enum + match 处理；开放世界能力（导入格式 / 分类规则 / 搜索）
//! 由 media（注册表）、library（trait）、ui-viewmodels（trait）等层承载。

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssetId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CategoryId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TagId(pub u32);

/// 素材类别（管线 / 索引 / 卡片渲染共享的最小分类，四点收敛到这一处）。
///
/// 枚举序即排序语义（Kind 排序按此升序）：Image < Video < Text < Other。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AssetKind {
    Image,
    Video,
    Text,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub name: String,
    pub category: Option<CategoryId>,
    pub tags: Vec<TagId>,
    pub created_at: i64,
    /// 文件字节数（导入时得知；未知为 None）。排序维度之一（P2 排序器扩展）。
    pub size_bytes: Option<u64>,
    /// 素材类别（未知/合成数据为 Other）。卡片渲染与 Kind 排序依赖。
    pub kind: AssetKind,
}

/// 过滤器：组合谓词树。求值在 index 层（位图运算），此处仅承载结构。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    All,
    InCategory(CategoryId),
    HasTag(TagId),
    /// 文件名子串匹配（大小写不敏感）。v1 由 index 内存扫描实现；
    /// ≥3 字符的查全量检索走 Store FTS5（SearchProvider 统一入口）。
    NameContains(String),
    /// D52 混合路由：FTS5 命中经 uuid→行号二分映射出的 id 白名单（瞬时构造，
    /// 不携带位图外物——D4 候选集抽象纪律）。求值 = 与活集求交：FTS 行不随
    /// 软删移除（D46），传入已删/未知 id 被静默剔除。
    NameIn(Vec<u32>),
    /// D46 回收站视图：求值结果 = deleted 位图（与活集互斥，D47 语义下
    /// 浏览/计数/搜索全走活集，只有显式切到此过滤器才见回收站行）。
    Trash,
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
    /// 文件字节数（未知尺寸恒排后，不区分方向）。
    Size,
    /// 素材类别（Image < Video < Text < Other）。
    Kind,
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
                let ord = match key.field {
                    SortField::CreatedAt => {
                        apply_dir(key.direction, a.created_at.cmp(&b.created_at))
                    }
                    SortField::Name => apply_dir(key.direction, a.name.cmp(&b.name)),
                    SortField::Size => sort_size(a.size_bytes, b.size_bytes, key.direction),
                    SortField::Kind => apply_dir(key.direction, a.kind.cmp(&b.kind)),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            Ordering::Equal
        });
    }
}

/// 方向仅作用于「两者都有值」的比较；未知（None）尺寸恒排已知之后，
/// 不随方向翻转（不会把未知顶到最前，两种方向都保持「未知垫底」）。
fn apply_dir(direction: SortDirection, ord: Ordering) -> Ordering {
    match direction {
        SortDirection::Asc => ord,
        SortDirection::Desc => ord.reverse(),
    }
}

fn sort_size(a: Option<u64>, b: Option<u64>, direction: SortDirection) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => apply_dir(direction, x.cmp(&y)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
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
            size_bytes: None,
            kind: AssetKind::Other,
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
    fn sorter_by_size_puts_unknown_last_in_both_directions() {
        let mut items = vec![asset(1, "a", 0), asset(2, "b", 0), asset(3, "c", 0)];
        items[0].size_bytes = Some(500);
        items[1].size_bytes = Some(1000);
        // items[2] 尺寸未知
        let asc = Sorter {
            keys: vec![SortSpec {
                field: SortField::Size,
                direction: SortDirection::Asc,
            }],
        };
        asc.sort_assets(&mut items);
        let ids: Vec<u32> = items.iter().map(|a| a.id.0).collect();
        assert_eq!(ids, vec![1, 2, 3]); // 未知恒在最后
        let desc = Sorter {
            keys: vec![SortSpec {
                field: SortField::Size,
                direction: SortDirection::Desc,
            }],
        };
        desc.sort_assets(&mut items);
        let ids: Vec<u32> = items.iter().map(|a| a.id.0).collect();
        assert_eq!(ids, vec![2, 1, 3]); // 逆序后未知仍不顶到最前
    }

    #[test]
    fn sorter_by_kind_orders_image_video_text_other() {
        let mut items = vec![
            asset(1, "x", 0),
            asset(2, "y", 0),
            asset(3, "z", 0),
            asset(4, "w", 0),
        ];
        items[0].kind = AssetKind::Text;
        items[1].kind = AssetKind::Video;
        items[2].kind = AssetKind::Image;
        items[3].kind = AssetKind::Other;
        let sorter = Sorter {
            keys: vec![SortSpec {
                field: SortField::Kind,
                direction: SortDirection::Asc,
            }],
        };
        sorter.sort_assets(&mut items);
        let kinds: Vec<AssetKind> = items.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            vec![
                AssetKind::Image,
                AssetKind::Video,
                AssetKind::Text,
                AssetKind::Other
            ]
        );
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

    #[test]
    fn filter_name_contains_serde_roundtrip() {
        let f = Filter::NameContains("截图".to_string());
        let json = serde_json::to_string(&f).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }
}

/// D52 共享的文本折叠工具：needle 一次折叠，haystack 流式比对（零逐行分配）。
///
/// 折叠语义 = `char::to_lowercase`（Unicode 表）：ASCII 快慢无所谓——正确性
/// 先行；中文无大小写，折叠恒等。双侧同一函数保证 İ/i̇ 这类展开对称。
pub mod text {
    /// 大小写折叠子串匹配：haystack 逐字符折叠流过定长环形窗口。
    /// `ring` 由调用方分配一次、跨行复用（百万行扫描的关键：零逐行分配）。
    pub fn contains_case_fold(haystack: &str, needle: &[char], ring: &mut Vec<char>) -> bool {
        if needle.is_empty() {
            return true;
        }
        ring.clear();
        ring.resize(needle.len(), ' ');
        let mut fill = 0usize;
        let mut head = 0usize;
        for ch in haystack.chars().flat_map(char::to_lowercase) {
            ring[head] = ch;
            head = (head + 1) % ring.len();
            if fill < ring.len() {
                fill += 1;
            }
            if fill == ring.len() {
                let mut matched = true;
                for (offset, expected) in needle.iter().enumerate() {
                    if ring[(head + offset) % ring.len()] != *expected {
                        matched = false;
                        break;
                    }
                }
                if matched {
                    return true;
                }
            }
        }
        false
    }

    /// needle 折叠为 char 序列（每次查询一次的分配，与行数无关）。
    pub fn fold_lower(text: &str) -> Vec<char> {
        text.trim().chars().flat_map(char::to_lowercase).collect()
    }
}
