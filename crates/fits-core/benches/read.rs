//! Benchmarks for the read path.
//!
//! The target from the plan is a 6000x4000 16-bit image decoded in under
//! 150 ms on a modern laptop, warm cache. Run with:
//!
//! ```text
//! cargo bench --package fits-core --all-features
//! ```

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use fits_core::testutil::{synthetic_fits, SyntheticSpec};
use fits_core::{finite_min_max, read_fits_from_bytes};

/// A realistic full-frame astronomy image: 24 megapixels, unsigned 16-bit.
fn full_frame_bytes() -> Vec<u8> {
    let (w, h) = (6000, 4000);
    let spec = SyntheticSpec::new(w, h, 16).with_scaling(32768.0, 1.0);
    // A gradient rather than constant data, so the optimiser cannot fold the
    // conversion away and the numbers mean something.
    let pixels: Vec<f64> = (0..w * h).map(|i| (i % 65_536) as f64).collect();
    synthetic_fits(&spec, &pixels).expect("synthetic image")
}

fn bench_read(c: &mut Criterion) {
    let bytes = full_frame_bytes();
    let mut group = c.benchmark_group("read");
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.sample_size(20);
    group.bench_function("6000x4000 u16 decode", |b| {
        b.iter(|| {
            let img = read_fits_from_bytes(black_box(&bytes)).expect("decode");
            black_box(img.data.len())
        });
    });
    group.finish();
}

fn bench_stats(c: &mut Criterion) {
    let data: Vec<f32> = (0..24_000_000).map(|i| (i % 65_536) as f32).collect();
    let mut group = c.benchmark_group("stats");
    group.sample_size(20);
    group.bench_function("finite_min_max 24 MP", |b| {
        b.iter(|| black_box(finite_min_max(black_box(&data))));
    });
    group.finish();
}

criterion_group!(benches, bench_read, bench_stats);
criterion_main!(benches);
