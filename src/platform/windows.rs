use super::{PlatformEvent, PlatformIntegration};
use anyhow::Result;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use std::{
    ffi::OsStr,
    fs,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
};
use url::Url;
use windows_sys::Win32::{
    Foundation::{GetLastError, POINT},
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED},
    System::Registry::{
        RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, RegQueryValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET,
    },
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
        },
        WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, GetSystemMetrics, SetForegroundWindow, ShowWindow,
            TranslateMessage, MSG, SW_SHOW, WM_HOTKEY,
        },
    },
};

pub struct WindowsPlatform;

/// Legacy store index: enumerate the shell's AppsFolder first (which provides
/// the localized display name), then use PackageManager only for entries that
/// AppsFolder did not expose.
pub(crate) fn discover_store_applications() -> Vec<super::StoreApplication> {
    let mut applications = Vec::new();
    collect_apps_folder(&mut applications);
    collect_packaged_apps(&mut applications);
    applications
}

/// Native icon extraction entry point. The pixel conversion is intentionally
/// kept behind the platform boundary so callers can cache the result without
/// depending on HICON/HBITMAP types.
pub(crate) fn application_icon(_target: &Path) -> Option<super::IconData> {
    use std::{mem, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::{
        Graphics::Gdi::{GetDC, GetDIBits, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS},
        UI::{
            Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON},
            WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO},
        },
    };
    let wide: Vec<u16> = _target.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file_info: SHFILEINFOW = unsafe { mem::zeroed() };
    let mut ok = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut file_info,
            mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if (ok == 0 || file_info.hIcon.is_null())
        && _target
            .to_string_lossy()
            .get(.."shell:".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:"))
    {
        use windows::{
            core::PCWSTR,
            Win32::{System::Com::CoTaskMemFree, UI::Shell::SHParseDisplayName},
        };
        let mut pidl = std::ptr::null_mut();
        if unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None).is_ok() }
            && !pidl.is_null()
        {
            ok = unsafe {
                SHGetFileInfoW(
                    pidl.cast(),
                    0,
                    &mut file_info,
                    mem::size_of::<SHFILEINFOW>() as u32,
                    SHGFI_ICON | SHGFI_LARGEICON | windows_sys::Win32::UI::Shell::SHGFI_PIDL,
                )
            };
            unsafe { CoTaskMemFree(Some(pidl.cast())) };
        }
    }
    if ok == 0 || file_info.hIcon.is_null() {
        return None;
    }
    let mut icon_info: ICONINFO = unsafe { mem::zeroed() };
    let result = (|| {
        if unsafe { GetIconInfo(file_info.hIcon, &mut icon_info) } == 0 {
            return None;
        }
        let width = 32_u32;
        let height = 32_u32;
        let mut pixels = vec![0_u8; (width * height * 4) as usize];
        let mut bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..unsafe { mem::zeroed() }
            },
            bmiColors: [unsafe { mem::zeroed() }],
        };
        let dc = unsafe { GetDC(std::ptr::null_mut()) };
        if dc.is_null()
            || unsafe {
                GetDIBits(
                    dc,
                    icon_info.hbmColor,
                    0,
                    height,
                    pixels.as_mut_ptr().cast(),
                    &mut bitmap_info,
                    DIB_RGB_COLORS,
                )
            } == 0
        {
            return None;
        }
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        Some(super::IconData {
            width,
            height,
            rgba: pixels,
        })
    })();
    unsafe {
        DestroyIcon(file_info.hIcon);
    }
    if !icon_info.hbmColor.is_null() {
        unsafe {
            windows_sys::Win32::Graphics::Gdi::DeleteObject(icon_info.hbmColor as _);
        }
    }
    if !icon_info.hbmMask.is_null() {
        unsafe {
            windows_sys::Win32::Graphics::Gdi::DeleteObject(icon_info.hbmMask as _);
        }
    }
    result
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
        if title.trim().is_empty() || has_store_title(applications, &title) {
            continue;
        }
        applications.push(apps_folder_application(&parsing, filesystem_path, title));
    }
    unsafe { CoTaskMemFree(Some(root_pidl.cast())) };
}

