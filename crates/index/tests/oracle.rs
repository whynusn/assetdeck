use domain::{Asset, AssetId, AssetKind, CategoryId, TagId};
use index::FacetIndex;
use proptest::prelude::*;

fn arb_assets() -> impl Strategy<Value = Vec<Asset>> {
    prop::collection::vec(
        (
            prop::option::of(0u32..5),
            "[a-z]{1,6}",
            prop::collection::vec(0u32..8, 0..4),
            0i64..1_000_000,
        ),
        0..100,
    )
    .prop_map(|raw| {
        raw.into_iter()
            .enumerate()
            .map(|(i, (cat, name, tags, ts))| Asset {
                id: AssetId(i as u32),
                name,
                category: cat.map(CategoryId),
                tags: {
                    let mut seen = std::collections::HashSet::new();
                    tags.into_iter()
                        .filter(|t| seen.insert(*t))
                        .map(TagId)
                        .collect()
                },
                created_at: ts,
                size_bytes: None,
                kind: AssetKind::Other,
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn facet_counts_match_bruteforce_oracle(assets in arb_assets()) {
        let mut index = FacetIndex::new();
        for a in &assets {
            index.insert(a);
        }

        let mut oracle: std::collections::HashMap<TagId, u64> = std::collections::HashMap::new();
        for a in &assets {
            for t in &a.tags {
                *oracle.entry(*t).or_insert(0) += 1;
            }
        }
        oracle.retain(|_, v| *v > 0);

        let actual = index.tag_counts();
        prop_assert_eq!(oracle, actual);
    }
}
