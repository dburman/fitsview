//! Making the frame on screen ready to show without making the interface wait.
//!
//! A frame comes off the disk on the loader's thread, but what is shown is often
//! not that frame: it may have a dark taken off it, be divided by a flat, be
//! reconstructed into colour, and have its gradient taken out. On a colour
//! frame of 61 megapixels that came to a quarter of a second, and done where
//! the image is drawn, every step through a folder froze the window for it.
//!
//! So it happens here, on a thread of its own, one frame at a time. Only the
//! newest request waits: one asked for while another is being worked on
//! replaces any that had not yet started, so holding down an arrow key does not
//! queue a quarter of a second for every frame passed over.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};

use fits_core::calib::{self, MasterFlat, MasterFrame};
use fits_core::debayer::BayerPattern;
use fits_core::gradient;
use fits_core::FitsImage;

/// What is done to a frame to make it the one shown.
#[derive(Debug, Clone, Default)]
pub struct Recipe {
    /// The dark taken off it.
    pub dark: Option<Arc<MasterFrame>>,
    /// The flat it is divided by.
    pub flat: Option<Arc<MasterFlat>>,
    /// The mosaic it is reconstructed from.
    pub pattern: Option<BayerPattern>,
    /// Whether its gradient is taken out.
    pub extract: bool,
}

impl Recipe {
    /// Whether anything is done to the frame at all.
    #[must_use]
    pub fn does_anything(&self) -> bool {
        self.calibrates() || self.extract
    }

    /// Whether it is calibrated or reconstructed, rather than only flattened.
    fn calibrates(&self) -> bool {
        self.dark.is_some() || self.flat.is_some() || self.pattern.is_some()
    }
}

/// `image` made ready to show by `recipe`.
///
/// The gradient is taken out last, from the frame as it will be seen:
/// calibrated, and in colour where it has colour, so that each colour loses
/// its own.
///
/// # Errors
///
/// When calibrating fails, with the reason.
pub fn prepare(image: &FitsImage, recipe: &Recipe) -> Result<FitsImage, String> {
    let settings = gradient::Settings::default();
    if !recipe.calibrates() {
        return Ok(gradient::flatten(image, &settings));
    }
    let calibrated = calib::calibrate_and_debayer(
        image,
        recipe.dark.as_deref(),
        recipe.flat.as_deref(),
        recipe.pattern,
    )
    .map_err(|e| e.to_string())?;
    Ok(if recipe.extract {
        gradient::flatten(&calibrated, &settings)
    } else {
        calibrated
    })
}

/// A frame to make ready.
#[derive(Debug)]
struct Request {
    generation: u64,
    path: PathBuf,
    image: Arc<FitsImage>,
    recipe: Recipe,
}

/// A frame made ready, or the reason it could not be.
#[derive(Debug)]
pub struct Prepared {
    /// The generation of the settings it was asked for under.
    pub generation: u64,
    /// Which file it is.
    pub path: PathBuf,
    /// The frame as it is to be shown.
    pub result: Result<Arc<FitsImage>, String>,
}

/// What the worker is to do next.
#[derive(Debug)]
struct Slot {
    /// The newest request not yet started.
    next: Option<Request>,
    /// False once the preparer is dropped, which tells the worker to stop.
    open: bool,
}

/// Makes frames ready on a worker thread, the newest first and only.
#[derive(Debug)]
pub struct Preparer {
    slot: Arc<(Mutex<Slot>, Condvar)>,
    results: mpsc::Receiver<Prepared>,
    /// Where an answer made on this thread is sent, when there is no worker.
    here: mpsc::Sender<Prepared>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Requests made and not yet answered.
    outstanding: usize,
}

impl Default for Preparer {
    fn default() -> Self {
        Self::new()
    }
}

