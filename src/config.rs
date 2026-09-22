use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::PathBuf,
};

const DEFAULT_HOTKEY: &str = "Alt+Space";
const DEFAULT_RENDERER: &str = "femtovg";
const DEFAULT_WINDOW_WIDTH: u32 = 720;
const DEFAULT_WINDOW_HEIGHT: u32 = 550;
const DEFAULT_LIST_ITEM_HEIGHT: u32 = 50;
const DEFAULT_GRID_ITEM_SIZE: u32 = 124;
const MIN_WINDOW_WIDTH: u32 = 420;
pub(crate) const MIN_WINDOW_HEIGHT: u32 = 350;
const MAX_WINDOW_WIDTH: u32 = 1200;
const MIN_LIST_ITEM_HEIGHT: u32 = 36;
const MAX_LIST_ITEM_HEIGHT: u32 = 96;
const MIN_GRID_ITEM_SIZE: u32 = 96;
const MAX_GRID_ITEM_SIZE: u32 = 240;
const DEFAULT_THEME: &str = "system";
const DEFAULT_LANGUAGE: &str = "system";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: String,
    pub renderer: String,
    pub window_width: u32,
    pub window_height: u32,
    pub list_item_height: u32,
    pub grid_item_size: u32,
    pub theme: String,
    pub language: String,
    pub show_on_startup: bool,
    pub hide_on_focus_loss: bool,
    pub show_advanced_details: bool,
    pub show_advanced_launch_entries: bool,
    pub show_results_on_empty_query: bool,
    pub fuzzy_search: bool,
    pub shortcuts: Vec<ShortcutConfig>,
    pub plugins: BTreeMap<String, PluginConfig>,
    pub providers: ProviderConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutConfig {
    pub name: String,
    pub prefix: String,
    /// Optional text shown in the launcher input after entering this engine.
    #[serde(default)]
    pub placeholder: String,
    #[serde(rename = "type", default)]
    pub shortcut_type: String,
    /// Interaction mode after the prefix is recognized. Existing shortcut
    /// configurations default to direct execution for compatibility.
    #[serde(default = "default_shortcut_mode")]
    pub mode: String,
    pub template: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub arguments: String,
    /// Human-facing subtitle for a configured shortcut. Unlike `template`,
    /// this is presentation data and is never exposed as a generated URL.
    #[serde(default)]
    pub description: String,
}

