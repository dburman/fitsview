//! Benchmarks for the path from decoded samples to an uploadable texture.
//!
//! Everything else in this project is measured; this was not, which meant the
//! one piece of work that happens on the way to the screen for every image was
//! the only one nobody had a number for.
//!
//! ```text
//! cargo bench --package fitsview --all-features
//! ```

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use fits_core::stretch::StretchParams;
use fits_core::testutil::{synthetic_fits, SyntheticSpec};
use fits_core::{read_fits_from_bytes, FitsImage};
use fitsview::texture::{self, Mapping};

/// A mono frame of the given size, with varied content so nothing folds away.
fn mono(width: usize, height: usize) -> FitsImage {
    let spec = SyntheticSpec::new(width, height, 16).with_scaling(32768.0, 1.0);
    let pixels: Vec<f64> = (0..width * height).map(|i| (i % 60_000) as f64).collect();
    read_fits_from_bytes(&synthetic_fits(&spec, &pixels).expect("build")).expect("decode")
}

/// A three-channel frame, which walks three planes rather than one.
fn colour(width: usize, height: usize) -> FitsImage {
    let spec = SyntheticSpec::new(width, height, 16)
        .with_channels(3)
        .with_scaling(32768.0, 1.0);
    let pixels: Vec<f64> = (0..width * height * 3)
        .map(|i| (i % 60_000) as f64)
        .collect();
    read_fits_from_bytes(&synthetic_fits(&spec, &pixels).expect("build")).expect("decode")
}

fn bench_texture(c: &mut Criterion) {
    let mut group = c.benchmark_group("texture");
    group.sample_size(20);

    // A modest frame at native size: the factor-1 path.
    let small = mono(2048, 2048);
    let linear = Mapping::linear(&small);
    group.bench_function("4 MP mono, factor 1, linear", |b| {
        b.iter(|| black_box(texture::to_color_image(black_box(&small), &linear, 1)));
    });

    // The common case: a full frame, downsampled by two.
    let full = mono(6000, 4000);
    let factor = texture::downsample_factor(full.width, full.height, texture::MAX_TEXTURE_EDGE);
    assert_eq!(factor, 2, "the common case should downsample by two");
    let linear_full = Mapping::linear(&full);
    group.bench_function("24 MP mono, factor 2, linear", |b| {
        b.iter(|| {
            black_box(texture::to_color_image(
                black_box(&full),
                &linear_full,
                factor,
            ))
        });
    });

    // The same with a stretch, which adds a table lookup per sample.
    let stretched = Mapping::stretched(&full, &StretchParams::default());
    group.bench_function("24 MP mono, factor 2, stretched", |b| {
        b.iter(|| {
            black_box(texture::to_color_image(
                black_box(&full),
                &stretched,
                factor,
            ))
        });
    });

    // Colour walks three planes for every output texel.
    let rgb = colour(3000, 2000);
    let linear_rgb = Mapping::linear(&rgb);
    group.bench_function("6 MP colour, factor 1, linear", |b| {
        b.iter(|| black_box(texture::to_color_image(black_box(&rgb), &linear_rgb, 1)));
    });

    // What a one-shot colour user actually gets: a full frame, debayered to
    // three channels, then downsampled by two. The worst case in the
    // application, and the one worth knowing.
    let full_rgb = colour(6000, 4000);
    let linear_full_rgb = Mapping::linear(&full_rgb);
    group.bench_function("24 MP colour, factor 2, linear", |b| {
        b.iter(|| {
            black_box(texture::to_color_image(
                black_box(&full_rgb),
                &linear_full_rgb,
                factor,
            ))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_texture);
criterion_main!(benches);
