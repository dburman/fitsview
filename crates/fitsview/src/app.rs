//! Application state and the rules that change it.
//!
//! [`Model`] is deliberately free of anything that needs a window: no context,
//! no texture handle, no painter. Every state transition goes through
//! [`Model::handle`], so the behaviour a user actually notices, such as what
//! happens when they press a key, is testable without opening a window.
//!
//! The thin `eframe` wrapper that turns input into [`Action`]s and draws the
//! result lives in [`crate::ui`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use fits_core::calib::{self, MasterFlat, MasterFrame};
use fits_core::debayer::{self, BayerPattern};
use fits_core::stretch::StretchParams;
use fits_core::FitsImage;

use crate::actions::{self, ActionError, FileOps, Outcome, RealFileOps};
use crate::folder::{scan_folder, Folder, SortKey};
use crate::jobs::{self, Job};
use crate::loader::{Cache, Loader};
use crate::sidecar;
use crate::view::ViewState;

/// An image that has been loaded and is being displayed.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// Where it came from.
    pub path: PathBuf,
    /// The samples as displayed: calibrated, and debayered when that is on.
    pub image: Arc<FitsImage>,
    /// The samples as they came off the disk.
    ///
    /// Calibration frames describe this, not the displayed image. A dark for a
    /// one-shot colour camera is a single-channel mosaic, and comparing it
    /// against a debayered three-channel display would report a mismatch that
    /// is not real.
    pub raw: Arc<FitsImage>,
    /// How long the read took, in milliseconds, for the status bar.
    pub load_ms: f64,
}

impl Loaded {
    /// The image's dimensions as a vector, for view arithmetic.
    #[must_use]
    pub fn size(&self) -> Vec2 {
        #[allow(clippy::cast_precision_loss)]
        Vec2::new(self.image.width as f32, self.image.height as f32)
    }

    /// A one-line description for the status bar.
    #[must_use]
    pub fn summary(&self) -> String {
        let name = self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let bitpix = self.image.header.get("BITPIX").unwrap_or("?");
        let colour = if self.image.channels == 3 { " RGB" } else { "" };
        format!(
            "{name}  {}x{}{colour}  BITPIX {bitpix}  {:.0} ms",
            self.image.width, self.image.height, self.load_ms
        )
    }
}

/// Everything the user can ask the application to do.
///
/// Input handling turns key presses, clicks and dialogs into these; nothing
/// else mutates the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Open a path. A folder is browsed; a file is displayed with its folder
    /// loaded around it, so the arrow keys work straight away.
    Open(PathBuf),
    /// Show the file at this index in the current folder.
    Select(usize),
    /// Move to the next file. Stops at the end.
    NextFile,
    /// Move to the previous file. Stops at the start.
    PreviousFile,
    /// Jump to the first file.
    FirstFile,
    /// Jump to the last file.
    LastFile,
    /// Re-read the folder from disk, keeping the selection where possible.
    Rescan,
    /// Scale the image to fit the viewport and centre it.
    FitToWindow,
    /// Show the image at one screen pixel per image pixel.
    ActualSize,
    /// Move the image by a screen-space delta.
    Pan(Vec2),
    /// Zoom by `factor` while keeping `anchor` fixed.
    ZoomAt {
        /// Screen position to keep fixed.
        anchor: Pos2,
        /// Multiplier applied to the current zoom.
        factor: f32,
    },
    /// Turn the selected file's keep flag on or off.
    ToggleFlag,
    /// Ask to delete the selected file. Opens a confirmation if one is needed.
    RequestDelete,
    /// Delete without asking. Only reached from the confirmation dialog.
    ConfirmDelete,
    /// Begin renaming the selected file.
    BeginRename,
    /// The rename editor's text changed, so revalidate it.
    RenameTextChanged(String),
    /// Finish renaming, using the name currently typed.
    CommitRename(String),
    /// Abandon a confirmation or a rename.
    Cancel,
    /// Turn the "confirm every delete" setting on or off.
    ToggleConfirmEveryDelete,
    /// Offer these files as dark frames to combine.
    AddDarks(Vec<PathBuf>),
    /// Combine the collected dark frames into a master.
    BuildMasterDark,
    /// Use an existing master dark from disk.
    LoadMasterDark(PathBuf),
    /// Write the current master dark to disk.
    SaveMasterDark(PathBuf),
    /// Forget the collected darks and the master.
    ClearDarks,
    /// Turn dark subtraction on or off.
    ToggleApplyDark,
    /// Offer these files as flat frames to combine.
    AddFlats(Vec<PathBuf>),
    /// Offer these files as flat darks, subtracted from the flats.
    AddFlatDarks(Vec<PathBuf>),
    /// Combine the collected flats into a gain map.
    BuildMasterFlat,
    /// Use an existing master flat from disk.
    LoadMasterFlat(PathBuf),
    /// Write the current gain map to disk.
    SaveMasterFlat(PathBuf),
    /// Forget the collected flats and the gain map.
    ClearFlats,
    /// Turn flat division on or off.
    ToggleApplyFlat,
    /// Turn colour reconstruction on or off.
    ToggleDebayer,
    /// Use this filter pattern.
    SetBayerPattern(BayerPattern),
    /// Read the pattern the other way up, for a file whose header refers to the
    /// sensor rather than to the stored row order.
    ToggleBayerFlip,
    /// Measure every file in the folder, so bad frames can be sorted out.
    MeasureFolder,
    /// Order the file list by this measure.
    SortBy(SortKey),
    /// Write calibrated copies of every file in the folder into this folder.
    StartExport(PathBuf),
    /// Stop whatever background job is running.
    CancelJob,
    /// Turn the automatic screen stretch on or off.
    ToggleStretch,
    /// Change the stretch settings.
    SetStretchParams(StretchParams),
    /// Return the stretch settings to their defaults.
    ResetStretchParams,
    /// Collapse the file list to the left edge, or bring it back.
    ToggleFileList,
    /// Set whether the file list is showing, when the panel itself decides.
    SetFileListVisible(bool),
    /// Show or hide the histogram strip.
    ToggleHistogram,
    /// Show or hide the FITS header panel.
    ToggleHeader,
    /// Narrow the header panel to matching cards.
    SetHeaderFilter(String),
    /// Show or hide the keyboard shortcut overlay.
    ToggleHelp,
    /// Dismiss the current error message.
    ClearError,
}

/// A modal question or edit that is waiting on the user.
///
/// Only one can be active at a time, which is why this is an enum rather than
/// a set of booleans that could contradict each other.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pending {
    /// Nothing is waiting.
    #[default]
    None,
    /// Waiting for the user to confirm deleting this file.
    DeleteConfirm {
        /// Name of the file in question.
        name: String,
        /// Whether it carries a keep flag, which changes the wording.
        flagged: bool,
    },
    /// Renaming, with the text typed so far.
    Rename {
        /// Current contents of the edit box.
        text: String,
        /// Why the current text is unusable, if it is.
        problem: Option<String>,
    },
}

/// The calibration frames in use, and whether they are applied.
#[derive(Debug, Default)]
pub struct Calibration {
    /// Files offered as darks, waiting to be combined.
    pub dark_sources: Vec<PathBuf>,
    /// The combined master dark, once built or loaded.
    pub dark: Option<Arc<MasterFrame>>,
    /// Whether the master is subtracted from what is displayed.
    pub apply_dark: bool,
    /// Files offered as flats, waiting to be combined.
    pub flat_sources: Vec<PathBuf>,
    /// Files offered as flat darks, subtracted from the flats before combining.
    pub flat_dark_sources: Vec<PathBuf>,
    /// Where the current master dark came from, for the folder's sidecar.
    pub dark_path: Option<PathBuf>,
    /// Where the current master flat came from, for the folder's sidecar.
    pub flat_path: Option<PathBuf>,
    /// The combined master flat, normalised into a gain map.
    pub flat: Option<Arc<MasterFlat>>,
    /// Whether the gain map is divided out of what is displayed.
    pub apply_flat: bool,
    /// Reasons the master may not suit the current image.
    pub warnings: Vec<String>,
    /// Why the master cannot be applied at all, if it cannot.
    pub blocked: Option<String>,
}

impl Calibration {
    /// Whether a master dark is available to apply.
    #[must_use]
    pub fn has_dark(&self) -> bool {
        self.dark.is_some()
    }

    /// Whether a master flat is available to apply.
    #[must_use]
    pub fn has_flat(&self) -> bool {
        self.flat.is_some()
    }

    /// The dark to use for display, or `None` when it is off or unusable.
    #[must_use]
    pub fn active_dark(&self) -> Option<&MasterFrame> {
        if self.apply_dark && self.blocked.is_none() {
            self.dark.as_deref()
        } else {
            None
        }
    }

    /// The flat to use for display, or `None` when it is off or unusable.
    #[must_use]
    pub fn active_flat(&self) -> Option<&MasterFlat> {
        if self.apply_flat && self.blocked.is_none() {
            self.flat.as_deref()
        } else {
            None
        }
    }

    /// Whether anything is actually being applied.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active_dark().is_some() || self.active_flat().is_some()
    }

    /// A one-line description of the flat, for the panel.
    #[must_use]
    pub fn flat_summary(&self) -> String {
        match &self.flat {
            None if self.flat_sources.is_empty() => "No flats".to_string(),
            None => format!("{} flats, not yet combined", self.flat_sources.len()),
            Some(flat) => {
                let unusable = if flat.unusable == 0 {
                    String::new()
                } else {
                    format!(", {} unusable pixels", flat.unusable)
                };
                format!(
                    "Gain map from {} frame{}{unusable}",
                    flat.source_count,
                    if flat.source_count == 1 { "" } else { "s" }
                )
            }
        }
    }

    /// A one-line description of the master, for the panel.
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.dark {
            None if self.dark_sources.is_empty() => "No darks".to_string(),
            None => format!("{} darks, not yet combined", self.dark_sources.len()),
            Some(dark) => {
                let exposure = dark
                    .exptime
                    .map_or_else(String::new, |e| format!(", {e:.0} s"));
                format!(
                    "Master of {} frame{}{exposure}",
                    dark.source_count,
                    if dark.source_count == 1 { "" } else { "s" }
                )
            }
        }
    }
}

/// How a one-shot colour mosaic is turned back into colour.
#[derive(Debug, Clone, Default)]
pub struct Bayer {
    /// Whether colour reconstruction is applied.
    pub enabled: bool,
    /// The filter pattern in use, once one is known.
    pub pattern: Option<BayerPattern>,
    /// Whether the pattern is read with its rows the other way up.
    ///
    /// FITS stores the bottom row first, and capture programs disagree about
    /// which end `BAYERPAT` describes. The symptom of needing this is an image
    /// in the wrong colours rather than a broken one.
    pub flip_rows: bool,
    /// True when the pattern came from the file rather than from the user.
    pub from_header: bool,
}

impl Bayer {
    /// The pattern to actually use, or `None` when reconstruction is off.
    #[must_use]
    pub fn active(&self) -> Option<BayerPattern> {
        if !self.enabled {
            return None;
        }
        self.pattern
            .map(|p| if self.flip_rows { p.flipped_rows() } else { p })
    }

    /// A one-line description for the panel.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.pattern {
            None => "No filter pattern known".to_string(),
            Some(pattern) => {
                let source = if self.from_header {
                    "from the file"
                } else {
                    "chosen"
                };
                let flipped = if self.flip_rows { ", rows flipped" } else { "" };
                format!("{} {source}{flipped}", pattern.name())
            }
        }
    }
}

/// How many calibrated images to keep.
///
/// The selected file and its two neighbours, which is what the loader
/// prefetches, so stepping either way is instant.
const CALIBRATED_CACHE_ENTRIES: usize = 3;

/// How much memory those may occupy.
///
/// Three debayered 24-megapixel colour frames come to about 864 MB, which is
/// the case this has to accommodate; anything smaller is limited by the entry
/// count instead.
const CALIBRATED_CACHE_BYTES: usize = 1024 * 1024 * 1024;

/// The pixel under the pointer, and what it holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelReadout {
    /// Column, counting from the left of the picture.
    pub x: usize,
    /// Row, counting from the **top** of the picture as displayed.
    pub y: usize,
    /// The sample in each channel, as the file holds it.
    pub values: [f32; 3],
    /// How many of those are meaningful: 1 for mono, 3 for colour.
    pub channels: usize,
    /// True when the three values were reconstructed from a colour mosaic
    /// rather than measured directly, so the readout can say so.
    pub reconstructed: bool,
}

impl PixelReadout {
    /// The readout as it appears in the status bar.
    #[must_use]
    pub fn describe(&self) -> String {
        let value = |v: f32| {
            if v.is_finite() {
                // Whole numbers for the integer formats astronomy cameras
                // produce, decimals only where they carry information.
                if v.abs() >= 1000.0 || v.fract() == 0.0 {
                    format!("{v:.0}")
                } else {
                    format!("{v:.3}")
                }
            } else {
                "—".to_string()
            }
        };
        let samples = match self.channels {
            3 => format!(
                "{}, {}, {}",
                value(self.values[0]),
                value(self.values[1]),
                value(self.values[2])
            ),
            _ => value(self.values[0]),
        };
        // A tilde marks values that were interpolated from the filter grid
        // rather than measured, so a reading is never taken for more than it is.
        let note = if self.reconstructed { "~" } else { "" };
        format!("({}, {})  {note}{samples}", self.x, self.y)
    }
}

/// A short-lived message shown after an action.
#[derive(Debug, Clone)]
pub struct Toast {
    /// What to show.
    pub text: String,
    /// When it stops being shown.
    pub until: std::time::Instant,
}

impl Toast {
    /// How long a message stays on screen.
    pub const LIFETIME: std::time::Duration = std::time::Duration::from_secs(3);

    /// A message shown from now.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            until: std::time::Instant::now() + Self::LIFETIME,
        }
    }

    /// Whether it should still be shown.
    #[must_use]
    pub fn is_live(&self) -> bool {
        std::time::Instant::now() < self.until
    }
}

