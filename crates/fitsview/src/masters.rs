//! Masters made from a calibration library's frames, and kept for next time.
//!
//! A master dark is the average of many darks, so that the sensor's own signal
//! can be taken off a light without adding the noise of any single dark. Made
//! the way masters used to be — every frame held at once and the median taken —
//! forty 61-megapixel darks need ten gigabytes. Here they are combined a frame
//! at a time by the stack's own outlier-rejecting average, reading each frame
//! twice: two gigabytes however many there are, and less noise than a median,
//! which throws away a fifth of what the frames know.
//!
//! A master is kept in a hidden folder inside the library, named for what it
//! is and for a fingerprint of the frames it came from — every file's path,
//! size and time of change. A set that gains a frame, loses one or has one
//! rewritten no longer matches its master, and a fresh one is made.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use fits_core::calib::{flat_from_combined, MasterFlat, MasterFrame};
use fits_core::stack::{Alignment, Stack, DEFAULT_CLIP};
use fits_core::{read_fits, write_fits, FitsHeader};

use crate::library::{Kind, Set};
use crate::measurements::Stamp;

/// The hidden folder masters are kept in, inside the library.
pub const MASTERS_FOLDER: &str = ".fitsview-masters";

/// How masters are made. Changed when that changes, so that masters made the
/// old way are made again rather than trusted.
const RECIPE: u32 = 1;

/// Why a master could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failed {
    /// It was asked to stop.
    Cancelled,
    /// Something about the frames, in words the user can act on.
    Because(String),
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("stopped"),
            Self::Because(why) => f.write_str(why),
        }
    }
}

/// Where a master came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Read back from the library, as made before.
    Kept,
    /// Made now, from the frames.
    Made,
}

/// Reports progress: frames done, frames in all.
pub type Progress<'a> = &'a mut dyn FnMut(usize, usize);

/// Combines a set's frames into one master: the average of them all, leaving
/// out what a cosmic ray or a satellite put into any one of them.
///
/// Frames that cannot be read, or are not the size of the first that can, are
/// passed over. Fewer than [`fits_core::stack::MINIMUM_TO_CLIP`] frames are
/// simply averaged: there are too few to say what is out of the ordinary.
///
/// # Errors
///
/// [`Failed::Cancelled`] if `cancel` is set, and [`Failed::Because`] if no
/// frame of the set can be read.
pub fn combine(set: &Set, cancel: &AtomicBool, progress: Progress) -> Result<MasterFrame, Failed> {
    let total = set.frames.len();
    let passes = if total >= fits_core::stack::MINIMUM_TO_CLIP {
        2
    } else {
        1
    };
    let steps = total * passes;
    let mut done = 0;

    let mut stack: Option<Stack> = None;
    let mut used: Vec<&Path> = Vec::new();
    for path in &set.frames {
        if cancel.load(Ordering::Relaxed) {
            return Err(Failed::Cancelled);
        }
        progress(done, steps);
        done += 1;
        let Ok(image) = read_fits(path) else {
            log::warn!("passing over {}: it cannot be read", path.display());
            continue;
        };
        let stack = stack
            .get_or_insert_with(|| Stack::rejecting(image.width, image.height, image.channels));
        if stack.add(&image, Alignment::still()) {
            used.push(path);
        } else {
            log::warn!(
                "passing over {}: it is not the size of the rest",
                path.display()
            );
        }
    }
    let Some(stack) = stack.filter(|s| s.frames() > 0) else {
        return Err(Failed::Because(format!(
            "none of the {total} {}s could be read",
            set.taken.kind.name()
        )));
    };
    let frames = stack.frames();

    let image = match stack.into_rejecting(DEFAULT_CLIP) {
        Ok(mut second) => {
            for path in &used {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Failed::Cancelled);
                }
                progress(done, steps);
                done += 1;
                if let Ok(image) = read_fits(path) {
                    second.add(&image, Alignment::still());
                }
            }
            second.finish(FitsHeader::default())
        }
        Err(stack) => stack.finish(FitsHeader::default()),
    };
    progress(steps, steps);

    Ok(MasterFrame {
        width: image.width,
        height: image.height,
        channels: image.channels,
        data: image.data,
        source_count: frames,
        exptime: set.taken.exposure,
        temperature: set.taken.cooled_to(),
    })
}

/// The masters kept in one library.
#[derive(Debug, Clone)]
pub struct Masters {
    folder: PathBuf,
}

impl Masters {
    /// The masters of the library at `root`.
    #[must_use]
    pub fn of_library(root: &Path) -> Self {
        Self {
            folder: root.join(MASTERS_FOLDER),
        }
    }

