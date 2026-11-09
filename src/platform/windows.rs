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
};
use url::Url;
use windows_sys::Win32::{
    Foundation::{GetLastError, POINT},
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_TRANSITIONS_FORCEDISABLED},
    Storage::FileSystem::{
        FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    },
    System::Registry::{
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, RegQueryValueExW, HKEY,
        HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ,
        REG_NOTIFY_CHANGE_LAST_SET,
    },
    System::Threading::{WaitForMultipleObjects, WaitForSingleObject, INFINITE},
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
            MOD_WIN,
        },
        WindowsAndMessaging::{
            DispatchMessageW, GetSystemMetrics, PeekMessageW, SetForegroundWindow,
            TranslateMessage, MSG, PM_REMOVE, WM_HOTKEY,
        },
    },
};

mod com;
mod file_search;
mod icons;
mod settings;
mod shell;
mod shortcuts;
mod startup;
mod tray;
mod watchers;
mod window_background;
mod window_switcher;

pub(crate) use window_switcher::{switch_window, visible_windows};

pub(super) fn set_window_background(
    window: &slint::Window,
    color: slint::Color,
    border: slint::Color,
) {
    if let Some(hwnd) = hwnd_from_window(window) {
        window_background::set_background(hwnd, color);
        // Use the same theme token as the non-native Slint outline. DWM draws
        // the entire rounded contour, including the non-client top strip.
        let border = u32::from(border.red())
            | (u32::from(border.green()) << 8)
            | (u32::from(border.blue()) << 16);
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                &border as *const _ as _,
                std::mem::size_of_val(&border) as u32,
            );
        }
    }
}

use com::{wide_path, ComScope, WinRtMtaScope, WinRtScope};
pub(crate) use file_search::query_file_index;
pub(crate) use icons::{application_icon, default_open_icon, indexed_application_icon};
pub(crate) use settings::SettingsAvailability;
pub(crate) use shell::discover_control_panel_items;
#[cfg(test)]
use shell::{
    apps_folder_application, control_panel_clsid, parse_guid, push_store_application,
    resolve_known_folder_path, store_application_with_metadata,
};
use shell::{collect_apps_folder, collect_packaged_apps};
pub(crate) use shell::{windows_build_number, windows_setting_capability_available};
pub(crate) use shortcuts::resolve_shortcut;
pub(crate) use startup::set_run_at_sign_in;
#[cfg(test)]
use watchers::DirectoryChangeWatch;
pub(crate) use watchers::{
    discover_apps_folder_applications, discover_packaged_applications,
    package_inventory_fingerprint, watch_app_provider_sources,
};

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
static APPLICATION_SEARCH_METADATA_CACHE: OnceLock<
    Mutex<HashMap<String, crate::result::SearchMetadata>>,
> = OnceLock::new();

const MAX_VERSION_RESOURCE_BYTES: u32 = 2 * 1024 * 1024;
const MAX_VERSION_METADATA_VALUES: usize = 32;
const MAX_VERSION_METADATA_CHARS: usize = 256;

pub(crate) fn application_description(target: &Path) -> Option<String> {
    let key = version_cache_key(target);
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

pub(crate) fn application_search_metadata(target: &Path) -> crate::result::SearchMetadata {
    let Some(extension) = target.extension().and_then(|value| value.to_str()) else {
        return crate::result::SearchMetadata::default();
    };
    if !extension.eq_ignore_ascii_case("exe") {
        return crate::result::SearchMetadata::default();
    }
    let key = version_cache_key(target);
    let cache = APPLICATION_SEARCH_METADATA_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(values) = cache.lock() {
        if let Some(metadata) = values.get(&key) {
            return metadata.clone();
        }
    }
    let metadata = file_version_search_metadata(target);
    if let Ok(mut values) = cache.lock() {
        if values.len() >= 512 {
            values.clear();
        }
        values.insert(key, metadata.clone());
    }
    metadata
}

fn file_description(path: &Path) -> Option<String> {
    read_version_resource(path).and_then(|data| {
        version_values_limited(&data, "FileDescription", None)
            .into_iter()
            .next()
    })
}

fn file_version_search_metadata(path: &Path) -> crate::result::SearchMetadata {
    let data = read_version_resource(path);
    let Some(data) = data else {
        return crate::result::SearchMetadata::default();
    };
    let descriptions = version_values(&data, "FileDescription");
    let product_names = version_values(&data, "ProductName");
    version_search_metadata(&descriptions, &product_names)
}

fn read_version_resource(path: &Path) -> Option<Vec<u8>> {
    let path = wide_path(path);
    let mut handle = 0;
    let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut handle) };
    if size == 0 || size > MAX_VERSION_RESOURCE_BYTES {
        return None;
    }
    let mut data = vec![0_u8; size as usize];
    if unsafe { GetFileVersionInfoW(path.as_ptr(), handle, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    Some(data)
}

fn version_values(data: &[u8], field: &str) -> Vec<String> {
    version_values_limited(data, field, Some(MAX_VERSION_METADATA_CHARS))
}

fn version_values_limited(data: &[u8], field: &str, max_chars: Option<usize>) -> Vec<String> {
    let translations = query_version_value(data, r"\VarFileInfo\Translation")
        .and_then(|(pointer, length)| {
            (length >= 4).then(|| {
                unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), length as usize / 2) }
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .take(MAX_VERSION_METADATA_VALUES)
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

    let mut values = Vec::new();
    for (language, code_page) in translations {
        let query = format!(r"\StringFileInfo\{language:04x}{code_page:04x}\{field}");
        let Some((pointer, length)) = query_version_value(data, &query) else {
            continue;
        };
        let value = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), length as usize) };
        let end = value
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(value.len());
        let value = String::from_utf16_lossy(&value[..end]);
        let value = value.trim();
        if value.is_empty()
            || max_chars.is_some_and(|maximum| value.chars().count() > maximum)
            || values
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(value))
        {
            continue;
        }
        values.push(value.to_owned());
        if values.len() >= MAX_VERSION_METADATA_VALUES {
            break;
        }
    }
    values
}