fn provider_debug(message: &str) {
    if std::env::var_os("WOWIE_PROVIDER_DEBUG").is_some() {
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
        for entry in entries {
            let (Ok(display), Ok(aumid)) = (entry.DisplayInfo(), entry.AppUserModelId()) else {
                continue;
            };
            let Ok(title) = display.DisplayName() else {
                continue;
            };
            let title = title.to_string();
            let aumid = aumid.to_string();
            if title.trim().is_empty() || aumid.is_empty() || has_store_title(applications, &title)
            {
                continue;
            }
            applications.push(store_application(&aumid, title, "Package"));
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
            source: "AppsFolder",
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
            source: "AppsFolder",
        };
    }

    super::StoreApplication {
        identity: format!("shell:{}", value.to_lowercase()),
        title,
        aliases: Vec::new(),
        launch_target: PathBuf::from(format!("shell:AppsFolder\\{value}")),
        executable: None,
        source: "AppsFolder",
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
    super::StoreApplication {
        identity: format!("aumid:{}", aumid.to_lowercase()),
        title,
        aliases,
        launch_target: PathBuf::from(format!("shell:AppsFolder\\{aumid}")),
        executable: None,
        source,
    }
}

fn has_store_title(applications: &[super::StoreApplication], title: &str) -> bool {
    applications
        .iter()
        .any(|application| application.title.eq_ignore_ascii_case(title))
}

pub(crate) fn resolve_shortcut(path: &Path) -> super::ShortcutInfo {
    if let Some(shortcut) = resolve_shortcut_native(path) {
        return shortcut;
    }

    // Keep the binary parser as a fallback for environments where COM cannot
    // be initialized. The legacy path above is the authoritative resolver.
    let Ok(data) = fs::read(path) else {
        return super::ShortcutInfo::default();
    };
    if data.len() < 0x4c || &data[..4] != b"\x4c\x00\x00\x00" {
        return super::ShortcutInfo::default();
    }

    let target = u32_at(&data, 0x48)
        .and_then(|offset| (offset != 0).then_some(offset as usize))
        .and_then(|offset| parse_link_info(&data, offset));
    super::ShortcutInfo {
        target,
        ..Default::default()
    }
}

fn resolve_shortcut_native(path: &Path) -> Option<super::ShortcutInfo> {
    use windows::{
        core::{Interface, PCWSTR},
        Win32::{
            Storage::FileSystem::WIN32_FIND_DATAW,
            System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ},
            UI::Shell::{IShellLinkW, SLGP_RAWPATH},
        },
    };

    let _com = ComScope::initialize()?;
    let link: IShellLinkW =
        unsafe { CoCreateInstance(&SHELL_LINK_CLSID, None, CLSCTX_INPROC_SERVER).ok()? };
    let persist: IPersistFile = link.cast().ok()?;
    let path = wide_path(path);
    unsafe { persist.Load(PCWSTR(path.as_ptr()), STGM_READ).ok()? };
    unsafe {
        link.Resolve(
            windows::Win32::Foundation::HWND(std::ptr::null_mut()),
            windows::Win32::UI::Shell::SLR_NO_UI.0 as u32,
        )
        .ok()?;
    }

    let mut target = vec![0_u16; 32_768];
    unsafe {
        link.GetPath(
            &mut target,
            std::ptr::null_mut::<WIN32_FIND_DATAW>(),
            SLGP_RAWPATH.0 as u32,
        )
        .ok()?;
    }
    let length = target.iter().position(|unit| *unit == 0)?;
    if length == 0 {
        return None;
    }
    let mut arguments = vec![0_u16; 32_768];
    unsafe { link.GetArguments(&mut arguments).ok()? };
    let argument_length = arguments.iter().position(|unit| *unit == 0).unwrap_or(0);
    let arguments =
        (argument_length > 0).then(|| String::from_utf16_lossy(&arguments[..argument_length]));
    let mut working_directory = vec![0_u16; 32_768];
    unsafe { link.GetWorkingDirectory(&mut working_directory).ok()? };
    let working_directory_length = working_directory
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(0);
    let working_directory = (working_directory_length > 0).then(|| {
        PathBuf::from(String::from_utf16_lossy(
            &working_directory[..working_directory_length],
        ))
    });
    Some(super::ShortcutInfo {
        target: Some(PathBuf::from(String::from_utf16_lossy(&target[..length]))),
        arguments,
        working_directory,
    })
}

