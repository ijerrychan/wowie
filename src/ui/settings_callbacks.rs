use super::*;

fn load_editable_config(settings: &slint::Weak<SettingsWindow>) -> Option<config::Config> {
    match config::Config::load_for_update() {
        Ok(config) => Some(config),
        Err(error) => {
            eprintln!("could not load settings for editing: {error:#}");
            if let Some(settings) = settings.upgrade() {
                settings.set_status_message(error.to_string().into());
            }
            None
        }
    }
}

pub(super) fn register_settings_callbacks(
    settings_window: &SettingsWindow,
    window: &Launcher,
    context_window: &ContextMenu,
    plugin_settings: Vec<plugin::PluginSettingsDefinition>,
) {
    {
        let settings = settings_window.as_weak();
        let plugin_settings = plugin_settings.clone();
        let transition_generation = Arc::new(AtomicU64::new(0));
        let transition_generation_for_callback = Arc::clone(&transition_generation);
        settings_window.on_category_selected(move |category| {
            let category = category.to_string();
            let (config, _) = config::Config::load();
            if let Some(settings) = settings.upgrade() {
                settings.set_page_transition_opacity(0.78);
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
                let generation =
                    transition_generation_for_callback.fetch_add(1, Ordering::AcqRel) + 1;
                let weak = settings.as_weak();
                let transition_generation = Arc::clone(&transition_generation_for_callback);
                Timer::single_shot(Duration::from_millis(75), move || {
                    if transition_generation.load(Ordering::Acquire) == generation {
                        if let Some(settings) = weak.upgrade() {
                            settings.set_page_transition_opacity(1.0);
                        }
                    }
                });
            }
        });
    }
    {
        let settings_for_change = settings_window.as_weak();
        let window_for_typography = window.as_weak();
        let context_for_typography = context_window.as_weak();
        let plugin_settings = plugin_settings.clone();
        let plugin_settings_for_drag = plugin_settings.clone();
        settings_window.on_setting_changed(move |id, value| {
            let Some(mut config) = load_editable_config(&settings_for_change) else {
                return;
            };
            let setting_result = settings::apply(&mut config, &plugin_settings, &id, &value)
                .and_then(|()| config.save());
            if let Err(error) = setting_result {
                if let Some(settings) = settings_for_change.upgrade() {
                    settings.set_status_message(error.to_string().into());
                }
                eprintln!("could not save setting {id}: {error:#}");
                return;
            }
            let (updated, valid) = config::Config::load();
            if valid {
                if let Err(error) = updated.save() {
                    eprintln!("could not normalize settings: {error:#}");
                }
            }
            if let Some(window) = window_for_typography.upgrade() {
                window.set_ui_font_family(updated.font_family.clone().into());
                window.set_ui_font_size(updated.font_size as f32);
            }
            if let Some(context) = context_for_typography.upgrade() {
                context.set_ui_font_family(updated.font_family.clone().into());
                context.set_ui_font_size(updated.font_size as f32);
            }
            if let Some(settings) = settings_for_change.upgrade() {
                settings.set_ui_font_family(updated.font_family.clone().into());
                settings.set_ui_font_size(updated.font_size as f32);
                settings::refresh(&settings, &updated, &plugin_settings, &updated.language);
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
                        settings.set_status_message(error.to_string().into());
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
                    Err(error) => settings.set_status_message(error.to_string().into()),
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
                    Err(error) => window.set_status_message(error.to_string().into()),
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
                    Err(error) => window.set_status_message(error.to_string().into()),
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
                    window.set_status_message(error.to_string().into());
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
                Err(error) => window.set_status_message(error.to_string().into()),
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
}
