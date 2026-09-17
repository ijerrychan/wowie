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
