//! Long-running work on a background thread.
//!
//! Two operations take long enough to freeze the window if run inline:
//! combining a set of darks into a master, and writing calibrated copies of a
//! whole folder. Both run here, report progress, and can be cancelled.
//!
//! Cancellation is cooperative: the worker checks a flag between files, so it
//! stops at the next boundary rather than mid-write. That is the point at which
//! stopping leaves nothing half-written.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use fits_core::calib::{self, MasterFlat, MasterFrame};
use fits_core::{quality, read_fits, write_fits, FitsImage, Quality};

/// What a finished job produced.
#[derive(Debug)]
pub enum Outcome {
    /// A master calibration frame was built.
    Master(Box<MasterFrame>),
    /// A master flat, already normalised into a gain map, was built.
    Flat(Box<MasterFlat>),
    /// Every file in the folder was measured.
    Measured(Vec<(PathBuf, Quality)>),
    /// Calibrated copies were written, and this many succeeded.
    Exported {
        /// Files written.
        written: usize,
        /// Where they went.
        directory: PathBuf,
    },
}

/// A message from the worker.
#[derive(Debug)]
pub enum Update {
    /// How far along the job is.
    Progress {
        /// Items finished.
        done: usize,
        /// Items in total.
        total: usize,
        /// What is being worked on.
        item: String,
    },
    /// The job finished successfully.
    Finished(Outcome),
    /// The job stopped because it was asked to.
    Cancelled,
    /// The job could not be completed.
    Failed(String),
}

/// A running background job.
#[derive(Debug)]
pub struct Job {
    /// What the job is, for the progress display.
    pub label: String,
    updates: mpsc::Receiver<Update>,
    cancel: Arc<AtomicBool>,
    /// Latest progress, so the interface can draw a bar without storing it.
    pub done: usize,
    /// Items in total.
    pub total: usize,
    /// The item currently being worked on.
    pub item: String,
    finished: bool,
}

