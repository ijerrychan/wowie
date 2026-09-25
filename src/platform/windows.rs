use super::{PlatformEvent, PlatformIntegration};
use anyhow::Result;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use std::{
    collections::HashMap,
    ffi::c_void,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{mpsc, Mutex, OnceLock},
    thread,
};
use url::Url;
use windows_sys::Win32::{
    Foundation::{GetLastError, POINT},
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED},
    Storage::FileSystem::{
        FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    },
    System::Registry::{
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, RegQueryValueExW, HKEY,
        HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET,
    },
    System::Threading::{WaitForSingleObject, INFINITE},
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
            MOD_WIN,
        },
        WindowsAndMessaging::{
            DispatchMessageW, GetForegroundWindow, GetSystemMetrics, PeekMessageW,
            SetForegroundWindow, ShowWindow, TranslateMessage, MSG, PM_REMOVE, SW_SHOW, WM_HOTKEY,
        },
    },
};

mod com;
mod icons;
mod shortcuts;
mod tray;

use com::{wide_path, ComScope, WinRtMtaScope, WinRtScope};
pub(crate) use icons::{application_icon, default_open_icon, indexed_application_icon};
pub(crate) use shortcuts::resolve_shortcut;

#[link(name = "Version")]
unsafe extern "system" {
    fn GetFileVersionInfoSizeW(file_name: *const u16, handle: *mut u32) -> u32;
    fn GetFileVersionInfoW(
        file_name: *const u16,
        handle: u32,
        length: u32,
        data: *mut c_void,
    ) -> i32;
    fn VerQueryValueW(
        block: *const c_void,
        sub_block: *const u16,
        buffer: *mut *mut c_void,
        length: *mut u32,
    ) -> i32;
}

pub struct WindowsPlatform;

static APPLICATION_DESCRIPTION_CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> =
    OnceLock::new();

pub(crate) fn application_description(target: &Path) -> Option<String> {
    let key = target.to_string_lossy().to_lowercase();
    let cache = APPLICATION_DESCRIPTION_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(values) = cache.lock() {
        if let Some(description) = values.get(&key) {
            return description.clone();
        }
    }

    let value = target.to_string_lossy();
    let value = value.trim();
    let apps_folder_prefix = ["shell:AppsFolder\\", "shell:AppsFolder/"]
        .into_iter()
        .find(|prefix| {
            value
                .get(..prefix.len())
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        });
    let description = if let Some(prefix) = apps_folder_prefix {
        let aumid = &value[prefix.len()..];
        package_description(aumid)
    } else {
        file_description(target)
    };
    if let Ok(mut values) = cache.lock() {
        values.insert(key, description.clone());
    }
    description
}

fn file_description(path: &Path) -> Option<String> {
    let path = wide_path(path);
    let mut handle = 0;
    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0_u8; size as usize];
    if unsafe { GetFileVersionInfoW(path.as_ptr(), handle, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }

    let translations = query_version_value(&data, r"\VarFileInfo\Translation")
        .and_then(|(pointer, length)| {
            (length >= 4).then(|| {
                unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), length as usize / 2) }
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| (pair[0], pair[1]))
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    let translations = if translations.is_empty() {
        vec![(0x0409, 0x04b0), (0x0409, 0x04e4)]
    } else {
        translations
    };

    translations.into_iter().find_map(|(language, code_page)| {
        let path = format!(r"\StringFileInfo\{language:04x}{code_page:04x}\FileDescription");
        let (pointer, length) = query_version_value(&data, &path)?;
        let value = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), length as usize) };
        let end = value
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(value.len());
        let value = String::from_utf16_lossy(&value[..end]);
        (!value.trim().is_empty()).then_some(value.trim().to_owned())
    })
}

fn query_version_value(data: &[u8], path: &str) -> Option<(*mut c_void, u32)> {
    let path = path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut pointer = std::ptr::null_mut();
    let mut length = 0;
    let success = unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            path.as_ptr(),
            &mut pointer,
            &mut length,
        )
    };
    (success != 0 && !pointer.is_null() && length > 0).then_some((pointer, length))
}

fn package_description(aumid: &str) -> Option<String> {
    use windows::core::HSTRING;
    use windows::ApplicationModel::AppInfo;
    use windows::Management::Deployment::{PackageManager, PackageTypes};

    let _winrt = WinRtScope::initialize()?;

    // Resolve the exact application first. This also exposes the full Package
    // object, whose manifest description is populated for packages that leave
    // AppListEntry.DisplayInfo.Description empty.
    if let Ok(app_info) = AppInfo::GetFromAppUserModelId(&HSTRING::from(aumid)) {
        let package = app_info.Package().ok();
        let package_full_name = package.as_ref().and_then(|package| {
            package
                .Id()
                .ok()
                .and_then(|id| id.FullName().ok())
                .map(|value| value.to_string())
        });
        if let Ok(display) = app_info.DisplayInfo() {
            if let Ok(description) = display.Description() {
                if let Some(description) = resolve_package_description(
                    &description.to_string(),
                    package_full_name.as_deref(),
                ) {
                    return Some(description);
                }
            }
        }
        if let Some(package) = package {
            if let Ok(description) = package.Description() {
                if let Some(description) = resolve_package_description(
                    &description.to_string(),
                    package_full_name.as_deref(),
                ) {
                    return Some(description);
                }
            }
        }
    }

    // Keep the package enumeration for older Windows builds and for entries
    // that cannot be resolved through AppInfo.
    let manager = PackageManager::new().ok()?;
    let packages = manager
        .FindPackagesWithPackageTypes(PackageTypes::Main | PackageTypes::Optional)
        .ok()?;
    for package in packages {
        let package_full_name = package
            .Id()
            .ok()
            .and_then(|id| id.FullName().ok())
            .map(|value| value.to_string());
        let Ok(entries) = package.GetAppListEntries() else {
            continue;
        };
        for entry in entries {
            let Ok(entry_aumid) = entry.AppUserModelId() else {
                continue;
            };
            if !entry_aumid.to_string().eq_ignore_ascii_case(aumid) {
                continue;
            }
            let Ok(display) = entry.DisplayInfo() else {
                return None;
            };
            let Ok(description) = display.Description() else {
                return None;
            };
            return resolve_package_description(
                &description.to_string(),
                package_full_name.as_deref(),
            );
        }
    }
    None
}

