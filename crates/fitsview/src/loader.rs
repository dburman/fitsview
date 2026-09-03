//! Background image loading with a bounded cache.
//!
//! Decoding a 24-megapixel frame takes tens of milliseconds. Doing that on the
//! UI thread would drop frames every time the selection moved, so it happens on
//! a worker thread and the UI collects results when they arrive.
//!
//! Two design points are worth stating because they are what make holding down
//! the arrow key feel instant rather than sluggish:
//!
//! 1. **The queue is replaced, not appended to.** When the selection moves, the
//!    UI hands the worker the complete list of what it now wants. Work that is
//!    no longer wanted is dropped before it starts, so jumping to the end of a
//!    folder does not wait for fifty intermediate files to decode.
//! 2. **Results are cached and neighbours are prefetched.** Stepping back to the
//!    previous file is free, and stepping forward usually finds the image
//!    already decoded.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};

use fits_core::{quality, read_fits, FitsImage, Quality};

/// Default number of decoded images held in memory.
pub const DEFAULT_MAX_ENTRIES: usize = 8;

/// Default cap on decoded image bytes, chosen so a folder of full-frame images
/// cannot exhaust memory on a modest machine.
pub const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Bytes a decoded image occupies, near enough for a memory cap.
#[must_use]
pub fn image_bytes(image: &FitsImage) -> usize {
    image.data.len() * std::mem::size_of::<f32>()
}

/// A bounded least-recently-used cache of decoded images.
///
/// Bounded by both entry count and total bytes, because eight thumbnails and
/// eight full frames are very different amounts of memory.
#[derive(Debug)]
pub struct Cache {
    entries: HashMap<PathBuf, Arc<FitsImage>>,
    /// Least recently used at the front.
    order: VecDeque<PathBuf>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Cache {
    /// A cache with the given bounds. Both are clamped to at least one entry,
    /// so a cache can always hold the image being displayed.
    #[must_use]
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            max_entries: max_entries.max(1),
            max_bytes,
        }
    }

    /// Fetches an image, marking it as recently used.
    pub fn get(&mut self, path: &Path) -> Option<Arc<FitsImage>> {
        let image = self.entries.get(path)?.clone();
        self.touch(path);
        Some(image)
    }

    /// Whether the cache holds this path, without affecting its position.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        self.entries.contains_key(path)
    }

    /// Inserts an image, evicting least-recently-used entries until the cache
    /// is within both bounds.
    ///
    /// The newly inserted entry is never evicted, even if it alone exceeds the
    /// byte cap: the alternative is being unable to display a very large image
    /// at all.
    pub fn insert(&mut self, path: PathBuf, image: Arc<FitsImage>) {
        if self.entries.contains_key(&path) {
            self.touch(&path);
            return;
        }
        self.bytes += image_bytes(&image);
        self.entries.insert(path.clone(), image);
        self.order.push_back(path);
        self.evict();
    }

    /// Removes an entry, for example after its file is deleted.
    pub fn remove(&mut self, path: &Path) {
        if let Some(image) = self.entries.remove(path) {
            self.bytes = self.bytes.saturating_sub(image_bytes(&image));
            self.order.retain(|p| p != path);
        }
    }

    /// Empties the cache, as when a different folder is opened.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// Number of cached images.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total bytes of cached image data.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Moves a path to the most-recently-used end.
    fn touch(&mut self, path: &Path) {
        if let Some(i) = self.order.iter().position(|p| p == path) {
            let owned = self.order.remove(i).expect("index came from this deque");
            self.order.push_back(owned);
        }
    }

    /// Drops least-recently-used entries until both bounds are satisfied.
    fn evict(&mut self) {
        while self.order.len() > self.max_entries
            || (self.bytes > self.max_bytes && self.order.len() > 1)
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(image) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(image_bytes(&image));
            }
        }
    }
}

/// What the worker is asked to decode.
#[derive(Debug, Clone)]
struct Request {
    path: PathBuf,
    generation: u64,
}

/// What the worker sends back.
#[derive(Debug)]
struct Response {
    path: PathBuf,
    generation: u64,
    result: Result<Arc<FitsImage>, String>,
    millis: f64,
    quality: Option<Quality>,
}