    /// The master dark — or bias, or flat dark — of `set`: the one kept, if its
    /// frames are the same as when it was made, or else one made now and kept.
    ///
    /// # Errors
    ///
    /// As [`combine`].
    pub fn dark(
        &self,
        set: &Set,
        cancel: &AtomicBool,
        progress: Progress,
    ) -> Result<(MasterFrame, Origin), Failed> {
        let path = self.file_for(set, None);
        if let Some(kept) = read_fits(&path)
            .ok()
            .map(|image| MasterFrame::from_image(&image))
            .filter(|m| (m.width, m.height) == set.taken.size)
        {
            return Ok((kept, Origin::Kept));
        }
        let master = combine(set, cancel, progress)?;
        self.keep(
            &path,
            &master.to_image(),
            &format!("{} combined from {}", set.describe(), master.source_count),
        );
        Ok((master, Origin::Made))
    }

    /// The master flat of `flats`, with `flat_dark` taken off first: the one
    /// kept, if both sets' frames are the same as when it was made, or else one
    /// made now and kept.
    ///
    /// # Errors
    ///
    /// As [`combine`], and [`Failed::Because`] if the flats hold no signal or
    /// are not the size of their dark.
    pub fn flat(
        &self,
        flats: &Set,
        flat_dark: Option<&Set>,
        cancel: &AtomicBool,
        progress: Progress,
    ) -> Result<(MasterFlat, Origin), Failed> {
        let path = self.file_for(flats, flat_dark);
        if let Some(kept) = read_fits(&path)
            .ok()
            .filter(|image| image.header.get_bool("FITSVFLT") == Some(true))
            .map(|image| MasterFlat::from_image(&image))
            .filter(|m| (m.width, m.height) == flats.taken.size)
        {
            return Ok((kept, Origin::Kept));
        }
        let dark = match flat_dark {
            Some(set) => Some(self.dark(set, cancel, &mut |_, _| {})?.0),
            None => None,
        };
        let combined = combine(flats, cancel, progress)?;
        let flat = flat_from_combined(combined, dark.as_ref())
            .map_err(|e| Failed::Because(format!("the flats cannot be used: {e}")))?;
        self.keep(
            &path,
            &flat.to_image(),
            &format!("{} combined from {}", flats.describe(), flat.source_count),
        );
        Ok((flat, Origin::Made))
    }

    /// Writes a master into the library, or says why not in the log. A library
    /// that cannot be written to still gets its masters; they are just made
    /// again next time.
    fn keep(&self, path: &Path, image: &fits_core::FitsImage, history: &str) {
        let written = std::fs::create_dir_all(&self.folder)
            .map_err(|e| e.to_string())
            .and_then(|()| {
                write_fits(path, image, &[format!("fitsview: {history}")])
                    .map_err(|e| e.to_string())
            });
        if let Err(e) = written {
            log::warn!("not keeping {}: {e}", path.display());
        }
    }

    /// Where the master of `set` — with `also` taken off it, for a flat — is
    /// kept: named for what it is, and for a fingerprint of every frame that
    /// went into it.
    fn file_for(&self, set: &Set, also: Option<&Set>) -> PathBuf {
        let taken = &set.taken;
        let mut name = taken.kind.name().replace(' ', "-");
        if let Some(exposure) = taken.exposure {
            if taken.kind != Kind::Flat {
                name.push_str(&format!("-{exposure}s"));
            }
        }
        if let (Kind::Flat, Some(filter)) = (taken.kind, &taken.filter) {
            let filter: String = filter
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            name.push('-');
            name.push_str(&filter);
        }
        let mut hash = Fingerprint::new();
        hash.add(&RECIPE.to_le_bytes());
        hash.add_set(set);
        if let Some(also) = also {
            hash.add(b"with");
            hash.add_set(also);
        }
        self.folder.join(format!("{name}-{:016x}.fits", hash.0))
    }
}

/// FNV-1a over what a master was made from.
struct Fingerprint(u64);