impl Preparer {
    /// A preparer with its worker waiting.
    #[must_use]
    pub fn new() -> Self {
        let slot = Arc::new((
            Mutex::new(Slot {
                next: None,
                open: true,
            }),
            Condvar::new(),
        ));
        let (tx, results) = mpsc::channel();
        let here = tx.clone();
        let worker_slot = Arc::clone(&slot);
        let worker = std::thread::Builder::new()
            .name("fitsview-prepare".into())
            .spawn(move || worker_loop(&worker_slot, &tx))
            .ok();
        Self {
            slot,
            results,
            here,
            worker,
            outstanding: 0,
        }
    }

    /// Asks for `image`, the frame at `path`, made ready by `recipe`, in place
    /// of any request the worker has not yet started.
    ///
    /// Without a worker — the system would not start a thread — the frame is
    /// made ready here and now, which is slow but still right.
    pub fn request(&mut self, generation: u64, path: &Path, image: Arc<FitsImage>, recipe: Recipe) {
        let request = Request {
            generation,
            path: path.to_path_buf(),
            image,
            recipe,
        };
        if self.worker.is_none() {
            log::warn!("no thread to prepare frames on; preparing it here");
            self.outstanding += 1;
            let _ = self.here.send(answer(request));
            return;
        }
        let (lock, cv) = &*self.slot;
        if let Ok(mut slot) = lock.lock() {
            if slot.next.replace(request).is_none() {
                self.outstanding += 1;
            }
        }
        cv.notify_all();
    }

    /// Everything finished since the last call. Never blocks.
    pub fn poll(&mut self) -> Vec<Prepared> {
        let mut out = Vec::new();
        while let Ok(prepared) = self.results.try_recv() {
            self.outstanding = self.outstanding.saturating_sub(1);
            out.push(prepared);
        }
        if self
            .worker
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            // A worker that stopped answers nothing more.
            self.outstanding = 0;
        }
        out
    }

    /// Whether an answer is still to come.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.outstanding > 0
    }
}

impl Drop for Preparer {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.slot;
            if let Ok(mut slot) = lock.lock() {
                slot.open = false;
                slot.next = None;
            }
            cv.notify_all();
        }
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// The worker: wait for a request, make it ready, send it back.
fn worker_loop(slot: &Arc<(Mutex<Slot>, Condvar)>, tx: &mpsc::Sender<Prepared>) {
    let (lock, cv) = &**slot;
    loop {
        let request = {
            let Ok(mut guard) = lock.lock() else {
                return;
            };
            loop {
                if !guard.open {
                    return;
                }
                if let Some(request) = guard.next.take() {
                    break request;
                }
                guard = match cv.wait(guard) {
                    Ok(guard) => guard,
                    Err(_) => return,
                };
            }
        };
        if tx.send(answer(request)).is_err() {
            return; // nobody is listening any more
        }
    }
}

