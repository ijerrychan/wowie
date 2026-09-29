use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

pub(crate) fn percentiles(samples: &mut [Duration]) -> (Duration, Duration) {
    samples.sort_unstable();
    let index = |percent: usize| {
        samples
            .len()
            .saturating_mul(percent)
            .div_ceil(100)
            .saturating_sub(1)
    };
    (
        samples.get(index(50)).copied().unwrap_or_default(),
        samples.get(index(95)).copied().unwrap_or_default(),
    )
}

pub(crate) fn measure_peak_threads<T>(measure: impl FnOnce() -> T) -> (T, usize) {
    let running = Arc::new(AtomicBool::new(true));
    let peak = Arc::new(AtomicUsize::new(process_thread_count()));
    let sampler_running = Arc::clone(&running);
    let sampler_peak = Arc::clone(&peak);
    let sampler = std::thread::spawn(move || {
        while sampler_running.load(Ordering::Acquire) {
            sampler_peak.fetch_max(process_thread_count(), Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
        }
        sampler_peak.fetch_max(process_thread_count(), Ordering::Relaxed);
    });
    let result = measure();
    running.store(false, Ordering::Release);
    let _ = sampler.join();
    (result, peak.load(Ordering::Acquire))
}

#[cfg(windows)]
fn process_thread_count() -> usize {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            Threading::GetCurrentProcessId,
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return 0;
    }
    let process_id = unsafe { GetCurrentProcessId() };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let mut count = 0;
    let mut available = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while available {
        if entry.th32OwnerProcessID == process_id {
            count += 1;
        }
        available = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    count
}

#[cfg(not(windows))]
fn process_thread_count() -> usize {
    std::fs::read_dir("/proc/self/task")
        .map(|threads| threads.count())
        .unwrap_or_else(|_| std::thread::available_parallelism().map_or(1, usize::from))
}
