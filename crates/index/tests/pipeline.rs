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
