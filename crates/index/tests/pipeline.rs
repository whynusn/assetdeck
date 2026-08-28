use domain::{
    Asset, AssetId, AssetKind, CategoryId, Filter, SortDirection, SortField, SortSpec, Sorter,
    TagId,
};
use index::FacetIndex;

const CAT_PHOTO: CategoryId = CategoryId(1);
const CAT_VIDEO: CategoryId = CategoryId(2);
const TAG_RED: TagId = TagId(10);
const TAG_BLUE: TagId = TagId(11);
const TAG_PROMO: TagId = TagId(12);

fn asset_of(
    id: u32,
    name: &str,
    category: Option<CategoryId>,
    tags: Vec<TagId>,
    created_at: i64,
) -> Asset {
    Asset {
        id: AssetId(id),
        name: name.into(),
        category,
        tags,
        created_at,
        size_bytes: None,
        kind: AssetKind::Other,
    }
}

fn sample_assets() -> Vec<Asset> {
    vec![
        asset_of(1, "a", Some(CAT_PHOTO), vec![TAG_RED], 300),
        asset_of(2, "b", Some(CAT_PHOTO), vec![TAG_RED, TAG_BLUE], 200),
        asset_of(3, "c", Some(CAT_VIDEO), vec![TAG_BLUE], 100),
        asset_of(4, "d", Some(CAT_VIDEO), vec![], 400),
        asset_of(5, "e", None, vec![TAG_PROMO], 150),
    ]
}

fn idx() -> FacetIndex {
    let mut index = FacetIndex::new();
    for asset in sample_assets() {
        index.insert(&asset);
    }
    index
}

fn ids(bm: &roaring::RoaringBitmap) -> Vec<u32> {
    let mut v: Vec<u32> = bm.iter().collect();
    v.sort_unstable();
    v
}

#[test]
fn filter_by_single_category_returns_matching_ids() {
    let index = idx();
    assert_eq!(
        ids(&index.evaluate(&Filter::InCategory(CAT_PHOTO))),
        vec![1, 2]
    );
    assert_eq!(
        ids(&index.evaluate(&Filter::InCategory(CAT_VIDEO))),
        vec![3, 4]
    );
    assert!(index
        .evaluate(&Filter::InCategory(CategoryId(99)))
        .is_empty());
}

#[test]
fn intersect_two_facets_returns_conjunction() {
    let index = idx();
    let both_tags = Filter::AllOf(vec![Filter::HasTag(TAG_RED), Filter::HasTag(TAG_BLUE)]);
    assert_eq!(ids(&index.evaluate(&both_tags)), vec![2]);

    let cat_and_tag = Filter::AllOf(vec![
        Filter::InCategory(CAT_VIDEO),
        Filter::HasTag(TAG_BLUE),
    ]);
    assert_eq!(ids(&index.evaluate(&cat_and_tag)), vec![3]);

    let either_tag = Filter::AnyOf(vec![Filter::HasTag(TAG_RED), Filter::HasTag(TAG_PROMO)]);
    assert_eq!(ids(&index.evaluate(&either_tag)), vec![1, 2, 5]);
}

#[test]
fn negated_filter_excludes_ids() {
    let index = idx();
    let not_photo = Filter::Not(Box::new(Filter::InCategory(CAT_PHOTO)));
    assert_eq!(ids(&index.evaluate(&not_photo)), vec![3, 4, 5]);
    assert!(index
        .evaluate(&Filter::Not(Box::new(Filter::All)))
        .is_empty());
}

#[test]
fn name_contains_filter_matches_case_insensitive_substring() {
    let mut index = idx();
    // 补一个真实文件名字样的行，验证子串 + 大小写不敏感。
    index.insert(&asset_of(
        9,
        "暑期促销图-1.PNG",
        Some(CAT_PHOTO),
        vec![],
        900,
    ));
    let hits = index.evaluate(&Filter::NameContains("促销".to_string()));
    assert_eq!(ids(&hits), vec![9]);
    let hits_upper = index.evaluate(&Filter::NameContains("PNG".to_string()));
    assert_eq!(ids(&hits_upper), vec![9]);
    // 空查询不匹配任何行（调用方应回落当前视图）。
    assert!(index
        .evaluate(&Filter::NameContains("  ".to_string()))
        .is_empty());
}

