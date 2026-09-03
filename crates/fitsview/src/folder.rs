//! The list of files being browsed, and the selection within it.
//!
//! Plain data with no I/O beyond the single scan, so every rule about how
//! selection moves is testable directly.

use std::path::{Path, PathBuf};

use fits_core::{is_fits_path, Quality};

use crate::natsort;
use crate::sidecar;

/// One file in the browsed folder.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    /// Full path on disk.
    pub path: PathBuf,
    /// File name, used for display and sorting.
    pub name: String,
    /// Size in bytes, shown in the list.
    pub size: u64,
    /// Marked to keep. Phase 4 gives this meaning; the column exists now so the
    /// list layout does not change later.
    pub flagged: bool,
    /// What the frame looks like, once it has been measured.
    ///
    /// `None` until the file has been opened or the folder measured, since
    /// measuring means reading the file.
    pub quality: Option<Quality>,
}

/// What the file list is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Natural name order, which is capture order for most cameras.
    #[default]
    Name,
    /// Sky background: cloud, moonlight and dawn raise it.
    Background,
    /// Structure relative to noise: blur and cloud reduce it.
    Sharpness,
}

impl SortKey {
    /// Every ordering, for offering a choice.
    pub const ALL: [SortKey; 3] = [SortKey::Name, SortKey::Background, SortKey::Sharpness];

    /// What to call it in the interface.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Background => "Background",
            SortKey::Sharpness => "Sharpness",
        }
    }

    /// The measured value this key orders by.
    #[must_use]
    pub fn value_of(self, entry: &FileEntry) -> Option<f64> {
        let quality = entry.quality?;
        match self {
            SortKey::Name => None,
            SortKey::Background => Some(quality.background),
            SortKey::Sharpness => Some(quality.sharpness),
        }
    }
}

/// The range of a measure that counts as ordinary for a folder.
///
/// Anything outside it is worth a second look. Derived from the median and the
/// median absolute deviation rather than the mean and standard deviation, so
/// that a handful of ruined frames do not widen the range enough to hide
/// themselves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsualRange {
    /// Below this is unusual.
    pub low: f64,
    /// Above this is unusual.
    pub high: f64,
}

/// How many deviations from the median count as unusual.
const OUTLIER_DEVIATIONS: f64 = 3.0;

/// A folder of FITS files and the current position within it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Folder {
    /// The folder that was scanned.
    pub dir: PathBuf,
    /// Matching files, in natural name order.
    pub files: Vec<FileEntry>,
    /// Index into `files`, or `None` when the folder is empty.
    pub selected: Option<usize>,
}

/// Scans a folder for FITS files.
///
/// Non-recursive, because a session lives in one folder and descending into
/// subfolders would mix calibration frames in with lights. Hidden files are
/// skipped, as are anything without a FITS extension. Entries that cannot be
/// read are skipped rather than failing the whole scan, so one bad file does
/// not make the folder unopenable.
///
/// # Errors
///
/// Returns the underlying error if the folder itself cannot be listed.
pub fn scan_folder(dir: &Path) -> std::io::Result<Folder> {
    let mut files = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();

        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Hidden files include our own sidecar, so this must come first.
        if name.starts_with('.') {
            continue;
        }
        if !is_fits_path(&path) {
            continue;
        }
        // A folder named `something.fits` is not a file to open.
        match entry.file_type() {
            Ok(t) if t.is_dir() => continue,
            Err(_) => continue,
            Ok(_) => {}
        }

        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        files.push(FileEntry {
            name: name.to_string(),
            path,
            size,
            flagged: false,
            quality: None,
        });
    }

    files.sort_by(|a, b| natsort::natural_cmp(&a.name, &b.name));

    // Keep flags live beside the images, so they survive a restart and travel
    // with the folder if it is copied elsewhere.
    let flags = sidecar::load(dir);
    for entry in &mut files {
        entry.flagged = flags.is_flagged(&entry.name);
    }

    let selected = if files.is_empty() { None } else { Some(0) };
    Ok(Folder {
        dir: dir.to_path_buf(),
        files,
        selected,
    })
}

impl Folder {
    /// Re-sorts the file list into natural name order.
    ///
    /// Needed after a rename, since the new name may belong elsewhere.
    pub fn sort(&mut self) {
        self.files
            .sort_by(|a, b| natsort::natural_cmp(&a.name, &b.name));
    }