fn package_install_location(aumid: &str) -> Option<PathBuf> {
    use windows::core::HSTRING;
    use windows::ApplicationModel::AppInfo;

    let _winrt = WinRtScope::initialize()?;
    let app_info = AppInfo::GetFromAppUserModelId(&HSTRING::from(aumid)).ok()?;
    let package = app_info.Package().ok()?;
    let location = package.InstalledLocation().ok()?;
    let path = location.Path().ok()?.to_string();
    (!path.trim().is_empty()).then(|| PathBuf::from(path))
}

fn resolve_package_description(value: &str, package_full_name: Option<&str>) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let value_lowercase = value.to_ascii_lowercase();
    let is_resource_reference = value_lowercase.starts_with("ms-resource:");
    let indirect = if value.starts_with("@{") {
        Some(value.to_owned())
    } else if is_resource_reference {
        package_full_name.map(|full_name| format!("@{{{full_name}?{value}}}"))
    } else {
        return Some(value.to_owned());
    }?;

    let wide_source = indirect
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut output = vec![0_u16; 4096];
    unsafe {
        windows::Win32::UI::Shell::SHLoadIndirectString(
            windows::core::PCWSTR(wide_source.as_ptr()),
            &mut output,
            None,
        )
        .ok()?;
    }
    let end = output
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(output.len());
    let resolved = String::from_utf16_lossy(&output[..end]);
    (!resolved.trim().is_empty()
        && !resolved
            .trim()
            .to_ascii_lowercase()
            .starts_with("ms-resource:"))
    .then_some(resolved.trim().to_owned())
}

#[cfg(not(debug_assertions))]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16)
        -> *mut c_void;
}

#[cfg(not(debug_assertions))]
pub(crate) fn try_acquire_single_instance() -> Result<Option<super::SingleInstanceGuard>> {
    const ERROR_ALREADY_EXISTS: u32 = 183;
    let name = wide_path(Path::new("Global\\Wowie.SingleInstance"));
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return Err(anyhow::anyhow!(
            "Windows could not create the single-instance mutex"
        ));
    }
    if unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(super::SingleInstanceGuard {
        handle: handle as isize,
    }))
}

/// Watch each indexed source with the Win32 directory notification API.
///
/// The provider intentionally receives source-level invalidations instead of
/// raw file events. Windows can coalesce directory notifications, so a
/// complete source re-enumeration is the safe recovery path.
pub(crate) fn watch_app_provider_sources(
    custom_directories: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
) -> std::sync::mpsc::Receiver<super::AppProviderSource> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut watches = Vec::new();
    watches.extend(
        start_menu_watch_roots()
            .into_iter()
            .map(|path| (path, super::AppProviderSource::StartMenu)),
    );
    watches.extend(
        desktop_watch_roots()
            .into_iter()
            .map(|path| (path, super::AppProviderSource::Desktop)),
    );
    watches.extend(
        store_watch_roots()
            .into_iter()
            .map(|path| (path, super::AppProviderSource::PackagedApps)),
    );
    for (path, source) in watches {
        let sender = sender.clone();
        thread::spawn(move || watch_directory(path, source, sender));
    }
    watch_package_catalog(sender.clone());
    watch_apps_folder_shell(sender.clone());
    let sender = sender.clone();
    thread::spawn(move || watch_custom_directories(custom_directories, sender));
    receiver
}