fn default_shortcut_mode() -> String {
    "execute".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PluginConfig {
    pub enabled: bool,
    pub weight: i32,
    pub preferences: BTreeMap<String, serde_json::Value>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            weight: 0,
            preferences: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub store_apps: bool,
    pub start_menu: bool,
    pub desktop: bool,
    pub control_panel: bool,
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
            window_height: DEFAULT_WINDOW_HEIGHT,
            list_item_height: DEFAULT_LIST_ITEM_HEIGHT,
            grid_item_size: DEFAULT_GRID_ITEM_SIZE,
            theme: DEFAULT_THEME.into(),
            language: DEFAULT_LANGUAGE.into(),
            show_on_startup: true,
            hide_on_focus_loss: true,
            show_advanced_details: false,
            show_advanced_launch_entries: false,
            show_results_on_empty_query: true,
            fuzzy_search: true,
            shortcuts: vec![ShortcutConfig {
                name: "Search with Google".into(),
                prefix: "g ".into(),
                placeholder: String::new(),
                shortcut_type: "url".into(),
                mode: "execute".into(),
                template: "https://www.google.com/search?q={query}".into(),
                target: String::new(),
                arguments: String::new(),
                description: String::new(),
            }],
            plugins: BTreeMap::new(),
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
            && self.renderer != "femtovg-wgpu"
            && self.renderer != "skia"
            && self.renderer != "skia-opengl"
            && self.renderer != "software"
        {
            self.renderer = DEFAULT_RENDERER.into();
        }
        self.window_width = self.window_width.clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_WIDTH);
        self.window_height = self.window_height.max(MIN_WINDOW_HEIGHT);
        self.list_item_height = self
            .list_item_height
            .clamp(MIN_LIST_ITEM_HEIGHT, MAX_LIST_ITEM_HEIGHT);
        self.grid_item_size = self
            .grid_item_size
            .clamp(MIN_GRID_ITEM_SIZE, MAX_GRID_ITEM_SIZE);
        if self.theme != "system" && self.theme != "light" && self.theme != "dark" {
            self.theme = DEFAULT_THEME.into();
        }
        if self.language != "system" && !crate::utils::i18n::is_supported_language(&self.language) {
            self.language = DEFAULT_LANGUAGE.into();
        }
        for shortcut in &mut self.shortcuts {
            shortcut.name = shortcut.name.trim().to_string();
            shortcut.prefix = normalize_shortcut_prefix(&shortcut.prefix);
            shortcut.placeholder = shortcut.placeholder.trim().to_string();
            shortcut.shortcut_type = shortcut.shortcut_type.trim().to_ascii_lowercase();
            if shortcut.shortcut_type.is_empty() {
                shortcut.shortcut_type = "url".into();
            }
            shortcut.mode = shortcut.mode.trim().to_ascii_lowercase();
            if !matches!(shortcut.mode.as_str(), "execute" | "search" | "panel") {
                shortcut.mode = default_shortcut_mode();
            }
            shortcut.template = shortcut.template.trim().to_string();
            shortcut.target = shortcut.target.trim().to_string();
            shortcut.arguments = shortcut.arguments.trim().to_string();
            shortcut.description = shortcut.description.trim().to_string();
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
        // A prefix selects exactly one custom result/search engine. Keeping
        // the first declaration makes hot reload deterministic and prevents
        // ambiguous shortcut routing or unstable result identities.
        let mut prefixes = HashSet::new();
        self.shortcuts
            .retain(|shortcut| prefixes.insert(shortcut.prefix.clone()));
        self.plugins = std::mem::take(&mut self.plugins)
            .into_iter()
            .filter_map(|(id, mut plugin)| {
                let id = id.trim().to_ascii_lowercase();
                if id.is_empty() {
                    return None;
                }
                plugin.weight = plugin.weight.clamp(-1000, 1000);
                Some((id, plugin))
            })
            .collect();
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

/// Shortcut prefixes are matched literally by the provider. Keep a trailing
/// space in configured prefixes so a prefix such as `g` does not accidentally
/// match the beginning of a regular word such as `git`.
fn normalize_shortcut_prefix(prefix: &str) -> String {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix} ")
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
        assert_eq!(config.window_height, DEFAULT_WINDOW_HEIGHT);
        assert_eq!(config.list_item_height, DEFAULT_LIST_ITEM_HEIGHT);
        assert_eq!(config.grid_item_size, DEFAULT_GRID_ITEM_SIZE);
        assert!(config.show_on_startup);
        assert!(config.hide_on_focus_loss);
        assert!(!config.show_advanced_details);
        assert!(!config.show_advanced_launch_entries);
        assert!(config.fuzzy_search);
        assert!(config
            .shortcuts
            .iter()
            .any(|shortcut| { shortcut.prefix == "g " && shortcut.name == "Search with Google" }));
        assert!(config.providers.desktop);
        assert!(config.providers.indexed_directories.is_empty());
        assert!(config
            .providers
            .indexed_extensions
            .iter()
            .any(|extension| extension == "exe"));
        assert!(config.plugins.is_empty());
    }

    #[test]
    fn supported_renderers_are_preserved_by_normalization() {
        for renderer in ["femtovg", "femtovg-wgpu", "skia", "skia-opengl", "software"] {
            let mut config = Config {
                renderer: renderer.into(),
                ..Config::default()
            };
            config.normalize();
            assert_eq!(config.renderer, renderer);
        }
    }

    #[test]
    fn item_sizes_are_normalized_to_safe_layout_ranges() {
        let mut config = Config {
            list_item_height: 0,
            grid_item_size: u32::MAX,
            ..Config::default()
        };

        config.normalize();

        assert_eq!(config.list_item_height, MIN_LIST_ITEM_HEIGHT);
        assert_eq!(config.grid_item_size, MAX_GRID_ITEM_SIZE);
    }

    #[test]
    fn plugin_settings_are_normalized() {
        let mut config = Config::default();
        config.plugins.insert(
            " GitHub ".into(),
            PluginConfig {
                enabled: false,
                weight: 5000,
                preferences: BTreeMap::new(),
            },
        );
        config.normalize();
        let settings = config.plugins.get("github").unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.weight, 1000);
    }

    #[test]
    fn shortcut_prefixes_are_normalized_with_one_trailing_space() {
        let mut config = Config {
            shortcuts: vec![ShortcutConfig {
                name: "Search".into(),
                prefix: "  g  ".into(),
                placeholder: String::new(),
                shortcut_type: "url".into(),
                mode: "execute".into(),
                template: "https://example.com/?q={query}".into(),
                target: String::new(),
                arguments: String::new(),
                description: String::new(),
            }],
            ..Config::default()
        };

        config.normalize();

        assert_eq!(config.shortcuts[0].prefix, "g ");
    }

    #[test]
    fn shortcut_modes_are_normalized_and_legacy_configs_default_to_execute() {
        let mut config: Config = serde_json::from_str(
            r#"{
                "shortcuts": [
                    {"name":"Search","prefix":"s","type":"url","template":"https://example.com/?q={query}","mode":"search"},
                    {"name":"Panel","prefix":"p","type":"url","template":"https://example.com/?q={query}","mode":"panel"},
                    {"name":"Legacy","prefix":"l","type":"url","template":"https://example.com/?q={query}"},
                    {"name":"Invalid","prefix":"i","type":"url","template":"https://example.com/?q={query}","mode":"unknown"}
                ]
            }"#,
        )
        .expect("shortcut config JSON");

        config.normalize();

        assert_eq!(config.shortcuts[0].mode, "search");
        assert_eq!(config.shortcuts[1].mode, "panel");
        assert_eq!(config.shortcuts[2].mode, "execute");
        assert_eq!(config.shortcuts[3].mode, "execute");
    }
}
