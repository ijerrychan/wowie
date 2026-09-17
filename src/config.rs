use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: String,
    pub renderer: String,
    pub window_width: u32,
    pub max_results: usize,
    pub theme: String,
    pub language: String,
    pub show_on_startup: bool,
    pub hide_on_focus_loss: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Alt+Space".into(),
            renderer: "skia".into(),
            window_width: 680,
            max_results: 12,
            theme: "system".into(),
            language: "system".into(),
            show_on_startup: true,
            hide_on_focus_loss: true,
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
            self.hotkey = Self::default().hotkey;
        }
        if self.renderer != "skia" && self.renderer != "software" {
            self.renderer = "skia".into();
        }
        self.window_width = self.window_width.clamp(420, 1200);
        self.max_results = self.max_results.clamp(1, 50);
        if self.theme != "system" && self.theme != "light" && self.theme != "dark" {
            self.theme = "system".into();
        }
        if self.language != "system" && !crate::i18n::is_supported_language(&self.language) {
            self.language = "system".into();
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_stable() {
        let config = Config::default();
        assert_eq!(config.hotkey, "Alt+Space");
        assert_eq!(config.max_results, 12);
        assert!(config.show_on_startup);
        assert!(config.hide_on_focus_loss);
    }
}
