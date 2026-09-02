//! Measured checks on folder browsing.
//!
//! Two of Phase 3's acceptance criteria are about behaviour under load rather
//! than correctness: stepping through a folder must not stall the interface,
//! and memory must stay bounded however many files the folder holds. Both are
//! easy to claim and easy to get wrong, so they are measured here.

use std::time::{Duration, Instant};

use fits_core::testutil::{write_synthetic, SyntheticSpec};
use fitsview::app::{Action, Model};
use fitsview::loader::DEFAULT_MAX_ENTRIES;
use tempfile::TempDir;

/// Writes `count` files of `width` x `height` into a fresh folder.
fn folder_of(count: usize, width: usize, height: usize) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let spec = SyntheticSpec::new(width, height, 16).with_scaling(32768.0, 1.0);
    let pixels: Vec<f64> = (0..width * height).map(|i| (i % 60_000) as f64).collect();
    for i in 1..=count {
        write_synthetic(dir.path(), &format!("light_{i}.fits"), &spec, &pixels).unwrap();
    }
    dir
}

/// Pumps until the current selection is resolved.
fn settle(model: &mut Model) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        model.poll();
        if !model.loading && (model.loaded.is_some() || model.error.is_some()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("model never settled");
}

#[test]
fn stepping_through_a_folder_never_blocks_the_interface() {
    // The interface thread must never decode. Holding the arrow key down sends
    // a burst of NextFile actions, and every one must return promptly whether
    // or not the image is ready. If decoding leaked back onto this thread, the
    // 40 steps below would take a second or more instead of microseconds.
    let count = 40;
    let dir = folder_of(count, 1000, 1000);

    let mut model = Model::new();
    model.handle(Action::Open(dir.path().to_path_buf()));
    settle(&mut model);

    let mut worst = Duration::ZERO;
    let started = Instant::now();
    for _ in 0..count {
        let step = Instant::now();
        model.handle(Action::NextFile);
        model.poll();
        worst = worst.max(step.elapsed());
    }
    let total = started.elapsed();

    // One frame at 60 fps is 16.7 ms. A single step must be far inside that,
    // because the frame also has to draw.
    assert!(
        worst < Duration::from_millis(16),
        "the slowest step took {worst:?}, which would drop a frame"
    );
    assert!(
        total < Duration::from_millis(200),
        "40 steps took {total:?}, so work is happening on the interface thread"
    );
    assert_eq!(model.position_label(), format!("{count} / {count}"));
}

#[test]
fn stepping_through_full_frame_images_does_not_block_either() {
    // The same check with images the size a real camera produces. Fewer of
    // them, because each is 48 MB on disk.
    let dir = folder_of(4, 6000, 4000);
    // Guard against this test quietly shrinking: 6000 x 4000 at 16 bits is
    // about 48 MB per frame, and the point is to exercise that size.
    let size = std::fs::metadata(dir.path().join("light_1.fits"))
        .unwrap()
        .len();
    assert!(
        size > 45_000_000,
        "expected a full-frame file, got {size} bytes"
    );

    let mut model = Model::new();
    model.handle(Action::Open(dir.path().to_path_buf()));
    settle(&mut model);

    let mut worst = Duration::ZERO;
    for _ in 0..4 {
        let step = Instant::now();
        model.handle(Action::NextFile);
        model.poll();
        worst = worst.max(step.elapsed());
    }
    assert!(
        worst < Duration::from_millis(16),
        "a 24 megapixel step took {worst:?} on the interface thread"
    );
}

#[test]
fn memory_stays_bounded_across_a_large_folder() {
    // Visiting every file in a big folder must not accumulate every image.
    let count = 200;
    let dir = folder_of(count, 120, 120);

    let mut model = Model::new();
    model.handle(Action::Open(dir.path().to_path_buf()));
    settle(&mut model);

    let mut peak_entries = 0;
    let mut peak_bytes = 0;
    for _ in 0..count {
        model.handle(Action::NextFile);
        settle(&mut model);
        let (entries, bytes) = model.cache_stats();
        peak_entries = peak_entries.max(entries);
        peak_bytes = peak_bytes.max(bytes);
    }

    assert!(
        peak_entries <= DEFAULT_MAX_ENTRIES,
        "cache grew to {peak_entries} entries, above the {DEFAULT_MAX_ENTRIES} bound"
    );
    // 200 images of 120x120 would be about 11 MB if every one were kept.
    let all_of_them = count * 120 * 120 * std::mem::size_of::<f32>();
    assert!(
        peak_bytes < all_of_them / 4,
        "cache held {peak_bytes} bytes, close to the {all_of_them} of the whole folder"
    );
    assert_eq!(model.position_label(), format!("{count} / {count}"));
}

#[test]
fn a_folder_of_mixed_content_lists_only_fits_files() {
    let dir = folder_of(3, 10, 10);
    std::fs::write(dir.path().join("notes.txt"), b"text").unwrap();
    std::fs::write(dir.path().join("preview.png"), b"png").unwrap();
    std::fs::write(dir.path().join("session.log"), b"log").unwrap();

    let mut model = Model::new();
    model.handle(Action::Open(dir.path().to_path_buf()));
    settle(&mut model);

    assert_eq!(model.position_label(), "1 / 3");
    let names: Vec<String> = model
        .folder
        .as_ref()
        .unwrap()
        .files
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert!(names.iter().all(|n| n.ends_with(".fits")), "{names:?}");
}

#[test]
fn jumping_to_the_end_does_not_wait_for_every_file_in_between() {
    // The loader replaces its queue rather than appending, so a jump should
    // resolve in about the time of one decode, not forty.
    let dir = folder_of(40, 800, 800);

    let mut model = Model::new();
    model.handle(Action::Open(dir.path().to_path_buf()));
    settle(&mut model);

    let started = Instant::now();
    model.handle(Action::LastFile);
    settle(&mut model);
    let elapsed = started.elapsed();

    assert_eq!(model.position_label(), "40 / 40");
    assert!(
        elapsed < Duration::from_secs(2),
        "jumping to the last file took {elapsed:?}, so intermediate files were decoded"
    );
}
