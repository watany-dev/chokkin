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
    available.min(items.div_ceil(MIN_ITEMS_PER_WORKER))
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
    map_in_order_with(items, || (), |(), item| work(item))
}

/// [`map_in_order`] with per-worker state from `init`, such as a cache.
///
/// Each worker sees its items in increasing index order, so state built from
/// earlier items is visible to later ones on the same worker; which worker
/// gets an item is unspecified, so `work` must not let that state change its
/// result.
pub(crate) fn map_in_order_with<T, S, R, I, F>(items: &[T], init: I, work: F) -> Vec<R>
where
    T: Sync,
    S: Send,
    R: Send,
    I: Fn() -> S + Sync,
    F: Fn(&mut S, &T) -> R + Sync,
{
    let batches = fold_per_worker(
        items,
        || (init(), Vec::new()),
        |(state, done), index, item| done.push((index, work(state, item))),
    );
    let mut indexed: Vec<_> = batches.into_iter().flat_map(|(_, done)| done).collect();
    indexed.sort_unstable_by_key(|&(index, _)| index);
    indexed.into_iter().map(|(_, result)| result).collect()
}

/// Run `work` on every item and its index over threads, each worker folding
/// into its own state from `init`; returns the workers' states.
///
/// Each worker sees its items in increasing index order. Which worker gets
/// an item, and the order of the returned states, are unspecified: callers
/// merge them order-independently. A panic in `work` unwinds the caller.
pub(crate) fn fold_per_worker<T, S, I, F>(items: &[T], init: I, work: F) -> Vec<S>
where
    T: Sync,
    S: Send,
    I: Fn() -> S + Sync,
    F: Fn(&mut S, usize, &T) + Sync,
{
    let workers = worker_count(items.len());
    if workers <= 1 {
        let mut state = init();
        for (index, item) in items.iter().enumerate() {
            work(&mut state, index, item);
        }
        return vec![state];
    }

    // A shared cursor rather than fixed chunks: file sizes vary by an order of
    // magnitude, so static splits leave workers idle behind one large file.
    let cursor = AtomicUsize::new(0);
    let run = || {
        let mut state = init();
        loop {
            let index = cursor.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(index) else {
                return state;
            };
            work(&mut state, index, item);
        }
    };
    let states = std::thread::scope(|scope| {
        let mut helpers = Vec::with_capacity(workers);
        for _ in 1..workers {
            helpers.push(scope.spawn(run));
        }
        // The calling thread works too instead of idling until the join.
        let mut states = vec![Ok(run())];
        states.extend(helpers.into_iter().map(std::thread::ScopedJoinHandle::join));
        states
    });
    states
        .into_iter()
        .map(|state| state.unwrap_or_else(|payload| std::panic::resume_unwind(payload)))
        .collect()
}

/// `(a(), b())`, with `a` on a helper thread while the calling thread runs
/// `b`, for two independent stages over `units` files; sequential where
/// [`worker_count`] would be. A panic in either unwinds the caller.
pub(crate) fn join<A, B, RA, RB>(units: usize, a: A, b: B) -> (RA, RB)
where
    A: FnOnce() -> RA + Send,
    RA: Send,
    B: FnOnce() -> RB,
{
    if worker_count(units) <= 1 {
        return (a(), b());
    }
    std::thread::scope(|scope| {
        let helper = scope.spawn(a);
        let b = b();
        let a = helper
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
        (a, b)
    })
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

    #[test]
    fn map_in_order_with_gives_each_worker_its_items_in_order() {
        let items: Vec<usize> = (0..1_000).collect();
        // Each result is the item seen just before it on the same worker.
        let previous = map_in_order_with(
            &items,
            || None,
            |last: &mut Option<usize>, &item| last.replace(item),
        );
        for (item, before) in items.iter().zip(previous) {
            assert!(
                before.is_none_or(|before| before < *item),
                "{before:?} {item}"
            );
        }
    }

    #[test]
    fn join_returns_both_results_in_place() {
        assert_eq!(join(0, || 1, || "b"), (1, "b"));
        assert_eq!(join(10_000, || 1, || "b"), (1, "b"));
    }

    #[test]
    fn fold_per_worker_visits_every_item_once() {
        let items: Vec<usize> = (0..1_000).collect();
        let states = fold_per_worker(&items, Vec::new, |seen: &mut Vec<usize>, index, &item| {
            assert_eq!(index, item);
            seen.push(item);
        });
        let mut seen: Vec<usize> = states.into_iter().flatten().collect();
        seen.sort_unstable();
        assert_eq!(seen, items);
    }
}
