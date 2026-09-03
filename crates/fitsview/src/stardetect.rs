//! Running star detection without making the interface wait.
//!
//! Detection costs about 64 ms on a full frame, which is four frames' worth at
//! 60 per second. It therefore never runs where the image is drawn.
//!
//! This is deliberately smaller than [`crate::jobs`]: there is no progress to
//! report and nothing to cancel, only a result that either arrives in time to
//! matter or is discarded because the user has moved on.

use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use fits_core::stars::{self, DetectionParams};
use fits_core::{debayer::BayerPattern, FitsImage, StarField};

/// A finished detection.
#[derive(Debug)]
struct Finding {
    generation: u64,
    field: StarField,
}

/// Detects stars on a worker thread, one frame at a time.
#[derive(Debug)]
pub struct StarDetector {
    sender: mpsc::Sender<Finding>,
    findings: mpsc::Receiver<Finding>,
    /// Which frame the last request was for, so a stale answer is recognised.
    generation: u64,
    /// Whether a request is outstanding.
    busy: bool,
    /// The frame the current result describes.
    settled: Option<(u64, PathBuf)>,
}

impl Default for StarDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl StarDetector {
    /// A detector with nothing running.
    #[must_use]
    pub fn new() -> Self {
        let (sender, findings) = mpsc::channel();
        Self {
            sender,
            findings,
            generation: 0,
            busy: false,
            settled: None,
        }
    }

    /// Whether the result already held describes this frame.
    #[must_use]
    pub fn has_result_for(&self, generation: u64, path: &std::path::Path) -> bool {
        self.settled
            .as_ref()
            .is_some_and(|(g, p)| *g == generation && p == path)
    }

    /// Whether a detection is running.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Starts detecting on `image`, abandoning any earlier request.
    ///
    /// The image is shared rather than copied, and the work happens on a thread
    /// of its own so the caller returns immediately.
    pub fn request(
        &mut self,
        generation: u64,
        path: PathBuf,
        image: Arc<FitsImage>,
        pattern: Option<BayerPattern>,
        params: DetectionParams,
    ) {
        self.generation = generation;
        self.busy = true;
        self.settled = Some((generation, path));

        let sender = self.sender.clone();
        std::thread::Builder::new()
            .name("fitsview-stars".into())
            .spawn(move || {
                let field = match pattern {
                    Some(pattern) => stars::detect_mosaic(&image, pattern, &params),
                    None => stars::detect(&image, &params),
                };
                // A closed channel means the application has moved on.
                let _ = sender.send(Finding { generation, field });
            })
            .ok();
    }

    /// Forgets whatever was found, so the next frame starts clean.
    pub fn clear(&mut self) {
        self.settled = None;
        self.busy = false;
    }

    /// Collects a finished detection, if one has arrived for the current frame.
    ///
    /// Never blocks. Results for a frame the user has already moved past are
    /// discarded rather than shown against the wrong image.
    pub fn poll(&mut self) -> Option<StarField> {
        let mut found = None;
        while let Ok(finding) = self.findings.try_recv() {
            if finding.generation == self.generation {
                self.busy = false;
                found = Some(finding.field);
            } else {
                log::debug!("discarding stars for a frame no longer shown");
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::read_fits_from_bytes;
    use fits_core::testutil::{gaussian_background, synthetic_fits, SyntheticSpec};
    use std::time::{Duration, Instant};

    /// A small sky with a handful of stars on it.
    fn frame() -> Arc<FitsImage> {
        let (w, h) = (120usize, 120usize);
        let mut pixels = gaussian_background(w, h, 1000.0, 10.0, 3);
        for (cx, cy) in [(30usize, 40usize), (70, 60), (90, 90)] {
            for dy in 0..7 {
                for dx in 0..7 {
                    let (x, y) = (cx + dx - 3, cy + dy - 3);
                    let r = ((dx as f64 - 3.0).powi(2) + (dy as f64 - 3.0).powi(2)) / 4.0;
                    pixels[y * w + x] += 9000.0 * (-r).exp();
                }
            }
        }
        let spec = SyntheticSpec::new(w, h, -32);
        Arc::new(read_fits_from_bytes(&synthetic_fits(&spec, &pixels).unwrap()).unwrap())
    }

    /// Polls until a result arrives.
    fn wait(detector: &mut StarDetector) -> StarField {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(field) = detector.poll() {
                return field;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("detection never finished");
    }

    #[test]
    fn a_request_returns_the_stars_it_found() {
        let mut detector = StarDetector::new();
        detector.request(
            1,
            PathBuf::from("/a.fits"),
            frame(),
            None,
            DetectionParams::default(),
        );
        assert!(detector.is_busy());

        let field = wait(&mut detector);
        assert_eq!(field.count(), 3);
        assert!(!detector.is_busy());
    }

    #[test]
    fn requesting_does_not_wait_for_the_answer() {
        // The whole reason this exists: 64 ms of work on a full frame must not
        // land on the thread that draws. Asserted by the shape of the thing
        // rather than by a stopwatch, which would be fragile: the request
        // returns with nothing available, and the answer turns up later.
        let mut detector = StarDetector::new();
        detector.request(
            1,
            PathBuf::from("/a.fits"),
            frame(),
            None,
            DetectionParams::default(),
        );

        assert!(
            detector.is_busy(),
            "the request should still be outstanding on return"
        );

        let field = wait(&mut detector);
        assert_eq!(field.count(), 3, "and the answer arrives afterwards");
        assert!(!detector.is_busy());
    }

    #[test]
    fn a_result_for_a_frame_already_moved_past_is_discarded() {
        let mut detector = StarDetector::new();
        detector.request(
            1,
            PathBuf::from("/a.fits"),
            frame(),
            None,
            DetectionParams::default(),
        );
        // The user steps to the next image before the answer comes back.
        detector.request(
            2,
            PathBuf::from("/b.fits"),
            frame(),
            None,
            DetectionParams::default(),
        );

        let field = wait(&mut detector);
        assert_eq!(
            field.count(),
            3,
            "the answer should be for the second frame"
        );
        assert!(detector.has_result_for(2, std::path::Path::new("/b.fits")));
        assert!(!detector.has_result_for(1, std::path::Path::new("/a.fits")));
    }

    #[test]
    fn nothing_is_reported_before_a_request() {
        let mut detector = StarDetector::new();
        assert!(detector.poll().is_none());
        assert!(!detector.is_busy());
        assert!(!detector.has_result_for(0, std::path::Path::new("/a.fits")));
    }

    #[test]
    fn clearing_forgets_the_current_frame() {
        let mut detector = StarDetector::new();
        detector.request(
            1,
            PathBuf::from("/a.fits"),
            frame(),
            None,
            DetectionParams::default(),
        );
        wait(&mut detector);
        assert!(detector.has_result_for(1, std::path::Path::new("/a.fits")));

        detector.clear();
        assert!(!detector.has_result_for(1, std::path::Path::new("/a.fits")));
    }
}
