use super::*;

pub(crate) fn watch_app_provider_sources(
    custom_directories: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    shutdown: &super::super::PlatformShutdown,
) -> std::sync::mpsc::Receiver<super::super::AppProviderSource> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut watches = Vec::new();
    watches.extend(
        start_menu_watch_roots()
            .into_iter()
            .map(|path| (path, super::super::AppProviderSource::StartMenu)),
    );
    watches.extend(
        desktop_watch_roots()
            .into_iter()
            .map(|path| (path, super::super::AppProviderSource::Desktop)),
    );
    watches.extend(
        store_watch_roots()
            .into_iter()
            .map(|path| (path, super::super::AppProviderSource::AppsFolder)),
    );
    for (path, source) in watches {
        let sender = sender.clone();
        shutdown.spawn(move |signal| watch_directory(path, source, sender, signal));
    }
    watch_package_catalog(sender.clone(), shutdown);
    watch_apps_folder_shell(sender.clone(), shutdown);
    let sender = sender.clone();
    shutdown.spawn(move |signal| watch_custom_directories(custom_directories, sender, signal));
    receiver
}

fn watch_custom_directories(
    directories: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    sender: std::sync::mpsc::Sender<super::super::AppProviderSource>,
    signal: super::super::ShutdownSignal,
) {
    const RECONCILE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);
    const QUIET_PERIOD: std::time::Duration = std::time::Duration::from_millis(250);
    const MAX_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);
    const MAX_WAIT_HANDLES: usize = 64;

    let mut watched_paths = Vec::new();
    let mut watches = Vec::new();
    let mut previous_fingerprint = None;
    let mut last_reconcile = std::time::Instant::now();
    let mut pending: Option<(std::time::Instant, std::time::Instant)> = None;
    while !signal.is_stopped() {
        let Ok(mut paths) = directories.lock().map(|paths| paths.clone()) else {
            break;
        };
        paths.sort();
        paths.dedup();
        if paths != watched_paths {
            watches = paths
                .iter()
                .filter_map(|path| DirectoryChangeWatch::open(path))
                .collect();
            watched_paths = paths;
            previous_fingerprint = Some(crate::utils::filesystem_fingerprint(&watched_paths));
            last_reconcile = std::time::Instant::now();
            pending = None;
        }

        let now = std::time::Instant::now();
        if now.duration_since(last_reconcile) >= RECONCILE_INTERVAL {
            let current = crate::utils::filesystem_fingerprint(&watched_paths);
            if previous_fingerprint
                .replace(current)
                .is_some_and(|old| old != current)
            {
                if sender
                    .send(super::super::AppProviderSource::CustomDirectories)
                    .is_err()
                {
                    break;
                }
                // A previously missing root may have appeared since the last
                // check, so reopen the notification handles after reconciliation.
                watches = watched_paths
                    .iter()
                    .filter_map(|path| DirectoryChangeWatch::open(path))
                    .collect();
            }
            last_reconcile = now;
        }

        let timeout_ms = if pending.is_some() { 50 } else { 1000 };
        let mut signaled = false;
        if watches.is_empty() {
            unsafe { WaitForSingleObject(signal.event(), timeout_ms) };
        } else {
            let handles: Vec<_> = watches.iter().map(|watch| watch.handle).collect();
            let mut rebuild_watches = false;
            for chunk in handles.chunks(MAX_WAIT_HANDLES) {
                let wait = unsafe {
                    WaitForMultipleObjects(
                        chunk.len() as u32,
                        chunk.as_ptr(),
                        0,
                        if handles.len() <= MAX_WAIT_HANDLES {
                            timeout_ms
                        } else {
                            0
                        },
                    )
                };
                if wait < chunk.len() as u32 {
                    let handle = chunk[wait as usize];
                    if unsafe { FindNextChangeNotification(handle) } == 0 {
                        rebuild_watches = true;
                    }
                    signaled = true;
                } else if wait == u32::MAX {
                    rebuild_watches = true;
                }
            }
            if rebuild_watches {
                watches = watched_paths
                    .iter()
                    .filter_map(|path| DirectoryChangeWatch::open(path))
                    .collect();
            }
            if handles.len() > MAX_WAIT_HANDLES {
                unsafe {
                    WaitForSingleObject(signal.event(), if pending.is_some() { 50 } else { 250 })
                };
            }
        }
        if signal.is_stopped() {
            break;
        }
        let now = std::time::Instant::now();
        if signaled {
            pending = Some(pending.map_or((now, now), |(first, _)| (first, now)));
        }
        if pending.is_some_and(|(first, last)| {
            now.duration_since(last) >= QUIET_PERIOD || now.duration_since(first) >= MAX_DEBOUNCE
        }) {
            if sender
                .send(super::super::AppProviderSource::CustomDirectories)
                .is_err()
            {
                break;
            }
            pending = None;
        }
    }
}