/// The application's state.
#[derive(Debug)]
pub struct Model {
    /// The folder being browsed, if any.
    pub folder: Option<Folder>,
    /// The image on screen, if any.
    pub loaded: Option<Loaded>,
    /// Where that image is placed.
    pub view: ViewState,
    /// The most recent failure, shown until dismissed or superseded.
    pub error: Option<String>,
    /// Set when a new image arrives, so the next frame can fit it to a viewport
    /// whose size is only known during drawing.
    pub needs_fit: bool,
    /// Bumped whenever the displayed image changes, so the drawing layer knows
    /// its cached texture is stale.
    pub generation: u64,
    /// The viewport used by the last frame, so keyboard actions that need to
    /// know the window size can work before the next draw.
    pub viewport: Rect,
    /// True while the selected file is still being decoded.
    pub loading: bool,
    /// A confirmation or rename waiting on the user.
    pub pending: Pending,
    /// When on, even unflagged files ask before being deleted.
    pub confirm_every_delete: bool,
    /// Whether the shortcut overlay is showing.
    pub show_help: bool,
    /// What the file list is ordered by.
    pub sort_key: SortKey,
    /// Whether the file list is showing. Collapsing it gives the image the
    /// whole window, which matters when culling on a laptop screen.
    pub show_filelist: bool,
    /// Where the pointer is, when it is over the image.
    pub pointer: Option<Pos2>,
    /// Set for one frame after the keyboard moves the selection, so the list
    /// scrolls to follow it. Clicking a row must not scroll it under the
    /// pointer, which is why this is not simply always on.
    pub scroll_to_selection: bool,
    /// Whether the histogram strip beneath the image is showing.
    pub show_histogram: bool,
    /// Whether the metadata section of the right-hand panel is expanded.
    pub show_header: bool,
    /// Text narrowing the header panel.
    pub header_filter: String,
    /// Calibration frames and whether they are applied.
    pub calibration: Calibration,
    /// How a one-shot colour mosaic is turned back into colour.
    pub bayer: Bayer,
    /// The background job in progress, if any.
    pub job: Option<Job>,
    /// Calibrated images, so stepping back and forth does not recalibrate.
    ///
    /// Sized for the working set the loader prefetches, which is the selected
    /// file and its two neighbours. The bound has to be generous enough for
    /// **debayered colour**: a one-shot colour frame from a 24-megapixel camera
    /// becomes three planes of floats, 288 MB, so a budget sized for mono holds
    /// fewer than two and re-debayers almost every step.
    calibrated: Cache,
    /// Whether the automatic stretch is applied to every image shown.
    pub stretch_enabled: bool,
    /// How the stretch is chosen.
    pub stretch_params: StretchParams,
    /// The most recent transient message.
    pub toast: Option<Toast>,
    /// Decodes images off the UI thread and caches the results.
    loader: Loader,
    /// How files are deleted and renamed. Swapped in tests so that nothing
    /// reaches the real trash.
    ops: Box<dyn FileOps + Send>,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    /// A model with nothing loaded.
    ///
    /// The viewport starts at the window's default size so that actions
    /// arriving before the first frame, such as opening a file named on the
    /// command line, have something sensible to fit against.
    #[must_use]
    pub fn new() -> Self {
        Self {
            folder: None,
            loaded: None,
            view: ViewState::default(),
            error: None,
            needs_fit: false,
            generation: 0,
            viewport: Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0)),
            loading: false,
            pending: Pending::None,
            confirm_every_delete: false,
            show_help: false,
            sort_key: SortKey::default(),
            pointer: None,
            show_filelist: true,
            scroll_to_selection: false,
            show_histogram: false,
            show_header: true,
            header_filter: String::new(),
            calibration: Calibration::default(),
            bayer: Bayer::default(),
            job: None,
            calibrated: Cache::new(CALIBRATED_CACHE_ENTRIES, CALIBRATED_CACHE_BYTES),
            stretch_enabled: false,
            stretch_params: StretchParams::default(),
            toast: None,
            loader: Loader::default(),
            ops: Box::new(RealFileOps),
        }
    }

    /// A model that performs file operations through `ops`.
    ///
    /// Tests use this so that deleting never reaches the real trash.
    #[must_use]
    pub fn with_file_ops(ops: Box<dyn FileOps + Send>) -> Self {
        Self { ops, ..Self::new() }
    }

    /// Applies an action.
    ///
    /// Actions that cannot be satisfied, such as zooming with no image loaded,
    /// are ignored rather than treated as errors.
    pub fn handle(&mut self, action: Action) {
        match action {
            Action::Open(path) => self.open(&path),
            Action::Select(index) => {
                // A click already has the row under the pointer.
                self.move_selection(|f| f.select(index));
                self.scroll_to_selection = false;
            }
            Action::NextFile => {
                self.move_selection(Folder::select_next);
                self.scroll_to_selection = true;
            }
            Action::PreviousFile => {
                self.move_selection(Folder::select_previous);
                self.scroll_to_selection = true;
            }
            Action::FirstFile => {
                self.move_selection(Folder::select_first);
                self.scroll_to_selection = true;
            }
            Action::LastFile => {
                self.move_selection(Folder::select_last);
                self.scroll_to_selection = true;
            }
            Action::Rescan => self.rescan(),
            Action::FitToWindow => {
                if let Some(l) = &self.loaded {
                    self.view = ViewState::fit(l.size(), self.viewport);
                }
            }
            Action::ActualSize => {
                if self.loaded.is_some() {
                    let viewport = self.viewport;
                    self.view.set_zoom_about_centre(viewport, 1.0);
                }
            }
            Action::Pan(delta) => {
                if self.loaded.is_some() {
                    self.view.pan(delta);
                }
            }
            Action::ZoomAt { anchor, factor } => {
                if self.loaded.is_some() {
                    self.view.zoom_about(anchor, factor);
                }
            }
            Action::ToggleFlag => self.toggle_flag(),
            Action::RequestDelete => self.request_delete(),
            Action::ConfirmDelete => self.delete_now(),
            Action::BeginRename => self.begin_rename(),
            Action::RenameTextChanged(text) => self.check_rename(&text),
            Action::CommitRename(text) => self.commit_rename(&text),
            Action::Cancel => self.pending = Pending::None,
            Action::ToggleConfirmEveryDelete => {
                self.confirm_every_delete = !self.confirm_every_delete;
            }
            Action::AddDarks(paths) => {
                let added = paths.len();
                self.calibration.dark_sources.extend(paths);
                self.calibration.dark_sources.sort();
                self.calibration.dark_sources.dedup();
                self.toast = Some(Toast::new(format!(
                    "{added} dark{} added, {} in total",
                    if added == 1 { "" } else { "s" },
                    self.calibration.dark_sources.len()
                )));
            }
            Action::BuildMasterDark => self.build_master_dark(),
            Action::LoadMasterDark(path) => self.load_master_dark(&path),
            Action::SaveMasterDark(path) => self.save_master_dark(&path),
            Action::ClearDarks => {
                self.calibration = Calibration::default();
                self.calibrated.clear();
                self.refresh_after_calibration_change();
                self.toast = Some(Toast::new("Calibration cleared"));
            }
            Action::ToggleApplyDark => {
                if !self.calibration.has_dark() {
                    return;
                }
                self.calibration.apply_dark = !self.calibration.apply_dark;
                self.refresh_after_calibration_change();
                self.toast = Some(Toast::new(if self.calibration.apply_dark {
                    "Dark applied"
                } else {
                    "Dark not applied"
                }));
            }
            Action::AddFlats(paths) => {
                let added = paths.len();
                self.calibration.flat_sources.extend(paths);
                self.calibration.flat_sources.sort();
                self.calibration.flat_sources.dedup();
                self.toast = Some(Toast::new(format!(
                    "{added} flat{} added, {} in total",
                    if added == 1 { "" } else { "s" },
                    self.calibration.flat_sources.len()
                )));
            }
            Action::AddFlatDarks(paths) => {
                self.calibration.flat_dark_sources.extend(paths);
                self.calibration.flat_dark_sources.sort();
                self.calibration.flat_dark_sources.dedup();
                self.toast = Some(Toast::new(format!(
                    "{} flat darks",
                    self.calibration.flat_dark_sources.len()
                )));
            }
            Action::BuildMasterFlat => self.build_master_flat(),
            Action::LoadMasterFlat(path) => self.load_master_flat(&path),
            Action::SaveMasterFlat(path) => self.save_master_flat(&path),
            Action::ClearFlats => {
                self.calibration.flat = None;
                self.calibration.flat_sources.clear();
                self.calibration.flat_dark_sources.clear();
                self.calibration.apply_flat = false;
                self.refresh_after_calibration_change();
                self.toast = Some(Toast::new("Flats cleared"));
            }
            Action::ToggleApplyFlat => {
                if !self.calibration.has_flat() {
                    return;
                }
                self.calibration.apply_flat = !self.calibration.apply_flat;
                self.refresh_after_calibration_change();
                self.toast = Some(Toast::new(if self.calibration.apply_flat {
                    "Flat applied"
                } else {
                    "Flat not applied"
                }));
            }
            Action::ToggleDebayer => {
                if self.bayer.pattern.is_none() {
                    // Nothing to reconstruct with; the panel offers a chooser.
                    self.bayer.pattern = Some(BayerPattern::Rggb);
                    self.bayer.from_header = false;
                }
                self.bayer.enabled = !self.bayer.enabled;
                self.refresh_after_calibration_change();
                self.remember_calibration();
                self.toast = Some(Toast::new(if self.bayer.enabled {
                    "Colour reconstruction on"
                } else {
                    "Colour reconstruction off"
                }));
            }
            Action::SetBayerPattern(pattern) => {
                if self.bayer.pattern != Some(pattern) {
                    self.bayer.pattern = Some(pattern);
                    self.bayer.from_header = false;
                    self.refresh_after_calibration_change();
                    self.remember_calibration();
                }
            }
            Action::ToggleBayerFlip => {
                self.bayer.flip_rows = !self.bayer.flip_rows;
                self.refresh_after_calibration_change();
                self.remember_calibration();
            }
            Action::MeasureFolder => self.measure_folder(),
            Action::SortBy(key) => {
                if self.sort_key != key {
                    self.sort_key = key;
                    if let Some(folder) = self.folder.as_mut() {
                        folder.sort_by(key);
                    }
                    self.scroll_to_selection = true;
                }
            }
            Action::StartExport(directory) => self.start_export(directory),
            Action::CancelJob => {
                if let Some(job) = &self.job {
                    job.cancel();
                    self.toast = Some(Toast::new("Stopping…"));
                }
            }
            Action::ToggleStretch => {
                self.stretch_enabled = !self.stretch_enabled;
                self.invalidate_texture();
                self.toast = Some(Toast::new(if self.stretch_enabled {
                    "Stretch on"
                } else {
                    "Stretch off"
                }));
            }
            Action::SetStretchParams(params) => {
                if params != self.stretch_params {
                    self.stretch_params = params;
                    if self.stretch_enabled {
                        self.invalidate_texture();
                    }
                }
            }
            Action::ResetStretchParams => {
                self.stretch_params = StretchParams::default();
                if self.stretch_enabled {
                    self.invalidate_texture();
                }
            }
            Action::ToggleFileList => self.show_filelist = !self.show_filelist,
            Action::SetFileListVisible(visible) => self.show_filelist = visible,
            Action::ToggleHistogram => self.show_histogram = !self.show_histogram,
            Action::ToggleHeader => self.show_header = !self.show_header,
            Action::SetHeaderFilter(text) => self.header_filter = text,
            Action::ToggleHelp => self.show_help = !self.show_help,
            Action::ClearError => self.error = None,
        }
    }

    /// Turns the selected file's keep flag on or off and records it.
    fn toggle_flag(&mut self) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        let Some(Outcome::Flagged { name, flagged }) = actions::toggle_flag(folder) else {
            return;
        };
        if let Err(e) = actions::save_flags(folder) {
            // The flag is set in memory either way; say so rather than
            // pretending it will survive a restart.
            self.error = Some(format!("Could not save flags: {e}"));
        }
        self.toast = Some(Toast::new(if flagged {
            format!("Keeping {name}")
        } else {
            format!("No longer keeping {name}")
        }));
    }

    /// Deletes, or asks first when the file is flagged or the user has asked to
    /// be asked every time.
    fn request_delete(&mut self) {
        let Some(folder) = self.folder.as_ref() else {
            return;
        };
        let Some(entry) = folder.selected_entry() else {
            return;
        };
        if actions::needs_delete_confirmation(folder, self.confirm_every_delete) {
            self.pending = Pending::DeleteConfirm {
                name: entry.name.clone(),
                flagged: entry.flagged,
            };
        } else {
            self.delete_now();
        }
    }

    /// Performs the delete. The confirmation, if any, has already happened.
    fn delete_now(&mut self) {
        self.pending = Pending::None;
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        match actions::delete_selected(folder, self.ops.as_ref()) {
            Ok(Outcome::Deleted { name }) => {
                let hint = if actions::undo_supported() {
                    " — restore it from the trash"
                } else {
                    ""
                };
                self.toast = Some(Toast::new(format!("Deleted {name}{hint}")));
                self.error = None;
                self.after_list_changed();
            }
            Ok(_) => {}
            Err(e) => self.error = Some(format!("Could not delete: {e}")),
        }
    }

    /// Opens the rename editor with the current name in it.
    fn begin_rename(&mut self) {
        let Some(name) = self
            .folder
            .as_ref()
            .and_then(Folder::selected_entry)
            .map(|e| e.name.clone())
        else {
            return;
        };
        self.pending = Pending::Rename {
            text: name,
            problem: None,
        };
    }

    /// Checks the typed name and reports what is wrong with it, if anything.
    ///
    /// Called as the user types so the problem appears before they commit.
    pub fn check_rename(&mut self, text: &str) {
        let problem = self.folder.as_ref().and_then(|f| {
            actions::validate_new_name(f, self.ops.as_ref(), text)
                .err()
                .map(|e| e.to_string())
        });
        self.pending = Pending::Rename {
            text: text.to_string(),
            problem,
        };
    }

    /// Applies a rename, keeping the editor open if the name is unusable.
    fn commit_rename(&mut self, text: &str) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        match actions::rename_selected(folder, self.ops.as_ref(), text) {
            Ok(Outcome::Renamed { from, to }) => {
                self.pending = Pending::None;
                if let Err(e) = actions::save_flags(folder) {
                    self.error = Some(format!("Could not save flags: {e}"));
                }
                self.toast = Some(Toast::new(format!("Renamed {from} to {to}")));
                self.after_list_changed();
            }
            Ok(_) => self.pending = Pending::None,
            Err(ActionError::Rename(e)) => {
                // Keep the editor open so the user can correct the name.
                self.pending = Pending::Rename {
                    text: text.to_string(),
                    problem: Some(e.to_string()),
                };
            }
            Err(e) => {
                self.pending = Pending::None;
                self.error = Some(format!("Could not rename: {e}"));
            }
        }
    }

    /// Brings the display back in step after the file list changes.
    fn after_list_changed(&mut self) {
        if self
            .folder
            .as_ref()
            .is_some_and(|f| f.selected_path().is_none())
        {
            self.loaded = None;
            self.loading = false;
            return;
        }
        self.show_selection();
    }

    /// Starts combining the collected darks on a background thread.
    fn build_master_dark(&mut self) {
        if self.job.is_some() {
            return;
        }
        if self.calibration.dark_sources.is_empty() {
            self.error = Some("Add some dark frames first".into());
            return;
        }
        self.job = Some(Job::build_master(self.calibration.dark_sources.clone()));
    }

    /// Starts combining the collected flats on a background thread.
    ///
    /// Flat darks, if any, are combined first on this thread; there are usually
    /// only a handful and they are short exposures.
    fn build_master_flat(&mut self) {
        if self.job.is_some() {
            return;
        }
        if self.calibration.flat_sources.is_empty() {
            self.error = Some("Add some flat frames first".into());
            return;
        }

        let flat_dark = if self.calibration.flat_dark_sources.is_empty() {
            None
        } else {
            let frames: Result<Vec<Arc<FitsImage>>, _> = self
                .calibration
                .flat_dark_sources
                .iter()
                .map(|p| fits_core::read_fits(p).map(Arc::new))
                .collect();
            match frames
                .map_err(|e| e.to_string())
                .and_then(|f| calib::build_master_median(&f).map_err(|e| e.to_string()))
            {
                Ok(master) => Some(Arc::new(master)),
                Err(message) => {
                    self.error = Some(format!("Flat darks: {message}"));
                    return;
                }
            }
        };

        self.job = Some(Job::build_flat(
            self.calibration.flat_sources.clone(),
            flat_dark,
        ));
    }

    /// Loads a gain map saved earlier, or any frame to normalise into one.
    fn load_master_flat(&mut self, path: &Path) {
        match fits_core::read_fits(path) {
            Ok(image) => {
                self.calibration.flat = Some(Arc::new(MasterFlat::from_image(&image)));
                self.calibration.flat_sources = vec![path.to_path_buf()];
                self.calibration.flat_path = Some(path.to_path_buf());
                self.calibration.apply_flat = true;
                self.refresh_after_calibration_change();
                self.remember_calibration();
                self.toast = Some(Toast::new("Master flat loaded"));
            }
            Err(e) => self.error = Some(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the gain map so it can be reused in another session.
    fn save_master_flat(&mut self, path: &Path) {
        let Some(flat) = self.calibration.flat.as_ref() else {
            return;
        };
        let history = vec![format!(
            "fitsview: master flat combined from {} frames",
            flat.source_count
        )];
        match fits_core::write_fits(path, &flat.to_image(), &history) {
            Ok(()) => {
                self.calibration.flat_path = Some(path.to_path_buf());
                self.remember_calibration();
                self.toast = Some(Toast::new("Master flat saved"));
            }
            Err(e) => self.error = Some(format!("Could not save: {e}")),
        }
    }

    /// Loads a master that was saved earlier, or any single frame to use as one.
    fn load_master_dark(&mut self, path: &Path) {
        match fits_core::read_fits(path) {
            Ok(image) => {
                self.calibration.dark = Some(Arc::new(MasterFrame::from_image(&image)));
                self.calibration.dark_sources = vec![path.to_path_buf()];
                self.calibration.dark_path = Some(path.to_path_buf());
                self.calibration.apply_dark = true;
                self.calibrated.clear();
                self.refresh_after_calibration_change();
                self.remember_calibration();
                self.toast = Some(Toast::new("Master dark loaded"));
            }
            Err(e) => self.error = Some(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the master dark so it can be reused in another session.
    fn save_master_dark(&mut self, path: &Path) {
        let Some(dark) = self.calibration.dark.as_ref() else {
            return;
        };
        let history = vec![format!(
            "fitsview: master dark combined from {} frames",
            dark.source_count
        )];
        match fits_core::write_fits(path, &dark.to_image(), &history) {
            Ok(()) => {
                self.calibration.dark_path = Some(path.to_path_buf());
                self.remember_calibration();
                self.toast = Some(Toast::new("Master dark saved"));
            }
            Err(e) => self.error = Some(format!("Could not save: {e}")),
        }
    }

    /// Starts writing calibrated copies of the folder.
    fn start_export(&mut self, directory: PathBuf) {
        if self.job.is_some() {
            return;
        }
        let Some(folder) = self.folder.as_ref() else {
            return;
        };
        if folder.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = folder.files.iter().map(|e| e.path.clone()).collect();
        self.job = Some(Job::export(
            paths,
            self.calibration.dark.clone(),
            self.calibration.flat.clone(),
            directory,
        ));
    }

    /// Starts measuring every file in the folder.
    fn measure_folder(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(folder) = self.folder.as_ref() else {
            return;
        };
        if folder.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = folder.files.iter().map(|e| e.path.clone()).collect();
        self.job = Some(Job::measure(paths));
    }

    /// Collects progress from the background job. Called once per frame.
    ///
    /// Returns true if anything changed.
    fn poll_job(&mut self) -> bool {
        let Some(job) = self.job.as_mut() else {
            return false;
        };
        let updates = job.poll();
        let finished = job.is_finished();

        for update in updates {
            match update {
                jobs::Update::Finished(jobs::Outcome::Master(master)) => {
                    self.calibration.dark = Some(Arc::new(*master));
                    self.calibration.apply_dark = true;
                    self.calibrated.clear();
                    self.refresh_after_calibration_change();
                    self.toast = Some(Toast::new(self.calibration.summary()));
                }
                jobs::Update::Finished(jobs::Outcome::Flat(flat)) => {
                    if flat.unusable > 0 {
                        log::warn!("master flat has {} unusable pixels", flat.unusable);
                    }
                    self.calibration.flat = Some(Arc::new(*flat));
                    self.calibration.apply_flat = true;
                    self.refresh_after_calibration_change();
                    self.toast = Some(Toast::new(self.calibration.flat_summary()));
                }
                jobs::Update::Finished(jobs::Outcome::Measured(measured)) => {
                    let count = measured.len();
                    if let Some(folder) = self.folder.as_mut() {
                        for (path, quality) in measured {
                            folder.set_quality(&path, quality);
                        }
                        // Keep whatever ordering is in force, now that more
                        // files have a value to order by.
                        folder.sort_by(self.sort_key);
                    }
                    self.toast = Some(Toast::new(format!("Measured {count} frames")));
                }
                jobs::Update::Finished(jobs::Outcome::Exported { written, directory }) => {
                    self.toast = Some(Toast::new(format!(
                        "Wrote {written} file{} to {}",
                        if written == 1 { "" } else { "s" },
                        directory.display()
                    )));
                }
                jobs::Update::Cancelled => self.toast = Some(Toast::new("Stopped")),
                jobs::Update::Failed(message) => self.error = Some(message),
                jobs::Update::Progress { .. } => {}
            }
        }

        if finished {
            self.job = None;
        }
        true
    }

    /// Records the master paths in the folder's sidecar, so reopening the
    /// folder restores the calibration setup.
    fn remember_calibration(&mut self) {
        let Some(folder) = self.folder.as_ref() else {
            return;
        };
        let mut existing = sidecar::load(&folder.dir);
        existing.master_dark = self
            .calibration
            .dark_path
            .as_ref()
            .map(|p| p.display().to_string());
        existing.master_flat = self
            .calibration
            .flat_path
            .as_ref()
            .map(|p| p.display().to_string());
        existing.bayer_pattern = self.bayer.pattern.map(|p| p.name().to_string());
        existing.bayer_flip_rows = self.bayer.flip_rows;
        existing.debayer = self.bayer.enabled;
        if let Err(e) = sidecar::save(&folder.dir, &existing) {
            log::warn!("could not record calibration paths: {e}");
        }
    }

    /// Reloads the masters a previous session used with this folder.
    ///
    /// A path that no longer exists is skipped quietly: calibration frames get
    /// moved and deleted, and refusing to open the folder over it would be
    /// worse than starting without them.
    fn restore_calibration(&mut self) {
        let Some(folder) = self.folder.as_ref() else {
            return;
        };
        let saved = sidecar::load(&folder.dir);
        let existing_error = self.error.clone();

        if let Some(path) = saved.master_dark.as_ref().map(PathBuf::from) {
            if path.exists() {
                self.load_master_dark(&path);
            }
        }
        if let Some(path) = saved.master_flat.as_ref().map(PathBuf::from) {
            if path.exists() {
                self.load_master_flat(&path);
            }
        }

        // A remembered pattern was a deliberate choice, so it wins over
        // whatever the next file's header happens to say.
        if let Some(pattern) = saved.bayer_pattern.as_deref().and_then(BayerPattern::parse) {
            self.bayer.pattern = Some(pattern);
            self.bayer.from_header = false;
            self.bayer.flip_rows = saved.bayer_flip_rows;
            self.bayer.enabled = saved.debayer;
        }
        // Restoring is not itself news, so undo anything those loads announced.
        // The scan's own message, such as an empty folder, must survive.
        self.toast = None;
        self.error = existing_error;
    }

    /// Re-examines whether the master suits the current image, and redisplays.
    fn refresh_after_calibration_change(&mut self) {
        self.calibrated.clear();
        self.update_calibration_warnings();
        self.show_selection();
        self.invalidate_texture();
    }

    /// Works out whether the master can be applied to what is on screen.
    fn update_calibration_warnings(&mut self) {
        self.calibration.blocked = None;
        self.calibration.warnings.clear();

        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        // Against the frame as it came off the disk, since that is what a
        // calibration frame describes.
        let raw = &loaded.raw;
        let shape = format!("{}x{}x{}", raw.width, raw.height, raw.channels);

        // Dimensions are the one mismatch that cannot be worked around, so they
        // block. Everything else is advisory.
        let mut blocked = None;
        let mut warnings = Vec::new();

        if let Some(dark) = self.calibration.dark.as_ref() {
            if dark.matches(raw) {
                warnings.extend(calib::check_compatibility(dark, raw).warnings);
            } else {
                blocked = Some(format!(
                    "The dark is {}, but this image is {shape}",
                    dark.shape()
                ));
            }
        }

        if let Some(flat) = self.calibration.flat.as_ref() {
            if flat.matches(raw) {
                if flat.unusable > 0 {
                    warnings.push(format!(
                        "{} pixels of the flat carry too little signal and will show as blank",
                        flat.unusable
                    ));
                }
            } else {
                blocked = Some(format!(
                    "The flat is {}, but this image is {shape}",
                    flat.shape()
                ));
            }
        }

        self.calibration.blocked = blocked;
        self.calibration.warnings = warnings;
    }

    /// Marks the uploaded texture as stale without reloading the image.
    ///
    /// The generation counter is what the drawing layer compares against, so
    /// bumping it is enough to force a rebuild with the current tone mapping.
    fn invalidate_texture(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Drops the toast once its time is up. Called each frame.
    pub fn expire_toast(&mut self) {
        if self.toast.as_ref().is_some_and(|t| !t.is_live()) {
            self.toast = None;
        }
    }

    /// Opens a folder, or a file together with the folder containing it.
    ///
    /// Opening a single file still loads its folder, so the arrow keys work
    /// immediately without the user having to open the folder separately.
    fn open(&mut self, path: &Path) {
        let (dir, wanted) = if path.is_dir() {
            (path.to_path_buf(), None)
        } else {
            match path.parent() {
                Some(parent) => (parent.to_path_buf(), Some(path.to_path_buf())),
                None => {
                    self.error = Some(format!("{} has no parent folder", path.display()));
                    return;
                }
            }
        };

        match scan_folder(&dir) {
            Ok(mut folder) => {
                if let Some(w) = &wanted {
                    if !folder.select_path(w) {
                        // The file exists but the scan filtered it out, most
                        // likely a non-FITS file dropped on the window.
                        self.error = Some(format!(
                            "{} is not a FITS file",
                            w.file_name().unwrap_or(w.as_os_str()).to_string_lossy()
                        ));
                    }
                }
                if folder.is_empty() {
                    self.error = Some(format!("No FITS files in {}", dir.display()));
                }
                self.loader.reset();
                self.folder = Some(folder);
                self.show_selection();
                self.restore_calibration();
            }
            Err(e) => {
                log::warn!("could not scan {}: {e}", dir.display());
                self.error = Some(format!("{}: {e}", dir.display()));
            }
        }
    }

    /// Re-reads the folder, keeping the selection on the same file if it is
    /// still there.
    fn rescan(&mut self) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        match folder.rescan() {
            Ok(()) => {
                // Cached images may be stale, and files may have gone.
                self.loader.reset();
                self.show_selection();
            }
            Err(e) => self.error = Some(format!("Rescan failed: {e}")),
        }
    }

    /// Applies a selection change and displays whatever it lands on.
    fn move_selection(&mut self, change: impl FnOnce(&mut Folder)) {
        let Some(folder) = self.folder.as_mut() else {
            return;
        };
        let before = folder.selected;
        change(folder);
        if folder.selected != before {
            self.show_selection();
        }
    }

    /// Displays the selected file, from cache if possible, and asks the loader
    /// for it and its neighbours otherwise.
    fn show_selection(&mut self) {
        // Take what is needed from the folder up front, so the borrow ends
        // before anything mutates the model.
        let Some((path, wanted)) = self
            .folder
            .as_ref()
            .map(|f| (f.selected_path().map(Path::to_path_buf), f.prefetch_paths()))
        else {
            return;
        };
        let Some(path) = path else {
            self.loaded = None;
            self.loading = false;
            return;
        };

        // A cached image is shown at once, so stepping back and forth through
        // a folder never flickers.
        if let Some(image) = self.loader.cache_mut().get(&path) {
            self.display(path, image, None);
            self.loading = false;
        } else {
            self.loading = true;
        }

        self.loader.request(&wanted);
    }

    /// Collects finished loads. Call once per frame.
    ///
    /// Returns true if anything changed, so the caller knows to repaint.
    pub fn poll(&mut self) -> bool {
        let job_changed = self.poll_job();
        let arrivals = self.loader.poll();
        if arrivals.is_empty() {
            return job_changed;
        }

        let selected = self
            .folder
            .as_ref()
            .and_then(|f| f.selected_path())
            .map(Path::to_path_buf);

        let mut changed = job_changed;
        for arrival in arrivals {
            let is_selected = selected.as_deref() == Some(arrival.path.as_path());
            // The worker measured it while it had the samples to hand, so this
            // costs nothing here.
            if let (Some(quality), Some(folder)) = (arrival.quality, self.folder.as_mut()) {
                folder.set_quality(&arrival.path, quality);
            }

            match arrival.result {
                Ok(image) => {
                    if is_selected {
                        self.display(arrival.path, image, Some(arrival.millis));
                        self.loading = false;
                        changed = true;
                    }
                }
                Err(message) => {
                    // Only complain about the file the user is looking at. A
                    // prefetch that fails will be reported when they reach it.
                    if is_selected {
                        log::warn!("could not load {}: {message}", arrival.path.display());
                        self.error = Some(format!("{}: {message}", arrival.path.display()));
                        self.loading = false;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Adopts the filter pattern a file declares, if none has been chosen.
    ///
    /// Done once rather than per image: every frame in a folder comes from the
    /// same camera, and re-reading it each time would undo a deliberate choice.
    /// A file that says it is a colour raw is displayed as one without being
    /// asked, since that is plainly what it wants.
    fn adopt_bayer_pattern(&mut self, image: &FitsImage) {
        if self.bayer.pattern.is_some() || image.channels != 1 {
            return;
        }
        let Some(pattern) = debayer::detect(&image.header) else {
            return;
        };
        log::debug!("the file declares a {} filter pattern", pattern.name());
        self.bayer.pattern = Some(pattern);
        self.bayer.from_header = true;
        self.bayer.enabled = true;
    }

    /// Puts an image on screen, calibrated if a master is in use.
    ///
    /// The calibrated result is cached, so stepping back to a file already
    /// visited does not subtract the dark a second time.
    fn display(&mut self, path: PathBuf, image: Arc<FitsImage>, millis: Option<f64>) {
        let load_ms = millis.unwrap_or(0.0);
        self.adopt_bayer_pattern(&image);

        let shown = self.calibrated_version(&path, &image);
        let image = Arc::clone(&image);

        self.loaded = Some(Loaded {
            path,
            image: shown,
            raw: image,
            load_ms,
        });
        self.error = None;
        self.needs_fit = true;
        self.generation = self.generation.wrapping_add(1);
        self.update_calibration_warnings();
    }

    /// The image as it should be displayed: calibrated, or the original.
    fn calibrated_version(&mut self, path: &Path, image: &Arc<FitsImage>) -> Arc<FitsImage> {
        if !self.calibration.is_active() && self.bayer.active().is_none() {
            return Arc::clone(image);
        }
        let dark = self.calibration.active_dark().filter(|d| d.matches(image));
        let flat = self.calibration.active_flat().filter(|f| f.matches(image));
        let pattern = self.bayer.active();
        if dark.is_none() && flat.is_none() && pattern.is_none() {
            // Reported through `blocked`; showing the raw image beats showing
            // nothing.
            return Arc::clone(image);
        }
        if let Some(cached) = self.calibrated.get(path) {
            return cached;
        }
        match calib::calibrate_and_debayer(image, dark, flat, pattern) {
            Ok(result) => {
                let result = Arc::new(result);
                self.calibrated
                    .insert(path.to_path_buf(), Arc::clone(&result));
                result
            }
            Err(e) => {
                log::warn!("could not calibrate {}: {e}", path.display());
                Arc::clone(image)
            }
        }
    }

    /// Records the viewport and, if an image has just arrived, fits it.
    ///
    /// Called once per frame by the drawing layer, which is the only place the
    /// real viewport size is known.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
        if self.needs_fit {
            if let Some(l) = &self.loaded {
                self.view = ViewState::fit(l.size(), viewport);
                self.needs_fit = false;
            }
        }
    }

    /// The pixel under the pointer, if the pointer is over the image.
    ///
    /// Reports the sample as the **file** holds it, not as it is displayed.
    /// Calibration, stretching and colour reconstruction all change what is on
    /// screen; a readout of those would answer a question nobody asked. What is
    /// wanted is whether the star is saturated in the data.
    #[must_use]
    pub fn pixel_readout(&self) -> Option<PixelReadout> {
        let loaded = self.loaded.as_ref()?;
        let pointer = self.pointer?;
        let raw = &loaded.raw;

        let position = self.view.screen_to_image(pointer);
        if position.x < 0.0 || position.y < 0.0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (x, y) = (position.x as usize, position.y as usize);
        if x >= raw.width || y >= raw.height {
            return None;
        }

        // Display row 0 is the top of the picture; the file stores the bottom
        // row first.
        let source_y = raw.height - 1 - y;
        let plane = raw.width * raw.height;
        let index = source_y * raw.width + x;

        // On a one-shot colour frame each pixel measured only one colour, so a
        // single number would be misleading: neighbouring values differ because
        // they sit under different filters, not because the sky does. When
        // colour is being reconstructed, report the same three values the
        // picture is showing, from the raw samples rather than the calibrated
        // ones.
        if raw.channels == 1 {
            if let Some(pattern) = self.bayer.active() {
                return Some(PixelReadout {
                    x,
                    y,
                    values: debayer::colour_at(raw, pattern, x, source_y),
                    channels: 3,
                    reconstructed: true,
                });
            }
        }

        let mut values = [f32::NAN; 3];
        for (channel, slot) in values.iter_mut().enumerate().take(raw.channels.min(3)) {
            *slot = raw.data[channel * plane + index];
        }

        Some(PixelReadout {
            x,
            y,
            values,
            channels: raw.channels.min(3),
            reconstructed: false,
        })
    }

    /// Text for the status bar.
    #[must_use]
    pub fn status_text(&self) -> String {
        if let Some(e) = &self.error {
            return format!("Error: {e}");
        }
        if let Some(l) = &self.loaded {
            return format!("{}  {:.0}%", l.summary(), self.view.zoom * 100.0);
        }
        if self.loading {
            return "Loading…".to_string();
        }
        "No image. Use Open File or Open Folder, or drop a FITS file on the window.".to_string()
    }

    /// Cached images and the bytes they occupy.
    ///
    /// Exposed so that the memory bound can be asserted in tests and shown in
    /// a diagnostic panel later, rather than being taken on trust.
    #[must_use]
    pub fn cache_stats(&self) -> (usize, usize) {
        (self.loader.cache().len(), self.loader.cache().bytes())
    }

    /// The `3 / 142` position label, empty when no folder is open.
    #[must_use]
    pub fn position_label(&self) -> String {
        self.folder
            .as_ref()
            .map(Folder::position_label)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::testutil::{write_synthetic, SyntheticSpec};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Writes `count` numbered files of the given size into a fresh folder.
    fn folder_of(count: usize, width: usize, height: usize) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(width, height, 16);
        let pixels: Vec<f64> = (0..width * height).map(|i| (i % 1000) as f64).collect();
        for i in 1..=count {
            write_synthetic(dir.path(), &format!("light_{i}.fits"), &spec, &pixels).unwrap();
        }
        dir
    }

    /// A single file in its own folder.
    fn sample(width: usize, height: usize) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(width, height, 16);
        let pixels: Vec<f64> = (0..width * height).map(|i| (i % 1000) as f64).collect();
        let path = write_synthetic(dir.path(), "light.fits", &spec, &pixels).unwrap();
        (dir, path)
    }

    /// Pumps the model until it settles: an image is shown, or an error is
    /// recorded, or nothing is outstanding. Loading is asynchronous now, so
    /// tests must wait rather than assume.
    fn settle(model: &mut Model) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            model.poll();
            if !model.loading && (model.loaded.is_some() || model.error.is_some()) {
                return;
            }
            if !model.loading && model.folder.as_ref().is_some_and(Folder::is_empty) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!(
            "model never settled: loading={}, error={:?}",
            model.loading, model.error
        );
    }

    fn viewport() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))
    }

    #[test]
    fn a_new_model_has_nothing_loaded() {
        let m = Model::new();
        assert!(m.loaded.is_none());
        assert!(m.folder.is_none());
        assert!(m.error.is_none());
        assert!(m.status_text().contains("No image"));
        assert_eq!(m.position_label(), "");
    }

    #[test]
    fn opening_a_file_also_opens_its_folder() {
        // So that the arrow keys work without opening the folder separately.
        let dir = folder_of(3, 20, 15);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().join("light_2.fits")));
        settle(&mut m);

        let folder = m.folder.as_ref().expect("folder should be open");
        assert_eq!(folder.len(), 3);
        assert_eq!(folder.selected_entry().unwrap().name, "light_2.fits");
        assert_eq!(m.position_label(), "2 / 3");
        assert_eq!(m.loaded.as_ref().unwrap().image.width, 20);
    }

    #[test]
    fn opening_a_folder_selects_its_first_file() {
        let dir = folder_of(4, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        assert_eq!(m.position_label(), "1 / 4");
        assert_eq!(
            m.folder.as_ref().unwrap().selected_entry().unwrap().name,
            "light_1.fits"
        );
        assert!(m.loaded.is_some());
    }

    #[test]
    fn a_folder_with_no_fits_files_reports_that_rather_than_failing_silently() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"nothing here").unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        assert!(m.error.is_some(), "expected an explanatory error");
        assert!(m.error.as_ref().unwrap().contains("No FITS files"));
        assert!(m.loaded.is_none());
    }

    #[test]
    fn opening_a_missing_path_does_not_panic() {
        let mut m = Model::new();
        m.handle(Action::Open(PathBuf::from("/nowhere/at/all.fits")));
        assert!(m.error.is_some());
        assert!(m.loaded.is_none());
    }

    #[test]
    fn opening_a_corrupt_file_reports_it() {
        let dir = folder_of(1, 10, 10);
        let bad = dir.path().join("broken.fits");
        std::fs::write(&bad, b"SIMPLE but not really a fits file").unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(bad));
        settle(&mut m);
        assert!(m.error.is_some(), "a corrupt file should be reported");
        assert!(m.loaded.is_none());
    }

    #[test]
    fn arrow_navigation_moves_through_the_folder_and_stops_at_the_ends() {
        let dir = folder_of(3, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        m.handle(Action::PreviousFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 3", "must not wrap backwards");

        m.handle(Action::NextFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "2 / 3");

        m.handle(Action::LastFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "3 / 3");

        m.handle(Action::NextFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "3 / 3", "must not wrap forwards");

        m.handle(Action::FirstFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 3");
    }

    #[test]
    fn every_file_in_a_folder_can_be_stepped_through() {
        let dir = folder_of(6, 12, 9);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        let mut seen = Vec::new();
        for _ in 0..6 {
            seen.push(
                m.loaded
                    .as_ref()
                    .unwrap()
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            m.handle(Action::NextFile);
            settle(&mut m);
        }
        assert_eq!(
            seen,
            vec![
                "light_1.fits",
                "light_2.fits",
                "light_3.fits",
                "light_4.fits",
                "light_5.fits",
                "light_6.fits"
            ]
        );
    }

    #[test]
    fn selecting_by_index_shows_that_file() {
        let dir = folder_of(5, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        m.handle(Action::Select(3));
        settle(&mut m);
        assert_eq!(m.position_label(), "4 / 5");

        // Out of range is ignored rather than clearing the view.
        m.handle(Action::Select(99));
        settle(&mut m);
        assert_eq!(m.position_label(), "4 / 5");
    }

    #[test]
    fn stepping_back_to_a_visited_file_is_served_from_cache_without_reloading() {
        // The prefetch and cache exist so that going back is instant. If the
        // image were reloaded, `loading` would be set on the way back.
        let dir = folder_of(3, 40, 30);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        m.handle(Action::NextFile);
        settle(&mut m);
        m.handle(Action::PreviousFile);
        assert!(
            !m.loading,
            "returning to a cached image should not need loading"
        );
        assert_eq!(m.position_label(), "1 / 3");
        assert!(m.loaded.is_some());
    }

    #[test]
    fn navigation_does_nothing_when_no_folder_is_open() {
        let mut m = Model::new();
        for a in [
            Action::NextFile,
            Action::PreviousFile,
            Action::FirstFile,
            Action::LastFile,
            Action::Select(0),
            Action::Rescan,
        ] {
            m.handle(a);
        }
        assert!(m.folder.is_none());
        assert!(m.loaded.is_none());
    }

    #[test]
    fn rescan_picks_up_a_new_file() {
        let dir = folder_of(2, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 2");

        let spec = SyntheticSpec::new(10, 10, 16);
        write_synthetic(dir.path(), "light_3.fits", &spec, &vec![1.0; 100]).unwrap();

        m.handle(Action::Rescan);
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 3");
    }

    #[test]
    fn rescan_keeps_the_selection_on_the_same_file() {
        let dir = folder_of(3, 10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);
        m.handle(Action::LastFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "3 / 3");

        m.handle(Action::Rescan);
        settle(&mut m);
        assert_eq!(
            m.folder.as_ref().unwrap().selected_entry().unwrap().name,
            "light_3.fits"
        );
    }

    #[test]
    fn opening_a_different_folder_replaces_the_first() {
        let a = folder_of(2, 10, 10);
        let b = folder_of(5, 10, 10);
        let mut m = Model::new();

        m.handle(Action::Open(a.path().to_path_buf()));
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 2");

        m.handle(Action::Open(b.path().to_path_buf()));
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 5");
        assert_eq!(m.folder.as_ref().unwrap().dir, b.path());
    }

    #[test]
    fn the_first_frame_fits_the_image_to_the_viewport() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(2000, 1000, 16);
        write_synthetic(dir.path(), "big.fits", &spec, &vec![1.0; 2_000_000]).unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);
        m.set_viewport(viewport());

        assert!(!m.needs_fit, "fit should be consumed");
        assert!((m.view.zoom - 0.5).abs() < 0.001, "zoom {}", m.view.zoom);
    }

    #[test]
    fn later_frames_do_not_refit_and_undo_the_users_zoom() {
        let (_dir, path) = sample(2000, 1000);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.set_viewport(viewport());

        m.handle(Action::ZoomAt {
            anchor: Pos2::new(500.0, 400.0),
            factor: 2.0,
        });
        let zoomed = m.view.zoom;

        m.set_viewport(viewport());
        assert!(
            (m.view.zoom - zoomed).abs() < f32::EPSILON,
            "a redraw must not reset the view"
        );
    }

    #[test]
    fn moving_to_another_file_refits_it() {
        let dir = tempfile::tempdir().unwrap();
        write_synthetic(
            dir.path(),
            "a_small.fits",
            &SyntheticSpec::new(100, 100, 16),
            &vec![1.0; 10_000],
        )
        .unwrap();
        write_synthetic(
            dir.path(),
            "b_large.fits",
            &SyntheticSpec::new(4000, 2000, 16),
            &vec![1.0; 8_000_000],
        )
        .unwrap();

        let mut m = Model::new();
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);
        m.set_viewport(viewport());
        assert!((m.view.zoom - 1.0).abs() < 0.001, "small image at 1:1");

        m.handle(Action::NextFile);
        settle(&mut m);
        m.set_viewport(viewport());
        assert!((m.view.zoom - 0.25).abs() < 0.001, "zoom {}", m.view.zoom);
    }

    #[test]
    fn actions_are_ignored_when_no_image_is_loaded() {
        let mut m = Model::new();
        let before = m.view;
        m.handle(Action::FitToWindow);
        m.handle(Action::ActualSize);
        m.handle(Action::Pan(Vec2::new(10.0, 10.0)));
        m.handle(Action::ZoomAt {
            anchor: Pos2::ZERO,
            factor: 2.0,
        });
        assert_eq!(m.view, before, "view must not move with nothing loaded");
    }

    #[test]
    fn actual_size_sets_one_to_one_zoom() {
        let (_dir, path) = sample(2000, 1000);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.set_viewport(viewport());
        assert!(m.view.zoom < 1.0);

        m.handle(Action::ActualSize);
        assert!((m.view.zoom - 1.0).abs() < 0.001, "zoom {}", m.view.zoom);
    }

    #[test]
    fn fit_after_zooming_returns_to_the_fitted_view() {
        let (_dir, path) = sample(2000, 1000);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.set_viewport(viewport());
        let fitted = m.view;

        m.handle(Action::ZoomAt {
            anchor: Pos2::new(100.0, 100.0),
            factor: 4.0,
        });
        m.handle(Action::Pan(Vec2::new(50.0, -30.0)));
        assert_ne!(m.view, fitted);

        m.handle(Action::FitToWindow);
        assert!((m.view.zoom - fitted.zoom).abs() < 0.001);
        assert!((m.view.origin.x - fitted.origin.x).abs() < 0.001);
        assert!((m.view.origin.y - fitted.origin.y).abs() < 0.001);
    }

    #[test]
    fn clearing_the_error_leaves_the_image_alone() {
        let (_dir, path) = sample(10, 10);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.error = Some("something".into());
        m.handle(Action::ClearError);
        assert!(m.error.is_none());
        assert!(m.loaded.is_some());
    }

    #[test]
    fn the_status_line_reports_size_bit_depth_and_load_time() {
        let (_dir, path) = sample(40, 30);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        let s = m.status_text();
        assert!(s.contains("light.fits"), "{s}");
        assert!(s.contains("40x30"), "{s}");
        assert!(s.contains("BITPIX 16"), "{s}");
        assert!(s.contains("ms"), "{s}");
    }

    #[test]
    fn the_status_line_reports_the_zoom_level() {
        let (_dir, path) = sample(100, 100);
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        m.set_viewport(viewport());
        m.handle(Action::ActualSize);
        assert!(m.status_text().contains("100%"), "{}", m.status_text());
    }

    #[test]
    fn a_colour_image_is_labelled_as_rgb() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(4, 4, 16).with_channels(3);
        let path = write_synthetic(dir.path(), "rgb.fits", &spec, &vec![1.0; 48]).unwrap();
        let mut m = Model::new();
        m.handle(Action::Open(path));
        settle(&mut m);
        assert!(m.status_text().contains("RGB"), "{}", m.status_text());
    }

    /// A model whose file operations are recorded rather than performed, so
    /// that tests never reach the real trash.
    #[derive(Debug, Default)]
    struct SpyOps {
        trashed: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl crate::actions::FileOps for std::sync::Arc<SpyOps> {
        fn trash(&self, path: &Path) -> Result<(), String> {
            self.trashed.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
        fn rename(&self, from: &Path, to: &Path) -> Result<(), String> {
            std::fs::rename(from, to).map_err(|e| e.to_string())
        }
        fn exists(&self, path: &Path) -> bool {
            path.exists()
        }
    }

    /// A model over `dir` that records deletes instead of performing them.
    fn model_over(dir: &Path) -> (Model, std::sync::Arc<SpyOps>) {
        let spy = std::sync::Arc::new(SpyOps::default());
        let mut m = Model::with_file_ops(Box::new(std::sync::Arc::clone(&spy)));
        m.handle(Action::Open(dir.to_path_buf()));
        settle(&mut m);
        (m, spy)
    }

    #[test]
    fn flagging_marks_the_file_and_survives_reopening_the_folder() {
        let dir = folder_of(3, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::NextFile);
        settle(&mut m);
        m.handle(Action::ToggleFlag);

        assert!(m.folder.as_ref().unwrap().files[1].flagged);
        assert!(m.toast.is_some(), "the user should be told");

        // Reopen, as though the application had been restarted.
        let (m2, _spy2) = model_over(dir.path());
        assert!(
            m2.folder.as_ref().unwrap().files[1].flagged,
            "the flag must survive a restart"
        );
        assert!(!m2.folder.as_ref().unwrap().files[0].flagged);
    }

    #[test]
    fn unflagging_is_persisted_too() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);
        m.handle(Action::ToggleFlag);

        let (m2, _spy2) = model_over(dir.path());
        assert!(!m2.folder.as_ref().unwrap().files[0].flagged);
    }

    #[test]
    fn deleting_an_unflagged_file_happens_at_once_with_no_dialog() {
        let dir = folder_of(3, 10, 10);
        let (mut m, spy) = model_over(dir.path());

        m.handle(Action::RequestDelete);
        assert_eq!(m.pending, Pending::None, "no dialog for an unflagged file");
        assert_eq!(spy.trashed.lock().unwrap().len(), 1);
        assert_eq!(m.position_label(), "1 / 2", "selection should advance");
        settle(&mut m);
        assert!(m.loaded.is_some(), "the next image should be shown");
    }

    #[test]
    fn deleting_a_flagged_file_always_asks_first() {
        let dir = folder_of(2, 10, 10);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);

        m.handle(Action::RequestDelete);
        match &m.pending {
            Pending::DeleteConfirm { name, flagged } => {
                assert_eq!(name, "light_1.fits");
                assert!(flagged);
            }
            other => panic!("expected a confirmation, got {other:?}"),
        }
        assert!(
            spy.trashed.lock().unwrap().is_empty(),
            "nothing may be deleted before the user confirms"
        );
    }

    #[test]
    fn cancelling_the_confirmation_keeps_the_file() {
        let dir = folder_of(2, 10, 10);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);
        m.handle(Action::RequestDelete);

        m.handle(Action::Cancel);
        assert_eq!(m.pending, Pending::None);
        assert!(spy.trashed.lock().unwrap().is_empty());
        assert_eq!(m.position_label(), "1 / 2", "the file is still there");
    }

    #[test]
    fn confirming_deletes_the_flagged_file() {
        let dir = folder_of(2, 10, 10);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);
        m.handle(Action::RequestDelete);

        m.handle(Action::ConfirmDelete);
        assert_eq!(m.pending, Pending::None);
        assert_eq!(spy.trashed.lock().unwrap().len(), 1);
        assert_eq!(m.position_label(), "1 / 1");
    }

    #[test]
    fn confirm_every_delete_asks_even_for_unflagged_files() {
        let dir = folder_of(2, 10, 10);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::ToggleConfirmEveryDelete);
        assert!(m.confirm_every_delete);

        m.handle(Action::RequestDelete);
        assert!(matches!(m.pending, Pending::DeleteConfirm { .. }));
        assert!(spy.trashed.lock().unwrap().is_empty());
    }

    #[test]
    fn deleting_the_last_remaining_file_clears_the_view() {
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::RequestDelete);
        assert_eq!(m.position_label(), "0 / 0");
        assert!(m.loaded.is_none(), "nothing left to show");
        assert!(!m.loading);
    }

    #[test]
    fn a_delete_that_fails_is_reported_and_changes_nothing() {
        #[derive(Debug)]
        struct AlwaysFails;
        impl crate::actions::FileOps for AlwaysFails {
            fn trash(&self, _: &Path) -> Result<(), String> {
                Err("permission denied".into())
            }
            fn rename(&self, _: &Path, _: &Path) -> Result<(), String> {
                Err("permission denied".into())
            }
            fn exists(&self, _: &Path) -> bool {
                false
            }
        }

        let dir = folder_of(2, 10, 10);
        let mut m = Model::with_file_ops(Box::new(AlwaysFails));
        m.handle(Action::Open(dir.path().to_path_buf()));
        settle(&mut m);

        m.handle(Action::RequestDelete);
        assert!(m.error.is_some(), "the failure must be surfaced");
        assert_eq!(m.position_label(), "1 / 2", "the file is still listed");
    }

    #[test]
    fn renaming_moves_the_file_on_disk_and_keeps_it_selected() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::BeginRename);
        match &m.pending {
            Pending::Rename { text, problem } => {
                assert_eq!(text, "light_1.fits", "editor starts with the current name");
                assert!(problem.is_none());
            }
            other => panic!("expected the rename editor, got {other:?}"),
        }

        m.handle(Action::CommitRename("m31_first.fits".into()));
        assert_eq!(m.pending, Pending::None);
        assert!(dir.path().join("m31_first.fits").exists());
        assert!(!dir.path().join("light_1.fits").exists());
        assert_eq!(
            m.folder.as_ref().unwrap().selected_entry().unwrap().name,
            "m31_first.fits"
        );
    }

    #[test]
    fn a_rename_that_clashes_keeps_the_editor_open_with_a_reason() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::BeginRename);
        m.handle(Action::CommitRename("light_2.fits".into()));

        match &m.pending {
            Pending::Rename { problem, .. } => {
                let problem = problem.as_ref().expect("should explain the clash");
                assert!(problem.contains("already exists"), "{problem}");
            }
            other => panic!("the editor should stay open, got {other:?}"),
        }
        assert!(dir.path().join("light_1.fits").exists(), "nothing moved");
    }

    #[test]
    fn typing_an_invalid_name_reports_it_before_committing() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::BeginRename);

        m.handle(Action::RenameTextChanged("sub/dir.fits".into()));
        match &m.pending {
            Pending::Rename { problem, .. } => {
                assert!(problem.is_some(), "should complain about the separator");
            }
            other => panic!("expected the editor, got {other:?}"),
        }

        m.handle(Action::RenameTextChanged("fine.fits".into()));
        match &m.pending {
            Pending::Rename { problem, .. } => assert!(problem.is_none()),
            other => panic!("expected the editor, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_a_rename_leaves_the_file_alone() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::BeginRename);
        m.handle(Action::Cancel);

        assert_eq!(m.pending, Pending::None);
        assert!(dir.path().join("light_1.fits").exists());
    }

    #[test]
    fn a_renamed_file_keeps_its_flag_across_a_restart() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::ToggleFlag);
        m.handle(Action::BeginRename);
        m.handle(Action::CommitRename("keeper.fits".into()));

        let (m2, _spy2) = model_over(dir.path());
        let entry = m2
            .folder
            .as_ref()
            .unwrap()
            .files
            .iter()
            .find(|e| e.name == "keeper.fits")
            .expect("renamed file should be listed");
        assert!(entry.flagged, "the flag must follow the new name");
    }

    #[test]
    fn the_stretch_toggles_and_forces_a_redraw() {
        // The texture is cached against the generation counter, so toggling a
        // display setting has to bump it or the change would not appear.
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        let before = m.generation;

        m.handle(Action::ToggleStretch);
        assert!(m.stretch_enabled);
        assert_ne!(m.generation, before, "the texture must be rebuilt");
        assert!(m.toast.is_some(), "the user should be told");

        m.handle(Action::ToggleStretch);
        assert!(!m.stretch_enabled);
    }

    #[test]
    fn the_stretch_can_be_toggled_with_no_image_open() {
        // It is a display setting, not an operation on a file.
        let mut m = Model::new();
        m.handle(Action::ToggleStretch);
        assert!(m.stretch_enabled);
    }

    #[test]
    fn changing_the_stretch_settings_redraws_only_when_the_stretch_is_on() {
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        let before = m.generation;
        m.handle(Action::SetStretchParams(StretchParams {
            target_bg: 0.4,
            ..StretchParams::default()
        }));
        assert_eq!(
            m.generation, before,
            "no need to redraw while the stretch is off"
        );
        assert!((m.stretch_params.target_bg - 0.4).abs() < f32::EPSILON);

        m.handle(Action::ToggleStretch);
        let before = m.generation;
        m.handle(Action::SetStretchParams(StretchParams {
            target_bg: 0.2,
            ..StretchParams::default()
        }));
        assert_ne!(m.generation, before, "a live change must be shown");
    }

    #[test]
    fn setting_the_same_stretch_parameters_does_not_redraw() {
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::ToggleStretch);

        let before = m.generation;
        m.handle(Action::SetStretchParams(m.stretch_params));
        assert_eq!(m.generation, before, "an unchanged setting is not a change");
    }

    #[test]
    fn resetting_the_stretch_settings_restores_the_defaults() {
        let mut m = Model::new();
        m.handle(Action::SetStretchParams(StretchParams {
            target_bg: 0.45,
            shadows_clip: -1.0,
        }));
        assert_ne!(m.stretch_params, StretchParams::default());

        m.handle(Action::ResetStretchParams);
        assert_eq!(m.stretch_params, StretchParams::default());
    }

    /// Writes `count` dark frames of a uniform level into their own folder.
    fn darks(count: usize, width: usize, height: usize, level: f64) -> (TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(width, height, 16);
        let paths = (0..count)
            .map(|i| {
                write_synthetic(
                    dir.path(),
                    &format!("dark_{i}.fits"),
                    &spec,
                    &vec![level; width * height],
                )
                .unwrap()
            })
            .collect();
        (dir, paths)
    }

    /// Pumps until the background job finishes.
    fn finish_job(model: &mut Model) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            model.poll();
            if model.job.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("job never finished");
    }

    #[test]
    fn building_a_master_dark_enables_calibration() {
        let dir = folder_of(2, 20, 15);
        let (_darks_dir, dark_paths) = darks(3, 20, 15, 50.0);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        assert_eq!(m.calibration.dark_sources.len(), 3);
        assert!(!m.calibration.has_dark(), "not combined yet");

        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        let dark = m.calibration.dark.as_ref().expect("a master should exist");
        assert_eq!(dark.source_count, 3);
        assert!(m.calibration.apply_dark, "building should switch it on");
        assert!(m.calibration.summary().contains("3 frames"));
    }

    #[test]
    fn adding_the_same_dark_twice_does_not_duplicate_it() {
        let dir = folder_of(1, 10, 10);
        let (_darks_dir, dark_paths) = darks(2, 10, 10, 50.0);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::AddDarks(dark_paths.clone()));
        m.handle(Action::AddDarks(dark_paths));
        assert_eq!(m.calibration.dark_sources.len(), 2);
    }

    #[test]
    fn building_with_no_darks_reports_it_rather_than_starting_a_job() {
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::BuildMasterDark);
        assert!(m.job.is_none());
        assert!(m.error.is_some());
    }

    #[test]
    fn applying_a_dark_changes_the_displayed_pixels() {
        // The point of the whole phase: what is on screen is the light minus
        // the dark, not the raw frame.
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(10, 10, 16);
        write_synthetic(dir.path(), "light.fits", &spec, &vec![500.0; 100]).unwrap();

        let (_darks_dir, dark_paths) = darks(3, 10, 10, 200.0);

        let (mut m, _spy) = model_over(dir.path());
        assert_eq!(m.loaded.as_ref().unwrap().image.data[0], 500.0);

        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        assert!(m.calibration.apply_dark);
        assert_eq!(
            m.loaded.as_ref().unwrap().image.data[0],
            300.0,
            "the displayed image should be light minus dark"
        );

        m.handle(Action::ToggleApplyDark);
        assert_eq!(
            m.loaded.as_ref().unwrap().image.data[0],
            500.0,
            "turning it off should restore the raw values"
        );
    }

    #[test]
    fn toggling_calibration_forces_a_redraw() {
        let dir = folder_of(1, 10, 10);
        let (_darks_dir, dark_paths) = darks(1, 10, 10, 5.0);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        let before = m.generation;
        m.handle(Action::ToggleApplyDark);
        assert_ne!(m.generation, before, "the texture must be rebuilt");
    }

    #[test]
    fn a_master_of_the_wrong_size_blocks_calibration_with_a_reason() {
        let dir = folder_of(1, 20, 15);
        let (_darks_dir, dark_paths) = darks(1, 8, 8, 10.0);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        let reason = m
            .calibration
            .blocked
            .as_ref()
            .expect("a size mismatch should be reported");
        assert!(reason.contains("8x8"), "{reason}");
        assert!(
            m.calibration.active_dark().is_none(),
            "a mismatched master must not be applied"
        );
        // The image is still shown, uncalibrated, rather than vanishing.
        assert!(m.loaded.is_some());
    }

    #[test]
    fn calibration_cannot_be_toggled_without_a_master() {
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::ToggleApplyDark);
        assert!(!m.calibration.apply_dark);
    }

    #[test]
    fn a_master_survives_being_saved_and_loaded() {
        let dir = folder_of(1, 12, 8);
        let (darks_dir, dark_paths) = darks(3, 12, 8, 77.0);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        let saved = darks_dir.path().join("master_dark.fits");
        m.handle(Action::SaveMasterDark(saved.clone()));
        assert!(saved.exists(), "the master should be written");

        m.handle(Action::ClearDarks);
        assert!(!m.calibration.has_dark());

        m.handle(Action::LoadMasterDark(saved));
        let dark = m.calibration.dark.as_ref().expect("should have loaded");
        assert_eq!(dark.source_count, 3, "the frame count should survive");
        assert!((dark.data[0] - 77.0).abs() < 0.01);
        assert!(m.calibration.apply_dark);
    }

    #[test]
    fn clearing_removes_the_master_and_restores_the_raw_image() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(10, 10, 16);
        write_synthetic(dir.path(), "light.fits", &spec, &vec![900.0; 100]).unwrap();
        let (_darks_dir, dark_paths) = darks(1, 10, 10, 100.0);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);
        assert_eq!(m.loaded.as_ref().unwrap().image.data[0], 800.0);

        m.handle(Action::ClearDarks);
        assert!(!m.calibration.has_dark());
        assert!(m.calibration.dark_sources.is_empty());
        assert_eq!(m.loaded.as_ref().unwrap().image.data[0], 900.0);
    }

    #[test]
    fn calibration_follows_the_selection_through_the_folder() {
        let dir = folder_of(3, 10, 10);
        let (_darks_dir, dark_paths) = darks(1, 10, 10, 1.0);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        for _ in 0..2 {
            m.handle(Action::NextFile);
            settle(&mut m);
            assert!(
                m.loaded.is_some(),
                "every file should display while calibration is on"
            );
        }
        assert!(m.calibration.blocked.is_none());
    }

    #[test]
    fn a_mismatched_exposure_warns_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let light = SyntheticSpec::new(10, 10, 16).with_card("EXPTIME", "300.0");
        write_synthetic(dir.path(), "light.fits", &light, &vec![500.0; 100]).unwrap();

        let darks_dir = tempfile::tempdir().unwrap();
        let dark_spec = SyntheticSpec::new(10, 10, 16).with_card("EXPTIME", "30.0");
        let dark_path =
            write_synthetic(darks_dir.path(), "d.fits", &dark_spec, &vec![100.0; 100]).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(vec![dark_path]));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        assert!(
            m.calibration.blocked.is_none(),
            "an exposure mismatch must not block"
        );
        assert!(
            m.calibration
                .warnings
                .iter()
                .any(|w| w.contains("exposure")),
            "expected a warning, got {:?}",
            m.calibration.warnings
        );
        assert_eq!(m.loaded.as_ref().unwrap().image.data[0], 400.0);
    }

    #[test]
    fn exporting_writes_calibrated_copies_and_leaves_originals_alone() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, 16);
        for i in 0..3 {
            write_synthetic(dir.path(), &format!("l{i}.fits"), &spec, &vec![400.0; 64]).unwrap();
        }
        let (_darks_dir, dark_paths) = darks(1, 8, 8, 100.0);
        let out = tempfile::tempdir().unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        m.handle(Action::StartExport(out.path().to_path_buf()));
        assert!(m.job.is_some(), "the export should run in the background");
        finish_job(&mut m);

        for i in 0..3 {
            let written = out.path().join(format!("l{i}_cal.fits"));
            assert!(written.exists(), "{} missing", written.display());
            let image = fits_core::read_fits(&written).unwrap();
            assert!((image.data[0] - 300.0).abs() < 0.01, "not calibrated");

            let original = fits_core::read_fits(&dir.path().join(format!("l{i}.fits"))).unwrap();
            assert!((original.data[0] - 400.0).abs() < 0.01, "original changed");
        }
    }

    #[test]
    fn only_one_background_job_runs_at_a_time() {
        let dir = folder_of(4, 10, 10);
        let (_darks_dir, dark_paths) = darks(4, 10, 10, 1.0);
        let out = tempfile::tempdir().unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);

        // A second request while the first is running is ignored rather than
        // starting a competing job.
        m.handle(Action::StartExport(out.path().to_path_buf()));
        assert!(m.job.is_some());
        finish_job(&mut m);
    }

    #[test]
    fn a_job_can_be_cancelled() {
        let dir = folder_of(20, 40, 40);
        let out = tempfile::tempdir().unwrap();
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::StartExport(out.path().to_path_buf()));
        m.handle(Action::CancelJob);
        finish_job(&mut m);

        assert!(m.job.is_none());
        let written = std::fs::read_dir(out.path()).unwrap().count();
        assert!(written < 20, "wrote {written} despite cancelling");
    }

    #[test]
    fn export_does_nothing_without_a_folder() {
        let out = tempfile::tempdir().unwrap();
        let mut m = Model::new();
        m.handle(Action::StartExport(out.path().to_path_buf()));
        assert!(m.job.is_none());
    }

    /// Writes `count` flat frames with a vignetting pattern.
    fn flats(count: usize, width: usize, height: usize, corner: f64) -> (TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(width, height, 16);
        let (cx, cy) = ((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0);
        let max_r = (cx * cx + cy * cy).sqrt().max(1.0);
        let pixels: Vec<f64> = (0..width * height)
            .map(|i| {
                let (x, y) = ((i % width) as f64, (i / width) as f64);
                let r = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / max_r;
                (1.0 - (1.0 - corner) * r) * 20_000.0
            })
            .collect();
        let paths = (0..count)
            .map(|i| {
                write_synthetic(dir.path(), &format!("flat_{i}.fits"), &spec, &pixels).unwrap()
            })
            .collect();
        (dir, paths)
    }

    #[test]
    fn building_a_master_flat_produces_a_gain_map() {
        let dir = folder_of(1, 16, 16);
        let (_flats_dir, flat_paths) = flats(3, 16, 16, 0.5);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddFlats(flat_paths));
        assert_eq!(m.calibration.flat_sources.len(), 3);

        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);

        let flat = m
            .calibration
            .flat
            .as_ref()
            .expect("a gain map should exist");
        assert_eq!(flat.source_count, 3);
        assert!(m.calibration.apply_flat, "building should switch it on");
        assert!(m.calibration.flat_summary().contains("3 frames"));
    }

    #[test]
    fn applying_a_flat_evens_out_the_frame() {
        // A uniformly lit sky seen through vignetting comes back uniform.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 16usize);
        let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
        let max_r = (cx * cx + cy * cy).sqrt();
        let gains: Vec<f64> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64, (i / w) as f64);
                let r = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / max_r;
                1.0 - 0.5 * r
            })
            .collect();
        let light: Vec<f64> = gains.iter().map(|g| g * 2000.0).collect();
        let spec = SyntheticSpec::new(w, h, 16);
        write_synthetic(dir.path(), "light.fits", &spec, &light).unwrap();

        let (_flats_dir, flat_paths) = flats(1, w, h, 0.5);

        let (mut m, _spy) = model_over(dir.path());
        let before = m.loaded.as_ref().unwrap().image.data.clone();
        let spread_before = before.iter().copied().fold(0.0f32, f32::max)
            - before.iter().copied().fold(f32::MAX, f32::min);

        m.handle(Action::AddFlats(flat_paths));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);

        let after = &m.loaded.as_ref().unwrap().image.data;
        let spread_after = after.iter().copied().fold(0.0f32, f32::max)
            - after.iter().copied().fold(f32::MAX, f32::min);

        assert!(
            spread_after < spread_before / 10.0,
            "the flat should even the frame: spread {spread_before} became {spread_after}"
        );
    }

    #[test]
    fn dark_and_flat_can_be_applied_together() {
        let dir = folder_of(1, 16, 16);
        let (_darks_dir, dark_paths) = darks(1, 16, 16, 50.0);
        let (_flats_dir, flat_paths) = flats(1, 16, 16, 0.8);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);
        m.handle(Action::AddFlats(flat_paths));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);

        assert!(m.calibration.apply_dark && m.calibration.apply_flat);
        assert!(m.calibration.blocked.is_none());
        assert!(m.loaded.is_some(), "the image should still display");
    }

    #[test]
    fn the_flat_can_be_toggled_independently_of_the_dark() {
        let dir = folder_of(1, 16, 16);
        let (_flats_dir, flat_paths) = flats(1, 16, 16, 0.6);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddFlats(flat_paths));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);

        let generation = m.generation;
        m.handle(Action::ToggleApplyFlat);
        assert!(!m.calibration.apply_flat);
        assert_ne!(m.generation, generation, "the texture must be rebuilt");

        m.handle(Action::ToggleApplyFlat);
        assert!(m.calibration.apply_flat);
    }

    #[test]
    fn a_flat_cannot_be_toggled_before_one_is_built() {
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::ToggleApplyFlat);
        assert!(!m.calibration.apply_flat);
    }

    #[test]
    fn a_flat_of_the_wrong_size_blocks_calibration_with_a_reason() {
        let dir = folder_of(1, 20, 15);
        let (_flats_dir, flat_paths) = flats(1, 8, 8, 0.7);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddFlats(flat_paths));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);

        let reason = m.calibration.blocked.as_ref().expect("should be reported");
        assert!(reason.contains("flat"), "{reason}");
        assert!(m.calibration.active_flat().is_none());
        assert!(m.loaded.is_some(), "the raw image should still show");
    }

    #[test]
    fn a_blank_flat_is_reported_rather_than_producing_an_empty_image() {
        let dir = folder_of(1, 8, 8);
        let blanks = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, 16);
        let path = write_synthetic(blanks.path(), "cap_on.fits", &spec, &vec![0.0; 64]).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddFlats(vec![path]));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);

        assert!(m.calibration.flat.is_none(), "a blank flat must be refused");
        assert!(m.error.is_some(), "the user should be told why");
    }

    #[test]
    fn a_flat_with_unusable_pixels_warns() {
        let dir = folder_of(1, 8, 8);
        let flats_dir = tempfile::tempdir().unwrap();
        let mut pixels = vec![10_000.0; 64];
        pixels[0] = 0.0;
        let spec = SyntheticSpec::new(8, 8, 16);
        let path = write_synthetic(flats_dir.path(), "f.fits", &spec, &pixels).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddFlats(vec![path]));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);

        assert_eq!(m.calibration.flat.as_ref().unwrap().unusable, 1);
        assert!(
            m.calibration
                .warnings
                .iter()
                .any(|w| w.contains("too little signal")),
            "expected a warning, got {:?}",
            m.calibration.warnings
        );
    }

    #[test]
    fn building_a_flat_with_no_flats_reports_it() {
        let dir = folder_of(1, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::BuildMasterFlat);
        assert!(m.job.is_none());
        assert!(m.error.is_some());
    }

    #[test]
    fn a_gain_map_survives_being_saved_and_loaded() {
        let dir = folder_of(1, 12, 12);
        let (flats_dir, flat_paths) = flats(3, 12, 12, 0.5);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddFlats(flat_paths));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);
        let original = m.calibration.flat.as_ref().unwrap().gain.clone();

        let saved = flats_dir.path().join("master_flat.fits");
        m.handle(Action::SaveMasterFlat(saved.clone()));
        m.handle(Action::ClearFlats);
        assert!(!m.calibration.has_flat());

        m.handle(Action::LoadMasterFlat(saved));
        let reloaded = m.calibration.flat.as_ref().expect("should have loaded");
        assert_eq!(reloaded.source_count, 3);
        for (a, b) in reloaded.gain.iter().zip(original.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn calibration_setup_is_restored_when_the_folder_is_reopened() {
        // The sidecar remembers which masters were used with this folder.
        let dir = folder_of(2, 12, 12);
        let library = tempfile::tempdir().unwrap();

        let (_darks_dir, dark_paths) = darks(2, 12, 12, 40.0);
        let (_flats_dir, flat_paths) = flats(2, 12, 12, 0.6);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);
        m.handle(Action::SaveMasterDark(library.path().join("dark.fits")));

        m.handle(Action::AddFlats(flat_paths));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);
        m.handle(Action::SaveMasterFlat(library.path().join("flat.fits")));

        // Reopen, as though the application had been restarted.
        let (m2, _spy2) = model_over(dir.path());
        assert!(m2.calibration.has_dark(), "the dark should come back");
        assert!(m2.calibration.has_flat(), "the flat should come back");
        assert_eq!(m2.calibration.dark.as_ref().unwrap().source_count, 2);
    }

    #[test]
    fn a_calibration_frame_that_has_been_moved_away_is_skipped_quietly() {
        let dir = folder_of(1, 12, 12);
        let library = tempfile::tempdir().unwrap();
        let (_darks_dir, dark_paths) = darks(1, 12, 12, 40.0);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);
        let saved = library.path().join("dark.fits");
        m.handle(Action::SaveMasterDark(saved.clone()));

        std::fs::remove_file(&saved).unwrap();

        let (m2, _spy2) = model_over(dir.path());
        assert!(!m2.calibration.has_dark());
        assert!(m2.error.is_none(), "a missing master must not be an error");
        assert!(m2.loaded.is_some(), "the folder should still open");
    }

    #[test]
    fn exporting_applies_both_frames_in_the_right_order() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, 16);
        write_synthetic(dir.path(), "l.fits", &spec, &vec![600.0; 64]).unwrap();
        let (_darks_dir, dark_paths) = darks(1, 8, 8, 100.0);
        // A uniform flat has a gain of exactly 1, so the arithmetic is easy to
        // check: 600 minus 100, then divided by 1.
        let flats_dir = tempfile::tempdir().unwrap();
        let flat_path =
            write_synthetic(flats_dir.path(), "f.fits", &spec, &vec![5000.0; 64]).unwrap();
        let out = tempfile::tempdir().unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);
        m.handle(Action::AddFlats(vec![flat_path]));
        m.handle(Action::BuildMasterFlat);
        finish_job(&mut m);

        m.handle(Action::StartExport(out.path().to_path_buf()));
        finish_job(&mut m);

        let written = fits_core::read_fits(&out.path().join("l_cal.fits")).unwrap();
        assert!(
            (written.data[0] - 500.0).abs() < 1.0,
            "got {}",
            written.data[0]
        );

        let bytes = std::fs::read(out.path().join("l_cal.fits")).unwrap();
        let text = String::from_utf8_lossy(&bytes[..2880]);
        assert!(text.contains("dark subtracted"), "{text}");
        assert!(text.contains("flat divided"));
    }

    #[test]
    fn the_file_list_can_be_collapsed_and_brought_back() {
        let mut m = Model::new();
        assert!(m.show_filelist, "it starts visible");

        m.handle(Action::ToggleFileList);
        assert!(!m.show_filelist);

        m.handle(Action::ToggleFileList);
        assert!(m.show_filelist);
    }

    #[test]
    fn the_panel_can_report_that_it_collapsed_itself() {
        // Dragging the resize edge past the minimum collapses the panel, and
        // the model has to learn about it or the toolbar arrow points the wrong
        // way.
        let mut m = Model::new();
        m.handle(Action::SetFileListVisible(false));
        assert!(!m.show_filelist);
        m.handle(Action::SetFileListVisible(true));
        assert!(m.show_filelist);
    }

    #[test]
    fn collapsing_the_list_does_not_disturb_the_image() {
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());
        let shown = m.loaded.as_ref().unwrap().path.clone();
        let generation = m.generation;

        m.handle(Action::ToggleFileList);

        assert_eq!(m.loaded.as_ref().unwrap().path, shown);
        assert_eq!(
            m.generation, generation,
            "hiding a panel is not a reason to rebuild the texture"
        );
    }

    #[test]
    fn keyboard_navigation_asks_the_list_to_follow_the_selection() {
        // Otherwise stepping past the bottom of a long folder leaves the
        // highlighted row out of sight.
        let dir = folder_of(3, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::NextFile);
        assert!(m.scroll_to_selection, "the list should follow the keyboard");

        // A click already has the row under the pointer, so it must not scroll.
        m.scroll_to_selection = false;
        m.handle(Action::Select(0));
        assert!(!m.scroll_to_selection, "clicking must not move the list");
    }

    #[test]
    fn the_vertical_arrows_move_through_the_folder() {
        let dir = folder_of(3, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        // The actions the down and up arrows produce.
        m.handle(Action::NextFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "2 / 3");

        m.handle(Action::PreviousFile);
        settle(&mut m);
        assert_eq!(m.position_label(), "1 / 3");
    }

    #[test]
    fn the_metadata_section_is_open_by_default_and_toggles() {
        let mut m = Model::new();
        assert!(
            m.show_header,
            "metadata should be visible without being asked for"
        );

        m.handle(Action::ToggleHeader);
        assert!(!m.show_header);
        m.handle(Action::ToggleHeader);
        assert!(m.show_header);
    }

    #[test]
    fn the_metadata_filter_is_remembered_while_stepping_through_files() {
        // Looking for the same keyword across several frames is the reason to
        // have a filter at all.
        let dir = folder_of(2, 10, 10);
        let (mut m, _spy) = model_over(dir.path());

        m.handle(Action::SetHeaderFilter("bitpix".into()));
        assert_eq!(m.header_filter, "bitpix");

        m.handle(Action::NextFile);
        settle(&mut m);
        assert_eq!(m.header_filter, "bitpix", "the filter should persist");
    }

    #[test]
    fn the_metadata_of_the_displayed_image_is_what_is_available() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, 16)
            .with_card("OBJECT", "'M31     '")
            .with_card("EXPTIME", "               300.0");
        write_synthetic(dir.path(), "light.fits", &spec, &vec![1.0; 64]).unwrap();

        let (m, _spy) = model_over(dir.path());
        let header = &m.loaded.as_ref().unwrap().image.header;
        assert_eq!(header.get("OBJECT"), Some("M31"));
        assert_eq!(header.get_f64("EXPTIME"), Some(300.0));
    }

    /// Writes a one-shot colour mosaic sampled from a flat colour.
    fn osc_folder(colour: [f64; 3], pattern: &str) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 16usize);
        let bayer = BayerPattern::parse(pattern).unwrap();
        let pixels: Vec<f64> = (0..w * h)
            .map(|i| colour[bayer.colour_at(i % w, i / w).plane()])
            .collect();
        let spec = SyntheticSpec::new(w, h, 16)
            .with_scaling(32768.0, 1.0)
            .with_card("BAYERPAT", &format!("'{pattern}    '"));
        write_synthetic(dir.path(), "osc.fits", &spec, &pixels).unwrap();
        dir
    }

    #[test]
    fn a_file_declaring_a_filter_pattern_is_shown_in_colour_without_being_asked() {
        // A file that says it is a colour raw plainly wants to be seen as one.
        let dir = osc_folder([2000.0, 800.0, 300.0], "RGGB");
        let (m, _spy) = model_over(dir.path());

        assert_eq!(m.bayer.pattern, Some(BayerPattern::Rggb));
        assert!(m.bayer.from_header, "the pattern came from the file");
        assert!(m.bayer.enabled);

        let shown = &m.loaded.as_ref().unwrap().image;
        assert_eq!(shown.channels, 3, "it should be displayed in colour");

        let plane = shown.width * shown.height;
        let index = 5 * shown.width + 5;
        assert!((shown.data[index] - 2000.0).abs() < 1.0, "red");
        assert!((shown.data[plane + index] - 800.0).abs() < 1.0, "green");
        assert!((shown.data[2 * plane + index] - 300.0).abs() < 1.0, "blue");
    }

    #[test]
    fn a_mono_file_without_a_pattern_is_left_alone() {
        let dir = folder_of(1, 16, 16);
        let (m, _spy) = model_over(dir.path());
        assert_eq!(m.bayer.pattern, None);
        assert!(!m.bayer.enabled);
        assert_eq!(m.loaded.as_ref().unwrap().image.channels, 1);
    }

    #[test]
    fn colour_reconstruction_can_be_turned_off_and_on() {
        let dir = osc_folder([2000.0, 800.0, 300.0], "RGGB");
        let (mut m, _spy) = model_over(dir.path());
        assert_eq!(m.loaded.as_ref().unwrap().image.channels, 3);

        m.handle(Action::ToggleDebayer);
        assert!(!m.bayer.enabled);
        assert_eq!(
            m.loaded.as_ref().unwrap().image.channels,
            1,
            "turning it off should show the mosaic again"
        );

        m.handle(Action::ToggleDebayer);
        assert_eq!(m.loaded.as_ref().unwrap().image.channels, 3);
    }

    #[test]
    fn choosing_the_wrong_pattern_changes_the_colours() {
        // So the chooser is worth having, and a wrong guess is visible.
        let dir = osc_folder([2000.0, 800.0, 300.0], "RGGB");
        let (mut m, _spy) = model_over(dir.path());
        let red_of = |m: &Model| m.loaded.as_ref().unwrap().image.data[5 * 16 + 5];
        assert!((red_of(&m) - 2000.0).abs() < 1.0);

        m.handle(Action::SetBayerPattern(BayerPattern::Bggr));
        assert!(!m.bayer.from_header, "the user has overridden the file");
        assert!(
            (red_of(&m) - 300.0).abs() < 1.0,
            "red and blue should swap, got {}",
            red_of(&m)
        );
    }

    #[test]
    fn flipping_the_pattern_rows_changes_the_result() {
        let dir = osc_folder([2000.0, 800.0, 300.0], "RGGB");
        let (mut m, _spy) = model_over(dir.path());
        let before = m.loaded.as_ref().unwrap().image.data.clone();

        m.handle(Action::ToggleBayerFlip);
        assert!(m.bayer.flip_rows);
        let after = &m.loaded.as_ref().unwrap().image.data;
        assert_ne!(&before, after, "the flip should do something visible");

        m.handle(Action::ToggleBayerFlip);
        assert_eq!(&before, &m.loaded.as_ref().unwrap().image.data);
    }

    #[test]
    fn the_pattern_choice_is_restored_when_the_folder_is_reopened() {
        let dir = osc_folder([2000.0, 800.0, 300.0], "RGGB");
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::SetBayerPattern(BayerPattern::Grbg));
        m.handle(Action::ToggleBayerFlip);

        let (m2, _spy2) = model_over(dir.path());
        assert_eq!(
            m2.bayer.pattern,
            Some(BayerPattern::Grbg),
            "a deliberate choice should win over the file's own header"
        );
        assert!(m2.bayer.flip_rows);
        assert!(m2.bayer.enabled);
    }

    #[test]
    fn a_dark_is_subtracted_from_the_mosaic_before_colour_is_reconstructed() {
        // The Phase 9 ordering rule, checked through the whole application: the
        // dark is a mosaic, so it must reach the image while it is still one.
        let dir = osc_folder([2000.0, 800.0, 300.0], "RGGB");
        let darks_dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(16, 16, 16).with_scaling(32768.0, 1.0);
        let dark_path =
            write_synthetic(darks_dir.path(), "d.fits", &spec, &vec![100.0; 256]).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(vec![dark_path]));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        let shown = &m.loaded.as_ref().unwrap().image;
        assert_eq!(shown.channels, 3, "still in colour after calibrating");

        // Every channel drops by the dark's level, which only works if the
        // subtraction happened on the mosaic.
        let plane = shown.width * shown.height;
        let index = 5 * shown.width + 5;
        assert!((shown.data[index] - 1900.0).abs() < 1.0, "red");
        assert!((shown.data[plane + index] - 700.0).abs() < 1.0, "green");
        assert!((shown.data[2 * plane + index] - 200.0).abs() < 1.0, "blue");
    }

    #[test]
    fn a_mono_calibration_frame_is_not_reported_as_mismatched_while_colour_is_shown() {
        // The displayed image has three channels once debayered, but the dark
        // describes the single-channel mosaic it came from. Comparing against
        // the display would block calibration that is perfectly valid.
        let dir = osc_folder([2000.0, 800.0, 300.0], "RGGB");
        let darks_dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(16, 16, 16).with_scaling(32768.0, 1.0);
        let dark_path =
            write_synthetic(darks_dir.path(), "d.fits", &spec, &vec![50.0; 256]).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(vec![dark_path]));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        assert_eq!(
            m.calibration.blocked, None,
            "a mono dark matches the mosaic it is for"
        );
        assert!(m.calibration.active_dark().is_some());
    }

    #[test]
    fn stepping_back_to_a_colour_frame_does_not_debayer_it_again() {
        // Reconstructing colour is the most expensive step in the application,
        // so a frame already shown must come back from the cache. Comparing the
        // pointers proves it: a recomputed image would be a different
        // allocation holding equal values, which an equality check would miss.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32usize, 32usize);
        let bayer = BayerPattern::Rggb;
        let spec = SyntheticSpec::new(w, h, 16)
            .with_scaling(32768.0, 1.0)
            .with_card("BAYERPAT", "'RGGB    '");
        for i in 0..3 {
            let pixels: Vec<f64> = (0..w * h)
                .map(|p| {
                    [2000.0, 800.0, 300.0][bayer.colour_at(p % w, p / w).plane()]
                        + f64::from(i) * 10.0
                })
                .collect();
            write_synthetic(dir.path(), &format!("osc_{i}.fits"), &spec, &pixels).unwrap();
        }

        let (mut m, _spy) = model_over(dir.path());
        assert!(m.bayer.enabled, "the header declares a pattern");
        let first = Arc::clone(&m.loaded.as_ref().unwrap().image);
        assert_eq!(first.channels, 3);

        m.handle(Action::NextFile);
        settle(&mut m);
        m.handle(Action::PreviousFile);
        settle(&mut m);

        let again = &m.loaded.as_ref().unwrap().image;
        assert!(
            Arc::ptr_eq(&first, again),
            "the frame should have come from the cache, not been rebuilt"
        );
    }

    #[test]
    fn the_calibrated_cache_holds_the_whole_prefetch_working_set() {
        // Three entries: the selected file and the neighbours either side, so
        // stepping in either direction is instant.
        assert_eq!(CALIBRATED_CACHE_ENTRIES, 3);
        // And enough room for three debayered full frames, which is the case
        // that would otherwise thrash.
        let debayered_full_frame = 6000 * 4000 * 3 * std::mem::size_of::<f32>();
        assert!(
            CALIBRATED_CACHE_BYTES >= 3 * debayered_full_frame,
            "the budget holds only {:.1} debayered frames",
            CALIBRATED_CACHE_BYTES as f64 / debayered_full_frame as f64
        );
    }

    #[test]
    fn exports_stay_as_mosaics_even_while_colour_is_shown() {
        // A stacker wants raw calibrated frames and does its own debayering.
        let dir = osc_folder([2000.0, 800.0, 300.0], "RGGB");
        let out = tempfile::tempdir().unwrap();

        let (mut m, _spy) = model_over(dir.path());
        assert_eq!(m.loaded.as_ref().unwrap().image.channels, 3);

        m.handle(Action::StartExport(out.path().to_path_buf()));
        finish_job(&mut m);

        let written = fits_core::read_fits(&out.path().join("osc_cal.fits")).unwrap();
        assert_eq!(
            written.channels, 1,
            "the exported file should still be a mosaic"
        );
    }

    #[test]
    fn a_frame_is_measured_as_soon_as_it_is_displayed() {
        // It has already been read, so measuring costs almost nothing and the
        // numbers fill in as the folder is browsed.
        let dir = folder_of(3, 32, 32);
        let (mut m, _spy) = model_over(dir.path());

        let quality = m.folder.as_ref().unwrap().files[0]
            .quality
            .expect("the displayed frame should be measured");
        assert!(quality.background > 0.0);
        assert!(quality.sharpness.is_finite());

        // Prefetched neighbours are measured too, since the worker has already
        // read them. Browsing therefore fills the folder in without asking.
        m.handle(Action::NextFile);
        settle(&mut m);
        m.handle(Action::NextFile);
        settle(&mut m);
        assert_eq!(
            m.folder.as_ref().unwrap().measured(),
            3,
            "browsing should have measured the whole folder"
        );
    }

    #[test]
    fn measuring_the_folder_fills_in_every_frame() {
        let dir = folder_of(5, 24, 24);
        let (mut m, _spy) = model_over(dir.path());
        assert!(
            m.folder.as_ref().unwrap().measured() < 5,
            "opening a folder measures only what it reads"
        );

        m.handle(Action::MeasureFolder);
        assert!(m.job.is_some(), "measuring should run in the background");
        finish_job(&mut m);

        assert_eq!(m.folder.as_ref().unwrap().measured(), 5);
    }

    #[test]
    fn measuring_never_changes_a_file() {
        // The whole point is that it advises. Nothing is deleted or flagged.
        let dir = folder_of(6, 16, 16);
        let (mut m, spy) = model_over(dir.path());
        m.handle(Action::MeasureFolder);
        finish_job(&mut m);

        assert!(
            spy.trashed.lock().unwrap().is_empty(),
            "measuring must not delete anything"
        );
        let folder = m.folder.as_ref().unwrap();
        assert_eq!(folder.len(), 6, "no file was removed");
        assert!(folder.files.iter().all(|e| !e.flagged), "none was flagged");
        for i in 1..=6 {
            assert!(dir.path().join(format!("light_{i}.fits")).exists());
        }
    }

    #[test]
    fn sorting_reorders_the_list_and_keeps_the_selection() {
        let dir = folder_of(4, 16, 16);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::MeasureFolder);
        finish_job(&mut m);

        let before = m.loaded.as_ref().unwrap().path.clone();
        m.handle(Action::SortBy(SortKey::Background));
        assert_eq!(m.sort_key, SortKey::Background);
        assert_eq!(
            m.folder.as_ref().unwrap().selected_path(),
            Some(before.as_path()),
            "the selection should follow the file through a re-sort"
        );

        // And the ordering really is by the measure.
        let values: Vec<f64> = m
            .folder
            .as_ref()
            .unwrap()
            .files
            .iter()
            .filter_map(|e| SortKey::Background.value_of(e))
            .collect();
        let mut sorted = values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(values, sorted);
    }

    #[test]
    fn sorting_does_not_change_which_image_is_displayed() {
        let dir = folder_of(4, 16, 16);
        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::MeasureFolder);
        finish_job(&mut m);

        let shown = m.loaded.as_ref().unwrap().path.clone();
        m.handle(Action::SortBy(SortKey::Sharpness));
        assert_eq!(m.loaded.as_ref().unwrap().path, shown);
    }

    #[test]
    fn measuring_and_sorting_do_nothing_without_a_folder() {
        let mut m = Model::new();
        m.handle(Action::MeasureFolder);
        m.handle(Action::SortBy(SortKey::Background));
        assert!(m.job.is_none());
        assert!(m.folder.is_none());
    }

    #[test]
    fn the_pixel_under_the_pointer_is_reported_at_several_zooms_and_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 16usize);
        // Each sample equals its own index, so a readout identifies its pixel.
        let pixels: Vec<f64> = (0..w * h).map(|i| i as f64).collect();
        let spec = SyntheticSpec::new(w, h, -32);
        write_synthetic(dir.path(), "grid.fits", &spec, &pixels).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        m.set_viewport(viewport);

        for zoom in [1.0, 4.0, 12.0] {
            m.view = ViewState::centred(Vec2::new(w as f32, h as f32), viewport, zoom);
            for (x, y) in [(0usize, 0usize), (3, 5), (15, 15)] {
                // The centre of that pixel, in screen space.
                let screen = m
                    .view
                    .image_to_screen(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                m.pointer = Some(screen);

                let readout = m
                    .pixel_readout()
                    .unwrap_or_else(|| panic!("no readout at zoom {zoom}, pixel ({x}, {y})"));
                assert_eq!((readout.x, readout.y), (x, y), "at zoom {zoom}");

                // Display row 0 is the top; the file stores the bottom first.
                let expected = ((h - 1 - y) * w + x) as f32;
                assert!(
                    (readout.values[0] - expected).abs() < 0.01,
                    "at zoom {zoom}, pixel ({x}, {y}): got {} wanted {expected}",
                    readout.values[0]
                );
            }
        }
    }

    #[test]
    fn a_pointer_outside_the_image_reports_nothing_rather_than_a_wrong_pixel() {
        let dir = folder_of(1, 16, 16);
        let (mut m, _spy) = model_over(dir.path());
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        m.set_viewport(viewport);
        m.view = ViewState::centred(Vec2::new(16.0, 16.0), viewport, 4.0);

        for offset in [
            Vec2::new(-500.0, 0.0),
            Vec2::new(500.0, 0.0),
            Vec2::new(0.0, -500.0),
            Vec2::new(0.0, 500.0),
        ] {
            m.pointer = Some(m.view.image_to_screen(Vec2::new(8.0, 8.0)) + offset);
            assert!(
                m.pixel_readout().is_none(),
                "a pointer off the image must report nothing, not the nearest pixel"
            );
        }

        m.pointer = None;
        assert!(m.pixel_readout().is_none());
    }

    #[test]
    fn the_readout_shows_the_file_value_not_the_displayed_one() {
        // Calibration changes what is on screen. The question a readout answers
        // is what the data holds, so it must not follow the display.
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(16, 16, 16);
        write_synthetic(dir.path(), "light.fits", &spec, &vec![900.0; 256]).unwrap();
        let (_darks_dir, dark_paths) = darks(1, 16, 16, 400.0);

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::AddDarks(dark_paths));
        m.handle(Action::BuildMasterDark);
        finish_job(&mut m);

        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        m.set_viewport(viewport);
        m.view = ViewState::centred(Vec2::new(16.0, 16.0), viewport, 4.0);
        m.pointer = Some(m.view.image_to_screen(Vec2::new(8.5, 8.5)));

        assert_eq!(
            m.loaded.as_ref().unwrap().image.data[0],
            500.0,
            "the displayed image is calibrated"
        );
        let readout = m.pixel_readout().expect("should have a readout");
        assert!(
            (readout.values[0] - 900.0).abs() < 0.01,
            "the readout should show the file's 900, not the displayed 500: got {}",
            readout.values[0]
        );
    }

    #[test]
    fn a_colour_frame_reports_all_three_channels() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(4, 4, -32).with_channels(3);
        let mut pixels = vec![0.0; 48];
        for (i, v) in pixels.iter_mut().enumerate() {
            *v = [10.0, 20.0, 30.0][i / 16];
        }
        write_synthetic(dir.path(), "rgb.fits", &spec, &pixels).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        m.set_viewport(viewport);
        m.view = ViewState::centred(Vec2::new(4.0, 4.0), viewport, 20.0);
        m.pointer = Some(m.view.image_to_screen(Vec2::new(2.5, 2.5)));

        let readout = m.pixel_readout().expect("should have a readout");
        assert_eq!(readout.channels, 3);
        assert!((readout.values[0] - 10.0).abs() < 0.01);
        assert!((readout.values[1] - 20.0).abs() < 0.01);
        assert!((readout.values[2] - 30.0).abs() < 0.01);
        assert!(
            readout.describe().contains("10, 20, 30"),
            "{}",
            readout.describe()
        );
    }

    #[test]
    fn an_undefined_sample_is_shown_as_such_rather_than_as_a_number() {
        let readout = PixelReadout {
            x: 1,
            y: 2,
            values: [f32::NAN, 0.0, 0.0],
            channels: 1,
            reconstructed: false,
        };
        assert!(readout.describe().contains('—'), "{}", readout.describe());
        assert!(readout.describe().contains("(1, 2)"));
    }

    #[test]
    fn a_colour_mosaic_reads_out_three_reconstructed_values() {
        // A single number on a one-shot colour frame is misleading: neighbours
        // differ because they sit under different filters, not because the sky
        // does. With colour reconstruction on, the readout should say what the
        // picture says.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 16usize);
        let bayer = BayerPattern::Rggb;
        let source = [3000.0, 1200.0, 400.0];
        let pixels: Vec<f64> = (0..w * h)
            .map(|i| source[bayer.colour_at(i % w, i / w).plane()])
            .collect();
        let spec = SyntheticSpec::new(w, h, 16)
            .with_scaling(32768.0, 1.0)
            .with_card("BAYERPAT", "'RGGB    '");
        write_synthetic(dir.path(), "osc.fits", &spec, &pixels).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        assert!(m.bayer.enabled, "the header declares a pattern");

        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        m.set_viewport(viewport);
        m.view = ViewState::centred(Vec2::new(16.0, 16.0), viewport, 20.0);
        m.pointer = Some(m.view.image_to_screen(Vec2::new(8.5, 8.5)));

        let readout = m.pixel_readout().expect("should have a readout");
        assert_eq!(
            readout.channels, 3,
            "a colour frame should read out in colour"
        );
        assert!(
            readout.reconstructed,
            "and say the values were interpolated"
        );
        for (channel, expected) in source.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let expected = *expected as f32;
            assert!(
                (readout.values[channel] - expected).abs() < 1.0,
                "channel {channel}: got {} wanted {expected}",
                readout.values[channel]
            );
        }
        assert!(readout.describe().contains('~'), "{}", readout.describe());
    }

    #[test]
    fn turning_colour_off_returns_the_readout_to_one_mosaic_value() {
        // Without reconstruction there is no colour to report, and the honest
        // answer is the single sample the pixel actually measured.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 16usize);
        let bayer = BayerPattern::Rggb;
        let source = [3000.0, 1200.0, 400.0];
        let pixels: Vec<f64> = (0..w * h)
            .map(|i| source[bayer.colour_at(i % w, i / w).plane()])
            .collect();
        let spec = SyntheticSpec::new(w, h, 16)
            .with_scaling(32768.0, 1.0)
            .with_card("BAYERPAT", "'RGGB    '");
        write_synthetic(dir.path(), "osc.fits", &spec, &pixels).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        m.handle(Action::ToggleDebayer);
        assert!(!m.bayer.enabled);

        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        m.set_viewport(viewport);
        m.view = ViewState::centred(Vec2::new(16.0, 16.0), viewport, 20.0);
        m.pointer = Some(m.view.image_to_screen(Vec2::new(8.5, 8.5)));

        let readout = m.pixel_readout().expect("should have a readout");
        assert_eq!(readout.channels, 1);
        assert!(!readout.reconstructed);
        assert!(!readout.describe().contains('~'));
    }

    #[test]
    fn a_reconstructed_readout_agrees_with_the_displayed_picture() {
        // The readout sits beside the image; the two must not disagree.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 16usize);
        let bayer = BayerPattern::Rggb;
        let pixels: Vec<f64> = (0..w * h)
            .map(|i| [5000.0, 2000.0, 800.0][bayer.colour_at(i % w, i / w).plane()])
            .collect();
        let spec = SyntheticSpec::new(w, h, 16)
            .with_scaling(32768.0, 1.0)
            .with_card("BAYERPAT", "'RGGB    '");
        write_synthetic(dir.path(), "osc.fits", &spec, &pixels).unwrap();

        let (mut m, _spy) = model_over(dir.path());
        let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        m.set_viewport(viewport);
        m.view = ViewState::centred(Vec2::new(16.0, 16.0), viewport, 20.0);

        let (x, y) = (6usize, 9usize);
        m.pointer = Some(
            m.view
                .image_to_screen(Vec2::new(x as f32 + 0.5, y as f32 + 0.5)),
        );
        let readout = m.pixel_readout().expect("should have a readout");

        let shown = &m.loaded.as_ref().unwrap().image;
        let plane = shown.width * shown.height;
        for channel in 0..3 {
            let displayed = shown.data[channel * plane + y * shown.width + x];
            assert_eq!(
                readout.values[channel].to_bits(),
                displayed.to_bits(),
                "channel {channel} differs from what is on screen"
            );
        }
    }

    #[test]
    fn the_histogram_toggles() {
        let mut m = Model::new();
        assert!(!m.show_histogram, "it starts out of the way");
        m.handle(Action::ToggleHistogram);
        assert!(m.show_histogram);
        m.handle(Action::ToggleHistogram);
        assert!(!m.show_histogram);
    }

    #[test]
    fn the_help_overlay_toggles() {
        let mut m = Model::new();
        assert!(!m.show_help);
        m.handle(Action::ToggleHelp);
        assert!(m.show_help);
        m.handle(Action::ToggleHelp);
        assert!(!m.show_help);
    }

    #[test]
    fn file_actions_do_nothing_without_a_folder() {
        let mut m = Model::new();
        for a in [
            Action::ToggleFlag,
            Action::RequestDelete,
            Action::ConfirmDelete,
            Action::BeginRename,
            Action::CommitRename("x.fits".into()),
        ] {
            m.handle(a);
        }
        assert!(m.folder.is_none());
        assert_eq!(m.pending, Pending::None);
    }

    #[test]
    fn a_toast_expires_on_its_own() {
        let mut m = Model::new();
        m.toast = Some(Toast {
            text: "gone".into(),
            until: std::time::Instant::now() - Duration::from_secs(1),
        });
        m.expire_toast();
        assert!(m.toast.is_none());

        m.toast = Some(Toast::new("still here"));
        m.expire_toast();
        assert!(m.toast.is_some());
    }

    #[test]
    fn polling_with_nothing_outstanding_reports_no_change() {
        let mut m = Model::new();
        assert!(!m.poll());
    }
}
