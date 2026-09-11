//! 轻量并行任务执行：AtomicUsize 工作窃取 + scoped threads。
//!
//! 不引入 rayon，线程数显式可控（EmEditor 经验：线程数按内存预算，
//! 不能默认吃满逻辑核）。任务粒度大（64MB 块），锁竞争可忽略。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// 把 0..n_tasks 均匀分给线程池并行执行，按任务序号返回结果。
/// `threads == 0` 时取 [`crate::sysinfo::ideal_thread_count`]。
pub fn parallel_map<T, F>(n_tasks: usize, threads: usize, f: F) -> Vec<T>
where
    F: Fn(usize) -> T + Sync,
    T: Send,
{
    if n_tasks == 0 {
        return Vec::new();
    }
    let threads = if threads == 0 {
        crate::sysinfo::ideal_thread_count()
    } else {
        threads.max(1)
    }
    .min(n_tasks);
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<T>>> = Mutex::new((0..n_tasks).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n_tasks {
                    break;
                }
                let t = f(i);
                let mut guard = slots.lock().unwrap();
                guard[i] = Some(t);
            });
        }
    });
    slots
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|o| o.expect("并行任务槽位未被填充"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn maps_all_tasks_once() {
        let count = AtomicUsize::new(0);
        let out = parallel_map(
            100,
            4,
            |i| {
                count.fetch_add(1, Ordering::Relaxed);
                i * 2
            },
        );
        assert_eq!(count.load(Ordering::Relaxed), 100);
        assert_eq!(out, (0..100).map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn zero_tasks() {
        let out: Vec<u32> = parallel_map(0, 4, |_| 1);
        assert!(out.is_empty());
    }
}
