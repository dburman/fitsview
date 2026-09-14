//! Measurements remembered beside the images.
//!
//! Measuring a folder means reading every frame, and finding its stars means
//! searching each one: twenty seconds for a night's work. None of those numbers
//! change unless the file does, so they are kept in a hidden file beside the
//! frames and read back when the folder is opened again.
//!
//! They are kept apart from the keep flags in [`crate::sidecar`], for three
//! reasons:
//!
//! 1. **They belong to the folder a frame is in**, not the one that was opened.
//!    A night measured by opening its target is still measured when the night
//!    itself is opened, or its filter.
//! 2. **They can always be worked out again**, so a damaged or discarded file
//!    costs time and never loses anything. Keep flags cannot be recovered.
//! 3. **There are hundreds of them**, which would bury the handful of settings
//!    someone might open the other file to read.
//!
//! A measurement is trusted only while the file is the size and age it was when
//! measured, and only by the release that wrote it: 0.1.7 changed every star
//! width, and figures from before that must not be shown as current.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use fits_core::{DetectionParams, Quality};
use serde::{Deserialize, Serialize};

use crate::folder::{Folder, StarMeasure};

/// Name of the file. Hidden, so folder scanning skips it already.
pub const MEASUREMENTS_NAME: &str = ".fitsview-measurements.json";

/// What a file looked like on disk when it was listed.
///
/// A file whose size or modification time differs from what was recorded has
/// been changed, and its measurements belong to what it used to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// Length in bytes.
    pub size: u64,
    /// Last modified, as whole seconds since 1970.
    pub modified_secs: u64,
    /// And the nanoseconds past that, where the filesystem keeps them.
    pub modified_nanos: u32,
}

impl Stamp {
    /// The stamp for a file's metadata, or `None` if the system cannot say
    /// when it was modified.
    #[must_use]
    pub fn of(metadata: &std::fs::Metadata) -> Option<Self> {
        let since = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            size: metadata.len(),
            modified_secs: since.as_secs(),
            modified_nanos: since.subsec_nanos(),
        })
    }

    /// The stamp of the file at `path` as it is now.
    #[must_use]
    pub fn read(path: &Path) -> Option<Self> {
        Self::of(&std::fs::metadata(path).ok()?)
    }
}

/// A number standing for a set of detection settings.
///
/// Star figures found at one threshold are not those found at another, so each
/// is recorded with the settings that produced it. A hash of the settings'
/// printed form, which is stable within a release, and releases do not trust
/// each other's figures anyway.
#[must_use]
pub fn settings_of(params: &DetectionParams) -> u64 {
    // FNV-1a: fixed, unlike the standard library's hasher, which is free to
    // change between compiler versions.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in format!("{params:?}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The file as stored.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
struct Stored {
    /// The release that wrote it. Nothing is trusted from any other.
    version: String,
    /// Measurements by file name within the folder.
    #[serde(default)]
    frames: BTreeMap<String, Frame>,
}

/// One frame's measurements, with the stamp of the file they came from.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Frame {
    #[serde(flatten)]
    stamp: Stamp,
    background: f64,
    noise: f64,
    sharpness: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stars: Option<Stars>,
}

/// A frame's star figures as stored.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Stars {
    fwhm: Option<f64>,
    fwhm_arcsec: Option<f64>,
    roundness: Option<f64>,
    count: usize,
    settings: u64,
}

/// The release doing the reading and writing.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the measurements for the frames in `dir` are kept.
#[must_use]
pub fn path_for(dir: &Path) -> PathBuf {
    dir.join(MEASUREMENTS_NAME)
}

/// Reads what is stored for `dir`, or nothing if it is missing, damaged or
/// written by another release.
fn load(dir: &Path) -> Stored {
    let path = path_for(dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Stored::default();
    };
    match serde_json::from_str::<Stored>(&text) {
        Ok(stored) if stored.version == VERSION => stored,
        Ok(stored) => {
            log::debug!(
                "measuring {} again: figures are from {}",
                dir.display(),
                stored.version
            );
            Stored::default()
        }
        Err(e) => {
            log::warn!("ignoring damaged {}: {e}", path.display());
            Stored::default()
        }
    }
}