impl Job {
    /// Combines the frames at `paths` into a master.
    #[must_use]
    pub fn build_master(paths: Vec<PathBuf>) -> Self {
        let (tx, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let total = paths.len();

        std::thread::Builder::new()
            .name("fitsview-build-master".into())
            .spawn(move || build_master_worker(&paths, &tx, &worker_cancel))
            .ok();

        Self {
            label: "Building master".into(),
            updates,
            cancel,
            done: 0,
            total,
            item: String::new(),
            finished: false,
        }
    }

    /// Combines the frames at `paths` into a master flat, subtracting
    /// `flat_dark` from them first when one is supplied.
    #[must_use]
    pub fn build_flat(paths: Vec<PathBuf>, flat_dark: Option<Arc<MasterFrame>>) -> Self {
        let (tx, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let total = paths.len();

        std::thread::Builder::new()
            .name("fitsview-build-flat".into())
            .spawn(move || {
                build_flat_worker(&paths, flat_dark.as_deref(), &tx, &worker_cancel);
            })
            .ok();

        Self {
            label: "Building master flat".into(),
            updates,
            cancel,
            done: 0,
            total,
            item: String::new(),
            finished: false,
        }
    }

    /// Measures every file in `paths`, so bad frames can be found by sorting.
    ///
    /// Measurement is on the raw frame, before any calibration, so the numbers
    /// stay comparable however the display is configured.
    #[must_use]
    pub fn measure(paths: Vec<PathBuf>) -> Self {
        let (tx, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let total = paths.len();

        std::thread::Builder::new()
            .name("fitsview-measure".into())
            .spawn(move || measure_worker(&paths, &tx, &worker_cancel))
            .ok();

        Self {
            label: "Measuring frames".into(),
            updates,
            cancel,
            done: 0,
            total,
            item: String::new(),
            finished: false,
        }
    }

    /// Writes a calibrated copy of every file in `paths` into `directory`.
    #[must_use]
    pub fn export(
        paths: Vec<PathBuf>,
        dark: Option<Arc<MasterFrame>>,
        flat: Option<Arc<MasterFlat>>,
        directory: PathBuf,
    ) -> Self {
        let (tx, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let total = paths.len();

        std::thread::Builder::new()
            .name("fitsview-export".into())
            .spawn(move || {
                export_worker(
                    &paths,
                    dark.as_deref(),
                    flat.as_deref(),
                    &directory,
                    &tx,
                    &worker_cancel,
                );
            })
            .ok();

        Self {
            label: "Exporting calibrated files".into(),
            updates,
            cancel,
            done: 0,
            total,
            item: String::new(),
            finished: false,
        }
    }

    /// Asks the job to stop at the next file boundary.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether the job has stopped, for any reason.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Fraction complete, between 0 and 1.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    /// Collects everything the worker has sent since the last call.
    ///
    /// Never blocks. Progress updates are folded into this structure; anything
    /// terminal is returned for the caller to act on.
    pub fn poll(&mut self) -> Vec<Update> {
        let mut terminal = Vec::new();
        while let Ok(update) = self.updates.try_recv() {
            match update {
                Update::Progress { done, total, item } => {
                    self.done = done;
                    self.total = total;
                    self.item = item;
                }
                other => {
                    self.finished = true;
                    terminal.push(other);
                }
            }
        }
        // A worker that died without reporting must not leave a progress bar up
        // for ever.
        if !self.finished
            && matches!(
                self.updates.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            )
        {
            self.finished = true;
        }
        terminal
    }
}

/// Reads every frame, then combines them.
fn build_master_worker(paths: &[PathBuf], tx: &mpsc::Sender<Update>, cancel: &AtomicBool) {
    let Some(frames) = read_all(paths, tx, cancel) else {
        return;
    };
    match calib::build_master_median(&frames) {
        Ok(master) => {
            let _ = tx.send(Update::Progress {
                done: paths.len(),
                total: paths.len(),
                item: "combining".into(),
            });
            let _ = tx.send(Update::Finished(Outcome::Master(Box::new(master))));
        }
        Err(e) => {
            let _ = tx.send(Update::Failed(e.to_string()));
        }
    }
}

/// Reads every flat, then combines and normalises them.
fn build_flat_worker(
    paths: &[PathBuf],
    flat_dark: Option<&MasterFrame>,
    tx: &mpsc::Sender<Update>,
    cancel: &AtomicBool,
) {
    let Some(frames) = read_all(paths, tx, cancel) else {
        return;
    };
    match calib::build_master_flat(&frames, flat_dark) {
        Ok(flat) => {
            let _ = tx.send(Update::Progress {
                done: paths.len(),
                total: paths.len(),
                item: "normalising".into(),
            });
            let _ = tx.send(Update::Finished(Outcome::Flat(Box::new(flat))));
        }
        Err(e) => {
            let _ = tx.send(Update::Failed(e.to_string()));
        }
    }
}

/// Measures each file in turn.
///
/// A file that cannot be read is skipped rather than failing the run: one
/// corrupt frame in two hundred should not deny the user the other 199
/// measurements.
fn measure_worker(paths: &[PathBuf], tx: &mpsc::Sender<Update>, cancel: &AtomicBool) {
    let mut measured = Vec::with_capacity(paths.len());

    for (index, path) in paths.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            // Report what was measured before stopping; the work is not wasted.
            let _ = tx.send(Update::Finished(Outcome::Measured(measured)));
            return;
        }
        let _ = tx.send(Update::Progress {
            done: index,
            total: paths.len(),
            item: file_name_of(path),
        });

        match read_fits(path) {
            Ok(image) => measured.push((path.clone(), quality::measure(&image))),
            Err(e) => log::warn!("could not measure {}: {e}", file_name_of(path)),
        }
    }

    let _ = tx.send(Update::Progress {
        done: paths.len(),
        total: paths.len(),
        item: String::new(),
    });
    let _ = tx.send(Update::Finished(Outcome::Measured(measured)));
}

/// Reads every path, reporting progress. Returns `None` if it stopped early.
fn read_all(
    paths: &[PathBuf],
    tx: &mpsc::Sender<Update>,
    cancel: &AtomicBool,
) -> Option<Vec<Arc<FitsImage>>> {
    let mut frames = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(Update::Cancelled);
            return None;
        }
        let _ = tx.send(Update::Progress {
            done: index,
            total: paths.len(),
            item: file_name_of(path),
        });
        match read_fits(path) {
            Ok(image) => frames.push(Arc::new(image)),
            Err(e) => {
                let _ = tx.send(Update::Failed(format!("{}: {e}", file_name_of(path))));
                return None;
            }
        }
    }
    Some(frames)
}

