use super::*;

fn current_language() -> String {
    config::Config::load().0.language
}

fn load_editable_config(settings: &slint::Weak<SettingsWindow>) -> Option<config::Config> {
    match config::Config::load_for_update() {
        Ok(config) => Some(config),
        Err(error) => {
            eprintln!("could not load settings for editing: {error:#}");
            if let Some(settings) = settings.upgrade() {
                let message = settings::localized_error(&current_language(), &error.to_string());
                settings.set_status_message(message.into());
            }
            None
        }
    }
}

pub(super) fn register_settings_callbacks(
    settings_window: &SettingsWindow,
    window: &Launcher,
    plugin_settings: Vec<plugin::PluginSettingsDefinition>,
    platform: Arc<dyn platform::PlatformIntegration>,
    animations_enabled: Arc<AtomicBool>,
) {
    {
        let settings = settings_window.as_weak();
        let plugin_settings = plugin_settings.clone();
        settings_window.on_category_selected(move |category| {
            let category = category.to_string();
            let (config, _) = config::Config::load();
            if let Some(settings) = settings.upgrade() {
                settings.set_status_message(String::new().into());
                settings.set_selected_category(category.clone().into());
                settings.set_selected_category_title(
                    settings::category_title(&category, &config.language, &plugin_settings).into(),
                );
                settings.set_settings_scroll_y(0.0);
                settings::update_entries(
                    &settings,
                    settings::entries(&config, &plugin_settings, &category, &config.language),
                );
            }
        });
    }
    {
        let settings_for_change = settings_window.as_weak();
        let window_for_typography = window.as_weak();
        let plugin_settings = plugin_settings.clone();
        let plugin_settings_for_drag = plugin_settings.clone();
        let platform_for_theme = Arc::clone(&platform);
        let animations_for_theme = Arc::clone(&animations_enabled);
        settings_window.on_setting_changed(move |id, value| {
            let Some(mut config) = load_editable_config(&settings_for_change) else {
                return;
            };
            let previous_run_at_sign_in = config.run_at_sign_in;
            let setting_result = settings::apply(&mut config, &plugin_settings, &id, &value)
                .and_then(|()| {
                    if id.as_str() == "run_at_sign_in" {
                        platform::set_run_at_sign_in(config.run_at_sign_in)?;
                    }
                    if let Err(error) = config.save() {
                        if id.as_str() == "run_at_sign_in" {
                            if let Err(rollback_error) =
                                platform::set_run_at_sign_in(previous_run_at_sign_in)
                            {
                                eprintln!(
                                    "could not roll back Windows sign-in startup setting: {rollback_error:#}"
                                );
                            }
                        }
                        return Err(error);
                    }
                    Ok(())
                });
            if let Err(error) = setting_result {
                if let Some(settings) = settings_for_change.upgrade() {
                    let message = settings::localized_error(&config.language, &error.to_string());
                    settings.set_status_message(message.into());
                }
                eprintln!("could not save setting {id}: {error:#}");
                return;
            }
            let (updated, valid) = config::Config::load();
            if valid {
                // Apply a language change immediately for every Slint string
                // instead of waiting for the file watcher to deliver another
                // reload.
                if id.as_str() == "language" {
                    let slint_language = crate::utils::i18n::effective_language(&updated.language);
                    let _ = slint::select_bundled_translation(&slint_language);
                }
                if let Err(error) = updated.save() {
                    eprintln!("could not normalize settings: {error:#}");
                }
            }
            animations_for_theme.store(updated.animations_enabled, Ordering::Release);
            let dark_mode = match updated.theme.as_str() {
                "dark" => true,
                "light" => false,
                _ => platform_for_theme.system_prefers_dark_theme(),
            };
            let window_handle = window_for_typography.upgrade();
            let settings_handle = settings_for_change.upgrade();
            super::apply_theme_to_windows(
                dark_mode,
                &updated,
                platform_for_theme.as_ref(),
                window_handle.as_ref(),
                settings_handle.as_ref(),
            );
            let ui_font_family = settings::resolved_ui_font_family(&updated.font_family);
            if let Some(window) = window_handle.as_ref() {
                window.set_ui_font_family(ui_font_family.clone().into());
                window.set_ui_font_size(updated.font_size as f32);
            }
            if let Some(settings) = settings_handle.as_ref() {
                settings.set_ui_font_family(ui_font_family.into());
                settings.set_ui_font_size(updated.font_size as f32);
                settings::refresh(settings, &updated, &plugin_settings, &updated.language);
                settings.set_status_message(String::new().into());
            }
        });
        settings_window.on_priority_drag_can_drop(|data, target| {
            data.plain_text()
                .ok()
                .is_some_and(|source| settings::can_move_priority(source.as_str(), target.as_str()))
        });
        let settings_for_priority_drop = settings_window.as_weak();
        settings_window.on_priority_dropped(move |data, target, after| {
            let source = match data.plain_text() {
                Ok(source) => source.to_string(),
                Err(error) => {
                    if let Some(settings) = settings_for_priority_drop.upgrade() {
                        let message =
                            settings::localized_error(&current_language(), &error.to_string());
                        settings.set_status_message(message.into());
                    }
                    return;
                }
            };
            let Some(mut config) = load_editable_config(&settings_for_priority_drop) else {
                return;
            };
            let result = settings::move_priority(
                &mut config,
                &plugin_settings_for_drag,
                &source,
                target.as_str(),
                after,
            )
            .and_then(|()| config.save());
            if let Some(settings) = settings_for_priority_drop.upgrade() {
                match result {
                    Ok(()) => {
                        let (config, _) = config::Config::load();
                        settings::refresh(
                            &settings,
                            &config,
                            &plugin_settings_for_drag,
                            &config.language,
                        );
                        settings.set_status_message(String::new().into());
                    }
                    Err(error) => {
                        let message =
                            settings::localized_error(&config.language, &error.to_string());
                        settings.set_status_message(message.into());
                    }
                }
            }
        });
        let settings_for_close = settings_window.as_weak();
        settings_window.on_dismiss_requested(move || {
            if let Some(settings) = settings_for_close.upgrade() {
                let _ = settings.hide();
            }
        });
    }
    {
        let settings = settings_window.as_weak();
        let plugin_settings = plugin_settings.clone();
        settings_window.on_shortcut_saved(move |index, shortcut: SettingsShortcut| {
            let Some(mut config) = load_editable_config(&settings) else {
                return;
            };
            let result = settings::save_shortcut(&mut config, index, shortcut, &plugin_settings)
                .and_then(|()| config.save());
            if let Some(window) = settings.upgrade() {
                match result {
                    Ok(()) => {
                        let (config, _) = config::Config::load();
                        settings::refresh(&window, &config, &plugin_settings, &config.language);
                        window.set_status_message(String::new().into());
                    }
                    Err(error) => {
                        let message =
                            settings::localized_error(&config.language, &error.to_string());
                        window.set_status_message(message.into());
                    }
                }
            }
        });
    }
    {
        let settings = settings_window.as_weak();
        let plugin_settings = plugin_settings.clone();
        settings_window.on_shortcut_removed(move |index| {
            let Some(mut config) = load_editable_config(&settings) else {
                return;
            };
            let result = settings::remove_shortcut(&mut config, index).and_then(|()| config.save());
            if let Some(window) = settings.upgrade() {
                match result {
                    Ok(()) => {
                        let (config, _) = config::Config::load();
                        settings::refresh(&window, &config, &plugin_settings, &config.language);
                        window.set_status_message(String::new().into());
                    }
                    Err(error) => {
                        let message =
                            settings::localized_error(&config.language, &error.to_string());
                        window.set_status_message(message.into());
                    }
                }
            }
        });
    }
    {
        let settings = settings_window.as_weak();
        let plugin_settings = plugin_settings.clone();
        settings_window.on_directory_browse_requested(move |id| {
            let Some(window) = settings.upgrade() else {
                return;
            };
            let path = match platform::pick_folder(window.window()) {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(error) => {
                    let language = current_language();
                    let prefix = crate::utils::i18n::tr_owned_in_context(
                        &language,
                        "Settings",
                        "Could not open the folder picker",
                    );
                    window.set_status_message(format!("{prefix}: {error}").into());
                    return;
                }
            };
            let Some(mut config) = load_editable_config(&settings) else {
                return;
            };
            let result =
                settings::set_directory(&mut config, &id, path).and_then(|()| config.save());
            match result {
                Ok(()) => {
                    let (config, _) = config::Config::load();
                    settings::refresh(&window, &config, &plugin_settings, &config.language);
                    window.set_status_message(String::new().into());
                }
                Err(error) => {
                    let message = settings::localized_error(&config.language, &error.to_string());
                    window.set_status_message(message.into());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::settings;

    #[test]
    fn invalid_setting_keeps_the_existing_value() {
        let mut config = crate::config::Config::default();
        let previous_size = config.font_size;
        assert!(settings::apply(&mut config, &[], "font_size", "not-a-number").is_err());
        assert_eq!(config.font_size, previous_size);
    }

    #[test]
    fn accent_setting_rejects_invalid_values_and_canonicalizes_valid_ones() {
        let mut config = crate::config::Config::default();
        assert!(settings::apply(&mut config, &[], "accent_color", "blue").is_err());
        assert_eq!(config.accent_color, "");
        settings::apply(&mut config, &[], "accent_color", "#6260A8").unwrap();
        assert_eq!(config.accent_color, "#6260a8");
        settings::apply(&mut config, &[], "accent_color", "SYSTEM").unwrap();
        assert_eq!(config.accent_color, "system");
        settings::apply(&mut config, &[], "accent_color", "").unwrap();
        assert_eq!(config.accent_color, "");
    }
}
