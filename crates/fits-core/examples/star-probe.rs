//! Reports what star detection makes of real frames, and how long it takes.
//!
//! Synthetic frames cannot answer the questions that matter here. They have
//! stars at six hundred deviations above a background with no gradient, no
//! filter mosaic and no hot pixels, so any threshold at all looks correct on
//! them. Every mistake this crate's detection has made was found by pointing
//! this at a folder of real sub-exposures and reading the columns:
//!
//! - noise far larger than it should be means the estimator is reading
//!   something structural, such as a colour mosaic, as though it were noise;
//! - a frame taken with the cover on reporting thousands of stars means the
//!   rejections are not doing their job;
//! - counts that jump about between consecutive frames of one sequence mean
//!   the measurement is describing the algorithm rather than the sky.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p fits-core --all-features --example star-probe -- \
//!     <folder> [frames] [threshold] [limit]
//! ```

use std::time::Instant;

use fits_core::debayer::BayerPattern;
use fits_core::stars::{self, DetectionParams};
use fits_core::{read_fits, FitsImage};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: star-probe <folder> [frames] [threshold] [limit]");
        std::process::exit(2);
    };
    let wanted: usize = args.next().map_or(8, |n| n.parse().unwrap_or(8));
    let params = DetectionParams {
        threshold: args.next().map_or(5.0, |n| n.parse().unwrap_or(5.0)),
        limit: args
            .next()
            .map_or(100_000, |n| n.parse().unwrap_or(100_000)),
        ..DetectionParams::default()
    };

    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("read the folder")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| {
                ["fits", "fit", "fts"]
                    .iter()
                    .any(|k| e.eq_ignore_ascii_case(k))
            })
        })
        .collect();
    paths.sort();
    if paths.is_empty() {
        eprintln!("no FITS files in {dir}");
        std::process::exit(1);
    }

    println!(
        "threshold {} deviations, smoothing {}, limit {}\n",
        params.threshold, params.smoothing, params.limit
    );
    println!(
        "{:<26} {:>12} {:>8} {:>7} {:>7} {:>6} {:>6} {:>7} {:>6}",
        "frame", "filter", "back", "noise", "stars", "fwhm", "round", "path", "ms"
    );

    let step = (paths.len() / wanted.max(1)).max(1);
    for path in paths.iter().step_by(step).take(wanted) {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let Ok(image) = read_fits(path) else {
            println!("{name:<26} could not be read");
            continue;
        };

        let keyword = |k: &str| {
            image
                .header
                .get(k)
                .map(|v| v.trim().trim_matches('\'').trim().to_string())
        };
        let filter_name = keyword("FILTER").unwrap_or_else(|| "—".to_string());
        let pattern = keyword("BAYERPAT").and_then(|p| BayerPattern::parse(&p));

        // The colour path when the frame says it is a mosaic, which is what
        // the application uses; the plain path otherwise.
        let started = Instant::now();
        let (field, route) = match pattern {
            Some(p) if image.channels == 1 => (stars::detect_mosaic(&image, p, &params), "mosaic"),
            _ => (stars::detect(&image, &params), "plain"),
        };
        let detect_ms = started.elapsed().as_secs_f64() * 1000.0;

        let (background, noise) = level_and_noise(&image);
        let route = if field.threshold_was_raised() {
            format!("{route} {:.0}x", field.threshold_scale)
        } else {
            route.to_string()
        };
        println!(
            "{:<26} {:>12} {:>8.1} {:>7.2} {:>7} {:>6} {:>6} {:>7} {:>6.0}",
            short(&name),
            filter_name,
            background,
            noise,
            field.count(),
            figure(field.fwhm),
            figure(field.roundness),
            route,
            detect_ms
        );
    }
}

/// The frame's own background and noise, for the report.
fn level_and_noise(image: &FitsImage) -> (f64, f64) {
    if image.channels != 1 {
        return (f64::NAN, f64::NAN);
    }
    let map = fits_core::background::BackgroundMap::measure(image);
    map.typical()
}

/// Two decimal places, or a dash where there was no answer.
fn figure(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |v| format!("{v:.2}"))
}

/// The tail of a long capture filename, which is the part that differs.
fn short(name: &str) -> String {
    let trimmed = name.strip_suffix(".fits").unwrap_or(name);
    if trimmed.len() <= 25 {
        return trimmed.to_string();
    }
    trimmed
        .char_indices()
        .nth(trimmed.chars().count().saturating_sub(25))
        .map_or_else(|| trimmed.to_string(), |(i, _)| trimmed[i..].to_string())
}
