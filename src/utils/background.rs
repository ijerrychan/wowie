//! Small bounded executor for UI and provider background work.
//!
//! A fixed worker set keeps rapid searches and icon/detail refreshes from
//! creating one operating-system thread per request. Callers still retain
//! their generation checks as the final write-back guard; a cancellation token
//! can additionally prevent queued work from starting.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{sync_channel, SyncSender, TrySendError},
    Arc, OnceLock,
};

type Task = Box<dyn FnOnce() + Send + 'static>;

const WORKER_COUNT: usize = 4;
const QUEUE_CAPACITY: usize = 128;

#[derive(Clone, Default)]
pub(crate) struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct BackgroundPool {
    sender: SyncSender<Task>,
    metrics: Arc<PoolMetrics>,
}

#[derive(Default)]
struct PoolMetrics {
    pending: AtomicUsize,
    active: AtomicUsize,
    peak_pending: AtomicUsize,
    peak_active: AtomicUsize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct BackgroundStats {
    pub(crate) pending: usize,
    pub(crate) active: usize,
    pub(crate) peak_pending: usize,
    pub(crate) peak_active: usize,
}

struct ActiveTask(Arc<PoolMetrics>);

impl Drop for ActiveTask {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl BackgroundPool {
    fn new() -> Self {
        let (sender, receiver) = sync_channel::<Task>(QUEUE_CAPACITY);
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        let metrics = Arc::new(PoolMetrics::default());
        for _ in 0..WORKER_COUNT {
            let receiver = Arc::clone(&receiver);
            let metrics = Arc::clone(&metrics);
            std::thread::Builder::new()
                .name("wowie-background".into())
                .spawn(move || loop {
                    let task = receiver.lock().ok().and_then(|queue| queue.recv().ok());
                    let Some(task) = task else { break };
                    metrics.pending.fetch_sub(1, Ordering::AcqRel);
                    let active = metrics.active.fetch_add(1, Ordering::AcqRel) + 1;
                    metrics.peak_active.fetch_max(active, Ordering::Relaxed);
                    let _active = ActiveTask(Arc::clone(&metrics));
                    task();
                })
                .expect("background task worker must start");
        }
        Self { sender, metrics }
    }

    fn try_spawn(&self, task: Task) -> bool {
        self.metrics.pending.fetch_add(1, Ordering::AcqRel);
        match self.sender.try_send(task) {
            Ok(()) => {
                let pending = self.metrics.pending.load(Ordering::Acquire);
                self.metrics
                    .peak_pending
                    .fetch_max(pending, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Full(_)) => {
                self.metrics.pending.fetch_sub(1, Ordering::AcqRel);
                eprintln!("background task queue is full; dropping stale work");
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.metrics.pending.fetch_sub(1, Ordering::AcqRel);
                false
            }
        }
    }

    #[cfg(test)]
    fn stats(&self) -> BackgroundStats {
        BackgroundStats {
            pending: self.metrics.pending.load(Ordering::Acquire),
            active: self.metrics.active.load(Ordering::Acquire),
            peak_pending: self.metrics.peak_pending.load(Ordering::Acquire),
            peak_active: self.metrics.peak_active.load(Ordering::Acquire),
        }
    }
}

fn global() -> &'static BackgroundPool {
    static POOL: OnceLock<BackgroundPool> = OnceLock::new();
    POOL.get_or_init(BackgroundPool::new)
}

#[cfg(test)]
pub(crate) fn stats() -> BackgroundStats {
    global().stats()
}

pub(crate) fn spawn(task: impl FnOnce() + Send + 'static) -> bool {
    global().try_spawn(Box::new(task))
}

pub(crate) fn spawn_cancellable(
    token: CancellationToken,
    task: impl FnOnce() + Send + 'static,
) -> bool {
    spawn(move || {
        if !token.is_cancelled() {
            task();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{CancellationToken, QUEUE_CAPACITY};

    #[test]
    #[ignore = "repeatable performance benchmark; run with --ignored --nocapture --test-threads=1"]
    fn bounded_queue_metrics_benchmark() {
        use super::{spawn, stats};
        use std::sync::{mpsc, Arc, Condvar, Mutex};

        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let ((pending_at_peak, accepted), peak_threads) =
            crate::perf_support::measure_peak_threads(|| {
                for _ in 0..super::WORKER_COUNT {
                    let gate = Arc::clone(&gate);
                    let started_tx = started_tx.clone();
                    let finished_tx = finished_tx.clone();
                    assert!(spawn(move || {
                        started_tx.send(()).unwrap();
                        let (lock, wake) = &*gate;
                        let mut released = lock.lock().unwrap();
                        while !*released {
                            released = wake.wait(released).unwrap();
                        }
                        let _ = finished_tx.send(());
                    }));
                }
                for _ in 0..super::WORKER_COUNT {
                    started_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                }
                let mut accepted = 0;
                for _ in 0..64 {
                    let finished_tx = finished_tx.clone();
                    if spawn(move || {
                        let _ = finished_tx.send(());
                    }) {
                        accepted += 1;
                    }
                }
                let pending_at_peak = stats().pending;
                {
                    let (lock, wake) = &*gate;
                    *lock.lock().unwrap() = true;
                    wake.notify_all();
                }
                for _ in 0..super::WORKER_COUNT + accepted {
                    finished_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                }
                (pending_at_peak, accepted)
            });
        let final_stats = stats();
        eprintln!(
            "[perf] background_queue accepted_while_workers_busy={accepted} pending_at_release={pending_at_peak} pending_peak={} active_peak={} workers={} process_peak_threads={} pending_tasks={}",
            final_stats.peak_pending,
            final_stats.peak_active,
            super::WORKER_COUNT,
            peak_threads,
            final_stats.pending,
        );
        assert_eq!(final_stats.pending, 0);
    }

    #[test]
    fn cancellation_token_is_shared_by_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn queue_capacity_is_fixed() {
        assert_eq!(QUEUE_CAPACITY, 128);
    }
}
