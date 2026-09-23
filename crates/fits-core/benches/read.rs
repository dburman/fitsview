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
use fits_core::calib::{build_master_flat, build_master_median, calibrate, subtract_dark};
use fits_core::debayer::{debayer, BayerPattern};
use fits_core::quality;
use fits_core::stretch::{build_lut, compute_stretch, StretchParams};
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

/// Computing the stretch and building its table, once per image. The plan
/// requires a stretched redraw to stay under 100 ms on a 24 megapixel frame,
/// and this is the part that is not already measured by `bench_read`.
fn bench_stretch(c: &mut Criterion) {
    let bytes = full_frame_bytes();
    let image = read_fits_from_bytes(&bytes).expect("decode");
    let params = StretchParams::default();

    let mut group = c.benchmark_group("stretch");
    group.sample_size(20);
    group.bench_function("compute 24 MP", |b| {
        b.iter(|| black_box(compute_stretch(black_box(&image), &params)));
    });

    let stretch = compute_stretch(&image, &params);
    group.bench_function("build lookup table", |b| {
        b.iter(|| black_box(build_lut(black_box(&stretch[0]))));
    });

    // A debayered frame is three planes, and the stretch measures all of them
    // together, so a one-shot colour user pays this rather than the figure
    // above. Worth knowing before optimising the wrong thing.
    let colour = debayer(&image, BayerPattern::Rggb).expect("debayer");
    group.bench_function("compute 24 MP colour", |b| {
        b.iter(|| black_box(compute_stretch(black_box(&colour), &params)));
    });
    group.finish();
}

/// Calibration. The plan requires applying a dark to a 24 megapixel frame to
/// add under 50 ms, since it happens on every image the user steps to.
fn bench_calibration(c: &mut Criterion) {
    let bytes = full_frame_bytes();
    let image = std::sync::Arc::new(read_fits_from_bytes(&bytes).expect("decode"));
    let master = build_master_median(std::slice::from_ref(&image)).expect("master");

    let mut group = c.benchmark_group("calibration");
    group.sample_size(20);
    group.bench_function("subtract dark 24 MP", |b| {
        b.iter(|| black_box(subtract_dark(black_box(&image), black_box(&master))));
    });
    group.bench_function("build master from 5 frames 24 MP", |b| {
        let frames: Vec<_> = (0..5).map(|_| image.clone()).collect();
        b.iter(|| black_box(build_master_median(black_box(&frames))));
    });

    // The plan requires dark and flat together to stay under 100 ms per image.
    let flat = build_master_flat(std::slice::from_ref(&image), None).expect("flat");
    group.bench_function("dark and flat together 24 MP", |b| {
        b.iter(|| {
            black_box(calibrate(
                black_box(&image),
                Some(black_box(&master)),
                Some(black_box(&flat)),
            ))
        });
    });
    group.finish();
}

/// Debayering, which happens once per image when a one-shot colour frame is
/// displayed and is cached alongside the calibrated result.
fn bench_debayer(c: &mut Criterion) {
    let bytes = full_frame_bytes();
    let image = read_fits_from_bytes(&bytes).expect("decode");

    let mut group = c.benchmark_group("debayer");
    group.sample_size(10);
    group.bench_function("24 MP mosaic", |b| {
        b.iter(|| black_box(debayer(black_box(&image), BayerPattern::Rggb)));
    });
    group.finish();
}

/// Measuring a frame, which the loader worker does alongside decoding so the
/// interface thread never pays for it.
fn bench_quality(c: &mut Criterion) {
    let bytes = full_frame_bytes();
    let image = read_fits_from_bytes(&bytes).expect("decode");
    let mut group = c.benchmark_group("quality");
    group.sample_size(20);
    group.bench_function("measure 24 MP", |b| {
        b.iter(|| black_box(quality::measure(black_box(&image))));
    });
    group.finish();
}

