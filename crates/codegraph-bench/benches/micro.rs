use std::hint::black_box;

use codegraph_bench::metrics::{mad, median, percentile};
use criterion::{Criterion, criterion_group, criterion_main};

fn stats_functions(c: &mut Criterion) {
    let samples: Vec<f64> = (0..512)
        .map(|index| ((index * 37) % 101) as f64 + index as f64 / 1000.0)
        .collect();

    c.bench_function("stats_median_mad_percentile", |bench| {
        bench.iter(|| {
            let values = black_box(&samples);
            black_box((median(values), mad(values), percentile(values, 99.0)))
        });
    });
}

criterion_group!(benches, stats_functions);
criterion_main!(benches);