const RPC_E_CHANGED_MODE: u32 = 0x8001_0106;

struct ComScope(bool);

struct WinRtScope(bool);

impl WinRtScope {
    fn initialize() -> Option<Self> {
        let result = unsafe { co_initialize_mta() };
        if result >= 0 {
            Some(Self(true))
        } else if result as u32 == RPC_E_CHANGED_MODE {
            Some(Self(false))
        } else {
            None
        }
    }
}

impl Drop for WinRtScope {
    fn drop(&mut self) {
        if self.0 {
            unsafe { co_uninitialize() };
        }
    }
}

impl ComScope {
    fn initialize() -> Option<Self> {
        let result = unsafe { co_initialize_mta() };
        if result >= 0 {
            Some(Self(true))
        } else if result as u32 == RPC_E_CHANGED_MODE {
            Some(Self(false))
        } else {
            None
        }
    }
}

unsafe fn co_initialize_mta() -> i32 {
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *const std::ffi::c_void, init_type: u32) -> i32;
    }
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED) }
}

unsafe fn co_uninitialize() {
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoUninitialize();
    }
    unsafe { CoUninitialize() };
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            unsafe { co_uninitialize() };
        }
    }
}

const SHELL_LINK_CLSID: windows::core::GUID =
    windows::core::GUID::from_u128(0x00021401_0000_0000_c000_000000000046);

