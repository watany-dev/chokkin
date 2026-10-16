//! Order-preserving work sharing over scoped threads.
//!
//! std only: the stages that use it hand out independent per-file work, and a
//! shared cursor over `std::thread::scope` covers that without a pool crate.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Threads worth starting for `items` units of per-file work.
pub(crate) fn worker_count(items: usize) -> usize {
    // Below this, thread setup costs more than the work it overlaps.
    const MIN_ITEMS_PER_WORKER: usize = 32;
    if items <= MIN_ITEMS_PER_WORKER {
        return 1;
    }
    let available = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    available.min(items.div_ceil(MIN_ITEMS_PER_WORKER)).max(1)
}

/// `items.iter().map(work)`, spread over threads, results in `items` order.
///
/// A panic in `work` unwinds the caller, as the sequential map would.
pub(crate) fn map_in_order<T, R, F>(items: &[T], work: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = worker_count(items.len());
    if workers <= 1 {
        return items.iter().map(work).collect();
    }

    // A shared cursor rather than fixed chunks: file sizes vary by an order of
    // magnitude, so static splits leave workers idle behind one large file.
    let cursor = AtomicUsize::new(0);
    let run = || {
        let mut done = Vec::new();
        loop {
            let index = cursor.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(index) else {
                return done;
            };
            done.push((index, work(item)));
        }
    };
    let batches = std::thread::scope(|scope| {
        let mut helpers = Vec::with_capacity(workers);
        for _ in 1..workers {
            helpers.push(scope.spawn(run));
        }
        // The calling thread works too instead of idling until the join.
        let mut batches = vec![Ok(run())];
        batches.extend(helpers.into_iter().map(std::thread::ScopedJoinHandle::join));
        batches
    });

    let mut indexed = Vec::with_capacity(items.len());
    for batch in batches {
        match batch {
            Ok(done) => indexed.extend(done),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
    indexed.sort_unstable_by_key(|&(index, _)| index);
    indexed.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_count_stays_sequential_for_small_inputs() {
        assert_eq!(worker_count(0), 1);
        assert_eq!(worker_count(32), 1);
        assert!(worker_count(10_000) >= 1);
    }

    #[test]
    fn map_in_order_keeps_input_order_across_workers() {
        let items: Vec<usize> = (0..1_000).collect();
        let mapped = map_in_order(&items, |&item| item * 2);
        assert_eq!(
            mapped,
            items.iter().map(|item| item * 2).collect::<Vec<_>>()
        );
    }
}
