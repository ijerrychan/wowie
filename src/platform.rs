use anyhow::Result;
use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::thread::JoinHandle;

#[cfg(windows)]
pub(crate) mod shell_id_list;
#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(crate) use windows::SettingsAvailability;

#[cfg(windows)]
pub(crate) use windows::query_file_index;

pub(crate) fn set_run_at_sign_in(enabled: bool) -> Result<()> {
    #[cfg(windows)]
    {
        windows::set_run_at_sign_in(enabled)
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) fn windows_build_number() -> Option<u32> {
    windows::windows_build_number()
}

#[cfg(windows)]
pub(crate) fn windows_setting_capability_available(capability: &str) -> Option<bool> {
    windows::windows_setting_capability_available(capability)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformEvent {
    TogglePalette,
    OpenSettings,
    SystemThemeChanged,
    Quit,
}

/// Convert a point expressed in Slint logical coordinates into screen
/// physical coordinates without embedding UI layout constants in a platform
/// implementation.
pub(crate) fn logical_point_to_physical(
    origin: (i32, i32),
    point: (f32, f32),
    scale_factor: f32,
) -> (i32, i32) {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    (
        origin.0 + (point.0 * scale_factor).round() as i32,
        origin.1 + (point.1 * scale_factor).round() as i32,
    )
}

pub struct HotkeyRegistration {
    pub events: std::sync::mpsc::Receiver<PlatformEvent>,
    pub updates: std::sync::mpsc::Sender<String>,
}

pub struct TrayRegistration {
    pub events: std::sync::mpsc::Receiver<PlatformEvent>,
    pub updates: std::sync::mpsc::Sender<TrayMenuLabels>,
}

struct ShutdownState {
    stopped: AtomicBool,
    #[cfg(windows)]
    event: isize,
}

#[derive(Clone)]
pub struct ShutdownSignal(Arc<ShutdownState>);

impl ShutdownSignal {
    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
    }

    #[cfg(windows)]
    pub(crate) fn event(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.0.event as _
    }
}

#[cfg(windows)]
impl Drop for ShutdownState {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.event as _) };
    }
}

pub struct PlatformShutdown {
    signal: ShutdownSignal,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl PlatformShutdown {
    pub fn new() -> Self {
        #[cfg(windows)]
        let event = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(
                std::ptr::null(),
                1,
                0,
                std::ptr::null(),
            )
        };
        #[cfg(windows)]
        assert!(!event.is_null(), "could not create platform shutdown event");
        Self {
            signal: ShutdownSignal(Arc::new(ShutdownState {
                stopped: AtomicBool::new(false),
                #[cfg(windows)]
                event: event as isize,
            })),
            threads: Mutex::new(Vec::new()),
        }
    }

    pub fn spawn(&self, work: impl FnOnce(ShutdownSignal) + Send + 'static) {
        let signal = self.signal.clone();
        let thread = std::thread::spawn(move || work(signal));
        self.threads
            .lock()
            .expect("platform thread list poisoned")
            .push(thread);
    }

    pub fn stop_and_join(&self) {
        self.signal.0.stopped.store(true, Ordering::Release);
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Threading::SetEvent(self.signal.event())
        };
        let threads =
            std::mem::take(&mut *self.threads.lock().expect("platform thread list poisoned"));
        for thread in threads {
            if thread.join().is_err() {
                eprintln!("platform listener panicked during shutdown");
            }
        }
    }
}