fn watch_custom_directories(
    directories: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    sender: std::sync::mpsc::Sender<super::AppProviderSource>,
) {
    let mut previous = None;
    loop {
        // Release the configuration lock before walking the filesystem.
        let Ok(paths) = directories.lock().map(|paths| paths.clone()) else {
            break;
        };
        let current = crate::utils::filesystem_fingerprint(&paths);
        if previous.is_some_and(|value| value != current)
            && sender
                .send(super::AppProviderSource::CustomDirectories)
                .is_err()
        {
            break;
        }
        previous = Some(current);
        thread::sleep(std::time::Duration::from_secs(2));
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
    source: super::AppProviderSource,
    sender: std::sync::mpsc::Sender<super::AppProviderSource>,
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
    loop {
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if wait != 0 {
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
pub(crate) fn discover_apps_folder_applications() -> Vec<super::StoreApplication> {
    let mut applications = Vec::new();
    collect_apps_folder(&mut applications);
    applications
}

fn watch_package_catalog(sender: mpsc::Sender<super::AppProviderSource>) {
    thread::spawn(move || {
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
        loop {
            if changes.recv().is_err() {
                break;
            }
            let debounce = std::time::Duration::from_secs(2);
            let mut quiet_deadline = std::time::Instant::now() + debounce;
            loop {
                let remaining = quiet_deadline.saturating_duration_since(std::time::Instant::now());
                match changes.recv_timeout(remaining) {
                    Ok(()) => quiet_deadline = std::time::Instant::now() + debounce,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            let _ = sender.send(super::AppProviderSource::PackagedApps);
            let _ = sender.send(super::AppProviderSource::AppsFolder);
        }
    });
}

#[repr(C)]
struct ShellChangeNotifyEntry {
    pidl: *mut c_void,
    recursive: i32,
}

const APPS_FOLDER_CHANGE_MESSAGE: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 0x271;
static APPS_FOLDER_CHANGE_SENDER: OnceLock<mpsc::Sender<super::AppProviderSource>> =
    OnceLock::new();

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
            let _ = sender.send(super::AppProviderSource::AppsFolder);
        }
        return 0;
    }
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn watch_apps_folder_shell(sender: mpsc::Sender<super::AppProviderSource>) {
    thread::spawn(move || {
        use windows::Win32::{
            System::Com::{CoTaskMemFree, IBindCtx},
            UI::Shell::SHParseDisplayName,
        };
        use windows_sys::Win32::{
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DispatchMessageW, GetMessageW, RegisterClassW, TranslateMessage,
                HWND_MESSAGE, WNDCLASSW,
            },
        };

        let Some(_com) = ComScope::initialize() else {
            eprintln!("[provider] AppsFolder Shell listener skipped: COM initialization failed");
            return;
        };
        if APPS_FOLDER_CHANGE_SENDER.set(sender).is_err() {
            return;
        }
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class_name = wide_path(Path::new("WowieAppsFolderChangeWatcher"));
        let class = WNDCLASSW {
            lpfnWndProc: Some(apps_folder_change_window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            eprintln!("[provider] AppsFolder Shell listener window registration failed");
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
            eprintln!("[provider] AppsFolder Shell listener registration failed");
            return;
        }
        let mut message =
            std::mem::MaybeUninit::<windows_sys::Win32::UI::WindowsAndMessaging::MSG>::zeroed();
        loop {
            let received = unsafe { GetMessageW(message.as_mut_ptr(), std::ptr::null_mut(), 0, 0) };
            if received <= 0 {
                break;
            }
            let message = unsafe { message.assume_init_ref() };
            unsafe {
                TranslateMessage(message);
                DispatchMessageW(message);
            }
        }
        unsafe {
            SHChangeNotifyDeregister(registration);
            CoTaskMemFree(Some(pidl.cast()));
        }
    });
}

pub(crate) fn discover_packaged_applications() -> Vec<super::StoreApplication> {
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

/// Read the language-independent Control Panel canonical name associated with
/// a shell item's CLSID. The parsing name is intentionally not exposed as an
/// alias because virtual-folder parsing names are commonly GUIDs.
fn control_panel_canonical_name(parsing_name: &str) -> Option<String> {
    let guid = control_panel_clsid(parsing_name)?;
    let key = open_registry_key(HKEY_CLASSES_ROOT, &format!(r"CLSID\{guid}"))?;
    let name = read_registry_string(key, "System.ApplicationName");
    unsafe { RegCloseKey(key) };
    name.filter(|value| !value.trim().is_empty())
}

fn control_panel_clsid(parsing_name: &str) -> Option<&str> {
    let start = parsing_name.rfind("::{")? + 2;
    if !parsing_name[..start - 2].ends_with(r"\0\") {
        return None;
    }
    let remaining = &parsing_name[start..];
    let end = remaining.find('}')? + 1;
    let guid = &remaining[..end];
    (guid.starts_with('{') && guid.ends_with('}')).then_some(guid)
}

/// Enumerate the localized items exposed by the Control Panel shell folder.
///
/// The shell namespace is preferable to reading the `Cpls` registry values:
/// it includes modern and third-party applets and gives us the same localized
/// display names that Explorer shows to the user.
pub(crate) fn discover_control_panel_items() -> Vec<super::ControlPanelItem> {
    use windows::Win32::{
        Foundation::HWND,
        System::Com::{CoTaskMemFree, IBindCtx},
        UI::Shell::{
            IShellFolder, SHGetDesktopFolder, SHParseDisplayName, SHCONTF_FOLDERS,
            SHCONTF_NONFOLDERS, SHGDN_FORPARSING, SHGDN_NORMAL,
        },
    };

    let Some(_com) = ComScope::initialize() else {
        provider_debug("Control Panel skipped: COM initialization failed");
        return Vec::new();
    };
    let namespace = wide_path(Path::new("shell:ControlPanelFolder"));
    let mut root_pidl = std::ptr::null_mut();
    if unsafe {
        SHParseDisplayName(
            windows::core::PCWSTR(namespace.as_ptr()),
            None::<&IBindCtx>,
            &mut root_pidl,
            0,
            None,
        )
    }
    .is_err()
        || root_pidl.is_null()
    {
        provider_debug("Control Panel parse failed");
        return Vec::new();
    }

    let result = (|| {
        let desktop = unsafe { SHGetDesktopFolder().ok()? };
        let folder = unsafe {
            desktop
                .BindToObject::<_, IShellFolder>(root_pidl, None::<&IBindCtx>)
                .ok()?
        };
        let mut enumerator = None;
        let flags = (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0) as u32;
        let enumeration =
            unsafe { folder.EnumObjects(HWND(std::ptr::null_mut()), flags, &mut enumerator) };
        if enumeration.is_err() {
            provider_debug(&format!(
                "Control Panel enumeration failed: {enumeration:?}"
            ));
            return None;
        }
        let enumerator = enumerator?;
        let mut items = Vec::new();
        loop {
            let mut child = std::ptr::null_mut();
            let mut fetched = 0;
            if unsafe { enumerator.Next(std::slice::from_mut(&mut child), Some(&mut fetched)) }
                .is_err()
                || fetched == 0
                || child.is_null()
            {
                break;
            }
            let title = shell_display_name(&folder, child, SHGDN_NORMAL);
            let parsing = shell_display_name(&folder, child, SHGDN_FORPARSING);
            unsafe { CoTaskMemFree(Some(child.cast())) };
            let (Some(title), Some(parsing)) = (title, parsing) else {
                continue;
            };
            if title.trim().is_empty() || parsing.trim().is_empty() {
                continue;
            }
            let target = PathBuf::from(parsing);
            let canonical_name = control_panel_canonical_name(target.to_string_lossy().as_ref());
            provider_debug(&format!(
                "Control Panel item title={title:?} parsing={target:?} canonical={canonical_name:?}"
            ));
            let identity = format!("control-panel:{}", target.to_string_lossy().to_lowercase());
            items.push(super::ControlPanelItem {
                identity,
                title,
                aliases: canonical_name.into_iter().collect(),
                launch_target: target,
            });
        }
        items.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then_with(|| left.identity.cmp(&right.identity))
        });
        Some(items)
    })()
    .unwrap_or_default();
    unsafe { CoTaskMemFree(Some(root_pidl.cast())) };
    result
}

fn open_registry_key(hive: HKEY, subkey: &str) -> Option<HKEY> {
    let wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = std::ptr::null_mut();
    let result = unsafe { RegOpenKeyExW(hive, wide.as_ptr(), 0, KEY_READ, &mut key) };
    (result == 0).then_some(key)
}

fn read_registry_string(key: HKEY, name: &str) -> Option<String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut kind = 0u32;
    let mut size = 0u32;
    let first = unsafe {
        RegQueryValueExW(
            key,
            wide.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if first != 0 || (kind != 1 && kind != 2) || size < 2 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    let result = unsafe {
        RegQueryValueExW(
            key,
            wide.as_ptr(),
            std::ptr::null(),
            &mut kind,
            buffer.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    if result != 0 {
        return None;
    }
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    String::from_utf16(&buffer).ok()
}

fn collect_apps_folder(applications: &mut Vec<super::StoreApplication>) {
    use windows::Win32::{
        Foundation::HWND,
        System::Com::{CoTaskMemFree, IBindCtx},
        UI::Shell::{
            IShellFolder, SHGetDesktopFolder, SHParseDisplayName, SHCONTF_FOLDERS,
            SHCONTF_NONFOLDERS, SHGDN_FORPARSING, SHGDN_NORMAL,
        },
    };

    let Some(_com) = ComScope::initialize() else {
        provider_debug("AppsFolder skipped: COM initialization failed");
        return;
    };
    let namespace = wide_path(Path::new("shell:AppsFolder"));
    let mut root_pidl = std::ptr::null_mut();
    let parsed = unsafe {
        SHParseDisplayName(
            windows::core::PCWSTR(namespace.as_ptr()),
            None::<&IBindCtx>,
            &mut root_pidl,
            0,
            None,
        )
    };
    if parsed.is_err() || root_pidl.is_null() {
        provider_debug(&format!(
            "AppsFolder parse failed: result={parsed:?}, pidl_is_null={}",
            root_pidl.is_null()
        ));
        return;
    }
    let Ok(desktop) = (unsafe { SHGetDesktopFolder() }) else {
        provider_debug("AppsFolder failed: SHGetDesktopFolder");
        unsafe { CoTaskMemFree(Some(root_pidl.cast())) };
        return;
    };
    let Ok(folder) =
        (unsafe { desktop.BindToObject::<_, IShellFolder>(root_pidl, None::<&IBindCtx>) })
    else {
        provider_debug("AppsFolder failed: BindToObject");
        unsafe { CoTaskMemFree(Some(root_pidl.cast())) };
        return;
    };
    let mut enumerator = None;
    let flags = (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0) as u32;
    let enumeration =
        unsafe { folder.EnumObjects(HWND(std::ptr::null_mut()), flags, &mut enumerator) };
    if enumeration.is_err() {
        provider_debug(&format!("AppsFolder failed: EnumObjects ({enumeration:?})"));
        unsafe { CoTaskMemFree(Some(root_pidl.cast())) };
        return;
    }
    let Some(enumerator) = enumerator else {
        provider_debug("AppsFolder returned no enumerator");
        unsafe { CoTaskMemFree(Some(root_pidl.cast())) };
        return;
    };

    loop {
        let mut child = std::ptr::null_mut();
        let mut fetched = 0;
        if unsafe { enumerator.Next(std::slice::from_mut(&mut child), Some(&mut fetched)) }.is_err()
        {
            break;
        }
        if fetched == 0 || child.is_null() {
            break;
        }
        let title = shell_display_name(&folder, child, SHGDN_NORMAL);
        let parsing = shell_display_name(&folder, child, SHGDN_FORPARSING);
        // `SHGDN_FORPARSING` is a Shell namespace name, which may use a
        // known-folder GUID rather than a filesystem path. When the item is
        // filesystem-backed, obtain its actual path from the PIDL so it can
        // be compared with targets resolved from .lnk files.
        let filesystem_path = shell_filesystem_path(&folder, root_pidl, child);
        provider_debug(&format!(
            "AppsFolder title={title:?} parsing={parsing:?} filesystem_path={filesystem_path:?}"
        ));
        unsafe { CoTaskMemFree(Some(child.cast())) };
        let (Some(title), Some(parsing)) = (title, parsing) else {
            continue;
        };
        if title.trim().is_empty() {
            continue;
        }
        let mut application = apps_folder_application(&parsing, filesystem_path, title);
        if application.app_user_model_id.is_some() || application.executable.is_some() {
            application.description = application_description(&application.launch_target);
        }
        push_store_application(applications, application);
    }
    unsafe { CoTaskMemFree(Some(root_pidl.cast())) };
}

fn provider_debug(message: &str) {
    if std::env::var_os("WOWIE_PROVIDER_DEBUG").is_some() && message.starts_with("Control Panel") {
        eprintln!("[provider] {message}");
    }
}

fn shell_filesystem_path(
    folder: &windows::Win32::UI::Shell::IShellFolder,
    root_pidl: *const windows::Win32::UI::Shell::Common::ITEMIDLIST,
    child: *const windows::Win32::UI::Shell::Common::ITEMIDLIST,
) -> Option<PathBuf> {
    use windows::Win32::{
        System::{Com::CoTaskMemFree, SystemServices::SFGAO_FILESYSTEM},
        UI::Shell::{IShellItem, SHCreateItemWithParent, SIGDN_FILESYSPATH},
    };

    let mut attributes = SFGAO_FILESYSTEM.0;
    unsafe { folder.GetAttributesOf(&[child], &mut attributes).ok()? };
    if attributes & SFGAO_FILESYSTEM.0 == 0 {
        return None;
    }
    let item: IShellItem = unsafe { SHCreateItemWithParent(Some(root_pidl), folder, child).ok()? };
    let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH).ok()? };
    let value = unsafe { name.to_string().ok() };
    unsafe { CoTaskMemFree(Some(name.0.cast())) };
    value.map(PathBuf::from)
}

fn shell_display_name(
    folder: &windows::Win32::UI::Shell::IShellFolder,
    child: *const windows::Win32::UI::Shell::Common::ITEMIDLIST,
    flags: windows::Win32::UI::Shell::SHGDNF,
) -> Option<String> {
    use windows::Win32::UI::Shell::StrRetToBufW;

    let mut display = windows::Win32::UI::Shell::Common::STRRET::default();
    unsafe { folder.GetDisplayNameOf(child, flags, &mut display).ok()? };
    let mut buffer = [0_u16; 1024];
    unsafe { StrRetToBufW(&mut display, Some(child), &mut buffer).ok()? };
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

fn collect_packaged_apps(applications: &mut Vec<super::StoreApplication>) {
    use windows::Management::Deployment::{PackageManager, PackageTypes};

    let Some(_winrt) = WinRtScope::initialize() else {
        return;
    };
    let Ok(manager) = PackageManager::new() else {
        return;
    };
    let Ok(packages) =
        manager.FindPackagesWithPackageTypes(PackageTypes::Main | PackageTypes::Optional)
    else {
        return;
    };
    for package in packages {
        let Ok(entries) = package.GetAppListEntries() else {
            continue;
        };
        let package_id = package.Id().ok();
        let package_metadata = Some(super::PackageMetadata {
            full_name: package_id
                .as_ref()
                .and_then(|id| id.FullName().ok())
                .map(|value| value.to_string()),
            install_location: None,
            identity_name: package_id
                .as_ref()
                .and_then(|id| id.Name().ok())
                .map(|value| value.to_string()),
        });
        for entry in entries {
            let (Ok(display), Ok(aumid)) = (entry.DisplayInfo(), entry.AppUserModelId()) else {
                continue;
            };
            let Ok(title) = display.DisplayName() else {
                continue;
            };
            let title = title.to_string();
            let aumid = aumid.to_string();
            if title.trim().is_empty() || aumid.is_empty() {
                continue;
            }
            let launch_target = PathBuf::from(format!("shell:AppsFolder\\{aumid}"));
            let description = application_description(&launch_target);
            let mut metadata = package_metadata.clone();
            if let Some(metadata) = metadata.as_mut() {
                metadata.install_location = package_install_location(&aumid);
            }
            push_store_application(
                applications,
                store_application_with_metadata(
                    &aumid,
                    title,
                    super::source_ids::PACKAGE,
                    metadata,
                    description,
                ),
            );
        }
    }
}

/// Preserve all of the legacy AppsFolder parsing-name cases. AppsFolder can
/// return an AUMID, a real executable path, or another virtual-shell name.
fn apps_folder_application(
    parsing: &str,
    filesystem_path: Option<PathBuf>,
    title: String,
) -> super::StoreApplication {
    let value = parsing
        .strip_prefix("shell:AppsFolder\\")
        .or_else(|| parsing.strip_prefix("shell:AppsFolder/"))
        .unwrap_or(parsing);
    if value.contains('!') && !value.contains(':') {
        return store_application(value, title, "AppsFolder");
    }

    // AppsFolder can expose Internet entries as virtual parsing names. Treat
    // these like `.url` entries so identity and launch target can converge.
    if let Some(url) = normalize_apps_folder_url(value) {
        return super::StoreApplication {
            identity: format!("url:{url}"),
            title,
            aliases: Vec::new(),
            launch_target: PathBuf::from(&url),
            executable: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            source: super::source_ids::APPS_FOLDER,
        };
    }

    // The parsing name of an AppsFolder item can contain a known-folder GUID
    // (for example {6D809377-...}\Tencent\Weixin\Weixin.exe). It is a Shell
    // namespace name, not a filesystem path. Resolve a known-folder GUID
    // through the Windows Known Folder API before comparing the path.
    let path = filesystem_path
        .or_else(|| resolve_known_folder_path(value))
        .unwrap_or_else(|| PathBuf::from(value));
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return super::StoreApplication {
            identity: format!("exe:{}", path.to_string_lossy().trim().to_lowercase()),
            title,
            aliases: Vec::new(),
            // A filesystem-backed AppsFolder item must be launched through
            // its resolved Win32 path. The parsing name (`{GUID}\\...`) is
            // only a Shell namespace identifier and is not launchable.
            launch_target: path.clone(),
            executable: Some(path),
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            source: super::source_ids::APPS_FOLDER,
        };
    }

    super::StoreApplication {
        identity: format!("shell:{}", value.to_lowercase()),
        title,
        aliases: Vec::new(),
        launch_target: PathBuf::from(format!("shell:AppsFolder\\{value}")),
        executable: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        source: super::source_ids::APPS_FOLDER,
    }
}

fn normalize_apps_folder_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

fn resolve_known_folder_path(value: &str) -> Option<PathBuf> {
    let value = value.strip_prefix('{')?;
    let closing_brace = value.find('}')?;
    let guid_text = &value[..closing_brace];
    let guid = parse_guid(guid_text)?;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            rfid: *const windows::core::GUID,
            flags: u32,
            token: isize,
            path: *mut *mut u16,
        ) -> i32;
    }

    let mut path = std::ptr::null_mut();
    let result = unsafe { SHGetKnownFolderPath(&guid, 0, 0, &mut path) };
    if result < 0 || path.is_null() {
        return None;
    }
    let length = unsafe { (0..).take_while(|offset| *path.add(*offset) != 0).count() };
    let base = PathBuf::from(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(path, length)
    }));
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(path.cast())) };

    let suffix = value[closing_brace + 1..].trim_matches(['\\', '/']);
    let resolved = if suffix.is_empty() {
        base
    } else {
        base.join(suffix)
    };
    Some(resolved)
}

