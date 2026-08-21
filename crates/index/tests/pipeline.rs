use domain::{
    Asset, AssetId, CategoryId, Filter, SortDirection, SortField, SortSpec, Sorter, TagId,
};
use index::FacetIndex;

const CAT_PHOTO: CategoryId = CategoryId(1);
const CAT_VIDEO: CategoryId = CategoryId(2);
const TAG_RED: TagId = TagId(10);
const TAG_BLUE: TagId = TagId(11);
const TAG_PROMO: TagId = TagId(12);

fn sample_assets() -> Vec<Asset> {
    vec![
        Asset {
            id: AssetId(1),
            name: "a".into(),
            category: Some(CAT_PHOTO),
            tags: vec![TAG_RED],
            created_at: 300,
        },
        Asset {
            id: AssetId(2),
            name: "b".into(),
            category: Some(CAT_PHOTO),
            tags: vec![TAG_RED, TAG_BLUE],
            created_at: 200,
        },
        Asset {
            id: AssetId(3),
            name: "c".into(),
            category: Some(CAT_VIDEO),
            tags: vec![TAG_BLUE],
            created_at: 100,
        },
        Asset {
            id: AssetId(4),
            name: "d".into(),
            category: Some(CAT_VIDEO),
            tags: vec![],
            created_at: 400,
        },
        Asset {
            id: AssetId(5),
            name: "e".into(),
            category: None,
            tags: vec![TAG_PROMO],
            created_at: 150,
        },
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
fn facet_count_cache_invalidates_on_tag_mutation() {
    let mut index = idx();
    let counts = index.tag_counts();
    assert_eq!(counts.get(&TAG_RED), Some(&2));
    assert_eq!(counts.get(&TAG_BLUE), Some(&2));
    assert_eq!(counts.get(&TAG_PROMO), Some(&1));

    let updated = Asset {
        id: AssetId(1),
        name: "a".into(),
        category: Some(CAT_PHOTO),
        tags: vec![TAG_BLUE],
        created_at: 300,
    };
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
        .map(|id| index.asset(id).unwrap().clone())
        .collect();
    by_name.sort_assets(&mut items_a);
    let mut items_b: Vec<Asset> = candidates
        .iter()
        .map(|id| index.asset(id).unwrap().clone())
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
