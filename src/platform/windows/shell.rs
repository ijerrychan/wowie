use super::*;

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

pub(super) fn control_panel_clsid(parsing_name: &str) -> Option<&str> {
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
pub(crate) fn discover_control_panel_items() -> Vec<super::super::ControlPanelItem> {
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
            let canonical_name =
                control_panel_application_name(&folder, root_pidl, child, &parsing)
                    .or_else(|| parsing.as_deref().and_then(control_panel_canonical_name));
            unsafe { CoTaskMemFree(Some(child.cast())) };
            let (Some(title), Some(parsing)) = (title, parsing) else {
                continue;
            };
            if title.trim().is_empty() || parsing.trim().is_empty() {
                continue;
            }
            let target = PathBuf::from(parsing);
            provider_debug(&format!(
                "Control Panel item title={title:?} parsing={target:?} canonical={canonical_name:?}"
            ));
            let identity = format!("control-panel:{}", target.to_string_lossy().to_lowercase());
            items.push(super::super::ControlPanelItem {
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

fn control_panel_application_name(
    folder: &windows::Win32::UI::Shell::IShellFolder,
    root_pidl: *const windows::Win32::UI::Shell::Common::ITEMIDLIST,
    child: *const windows::Win32::UI::Shell::Common::ITEMIDLIST,
    parsing_name: &Option<String>,
) -> Option<String> {
    use windows::{
        core::{Interface, GUID},
        Win32::{
            Foundation::PROPERTYKEY,
            System::Com::CoTaskMemFree,
            UI::Shell::{
                IShellItem, IShellItem2, SHCreateItemFromParsingName, SHCreateItemWithParent,
            },
        },
    };

    // PKEY_ApplicationName (System.ApplicationName) is also the canonical
    // name used by Control Panel's /name command. Querying it from the Shell
    // item works even when its parsing name ends in a localized display name.
    const APPLICATION_NAME: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID {
            data1: 0xF29F85E0,
            data2: 0x4FF9,
            data3: 0x1068,
            data4: [0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9],
        },
        pid: 18,
    };

    let read_application_name = |item: IShellItem| -> Option<String> {
        let item: IShellItem2 = item.cast().ok()?;
        let value = unsafe { item.GetString(&APPLICATION_NAME).ok()? };
        if value.0.is_null() {
            return None;
        }
        let name = unsafe { value.to_string().ok() };
        unsafe { CoTaskMemFree(Some(value.0.cast())) };
        name.filter(|name| !name.trim().is_empty())
    };

    unsafe { SHCreateItemWithParent(Some(root_pidl), folder, child).ok() }
        .and_then(read_application_name)
        .or_else(|| {
            let parsing_name = parsing_name.as_deref()?;
            let parsing_name = wide_path(Path::new(parsing_name));
            unsafe {
                SHCreateItemFromParsingName(windows::core::PCWSTR(parsing_name.as_ptr()), None).ok()
            }
            .and_then(read_application_name)
        })
}

fn open_registry_key(hive: HKEY, subkey: &str) -> Option<HKEY> {
    let wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = std::ptr::null_mut();
    let result = unsafe { RegOpenKeyExW(hive, wide.as_ptr(), 0, KEY_READ, &mut key) };
    (result == 0).then_some(key)
}

pub(crate) fn windows_build_number() -> Option<u32> {
    let key = open_registry_key(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
    )?;
    let build = read_registry_string(key, "CurrentBuild")
        .and_then(|value| value.parse::<u32>().ok())
        .or_else(|| {
            read_registry_string(key, "CurrentBuildNumber")
                .and_then(|value| value.parse::<u32>().ok())
        });
    unsafe { RegCloseKey(key) };
    build
}

pub(crate) fn windows_setting_capability_available(capability: &str) -> Option<bool> {
    match capability {
        "battery" => system_has_battery(),
        "touchpad" => system_has_precision_touchpad(),
        "wifi_adapter" => system_has_wifi_adapter(),
        "mixed_reality_portal" => package_is_installed("Microsoft.MixedReality.Portal"),
        _ => None,
    }
}

#[repr(C)]
struct SystemPowerStatus {
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    reserved1: u8,
    battery_life_time: u32,
    battery_full_life_time: u32,
}

#[link(name = "Kernel32")]
unsafe extern "system" {
    fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
}

fn system_has_battery() -> Option<bool> {
    let mut status = SystemPowerStatus {
        ac_line_status: 0,
        battery_flag: 0,
        battery_life_percent: 0,
        reserved1: 0,
        battery_life_time: 0,
        battery_full_life_time: 0,
    };
    let success = unsafe { GetSystemPowerStatus(&mut status) };
    if success == 0 || status.battery_flag == u8::MAX {
        return None;
    }
    Some(status.battery_flag != 128)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInputDeviceList {
    device: *mut std::ffi::c_void,
    device_type: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInputHidInfo {
    vendor_id: u32,
    product_id: u32,
    version_number: u32,
    usage_page: u16,
    usage: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
union RawInputDeviceInfoData {
    hid: RawInputHidInfo,
    padding: [u32; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInputDeviceInfo {
    size: u32,
    device_type: u32,
    data: RawInputDeviceInfoData,
}

#[link(name = "User32")]
unsafe extern "system" {
    fn GetRawInputDeviceList(
        devices: *mut RawInputDeviceList,
        device_count: *mut u32,
        device_size: u32,
    ) -> u32;
    fn GetRawInputDeviceInfoW(
        device: *mut std::ffi::c_void,
        command: u32,
        data: *mut std::ffi::c_void,
        data_size: *mut u32,
    ) -> u32;
}

fn system_has_precision_touchpad() -> Option<bool> {
    const RIM_TYPEHID: u32 = 2;
    const RIDI_DEVICEINFO: u32 = 0x2000_000b;
    let device_size = std::mem::size_of::<RawInputDeviceList>() as u32;
    let mut device_count = 0;
    let count_result =
        unsafe { GetRawInputDeviceList(std::ptr::null_mut(), &mut device_count, device_size) };
    if count_result == u32::MAX {
        return None;
    }
    if device_count == 0 {
        return Some(false);
    }

    let mut devices =
        vec![unsafe { std::mem::zeroed::<RawInputDeviceList>() }; device_count as usize];
    let listed =
        unsafe { GetRawInputDeviceList(devices.as_mut_ptr(), &mut device_count, device_size) };
    if listed == u32::MAX {
        return None;
    }

    let mut read_any_hid_info = false;
    for device in devices.iter().take(listed as usize) {
        if device.device_type != RIM_TYPEHID {
            continue;
        }
        let mut info = RawInputDeviceInfo {
            size: std::mem::size_of::<RawInputDeviceInfo>() as u32,
            device_type: 0,
            data: RawInputDeviceInfoData { padding: [0; 5] },
        };
        let mut info_size = info.size;
        let result = unsafe {
            GetRawInputDeviceInfoW(
                device.device,
                RIDI_DEVICEINFO,
                (&mut info as *mut RawInputDeviceInfo).cast(),
                &mut info_size,
            )
        };
        if result == u32::MAX {
            continue;
        }
        read_any_hid_info = true;
        if info.device_type != RIM_TYPEHID {
            continue;
        }
        let hid = unsafe { info.data.hid };
        if hid.usage_page == 0x000d && hid.usage == 0x0005 {
            return Some(true);
        }
    }
    read_any_hid_info.then_some(false)
}

#[repr(C)]
struct NativeGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct WlanInterfaceInfo {
    interface_guid: NativeGuid,
    description: [u16; 256],
    state: u32,
}

#[repr(C)]
struct WlanInterfaceInfoList {
    number_of_items: u32,
    index: u32,
    interfaces: [WlanInterfaceInfo; 1],
}

#[link(name = "Wlanapi")]
unsafe extern "system" {
    fn WlanOpenHandle(
        client_version: u32,
        reserved: *const std::ffi::c_void,
        negotiated_version: *mut u32,
        client_handle: *mut *mut std::ffi::c_void,
    ) -> u32;
    fn WlanCloseHandle(
        client_handle: *mut std::ffi::c_void,
        reserved: *const std::ffi::c_void,
    ) -> u32;
    fn WlanEnumInterfaces(
        client_handle: *mut std::ffi::c_void,
        reserved: *const std::ffi::c_void,
        interface_list: *mut *mut WlanInterfaceInfoList,
    ) -> u32;
    fn WlanFreeMemory(memory: *const std::ffi::c_void);
}

fn system_has_wifi_adapter() -> Option<bool> {
    let mut negotiated_version = 0;
    let mut client_handle = std::ptr::null_mut();
    let opened = unsafe {
        WlanOpenHandle(
            2,
            std::ptr::null(),
            &mut negotiated_version,
            &mut client_handle,
        )
    };
    if opened != 0 || client_handle.is_null() {
        return None;
    }

    let mut interface_list = std::ptr::null_mut();
    let enumerated =
        unsafe { WlanEnumInterfaces(client_handle, std::ptr::null(), &mut interface_list) };
    let available = if enumerated != 0 || interface_list.is_null() {
        None
    } else {
        Some(unsafe { (*interface_list).number_of_items > 0 })
    };
    if !interface_list.is_null() {
        unsafe { WlanFreeMemory(interface_list.cast()) };
    }
    unsafe { WlanCloseHandle(client_handle, std::ptr::null()) };
    available
}

fn package_is_installed(identity_name: &str) -> Option<bool> {
    use windows::Management::Deployment::{PackageManager, PackageTypes};

    let _winrt = WinRtScope::initialize()?;
    let manager = PackageManager::new().ok()?;
    let packages = manager
        .FindPackagesWithPackageTypes(PackageTypes::Main | PackageTypes::Optional)
        .ok()?;
    Some(packages.into_iter().any(|package| {
        package
            .Id()
            .ok()
            .and_then(|id| id.Name().ok())
            .is_some_and(|name| name.to_string().eq_ignore_ascii_case(identity_name))
    }))
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

pub(super) fn collect_apps_folder(applications: &mut Vec<super::super::StoreApplication>) {
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

pub(super) fn collect_packaged_apps(applications: &mut Vec<super::super::StoreApplication>) {
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
        let package_metadata = Some(super::super::PackageMetadata {
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
                    super::super::source_ids::PACKAGE,
                    metadata,
                    description,
                ),
            );
        }
    }
}

/// Preserve all of the legacy AppsFolder parsing-name cases. AppsFolder can
/// return an AUMID, a real executable path, or another virtual-shell name.
pub(super) fn apps_folder_application(
    parsing: &str,
    filesystem_path: Option<PathBuf>,
    title: String,
) -> super::super::StoreApplication {
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
        return super::super::StoreApplication {
            identity: format!("url:{url}"),
            title,
            aliases: Vec::new(),
            launch_target: PathBuf::from(&url),
            executable: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            source: super::super::source_ids::APPS_FOLDER,
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
        return super::super::StoreApplication {
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
            source: super::super::source_ids::APPS_FOLDER,
        };
    }

    super::super::StoreApplication {
        identity: format!("shell:{}", value.to_lowercase()),
        title,
        aliases: Vec::new(),
        launch_target: PathBuf::from(format!("shell:AppsFolder\\{value}")),
        executable: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        source: super::super::source_ids::APPS_FOLDER,
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

pub(super) fn resolve_known_folder_path(value: &str) -> Option<PathBuf> {
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

pub(super) fn parse_guid(value: &str) -> Option<windows::core::GUID> {
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

fn store_application(
    aumid: &str,
    title: String,
    source: &'static str,
) -> super::super::StoreApplication {
    store_application_with_metadata(aumid, title, source, None, None)
}

pub(super) fn store_application_with_metadata(
    aumid: &str,
    title: String,
    source: &'static str,
    package_metadata: Option<super::super::PackageMetadata>,
    description: Option<String>,
) -> super::super::StoreApplication {
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
    super::super::StoreApplication {
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

pub(super) fn push_store_application(
    applications: &mut Vec<super::super::StoreApplication>,
    application: super::super::StoreApplication,
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

#[cfg(test)]
mod tests {
    use super::control_panel_clsid;

    #[test]
    fn control_panel_parser_rejects_non_shell_and_malformed_entries() {
        assert_eq!(
            control_panel_clsid(r"C:\Windows\System32\notepad.exe"),
            None
        );
        assert_eq!(control_panel_clsid("::{not-a-guid}"), None);
    }
}