/// Calibrates each file and writes it into the output folder.
///
/// Originals are never opened for writing, and the output name always differs
/// from the input, so an export cannot overwrite what it is reading.
fn export_worker(
    paths: &[PathBuf],
    dark: Option<&MasterFrame>,
    flat: Option<&MasterFlat>,
    directory: &Path,
    tx: &mpsc::Sender<Update>,
    cancel: &AtomicBool,
) {
    if let Err(e) = std::fs::create_dir_all(directory) {
        let _ = tx.send(Update::Failed(format!("{}: {e}", directory.display())));
        return;
    }

    let history = calib::history_for(dark, flat);
    let mut written = 0usize;

    for (index, path) in paths.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(Update::Cancelled);
            return;
        }
        let _ = tx.send(Update::Progress {
            done: index,
            total: paths.len(),
            item: file_name_of(path),
        });

        let light = match read_fits(path) {
            Ok(image) => image,
            Err(e) => {
                let _ = tx.send(Update::Failed(format!("{}: {e}", file_name_of(path))));
                return;
            }
        };

        let calibrated = match calib::calibrate(&light, dark, flat) {
            Ok(image) => image,
            Err(e) => {
                let _ = tx.send(Update::Failed(format!("{}: {e}", file_name_of(path))));
                return;
            }
        };

        let out = directory.join(output_name_for(path));
        if let Err(e) = write_fits(&out, &calibrated, &history) {
            let _ = tx.send(Update::Failed(format!("{}: {e}", file_name_of(&out))));
            return;
        }
        written += 1;
    }

    let _ = tx.send(Update::Progress {
        done: paths.len(),
        total: paths.len(),
        item: String::new(),
    });
    let _ = tx.send(Update::Finished(Outcome::Exported {
        written,
        directory: directory.to_path_buf(),
    }));
}

/// The name a calibrated copy is written under.
///
/// Always different from the input name, so that exporting into the source
/// folder cannot overwrite an original.
#[must_use]
pub fn output_name_for(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map_or_else(|| "image".to_string(), |s| s.to_string_lossy().into_owned());
    format!("{stem}_cal.fits")
}