    /// Re-orders the list by a measure, keeping the selection on the same file.
    ///
    /// Files not yet measured sort last, in name order among themselves, since
    /// an unmeasured frame is not evidence of anything and should not displace
    /// one that has been looked at.
    pub fn sort_by(&mut self, key: SortKey) {
        let selected = self.selected_path().map(Path::to_path_buf);

        match key {
            SortKey::Name => self.sort(),
            _ => self.files.sort_by(|a, b| {
                match (key.value_of(a), key.value_of(b)) {
                    (Some(x), Some(y)) => x
                        .partial_cmp(&y)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        // Ties, and they happen, fall back to the name so the
                        // order is stable rather than arbitrary.
                        .then_with(|| natsort::natural_cmp(&a.name, &b.name)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => natsort::natural_cmp(&a.name, &b.name),
                }
            }),
        }

        if let Some(path) = selected {
            self.select_path(&path);
        }
    }

    /// Records a measurement against the file at `path`.
    ///
    /// Returns whether the file was found, since a folder can be rescanned
    /// while a measurement job is still running.
    pub fn set_quality(&mut self, path: &Path, quality: Quality) -> bool {
        if let Some(entry) = self.files.iter_mut().find(|e| e.path == path) {
            entry.quality = Some(quality);
            true
        } else {
            false
        }
    }

    /// How many files have been measured.
    #[must_use]
    pub fn measured(&self) -> usize {
        self.files.iter().filter(|e| e.quality.is_some()).count()
    }

    /// The range of a measure that counts as ordinary for this folder.
    ///
    /// `None` until enough files have been measured for a comparison to mean
    /// anything: with two or three frames, every one of them is an outlier.
    #[must_use]
    pub fn usual_range(&self, key: SortKey) -> Option<UsualRange> {
        let mut values: Vec<f64> = self
            .files
            .iter()
            .filter_map(|e| key.value_of(e))
            .filter(|v| v.is_finite())
            .collect();
        if values.len() < 5 {
            return None;
        }

        let middle = values.len() / 2;
        values.select_nth_unstable_by(middle, |a, b| {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        });
        let median = values[middle];

        let mut deviations: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();
        deviations.select_nth_unstable_by(middle, |a, b| {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        });
        // The same scaling the stretch uses, turning a median deviation into a
        // standard-deviation equivalent.
        let spread = deviations[middle] * 1.482_602_218_505_602;
        if spread <= 0.0 {
            return None;
        }

        Some(UsualRange {
            low: median - OUTLIER_DEVIATIONS * spread,
            high: median + OUTLIER_DEVIATIONS * spread,
        })
    }

    /// Whether a file stands out from the rest of the folder by this measure.
    ///
    /// Advisory only. **Nothing is ever deleted or flagged because of it**; the
    /// tool points, the user decides.
    #[must_use]
    pub fn is_unusual(&self, entry: &FileEntry, key: SortKey, range: Option<UsualRange>) -> bool {
        let (Some(value), Some(range)) = (key.value_of(entry), range) else {
            return false;
        };
        value < range.low || value > range.high
    }

    /// Number of files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the folder holds no FITS files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The selected entry, if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.selected.and_then(|i| self.files.get(i))
    }

    /// The selected file's path, if any.
    #[must_use]
    pub fn selected_path(&self) -> Option<&Path> {
        self.selected_entry().map(|e| e.path.as_path())
    }

    /// Selects an index, ignoring one that is out of range.
    pub fn select(&mut self, index: usize) {
        if index < self.files.len() {
            self.selected = Some(index);
        }
    }

    /// Selects the file at `path`, if the folder contains it.
    ///
    /// Returns whether it was found. Used when opening a single file, which
    /// also loads its folder so the arrow keys work immediately.
    pub fn select_path(&mut self, path: &Path) -> bool {
        if let Some(i) = self.files.iter().position(|e| e.path == path) {
            self.selected = Some(i);
            true
        } else {
            false
        }
    }

    /// Moves to the next file. Stops at the end rather than wrapping, so that
    /// holding the key down does not silently start a second pass.
    pub fn select_next(&mut self) {
        if let Some(i) = self.selected {
            if i + 1 < self.files.len() {
                self.selected = Some(i + 1);
            }
        }
    }