fn parse_guid(value: &str) -> Option<windows::core::GUID> {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 5
        || parts[0].len() != 8
        || parts[1].len() != 4
        || parts[2].len() != 4
        || parts[3].len() != 4
        || parts[4].len() != 12
    {
        return None;
    }
    let data1 = u32::from_str_radix(parts[0], 16).ok()?;
    let data2 = u16::from_str_radix(parts[1], 16).ok()?;
    let data3 = u16::from_str_radix(parts[2], 16).ok()?;
    let mut data4 = [0_u8; 8];
    for (index, byte) in data4[..2].iter_mut().enumerate() {
        *byte = u8::from_str_radix(&parts[3][index * 2..index * 2 + 2], 16).ok()?;
    }
    for (index, byte) in data4[2..].iter_mut().enumerate() {
        *byte = u8::from_str_radix(&parts[4][index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(windows::core::GUID::from_values(data1, data2, data3, data4))
}

fn store_application(aumid: &str, title: String, source: &'static str) -> super::StoreApplication {
    store_application_with_metadata(aumid, title, source, None, None)
}

fn store_application_with_metadata(
    aumid: &str,
    title: String,
    source: &'static str,
    package_metadata: Option<super::PackageMetadata>,
    description: Option<String>,
) -> super::StoreApplication {
    let mut aliases = Vec::new();
    if let Some(product) = aumid
        .split('!')
        .next()
        .and_then(|value| value.split('_').next())
    {
        let product = product.replace(['.', '_'], " ");
        if !product.is_empty() && !product.eq_ignore_ascii_case(&title) {
            aliases.push(product);
        }
    }
    let launch_target = PathBuf::from(format!("shell:AppsFolder\\{aumid}"));
    let app_user_model_id = Some(aumid.to_owned());
    let package_family_name = aumid
        .split('!')
        .next()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    super::StoreApplication {
        identity: format!("aumid:{}", aumid.to_lowercase()),
        title,
        aliases,
        launch_target,
        executable: None,
        description,
        app_user_model_id,
        package_family_name,
        package_metadata,
        source,
    }
}

fn push_store_application(
    applications: &mut Vec<super::StoreApplication>,
    application: super::StoreApplication,
) {
    // Display names are not identities: different packages can expose the
    // same localized title. Only suppress an entry when its stable identity
    // is already present; the later provider merge can still combine the
    // same application across AppsFolder and Package sources.
    if let Some(existing) = applications.iter_mut().find(|existing| {
        existing
            .identity
            .eq_ignore_ascii_case(&application.identity)
    }) {
        if existing.description.is_none() {
            existing.description = application.description;
        }
        if existing.app_user_model_id.is_none() {
            existing.app_user_model_id = application.app_user_model_id;
        }
        if existing.package_family_name.is_none() {
            existing.package_family_name = application.package_family_name;
        }
        if existing.package_metadata.is_none() {
            existing.package_metadata = application.package_metadata;
        }
        existing.aliases.extend(application.aliases);
        return;
    }
    applications.push(application);
}

fn register_hotkey_value(hotkey: &str) -> bool {
    let (modifiers, key) = match parse_hotkey(hotkey) {
        Some(parsed) => parsed,
        None => {
            eprintln!(
                "unsupported global hotkey '{hotkey}'; a modifier is required, using Alt+Space"
            );
            (MOD_ALT, 0x20)
        }
    };
    register_hotkey_parsed(hotkey, modifiers, key)
}

fn register_hotkey_parsed(hotkey: &str, modifiers: u32, key: u32) -> bool {
    if unsafe { RegisterHotKey(std::ptr::null_mut(), 1, modifiers | MOD_NOREPEAT, key) } != 0 {
        return true;
    }
    let error = unsafe { GetLastError() };
    if error == 1409 {
        eprintln!("could not register global hotkey {hotkey}: already registered by another application (Windows error 1409)");
    } else {
        eprintln!("could not register global hotkey {hotkey}: Windows error {error}");
    }
    false
}

impl PlatformIntegration for WindowsPlatform {
    fn system_prefers_dark_theme(&self) -> bool {
        read_system_prefers_dark_theme()
    }

    fn system_accent_color(&self) -> u32 {
        read_system_accent_color()
    }

    fn register_hotkey(
        &self,
        hotkey: &str,
        _watch_theme: bool,
    ) -> Result<super::HotkeyRegistration> {
        let (sender, receiver) = mpsc::channel();
        let (updates, update_receiver) = mpsc::channel::<String>();
        watch_system_theme(sender.clone());
        let hotkey = hotkey.to_owned();
        thread::spawn(move || {
            let mut active_hotkey = hotkey.clone();
            let mut registered = register_hotkey_value(&active_hotkey);
            let mut message = MSG {
                hwnd: std::ptr::null_mut(),
                message: 0,
                wParam: 0,
                lParam: 0,
                time: 0,
                pt: POINT { x: 0, y: 0 },
            };
            loop {
                while let Ok(updated) = update_receiver.try_recv() {
                    let Some((modifiers, key)) = parse_hotkey(&updated) else {
                        eprintln!(
                            "could not update global hotkey to '{updated}': a modifier is required; keeping '{active_hotkey}'"
                        );
                        continue;
                    };
                    if !registered {
                        registered = register_hotkey_parsed(&updated, modifiers, key);
                        if registered {
                            active_hotkey = updated;
                        }
                        continue;
                    }
                    unsafe { UnregisterHotKey(std::ptr::null_mut(), 1) };
                    if register_hotkey_parsed(&updated, modifiers, key) {
                        active_hotkey = updated;
                        registered = true;
                    } else {
                        registered = register_hotkey_value(&active_hotkey);
                        if !registered {
                            eprintln!(
                                "could not restore previous global hotkey '{}'; hotkey registration is currently inactive",
                                active_hotkey
                            );
                        }
                    }
                }
                if unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0
                {
                    if message.message == WM_HOTKEY && message.wParam == 1 {
                        let _ = sender.send(PlatformEvent::TogglePalette);
                    }
                    unsafe {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                } else {
                    thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        });
        Ok(super::HotkeyRegistration {
            events: receiver,
            updates,
        })
    }

    fn start_tray(&self, labels: super::TrayMenuLabels) -> super::TrayRegistration {
        tray::start(labels)
    }

    fn position_palette(&self, window: &slint::Window, result_height: f32) {
        let size = window.size();
        let scale = window.scale_factor();
        let window_width = size.width as i32;
        let window_height = (result_height * scale).round() as i32;
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        };
        let hwnd = hwnd_from_window(window).unwrap_or(std::ptr::null_mut());
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: Default::default(),
            rcWork: Default::default(),
            dwFlags: 0,
        };
        let (left, top, right, bottom) =
            if !monitor.is_null() && unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
                (
                    info.rcWork.left,
                    info.rcWork.top,
                    info.rcWork.right,
                    info.rcWork.bottom,
                )
            } else {
                (0, 0, unsafe { GetSystemMetrics(0) }, unsafe {
                    GetSystemMetrics(1)
                })
            };
        window.set_position(slint::PhysicalPosition::new(
            left + ((right - left - window_width).max(0)) / 2,
            top + ((bottom - top - window_height).max(0)) / 2,
        ));
    }

    fn position_context_menu(
        &self,
        window: &slint::Window,
        launcher: &slint::Window,
        anchor: Option<(f32, f32)>,
    ) {
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};
        let Some(menu_hwnd) = hwnd_from_window(window) else {
            return;
        };
        let mut menu_rect = windows_sys::Win32::Foundation::RECT::default();
        if unsafe { GetWindowRect(menu_hwnd, &mut menu_rect) } == 0 {
            return;
        }
        let width = menu_rect.right - menu_rect.left;
        let height = menu_rect.bottom - menu_rect.top;

        if let Some((anchor_x, anchor_y)) = anchor {
            let launcher_position = launcher.position();
            let scale = launcher.scale_factor();
            let (anchor_x, anchor_y) = super::logical_point_to_physical(
                (launcher_position.x, launcher_position.y),
                (anchor_x, anchor_y),
                scale,
            );
            window.set_position(slint::PhysicalPosition::new(
                anchor_x - width,
                anchor_y - height,
            ));
            return;
        }

        let mut cursor = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut cursor) } == 0 {
            return;
        }
        let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: Default::default(),
            rcWork: Default::default(),
            dwFlags: 0,
        };
        let (left, top, right, bottom) =
            if !monitor.is_null() && unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
                (
                    info.rcWork.left,
                    info.rcWork.top,
                    info.rcWork.right,
                    info.rcWork.bottom,
                )
            } else {
                (0, 0, unsafe { GetSystemMetrics(0) }, unsafe {
                    GetSystemMetrics(1)
                })
            };
        let x = (cursor.x + 8).min(right - width).max(left);
        let y = (cursor.y + 8).min(bottom - height).max(top);
        window.set_position(slint::PhysicalPosition::new(x, y));
    }

    fn activate_palette(&self, window: &slint::Window) {
        let Some(hwnd) = hwnd_from_window(window) else {
            return;
        };
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            if SetForegroundWindow(hwnd) == 0 {
                eprintln!("could not bring launcher window to foreground");
            }
        }
    }

    fn activate_context_menu(&self, window: &slint::Window) {
        self.activate_palette(window);
    }

    fn is_window_focused(&self, window: &slint::Window) -> bool {
        hwnd_from_window(window).is_some_and(|hwnd| unsafe { GetForegroundWindow() == hwnd })
    }

    fn install_focus_listener(&self, window: &slint::Window, on_lost_focus: Box<dyn Fn()>) {
        window.on_winit_window_event(move |_window, event| {
            if matches!(event, winit::event::WindowEvent::Focused(false)) {
                on_lost_focus();
            }
            EventResult::Propagate
        });
    }

    fn disable_window_transitions(&self, window: &slint::Window) {
        let Some(hwnd) = hwnd_from_window(window) else {
            return;
        };

        let disabled: i32 = 1;
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED as u32,
                &disabled as *const i32 as *const _,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
}