/// Fills in whatever was measured before and has not changed since.
///
/// Each file is looked up in the folder it sits in, so a folder opened from
/// above finds what was measured when it was opened directly, and the other
/// way round. A file changed since it was measured is left blank, and a figure
/// already known is left as it is.
pub fn recall(folder: &mut Folder) {
    let mut by_dir: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (index, entry) in folder.files.iter().enumerate() {
        if let Some(dir) = entry.path.parent() {
            by_dir.entry(dir.to_path_buf()).or_default().push(index);
        }
    }

    for (dir, indices) in by_dir {
        let stored = load(&dir);
        if stored.frames.is_empty() {
            continue;
        }
        for index in indices {
            let entry = &mut folder.files[index];
            let Some(frame) = stored.frames.get(entry.file_name()) else {
                continue;
            };
            if entry.stamp != Some(frame.stamp) {
                continue;
            }
            entry.quality = entry.quality.or(Some(Quality {
                background: frame.background,
                noise: frame.noise,
                sharpness: frame.sharpness,
            }));
            entry.stars = entry.stars.or(frame.stars.map(|s| StarMeasure {
                fwhm: s.fwhm,
                fwhm_arcsec: s.fwhm_arcsec,
                roundness: s.roundness,
                count: s.count,
                settings: s.settings,
            }));
        }
    }
}

/// Records every measurement the folder holds beside the frames it came from.
///
/// A file changed since the folder was listed is not recorded, because what was
/// measured may have been what it used to be. Entries for files that have gone
/// are dropped, so the record does not grow for ever.
///
/// Failure is logged rather than reported: nothing is lost by it, and a folder
/// on a read-only volume would otherwise complain after every measurement.
pub fn remember(folder: &Folder) {
    let mut by_dir: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (index, entry) in folder.files.iter().enumerate() {
        if entry.quality.is_some() {
            if let Some(dir) = entry.path.parent() {
                by_dir.entry(dir.to_path_buf()).or_default().push(index);
            }
        }
    }

    for (dir, indices) in by_dir {
        let before = load(&dir);
        let mut after = Stored {
            version: VERSION.to_string(),
            frames: before.frames.clone(),
        };

        for index in indices {
            let entry = &folder.files[index];
            let name = entry.file_name().to_string();
            let frame = entry
                .stamp
                .filter(|&stamp| Stamp::read(&entry.path) == Some(stamp))
                .and_then(|stamp| frame_of(stamp, entry.quality?, entry.stars));
            match frame {
                Some(frame) => after.frames.insert(name, frame),
                None => after.frames.remove(&name),
            };
        }

        let present: BTreeSet<String> = after
            .frames
            .keys()
            .filter(|name| dir.join(name.as_str()).is_file())
            .cloned()
            .collect();
        after.frames.retain(|name, _| present.contains(name));

        if after == before {
            continue;
        }
        if let Err(e) = save(&dir, &after) {
            log::debug!("could not record measurements in {}: {e}", dir.display());
        }
    }
}

/// A frame as stored, or `None` if a number could not be written.
///
/// JSON has no way to write infinity or not-a-number, and a file that would not
/// read back would lose every other frame's figures with it.
fn frame_of(stamp: Stamp, quality: Quality, stars: Option<StarMeasure>) -> Option<Frame> {
    let finite = |v: f64| v.is_finite();
    if !(finite(quality.background) && finite(quality.noise) && finite(quality.sharpness)) {
        return None;
    }
    let only_finite = |v: Option<f64>| v.filter(|v| v.is_finite());
    Some(Frame {
        stamp,
        background: quality.background,
        noise: quality.noise,
        sharpness: quality.sharpness,
        stars: stars.map(|s| Stars {
            fwhm: only_finite(s.fwhm),
            fwhm_arcsec: only_finite(s.fwhm_arcsec),
            roundness: only_finite(s.roundness),
            count: s.count,
            settings: s.settings,
        }),
    })
}

