const MAX_JOBS: usize = 8;

pub fn worker_count(item_count: usize) -> usize {
    item_count.min(MAX_JOBS)
}

pub fn map_ordered<T, R, F>(items: &[T], operation: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    map_ordered_with_limit(items, MAX_JOBS, operation)
}

fn map_ordered_with_limit<T, R, F>(items: &[T], limit: usize, operation: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = items.len().min(limit.max(1));
    if workers <= 1 {
        return items.iter().map(operation).collect();
    }

    let next = std::sync::atomic::AtomicUsize::new(0);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut results = std::iter::repeat_with(|| None)
        .take(items.len())
        .collect::<Vec<Option<R>>>();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let next = &next;
            let operation = &operation;
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    if sender.send((index, operation(item))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for (index, result) in receiver {
            results[index] = Some(result);
        }
    });

    results
        .into_iter()
        .map(|result| result.expect("parallel worker exited without a result"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::map_ordered_with_limit;

    #[test]
    fn bounded_map_runs_concurrently_and_preserves_input_order() {
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let items = [0, 1, 2, 3, 4, 5];
        let results = map_ordered_with_limit(&items, 2, |item| {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            active.fetch_sub(1, Ordering::SeqCst);
            item * 2
        });

        assert_eq!(results, [0, 2, 4, 6, 8, 10]);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }
}