fn open_personalize_key() -> Option<HKEY> {
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut key = std::ptr::null_mut();
    let opened =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) } == 0;
    opened.then_some(key)
}

fn read_system_prefers_dark_theme() -> bool {
    let Some(key) = open_personalize_key() else {
        return false;
    };
    let value: Vec<u16> = "AppsUseLightTheme"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut light_theme: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let result = unsafe {
        RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut light_theme as *mut u32 as *mut u8,
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };
    result == 0 && light_theme == 0
}

fn read_system_accent_color() -> u32 {
    let Some(key) = open_accent_key() else {
        return 0x0078d4;
    };
    let value: Vec<u16> = "AccentColorMenu"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut accent: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let result = unsafe {
        RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut accent as *mut u32 as *mut u8,
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };
    if result != 0 {
        return 0x0078d4;
    }
    // Windows stores this DWORD as AABBGGRR; Slint expects 0xRRGGBB.
    ((accent & 0x000000ff) << 16) | (accent & 0x0000ff00) | ((accent >> 16) & 0x000000ff)
}

fn open_accent_key() -> Option<HKEY> {
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut key = std::ptr::null_mut();
    let opened =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) } == 0;
    opened.then_some(key)
}

fn watch_system_theme(sender: std::sync::mpsc::Sender<PlatformEvent>) {
    thread::spawn(move || {
        let Some(key) = open_personalize_key() else {
            return;
        };
        loop {
            let result = unsafe {
                RegNotifyChangeKeyValue(key, 0, REG_NOTIFY_CHANGE_LAST_SET, std::ptr::null_mut(), 0)
            };
            if result != 0 {
                eprintln!("system theme watcher failed: Windows error {result}");
                break;
            }
            if sender.send(PlatformEvent::SystemThemeChanged).is_err() {
                break;
            }
        }
        unsafe { RegCloseKey(key) };
    });
}