impl Default for PlatformShutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PlatformShutdown {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[derive(Debug, Clone)]
pub struct TrayMenuLabels {
    pub settings: String,
    pub exit: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum AppProviderSource {
    AppsFolder,
    PackagedApps,
    /// Legacy source ID retained for compatibility with older snapshots.
    StoreApps,
    StartMenu,
    Desktop,
    ControlPanel,
    CustomDirectories,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SourceId {
    AppsFolder,
    Package,
    StartMenu,
    Desktop,
    ControlPanel,
    ExecutionAlias,
    Shortcut,
    CustomDirectories,
    Custom(String),
}

impl SourceId {
    pub fn as_str(&self) -> Cow<'_, str> {
        match self {
            Self::AppsFolder => Cow::Borrowed("AppsFolder"),
            Self::Package => Cow::Borrowed("Package"),
            Self::StartMenu => Cow::Borrowed("StartMenu"),
            Self::Desktop => Cow::Borrowed("Desktop"),
            Self::ControlPanel => Cow::Borrowed("ControlPanel"),
            Self::ExecutionAlias => Cow::Borrowed("ExecutionAlias"),
            Self::Shortcut => Cow::Borrowed("Shortcut"),
            Self::CustomDirectories => Cow::Borrowed("CustomDirectories"),
            Self::Custom(value) => Cow::Borrowed(value),
        }
    }

    pub fn priority(&self) -> u8 {
        match self {
            Self::AppsFolder => 0,
            Self::Package => 1,
            Self::StartMenu => 2,
            Self::Desktop => 3,
            Self::ExecutionAlias
            | Self::Shortcut
            | Self::ControlPanel
            | Self::CustomDirectories
            | Self::Custom(_) => 4,
        }
    }

    pub fn is_store_application(&self) -> bool {
        matches!(self, Self::AppsFolder | Self::Package)
    }

    pub fn is_file_source(&self) -> bool {
        matches!(self, Self::StartMenu | Self::Desktop)
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str().as_ref())
    }
}

impl From<&str> for SourceId {
    fn from(value: &str) -> Self {
        match value {
            "AppsFolder" => Self::AppsFolder,
            "Package" => Self::Package,
            "StartMenu" => Self::StartMenu,
            "Desktop" => Self::Desktop,
            "ControlPanel" => Self::ControlPanel,
            "ExecutionAlias" => Self::ExecutionAlias,
            "Shortcut" => Self::Shortcut,
            "CustomDirectories" => Self::CustomDirectories,
            value => Self::Custom(value.to_owned()),
        }
    }
}

impl From<String> for SourceId {
    fn from(value: String) -> Self {
        Self::from(value.as_str()).with_owned_custom(value)
    }
}

impl SourceId {
    fn with_owned_custom(self, original: String) -> Self {
        match self {
            Self::Custom(_) => Self::Custom(original),
            builtin => builtin,
        }
    }
}

/// Stable identifiers shared by providers and the result model. These are
/// data-source identifiers, not user-facing labels.
pub mod source_ids {
    pub const APPS_FOLDER: &str = "AppsFolder";
    pub const PACKAGE: &str = "Package";
    pub const START_MENU: &str = "StartMenu";
    pub const DESKTOP: &str = "Desktop";
    pub const CONTROL_PANEL: &str = "ControlPanel";
    pub const CUSTOM_DIRECTORIES: &str = "CustomDirectories";
}

/// Windows application-discovery rules shared by providers and icon lookup.
pub mod windows_rules {
    use std::path::Path;

    pub const SHORTCUT_EXTENSION: &str = "lnk";
    pub const URL_EXTENSION: &str = "url";
    pub const EXECUTABLE_EXTENSION: &str = "exe";
    pub const ICON_EXTENSION: &str = "ico";
    pub const ADMINISTRATIVE_TOOLS_DIRECTORY: &str = "Administrative Tools";
    pub const SYSTEM_TOOLS_DIRECTORY: &str = "System Tools";
    pub const UPDATER_FILE_STEM: &str = "update";

    pub fn is_shell_namespace(path: &Path) -> bool {
        let value = path.to_string_lossy();
        value
            .trim()
            .get(.."shell:".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:"))
            || value.trim_start().starts_with("::")
    }

    pub fn is_updater_path(path: &Path) -> bool {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case(UPDATER_FILE_STEM))
    }

    pub fn is_shortcut_path(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(SHORTCUT_EXTENSION))
    }

    pub fn is_icon_path(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(ICON_EXTENSION))
    }

    pub fn is_legacy_tools_path(path: &Path) -> bool {
        path.ancestors().any(|ancestor| {
            ancestor.file_name().is_some_and(|name| {
                name.eq_ignore_ascii_case(ADMINISTRATIVE_TOOLS_DIRECTORY)
                    || name.eq_ignore_ascii_case(SYSTEM_TOOLS_DIRECTORY)
            })
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::Path;

        #[test]
        fn recognizes_shell_namespace_paths() {
            assert!(is_shell_namespace(Path::new(r"shell:AppsFolder\Demo")));
            assert!(is_shell_namespace(Path::new(r":::{CLSID}")));
            assert!(!is_shell_namespace(Path::new(r"C:\Apps\Demo.exe")));
        }

        #[test]
        fn recognizes_updater_file_stems() {
            assert!(is_updater_path(Path::new(r"C:\Apps\Update.exe")));
            assert!(!is_updater_path(Path::new(r"C:\Apps\Demo.exe")));
        }

        #[test]
        fn recognizes_icon_targets_as_non_launchable_placeholders() {
            assert!(is_icon_path(Path::new(r"C:\Windows\Installer\wsl.ico")));
            assert!(!is_icon_path(Path::new(r"C:\Apps\Demo.exe")));
        }

        #[test]
        fn recognizes_legacy_tools_directories() {
            assert!(is_legacy_tools_path(Path::new(
                r"C:\Start Menu\System Tools\Task Manager.lnk"
            )));
            assert!(!is_legacy_tools_path(Path::new(
                r"C:\Start Menu\Utilities\Task Manager.lnk"
            )));
        }
    }
}

/// The shell used to execute command shortcuts.
///
/// Shell discovery is a platform concern. Dynamic providers consume this
/// small value without needing target-specific branches of their own.
#[derive(Debug, Clone)]
pub struct CommandShell {
    pub path: PathBuf,
    pub label: String,
}

pub fn command_shell() -> CommandShell {
    #[cfg(windows)]
    let path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|directory| {
            let candidate = directory.join("pwsh.exe");
            candidate.is_file().then_some(candidate)
        })
    });

    #[cfg(windows)]
    let path = path.unwrap_or_else(|| PathBuf::from("powershell.exe"));

    #[cfg(not(windows))]
    let path = PathBuf::from("pwsh");

    let label = command_shell_label(&path);
    CommandShell { path, label }
}

