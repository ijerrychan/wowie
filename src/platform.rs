use anyhow::Result;
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

#[cfg(not(windows))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformEvent {
    TogglePalette,
    SystemThemeChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppProviderSource {
    StoreApps,
    StartMenu,
    Desktop,
    ControlPanel,
    Settings,
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
    Settings,
    Steam,
    ExecutionAlias,
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
            Self::Settings => Cow::Borrowed("Settings"),
            Self::Steam => Cow::Borrowed("Steam"),
            Self::ExecutionAlias => Cow::Borrowed("ExecutionAlias"),
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
            | Self::Steam
            | Self::ControlPanel
            | Self::Settings
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
            "Settings" => Self::Settings,
            "Steam" => Self::Steam,
            "ExecutionAlias" => Self::ExecutionAlias,
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
    pub const SETTINGS: &str = "Settings";
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
        path.to_string_lossy()
            .trim()
            .get(.."shell:".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:"))
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
    fn register_hotkey(
        &self,
        hotkey: &str,
        watch_system_theme: bool,
    ) -> Result<std::sync::mpsc::Receiver<PlatformEvent>>;
    fn system_prefers_dark_theme(&self) -> bool;
    fn system_accent_color(&self) -> u32;
    fn position_palette(&self, window: &slint::Window);
    fn activate_palette(&self, window: &slint::Window);
    fn install_focus_listener(&self, window: &slint::Window, on_lost_focus: Box<dyn Fn()>);
    fn disable_window_transitions(&self, window: &slint::Window);
}

#[derive(Debug, Default)]
pub struct ShortcutInfo {
    pub target: Option<std::path::PathBuf>,
    pub arguments: Option<String>,
    pub working_directory: Option<std::path::PathBuf>,
    /// File that owns the shortcut's shell icon. The icon index is resolved
    /// by Shell32 and is not needed by the renderer.
    pub icon_path: Option<std::path::PathBuf>,
    pub icon_index: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct StoreApplication {
    pub identity: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub launch_target: std::path::PathBuf,
    pub executable: Option<std::path::PathBuf>,
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

#[derive(Debug, Clone)]
pub struct SettingsItem {
    pub identity: String,
    pub title: String,
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

pub fn discover_settings_items() -> Vec<SettingsItem> {
    #[cfg(windows)]
    {
        windows::discover_settings_items()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

pub fn settings_icon_target() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        windows::settings_icon_target()
    }
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from("SystemSettings.exe")
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