#[test]
fn sorted_ids_orders_by_multiple_keys_without_materializing_assets() {
    let index = idx();
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
    let base = index.evaluate(&Filter::All);
    let ordered = index.sorted_ids(&sorter, &base);
    // created_at 降序：4(400), 1(300), 2(200), 5(150), 3(100)
    assert_eq!(ordered, vec![4, 1, 2, 5, 3]);
}

#[test]
fn facet_count_cache_invalidates_on_tag_mutation() {
    let mut index = idx();
    let counts = index.tag_counts();
    assert_eq!(counts.get(&TAG_RED), Some(&2));
    assert_eq!(counts.get(&TAG_BLUE), Some(&2));
    assert_eq!(counts.get(&TAG_PROMO), Some(&1));

    let updated = asset_of(1, "a", Some(CAT_PHOTO), vec![TAG_BLUE], 300);
    index.insert(&updated);

    let counts = index.tag_counts();
    assert_eq!(counts.get(&TAG_RED), Some(&1));
    assert_eq!(counts.get(&TAG_BLUE), Some(&3));

    index.remove(AssetId(2));
    let counts = index.tag_counts();
    assert_eq!(counts.get(&TAG_BLUE), Some(&2));
}

#[test]
fn sorter_decoupled_from_filter_pipeline_order() {
    let index = idx();
    let filter = Filter::HasTag(TAG_RED);
    let candidates = index.evaluate(&filter);

    let by_name = Sorter {
        keys: vec![SortSpec {
            field: SortField::Name,
            direction: SortDirection::Asc,
        }],
    };
    let by_recency = Sorter {
        keys: vec![SortSpec {
            field: SortField::CreatedAt,
            direction: SortDirection::Desc,
        }],
    };

    let mut items_a: Vec<Asset> = candidates
        .iter()
        .map(|id| index.asset(id).unwrap())
        .collect();
    by_name.sort_assets(&mut items_a);
    let mut items_b: Vec<Asset> = candidates
        .iter()
        .map(|id| index.asset(id).unwrap())
        .collect();
    by_recency.sort_assets(&mut items_b);

    assert_eq!(
        items_a.iter().map(|a| a.id.0).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        items_b.iter().map(|a| a.id.0).collect::<Vec<_>>(),
        vec![1, 2]
    );

    let again = index.evaluate(&filter);
    assert_eq!(again, candidates);
}

#[test]
fn soa_rows_survive_upsert_and_remove_semantics() {
    let mut index = idx();
    // upsert 改 size/kind：双维度读取要反映新值。
    let mut upgraded = asset_of(1, "a", Some(CAT_PHOTO), vec![TAG_RED], 300);
    upgraded.size_bytes = Some(2048);
    upgraded.kind = AssetKind::Image;
    index.insert(&upgraded);
    assert_eq!(index.kind(1), AssetKind::Image);
    assert_eq!(index.asset(1).unwrap().size_bytes, Some(2048));
    assert_eq!(index.name(1), Some("a"));
    // remove 后：asset/name/kind 均不可见（行留孔但活集合不含）。
    index.remove(AssetId(1));
    assert!(index.asset(1).is_none());
    assert!(index.name(1).is_none());
    assert_eq!(index.kind(1), AssetKind::Other);
    assert_eq!(ids(&index.evaluate(&Filter::HasTag(TAG_RED))), vec![2]);
}

// ---------------------------------------------------------------------------
// D46 回收站：tombstone 位图——查询不可见但行数据可读；恢复回填类目成员；
// insert_as_deleted 保持行号位置对齐（装载期契约）。
// ---------------------------------------------------------------------------