pub fn command_shell_arguments(command: &str) -> String {
    let escaped = command.replace('"', "\\\"");
    format!("-NoLogo -NoExit -Command \"{escaped}\"")
}

fn command_shell_label(path: &Path) -> String {
    let name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "pwsh" => "PowerShell 7".into(),
        "powershell" => "Windows PowerShell".into(),
        "cmd" => "cmd".into(),
        _ => path.display().to_string(),
    }
}

pub fn watch_app_provider_sources(
    custom_directories: std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
    shutdown: &PlatformShutdown,
) -> std::sync::mpsc::Receiver<AppProviderSource> {
    #[cfg(windows)]
    {
        windows::watch_app_provider_sources(custom_directories, shutdown)
    }
    #[cfg(not(windows))]
    {
        let (sender, receiver) = std::sync::mpsc::channel();
        shutdown.spawn(move |signal| {
            let mut previous = None;
            while !signal.is_stopped() {
                let Ok(paths) = custom_directories.lock().map(|paths| paths.clone()) else {
                    break;
                };
                let current = crate::utils::filesystem_fingerprint(&paths);
                if previous.is_some_and(|value| value != current)
                    && sender.send(AppProviderSource::CustomDirectories).is_err()
                {
                    break;
                }
                previous = Some(current);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });
        receiver
    }
}

pub trait PlatformIntegration: Send + Sync {
    fn register_hotkey(
        &self,
        hotkey: &str,
        watch_system_theme: bool,
        shutdown: &PlatformShutdown,
    ) -> Result<HotkeyRegistration>;
    fn start_tray(&self, labels: TrayMenuLabels, shutdown: &PlatformShutdown) -> TrayRegistration;
    fn system_prefers_dark_theme(&self) -> bool;
    fn system_accent_color(&self) -> u32;
    fn position_palette(&self, window: &slint::Window, result_height: f32);
    fn position_context_menu(
        &self,
        window: &slint::Window,
        launcher: &slint::Window,
        anchor: Option<(f32, f32)>,
    );
    fn activate_context_menu(&self, window: &slint::Window);
    fn is_window_focused(&self, window: &slint::Window) -> bool;
    fn activate_palette(&self, window: &slint::Window);
    fn install_focus_listener(&self, window: &slint::Window, on_lost_focus: Box<dyn Fn()>);
    fn disable_window_transitions(&self, window: &slint::Window);
}

/// Match the native fallback surface to the Slint canvas before showing or resizing.
pub(crate) fn set_window_background(window: &slint::Window, color: slint::Color) {
    #[cfg(windows)]
    windows::set_window_background(window, color);
    #[cfg(not(windows))]
    let _ = (window, color);
}

/// Open the system folder picker owned by the settings window.
pub fn pick_folder(owner: &slint::Window) -> Result<Option<PathBuf>> {
    #[cfg(windows)]
    {
        windows::pick_folder(owner)
    }
    #[cfg(not(windows))]
    {
        let _ = owner;
        Err(anyhow::anyhow!(
            "folder picker is unavailable on this platform"
        ))
    }
}

#[cfg(not(debug_assertions))]
pub struct SingleInstanceGuard {
    #[cfg(windows)]
    handle: isize,
}

#[cfg(not(debug_assertions))]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.handle as _);
        }
    }
}

#[cfg(not(debug_assertions))]
pub fn try_acquire_single_instance() -> Result<Option<SingleInstanceGuard>> {
    #[cfg(windows)]
    {
        windows::try_acquire_single_instance()
    }
    #[cfg(not(windows))]
    {
        Ok(Some(SingleInstanceGuard {}))
    }
}

