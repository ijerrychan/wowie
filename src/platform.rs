use anyhow::Result;
use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformEvent {
    TogglePalette,
    SystemThemeChanged,
    Quit,
}

pub struct HotkeyRegistration {
    pub events: std::sync::mpsc::Receiver<PlatformEvent>,
    pub updates: std::sync::mpsc::Sender<String>,
}

pub struct TrayRegistration {
    pub events: std::sync::mpsc::Receiver<PlatformEvent>,
    pub updates: std::sync::mpsc::Sender<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppProviderSource {
    StoreApps,
    StartMenu,
    Desktop,
    ControlPanel,
    Steam,
    CustomDirectories,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceId {
    AppsFolder,
    Package,
    StartMenu,
    Desktop,
    ControlPanel,
    Steam,
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
            Self::Steam => Cow::Borrowed("Steam"),
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
            | Self::Steam
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
            "Steam" => Self::Steam,
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
    pub const STEAM: &str = "Steam";
    pub const CUSTOM_DIRECTORIES: &str = "CustomDirectories";
}

/// Windows application-discovery rules shared by providers and icon lookup.
pub mod windows_rules {
    use std::path::Path;

    pub const SHORTCUT_EXTENSION: &str = "lnk";
    pub const URL_EXTENSION: &str = "url";
    pub const EXECUTABLE_EXTENSION: &str = "exe";
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
) -> std::sync::mpsc::Receiver<AppProviderSource> {
    #[cfg(windows)]
    {
        windows::watch_app_provider_sources(custom_directories)
    }
    #[cfg(not(windows))]
    {
        let (_sender, receiver) = std::sync::mpsc::channel();
        receiver
    }
}

pub trait PlatformIntegration: Send + Sync {
    fn register_hotkey(&self, hotkey: &str, watch_system_theme: bool)
        -> Result<HotkeyRegistration>;
    fn start_tray(&self, exit_label: String) -> TrayRegistration;
    fn system_prefers_dark_theme(&self) -> bool;
    fn system_accent_color(&self) -> u32;
    fn position_palette(&self, window: &slint::Window, result_mode: bool, result_height: f32);
    fn position_context_menu(&self, window: &slint::Window);
    fn activate_context_menu(&self, window: &slint::Window);
    fn is_window_focused(&self, window: &slint::Window) -> bool;
    fn activate_palette(&self, window: &slint::Window);
    fn install_focus_listener(&self, window: &slint::Window, on_lost_focus: Box<dyn Fn()>);
    fn disable_window_transitions(&self, window: &slint::Window);
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Language-independent Control Panel canonical name, kept searchable
    /// alongside the localized title.
    pub aliases: Vec<String>,
    pub launch_target: std::path::PathBuf,
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

pub fn discover_store_applications() -> Vec<StoreApplication> {
    #[cfg(windows)]
    {
        windows::discover_store_applications()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
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

pub fn steam_install_roots() -> Vec<std::path::PathBuf> {
    #[cfg(windows)]
    {
        windows::steam_install_roots()
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