/// Star detection, which runs on the worker thread when it is asked for.
fn bench_stars(c: &mut Criterion) {
    use fits_core::stars::{self, DetectionParams};
    use fits_core::testutil::gaussian_background;

    // A realistic frame: sky, noise, and a few thousand stars.
    let (w, h) = (6000usize, 4000usize);
    let mut pixels = gaussian_background(w, h, 1000.0, 20.0, 77);
    let mut rng = fits_core::testutil::Prng::new(78);
    for _ in 0..3000 {
        let cx = rng.next_f64() * (w - 20) as f64 + 10.0;
        let cy = rng.next_f64() * (h - 20) as f64 + 10.0;
        for dy in -5i64..=5 {
            for dx in -5i64..=5 {
                let x = cx as i64 + dx;
                let y = cy as i64 + dy;
                if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                    continue;
                }
                let r = (dx * dx + dy * dy) as f64;
                pixels[y as usize * w + x as usize] += 9000.0 * (-r / 8.0).exp();
            }
        }
    }
    let spec = SyntheticSpec::new(w, h, -32);
    let image =
        read_fits_from_bytes(&synthetic_fits(&spec, &pixels).expect("build")).expect("decode");

    let mut group = c.benchmark_group("stars");
    group.sample_size(10);
    group.bench_function("detect on 24 MP", |b| {
        b.iter(|| {
            black_box(stars::detect(
                black_box(&image),
                &DetectionParams::default(),
            ))
        });
    });
    group.finish();
}

/// Adding a frame into a stack, and taking the average out again.
///
/// The stack is the one place a whole extra frame's worth of memory is spent,
/// so both the time and that cost are worth watching.
fn bench_stack(c: &mut Criterion) {
    use fits_core::header::FitsHeader;
    use fits_core::stack::{Alignment, Stack};

    let (w, h) = (6000usize, 4000usize);
    let pixels = fits_core::testutil::gaussian_background(w, h, 1000.0, 20.0, 9);
    let spec = SyntheticSpec::new(w, h, -32);
    let image =
        read_fits_from_bytes(&synthetic_fits(&spec, &pixels).expect("build")).expect("decode");

    // Shifted, since an aligned frame is the exception rather than the rule.
    let alignment = Alignment {
        dx: 7.0,
        dy: -3.0,
        ..Alignment::still()
    };

    let mut group = c.benchmark_group("stack");
    group.sample_size(10);
    group.bench_function("add a 24 MP frame", |b| {
        b.iter_batched_ref(
            || Stack::new(w, h, 1),
            |stack| stack.add(black_box(&image), alignment),
            criterion::BatchSize::LargeInput,
        );
    });
    // Rejection keeps running sums beside the totals in the first pass, and
    // judges every sample against them in the second.
    group.bench_function("add a 24 MP frame, recording for rejection", |b| {
        b.iter_batched_ref(
            || Stack::rejecting(w, h, 1),
            |stack| stack.add(black_box(&image), alignment),
            criterion::BatchSize::LargeInput,
        );
    });
    group.bench_function("judge a 24 MP frame against the others", |b| {
        let others: Vec<_> = (0..5u64)
            .map(|seed| {
                let pixels =
                    fits_core::testutil::gaussian_background(w, h, 1000.0, 20.0, 20 + seed);
                read_fits_from_bytes(&synthetic_fits(&spec, &pixels).expect("build"))
                    .expect("decode")
            })
            .collect();
        let mut first = Stack::rejecting(w, h, 1);
        for frame in &others {
            first.add(frame, alignment);
        }
        let second = first
            .into_rejecting(fits_core::stack::DEFAULT_CLIP)
            .expect("five frames");
        b.iter_batched_ref(
            || second.clone(),
            |second| second.add(black_box(&others[0]), alignment),
            criterion::BatchSize::LargeInput,
        );
    });
    // Almost every frame lands between the stack's pixels and is resampled
    // there. Whether its inner step was inlined once made the difference
    // between half a second and three on a night's stack, so it is watched.
    group.bench_function("add a 24 MP frame between pixels", |b| {
        let between = Alignment {
            dx: 7.37,
            dy: -3.61,
            rotation: 1.3e-5,
            ..Alignment::still()
        };
        b.iter_batched_ref(
            || Stack::new(w, h, 1),
            |stack| stack.add(black_box(&image), between),
            criterion::BatchSize::LargeInput,
        );
    });
    group.bench_function("average 24 MP out", |b| {
        let mut stack = Stack::new(w, h, 1);
        stack.add(&image, alignment);
        b.iter(|| black_box(stack.finish(FitsHeader { cards: Vec::new() })));
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_read,
    bench_stats,
    bench_stretch,
    bench_calibration,
    bench_debayer,
    bench_quality,
    bench_stars,
    bench_stack
);
criterion_main!(benches);