pub(super) struct DirectoryChangeWatch {
    pub(super) handle: windows_sys::Win32::Foundation::HANDLE,
}

impl DirectoryChangeWatch {
    pub(super) fn open(path: &Path) -> Option<Self> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            FindFirstChangeNotificationW(
                wide.as_ptr(),
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE,
            )
        };
        if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return None;
        }
        Some(Self { handle })
    }
}

impl Drop for DirectoryChangeWatch {
    fn drop(&mut self) {
        unsafe { FindCloseChangeNotification(self.handle) };
    }
}

fn start_menu_watch_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(value) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(value).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(value) = std::env::var("ProgramData") {
        roots.push(PathBuf::from(value).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    roots
}

fn desktop_watch_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(value) = std::env::var("USERPROFILE") {
        roots.push(PathBuf::from(value).join("Desktop"));
    }
    if let Ok(value) = std::env::var("PUBLIC") {
        roots.push(PathBuf::from(value).join("Desktop"));
    }
    roots
}

fn store_watch_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(value) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(value).join("Packages"));
    }
    if let Ok(value) = std::env::var("ProgramFiles") {
        roots.push(PathBuf::from(value).join("WindowsApps"));
    }
    roots
}

fn watch_directory(
    path: PathBuf,
    source: super::super::AppProviderSource,
    sender: std::sync::mpsc::Sender<super::super::AppProviderSource>,
    signal: super::super::ShutdownSignal,
) {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        FindFirstChangeNotificationW(
            wide.as_ptr(),
            1,
            FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_LAST_WRITE,
        )
    };
    if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return;
    }
    let handles = [handle, signal.event()];
    while !signal.is_stopped() {
        let wait = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
        if wait != 0 || signal.is_stopped() {
            break;
        }
        if sender.send(source).is_err() {
            break;
        }
        if unsafe { FindNextChangeNotification(handle) } == 0 {
            break;
        }
    }
    unsafe { FindCloseChangeNotification(handle) };
}

/// Legacy store index: enumerate the shell's AppsFolder first (which provides
/// the localized display name), then use PackageManager only for entries that
/// AppsFolder did not expose.
pub(crate) fn discover_apps_folder_applications() -> Vec<super::super::StoreApplication> {
    let mut applications = Vec::new();
    collect_apps_folder(&mut applications);
    applications
}