#[test]
fn mark_deleted_hides_from_every_query_but_keeps_row() {
    let mut index = idx();
    assert!(index.mark_deleted(AssetId(2)));
    // 一切查询路径不可见：活集 / All / 类目 / 标签 / 名称扫描 / 计数。
    assert_eq!(index.len(), 4);
    assert!(!index.all_ids().contains(2));
    assert_eq!(ids(&index.evaluate(&Filter::All)), vec![1, 3, 4, 5]);
    assert_eq!(
        ids(&index.evaluate(&Filter::InCategory(CAT_PHOTO))),
        vec![1]
    );
    assert_eq!(ids(&index.evaluate(&Filter::HasTag(TAG_RED))), vec![1]);
    assert!(!index
        .evaluate(&Filter::NameContains("b".into()))
        .contains(2));
    // 幂等：已删再删返回 false。
    assert!(!index.mark_deleted(AssetId(2)));
    // 但行数据必须可读（回收站视图/属性面板），且状态可查询。
    assert_eq!(index.name(2), Some("b"));
    assert!(index.asset(2).is_some());
    assert!(index.is_deleted(AssetId(2)));
}

#[test]
fn unmark_restores_category_membership_from_row_table() {
    let mut index = idx();
    index.mark_deleted(AssetId(2));
    assert!(index.unmark_deleted(AssetId(2)));
    assert_eq!(ids(&index.evaluate(&Filter::All)), vec![1, 2, 3, 4, 5]);
    // 类目按行表回填；tags 不回填（v1 边界：恢复后走重载，位图真相在 store）。
    assert_eq!(
        ids(&index.evaluate(&Filter::InCategory(CAT_PHOTO))),
        vec![1, 2]
    );
    assert_eq!(ids(&index.evaluate(&Filter::HasTag(TAG_RED))), vec![1]);
    // 幂等：非回收站行 unmark 返回 false。
    assert!(!index.unmark_deleted(AssetId(2)));
}

#[test]
fn insert_revives_soft_deleted_row() {
    let mut index = idx();
    index.mark_deleted(AssetId(2));
    // 「删后又改」（如移动到分类的 upsert 写回）必须复活行。
    index.insert(&asset_of(2, "b2", Some(CAT_VIDEO), vec![TAG_BLUE], 200));
    assert!(!index.is_deleted(AssetId(2)));
    assert_eq!(
        ids(&index.evaluate(&Filter::InCategory(CAT_VIDEO))),
        vec![2, 3, 4]
    );
}

#[test]
fn insert_as_deleted_keeps_row_alignment_and_skips_facets() {
    let mut index = idx();
    let ghost = asset_of(9, "ghost", Some(CAT_PHOTO), vec![TAG_RED], 999);
    index.insert_as_deleted(&ghost);
    // 行号 9 落点即回收站：活集与 facet 都不含，len 不增。
    assert_eq!(index.len(), 5);
    assert!(!index.evaluate(&Filter::InCategory(CAT_PHOTO)).contains(9));
    assert!(!index.evaluate(&Filter::HasTag(TAG_RED)).contains(9));
    // 但行数据在（位置对齐：下标 9 即该行），可被恢复。
    assert_eq!(index.name(9), Some("ghost"));
    assert!(index.is_deleted(AssetId(9)));
    assert!(index.unmark_deleted(AssetId(9)));
    assert_eq!(
        ids(&index.evaluate(&Filter::InCategory(CAT_PHOTO))),
        vec![1, 2, 9]
    );
}

#[test]
fn remove_clears_tombstone_membership() {
    let mut index = idx();
    index.mark_deleted(AssetId(3));
    // 彻底删除（重载前的 remove）：tombstone 集也不得留成员，否则行孔两栖。
    index.remove(AssetId(3));
    assert!(!index.is_deleted(AssetId(3)));
    assert!(!index.unmark_deleted(AssetId(3)));
}

#[test]
fn trash_filter_evaluates_to_tombstone_set() {
    let mut index = idx();
    index.mark_deleted(AssetId(2));
    index.insert_as_deleted(&asset_of(9, "ghost", Some(CAT_PHOTO), vec![TAG_RED], 999));
    assert_eq!(ids(&index.evaluate(&Filter::Trash)), vec![2, 9]);
    // Not(Trash) = 活集（all 天然不含标删行，补集语义成立）。
    assert_eq!(
        ids(&index.evaluate(&Filter::Not(Box::new(Filter::Trash)))),
        vec![1, 3, 4, 5]
    );
    // 恢复后自动退出回收站视图。
    index.unmark_deleted(AssetId(2));
    assert_eq!(ids(&index.evaluate(&Filter::Trash)), vec![9]);
}
