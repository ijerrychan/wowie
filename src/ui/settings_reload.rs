use super::*;
use std::{path::PathBuf, sync::mpsc::Sender};

pub(super) fn apply_if_valid(valid: bool, apply: impl FnOnce()) -> bool {
    if !valid {
        return false;
    }
    apply();
    true
}

/// Owns the settings reload inputs used by the provider worker. Keeping the
/// reload path here leaves the window lifecycle responsible for receiving the
/// file watcher event and refreshing search results.
pub(super) struct SettingsReloadContext {
    pub(super) engine: Arc<search::SearchCore>,
    pub(super) hotkey_updates: Sender<String>,
    pub(super) tray_updates: Sender<platform::TrayMenuLabels>,
    pub(super) follow_system_theme: Arc<AtomicBool>,
    pub(super) hide_on_focus_loss: Arc<AtomicBool>,
    pub(super) animations_enabled: Arc<AtomicBool>,
    pub(super) custom_directories: Arc<Mutex<Vec<PathBuf>>>,
    pub(super) show_advanced_details: Arc<AtomicBool>,
    pub(super) show_advanced_launch_entries: Arc<AtomicBool>,
    pub(super) platform: Arc<dyn platform::PlatformIntegration>,
    pub(super) window: slint::Weak<Launcher>,
    pub(super) settings_window: slint::Weak<SettingsWindow>,
    pub(super) plugin_settings: Vec<plugin::PluginSettingsDefinition>,
    pub(super) current_query: Arc<Mutex<String>>,
    pub(super) current_language: Arc<Mutex<String>>,
}

impl SettingsReloadContext {
    pub(super) fn apply(&self, updated: &config::Config, provider: &mut AppProvider) {
        let _ = self.hotkey_updates.send(updated.hotkey.clone());
        let _ = self.tray_updates.send(platform::TrayMenuLabels {
            settings: i18n::tr(&updated.language, "Settings").to_owned(),
            exit: i18n::tr(&updated.language, "Exit").to_owned(),
        });
        self.follow_system_theme
            .store(updated.theme == "system", Ordering::Release);
        self.hide_on_focus_loss
            .store(updated.hide_on_focus_loss, Ordering::Release);
        self.animations_enabled
            .store(updated.animations_enabled, Ordering::Release);
        if let Ok(mut directories) = self.custom_directories.lock() {
            *directories = updated.providers.indexed_directories.clone();
        }
        provider.apply_config(&updated.providers);
        self.engine
            .replace_shortcut_config(updated.shortcuts.clone(), updated.shortcut_keywords.clone());
        self.engine.replace_fuzzy_search(updated.fuzzy_search);
        self.engine
            .replace_separate_launch_entries(updated.separate_launch_entries);
        self.engine
            .replace_show_advanced_launch_entries(updated.show_advanced_launch_entries);
        self.engine.replace_file_search_preferences(
            &updated.file_search_backend,
            updated.file_search_enabled,
            &updated.file_search_keyword,
        );
        let plugin_order = settings::visible_plugin_order(updated, &self.plugin_settings);
        self.engine
            .replace_priority_order(&updated.result_order, &plugin_order);
        self.engine
            .replace_provider_language(updated.language.clone());
        self.engine.replace_plugin_config(&updated.plugins);
        self.show_advanced_details
            .store(updated.show_advanced_details, Ordering::Release);
        self.show_advanced_launch_entries
            .store(updated.show_advanced_launch_entries, Ordering::Release);

        let window = self.window.clone();
        let settings_window = self.settings_window.clone();
        let plugin_settings = self.plugin_settings.clone();
        let updated = updated.clone();
        let platform = Arc::clone(&self.platform);
        let current_query = Arc::clone(&self.current_query);
        let current_language = Arc::clone(&self.current_language);
        let _ = slint::invoke_from_event_loop(move || {
            if let Ok(mut language) = current_language.lock() {
                *language = updated.language.clone();
            }
            let slint_language = i18n::effective_language(&updated.language);
            let _ = slint::select_bundled_translation(&slint_language);
            let dark_mode = match updated.theme.as_str() {
                "dark" => true,
                "light" => false,
                _ => platform.system_prefers_dark_theme(),
            };
            let window = window.upgrade();
            let settings = settings_window.upgrade();
            super::apply_theme_to_windows(
                dark_mode,
                &updated,
                platform.as_ref(),
                window.as_ref(),
                settings.as_ref(),
            );
            let ui_font_family = settings::resolved_ui_font_family(&updated.font_family);
            if let Some(window) = window {
                window.set_ui_font_family(ui_font_family.clone().into());
                window.set_ui_font_size(updated.font_size as f32);
                window.set_compact_search(updated.compact_search);
                if updated.compact_search
                    && current_query
                        .lock()
                        .map(|query| query.trim().is_empty())
                        .unwrap_or(false)
                    && !window.get_shortcut_session_active()
                {
                    window.set_empty_query_results_visible(false);
                }
                window.set_window_width(updated.window_width as i32);
                window.set_result_window_height(updated.window_height as f32);
                if super::restore_result_window_height(&window) {
                    platform.position_palette(window.window(), window.get_result_window_height());
                }
                window.set_list_item_height(updated.list_item_height as f32);
            }
            if let Some(settings) = settings {
                settings.set_ui_font_family(ui_font_family.into());
                settings.set_ui_font_size(updated.font_size as f32);
                settings::refresh(&settings, &updated, &plugin_settings, &updated.language);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::apply_if_valid;

    #[test]
    fn invalid_config_does_not_apply_runtime_changes() {
        let mut applied = false;
        assert!(!apply_if_valid(false, || applied = true));
        assert!(!applied);
        assert!(apply_if_valid(true, || applied = true));
        assert!(applied);
    }
}