fn version_search_metadata(
    descriptions: &[String],
    product_names: &[String],
) -> crate::result::SearchMetadata {
    let mut metadata = crate::result::SearchMetadata::default();
    for description in descriptions {
        append_version_value(&mut metadata.keywords, description);
    }
    for product_name in product_names {
        if metadata.keywords.len() + metadata.context.len() >= MAX_VERSION_METADATA_VALUES {
            break;
        }
        append_version_value(&mut metadata.context, product_name);
    }
    metadata
}

fn append_version_value(values: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if values.len() < MAX_VERSION_METADATA_VALUES
        && !value.is_empty()
        && value.chars().count() <= MAX_VERSION_METADATA_CHARS
        && !values
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(value))
    {
        values.push(value.to_owned());
    }
}

fn version_cache_key(target: &Path) -> String {
    let path = target.to_string_lossy().to_lowercase();
    let Ok(metadata) = std::fs::metadata(target) else {
        return path;
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    format!("{path}|{}|{modified}", metadata.len())
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
        shutdown: &super::PlatformShutdown,
    ) -> Result<super::HotkeyRegistration> {
        let (sender, receiver) = mpsc::channel();
        let (updates, update_receiver) = mpsc::channel::<String>();
        watch_system_theme(sender.clone(), shutdown);
        let hotkey = hotkey.to_owned();
        shutdown.spawn(move |signal| {
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
            while !signal.is_stopped() {
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
                while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                    if message.message == WM_HOTKEY && message.wParam == 1 {
                        let _ = sender.send(PlatformEvent::TogglePalette);
                    }
                    unsafe {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                let stop = signal.event();
                let wait = unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::MsgWaitForMultipleObjectsEx(
                        1, &stop, 250,
                        windows_sys::Win32::UI::WindowsAndMessaging::QS_ALLINPUT,
                        windows_sys::Win32::UI::WindowsAndMessaging::MWMO_INPUTAVAILABLE,
                    )
                };
                if wait == 0 || wait == u32::MAX { break; }
            }
            if registered {
                unsafe { UnregisterHotKey(std::ptr::null_mut(), 1) };
            }
        });
        Ok(super::HotkeyRegistration {
            events: receiver,
            updates,
        })
    }

    fn start_tray(
        &self,
        labels: super::TrayMenuLabels,
        shutdown: &super::PlatformShutdown,
    ) -> super::TrayRegistration {
        tray::start(labels, shutdown)
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

    fn activate_palette(&self, window: &slint::Window) {
        let Some(hwnd) = hwnd_from_window(window) else {
            return;
        };
        unsafe {
            if SetForegroundWindow(hwnd) == 0 {
                eprintln!("could not bring launcher window to foreground");
            }
        }
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

fn watch_system_theme(
    sender: std::sync::mpsc::Sender<PlatformEvent>,
    shutdown: &super::PlatformShutdown,
) {
    shutdown.spawn(move |signal| {
        let Some(key) = open_personalize_key() else {
            return;
        };
        let changed = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
            )
        };
        if changed.is_null() {
            unsafe { RegCloseKey(key) };
            return;
        }
        let handles = [changed, signal.event()];
        while !signal.is_stopped() {
            let result =
                unsafe { RegNotifyChangeKeyValue(key, 0, REG_NOTIFY_CHANGE_LAST_SET, changed, 1) };
            if result != 0 {
                eprintln!("system theme watcher failed: Windows error {result}");
                break;
            }
            if unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) } != 0
                || signal.is_stopped()
            {
                break;
            }
            if sender.send(PlatformEvent::SystemThemeChanged).is_err() {
                break;
            }
        }
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(changed);
            RegCloseKey(key);
        };
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
    fn executable_version_strings_keep_description_and_product_in_weak_roles() {
        let metadata = version_search_metadata(
            &["Editor for documents".into(), "Document editor".into()],
            &["Acme Suite".into(), "Acme Suite".into()],
        );
        assert_eq!(
            metadata.keywords,
            vec!["Editor for documents", "Document editor"]
        );
        assert_eq!(metadata.context, vec!["Acme Suite"]);
        assert!(metadata.technical.is_empty());
    }

    #[test]
    fn system_executable_exposes_version_description_for_search() {
        let executable = PathBuf::from(std::env::var_os("WINDIR").expect("WINDIR"))
            .join("System32")
            .join("cmd.exe");
        let metadata = application_search_metadata(&executable);
        assert!(!metadata.keywords.is_empty());
    }

    #[test]
    #[ignore = "uses Windows hotkey, tray and Shell registrations"]
    fn platform_listeners_release_resources_across_restarts() {
        for _ in 0..2 {
            let shutdown = super::super::PlatformShutdown::new();
            let platform = WindowsPlatform;
            let _hotkey = platform
                .register_hotkey("Alt+Shift+F12", true, &shutdown)
                .unwrap();
            let _tray = platform.start_tray(
                super::super::TrayMenuLabels {
                    settings: "Settings".into(),
                    exit: "Exit".into(),
                },
                &shutdown,
            );
            let _sources = watch_app_provider_sources(
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                &shutdown,
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
            let started = std::time::Instant::now();
            shutdown.stop_and_join();
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
        }
    }

    #[test]
    #[ignore = "uses a temporary system-wide hotkey registration"]
    fn hotkey_update_conflict_restores_the_previous_registration() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::RegisterHotKey;

        const BLOCKER_ID: i32 = 900;
        const PROBE_ID: i32 = 901;
        const VK_F23: u32 = 0x86;
        const VK_F24: u32 = 0x87;
        let conflict_modifiers = MOD_CONTROL | MOD_ALT;
        assert_ne!(
            unsafe { RegisterHotKey(std::ptr::null_mut(), BLOCKER_ID, conflict_modifiers, VK_F24) },
            0,
            "could not reserve the temporary hotkey used to force an update conflict"
        );

        let shutdown = super::super::PlatformShutdown::new();
        let platform = WindowsPlatform;
        let hotkey = platform
            .register_hotkey("Ctrl+Alt+Shift+F23", true, &shutdown)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        hotkey.updates.send("Ctrl+Alt+F24".into()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(
                std::ptr::null_mut(),
                BLOCKER_ID,
            );
        }

        assert_eq!(
            unsafe { RegisterHotKey(std::ptr::null_mut(), PROBE_ID, conflict_modifiers, VK_F24,) },
            1,
            "the conflicting new registration should have been released"
        );
        unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(
                std::ptr::null_mut(),
                PROBE_ID,
            );
        }
        assert_eq!(
            unsafe {
                RegisterHotKey(
                    std::ptr::null_mut(),
                    PROBE_ID,
                    MOD_CONTROL | MOD_ALT | MOD_SHIFT,
                    VK_F23,
                )
            },
            0,
            "the previous registration should remain active after the failed update"
        );
        shutdown.stop_and_join();
    }

    #[test]
    fn custom_directory_notification_covers_deep_changes() {
        let root = std::env::temp_dir().join(format!(
            "wowie-custom-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let deep = root.join("one").join("two").join("three");
        std::fs::create_dir_all(&deep).unwrap();
        let watch = DirectoryChangeWatch::open(&root).unwrap();
        // Check idle and signaled behavior without a bulk scan or CPU sample.
        assert_eq!(unsafe { WaitForSingleObject(watch.handle, 0) }, 258);
        std::fs::write(deep.join("changed.txt"), b"new").unwrap();
        assert_eq!(unsafe { WaitForSingleObject(watch.handle, 5000) }, 0);
        drop(watch);
        std::fs::remove_dir_all(root).unwrap();
    }

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