fn watch_package_catalog(
    sender: mpsc::Sender<super::super::AppProviderSource>,
    shutdown: &super::super::PlatformShutdown,
) {
    shutdown.spawn(move |signal| {
        use windows::{
            ApplicationModel::{
                PackageCatalog, PackageInstallingEventArgs, PackageStatusChangedEventArgs,
                PackageUninstallingEventArgs, PackageUpdatingEventArgs,
            },
            Foundation::TypedEventHandler,
        };

        let Some(_winrt) = WinRtMtaScope::initialize() else {
            eprintln!("[provider] PackageCatalog listener skipped: WinRT initialization failed");
            return;
        };
        let Ok(catalog) = PackageCatalog::OpenForCurrentUser() else {
            eprintln!("[provider] PackageCatalog listener unavailable for current user");
            return;
        };
        let (change_sender, changes) = mpsc::channel();
        let installing_sender = change_sender.clone();
        let installing =
            TypedEventHandler::<PackageCatalog, PackageInstallingEventArgs>::new(move |_, _| {
                let _ = installing_sender.send(());
                Ok(())
            });
        let uninstalling_sender = change_sender.clone();
        let uninstalling =
            TypedEventHandler::<PackageCatalog, PackageUninstallingEventArgs>::new(move |_, _| {
                let _ = uninstalling_sender.send(());
                Ok(())
            });
        let updating_sender = change_sender.clone();
        let updating =
            TypedEventHandler::<PackageCatalog, PackageUpdatingEventArgs>::new(move |_, _| {
                let _ = updating_sender.send(());
                Ok(())
            });
        let status_sender = change_sender;
        let status_changed =
            TypedEventHandler::<PackageCatalog, PackageStatusChangedEventArgs>::new(move |_, _| {
                let _ = status_sender.send(());
                Ok(())
            });
        let registrations = (
            catalog.PackageInstalling(&installing),
            catalog.PackageUninstalling(&uninstalling),
            catalog.PackageUpdating(&updating),
            catalog.PackageStatusChanged(&status_changed),
        );
        if registrations.0.is_err()
            && registrations.1.is_err()
            && registrations.2.is_err()
            && registrations.3.is_err()
        {
            eprintln!("[provider] PackageCatalog change listener registration failed");
            return;
        }
        while !signal.is_stopped() {
            match changes.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(()) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let debounce = std::time::Duration::from_secs(2);
            let mut quiet_deadline = std::time::Instant::now() + debounce;
            while !signal.is_stopped() {
                let remaining = quiet_deadline.saturating_duration_since(std::time::Instant::now());
                match changes.recv_timeout(remaining.min(std::time::Duration::from_millis(250))) {
                    Ok(()) => quiet_deadline = std::time::Instant::now() + debounce,
                    Err(mpsc::RecvTimeoutError::Timeout)
                        if std::time::Instant::now() >= quiet_deadline =>
                    {
                        break
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            if signal.is_stopped() {
                break;
            }
            let _ = sender.send(super::super::AppProviderSource::AppsFolder);
        }
        if let Ok(token) = registrations.0 {
            let _ = catalog.RemovePackageInstalling(token);
        }
        if let Ok(token) = registrations.1 {
            let _ = catalog.RemovePackageUninstalling(token);
        }
        if let Ok(token) = registrations.2 {
            let _ = catalog.RemovePackageUpdating(token);
        }
        if let Ok(token) = registrations.3 {
            let _ = catalog.RemovePackageStatusChanged(token);
        }
    });
}

#[repr(C)]
struct ShellChangeNotifyEntry {
    pidl: *mut c_void,
    recursive: i32,
}

const APPS_FOLDER_CHANGE_MESSAGE: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 0x271;
static APPS_FOLDER_CHANGE_SENDER: OnceLock<
    Mutex<Option<mpsc::Sender<super::super::AppProviderSource>>>,
> = OnceLock::new();

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHChangeNotifyRegister(
        hwnd: windows_sys::Win32::Foundation::HWND,
        sources: i32,
        events: i32,
        message: u32,
        entry_count: i32,
        entries: *const ShellChangeNotifyEntry,
    ) -> u32;
    fn SHChangeNotifyDeregister(registration: u32) -> i32;
    fn SHChangeNotification_Lock(
        change: windows_sys::Win32::Foundation::HANDLE,
        process_id: u32,
        item_ids: *mut *mut *mut c_void,
        event: *mut i32,
    ) -> windows_sys::Win32::Foundation::HANDLE;
    fn SHChangeNotification_Unlock(lock: windows_sys::Win32::Foundation::HANDLE) -> i32;
}

unsafe extern "system" fn apps_folder_change_window_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    if message == APPS_FOLDER_CHANGE_MESSAGE {
        let lock = unsafe {
            SHChangeNotification_Lock(
                wparam as _,
                lparam as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if !lock.is_null() {
            unsafe { SHChangeNotification_Unlock(lock) };
        }
        if let Some(sender) = APPS_FOLDER_CHANGE_SENDER.get() {
            if let Ok(sender) = sender.lock() {
                if let Some(sender) = sender.as_ref() {
                    let _ = sender.send(super::super::AppProviderSource::AppsFolder);
                }
            }
        }
        return 0;
    }
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn watch_apps_folder_shell(
    sender: mpsc::Sender<super::super::AppProviderSource>,
    shutdown: &super::super::PlatformShutdown,
) {
    shutdown.spawn(move |signal| {
        use windows::Win32::{
            System::Com::{CoTaskMemFree, IBindCtx},
            UI::Shell::SHParseDisplayName,
        };
        use windows_sys::Win32::{
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, MsgWaitForMultipleObjectsEx,
                PeekMessageW, RegisterClassW, TranslateMessage, HWND_MESSAGE, MWMO_INPUTAVAILABLE,
                PM_REMOVE, QS_ALLINPUT, WNDCLASSW,
            },
        };

        let Some(_com) = ComScope::initialize() else {
            eprintln!("[provider] AppsFolder Shell listener skipped: COM initialization failed");
            return;
        };
        let send_slot = APPS_FOLDER_CHANGE_SENDER.get_or_init(|| Mutex::new(None));
        if let Ok(mut current) = send_slot.lock() {
            *current = Some(sender);
        }
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class_name = wide_path(Path::new("WowieAppsFolderChangeWatcher"));
        let class = WNDCLASSW {
            lpfnWndProc: Some(apps_folder_change_window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 && unsafe { GetLastError() } != 1410 {
            eprintln!("[provider] AppsFolder Shell listener window registration failed");
            if let Ok(mut current) = send_slot.lock() {
                *current = None;
            }
            return;
        }
        let window = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        if window.is_null() {
            eprintln!("[provider] AppsFolder Shell listener window creation failed");
            if let Ok(mut current) = send_slot.lock() {
                *current = None;
            }
            return;
        }
        let namespace = wide_path(Path::new("shell:AppsFolder"));
        let mut pidl = std::ptr::null_mut();
        if unsafe {
            SHParseDisplayName(
                windows::core::PCWSTR(namespace.as_ptr()),
                None::<&IBindCtx>,
                &mut pidl,
                0,
                None,
            )
        }
        .is_err()
            || pidl.is_null()
        {
            eprintln!("[provider] AppsFolder Shell listener could not resolve namespace");
            unsafe { DestroyWindow(window) };
            if let Ok(mut current) = send_slot.lock() {
                *current = None;
            }
            return;
        }
        let entry = ShellChangeNotifyEntry {
            pidl: pidl.cast(),
            recursive: 1,
        };
        // Shell change constants: create/delete/rename/mkdir/rmdir/update item
        // or directory. NewDelivery requires locking and unlocking each event.
        const SHELL_EVENTS: i32 = 0x0000_0001
            | 0x0000_0002
            | 0x0000_0004
            | 0x0000_0008
            | 0x0000_0010
            | 0x0000_1000
            | 0x0000_2000
            | 0x0002_0000
            | 0x0800_0000;
        // Recursive Shell notifications are requested on the entry itself;
        // SHCNRF_RecursiveInterrupt is only valid with InterruptLevel.
        const SHELL_SOURCE_FLAGS: i32 = 0x0002 | 0x8000;
        let registration = unsafe {
            SHChangeNotifyRegister(
                window,
                SHELL_SOURCE_FLAGS,
                SHELL_EVENTS,
                APPS_FOLDER_CHANGE_MESSAGE,
                1,
                &entry,
            )
        };
        if registration == 0 {
            unsafe { CoTaskMemFree(Some(pidl.cast())) };
            unsafe { DestroyWindow(window) };
            if let Ok(mut current) = send_slot.lock() {
                *current = None;
            }
            eprintln!("[provider] AppsFolder Shell listener registration failed");
            return;
        }
        let mut message =
            std::mem::MaybeUninit::<windows_sys::Win32::UI::WindowsAndMessaging::MSG>::zeroed();
        while !signal.is_stopped() {
            let stop = signal.event();
            let wait = unsafe {
                MsgWaitForMultipleObjectsEx(1, &stop, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
            };
            if wait != 1 || signal.is_stopped() {
                break;
            }
            while unsafe {
                PeekMessageW(message.as_mut_ptr(), std::ptr::null_mut(), 0, 0, PM_REMOVE)
            } != 0
            {
                let message = unsafe { message.assume_init_ref() };
                unsafe {
                    TranslateMessage(message);
                    DispatchMessageW(message);
                }
            }
        }
        unsafe {
            SHChangeNotifyDeregister(registration);
            CoTaskMemFree(Some(pidl.cast()));
            DestroyWindow(window);
        }
        if let Ok(mut current) = send_slot.lock() {
            *current = None;
        }
    });
}

pub(crate) fn discover_packaged_applications() -> Vec<super::super::StoreApplication> {
    let mut applications = Vec::new();
    collect_packaged_apps(&mut applications);
    applications
}

/// Read only stable package identities for startup cache reconciliation. This
/// avoids rebuilding AppListEntries and resolving descriptions when the
/// installed package inventory has not changed.
pub(crate) fn package_inventory_fingerprint() -> Option<u64> {
    use windows::Management::Deployment::{PackageManager, PackageTypes};

    let _winrt = WinRtScope::initialize()?;
    let manager = PackageManager::new().ok()?;
    let packages = manager
        .FindPackagesWithPackageTypes(PackageTypes::Main | PackageTypes::Optional)
        .ok()?;
    let mut names = packages
        .into_iter()
        .filter_map(|package| {
            package
                .Id()
                .ok()
                .and_then(|id| id.FullName().ok())
                .map(|name| name.to_string())
        })
        .collect::<Vec<_>>();
    names.sort_unstable();
    let mut fingerprint = 0xcbf29ce484222325_u64;
    for name in names {
        for byte in name.bytes().chain(std::iter::once(0)) {
            fingerprint ^= u64::from(byte);
            fingerprint = fingerprint.wrapping_mul(0x100000001b3);
        }
    }
    Some(fingerprint)
}

#[cfg(test)]
mod tests {
    use super::DirectoryChangeWatch;

    #[test]
    fn missing_directory_is_not_registered_for_notifications() {
        let missing = std::env::temp_dir().join(format!(
            "wowie-missing-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(DirectoryChangeWatch::open(&missing).is_none());
    }
}