    /// Moves to the previous file. Stops at the start.
    pub fn select_previous(&mut self) {
        if let Some(i) = self.selected {
            if i > 0 {
                self.selected = Some(i - 1);
            }
        }
    }

    /// Selects the first file.
    pub fn select_first(&mut self) {
        if !self.files.is_empty() {
            self.selected = Some(0);
        }
    }

    /// Selects the last file.
    pub fn select_last(&mut self) {
        if !self.files.is_empty() {
            self.selected = Some(self.files.len() - 1);
        }
    }

    /// The paths worth having in memory around the current selection, in the
    /// order they should be fetched: the selected file first, then the next,
    /// then the previous.
    ///
    /// Forward comes before backward because culling runs forwards, so the next
    /// file is far more likely to be wanted than the previous one.
    #[must_use]
    pub fn prefetch_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Some(i) = self.selected else {
            return out;
        };
        for index in [Some(i), i.checked_add(1), i.checked_sub(1)] {
            if let Some(entry) = index.and_then(|n| self.files.get(n)) {
                if !out.contains(&entry.path) {
                    out.push(entry.path.clone());
                }
            }
        }
        out
    }

    /// A `3 / 142` style position label for the toolbar.
    #[must_use]
    pub fn position_label(&self) -> String {
        match self.selected {
            Some(i) => format!("{} / {}", i + 1, self.files.len()),
            None => format!("0 / {}", self.files.len()),
        }
    }

    /// Replaces the file list with a fresh scan, keeping the selection on the
    /// same file where possible.
    ///
    /// If the selected file has gone, the selection stays at the same position
    /// in the list, which is what a user expects after deleting a file.
    ///
    /// # Errors
    ///
    /// Returns the underlying error if the folder cannot be listed.
    pub fn rescan(&mut self) -> std::io::Result<()> {
        let previous = self.selected_path().map(Path::to_path_buf);
        let index = self.selected.unwrap_or(0);
        let fresh = scan_folder(&self.dir)?;
        self.files = fresh.files;

        self.selected = if self.files.is_empty() {
            None
        } else if let Some(p) = previous {
            self.files
                .iter()
                .position(|e| e.path == p)
                .or(Some(index.min(self.files.len() - 1)))
        } else {
            Some(0)
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::testutil::{write_synthetic, SyntheticSpec};
    use fits_core::Quality;
    use tempfile::TempDir;

    /// Creates a folder holding the named FITS files plus some decoys.
    fn folder_with(names: &[&str]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(2, 2, 16);
        for n in names {
            write_synthetic(dir.path(), n, &spec, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        }
        dir
    }

    #[test]
    fn only_fits_files_are_listed() {
        let dir = folder_with(&["a.fits", "b.fit", "c.fts"]);
        std::fs::write(dir.path().join("notes.txt"), b"text").unwrap();
        std::fs::write(dir.path().join("image.png"), b"png").unwrap();
        std::fs::write(dir.path().join("archive.fits.gz"), b"gz").unwrap();
        std::fs::write(dir.path().join("noextension"), b"x").unwrap();

        let f = scan_folder(dir.path()).unwrap();
        let names: Vec<&str> = f.files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.fits", "b.fit", "c.fts"]);
    }

    #[test]
    fn extension_matching_ignores_case() {
        let dir = folder_with(&["UPPER.FITS", "Mixed.Fit"]);
        let f = scan_folder(dir.path()).unwrap();
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn hidden_files_are_skipped() {
        let dir = folder_with(&["visible.fits"]);
        std::fs::write(dir.path().join(".hidden.fits"), b"x").unwrap();
        std::fs::write(dir.path().join(".fitsview.json"), b"{}").unwrap();

        let f = scan_folder(dir.path()).unwrap();
        let names: Vec<&str> = f.files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["visible.fits"]);
    }

    #[test]
    fn a_directory_named_like_a_fits_file_is_not_listed() {
        let dir = folder_with(&["real.fits"]);
        std::fs::create_dir(dir.path().join("subfolder.fits")).unwrap();
        let f = scan_folder(dir.path()).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f.files[0].name, "real.fits");
    }

    #[test]
    fn scanning_is_not_recursive() {
        let dir = folder_with(&["top.fits"]);
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let spec = SyntheticSpec::new(2, 2, 16);
        write_synthetic(&sub, "nested.fits", &spec, &[1.0, 2.0, 3.0, 4.0]).unwrap();

        let f = scan_folder(dir.path()).unwrap();
        assert_eq!(f.len(), 1, "subfolders must not be descended into");
    }

    #[test]
    fn files_are_listed_in_natural_order() {
        let dir = folder_with(&[
            "light_10.fits",
            "light_2.fits",
            "Light_1.fits",
            "dark_1.fits",
        ]);
        let f = scan_folder(dir.path()).unwrap();
        let names: Vec<&str> = f.files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "dark_1.fits",
                "Light_1.fits",
                "light_2.fits",
                "light_10.fits"
            ]
        );
    }

    #[test]
    fn sizes_are_recorded() {
        let dir = folder_with(&["a.fits"]);
        let f = scan_folder(dir.path()).unwrap();
        assert!(f.files[0].size > 0);
    }

    #[test]
    fn an_empty_folder_has_no_selection() {
        let dir = tempfile::tempdir().unwrap();
        let f = scan_folder(dir.path()).unwrap();
        assert!(f.is_empty());
        assert_eq!(f.selected, None);
        assert!(f.selected_entry().is_none());
        assert_eq!(f.position_label(), "0 / 0");
        assert!(f.prefetch_paths().is_empty());
    }

    #[test]
    fn a_missing_folder_is_an_error_not_a_panic() {
        assert!(scan_folder(Path::new("/definitely/not/here")).is_err());
    }

    #[test]
    fn the_first_file_is_selected_after_a_scan() {
        let dir = folder_with(&["b.fits", "a.fits"]);
        let f = scan_folder(dir.path()).unwrap();
        assert_eq!(f.selected, Some(0));
        assert_eq!(f.selected_entry().unwrap().name, "a.fits");
    }

    /// A folder model built without touching the filesystem.
    fn fake(n: usize) -> Folder {
        Folder {
            dir: PathBuf::from("/tmp/x"),
            files: (0..n)
                .map(|i| FileEntry {
                    path: PathBuf::from(format!("/tmp/x/f{i}.fits")),
                    name: format!("f{i}.fits"),
                    size: 100,
                    flagged: false,
                    quality: None,
                })
                .collect(),
            selected: if n == 0 { None } else { Some(0) },
        }
    }

    #[test]
    fn selection_stops_at_the_ends_rather_than_wrapping() {
        let mut f = fake(3);
        f.select_previous();
        assert_eq!(f.selected, Some(0), "must not wrap to the end");

        f.select_last();
        assert_eq!(f.selected, Some(2));
        f.select_next();
        assert_eq!(f.selected, Some(2), "must not wrap to the start");
    }

    #[test]
    fn selection_moves_through_the_list() {
        let mut f = fake(4);
        f.select_next();
        f.select_next();
        assert_eq!(f.selected, Some(2));
        f.select_previous();
        assert_eq!(f.selected, Some(1));
        f.select_first();
        assert_eq!(f.selected, Some(0));
        f.select_last();
        assert_eq!(f.selected, Some(3));
    }

    #[test]
    fn selecting_an_out_of_range_index_is_ignored() {
        let mut f = fake(3);
        f.select(99);
        assert_eq!(f.selected, Some(0));
        f.select(2);
        assert_eq!(f.selected, Some(2));
    }

    #[test]
    fn selecting_by_path_finds_the_file() {
        let mut f = fake(3);
        assert!(f.select_path(Path::new("/tmp/x/f2.fits")));
        assert_eq!(f.selected, Some(2));
        assert!(!f.select_path(Path::new("/tmp/x/absent.fits")));
        assert_eq!(
            f.selected,
            Some(2),
            "a failed lookup must not move selection"
        );
    }

    #[test]
    fn moving_in_an_empty_folder_does_nothing() {
        let mut f = fake(0);
        f.select_next();
        f.select_previous();
        f.select_first();
        f.select_last();
        assert_eq!(f.selected, None);
    }

    #[test]
    fn the_position_label_counts_from_one() {
        let mut f = fake(142);
        f.select(2);
        assert_eq!(f.position_label(), "3 / 142");
        f.select_first();
        assert_eq!(f.position_label(), "1 / 142");
        f.select_last();
        assert_eq!(f.position_label(), "142 / 142");
    }

    #[test]
    fn prefetch_asks_for_the_selection_then_forwards_then_backwards() {
        let mut f = fake(5);
        f.select(2);
        let paths: Vec<String> = f
            .prefetch_paths()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, vec!["f2.fits", "f3.fits", "f1.fits"]);
    }

    #[test]
    fn prefetch_at_the_ends_asks_only_for_what_exists() {
        let mut f = fake(3);
        f.select_first();
        assert_eq!(f.prefetch_paths().len(), 2, "no previous file at the start");
        f.select_last();
        assert_eq!(f.prefetch_paths().len(), 2, "no next file at the end");

        let mut single = fake(1);
        single.select_first();
        assert_eq!(single.prefetch_paths().len(), 1);
    }

    /// A folder with measurements attached, built without touching the disk.
    fn measured(values: &[(f64, f64)]) -> Folder {
        let mut folder = fake(values.len());
        for (entry, (background, sharpness)) in folder.files.iter_mut().zip(values) {
            entry.quality = Some(Quality {
                background: *background,
                noise: 10.0,
                sharpness: *sharpness,
            });
        }
        folder
    }

    #[test]
    fn sorting_by_a_measure_orders_the_list_by_it() {
        let mut f = measured(&[(3000.0, 1.1), (1000.0, 2.5), (2000.0, 0.4)]);

        f.sort_by(SortKey::Background);
        let backgrounds: Vec<f64> = f
            .files
            .iter()
            .filter_map(|e| SortKey::Background.value_of(e))
            .collect();
        assert_eq!(backgrounds, vec![1000.0, 2000.0, 3000.0]);

        f.sort_by(SortKey::Sharpness);
        let sharpness: Vec<f64> = f
            .files
            .iter()
            .filter_map(|e| SortKey::Sharpness.value_of(e))
            .collect();
        assert_eq!(sharpness, vec![0.4, 1.1, 2.5]);
    }

    #[test]
    fn sorting_by_name_still_works_after_sorting_by_a_measure() {
        let mut f = measured(&[(3000.0, 1.0), (1000.0, 2.0), (2000.0, 3.0)]);
        f.sort_by(SortKey::Background);
        f.sort_by(SortKey::Name);
        let names: Vec<&str> = f.files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["f0.fits", "f1.fits", "f2.fits"]);
    }

    #[test]
    fn sorting_keeps_the_selection_on_the_same_file() {
        let mut f = measured(&[(3000.0, 1.0), (1000.0, 2.0), (2000.0, 3.0)]);
        f.select(0);
        let before = f.selected_entry().unwrap().name.clone();

        f.sort_by(SortKey::Background);
        assert_eq!(
            f.selected_entry().unwrap().name,
            before,
            "the selection should follow the file, not the position"
        );
        assert_eq!(f.selected, Some(2), "which has moved to the end");
    }

    #[test]
    fn unmeasured_files_sort_last_rather_than_first() {
        // An unmeasured frame is not evidence of anything and must not displace
        // one that has been looked at.
        let mut f = measured(&[(3000.0, 1.0), (1000.0, 2.0), (2000.0, 3.0)]);
        f.files[1].quality = None;

        f.sort_by(SortKey::Background);
        assert!(f.files[0].quality.is_some());
        assert!(f.files[1].quality.is_some());
        assert!(f.files[2].quality.is_none(), "the unmeasured one goes last");
    }

    #[test]
    fn ties_fall_back_to_the_name_so_the_order_is_stable() {
        let mut f = measured(&[(1000.0, 1.0), (1000.0, 1.0), (1000.0, 1.0)]);
        f.sort_by(SortKey::Background);
        let names: Vec<&str> = f.files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["f0.fits", "f1.fits", "f2.fits"]);
    }

    #[test]
    fn a_frame_unlike_the_rest_of_the_folder_is_marked() {
        // Nine ordinary frames and one taken through cloud.
        let mut values: Vec<(f64, f64)> = (0..9).map(|i| (1000.0 + f64::from(i), 2.0)).collect();
        values.push((9000.0, 2.0));
        let f = measured(&values);

        let range = f.usual_range(SortKey::Background);
        assert!(range.is_some(), "ten frames is enough to compare");
        assert!(
            f.is_unusual(&f.files[9], SortKey::Background, range),
            "the cloudy frame should stand out"
        );
        for ordinary in &f.files[..9] {
            assert!(
                !f.is_unusual(ordinary, SortKey::Background, range),
                "{} should not be marked",
                ordinary.name
            );
        }
    }

    #[test]
    fn a_folder_too_small_to_compare_marks_nothing() {
        // With three frames every one of them is an outlier, which is useless.
        let f = measured(&[(1000.0, 1.0), (5000.0, 2.0), (9000.0, 3.0)]);
        assert_eq!(f.usual_range(SortKey::Background), None);
        assert!(!f.is_unusual(&f.files[1], SortKey::Background, None));
    }

    #[test]
    fn a_folder_of_identical_frames_marks_nothing() {
        // No spread means no basis for calling anything unusual.
        let f = measured(&[(1000.0, 1.0); 8]);
        assert_eq!(f.usual_range(SortKey::Background), None);
    }

    #[test]
    fn marking_a_frame_never_changes_it() {
        // The tool points; the user decides. Nothing is deleted or flagged.
        let mut values: Vec<(f64, f64)> = (0..9).map(|i| (1000.0 + f64::from(i), 2.0)).collect();
        values.push((9000.0, 2.0));
        let f = measured(&values);
        let range = f.usual_range(SortKey::Background);

        for entry in &f.files {
            let _ = f.is_unusual(entry, SortKey::Background, range);
        }
        assert_eq!(f.len(), 10, "no file was removed");
        assert!(
            f.files.iter().all(|e| !e.flagged),
            "no file was flagged either"
        );
    }

    #[test]
    fn a_measurement_can_be_recorded_against_a_file() {
        let mut f = fake(3);
        assert_eq!(f.measured(), 0);
        let q = Quality {
            background: 500.0,
            noise: 5.0,
            sharpness: 1.2,
        };
        assert!(f.set_quality(Path::new("/tmp/x/f1.fits"), q));
        assert_eq!(f.measured(), 1);
        assert_eq!(f.files[1].quality, Some(q));

        assert!(
            !f.set_quality(Path::new("/tmp/x/gone.fits"), q),
            "a file that has since disappeared is reported, not panicked over"
        );
    }

    #[test]
    fn rescan_keeps_the_selection_on_the_same_file() {
        let dir = folder_with(&["a.fits", "b.fits", "c.fits"]);
        let mut f = scan_folder(dir.path()).unwrap();
        f.select(2);
        assert_eq!(f.selected_entry().unwrap().name, "c.fits");

        // A new file sorts before the selection, shifting its index.
        let spec = SyntheticSpec::new(2, 2, 16);
        write_synthetic(dir.path(), "aa.fits", &spec, &[1.0, 2.0, 3.0, 4.0]).unwrap();

        f.rescan().unwrap();
        assert_eq!(f.len(), 4);
        assert_eq!(
            f.selected_entry().unwrap().name,
            "c.fits",
            "selection should follow the file, not the index"
        );
    }

    #[test]
    fn rescan_after_the_selected_file_disappears_keeps_the_position() {
        let dir = folder_with(&["a.fits", "b.fits", "c.fits"]);
        let mut f = scan_folder(dir.path()).unwrap();
        f.select(1);

        std::fs::remove_file(dir.path().join("b.fits")).unwrap();
        f.rescan().unwrap();

        assert_eq!(f.len(), 2);
        assert_eq!(
            f.selected_entry().unwrap().name,
            "c.fits",
            "should land on whatever took the deleted file's place"
        );
    }

    #[test]
    fn rescan_of_a_folder_emptied_completely_clears_the_selection() {
        let dir = folder_with(&["only.fits"]);
        let mut f = scan_folder(dir.path()).unwrap();
        std::fs::remove_file(dir.path().join("only.fits")).unwrap();
        f.rescan().unwrap();
        assert!(f.is_empty());
        assert_eq!(f.selected, None);
    }

    #[test]
    fn rescan_picks_up_new_files() {
        let dir = folder_with(&["a.fits"]);
        let mut f = scan_folder(dir.path()).unwrap();
        assert_eq!(f.len(), 1);

        let spec = SyntheticSpec::new(2, 2, 16);
        write_synthetic(dir.path(), "b.fits", &spec, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        f.rescan().unwrap();
        assert_eq!(f.len(), 2);
    }
}