/// A path's file name, for messages.
fn file_name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::testutil::{write_synthetic, SyntheticSpec};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Writes `count` frames whose pixels are all `value`.
    fn frames(count: usize, value: f64) -> (TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, -32);
        let paths = (0..count)
            .map(|i| {
                write_synthetic(dir.path(), &format!("f{i}.fits"), &spec, &vec![value; 64]).unwrap()
            })
            .collect();
        (dir, paths)
    }

    /// Polls a job to completion and returns what it reported.
    fn run(job: &mut Job) -> Vec<Update> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut out = Vec::new();
        while Instant::now() < deadline {
            out.extend(job.poll());
            if job.is_finished() {
                return out;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("job never finished");
    }

    #[test]
    fn building_a_master_reports_progress_and_returns_it() {
        let (_dir, paths) = frames(3, 100.0);
        let mut job = Job::build_master(paths);
        let updates = run(&mut job);

        assert_eq!(job.done, 3);
        assert_eq!(job.total, 3);
        assert!((job.fraction() - 1.0).abs() < f32::EPSILON);

        match updates.into_iter().next() {
            Some(Update::Finished(Outcome::Master(master))) => {
                assert_eq!(master.source_count, 3);
                assert_eq!(master.width, 8);
                assert!((master.data[0] - 100.0).abs() < 0.001);
            }
            other => panic!("expected a master, got {other:?}"),
        }
    }

    #[test]
    fn building_from_frames_of_different_sizes_fails_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_synthetic(
            dir.path(),
            "a.fits",
            &SyntheticSpec::new(8, 8, -32),
            &[1.0; 64],
        )
        .unwrap();
        let b = write_synthetic(
            dir.path(),
            "b.fits",
            &SyntheticSpec::new(4, 4, -32),
            &[1.0; 16],
        )
        .unwrap();

        let mut job = Job::build_master(vec![a, b]);
        let updates = run(&mut job);
        assert!(
            matches!(updates.first(), Some(Update::Failed(_))),
            "got {updates:?}"
        );
    }

    #[test]
    fn building_from_an_unreadable_file_fails_rather_than_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("broken.fits");
        std::fs::write(&bad, b"SIMPLE but nonsense").unwrap();

        let mut job = Job::build_master(vec![bad]);
        let updates = run(&mut job);
        match updates.first() {
            Some(Update::Failed(message)) => assert!(message.contains("broken.fits"), "{message}"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn exporting_writes_a_calibrated_copy_of_every_file() {
        let (dir, paths) = frames(3, 500.0);
        let out_dir = tempfile::tempdir().unwrap();

        let dark =
            calib::build_master_median(&[Arc::new(fits_core::read_fits(&paths[0]).unwrap())])
                .unwrap();

        let mut job = Job::export(
            paths.clone(),
            Some(Arc::new(dark)),
            None,
            out_dir.path().to_path_buf(),
        );
        let updates = run(&mut job);

        match updates.into_iter().next() {
            Some(Update::Finished(Outcome::Exported { written, .. })) => assert_eq!(written, 3),
            other => panic!("expected an export, got {other:?}"),
        }

        for i in 0..3 {
            let written = out_dir.path().join(format!("f{i}_cal.fits"));
            assert!(written.exists(), "{} missing", written.display());
            let image = fits_core::read_fits(&written).unwrap();
            // Every frame minus itself is zero.
            assert!(image.data.iter().all(|v| *v == 0.0), "not calibrated");
        }

        // The originals are untouched.
        for path in &paths {
            let original = fits_core::read_fits(path).unwrap();
            assert!(original.data.iter().all(|v| (*v - 500.0).abs() < 0.001));
        }
        let _ = dir;
    }

    #[test]
    fn exporting_without_a_dark_copies_the_values_through() {
        let (_dir, paths) = frames(2, 42.0);
        let out_dir = tempfile::tempdir().unwrap();

        let mut job = Job::export(paths, None, None, out_dir.path().to_path_buf());
        run(&mut job);

        let image = fits_core::read_fits(&out_dir.path().join("f0_cal.fits")).unwrap();
        assert!(image.data.iter().all(|v| (*v - 42.0).abs() < 0.001));
    }

    #[test]
    fn exporting_records_what_was_applied_in_the_file() {
        let (_dir, paths) = frames(1, 100.0);
        let out_dir = tempfile::tempdir().unwrap();
        let dark =
            calib::build_master_median(&[Arc::new(fits_core::read_fits(&paths[0]).unwrap())])
                .unwrap();

        let mut job = Job::export(
            paths,
            Some(Arc::new(dark)),
            None,
            out_dir.path().to_path_buf(),
        );
        run(&mut job);

        let bytes = std::fs::read(out_dir.path().join("f0_cal.fits")).unwrap();
        let text = String::from_utf8_lossy(&bytes[..2880]);
        assert!(text.contains("HISTORY fitsview: dark subtracted"), "{text}");
    }

    #[test]
    fn an_export_never_overwrites_an_original_even_into_the_same_folder() {
        // The output name always differs from the input name, so exporting in
        // place is safe rather than destructive.
        let (dir, paths) = frames(2, 77.0);
        let mut job = Job::export(paths.clone(), None, None, dir.path().to_path_buf());
        run(&mut job);

        for path in &paths {
            let original = fits_core::read_fits(path).unwrap();
            assert!(
                original.data.iter().all(|v| (*v - 77.0).abs() < 0.001),
                "{} was modified",
                path.display()
            );
        }
        assert!(dir.path().join("f0_cal.fits").exists());
    }

    #[test]
    fn a_cancelled_export_stops_and_reports_it() {
        let (_dir, paths) = frames(40, 100.0);
        let out_dir = tempfile::tempdir().unwrap();

        let mut job = Job::export(paths, None, None, out_dir.path().to_path_buf());
        job.cancel();
        let updates = run(&mut job);

        assert!(
            updates
                .iter()
                .any(|u| matches!(u, Update::Cancelled | Update::Finished(_))),
            "expected a terminal update, got {updates:?}"
        );
        // Cancelling promptly must not have written the whole folder.
        let count = std::fs::read_dir(out_dir.path()).unwrap().count();
        assert!(count < 40, "wrote {count} files despite cancelling");
    }

    #[test]
    fn exporting_to_a_folder_that_does_not_exist_creates_it() {
        let (_dir, paths) = frames(1, 5.0);
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("new").join("nested");

        let mut job = Job::export(paths, None, None, target.clone());
        run(&mut job);
        assert!(target.join("f0_cal.fits").exists());
    }

    #[test]
    fn output_names_are_derived_from_the_input_and_always_differ() {
        assert_eq!(
            output_name_for(Path::new("/x/light_1.fits")),
            "light_1_cal.fits"
        );
        assert_eq!(
            output_name_for(Path::new("/x/light_1.fit")),
            "light_1_cal.fits"
        );
        assert_ne!(output_name_for(Path::new("/x/a.fits")), "a.fits");
    }

    #[test]
    fn measuring_reports_a_value_for_every_readable_file() {
        let (_dir, paths) = frames(3, 100.0);
        let mut job = Job::measure(paths.clone());
        let updates = run(&mut job);

        match updates.into_iter().next() {
            Some(Update::Finished(Outcome::Measured(measured))) => {
                assert_eq!(measured.len(), 3);
                for (path, quality) in measured {
                    assert!(paths.contains(&path));
                    assert!((quality.background - 100.0).abs() < 0.01);
                    assert!(quality.sharpness.is_finite());
                }
            }
            other => panic!("expected measurements, got {other:?}"),
        }
    }

    #[test]
    fn one_unreadable_file_does_not_deny_the_measurements_of_the_rest() {
        let (dir, mut paths) = frames(3, 50.0);
        let bad = dir.path().join("broken.fits");
        std::fs::write(&bad, b"SIMPLE but nonsense").unwrap();
        paths.push(bad);

        let mut job = Job::measure(paths);
        let updates = run(&mut job);
        match updates.into_iter().next() {
            Some(Update::Finished(Outcome::Measured(measured))) => {
                assert_eq!(measured.len(), 3, "the readable ones should survive");
            }
            other => panic!("expected measurements, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_a_measurement_keeps_what_was_already_done() {
        // Throwing the finished work away would make cancelling costly, and
        // there is no reason for it: partial measurements are still useful.
        let (_dir, paths) = frames(30, 10.0);
        let mut job = Job::measure(paths);
        job.cancel();
        let updates = run(&mut job);

        match updates.into_iter().next() {
            Some(Update::Finished(Outcome::Measured(measured))) => {
                assert!(measured.len() < 30, "it should have stopped early");
            }
            other => panic!("expected partial measurements, got {other:?}"),
        }
    }

    #[test]
    fn fraction_is_safe_for_an_empty_job() {
        let mut job = Job::build_master(Vec::new());
        assert!((job.fraction() - 0.0).abs() < f32::EPSILON);
        run(&mut job);
    }
}
