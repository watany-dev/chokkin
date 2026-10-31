//! Order-preserving work sharing over scoped threads.
//!
//! std only: the stages that use it hand out independent per-file work, and a
//! shared cursor over `std::thread::scope` covers that without a pool crate.

use std::num::NonZeroUsize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// CPUs this process may use, read once: each
/// `std::thread::available_parallelism` call reads cgroup files on Linux.
pub(crate) fn available_parallelism() -> usize {
    static AVAILABLE: OnceLock<usize> = OnceLock::new();
    *AVAILABLE.get_or_init(|| std::thread::available_parallelism().map_or(1, NonZeroUsize::get))
}

/// Threads worth starting for `items` units of per-file work.
pub(crate) fn worker_count(items: usize) -> usize {
    // Below this, thread setup costs more than the work it overlaps.
    const MIN_ITEMS_PER_WORKER: usize = 32;
    if items <= MIN_ITEMS_PER_WORKER {
        return 1;
    }
    available_parallelism().min(items.div_ceil(MIN_ITEMS_PER_WORKER))
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
    map_on(worker_count(items.len()), items, init, work)
}

/// [`map_in_order`] over a few heavy `items` that each walk the project's
/// `units` files, such as one extractor per plugin: threads are worth
/// starting for the files, not for the item count.
pub(crate) fn map_heavy_in_order<T, R, F>(units: usize, items: &[T], work: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = worker_count(units).min(items.len()).max(1);
    map_on(workers, items, || (), |(), item| work(item))
}

/// [`map_in_order_with`] over `workers` threads.
fn map_on<T, S, R, I, F>(workers: usize, items: &[T], init: I, work: F) -> Vec<R>
where
    T: Sync,
    S: Send,
    R: Send,
    I: Fn() -> S + Sync,
    F: Fn(&mut S, &T) -> R + Sync,
{
    let batches = fold_on(
        workers,
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
    fold_on(worker_count(items.len()), items, init, work)
}

/// [`fold_per_worker`] over `workers` threads, the calling one included.
fn fold_on<T, S, I, F>(workers: usize, items: &[T], init: I, work: F) -> Vec<S>
where
    T: Sync,
    S: Send,
    I: Fn() -> S + Sync,
    F: Fn(&mut S, usize, &T) + Sync,
{
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

/// Drop `value` on a detached thread, so freeing a large intermediate
/// overlaps the caller's remaining work instead of running on its path.
///
/// A failed spawn drops the closure, and `value` with it, on the caller. A
/// process that exits first skips the rest of the drop, which only returns
/// memory to the OS.
pub(crate) fn drop_in_background<T: Send + 'static>(value: T) {
    let spawned = std::thread::Builder::new()
        .name("chokkin-drop".to_owned())
        .spawn(move || drop(value));
    drop(spawned);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_in_background_drops_the_value_off_the_calling_thread() {
        struct Probe(std::sync::mpsc::Sender<std::thread::ThreadId>);
        impl Drop for Probe {
            fn drop(&mut self) {
                let _ = self.0.send(std::thread::current().id());
            }
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        drop_in_background(Probe(sender));
        let dropped_on = receiver.recv().expect("the value is dropped");
        assert_ne!(dropped_on, std::thread::current().id());
    }

    #[test]
    fn worker_count_stays_sequential_for_small_inputs() {
        assert_eq!(worker_count(0), 1);
        assert_eq!(worker_count(32), 1);
    }

    #[test]
    fn worker_count_uses_every_cpu_for_large_inputs() {
        let available = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        assert_eq!(worker_count(32 * available), available);
    }

    #[test]
    fn join_hands_the_first_stage_to_a_helper_only_for_large_inputs() {
        let caller = std::thread::current().id();
        let (small, ()) = join(1, || std::thread::current().id(), || ());
        assert_eq!(small, caller);
        let (large, ()) = join(10_000, || std::thread::current().id(), || ());
        assert_eq!(large != caller, worker_count(10_000) > 1);
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
    fn map_heavy_in_order_spreads_few_items_only_for_large_inputs() {
        let items = [0, 1, 2, 3];
        let caller = std::thread::current().id();
        let small = map_heavy_in_order(1, &items, |_| std::thread::current().id());
        assert!(small.iter().all(|id| *id == caller));
        let large = map_heavy_in_order(10_000, &items, |&item| (item, item * 2));
        assert_eq!(large, vec![(0, 0), (1, 2), (2, 4), (3, 6)]);
        assert!(map_heavy_in_order(10_000, &[] as &[usize], |&item| item).is_empty());
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

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Lengths around `MIN_ITEMS_PER_WORKER` and its multiples, where
        /// the worker count changes.
        fn length() -> impl Strategy<Value = usize> {
            prop_oneof![0usize..8, 28usize..37, 60usize..69, 92usize..101]
        }

        proptest! {
            /// The order-keeping map equals the sequential one on any number
            /// of workers, even when each worker keeps a cache that changes
            /// nothing but how a result is reached.
            #[test]
            fn map_matches_the_sequential_map(len in length(), workers in 1usize..9, salt in any::<u64>()) {
                let items: Vec<u64> = (0..len as u64).map(|item| item ^ salt).collect();
                let work = |item: &u64| item.wrapping_mul(31).rotate_left(7);
                let sequential: Vec<u64> = items.iter().map(work).collect();
                prop_assert_eq!(map_on(workers, &items, || (), |(), item| work(item)), sequential.as_slice());
                let cached = map_on(
                    workers,
                    &items,
                    std::collections::HashMap::new,
                    |cache: &mut std::collections::HashMap<u64, u64>, item| {
                        *cache.entry(*item).or_insert_with(|| work(item))
                    },
                );
                prop_assert_eq!(cached, sequential);
            }

            /// Every item is folded exactly once, by one worker, in index
            /// order within that worker.
            #[test]
            fn fold_visits_each_item_once_in_order(len in length(), workers in 1usize..9) {
                let items: Vec<usize> = (0..len).collect();
                let states = fold_on(workers, &items, Vec::new, |seen: &mut Vec<usize>, index, &item| {
                    assert_eq!(index, item);
                    seen.push(item);
                });
                prop_assert!(states.len() <= workers.max(1));
                for seen in &states {
                    prop_assert!(seen.windows(2).all(|pair| pair[0] < pair[1]), "{:?}", seen);
                }
                let mut all: Vec<usize> = states.into_iter().flatten().collect();
                all.sort_unstable();
                prop_assert_eq!(all, items);
            }

            /// One worker up to `MIN_ITEMS_PER_WORKER` items, then one more
            /// per such batch, up to the CPUs, never fewer for more items.
            #[test]
            fn worker_count_grows_with_the_batches(len in 0usize..10_000) {
                let available = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
                let workers = worker_count(len);
                prop_assert_eq!(workers, available.min(len.div_ceil(32)).max(1));
                prop_assert!(worker_count(len + 1) >= workers);
            }
        }
    }
}
