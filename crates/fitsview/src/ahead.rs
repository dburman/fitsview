//! Working on one item while the next is got ready.
//!
//! Stacking and making masters read and prepare every frame, then add it, one
//! after the other: while a frame is read from disk the processors wait, and
//! while it is added the disk does. Preparing the next frame on a thread of its
//! own while this one is added hides whichever is shorter behind the other.
//!
//! Exactly one item ahead, and no more: a prepared 61-megapixel colour frame is
//! most of a gigabyte, and a second one waiting would be another.

use std::sync::mpsc;

/// Calls `consume` with each of `items`, in order, and what `produce` made of
/// it — `produce` running on a thread of its own, one item ahead.
///
/// `consume` returns false to stop, and then nothing further is produced
/// beyond the item already being made.
pub fn each<T, R>(
    items: &[T],
    produce: impl Fn(&T) -> R + Send + Sync,
    mut consume: impl FnMut(&T, R) -> bool,
) where
    T: Sync,
    R: Send,
{
    std::thread::scope(|scope| {
        // No buffer: the producer holds the item it has made until it is
        // taken, so there are never two waiting.
        let (tx, rx) = mpsc::sync_channel(0);
        scope.spawn(move || {
            for item in items {
                // Fails once the consumer has stopped taking items.
                if tx.send((item, produce(item))).is_err() {
                    break;
                }
            }
        });
        for (item, made) in rx.iter() {
            if !consume(item, made) {
                break;
            }
        }
        // Dropping the receiver is what releases a producer waiting to hand
        // over an item nobody will take; the scope then waits for it to end.
        drop(rx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn every_item_arrives_in_order_with_what_was_made_of_it() {
        let items: Vec<u32> = (0..50).collect();
        let mut seen = Vec::new();
        each(
            &items,
            |n| n * 10,
            |item, made| {
                seen.push((*item, made));
                true
            },
        );
        let expected: Vec<(u32, u32)> = (0..50).map(|n| (n, n * 10)).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn stopping_stops_the_producing_too() {
        let made = AtomicUsize::new(0);
        let items: Vec<u32> = (0..100).collect();
        let mut taken = 0;
        each(
            &items,
            |n| {
                made.fetch_add(1, Ordering::Relaxed);
                *n
            },
            |_, _| {
                taken += 1;
                taken < 5
            },
        );
        assert_eq!(taken, 5);
        // The five taken, the one handed over while the fifth was, and the one
        // being made when it stopped.
        assert!(
            made.load(Ordering::Relaxed) <= 7,
            "made {}",
            made.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn making_the_next_overlaps_using_this_one() {
        // Ten items, each taking 20 ms to make and 20 ms to use: 400 ms one
        // after the other, about 220 ms overlapped.
        let items: Vec<u32> = (0..10).collect();
        let started = Instant::now();
        each(
            &items,
            |_| std::thread::sleep(Duration::from_millis(20)),
            |_, ()| {
                std::thread::sleep(Duration::from_millis(20));
                true
            },
        );
        let took = started.elapsed();
        assert!(took < Duration::from_millis(330), "took {took:?}");
    }

    #[test]
    fn no_more_than_one_item_waits() {
        // Making is instant and using is slow, so the producer is always
        // ahead: it may hold one made item, never two.
        let waiting = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let items: Vec<u32> = (0..20).collect();
        each(
            &items,
            |_| {
                let now = waiting.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
            },
            |_, ()| {
                std::thread::sleep(Duration::from_millis(5));
                waiting.fetch_sub(1, Ordering::SeqCst);
                true
            },
        );
        // The one in use and the one made and waiting.
        assert!(
            most.load(Ordering::SeqCst) <= 2,
            "{} made at once",
            most.load(Ordering::SeqCst)
        );
    }
}