fn hwnd_from_window(window: &slint::Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    let handle = window.window_handle();
    let Ok(handle) = handle.window_handle() else {
        return None;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.hwnd.get() as _)
}

pub(super) fn pick_folder(owner: &slint::Window) -> Result<Option<PathBuf>> {
    use anyhow::{anyhow, Context};
    use windows::Win32::{
        Foundation::HWND,
        System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER},
        UI::Shell::{
            FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
            FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
        },
    };

    let _com = ComScope::initialize().ok_or_else(|| anyhow!("initialize folder picker COM"))?;
    let dialog: IFileOpenDialog = unsafe {
        CoCreateInstance(
            &FileOpenDialog,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
    }
    .context("create folder picker")?;
    unsafe {
        dialog.SetOptions(
            dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
        )?;
    }
    let hwnd = hwnd_from_window(owner).map(HWND);
    if let Err(error) = unsafe { dialog.Show(hwnd) } {
        // HRESULT_FROM_WIN32(ERROR_CANCELLED) is the normal result of Cancel.
        if error.code().0 as u32 == 0x8007_04c7 {
            return Ok(None);
        }
        return Err(error).context("show folder picker");
    }
    let item = unsafe { dialog.GetResult() }.context("read selected folder")?;
    let display_name =
        unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.context("read selected folder path")?;
    let path = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
        display_name.as_wide()
    }));
    unsafe { CoTaskMemFree(Some(display_name.0.cast())) };
    Ok(Some(path))
}

