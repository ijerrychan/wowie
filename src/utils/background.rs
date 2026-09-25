//! Small bounded executor for UI and provider background work.
//!
//! A fixed worker set keeps rapid searches and icon/detail refreshes from
//! creating one operating-system thread per request. Callers still retain
//! their generation checks as the final write-back guard; a cancellation token
//! can additionally prevent queued work from starting.

use std::sync::{
    atomic::{AtomicBool, Ordering},
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
}

impl BackgroundPool {
    fn new() -> Self {
        let (sender, receiver) = sync_channel::<Task>(QUEUE_CAPACITY);
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        for _ in 0..WORKER_COUNT {
            let receiver = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name("wowie-background".into())
                .spawn(move || loop {
                    let task = receiver.lock().ok().and_then(|queue| queue.recv().ok());
                    let Some(task) = task else { break };
                    task();
                })
                .expect("background task worker must start");
        }
        Self { sender }
    }

    fn try_spawn(&self, task: Task) -> bool {
        match self.sender.try_send(task) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                eprintln!("background task queue is full; dropping stale work");
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

fn global() -> &'static BackgroundPool {
    static POOL: OnceLock<BackgroundPool> = OnceLock::new();
    POOL.get_or_init(BackgroundPool::new)
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
