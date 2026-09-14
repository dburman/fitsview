//! Watching the open folder for frames as they are written.
//!
//! During a session the capture software adds a frame every few minutes. The
//! list should grow as it does, so a cloud or a slipped focus shows up while
//! there is still time to do something about it rather than the next morning.
//!
//! The folder is looked at every couple of seconds rather than subscribed to.
//! Change notifications are not delivered for network shares on every system,
//! and a camera controller saving over the network is exactly the case that
//! matters; a listing works everywhere, and costs a millisecond for a night's
//! frames. A look that takes longer — a large archive over a slow share — is
//! followed by a proportionately longer wait, so watching never becomes the
//! main thing the disk is doing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::folder::{self, FileEntry};
use crate::measurements::Stamp;

/// How long to wait between looks.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// How many times as long as a look took to wait before the next.
const BACKOFF: u32 = 10;

/// Size and age of each file, as last seen or last reported.
type Snapshot = HashMap<PathBuf, Option<Stamp>>;

/// Watches one folder on a thread of its own.
///
/// Dropping it stops the thread at its next wake, without waiting for a look
/// in progress to finish.
#[derive(Debug)]
pub struct Watcher {
    listings: mpsc::Receiver<Vec<FileEntry>>,
    /// Never sent on. Dropping it is what tells the thread to stop.
    _stop: mpsc::Sender<()>,
}

impl Watcher {
    /// Starts watching `dir`, whose files are already known to be `known`.
    #[must_use]
    pub fn start(dir: PathBuf, known: &[FileEntry], interval: Duration) -> Self {
        let mut reported = snapshot(known);
        let mut seen = reported.clone();
        let (stop, stopped) = mpsc::channel::<()>();
        let (tx, listings) = mpsc::channel();

        let spawned = std::thread::Builder::new()
            .name("fitsview-watch".into())
            .spawn(move || {
                let mut wait = interval;
                loop {
                    match stopped.recv_timeout(wait) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                    let started = Instant::now();
                    // A folder that cannot be listed — a drive unplugged for a
                    // moment — is not a folder that has been emptied.
                    let Ok(listing) = folder::list_files(&dir) else {
                        continue;
                    };
                    wait = interval.max(started.elapsed() * BACKOFF);

                    let listing = settled(listing, &reported, &mut seen);
                    let now = snapshot(&listing);
                    if now != reported {
                        reported = now;
                        if tx.send(listing).is_err() {
                            return;
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("not watching the folder: {e}");
        }

        Self {
            listings,
            _stop: stop,
        }
    }

    /// The newest listing that differs from the last one handed over, if the
    /// folder has changed since. Never blocks.
    pub fn poll(&self) -> Option<Vec<FileEntry>> {
        self.listings.try_iter().last()
    }
}

fn snapshot(entries: &[FileEntry]) -> Snapshot {
    entries.iter().map(|e| (e.path.clone(), e.stamp)).collect()
}

/// A listing with every file still being written held back.
///
/// A file is taken to have finished once it is the same size and age on two
/// looks running. Until then a new file is left out and a changed one is
/// reported as it was, so that the list does not flicker with a frame that is
/// half there, and nothing is measured that is about to change under it.
///
/// `reported` is what was last handed over; `seen` is what the previous look
/// found, and is brought up to date here.
fn settled(listing: Vec<FileEntry>, reported: &Snapshot, seen: &mut Snapshot) -> Vec<FileEntry> {
    let looked = snapshot(&listing);
    let mut out = Vec::with_capacity(listing.len());
    for mut entry in listing {
        let before = reported.get(&entry.path);
        let steady = seen.get(&entry.path) == Some(&entry.stamp);
        match before {
            Some(stamp) if *stamp == entry.stamp || steady => out.push(entry),
            Some(stamp) => {
                entry.stamp = *stamp;
                out.push(entry);
            }
            None if steady => out.push(entry),
            None => {}
        }
    }
    *seen = looked;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, size: u64) -> FileEntry {
        FileEntry {
            path: PathBuf::from("/night").join(name),
            name: name.into(),
            size,
            stamp: Some(Stamp {
                size,
                modified_secs: 1000 + size,
                modified_nanos: 0,
            }),
            flagged: false,
            stars: None,
            quality: None,
        }
    }

    fn names(listing: &[FileEntry]) -> Vec<(&str, u64)> {
        listing
            .iter()
            .map(|e| (e.name.as_str(), e.stamp.unwrap().size))
            .collect()
    }

    #[test]
    fn a_new_file_appears_once_it_has_stopped_growing() {
        let reported = snapshot(&[entry("a.fits", 10)]);
        let mut seen = reported.clone();

        // First seen part written: held back.
        let out = settled(
            vec![entry("a.fits", 10), entry("b.fits", 3)],
            &reported,
            &mut seen,
        );
        assert_eq!(names(&out), [("a.fits", 10)]);

        // Still growing: still held back.
        let out = settled(
            vec![entry("a.fits", 10), entry("b.fits", 7)],
            &reported,
            &mut seen,
        );
        assert_eq!(names(&out), [("a.fits", 10)]);

        // The same on two looks running: finished.
        let out = settled(
            vec![entry("a.fits", 10), entry("b.fits", 7)],
            &reported,
            &mut seen,
        );
        assert_eq!(names(&out), [("a.fits", 10), ("b.fits", 7)]);
    }

    #[test]
    fn a_file_being_rewritten_is_reported_as_it_was_until_it_settles() {
        let reported = snapshot(&[entry("a.fits", 10)]);
        let mut seen = reported.clone();

        let out = settled(vec![entry("a.fits", 4)], &reported, &mut seen);
        assert_eq!(names(&out), [("a.fits", 10)], "not yet");
        let out = settled(vec![entry("a.fits", 4)], &reported, &mut seen);
        assert_eq!(names(&out), [("a.fits", 4)]);
    }

    #[test]
    fn a_file_that_has_gone_is_gone_at_once() {
        let reported = snapshot(&[entry("a.fits", 10), entry("b.fits", 10)]);
        let mut seen = reported.clone();
        let out = settled(vec![entry("a.fits", 10)], &reported, &mut seen);
        assert_eq!(names(&out), [("a.fits", 10)]);
    }

    #[test]
    fn the_watcher_hands_over_a_frame_once_it_is_written() {
        use fits_core::testutil::{write_synthetic, SyntheticSpec};
        let dir = tempfile::tempdir().unwrap();
        let spec = SyntheticSpec::new(4, 4, 16);
        write_synthetic(dir.path(), "a.fits", &spec, &[1.0; 16]).unwrap();
        let known = folder::list_files(dir.path()).unwrap();

        let watcher = Watcher::start(dir.path().to_path_buf(), &known, Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(100));
        assert!(watcher.poll().is_none(), "nothing has changed");

        write_synthetic(dir.path(), "b.fits", &spec, &[1.0; 16]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let listing = loop {
            if let Some(listing) = watcher.poll() {
                break listing;
            }
            assert!(Instant::now() < deadline, "the new frame never arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        let names: Vec<&str> = listing.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a.fits", "b.fits"]);
    }
}
