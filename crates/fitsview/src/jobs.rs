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
use fits_core::stack::{align, Alignment, PierSide, Stack};
use fits_core::stars::{self, DetectionParams, StarField};
use fits_core::{quality, read_fits, read_fits_header, write_fits, FitsHeader, FitsImage, Quality};

use crate::folder::StarMeasure;
use crate::library::Taken;
use crate::masters;
use crate::memory::{self, Memory};

/// What a finished job produced.
#[derive(Debug)]
pub enum Outcome {
    /// A master calibration frame was built.
    Master(Box<MasterFrame>),
    /// A master flat, already normalised into a gain map, was built.
    Flat(Box<MasterFlat>),
    /// A stack was written for each filter that had frames.
    Stacked {
        /// What was written, and how many frames went into each.
        stacks: Vec<(PathBuf, usize)>,
        /// Frames that could not be lined up with the rest of their filter.
        unaligned: usize,
        /// Samples left out as outliers, across every stack.
        rejected: usize,
        /// Whether a stack was made a colour at a time, to fit in memory.
        colour_at_a_time: bool,
        /// Each filter's calibration, a line each: `L-Pro — dark: …`.
        calibration: Vec<String>,
        /// What each filter lacked, a line each, saying what it costs.
        missing: Vec<String>,
    },
    /// Every file in the folder was measured.
    Measured(Vec<(PathBuf, Quality, Option<StarMeasure>)>),
    /// Calibrated copies were written, and this many succeeded.
    Exported {
        /// Files written.
        written: usize,
        /// Where they went.
        directory: PathBuf,
        /// What each filter lacked, a line each, saying what it costs.
        missing: Vec<String>,
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
    /// Stacks every file in `paths`, one output per filter.
    pub fn stack(paths: Vec<PathBuf>, recipe: StackRecipe) -> Self {
        let (tx, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let total = paths.len();

        std::thread::Builder::new()
            .name("fitsview-stack".into())
            .spawn(move || {
                stack_worker(&paths, &recipe, &tx, &worker_cancel);
            })
            .ok();

        Self {
            label: "Stacking".into(),
            updates,
            cancel,
            done: 0,
            total,
            item: String::new(),
            finished: false,
        }
    }

    pub fn measure(paths: Vec<PathBuf>, stars: Option<DetectionParams>) -> Self {
        let (tx, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let total = paths.len();

        std::thread::Builder::new()
            .name("fitsview-measure".into())
            .spawn(move || measure_worker(&paths, stars.as_ref(), &tx, &worker_cancel))
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
        library: Option<Arc<masters::InUse>>,
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
                    (dark, flat),
                    library.as_deref(),
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

/// Finds and measures the stars of one frame.
///
/// Takes the colour path when the file says it is a mosaic, exactly as the
/// viewer does, so that a folder's figures and the frame on screen agree.
fn measure_stars(image: &fits_core::FitsImage, params: &DetectionParams) -> StarMeasure {
    let pattern = image
        .header
        .get("BAYERPAT")
        .and_then(|v| fits_core::BayerPattern::parse(v.trim().trim_matches('\'').trim()));

    let field = match pattern {
        Some(pattern) if image.channels == 1 => stars::detect_mosaic(image, pattern, params),
        _ => stars::detect(image, params),
    };

    let scale = fits_core::header::plate_scale_arcsec(&image.header);
    StarMeasure {
        fwhm: field.fwhm,
        fwhm_arcsec: field.fwhm.zip(scale).map(|(fwhm, scale)| fwhm * scale),
        roundness: field.roundness,
        count: field.count(),
        settings: crate::measurements::settings_of(params),
    }
}

/// What a stack's own output is called, and what marks it as not a frame.
pub const STACK_PREFIX: &str = "stack_";

/// What a frame counts for, or one when frames are not being weighted.
fn frame_weight(weighted: bool, noise: f64, fwhm: Option<f64>) -> f64 {
    if !weighted {
        return 1.0;
    }
    fits_core::stack::weight_of(noise, fwhm).unwrap_or(1.0)
}

/// Everything a stack needs beyond the files themselves.
#[derive(Debug, Clone)]
pub struct StackRecipe {
    /// Master dark to subtract, if any.
    pub dark: Option<Arc<MasterFrame>>,
    /// Master flat to divide by, if any.
    pub flat: Option<Arc<MasterFlat>>,
    /// The mosaic's filter pattern, for frames from a colour camera.
    pub pattern: Option<fits_core::BayerPattern>,
    /// How hard to look for the stars the frames are lined up by.
    pub params: DetectionParams,
    /// Whether to leave out samples the other frames disagree with.
    pub reject: bool,
    /// Whether a quiet, sharp frame counts for more than a noisy, soft one.
    pub weighted: bool,
    /// Bytes the stack may take, or `None` to ask the system when it starts.
    /// A colour stack that would take more is stacked a colour at a time.
    pub room: Option<u64>,
    /// The calibration library, if one is set: each filter's dark and flat
    /// come from it, unless chosen by hand.
    pub library: Option<Arc<masters::InUse>>,
}

/// Stacks a folder, one file per filter.
///
/// Frames are grouped by their `FILTER` keyword, since combining filters
/// averages away the thing the filters were for. Within a group the first
/// readable frame is the reference and the rest are lined up with it by their
/// stars, turning any that were taken on the other side of the pier.
///
/// Only one frame is held at a time beyond the running totals, because a
/// night of full-frame captures does not fit in memory otherwise. When even the
/// totals would not fit, a colour stack is made a colour at a time: the frames
/// are lined up once, and each further colour reads them again and adds them
/// where they were put. The result is the same to the bit.
#[allow(clippy::too_many_lines)]
fn stack_worker(
    paths: &[PathBuf],
    recipe: &StackRecipe,
    tx: &mpsc::Sender<Update>,
    cancel: &AtomicBool,
) {
    let StackRecipe {
        dark: dark_by_hand,
        flat: flat_by_hand,
        pattern,
        params,
        reject,
        weighted,
        room,
        library,
    } = recipe;
    let (pattern, reject, weighted) = (*pattern, *reject, *weighted);
    let room = room.unwrap_or_else(|| memory::room(Memory::now(), 0));
    // Which filter each frame belongs to, read from the header alone so that
    // grouping costs a few kilobytes a frame rather than a decode. A frame
    // whose pixels turn out to be unreadable is passed over when it is
    // stacked, as one that will not line up is.
    let mut groups: Vec<(String, Vec<PathBuf>)> = Vec::new();
    for path in paths {
        // A stack this job wrote earlier is not a frame. Without this, running
        // it a second time folds the first result back into the new one.
        if file_name_of(path).starts_with(STACK_PREFIX) {
            continue;
        }
        let filter = match read_fits_header(path) {
            Ok(header) => filter_of(&header),
            Err(_) => continue,
        };
        match groups.iter_mut().find(|(name, _)| *name == filter) {
            Some((_, list)) => list.push(path.clone()),
            None => groups.push((filter, vec![path.clone()])),
        }
    }

    // A frame calibrated and in colour, ready to add, or `None` if it cannot
    // be read.
    let prepare = |path: &Path, dark: Option<&MasterFrame>, flat: Option<&MasterFlat>| {
        let light = read_fits(path).ok()?;
        calib::calibrate_and_debayer(&light, dark, flat, pattern).ok()
    };
    let new_stack = |(width, height): (usize, usize), channels: usize| {
        if reject {
            Stack::rejecting(width, height, channels)
        } else {
            Stack::new(width, height, channels)
        }
    };

    let mut stacks = Vec::new();
    let mut unaligned = 0usize;
    let mut rejected = 0usize;
    let mut colour_at_a_time = false;
    let mut calibration: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut done = 0usize;
    // Rejecting means reading every frame a second time, to measure it against
    // what the first pass found ordinary. A colour at a time means reading it
    // again for each further colour, in each pass; that is added when it is
    // decided.
    let passes = if reject { 2 } else { 1 };
    let mut steps = paths.len() * passes;

    for (filter, group) in &groups {
        // This filter's dark and flat: chosen by hand, or found in the
        // library by how its first frame was taken.
        let lights = group
            .first()
            .and_then(|path| read_fits_header(path).ok())
            .as_ref()
            .and_then(Taken::of_light);
        let by_hand = (dark_by_hand.clone(), flat_by_hand.clone());
        let chosen = match lights {
            Some(lights) => {
                let mut report = |item: String| {
                    let _ = tx.send(Update::Progress {
                        done,
                        total: steps,
                        item,
                    });
                };
                match masters::choose(&lights, by_hand, library.as_deref(), cancel, &mut report) {
                    Ok(chosen) => chosen,
                    Err(_) => {
                        let _ = tx.send(Update::Cancelled);
                        return;
                    }
                }
            }
            None => masters::Chosen {
                dark: by_hand.0,
                flat: by_hand.1,
                ..masters::Chosen::default()
            },
        };
        calibration.extend(chosen.used.iter().map(|line| format!("{filter} — {line}")));
        missing.extend(
            chosen
                .missing
                .iter()
                .map(|line| format!("{filter} — {line}")),
        );
        let (dark, flat) = (chosen.dark.as_deref(), chosen.flat.as_deref());

        let mut stack: Option<Stack> = None;
        let mut reference: Option<(StarField, PierSide, (usize, usize))> = None;
        let mut header = None;
        let mut sky: Option<f64> = None;
        // The frames' channels, and how many of them each stack holds: all of
        // them, or one when all at once would not fit. Set by the first frame.
        let mut channels = 0usize;
        let mut per_stack = 0usize;
        // Where each frame ended up, so that later passes do not have to find
        // its stars all over again. That is most of what makes rejection
        // cheaper the second time round.
        let mut placed: Vec<(PathBuf, Alignment)> = Vec::new();

        for path in group {
            if cancel.load(Ordering::Relaxed) {
                let _ = tx.send(Update::Cancelled);
                return;
            }
            let _ = tx.send(Update::Progress {
                done,
                total: steps,
                item: file_name_of(path),
            });
            done += 1;

            let Ok(light) = read_fits(path) else { continue };
            let side = PierSide::from_header(&light.header);
            let Ok(prepared) = calib::calibrate_and_debayer(&light, dark, flat, pattern) else {
                continue;
            };

            // What this frame is worth against the others: quiet and sharp
            // counts for more than noisy and soft.
            let measured = quality::measure(&light);
            let size = (prepared.width, prepared.height);
            let found = match pattern {
                // Detection wants the mosaic, not the reconstruction.
                Some(p) if light.channels == 1 => stars::detect_mosaic(&light, p, params),
                _ => stars::detect(&prepared, params),
            };

            match &reference {
                None => {
                    // The first readable frame of the group sets the frame of
                    // reference for the rest of it, its sky included, and says
                    // how big the stack will be.
                    channels = prepared.channels;
                    per_stack = if memory::together_fits(size.0, size.1, channels, reject, room) {
                        channels
                    } else {
                        1
                    };
                    if per_stack < channels {
                        log::info!("stacking {filter} a colour at a time to fit in memory");
                        colour_at_a_time = true;
                        steps += group.len() * passes * (channels / per_stack - 1);
                    }
                    let mut fresh = new_stack(size, per_stack);
                    let placing = Alignment {
                        weight: frame_weight(weighted, measured.noise, found.fwhm),
                        ..Alignment::still()
                    };
                    fresh.add_channels(&prepared, 0, placing);
                    placed.push((path.clone(), placing));
                    header = Some(light.header.clone());
                    sky = Some(fits_core::stack::sky_level(&prepared));
                    reference = Some((found, side, size));
                    stack = Some(fresh);
                }
                Some((anchor, anchor_side, anchor_size)) => {
                    let turned = side.needs_turning(*anchor_side);
                    match align(anchor, &found, *anchor_size, turned) {
                        Some(mut alignment) => {
                            // Brought to the reference's sky before it is
                            // added: a frame taken under a brighter sky lifts
                            // the result otherwise, and rejection then treats
                            // it as the outlier at every pixel and discards
                            // the whole of it.
                            if let Some(sky) = sky {
                                alignment.offset = fits_core::stack::levelling(&prepared, sky);
                            }
                            alignment.weight = frame_weight(weighted, measured.noise, found.fwhm);
                            if let Some(stack) = stack.as_mut() {
                                if prepared.channels == channels
                                    && stack.add_channels(&prepared, 0, alignment)
                                {
                                    placed.push((path.clone(), alignment));
                                } else {
                                    unaligned += 1;
                                }
                            }
                        }
                        None => {
                            log::warn!("could not line up {}", file_name_of(path));
                            unaligned += 1;
                        }
                    }
                }
            }
        }

        let (Some(stack), Some(header), Some((_, _, size))) = (stack, header, reference) else {
            continue;
        };
        if stack.frames() == 0 {
            continue;
        }

        let frames = stack.frames();
        let Some(directory) = group.first().and_then(|p| p.parent()) else {
            continue;
        };
        let out = directory.join(format!("{STACK_PREFIX}{}.fits", safe_name(filter)));

        // Each stack's channels in turn: all of them at once, or a colour after
        // another, each further colour reading the frames again and adding
        // them where the first pass put them.
        let mut first_stack = Some(stack);
        let mut data: Vec<f32> = Vec::with_capacity(size.0 * size.1 * channels);
        let mut judged = false;
        let mut group_rejected = 0usize;
        for first in (0..channels).step_by(per_stack) {
            let stack = match first_stack.take() {
                Some(stack) => stack,
                None => {
                    let mut stack = new_stack(size, per_stack);
                    for (path, alignment) in &placed {
                        if cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(Update::Cancelled);
                            return;
                        }
                        let _ = tx.send(Update::Progress {
                            done,
                            total: steps,
                            item: file_name_of(path),
                        });
                        done += 1;
                        if let Some(prepared) = prepare(path, dark, flat) {
                            stack.add_channels(&prepared, first, *alignment);
                        }
                    }
                    stack
                }
            };

            // The second pass, when it is wanted and there are enough frames
            // for it. The first pass is handed over rather than copied, which
            // on full frames was several gigabytes held for nothing.
            let part = if reject {
                stack.into_rejecting(fits_core::stack::DEFAULT_CLIP)
            } else {
                Err(Box::new(stack))
            };
            let finished = match part {
                Ok(mut second) => {
                    for (path, alignment) in &placed {
                        if cancel.load(Ordering::Relaxed) {
                            let _ = tx.send(Update::Cancelled);
                            return;
                        }
                        let _ = tx.send(Update::Progress {
                            done,
                            total: steps,
                            item: file_name_of(path),
                        });
                        done += 1;
                        if let Some(prepared) = prepare(path, dark, flat) {
                            second.add_channels(&prepared, first, *alignment);
                        }
                    }
                    judged = true;
                    group_rejected += second.rejected();
                    second.finish(header.clone())
                }
                Err(stack) => stack.finish(header.clone()),
            };
            data.extend(finished.data);
        }

        rejected += group_rejected;
        let (min, max) = fits_core::finite_min_max(&data);
        let result = FitsImage {
            width: size.0,
            height: size.1,
            channels,
            data,
            header,
            min,
            max,
        };
        let mut history = format!("Stacked {frames} frames of filter {filter}");
        if judged {
            history.push_str(&format!(", {group_rejected} samples rejected"));
        }
        if per_stack < channels {
            history.push_str(", a colour at a time");
        }
        // How it was calibrated, and what it was not, travels with the file.
        let mut history = vec![history];
        history.extend(calib::history_for(dark, flat));
        history.extend(chosen.used.iter().map(|line| history_line(line)));
        history.extend(chosen.missing.iter().map(|line| history_line(line)));
        if let Err(e) = write_fits(&out, &result, &history) {
            let _ = tx.send(Update::Failed(format!("{}: {e}", file_name_of(&out))));
            return;
        }
        stacks.push((out, frames));
    }

    let _ = tx.send(Update::Progress {
        done: steps,
        total: steps,
        item: String::new(),
    });
    let _ = tx.send(Update::Finished(Outcome::Stacked {
        stacks,
        unaligned,
        rejected,
        colour_at_a_time,
        calibration,
        missing,
    }));
}

/// The filter a frame was taken through, or a stand-in when it does not say.
fn filter_of(header: &FitsHeader) -> String {
    header
        .get("FILTER")
        .map(|v| v.trim().trim_matches('\'').trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unfiltered".to_string())
}

/// A filter name reduced to something safe to put in a file name.
fn safe_name(filter: &str) -> String {
    let cleaned: String = filter
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unfiltered".to_string()
    } else {
        trimmed
    }
}

/// Measures each file in turn.
///
/// A file that cannot be read is skipped rather than failing the run: one
/// corrupt frame in two hundred should not deny the user the other 199
/// measurements.
///
/// The next frame is read while this one is measured. Reading and decoding
/// are a third of the time on an internal disk and most of it on a slow
/// external one, and the disk would otherwise sit idle while the stars are
/// found. One frame ahead, and no more: a 61-megapixel frame is a quarter of a
/// gigabyte decoded, so the reader waits for each to be taken before starting
/// the next.
fn measure_worker(
    paths: &[PathBuf],
    stars: Option<&DetectionParams>,
    tx: &mpsc::Sender<Update>,
    cancel: &AtomicBool,
) {
    let mut measured = Vec::with_capacity(paths.len());

    std::thread::scope(|scope| {
        // No buffer: the reader holds the one frame it has read until it is
        // taken, so there is never more than one waiting.
        let (frames_tx, frames) = mpsc::sync_channel(0);
        scope.spawn(move || {
            for path in paths {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                // Fails once measuring has stopped taking frames.
                if frames_tx.send((path, read_fits(path))).is_err() {
                    break;
                }
            }
        });

        for (index, (path, decoded)) in frames.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                // Report what was measured before stopping; the work is not
                // wasted.
                break;
            }
            let _ = tx.send(Update::Progress {
                done: index,
                total: paths.len(),
                item: file_name_of(path),
            });

            match decoded {
                Ok(image) => {
                    // Both only read the frame, and the background is taken
                    // on one thread while the star search leaves others idle
                    // between its parallel stages.
                    let (quality, found) = rayon::join(
                        || quality::measure(&image),
                        || stars.map(|params| measure_stars(&image, params)),
                    );
                    measured.push((path.clone(), quality, found));
                }
                Err(e) => log::warn!("could not measure {}: {e}", file_name_of(path)),
            }
        }
        // Closing the channel is what releases a reader waiting to hand over
        // a frame nobody will take. It would close at the end of this closure
        // anyway, before the scope waits for the reader; this says so, so that
        // moving the channel outside the scope is seen to be a deadlock.
        drop(frames);
    });

    if !cancel.load(Ordering::Relaxed) {
        let _ = tx.send(Update::Progress {
            done: paths.len(),
            total: paths.len(),
            item: String::new(),
        });
    }
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
#[allow(clippy::too_many_lines)]
fn export_worker(
    paths: &[PathBuf],
    by_hand: (Option<Arc<MasterFrame>>, Option<Arc<MasterFlat>>),
    library: Option<&masters::InUse>,
    directory: &Path,
    tx: &mpsc::Sender<Update>,
    cancel: &AtomicBool,
) {
    if let Err(e) = std::fs::create_dir_all(directory) {
        let _ = tx.send(Update::Failed(format!("{}: {e}", directory.display())));
        return;
    }

    // Each way the frames were taken is worked out once: a folder holds a few
    // filters and exposures, not a few hundred.
    let mut chosen: std::collections::HashMap<String, masters::Chosen> =
        std::collections::HashMap::new();
    let mut missing: Vec<String> = Vec::new();
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

        let taken = Taken::of_light(&light.header);
        let key = taken.as_ref().map_or_else(String::new, |t| {
            format!(
                "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
                t.filter,
                t.exposure,
                t.size,
                t.binning,
                t.gain,
                t.offset,
                t.set_temperature,
                t.camera
            )
        });
        if !chosen.contains_key(&key) {
            let this = match &taken {
                Some(taken) => {
                    let mut report = |item: String| {
                        let _ = tx.send(Update::Progress {
                            done: index,
                            total: paths.len(),
                            item,
                        });
                    };
                    match masters::choose(taken, by_hand.clone(), library, cancel, &mut report) {
                        Ok(this) => this,
                        Err(_) => {
                            let _ = tx.send(Update::Cancelled);
                            return;
                        }
                    }
                }
                None => masters::Chosen {
                    dark: by_hand.0.clone(),
                    flat: by_hand.1.clone(),
                    ..masters::Chosen::default()
                },
            };
            let filter = taken
                .as_ref()
                .and_then(|t| t.filter.clone())
                .unwrap_or_else(|| "unfiltered".into());
            missing.extend(this.missing.iter().map(|line| format!("{filter} — {line}")));
            chosen.insert(key.clone(), this);
        }
        let this = &chosen[&key];
        let (dark, flat) = (this.dark.as_deref(), this.flat.as_deref());

        let calibrated = match calib::calibrate(&light, dark, flat) {
            Ok(image) => image,
            Err(e) => {
                let _ = tx.send(Update::Failed(format!("{}: {e}", file_name_of(path))));
                return;
            }
        };

        let mut history = calib::history_for(dark, flat);
        history.extend(this.used.iter().map(|line| history_line(line)));
        history.extend(this.missing.iter().map(|line| history_line(line)));
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
        missing,
    }));
}

