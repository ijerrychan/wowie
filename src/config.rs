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
const DEFAULT_FONT_SIZE: u32 = 14;
const MIN_WINDOW_WIDTH: u32 = 420;
pub(crate) const MIN_WINDOW_HEIGHT: u32 = 350;
const MAX_WINDOW_WIDTH: u32 = 1200;
const MIN_LIST_ITEM_HEIGHT: u32 = 36;
const MAX_LIST_ITEM_HEIGHT: u32 = 96;
const MIN_FONT_SIZE: u32 = 11;
const MAX_FONT_SIZE: u32 = 20;
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
    pub font_family: String,
    pub font_size: u32,
    pub theme: String,
    pub language: String,
    pub show_on_startup: bool,
    pub hide_on_focus_loss: bool,
    pub show_advanced_details: bool,
    pub show_advanced_launch_entries: bool,
    pub compact_search: bool,
    pub separate_launch_entries: bool,
    /// Ordered result category IDs. Earlier categories win when match strength is equal.
    pub result_order: Vec<String>,
    /// Ordered plugin IDs. Earlier plugins win when match strength is equal.
    pub plugin_order: Vec<String>,
    /// Legacy numeric category adjustments, migrated to `result_order` on load.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub result_priorities: BTreeMap<String, i32>,
    pub fuzzy_search: bool,
    pub shortcuts: Vec<ShortcutConfig>,
    pub shortcut_keywords: ShortcutKeywords,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ShortcutKeywords {
    pub web_search: String,
    pub command: String,
}

impl ShortcutKeywords {
    pub fn web_search_prefix(&self) -> String {
        format!("{} ", self.web_search.trim())
    }

    pub fn command_prefix(&self) -> String {
        format!("{} ", self.command.trim())
    }

    fn normalize(&mut self) {
        self.web_search = normalize_shortcut_keyword(&self.web_search, "w");
        self.command = normalize_shortcut_keyword(&self.command, ">");
        if self.web_search.eq_ignore_ascii_case(&self.command) {
            *self = Self::default();
        }
    }
}

impl Default for ShortcutKeywords {
    fn default() -> Self {
        Self {
            web_search: "w".into(),
            command: ">".into(),
        }
    }
}

fn normalize_shortcut_keyword(value: &str, default: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        default.into()
    } else {
        value.into()
    }
}

