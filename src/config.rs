use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, path::PathBuf};

const DEFAULT_HOTKEY: &str = "Alt+Space";
const DEFAULT_RENDERER: &str = "femtovg";
const DEFAULT_WINDOW_WIDTH: u32 = 720;
const MIN_WINDOW_WIDTH: u32 = 420;
const MAX_WINDOW_WIDTH: u32 = 1200;
const DEFAULT_THEME: &str = "system";
const DEFAULT_LANGUAGE: &str = "system";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: String,
    pub renderer: String,
    pub window_width: u32,
    pub theme: String,
    pub language: String,
    pub show_on_startup: bool,
    pub hide_on_focus_loss: bool,
    pub show_advanced_details: bool,
    pub fuzzy_search: bool,
    pub shortcuts: Vec<ShortcutConfig>,
    pub providers: ProviderConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutConfig {
    pub name: String,
    pub prefix: String,
    #[serde(rename = "type", default)]
    pub shortcut_type: String,
    pub template: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub store_apps: bool,
    pub start_menu: bool,
    pub desktop: bool,
    pub control_panel: bool,
    pub steam: bool,
    pub steam_excluded_app_ids: Vec<String>,
    pub steam_excluded_name_markers: Vec<String>,
    pub indexed_directories: Vec<PathBuf>,
    pub indexed_extensions: Vec<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            store_apps: true,
            start_menu: true,
            desktop: true,
            control_panel: true,
            steam: true,
            steam_excluded_app_ids: vec!["228980".into()],
            steam_excluded_name_markers: vec!["redistributable".into()],
            indexed_directories: Vec::new(),
            indexed_extensions: vec!["lnk".into(), "url".into(), "exe".into()],
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.into(),
            renderer: DEFAULT_RENDERER.into(),
            window_width: DEFAULT_WINDOW_WIDTH,
            theme: DEFAULT_THEME.into(),
            language: DEFAULT_LANGUAGE.into(),
            show_on_startup: true,
            hide_on_focus_loss: true,
            show_advanced_details: false,
            fuzzy_search: true,
            shortcuts: vec![ShortcutConfig {
                name: "Search with Google".into(),
                prefix: "g".into(),
                shortcut_type: "url".into(),
                template: "https://www.google.com/search?q={query}".into(),
                target: String::new(),
                arguments: String::new(),
            }],
            providers: ProviderConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> (Self, bool) {
        let path = config_path();
        let Some(text) = fs::read_to_string(&path).ok() else {
            return (Self::default(), true);
        };
        let mut config: Self = match serde_json::from_str(&text) {
            Ok(config) => config,
            Err(error) => {
                eprintln!(
                    "could not read configuration {}: {error}; using defaults",
                    path.display()
                );
                return (Self::default(), false);
            }
        };
        config.normalize();
        (config, true)
    }

    fn normalize(&mut self) {
        self.hotkey = self.hotkey.trim().to_string();
        if self.hotkey.is_empty() {
            self.hotkey = DEFAULT_HOTKEY.into();
        }
        if self.renderer != "femtovg"
            && self.renderer != "skia"
            && self.renderer != "skia-opengl"
            && self.renderer != "software"
        {
            self.renderer = DEFAULT_RENDERER.into();
        }
        self.window_width = self.window_width.clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_WIDTH);
        if self.theme != "system" && self.theme != "light" && self.theme != "dark" {
            self.theme = DEFAULT_THEME.into();
        }
        if self.language != "system" && !crate::i18n::is_supported_language(&self.language) {
            self.language = DEFAULT_LANGUAGE.into();
        }
        for shortcut in &mut self.shortcuts {
            shortcut.name = shortcut.name.trim().to_string();
            shortcut.prefix = shortcut.prefix.trim().to_string();
            shortcut.shortcut_type = shortcut.shortcut_type.trim().to_ascii_lowercase();
            if shortcut.shortcut_type.is_empty() {
                shortcut.shortcut_type = "url".into();
            }
            shortcut.template = shortcut.template.trim().to_string();
            shortcut.target = shortcut.target.trim().to_string();
            shortcut.arguments = shortcut.arguments.trim().to_string();
        }
        self.shortcuts.retain(|shortcut| {
            !shortcut.name.is_empty()
                && !shortcut.prefix.is_empty()
                && match shortcut.shortcut_type.as_str() {
                    "url" => shortcut.template.contains("{query}"),
                    "application" | "command" => !shortcut.target.is_empty(),
                    _ => false,
                }
        });
        self.providers
            .steam_excluded_app_ids
            .retain(|value| !value.trim().is_empty());
        self.providers
            .steam_excluded_name_markers
            .retain(|value| !value.trim().is_empty());
        let mut seen_directories = HashSet::new();
        self.providers.indexed_directories.retain(|path| {
            if path.as_os_str().is_empty() || !path.is_dir() {
                eprintln!(
                    "ignoring custom index directory that is not accessible: {}",
                    path.display()
                );
                return false;
            }
            seen_directories.insert(path.to_string_lossy().to_lowercase())
        });
        for extension in &mut self.providers.indexed_extensions {
            *extension = extension
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase();
        }
        self.providers
            .indexed_extensions
            .retain(|value| !value.is_empty());
        self.providers.indexed_extensions.sort();
        self.providers.indexed_extensions.dedup();
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create config directory")?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize config")?;
        fs::write(path, format!("{json}\n")).context("write config")
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wowie")
        .join("config.json")
}

pub fn history_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wowie")
        .join("history.json")
}

pub fn watch_changes() -> std::sync::mpsc::Receiver<()> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let path = config_path();
    std::thread::spawn(move || {
        let mut last_modified = None;
        loop {
            let modified = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok();
            if modified.is_some() && modified != last_modified {
                if last_modified.is_some() && sender.send(()).is_err() {
                    break;
                }
                last_modified = modified;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_stable() {
        let config = Config::default();
        assert_eq!(config.hotkey, "Alt+Space");
        assert_eq!(config.renderer, "femtovg");
        assert!(config.show_on_startup);
        assert!(config.hide_on_focus_loss);
        assert!(!config.show_advanced_details);
        assert!(config.fuzzy_search);
        assert!(config
            .shortcuts
            .iter()
            .any(|shortcut| { shortcut.prefix == "g" && shortcut.name == "Search with Google" }));
        assert!(config.providers.steam);
        assert!(config.providers.desktop);
        assert!(config.providers.indexed_directories.is_empty());
        assert!(config
            .providers
            .indexed_extensions
            .iter()
            .any(|extension| extension == "exe"));
    }
}
