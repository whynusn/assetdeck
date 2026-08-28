use std::time::Instant;

use domain::{Asset, AssetId, AssetKind, CategoryId, Filter};
use index::FacetIndex;

fn synthetic_index(n: u32) -> FacetIndex {
    let mut idx = FacetIndex::new();
    for i in 0..n {
        idx.insert(&Asset {
            id: AssetId(i),
            name: format!("asset-{i}"),
            category: Some(CategoryId(i % 200)),
            tags: vec![],
            created_at: i as i64,
            size_bytes: Some(i as u64 * 3),
            kind: AssetKind::Image,
        });
    }
    idx
}

fn avg_elapsed_ms(index: &FacetIndex, filter: &Filter, warmup: usize, samples: usize) -> f64 {
    for _ in 0..warmup {
        let _ = black_box_eval(index, filter);
    }
    let start = Instant::now();
    for _ in 0..samples {
        black_box_eval(index, filter);
    }
    start.elapsed().as_secs_f64() * 1000.0 / samples as f64
}

fn black_box_eval(index: &FacetIndex, filter: &Filter) -> roaring::RoaringBitmap {
    let bm = index.evaluate(filter);
    std::hint::black_box(&bm);
    bm
}

#[test]
fn empty_filter_returns_all_within_budget_1ms_at_1m() {
    let idx = synthetic_index(1_000_000);
    let avg = avg_elapsed_ms(&idx, &Filter::All, 10, 50);
    assert!(
        avg < 1.0,
        "Filter::All 求值平均 {avg:.3}ms 超出 1ms 预算（D4）"
    );
}

#[test]
fn two_facet_intersect_under_1ms_at_1m() {
    let idx = synthetic_index(1_000_000);
    let two_cat = Filter::AllOf(vec![
        Filter::InCategory(CategoryId(3)),
        Filter::InCategory(CategoryId(97)),
    ]);
    let avg = avg_elapsed_ms(&idx, &two_cat, 10, 50);
    assert!(
        avg < 1.0,
        "双分类位图交集平均 {avg:.3}ms 超出 1ms 预算（D4 验收线）"
    );
}

#[test]
fn single_category_query_is_sub_millisecond_at_1m() {
    let idx = synthetic_index(1_000_000);
    let avg = avg_elapsed_ms(&idx, &Filter::InCategory(CategoryId(42)), 10, 50);
    assert!(avg < 1.0, "单分类查询平均 {avg:.3}ms 超出 1ms");
}
