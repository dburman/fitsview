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

/// Work the interface does on every frame, rather than once per image.
///
/// Anything here is paid sixty times a second, so a millisecond is expensive
/// in a way that a millisecond per image is not.
fn bench_per_frame(c: &mut Criterion) {
    use fitsview::folder::{FileEntry, Folder, SortKey};
    use std::path::PathBuf;

    let mut group = c.benchmark_group("per frame");
    group.sample_size(50);

    // Phase 13: the file list works out what counts as unusual, every frame.
    let folder = Folder {
        dir: PathBuf::from("/session"),
        files: (0..200)
            .map(|i| FileEntry {
                path: PathBuf::from(format!("/session/light_{i}.fits")),
                name: format!("light_{i}.fits"),
                size: 1024,
                flagged: false,
                quality: Some(fits_core::Quality {
                    background: 1000.0 + f64::from(i % 17),
                    noise: 20.0,
                    sharpness: 1.0 + f64::from(i % 7) / 10.0,
                }),
                stars: None,
            })
            .collect(),
        selected: Some(0),
    };
    group.bench_function("usual range over 200 files", |b| {
        b.iter(|| black_box(folder.usual_range(black_box(SortKey::Background))));
    });

    // Phase 14 used to measure the stretch here, every frame, for the
    // histogram's marks. It is now computed once per image, so the only
    // per-frame cost is drawing 256 bars. Kept as a record of what was avoided.
    let full = mono(6000, 4000);
    group.bench_function("what the histogram marks used to cost per frame", |b| {
        b.iter(|| {
            black_box(fits_core::compute_stretch(
                black_box(&full),
                &StretchParams::default(),
            ))
        });
    });

    // The pixel readout, which runs every frame with no way to turn it off.
    {
        use egui::{Pos2, Rect, Vec2};
        use fitsview::app::{Action, Model};
        let dir = tempfile::tempdir().unwrap();
        fits_core::testutil::write_synthetic(
            dir.path(),
            "light.fits",
            &SyntheticSpec::new(512, 512, 16),
            &vec![100.0; 512 * 512],
        )
        .unwrap();
        let mut model = Model::new();
        model.handle(Action::Open(dir.path().to_path_buf()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while model.loaded.is_none() && std::time::Instant::now() < deadline {
            model.poll();
        }
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0));
        model.set_viewport(viewport);
        model.pointer = Some(Pos2::new(700.0, 450.0));

        group.bench_function("pixel readout", |b| {
            b.iter(|| black_box(model.pixel_readout()));
        });
    }

    // What it costs now: counting the samples, also once per image.
    group.bench_function("histogram, once per image", |b| {
        b.iter(|| black_box(fits_core::histogram::compute(black_box(&full))));
    });

    group.finish();
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

criterion_group!(benches, bench_texture, bench_per_frame);
criterion_main!(benches);