/// The worker's queue, replaced wholesale whenever the UI's wishes change.
#[derive(Debug)]
struct Queue {
    /// `None` once the loader is dropped, which tells the worker to stop.
    pending: Option<VecDeque<Request>>,
    /// What the worker has taken off the queue and is decoding right now.
    ///
    /// Replacing the queue cannot cancel work already started, so this is how
    /// the loader knows a response is still coming for that path and does not
    /// queue it a second time.
    current: Option<PathBuf>,
}

/// An image that finished loading.
#[derive(Debug, Clone)]
pub struct Arrival {
    /// Which file it was.
    pub path: PathBuf,
    /// The decoded image, or the reason it could not be decoded.
    pub result: Result<Arc<FitsImage>, String>,
    /// How long the read took, in milliseconds.
    pub millis: f64,
    /// What the frame looks like, measured here rather than on the interface
    /// thread.
    ///
    /// The worker has just read the image, so measuring costs it little, and
    /// doing it here keeps a step through a folder free of the statistics work
    /// that would otherwise drop frames.
    pub quality: Option<Quality>,
}

/// Loads images on a worker thread and caches the results.
#[derive(Debug)]
pub struct Loader {
    cache: Cache,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    responses: mpsc::Receiver<Response>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Paths handed to the worker and not yet returned.
    inflight: HashSet<PathBuf>,
    /// Bumped when the folder changes, so results for the old folder are
    /// recognised as stale and dropped.
    generation: u64,
}

impl Default for Loader {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_BYTES)
    }
}

impl Loader {
    /// Starts a loader with the given cache bounds.
    #[must_use]
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        let queue = Arc::new((
            Mutex::new(Queue {
                pending: Some(VecDeque::new()),
                current: None,
            }),
            Condvar::new(),
        ));
        let (tx, responses) = mpsc::channel();

        let worker_queue = Arc::clone(&queue);
        let worker = std::thread::Builder::new()
            .name("fitsview-loader".to_string())
            .spawn(move || worker_loop(&worker_queue, &tx))
            .ok();