fn parse_hotkey(value: &str) -> Option<(u32, u32)> {
    let mut modifiers = 0;
    let mut key = None;
    for part in value
        .split('+')
        .map(|part| part.trim().to_ascii_uppercase())
    {
        let modifier = match part.as_str() {
            "ALT" => Some(MOD_ALT),
            "CTRL" | "CONTROL" => Some(MOD_CONTROL),
            "SHIFT" => Some(MOD_SHIFT),
            "WIN" | "META" => Some(MOD_WIN),
            _ => None,
        };
        if let Some(modifier) = modifier {
            modifiers |= modifier;
            continue;
        }

        let virtual_key = match part.as_str() {
            "SPACE" => Some(0x20),
            "ENTER" | "RETURN" => Some(0x0D),
            "TAB" => Some(0x09),
            "BACKSPACE" => Some(0x08),
            "ESC" | "ESCAPE" => Some(0x1B),
            "INSERT" => Some(0x2D),
            "DELETE" => Some(0x2E),
            "HOME" => Some(0x24),
            "END" => Some(0x23),
            "PAGEUP" => Some(0x21),
            "PAGEDOWN" => Some(0x22),
            "UP" => Some(0x26),
            "DOWN" => Some(0x28),
            "LEFT" => Some(0x25),
            "RIGHT" => Some(0x27),
            "-" | "_" => Some(0xBD),
            "=" | "+" | "PLUS" => Some(0xBB),
            "," | "<" => Some(0xBC),
            "." | ">" => Some(0xBE),
            "/" | "?" => Some(0xBF),
            ";" | ":" => Some(0xBA),
            "'" | "\"" => Some(0xDE),
            "[" | "{" => Some(0xDB),
            "]" | "}" => Some(0xDD),
            "\\" | "|" => Some(0xDC),
            "`" | "~" => Some(0xC0),
            "!" => Some(b'1' as u32),
            "@" => Some(b'2' as u32),
            "#" => Some(b'3' as u32),
            "$" => Some(b'4' as u32),
            "%" => Some(b'5' as u32),
            "^" => Some(b'6' as u32),
            "&" => Some(b'7' as u32),
            "*" => Some(b'8' as u32),
            "(" => Some(b'9' as u32),
            ")" => Some(b'0' as u32),
            function if function.starts_with('F') => function[1..]
                .parse::<u32>()
                .ok()
                .filter(|number| (1..=24).contains(number))
                .map(|number| 0x70 + number - 1),
            character
                if character.len() == 1 && (character.as_bytes()[0].is_ascii_alphanumeric()) =>
            {
                Some(character.as_bytes()[0].to_ascii_uppercase() as u32)
            }
            _ => None,
        }
        .filter(|_| key.is_none())?;
        key = Some(virtual_key);
    }
    (modifiers != 0).then(|| Some((modifiers, key?)))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_folder_aumid_uses_localized_shell_entry() {
        let application = apps_folder_application(
            "shell:AppsFolder\\Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
            None,
            "记事本".into(),
        );
        assert_eq!(
            application.identity,
            "aumid:microsoft.windowsnotepad_8wekyb3d8bbwe!app"
        );
        assert_eq!(application.title, "记事本");
        assert_eq!(
            application.app_user_model_id.as_deref(),
            Some("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App")
        );
        assert_eq!(
            application.package_family_name.as_deref(),
            Some("Microsoft.WindowsNotepad_8wekyb3d8bbwe")
        );
        assert_eq!(
            application.launch_target,
            PathBuf::from("shell:AppsFolder\\Microsoft.WindowsNotepad_8wekyb3d8bbwe!App")
        );
    }

    #[test]
    fn apps_folder_keeps_executable_parsing_names() {
        let application =
            apps_folder_application(r"C:\Windows\System32\notepad.exe", None, "Notepad".into());
        assert_eq!(
            application.identity,
            "exe:c:\\windows\\system32\\notepad.exe"
        );
        assert_eq!(
            application.launch_target,
            PathBuf::from(r"C:\Windows\System32\notepad.exe")
        );
    }

    #[test]
    fn apps_folder_uses_filesystem_path_for_shell_parsing_name() {
        let filesystem_path = PathBuf::from(r"C:\Program Files\Tencent\Weixin\Weixin.exe");
        let application = apps_folder_application(
            r"{6D809377-6AF0-444B-8957-A3773F02200E}\Tencent\Weixin\Weixin.exe",
            Some(filesystem_path.clone()),
            "Weixin".into(),
        );
        assert_eq!(application.executable, Some(filesystem_path.clone()));
        assert_eq!(application.launch_target, filesystem_path);
        assert_eq!(
            application.identity,
            "exe:c:\\program files\\tencent\\weixin\\weixin.exe"
        );
    }

    #[test]
    fn apps_folder_url_uses_url_identity_and_target() {
        let application = apps_folder_application(
            "http://support.steampowered.com/",
            None,
            "Steam Support Center".into(),
        );
        assert_eq!(application.identity, "url:http://support.steampowered.com/");
        assert_eq!(
            application.launch_target,
            PathBuf::from("http://support.steampowered.com/")
        );
    }

    #[test]
    fn store_index_keeps_same_title_with_distinct_identities() {
        let mut applications = Vec::new();
        push_store_application(
            &mut applications,
            apps_folder_application(
                r"shell:AppsFolder\Contoso.Editor_abc!App",
                None,
                "Editor".into(),
            ),
        );
        push_store_application(
            &mut applications,
            apps_folder_application(
                r"shell:AppsFolder\Fabrikam.Editor_def!App",
                None,
                "Editor".into(),
            ),
        );

        assert_eq!(applications.len(), 2);
        assert_ne!(applications[0].identity, applications[1].identity);
    }

    #[test]
    fn store_index_deduplicates_same_identity_case_insensitively() {
        let mut applications = Vec::new();
        push_store_application(
            &mut applications,
            apps_folder_application(
                r"shell:AppsFolder\Contoso.Editor_abc!App",
                None,
                "Editor".into(),
            ),
        );
        push_store_application(
            &mut applications,
            apps_folder_application(
                r"shell:AppsFolder\contoso.editor_ABC!APP",
                None,
                "Editor (Package)".into(),
            ),
        );

        assert_eq!(applications.len(), 1);
        assert_eq!(applications[0].title, "Editor");
    }

    #[test]
    fn package_metadata_is_kept_on_packaged_application_records() {
        let application = store_application_with_metadata(
            "Contoso.Editor_abc!Main",
            "Editor".into(),
            crate::platform::source_ids::PACKAGE,
            Some(crate::platform::PackageMetadata {
                full_name: Some("Contoso.Editor_1.0.0.0_neutral__abc".into()),
                install_location: None,
                identity_name: Some("Contoso.Editor".into()),
            }),
            None,
        );

        assert_eq!(
            application
                .package_metadata
                .as_ref()
                .and_then(|metadata| metadata.full_name.as_deref()),
            Some("Contoso.Editor_1.0.0.0_neutral__abc")
        );
        assert_eq!(
            application
                .package_metadata
                .as_ref()
                .and_then(|metadata| metadata.identity_name.as_deref()),
            Some("Contoso.Editor")
        );
    }

    #[test]
    fn package_description_resolves_only_usable_values() {
        assert_eq!(
            resolve_package_description("  A useful editor  ", None),
            Some("A useful editor".into())
        );
        assert_eq!(
            resolve_package_description("ms-resource://Contoso.Editor/Description", None),
            None
        );
        assert_eq!(resolve_package_description("", None), None);
    }

    #[test]
    fn package_description_resource_references_require_package_context() {
        assert_eq!(
            resolve_package_description("MS-RESOURCE://Contoso.Editor/Description", None),
            None
        );
        assert_eq!(
            resolve_package_description("@{Contoso.Editor_1.0.0.0_neutral__abc?Description}", None),
            None
        );
    }

    #[test]
    fn package_description_resource_reference_with_package_context_is_safe() {
        // The synthetic package does not exist on the test machine, so the
        // expected result is the documented empty fallback. This still covers
        // the ms-resource -> indirect-string construction path without
        // depending on a particular installed Store package or UI language.
        assert_eq!(
            resolve_package_description(
                "ms-resource://Contoso.Editor/Resources/Description",
                Some("Contoso.Editor_1.0.0.0_neutral__abc")
            ),
            None
        );
    }

    #[test]
    fn parses_known_folder_guid() {
        assert!(parse_guid("6D809377-6AF0-444B-8957-A3773F02200E").is_some());
    }

    #[test]
    fn control_panel_clsid_uses_the_leaf_guid() {
        assert_eq!(
            control_panel_clsid(
                r"::{26EE0668-A00A-44D7-9371-BEB064C98683}\0\::{025A5937-A6BE-4686-A844-36FE4BEC8B6D}"
            ),
            Some("{025A5937-A6BE-4686-A844-36FE4BEC8B6D}")
        );
        assert_eq!(
            control_panel_clsid(r"::{26EE0668-A00A-44D7-9371-BEB064C98683}\0\字体"),
            None
        );
    }

    #[test]
    fn resolves_apps_folder_known_folder_parsing_name() {
        let path =
            resolve_known_folder_path(r"{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\Steam\Steam.exe")
                .expect("FOLDERID_ProgramFilesX86 should resolve");
        assert!(path.ends_with(r"Steam\Steam.exe"));
    }
}