#[derive(Debug, Default)]
pub struct ShortcutInfo {
    pub target: Option<std::path::PathBuf>,
    pub arguments: Option<String>,
    pub working_directory: Option<std::path::PathBuf>,
    /// File that owns the shortcut's shell icon. The icon index selects the
    /// resource used when resolving the icon through Shell32.
    pub icon_path: Option<std::path::PathBuf>,
    pub icon_index: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageMetadata {
    pub full_name: Option<String>,
    pub install_location: Option<std::path::PathBuf>,
    /// The package Identity/Name value from the package metadata.
    pub identity_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoreApplication {
    pub identity: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub launch_target: std::path::PathBuf,
    pub executable: Option<std::path::PathBuf>,
    pub description: Option<String>,
    pub app_user_model_id: Option<String>,
    pub package_family_name: Option<String>,
    pub package_metadata: Option<PackageMetadata>,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub struct ControlPanelItem {
    pub identity: String,
    pub title: String,
    /// Canonical applet names or OS-localized task keywords.
    pub aliases: Vec<String>,
    pub area: Option<String>,
    pub launch_target: std::path::PathBuf,
    /// Captured absolute Shell PIDL; independent of the launch destination.
    pub icon_target: std::path::PathBuf,
}

/// A small, renderer-neutral icon payload. Pixels are kept off the Slint UI
/// thread until the result model is materialized.
#[derive(Debug, Clone)]
pub struct IconData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn application_icon(_target: &std::path::Path) -> Option<IconData> {
    #[cfg(windows)]
    {
        windows::application_icon(_target)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn indexed_application_icon(_target: &std::path::Path, _index: i32) -> Option<IconData> {
    #[cfg(windows)]
    {
        windows::indexed_application_icon(_target, _index)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Return the user-facing description associated with an application target.
///
/// Windows stores this in the executable's version information, or exposes it
/// through the package metadata for AppsFolder targets. Other platforms simply
/// report no description and let the UI use its category fallback.
pub fn application_description(target: &std::path::Path) -> Option<String> {
    #[cfg(windows)]
    {
        windows::application_description(target)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        None
    }
}

pub fn default_open_icon(_target: &std::path::Path) -> Option<IconData> {
    #[cfg(windows)]
    {
        windows::default_open_icon(_target)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn resolve_shortcut(path: &std::path::Path) -> ShortcutInfo {
    #[cfg(windows)]
    {
        windows::resolve_shortcut(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        ShortcutInfo::default()
    }
}

pub fn discover_apps_folder_applications() -> Vec<StoreApplication> {
    #[cfg(windows)]
    {
        windows::discover_apps_folder_applications()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

pub fn discover_packaged_applications() -> Vec<StoreApplication> {
    #[cfg(windows)]
    {
        windows::discover_packaged_applications()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

pub fn package_inventory_fingerprint() -> Option<u64> {
    #[cfg(windows)]
    {
        windows::package_inventory_fingerprint()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn discover_control_panel_items() -> Vec<ControlPanelItem> {
    #[cfg(windows)]
    {
        windows::discover_control_panel_items()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

pub fn current() -> Arc<dyn PlatformIntegration> {
    #[cfg(windows)]
    {
        Arc::new(windows::WindowsPlatform)
    }
    #[cfg(not(windows))]
    {
        Arc::new(unsupported::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::logical_point_to_physical;

    #[test]
    fn platform_shutdown_joins_all_listeners_across_restarts() {
        use super::PlatformShutdown;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc,
        };
        let active = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let shutdown = PlatformShutdown::new();
            let (started, ready) = mpsc::channel();
            for _ in 0..3 {
                let active = Arc::clone(&active);
                let started = started.clone();
                shutdown.spawn(move |signal| {
                    active.fetch_add(1, Ordering::SeqCst);
                    started.send(()).unwrap();
                    #[cfg(windows)]
                    unsafe {
                        windows_sys::Win32::System::Threading::WaitForSingleObject(
                            signal.event(),
                            u32::MAX,
                        )
                    };
                    #[cfg(not(windows))]
                    while !signal.is_stopped() {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
            for _ in 0..3 {
                ready.recv().unwrap();
            }
            shutdown.stop_and_join();
            assert_eq!(active.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn logical_point_conversion_applies_origin_and_scale() {
        assert_eq!(
            logical_point_to_physical((100, 200), (12.25, 8.5), 1.5),
            (118, 213)
        );
    }

    #[test]
    fn logical_point_conversion_rounds_negative_offsets_consistently() {
        assert_eq!(
            logical_point_to_physical((100, 200), (-2.4, -1.6), 1.25),
            (97, 198)
        );
    }

    #[test]
    fn invalid_scale_falls_back_to_one() {
        assert_eq!(
            logical_point_to_physical((10, 20), (2.5, 3.5), 0.0),
            (13, 24)
        );
        assert_eq!(
            logical_point_to_physical((10, 20), (2.5, 3.5), f32::NAN),
            (13, 24)
        );
    }
}