fn wide_path(path: &Path) -> Vec<u16> {
    OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn parse_link_info(data: &[u8], offset: usize) -> Option<PathBuf> {
    let size = u32_at(data, offset)? as usize;
    let header_size = u32_at(data, offset + 4)? as usize;
    if size < 0x1c || offset.checked_add(size)? > data.len() {
        return None;
    }
    let ansi_local_offset = u32_at(data, offset + 0x10).unwrap_or_default() as usize;
    let ansi_suffix_offset = u32_at(data, offset + 0x18).unwrap_or_default() as usize;
    let unicode_local_offset = (header_size >= 0x24)
        .then(|| u32_at(data, offset + 0x1c).unwrap_or_default() as usize)
        .filter(|value| *value != 0);
    let unicode_suffix_offset = (header_size >= 0x24)
        .then(|| u32_at(data, offset + 0x20).unwrap_or_default() as usize)
        .filter(|value| *value != 0);
    if ansi_local_offset == 0 && unicode_local_offset.is_none() {
        return None;
    }
    let base = if let Some(local_offset) = unicode_local_offset {
        read_utf16_z(data, offset + local_offset)
    } else {
        read_ansi_z(data, offset + ansi_local_offset)
    }?;
    let suffix = if let Some(suffix_offset) = unicode_suffix_offset {
        read_utf16_z(data, offset + suffix_offset).unwrap_or_default()
    } else if ansi_suffix_offset == 0 {
        String::new()
    } else {
        read_ansi_z(data, offset + ansi_suffix_offset).unwrap_or_default()
    };
    Some(PathBuf::from(if suffix.is_empty() {
        base
    } else {
        format!("{base}\\{suffix}")
    }))
}

fn read_utf16_z(data: &[u8], offset: usize) -> Option<String> {
    let bytes = data.get(offset..)?;
    let end = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .position(|pair| *pair == [0, 0])?;
    String::from_utf16(
        &bytes[..end * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>(),
    )
    .ok()
}

fn read_ansi_z(data: &[u8], offset: usize) -> Option<String> {
    let bytes = data.get(offset..)?;
    let end = bytes.iter().position(|byte| *byte == 0)?;
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

impl PlatformIntegration for WindowsPlatform {
    fn system_prefers_dark_theme(&self) -> bool {
        read_system_prefers_dark_theme()
    }

    fn system_accent_color(&self) -> u32 {
        read_system_accent_color()
    }

    fn register_hotkey(&self, hotkey: &str, watch_theme: bool) -> Result<Receiver<PlatformEvent>> {
        let (sender, receiver) = mpsc::channel();
        if watch_theme {
            watch_system_theme(sender.clone());
        }
        let hotkey = hotkey.to_owned();
        let (modifiers, key) = match parse_hotkey(&hotkey) {
            Some(parsed) => parsed,
            None => {
                eprintln!(
                    "unsupported global hotkey '{hotkey}'; a modifier is required, using Alt+Space"
                );
                (MOD_ALT, 0x20)
            }
        };
        thread::spawn(move || {
            if unsafe { RegisterHotKey(std::ptr::null_mut(), 1, modifiers | MOD_NOREPEAT, key) }
                == 0
            {
                let error = unsafe { GetLastError() };
                if error == 1409 {
                    eprintln!("could not register global hotkey {hotkey}: already registered by another application (Windows error 1409)");
                } else {
                    eprintln!("could not register global hotkey {hotkey}: Windows error {error}");
                }
                return;
            }
            let mut message = MSG {
                hwnd: std::ptr::null_mut(),
                message: 0,
                wParam: 0,
                lParam: 0,
                time: 0,
                pt: POINT { x: 0, y: 0 },
            };
            loop {
                let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
                if result == -1 {
                    let error = unsafe { GetLastError() };
                    eprintln!("global hotkey message loop failed: Windows error {error}");
                    break;
                }
                if result == 0 {
                    break;
                }
                if message.message == WM_HOTKEY && message.wParam == 1 {
                    let _ = sender.send(PlatformEvent::TogglePalette);
                }
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        });
        Ok(receiver)
    }

    fn position_palette(&self, window: &slint::Window) {
        let size = window.size();
        let scale = window.scale_factor();
        let window_width = (size.width as f32 * scale).round() as i32;
        // Keep the search field at a stable screen position even though the
        // compact palette changes its native window height with the result set.
        const PALETTE_ANCHOR_HEIGHT: f32 = 538.0;
        const PALETTE_VERTICAL_OFFSET: f32 = 160.0;
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
            top + (((bottom - top) as f32 - PALETTE_ANCHOR_HEIGHT * scale).max(0.0) / 3.0
                + PALETTE_VERTICAL_OFFSET * scale) as i32,
        ));
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

fn parse_hotkey(value: &str) -> Option<(u32, u32)> {
    let mut modifiers = 0;
    let mut key = None;
    for part in value
        .split('+')
        .map(|part| part.trim().to_ascii_uppercase())
    {
        modifiers |= match part.as_str() {
            "ALT" => MOD_ALT,
            "CTRL" | "CONTROL" => MOD_CONTROL,
            "SHIFT" => MOD_SHIFT,
            "WIN" | "META" => MOD_WIN,
            "SPACE" => {
                key = Some(0x20);
                0
            }
            "ENTER" | "RETURN" => {
                key = Some(0x0D);
                0
            }
            "ESC" | "ESCAPE" => {
                key = Some(0x1B);
                0
            }
            letter if letter.len() == 1 && letter.as_bytes()[0].is_ascii_alphabetic() => {
                key = Some(letter.as_bytes()[0].to_ascii_uppercase() as u32);
                0
            }
            _ => return None,
        };
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
    fn parses_known_folder_guid() {
        assert!(parse_guid("6D809377-6AF0-444B-8957-A3773F02200E").is_some());
    }

    #[test]
    fn resolves_apps_folder_known_folder_parsing_name() {
        let path =
            resolve_known_folder_path(r"{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\Steam\Steam.exe")
                .expect("FOLDERID_ProgramFilesX86 should resolve");
        assert!(path.ends_with(r"Steam\Steam.exe"));
    }
}