impl Fingerprint {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn add(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    /// Every frame of a set: its path, and its size and time of change.
    fn add_set(&mut self, set: &Set) {
        self.add(set.taken.kind.name().as_bytes());
        for frame in &set.frames {
            self.add(frame.to_string_lossy().as_bytes());
            if let Some(stamp) = Stamp::read(frame) {
                self.add(&stamp.size.to_le_bytes());
                self.add(&stamp.modified_secs.to_le_bytes());
                self.add(&stamp.modified_nanos.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;
    use fits_core::testutil::{gaussian_background, write_synthetic, SyntheticSpec};
    use std::time::{Duration, SystemTime};

    const W: usize = 48;
    const H: usize = 40;

    /// Writes `count` frames of `kind` into `folder`, each made by `pixels`
    /// from its index.
    fn write_set(
        root: &Path,
        folder: &str,
        kind: &str,
        cards: &[(&str, &str)],
        count: usize,
        pixels: impl Fn(usize) -> Vec<f64>,
    ) {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..count {
            let mut spec =
                SyntheticSpec::new(W, H, -32).with_card("IMAGETYP", &format!("'{kind}'"));
            for (key, value) in cards {
                spec = spec.with_card(key, value);
            }
            write_synthetic(&dir, &format!("f{i:02}.fits"), &spec, &pixels(i)).unwrap();
        }
    }

    /// Darks: an offset of 500, three hot pixels, noise, and in one frame a
    /// cosmic ray.
    fn darks(root: &Path, count: usize) {
        write_set(
            root,
            "DARK/180",
            "DARK",
            &[("EXPTIME", "180.0"), ("SET-TEMP", "-14.0")],
            count,
            |i| {
                let mut p = gaussian_background(W, H, 500.0, 8.0, 100 + i as u64);
                for hot in [5 * W + 5, 20 * W + 30, 33 * W + 12] {
                    p[hot] += 3_000.0;
                }
                if i == 2 {
                    p[10 * W + 40] += 20_000.0;
                }
                p
            },
        );
    }

    fn only(library: &Library, kind: Kind) -> &Set {
        library.sets.iter().find(|s| s.taken.kind == kind).unwrap()
    }

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_master_dark_keeps_the_hot_pixels_and_loses_the_cosmic_ray() {
        let dir = tempfile::tempdir().unwrap();
        darks(dir.path(), 8);
        let library = Library::scan(dir.path());
        let master = combine(only(&library, Kind::Dark), &never(), &mut |_, _| {}).unwrap();

        assert_eq!(master.source_count, 8);
        assert_eq!(master.exptime, Some(180.0));
        assert_eq!(master.temperature, Some(-14.0));
        let at = |i: usize| f64::from(master.data[i]);
        assert!(
            (at(5 * W + 5) - 3_500.0).abs() < 20.0,
            "a hot pixel is part of the dark: {}",
            at(5 * W + 5)
        );
        assert!(
            (at(10 * W + 40) - 500.0).abs() < 20.0,
            "the cosmic ray is not: {}",
            at(10 * W + 40)
        );

        // Eight frames of noise eight average to about three.
        let quiet: Vec<f64> = (0..W * H)
            .filter(|i| ![5 * W + 5, 20 * W + 30, 33 * W + 12].contains(i))
            .map(at)
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let n = quiet.len() as f64;
        let mean = quiet.iter().sum::<f64>() / n;
        let spread = (quiet.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
        assert!((mean - 500.0).abs() < 1.0, "{mean}");
        assert!(spread < 8.0 / 8f64.sqrt() * 1.2, "{spread}");
    }

    #[test]
    fn a_few_frames_are_simply_averaged() {
        let dir = tempfile::tempdir().unwrap();
        write_set(dir.path(), "BIAS", "BIAS", &[], 3, |i| {
            vec![100.0 + i as f64; W * H]
        });
        let library = Library::scan(dir.path());
        let master = combine(only(&library, Kind::Bias), &never(), &mut |_, _| {}).unwrap();
        assert_eq!(master.source_count, 3);
        assert!(master.data.iter().all(|v| (*v - 101.0).abs() < 1e-3));
    }

    #[test]
    fn an_unreadable_frame_is_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        darks(dir.path(), 6);
        let library = Library::scan(dir.path());
        let set = only(&library, Kind::Dark).clone();
        std::fs::write(&set.frames[3], b"not any more").unwrap();
        let master = combine(&set, &never(), &mut |_, _| {}).unwrap();
        assert_eq!(master.source_count, 5);
    }

    #[test]
    fn nothing_readable_is_said_plainly() {
        let dir = tempfile::tempdir().unwrap();
        darks(dir.path(), 2);
        let library = Library::scan(dir.path());
        let set = only(&library, Kind::Dark).clone();
        for frame in &set.frames {
            std::fs::write(frame, b"gone").unwrap();
        }
        let why = combine(&set, &never(), &mut |_, _| {}).unwrap_err();
        assert_eq!(
            why,
            Failed::Because("none of the 2 darks could be read".into())
        );
    }

    #[test]
    fn making_a_master_can_be_stopped() {
        let dir = tempfile::tempdir().unwrap();
        darks(dir.path(), 6);
        let library = Library::scan(dir.path());
        let stop = AtomicBool::new(false);
        let result = combine(only(&library, Kind::Dark), &stop, &mut |done, _| {
            if done == 3 {
                stop.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(result.unwrap_err(), Failed::Cancelled);
    }

    #[test]
    fn a_master_is_kept_and_used_again_until_its_frames_change() {
        let dir = tempfile::tempdir().unwrap();
        darks(dir.path(), 6);
        let library = Library::scan(dir.path());
        let set = only(&library, Kind::Dark);
        let masters = Masters::of_library(dir.path());

        let (made, origin) = masters.dark(set, &never(), &mut |_, _| {}).unwrap();
        assert_eq!(origin, Origin::Made);
        let (kept, origin) = masters
            .dark(set, &never(), &mut |_, _| panic!("nothing to read"))
            .unwrap();
        assert_eq!(origin, Origin::Kept);
        assert_eq!(kept.data, made.data);
        assert_eq!(kept.source_count, 6);

        // The library does not see its own masters as frames.
        assert_eq!(Library::scan(dir.path()).sets.len(), 1);

        // A frame rewritten since: the master is made again.
        std::fs::File::options()
            .write(true)
            .open(&set.frames[0])
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            masters.dark(set, &never(), &mut |_, _| {}).unwrap().1,
            Origin::Made
        );
    }

    #[test]
    fn a_master_flat_takes_its_dark_off_and_evens_out_the_vignetting() {
        let dir = tempfile::tempdir().unwrap();
        // Flats: a bias of 500 under a field that falls to half at the corners.
        let vignetting = |x: usize, y: usize| {
            #[allow(clippy::cast_precision_loss)]
            let r2 = ((x as f64 - 23.5) / 23.5).powi(2) + ((y as f64 - 19.5) / 19.5).powi(2);
            1.0 - 0.25 * r2
        };
        write_set(
            dir.path(),
            "FLAT/L",
            "FLAT",
            &[("FILTER", "'L'"), ("EXPTIME", "2.2")],
            6,
            |i| {
                let noise = gaussian_background(W, H, 0.0, 30.0, 300 + i as u64);
                (0..W * H)
                    .map(|k| 500.0 + 20_000.0 * vignetting(k % W, k / W) + noise[k])
                    .collect()
            },
        );
        write_set(
            dir.path(),
            "BIAS",
            "BIAS",
            &[("EXPTIME", "0.0001")],
            6,
            |i| gaussian_background(W, H, 500.0, 3.0, 400 + i as u64),
        );

        let library = Library::scan(dir.path());
        let flats = only(&library, Kind::Flat);
        let bias = library.dark_for_flats(&flats.taken).expect("the bias");
        let masters = Masters::of_library(dir.path());
        let (flat, origin) = masters
            .flat(flats, Some(bias), &never(), &mut |_, _| {})
            .unwrap();
        assert_eq!(origin, Origin::Made);
        assert_eq!(flat.source_count, 6);

        // Centre over corner as the field had it, which it would not be with
        // the 500 of bias left in.
        let gain = |x: usize, y: usize| f64::from(flat.gain[y * W + x]);
        let ratio = gain(24, 20) / gain(1, 1);
        let truth = vignetting(24, 20) / vignetting(1, 1);
        assert!(
            (ratio - truth).abs() < 0.02,
            "{ratio:.3} against {truth:.3}"
        );

        // Kept as a gain map, and read back as one.
        let (kept, origin) = masters
            .flat(flats, Some(bias), &never(), &mut |_, _| panic!("kept"))
            .unwrap();
        assert_eq!(origin, Origin::Kept);
        assert_eq!(kept.gain, flat.gain);
        // A different dark under the same flats is a different master.
        assert_ne!(
            masters.file_for(flats, Some(bias)),
            masters.file_for(flats, None)
        );
    }

    #[test]
    fn a_library_that_cannot_be_written_to_still_gets_its_masters() {
        let dir = tempfile::tempdir().unwrap();
        darks(dir.path(), 6);
        let library = Library::scan(dir.path());
        // A file where the folder would go, which no master can be written into.
        std::fs::write(dir.path().join(MASTERS_FOLDER), b"in the way").unwrap();
        let masters = Masters::of_library(dir.path());
        let (_, origin) = masters
            .dark(only(&library, Kind::Dark), &never(), &mut |_, _| {})
            .unwrap();
        assert_eq!(origin, Origin::Made);
        assert_eq!(
            masters
                .dark(only(&library, Kind::Dark), &never(), &mut |_, _| {})
                .unwrap()
                .1,
            Origin::Made
        );
    }
}
