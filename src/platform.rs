use anyhow::Result;
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
    Steam,
}

pub fn watch_app_provider_sources() -> std::sync::mpsc::Receiver<AppProviderSource> {
    #[cfg(windows)]
    {
        windows::watch_app_provider_sources()
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
    /// Canonical Shell/parsing name, kept searchable alongside the localized title.
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