/// A line of what was said about calibration, as a `HISTORY` card holds it.
///
/// Cards are ASCII, and anything else is written as a question mark, so the
/// degrees, times signs and dashes the interface uses are spelt out plainly
/// rather than arriving as `-14 ?C` and `1?1 binning`.
fn history_line(line: &str) -> String {
    let plain = line
        .replace(" °C", " C")
        .replace('°', "")
        .replace('×', "x")
        .replace('—', "-");
    format!("fitsview: {plain}")
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

        let mut job = Job::export(paths, None, None, None, out_dir.path().to_path_buf());
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
        let mut job = Job::export(paths.clone(), None, None, None, dir.path().to_path_buf());
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

        let mut job = Job::export(paths, None, None, None, out_dir.path().to_path_buf());
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

        let mut job = Job::export(paths, None, None, None, target.clone());
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
        let mut job = Job::measure(paths.clone(), None);
        let updates = run(&mut job);

        match updates.into_iter().next() {
            Some(Update::Finished(Outcome::Measured(measured))) => {
                assert_eq!(measured.len(), 3);
                for (path, quality, _) in measured {
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

        let mut job = Job::measure(paths, None);
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
        let mut job = Job::measure(paths, None);
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
    fn cancelling_while_the_next_frame_waits_to_be_taken_still_finishes() {
        // The reader is a frame ahead, blocked handing it over. Stopping has
        // to release it, or the job never reports and the window waits on it
        // for ever.
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(200, 200, -32);
        let pixels = fits_core::testutil::gaussian_background(200, 200, 1000.0, 10.0, 3);
        let paths: Vec<PathBuf> = (0..40)
            .map(|i| write_synthetic(dir.path(), &format!("f{i}.fits"), &spec, &pixels).unwrap())
            .collect();

        let mut job = Job::measure(paths, Some(DetectionParams::default()));
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut started = false;
        while !started && Instant::now() < deadline {
            assert!(job.poll().is_empty(), "finished before it could be stopped");
            started = job.fraction() >= 2.0 / 40.0;
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(started, "measuring never got going");
        job.cancel();

        let finished = run(&mut job).into_iter().find_map(|u| match u {
            Update::Finished(Outcome::Measured(m)) => Some(m.len()),
            _ => None,
        });
        let measured = finished.expect("a cancelled measurement still reports");
        assert!((2..40).contains(&measured), "measured {measured}");
    }

    #[test]
    fn measurements_come_back_in_the_order_asked_for() {
        // Read on one thread and measured on another; the order must survive.
        let (_dir, paths) = frames(12, 10.0);
        let mut job = Job::measure(paths.clone(), None);
        let measured = run(&mut job)
            .into_iter()
            .find_map(|u| match u {
                Update::Finished(Outcome::Measured(m)) => Some(m),
                _ => None,
            })
            .unwrap();
        let order: Vec<PathBuf> = measured.into_iter().map(|(p, _, _)| p).collect();
        assert_eq!(order, paths);
    }

    #[test]
    fn fraction_is_safe_for_an_empty_job() {
        let mut job = Job::build_master(Vec::new());
        assert!((job.fraction() - 0.0).abs() < f32::EPSILON);
        run(&mut job);
    }
}