        Self {
            cache: Cache::new(max_entries, max_bytes),
            queue,
            responses,
            worker,
            inflight: HashSet::new(),
            generation: 0,
        }
    }

    /// The cache, for inspection and for taking already-decoded images.
    pub fn cache_mut(&mut self) -> &mut Cache {
        &mut self.cache
    }

    /// The cache, read only.
    #[must_use]
    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Invalidates outstanding work and empties the cache.
    ///
    /// Called when a different folder is opened, so results still in flight for
    /// the previous folder are discarded when they arrive.
    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.inflight.clear();
        self.cache.clear();
        self.set_queue(Vec::new());
    }

    /// Asks for `paths`, in priority order, replacing any previous wishes.
    ///
    /// Anything previously queued but no longer wanted is dropped before it is
    /// decoded, which is what keeps a jump across a folder responsive.
    ///
    /// Two things are deliberately not re-queued: paths already in the cache,
    /// and the path the worker is decoding at this moment. The latter cannot be
    /// cancelled, and its result will arrive on its own, so queueing it again
    /// would decode the same file twice.
    pub fn request(&mut self, paths: &[PathBuf]) {
        let current = self.current_path();

        let mut queued = Vec::new();
        let mut expected = HashSet::new();

        for path in paths {
            if self.cache.contains(path) {
                continue;
            }
            // Still expect a response for it, but do not queue it again.
            if current.as_deref() == Some(path.as_path()) {
                expected.insert(path.clone());
                continue;
            }
            expected.insert(path.clone());
            queued.push(Request {
                path: path.clone(),
                generation: self.generation,
            });
        }

        // A path that was queued but is no longer wanted has just been dropped
        // from the queue, so stop expecting it. One already being decoded still
        // arrives, and `poll` discards it if nothing wants it.
        self.inflight = expected;
        if let Some(c) = current {
            self.inflight.insert(c);
        }

        self.set_queue(queued);
    }

    /// The path the worker is decoding right now, if any.
    fn current_path(&self) -> Option<PathBuf> {
        let (lock, _) = &*self.queue;
        lock.lock().ok().and_then(|q| q.current.clone())
    }

    /// Collects everything the worker has finished since the last call.
    ///
    /// Never blocks. Results from a previous folder are dropped.
    pub fn poll(&mut self) -> Vec<Arrival> {
        let mut out = Vec::new();
        while let Ok(response) = self.responses.try_recv() {
            self.inflight.remove(&response.path);
            if response.generation != self.generation {
                log::debug!("dropping stale result for {}", response.path.display());
                continue;
            }
            if let Ok(image) = &response.result {
                self.cache.insert(response.path.clone(), Arc::clone(image));
            }
            out.push(Arrival {
                path: response.path,
                result: response.result,
                millis: response.millis,
                quality: response.quality,
            });
        }
        out
    }

    /// Whether anything is still being decoded.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        !self.inflight.is_empty()
    }

    /// Replaces the worker's queue.
    fn set_queue(&self, requests: Vec<Request>) {
        let (lock, cv) = &*self.queue;
        if let Ok(mut q) = lock.lock() {
            if let Some(pending) = q.pending.as_mut() {
                pending.clear();
                pending.extend(requests);
            }
        }
        cv.notify_all();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.queue;
            if let Ok(mut q) = lock.lock() {
                q.pending = None;
            }
            cv.notify_all();
        }
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// The worker thread: take the highest-priority request, decode it, report back.
fn worker_loop(queue: &Arc<(Mutex<Queue>, Condvar)>, tx: &mpsc::Sender<Response>) {
    let (lock, cv) = &**queue;
    loop {
        let request = {
            let mut guard = match lock.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let taken = loop {
                match guard.pending.as_mut() {
                    None => return, // the loader was dropped
                    Some(pending) => {
                        if let Some(next) = pending.pop_front() {
                            break next;
                        }
                    }
                }
                guard = match cv.wait(guard) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            };
            guard.current = Some(taken.path.clone());
            taken
        };

        let started = std::time::Instant::now();
        let decoded = read_fits(&request.path);
        let millis = started.elapsed().as_secs_f64() * 1000.0;

        // Measured here, on the worker, while the samples are to hand.
        let quality = decoded.as_ref().ok().map(quality::measure);
        let result = decoded.map(Arc::new).map_err(|e| e.to_string());

        if let Ok(mut guard) = lock.lock() {
            guard.current = None;
        }

        if tx
            .send(Response {
                path: request.path,
                generation: request.generation,
                result,
                millis,
                quality,
            })
            .is_err()
        {
            return; // nobody is listening any more
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fits_core::testutil::{write_synthetic, SyntheticSpec};
    use fits_core::{read_fits_from_bytes, testutil::synthetic_fits};
    use std::time::{Duration, Instant};

    /// A decoded image of a given size, for filling the cache.
    fn image(width: usize, height: usize) -> Arc<FitsImage> {
        let spec = SyntheticSpec::new(width, height, 16);
        let bytes = synthetic_fits(&spec, &vec![1.0; width * height]).unwrap();
        Arc::new(read_fits_from_bytes(&bytes).unwrap())
    }

    fn path(n: usize) -> PathBuf {
        PathBuf::from(format!("/tmp/x/f{n}.fits"))
    }

    #[test]
    fn image_bytes_counts_four_bytes_per_sample() {
        let img = image(10, 10);
        assert_eq!(image_bytes(&img), 100 * 4);
    }

    #[test]
    fn a_cached_image_can_be_fetched_back() {
        let mut c = Cache::new(4, usize::MAX);
        c.insert(path(0), image(4, 4));
        assert!(c.contains(&path(0)));
        assert!(c.get(&path(0)).is_some());
        assert!(c.get(&path(1)).is_none());
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn the_cache_evicts_by_entry_count() {
        let mut c = Cache::new(3, usize::MAX);
        for i in 0..5 {
            c.insert(path(i), image(2, 2));
        }
        assert_eq!(c.len(), 3);
        // The two oldest are gone, the three newest remain.
        assert!(!c.contains(&path(0)));
        assert!(!c.contains(&path(1)));
        for i in 2..5 {
            assert!(c.contains(&path(i)), "expected f{i} to survive");
        }
    }

    #[test]
    fn the_cache_evicts_by_total_bytes() {
        // Each image is 100 samples, 400 bytes. Allow only 1000.
        let mut c = Cache::new(100, 1000);
        for i in 0..5 {
            c.insert(path(i), image(10, 10));
        }
        assert!(c.bytes() <= 1000, "cache held {} bytes", c.bytes());
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn an_image_larger_than_the_whole_budget_is_still_kept() {
        // Otherwise a very large image could never be displayed at all.
        let mut c = Cache::new(4, 10);
        c.insert(path(0), image(100, 100));
        assert_eq!(c.len(), 1);
        assert!(c.bytes() > 10);
    }

    #[test]
    fn recently_used_entries_survive_eviction() {
        let mut c = Cache::new(2, usize::MAX);
        c.insert(path(0), image(2, 2));
        c.insert(path(1), image(2, 2));
        // Touch the oldest so it is no longer the least recently used.
        assert!(c.get(&path(0)).is_some());
        c.insert(path(2), image(2, 2));

        assert!(c.contains(&path(0)), "the touched entry should survive");
        assert!(
            !c.contains(&path(1)),
            "the untouched entry should be evicted"
        );
        assert!(c.contains(&path(2)));
    }

    #[test]
    fn inserting_the_same_path_twice_does_not_double_count_bytes() {
        let mut c = Cache::new(4, usize::MAX);
        c.insert(path(0), image(10, 10));
        let once = c.bytes();
        c.insert(path(0), image(10, 10));
        assert_eq!(c.bytes(), once);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn removing_and_clearing_release_the_accounted_bytes() {
        let mut c = Cache::new(4, usize::MAX);
        c.insert(path(0), image(10, 10));
        c.insert(path(1), image(10, 10));
        c.remove(&path(0));
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), 400);

        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.bytes(), 0);
    }

    #[test]
    fn a_zero_entry_bound_still_holds_one_image() {
        let mut c = Cache::new(0, usize::MAX);
        c.insert(path(0), image(2, 2));
        assert_eq!(c.len(), 1);
    }

    /// Polls the loader until `done` holds, returning everything that arrived.
    ///
    /// The predicate inspects the loader but must not poll it, or it would
    /// consume the arrivals this function is collecting.
    fn pump(
        loader: &mut Loader,
        timeout: Duration,
        mut done: impl FnMut(&Loader) -> bool,
    ) -> Vec<Arrival> {
        let deadline = Instant::now() + timeout;
        let mut arrivals = Vec::new();
        while Instant::now() < deadline {
            arrivals.extend(loader.poll());
            if done(loader) {
                return arrivals;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("timed out waiting for the loader; arrivals so far: {arrivals:?}");
    }

    #[test]
    fn the_worker_decodes_a_requested_file() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, 16);
        let p = write_synthetic(dir.path(), "a.fits", &spec, &vec![3.0; 64]).unwrap();

        let mut loader = Loader::default();
        loader.request(std::slice::from_ref(&p));
        pump(&mut loader, Duration::from_secs(5), |l| {
            l.cache().contains(&p)
        });

        let img = loader.cache_mut().get(&p).expect("should be cached");
        assert_eq!((img.width, img.height), (8, 8));
        assert!(!loader.is_busy());
    }

    #[test]
    fn a_failed_read_is_reported_rather_than_silently_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("broken.fits");
        std::fs::write(&bad, b"SIMPLE but truncated nonsense").unwrap();

        let mut loader = Loader::default();
        loader.request(std::slice::from_ref(&bad));

        let arrivals = pump(&mut loader, Duration::from_secs(5), |l| !l.is_busy());
        let reported = arrivals.iter().find(|a| a.path == bad).expect("no arrival");
        assert!(
            reported.result.is_err(),
            "a bad file should report an error"
        );
        assert!(
            !loader.cache().contains(&bad),
            "failures must not be cached"
        );
    }

    #[test]
    fn a_decoded_frame_arrives_already_measured() {
        // Measuring on the worker is what keeps stepping through a folder free
        // of statistics work on the interface thread.
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(16, 16, -32);
        let p = write_synthetic(dir.path(), "a.fits", &spec, &vec![250.0; 256]).unwrap();

        let mut loader = Loader::default();
        loader.request(std::slice::from_ref(&p));

        let arrivals = pump(&mut loader, Duration::from_secs(5), |l| !l.is_busy());
        let arrival = arrivals.first().expect("no arrival");
        let quality = arrival.quality.expect("should have been measured");
        assert!((quality.background - 250.0).abs() < 0.01);
    }

    #[test]
    fn a_failed_read_carries_no_measurement() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("broken.fits");
        std::fs::write(&bad, b"SIMPLE but nonsense").unwrap();

        let mut loader = Loader::default();
        loader.request(std::slice::from_ref(&bad));
        let arrivals = pump(&mut loader, Duration::from_secs(5), |l| !l.is_busy());
        assert!(arrivals[0].quality.is_none());
    }

    #[test]
    fn results_are_reported_with_a_measured_duration() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, 16);
        let p = write_synthetic(dir.path(), "a.fits", &spec, &vec![1.0; 64]).unwrap();

        let mut loader = Loader::default();
        loader.request(std::slice::from_ref(&p));

        let arrivals = pump(&mut loader, Duration::from_secs(5), |l| !l.is_busy());
        let millis = arrivals.first().expect("no arrival").millis;
        assert!(millis >= 0.0 && millis.is_finite(), "got {millis}");
    }

    #[test]
    fn an_already_cached_file_is_not_requested_again() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, 16);
        let p = write_synthetic(dir.path(), "a.fits", &spec, &vec![1.0; 64]).unwrap();

        let mut loader = Loader::default();
        loader.request(std::slice::from_ref(&p));
        pump(&mut loader, Duration::from_secs(5), |l| {
            l.cache().contains(&p)
        });

        loader.request(std::slice::from_ref(&p));
        assert!(!loader.is_busy(), "a cached file should need no work");
    }

    #[test]
    fn results_for_a_previous_folder_are_dropped_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(64, 64, 16);
        let p = write_synthetic(dir.path(), "a.fits", &spec, &vec![1.0; 4096]).unwrap();

        let mut loader = Loader::default();
        loader.request(std::slice::from_ref(&p));
        // Change folders before the result can be collected.
        loader.reset();

        // Give the worker time to finish and reply, then confirm nothing from
        // the old generation reaches the caller or the cache.
        std::thread::sleep(Duration::from_millis(200));
        let arrivals = loader.poll();
        assert!(
            arrivals.is_empty(),
            "stale results should not be delivered: {arrivals:?}"
        );
        assert!(!loader.cache().contains(&p));
    }

    #[test]
    fn resetting_clears_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(8, 8, 16);
        let p = write_synthetic(dir.path(), "a.fits", &spec, &vec![1.0; 64]).unwrap();

        let mut loader = Loader::default();
        loader.request(std::slice::from_ref(&p));
        pump(&mut loader, Duration::from_secs(5), |l| {
            l.cache().contains(&p)
        });

        loader.reset();
        assert!(loader.cache().is_empty());
    }

    #[test]
    fn a_new_request_replaces_the_old_queue_rather_than_appending() {
        // This is what stops a jump to the end of a folder from waiting for
        // everything in between.
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(256, 256, 16);
        let pixels = vec![1.0; 256 * 256];
        let paths: Vec<PathBuf> = (0..12)
            .map(|i| write_synthetic(dir.path(), &format!("f{i:02}.fits"), &spec, &pixels).unwrap())
            .collect();

        let mut loader = Loader::default();
        loader.request(&paths);
        // Immediately change our mind and ask only for the last file.
        let wanted = paths[11].clone();
        loader.request(std::slice::from_ref(&wanted));

        pump(&mut loader, Duration::from_secs(10), |l| {
            l.cache().contains(&wanted)
        });

        // Only the file still wanted, plus whatever was already being decoded
        // when we changed our mind, should have been loaded. Certainly not all
        // twelve.
        assert!(
            loader.cache().len() <= 2,
            "cache held {} entries, so the queue was not replaced",
            loader.cache().len()
        );
    }

    #[test]
    fn dropping_the_loader_stops_the_worker() {
        // The Drop implementation joins the worker; if it did not shut down,
        // this test would hang rather than fail, which is still a signal.
        let loader = Loader::default();
        drop(loader);
    }
}
