//! 布局数学基准：@10k 混合宽高比（implement.md 门 1 的回退预案判定点）。

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ui_viewmodels::masonry_layout;

fn bench_masonry_layout(c: &mut Criterion) {
    // 与内存守卫测试同一确定性生成式，保证基准与测试输入同分布
    let aspects: Vec<f32> = (0..10_000u32)
        .map(|i| (i % 7 + 1) as f32 / ((i % 5) as f32 + 1.0))
        .collect();
    c.bench_function("masonry_layout/10k_aspects", |b| {
        b.iter(|| {
            masonry_layout(
                black_box(1200.0),
                black_box(6),
                black_box(8.0),
                black_box(&aspects),
            )
        })
    });
}

criterion_group!(benches, bench_masonry_layout);
criterion_main!(benches);
