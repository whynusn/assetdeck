use criterion::{black_box, criterion_group, criterion_main, Criterion};
use domain::{Asset, AssetId, CategoryId, Filter};
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
        });
    }
    idx
}

fn bench_evaluate(c: &mut Criterion) {
    let idx = synthetic_index(1_000_000);
    let all = Filter::All;
    let two_cat = Filter::AllOf(vec![
        Filter::InCategory(CategoryId(3)),
        Filter::InCategory(CategoryId(97)),
    ]);
    let single_cat = Filter::InCategory(CategoryId(42));

    c.bench_function("evaluate/all_1m", |b| {
        b.iter(|| idx.evaluate(black_box(&all)))
    });
    c.bench_function("evaluate/two_cat_intersect_1m", |b| {
        b.iter(|| idx.evaluate(black_box(&two_cat)))
    });
    c.bench_function("evaluate/single_category_1m", |b| {
        b.iter(|| idx.evaluate(black_box(&single_cat)))
    });
}

criterion_group!(benches, bench_evaluate);
criterion_main!(benches);