fn default_shortcut_mode() -> String {
    "execute".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PluginConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub weight: i32,
    pub preferences: BTreeMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub command_action_words: BTreeMap<String, Vec<String>>,
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            weight: 0,
            preferences: BTreeMap::new(),
            command_action_words: BTreeMap::new(),
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
            font_family: String::new(),
            font_size: DEFAULT_FONT_SIZE,
            theme: DEFAULT_THEME.into(),
            language: DEFAULT_LANGUAGE.into(),
            show_on_startup: true,
            hide_on_focus_loss: true,
            show_advanced_details: false,
            show_advanced_launch_entries: false,
            compact_search: false,
            separate_launch_entries: false,
            result_order: default_result_order(),
            plugin_order: Vec::new(),
            result_priorities: BTreeMap::new(),
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
            shortcut_keywords: ShortcutKeywords::default(),
            plugins: BTreeMap::new(),
            providers: ProviderConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> (Self, bool) {
        match Self::load_for_update() {
            Ok(config) => (config, true),
            Err(error) => {
                eprintln!("could not load configuration: {error:#}; using defaults");
                (Self::default(), false)
            }
        }
    }

    /// Loading for an edit must fail on unreadable or invalid existing data,
    /// so an unrelated setting change cannot overwrite the user's file.
    pub fn load_for_update() -> anyhow::Result<Self> {
        Self::load_from_path(&config_path())
    }

    fn load_from_path(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).context(format!("read configuration {}", path.display()))
            }
        };
        let mut config: Self = serde_json::from_str(&text)
            .context(format!("parse configuration {}", path.display()))?;
        config.normalize();
        Ok(config)
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
        self.font_family = self.font_family.trim().to_owned();
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if self.theme != "system" && self.theme != "light" && self.theme != "dark" {
            self.theme = DEFAULT_THEME.into();
        }
        if self.language != "system" && !crate::utils::i18n::is_supported_language(&self.language) {
            self.language = DEFAULT_LANGUAGE.into();
        }
        self.shortcut_keywords.normalize();
        if !self.result_priorities.is_empty() {
            let mut old_priorities = std::mem::take(&mut self.result_priorities);
            let mut categories = default_result_order();
            categories.sort_by_key(|category| {
                let base = match category.as_str() {
                    "calculator" => 35,
                    "application" => 30,
                    "control_panel" => 20,
                    "settings" => 19,
                    "game" => 15,
                    "document" => 5,
                    "website" => -10,
                    _ => 0,
                };
                std::cmp::Reverse(
                    base + old_priorities
                        .remove(category)
                        .unwrap_or_default()
                        .clamp(-100, 100),
                )
            });
            self.result_order = categories;
        }
        let valid_result_order = default_result_order();
        self.result_order = normalize_order(&self.result_order, &valid_result_order);
        self.result_order
            .retain(|category| valid_result_order.contains(category));
        let migrate_plugin_weights = self.plugins.values().any(|plugin| plugin.weight != 0);
        if self.plugin_order.is_empty() && migrate_plugin_weights {
            let mut weighted = self.plugins.iter().collect::<Vec<_>>();
            weighted.sort_by(|(left_id, left), (right_id, right)| {
                right
                    .weight
                    .cmp(&left.weight)
                    .then_with(|| left_id.cmp(right_id))
            });
            self.plugin_order = weighted.into_iter().map(|(id, _)| id.clone()).collect();
        }
        self.plugin_order = normalize_order(&self.plugin_order, &[]);
        self.result_priorities.clear();
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
                // Legacy numeric plugin weights are migrated into the ordered
                // `plugin_order` list above and no longer act as hidden bias.
                plugin.weight = 0;
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
        self.save_to_path(&config_path())
    }

    fn save_to_path(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create config directory")?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize config")?;
        crate::utils::atomic_write(path, format!("{json}\n").as_bytes()).context("write config")
    }
}

pub(crate) fn default_result_order() -> Vec<String> {
    [
        "calculator",
        "application",
        "control_panel",
        "settings",
        "game",
        "document",
        "other",
        "unclassified",
        "website",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn normalize_order(values: &[String], defaults: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .iter()
        .chain(defaults)
        .filter_map(|value| {
            let value = value.trim().to_ascii_lowercase();
            (!value.is_empty() && seen.insert(value.clone())).then_some(value)
        })
        .collect()
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
    fn invalid_existing_config_is_preserved_and_valid_writes_replace_it() {
        let directory = std::env::temp_dir().join(format!(
            "wowie-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("config.json");
        assert_eq!(
            Config::load_from_path(&path).unwrap().hotkey,
            DEFAULT_HOTKEY
        );
        std::fs::create_dir_all(&directory).unwrap();
        let invalid_fixture = include_bytes!("../tests/fixtures/windows/invalid-config.json");
        std::fs::write(&path, invalid_fixture).unwrap();
        assert!(Config::load_from_path(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), invalid_fixture);

        let config = Config {
            hotkey: "Ctrl+Space".into(),
            ..Config::default()
        };
        config.save_to_path(&path).unwrap();
        assert_eq!(Config::load_from_path(&path).unwrap().hotkey, "Ctrl+Space");
        assert!(!directory.join("config.json.tmp").exists());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn defaults_are_stable() {
        let config = Config::default();
        assert_eq!(config.hotkey, "Alt+Space");
        assert_eq!(config.renderer, "femtovg");
        assert_eq!(config.window_height, DEFAULT_WINDOW_HEIGHT);
        assert_eq!(config.list_item_height, DEFAULT_LIST_ITEM_HEIGHT);
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
            ..Config::default()
        };

        config.normalize();

        assert_eq!(config.list_item_height, MIN_LIST_ITEM_HEIGHT);
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
                command_action_words: BTreeMap::new(),
            },
        );
        config.normalize();
        let settings = config.plugins.get("github").unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.weight, 0);
        assert_eq!(config.plugin_order, ["github"]);
        config.normalize();
        assert_eq!(config.plugin_order, ["github"]);
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