/// Does what `request` asks.
fn answer(request: Request) -> Prepared {
    let started = std::time::Instant::now();
    let result = prepare(&request.image, &request.recipe).map(Arc::new);
    log::debug!(
        "prepared {} in {:?}",
        request.path.display(),
        started.elapsed()
    );
    Prepared {
        generation: request.generation,
        path: request.path,
        result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::read_fits_from_bytes;
    use fits_core::testutil::{synthetic_fits, SyntheticSpec};
    use std::time::{Duration, Instant};

    /// A frame whose every sample is `value`.
    fn frame(width: usize, height: usize, value: f64) -> Arc<FitsImage> {
        let spec = SyntheticSpec::new(width, height, -32);
        let bytes = synthetic_fits(&spec, &vec![value; width * height]).unwrap();
        Arc::new(read_fits_from_bytes(&bytes).unwrap())
    }

    /// A dark whose every sample is `value`.
    fn dark(width: usize, height: usize, value: f64) -> Arc<MasterFrame> {
        Arc::new(MasterFrame::from_image(&frame(width, height, value)))
    }

    /// Polls until nothing is outstanding, and returns what came.
    fn drain(preparer: &mut Preparer) -> Vec<Prepared> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut out = Vec::new();
        while Instant::now() < deadline {
            out.extend(preparer.poll());
            if !preparer.is_busy() {
                return out;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the preparer never finished");
    }

    #[test]
    fn a_recipe_of_nothing_does_nothing() {
        assert!(!Recipe::default().does_anything());
        assert!(Recipe {
            extract: true,
            ..Recipe::default()
        }
        .does_anything());
        assert!(Recipe {
            pattern: Some(BayerPattern::Rggb),
            ..Recipe::default()
        }
        .does_anything());
    }

    #[test]
    fn a_frame_is_prepared_as_the_recipe_says() {
        let recipe = Recipe {
            dark: Some(dark(8, 8, 100.0)),
            ..Recipe::default()
        };
        let prepared = prepare(&frame(8, 8, 500.0), &recipe).unwrap();
        assert!(prepared.data.iter().all(|v| (*v - 400.0).abs() < 1e-3));
    }

    #[test]
    fn a_frame_that_cannot_be_calibrated_says_why() {
        let recipe = Recipe {
            dark: Some(dark(4, 4, 100.0)),
            ..Recipe::default()
        };
        assert!(prepare(&frame(8, 8, 500.0), &recipe).is_err());
    }

    #[test]
    fn what_is_asked_for_comes_back_off_this_thread() {
        let mut preparer = Preparer::new();
        let recipe = Recipe {
            dark: Some(dark(8, 8, 100.0)),
            ..Recipe::default()
        };
        preparer.request(7, Path::new("a.fits"), frame(8, 8, 300.0), recipe);
        assert!(preparer.is_busy());

        let answers = drain(&mut preparer);
        assert_eq!(answers.len(), 1);
        let answer = &answers[0];
        assert_eq!(
            (answer.generation, answer.path.as_path()),
            (7, Path::new("a.fits"))
        );
        let image = answer.result.as_ref().unwrap();
        assert!((image.data[0] - 200.0).abs() < 1e-3);
    }

    #[test]
    fn a_failure_comes_back_as_one_and_does_not_stop_the_worker() {
        let mut preparer = Preparer::new();
        let wrong = Recipe {
            dark: Some(dark(4, 4, 100.0)),
            ..Recipe::default()
        };
        preparer.request(1, Path::new("a.fits"), frame(8, 8, 300.0), wrong);
        let answers = drain(&mut preparer);
        assert!(answers[0].result.is_err());

        let right = Recipe {
            dark: Some(dark(8, 8, 100.0)),
            ..Recipe::default()
        };
        preparer.request(2, Path::new("b.fits"), frame(8, 8, 300.0), right);
        let answers = drain(&mut preparer);
        assert!(answers[0].result.is_ok());
    }

    #[test]
    fn requests_made_faster_than_they_are_answered_leave_only_the_newest_waiting() {
        // Big enough that the first is still being worked on when the rest
        // arrive. Every one asked for while it is, but the last, is passed
        // over unanswered.
        let mut preparer = Preparer::new();
        let recipe = Recipe {
            extract: true,
            ..Recipe::default()
        };
        let big = frame(1500, 1000, 50.0);
        for n in 0..20u64 {
            preparer.request(
                n,
                Path::new(&format!("{n}.fits")),
                Arc::clone(&big),
                recipe.clone(),
            );
        }
        let answers = drain(&mut preparer);
        assert!(answers.len() < 20, "{} answered", answers.len());
        assert_eq!(answers.last().map(|a| a.generation), Some(19));
    }

    #[test]
    fn dropping_a_busy_preparer_does_not_hang() {
        let mut preparer = Preparer::new();
        let recipe = Recipe {
            extract: true,
            ..Recipe::default()
        };
        preparer.request(
            1,
            Path::new("a.fits"),
            frame(1500, 1000, 50.0),
            recipe.clone(),
        );
        preparer.request(2, Path::new("b.fits"), frame(1500, 1000, 50.0), recipe);
        let started = Instant::now();
        drop(preparer);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