/// Writes the file atomically, or removes it when there is nothing to keep.
fn save(dir: &Path, stored: &Stored) -> std::io::Result<()> {
    let target = path_for(dir);
    if stored.frames.is_empty() {
        return match std::fs::remove_file(&target) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }

    // Compact rather than pretty: it is read by this program, not by people,
    // and a night's worth is a quarter of the size this way.
    let text = serde_json::to_string(stored)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let temp = dir.join(".fitsview-measurements.json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, &target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::scan_folder;
    use fits_core::testutil::{write_synthetic, SyntheticSpec};
    use std::time::{Duration, SystemTime};

    fn write(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join(name).parent().unwrap()).unwrap();
        let spec = SyntheticSpec::new(2, 2, 16);
        write_synthetic(dir, name, &spec, &[1.0, 2.0, 3.0, 4.0]).unwrap();
    }

    fn quality(background: f64) -> Quality {
        Quality {
            background,
            noise: 3.0,
            sharpness: 1.5,
        }
    }

    fn stars(fwhm: f64) -> StarMeasure {
        StarMeasure {
            fwhm: Some(fwhm),
            fwhm_arcsec: Some(fwhm * 1.795),
            roundness: Some(0.9),
            count: 1234,
            settings: settings_of(&DetectionParams::default()),
        }
    }

    /// Scans `dir`, gives every frame figures, and records them.
    fn measure(dir: &Path) -> Folder {
        let mut folder = scan_folder(dir).unwrap();
        for (i, entry) in folder.files.iter_mut().enumerate() {
            let i = i as f64;
            entry.quality = Some(quality(100.0 + i));
            entry.stars = Some(stars(2.0 + i));
        }
        remember(&folder);
        folder
    }

    fn set_modified(path: &Path, when: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[test]
    fn a_measured_folder_is_measured_when_opened_again() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        write(dir.path(), "b.fits");
        let measured = measure(dir.path());

        let reopened = scan_folder(dir.path()).unwrap();
        assert_eq!(reopened.files.len(), 2);
        for (was, is) in measured.files.iter().zip(&reopened.files) {
            assert_eq!(is.quality, was.quality, "{}", is.name);
            assert_eq!(is.stars, was.stars, "{}", is.name);
        }
    }

    #[test]
    fn a_frame_changed_since_it_was_measured_is_measured_again() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        write(dir.path(), "b.fits");
        measure(dir.path());

        // Same size, a different time: as when a frame is recalibrated in place.
        set_modified(
            &dir.path().join("a.fits"),
            SystemTime::now() + Duration::from_secs(3600),
        );

        let reopened = scan_folder(dir.path()).unwrap();
        let a = reopened.files.iter().find(|e| e.name == "a.fits").unwrap();
        let b = reopened.files.iter().find(|e| e.name == "b.fits").unwrap();
        assert!(
            a.quality.is_none() && a.stars.is_none(),
            "stale figures kept"
        );
        assert!(b.quality.is_some(), "the untouched frame lost its figures");
    }

    #[test]
    fn a_frame_changed_after_listing_is_not_recorded() {
        // Its figures may be of what it was before; recording them against
        // what it is now would make them look current for ever.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        let mut folder = scan_folder(dir.path()).unwrap();
        folder.files[0].quality = Some(quality(100.0));
        set_modified(
            &dir.path().join("a.fits"),
            SystemTime::now() + Duration::from_secs(3600),
        );
        remember(&folder);

        assert!(!path_for(dir.path()).exists());
    }

    #[test]
    fn figures_from_another_release_are_not_trusted() {
        // 0.1.7 changed every star width; a later release opening a folder
        // measured by an earlier one must not show the old numbers.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        measure(dir.path());

        let path = path_for(dir.path());
        let text = std::fs::read_to_string(&path).unwrap();
        let old = text.replace(
            &format!("\"version\":\"{VERSION}\""),
            "\"version\":\"0.0.1\"",
        );
        assert_ne!(text, old, "the version should be recorded");
        std::fs::write(&path, old).unwrap();

        let reopened = scan_folder(dir.path()).unwrap();
        assert!(reopened.files[0].quality.is_none());
        assert!(reopened.files[0].stars.is_none());
    }

    #[test]
    fn a_damaged_record_is_ignored_rather_than_failing_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        std::fs::write(path_for(dir.path()), b"{\"version\": ").unwrap();

        let folder = scan_folder(dir.path()).unwrap();
        assert_eq!(folder.files.len(), 1);
        assert!(folder.files[0].quality.is_none());

        // And the next measurement replaces it.
        let mut folder = folder;
        folder.files[0].quality = Some(quality(5.0));
        remember(&folder);
        let again = scan_folder(dir.path()).unwrap();
        assert_eq!(again.files[0].quality, Some(quality(5.0)));
    }

    #[test]
    fn figures_are_found_whichever_level_the_folder_is_opened_from() {
        // Measured by opening the target; then the night is opened by itself.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "2026-09-01/L/a.fits");
        write(dir.path(), "2026-09-02/L/a.fits");
        let measured = measure(dir.path());

        assert!(
            path_for(&dir.path().join("2026-09-01/L")).exists(),
            "kept beside the frames, not in the folder that was opened"
        );
        assert!(!path_for(dir.path()).exists());

        let night = scan_folder(&dir.path().join("2026-09-02")).unwrap();
        let from_target = measured
            .files
            .iter()
            .find(|e| e.name == "2026-09-02/L/a.fits")
            .unwrap();
        assert_eq!(night.files[0].name, "L/a.fits");
        assert_eq!(night.files[0].quality, from_target.quality);
        assert_eq!(night.files[0].stars, from_target.stars);
    }

    #[test]
    fn frames_that_have_gone_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        write(dir.path(), "b.fits");
        measure(dir.path());

        std::fs::remove_file(dir.path().join("a.fits")).unwrap();
        measure(dir.path());

        let text = std::fs::read_to_string(path_for(dir.path())).unwrap();
        assert!(!text.contains("a.fits"), "{text}");
        assert!(text.contains("b.fits"), "{text}");
    }

    #[test]
    fn a_rename_carries_the_figures_to_the_new_name() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        let mut folder = measure(dir.path());

        let to = dir.path().join("renamed.fits");
        std::fs::rename(&folder.files[0].path, &to).unwrap();
        folder.files[0].path = to;
        folder.files[0].name = "renamed.fits".into();
        remember(&folder);

        let text = std::fs::read_to_string(path_for(dir.path())).unwrap();
        assert!(!text.contains("\"a.fits\""), "{text}");
        let reopened = scan_folder(dir.path()).unwrap();
        assert_eq!(reopened.files[0].quality, Some(quality(100.0)));
    }

    #[test]
    fn a_number_json_cannot_hold_does_not_lose_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        write(dir.path(), "b.fits");
        let mut folder = scan_folder(dir.path()).unwrap();
        folder.files[0].quality = Some(quality(f64::NAN));
        folder.files[1].quality = Some(quality(7.0));
        folder.files[1].stars = Some(StarMeasure {
            roundness: Some(f64::INFINITY),
            ..stars(3.0)
        });
        remember(&folder);

        let reopened = scan_folder(dir.path()).unwrap();
        assert!(reopened.files[0].quality.is_none());
        assert_eq!(reopened.files[1].quality, Some(quality(7.0)));
        let s = reopened.files[1].stars.unwrap();
        assert_eq!(s.fwhm, Some(3.0));
        assert_eq!(s.roundness, None);
    }

    #[test]
    fn nothing_measured_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        remember(&scan_folder(dir.path()).unwrap());
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.fits"]);
    }

    #[test]
    fn the_record_is_not_listed_as_a_frame() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        measure(dir.path());
        assert!(MEASUREMENTS_NAME.starts_with('.'));
        assert_eq!(scan_folder(dir.path()).unwrap().files.len(), 1);
    }

    #[test]
    fn remembering_the_same_figures_again_does_not_rewrite_the_file() {
        // Otherwise every measurement touches every folder, and a synced
        // folder uploads them all again.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        let folder = measure(dir.path());
        let path = path_for(dir.path());
        let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        set_modified(&path, long_ago);

        remember(&folder);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            long_ago
        );
    }

    #[test]
    fn figures_come_back_to_the_last_bit() {
        // Without exact float parsing this came back one bit out, which is
        // harmless to look at but means a remembered figure is not the figure.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.fits");
        let mut folder = scan_folder(dir.path()).unwrap();
        folder.files[0].quality = Some(Quality {
            background: 21.0,
            noise: 13.343_419_966_550_417,
            sharpness: 154.818_160_641_131_1,
        });
        remember(&folder);
        assert_eq!(
            scan_folder(dir.path()).unwrap().files[0].quality,
            folder.files[0].quality
        );
    }

    #[test]
    fn different_settings_are_told_apart() {
        let default = DetectionParams::default();
        let stricter = DetectionParams {
            threshold: default.threshold + 1.0,
            ..default
        };
        assert_eq!(settings_of(&default), settings_of(&default));
        assert_ne!(settings_of(&default), settings_of(&stricter));
    }
}
