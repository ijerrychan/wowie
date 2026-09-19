#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod classifier;
mod config;
mod details;
mod http;
mod i18n;
mod icon_service;
mod launcher;
mod platform;
mod plugin;
mod provider;
mod providers;
mod ranking;
mod result;
mod search;

use anyhow::Result;
use icon_service::IconService;
use providers::app::AppProvider;
use result::{DetailBlock as ResultDetailBlock, DetailView, ResultCategory, SearchResult};
use slint::{ComponentHandle, Model, ModelRc, Timer, VecModel};
use std::{
    collections::HashSet,
    rc::Rc,
    sync::{
        atomic::AtomicU64,
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

slint::include_modules!();

fn main() -> Result<()> {
    #[cfg(not(debug_assertions))]
    let _single_instance = match platform::try_acquire_single_instance()? {
        Some(instance) => instance,
        None => return Ok(()),
    };

    let (config, config_was_valid) = config::Config::load();
    let window = create_window(&config)?;
    let context_window = ContextMenu::new()?;
    if config.language != "system" {
        slint::select_bundled_translation(&config.language)
            .map_err(|error| anyhow::anyhow!("could not select language: {error}"))?;
    }
    let platform = platform::current();
    let tray_registration = platform.start_tray(i18n::tr(&config.language, "Exit").to_owned());
    let tray_updates = tray_registration.updates.clone();
    let dark_mode = match config.theme.as_str() {
        "dark" => true,
        "light" => false,
        _ => platform.system_prefers_dark_theme(),
    };
    if config_was_valid {
        if let Err(error) = config.save() {
            eprintln!("could not persist configuration: {error:#}");
        }
    }
    // Do not block process startup on Windows Shell/Steam enumeration. The
    // provider worker populates this empty snapshot after the UI is running.
    let mut provider = AppProvider::empty_with_config(&config.providers);
    let custom_directories = Arc::new(Mutex::new(config.providers.indexed_directories.clone()));
    let app_source_events = AppProvider::watch_sources(Arc::clone(&custom_directories));
    let config_events = config::watch_changes();
    let engine = search::SearchCore::with_plugins_and_shortcuts(
        provider.snapshot().as_ref().clone(),
        plugin::PluginManager::discover_with_config(&config.language, &config.plugins),
        config.shortcuts.clone(),
    );
    let engine = Arc::new(engine);
    engine.replace_fuzzy_search(config.fuzzy_search);
    engine.replace_provider_language(config.language.clone());
    let icon_service = IconService::new();
    let results = Rc::new(VecModel::<ResultRow>::default());
    let visible = Arc::new(AtomicBool::new(false));
    let context_visible = Arc::new(AtomicBool::new(false));
    let context_generation = Arc::new(AtomicU64::new(0));
    let context_item = Arc::new(Mutex::new(None::<SearchResult>));
    let show_advanced_details = Arc::new(AtomicBool::new(config.show_advanced_details));
    let show_advanced_launch_entries =
        Arc::new(AtomicBool::new(config.show_advanced_launch_entries));
    let follow_system_theme = Arc::new(AtomicBool::new(config.theme == "system"));
    let hide_on_focus_loss = Arc::new(AtomicBool::new(config.hide_on_focus_loss));
    let query_generation = Arc::new(AtomicU64::new(0));
    let detail_generation = Arc::new(AtomicU64::new(0));
    let detail_registry = details::DetailRegistry::default_providers();
    let current_query = Arc::new(Mutex::new(String::new()));
    // Keep one shared result snapshot for icon loading. Cloning the complete
    // result list on every visible-range notification caused avoidable memory
    // spikes while scrolling.
    let icon_items = Arc::new(Mutex::new((0_u64, Arc::new(Vec::<SearchResult>::new()))));
    let icon_requests = Arc::new(Mutex::new((0_u64, HashSet::<usize>::new())));

    window.set_results(ModelRc::from(results.clone()));
    window.set_window_width(config.window_width as i32);
    window.set_dark_mode(dark_mode);
    let accent = platform.system_accent_color();
    window.set_accent_color(slint::Color::from_rgb_u8(
        ((accent >> 16) & 0xff) as u8,
        ((accent >> 8) & 0xff) as u8,
        (accent & 0xff) as u8,
    ));
    window.set_accent_background(slint::Color::from_argb_u8(
        if dark_mode { 0x42 } else { 0x28 },
        ((accent >> 16) & 0xff) as u8,
        ((accent >> 8) & 0xff) as u8,
        (accent & 0xff) as u8,
    ));
    window.set_result_count(0);
    window.set_launch_error(String::new().into());
    context_window.set_dark_mode(dark_mode);
    context_window.set_accent_color(slint::Color::from_rgb_u8(
        ((accent >> 16) & 0xff) as u8,
        ((accent >> 8) & 0xff) as u8,
        (accent & 0xff) as u8,
    ));

    let hotkey_registration = platform.register_hotkey(&config.hotkey, true)?;
    let hotkey_updates = hotkey_registration.updates.clone();

    {
        let icon_items = Arc::clone(&icon_items);
        let icon_requests = Arc::clone(&icon_requests);
        let icon_service = icon_service.clone();
        let weak = window.as_weak();
        let query_generation = Arc::clone(&query_generation);
        window.on_visible_range_changed(move |start, end| {
            request_icons(
                start,
                end,
                &icon_items,
                &icon_requests,
                &icon_service,
                &weak,
                &query_generation,
            );
        });
    }

    {
        let engine = engine.clone();
        let weak = window.as_weak();
        let current_query = Arc::clone(&current_query);
        window.on_detail_action_requested(move |index| {
            let Some(window) = weak.upgrade() else {
                return;
            };
            let selected = window.get_selected_index();
            let Some(row) = (selected >= 0)
                .then(|| window.get_results().row_data(selected as usize))
                .flatten()
            else {
                return;
            };
            let Some(item) = engine.by_id(row.id.as_str()) else {
                return;
            };
            let Some(detail_action) = item
                .detail
                .as_ref()
                .and_then(|view| view.actions.get(index as usize))
            else {
                return;
            };
            match engine.execute_plugin_action(&item.id, &detail_action.id) {
                Ok(output) => {
                    let query = current_query
                        .lock()
                        .map(|query| query.clone())
                        .unwrap_or_default();
                    engine.record_success(&item.id, &query);
                    window.set_last_action_id(item.id.into());
                    window.set_last_action_result(output.title.into());
                    window.set_last_action_subtitle(output.subtitle.into());
                    window.set_launch_error(String::new().into());
                }
                Err(error) => window.set_launch_error(error.to_string().into()),
            }
        });
    }
    {
        let engine = engine.clone();
        let icon_service = icon_service.clone();
        let icon_items = Arc::clone(&icon_items);
        let icon_requests = Arc::clone(&icon_requests);
        let weak = window.as_weak();
        let query_generation = Arc::clone(&query_generation);
        let current_query = Arc::clone(&current_query);
        let show_advanced_details = Arc::clone(&show_advanced_details);
        let detail_registry = detail_registry.clone();
        let detail_language = config.language.clone();
        let detail_generation = Arc::clone(&detail_generation);
        window.on_query_changed(move |query| {
            // Invalidate an in-flight detail request immediately. The result
            // list for the new query may arrive slightly later on the worker
            // thread, so waiting for selection changes here can let stale
            // weather data briefly overwrite the new query's detail panel.
            detail_generation.fetch_add(1, Ordering::AcqRel);
            if let Ok(mut current) = current_query.lock() {
                *current = query.to_string();
            }
            if let Some(window) = weak.upgrade() {
                window.set_last_action_id(String::new().into());
                window.set_last_action_result(String::new().into());
                window.set_last_action_subtitle(String::new().into());
                window.set_launch_error(String::new().into());
                window.set_detail_panel_visible(false);
            }
            let query_id = query_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let engine = Arc::clone(&engine);
            let icon_service = icon_service.clone();
            let icon_items = Arc::clone(&icon_items);
            let icon_requests = Arc::clone(&icon_requests);
            let weak = weak.clone();
            let query_generation = Arc::clone(&query_generation);
            let detail_generation = Arc::clone(&detail_generation);
            let show_advanced_details_for_query = Arc::clone(&show_advanced_details);
            let detail_registry_for_query = detail_registry.clone();
            let detail_language_for_query = detail_language.clone();
            std::thread::spawn(move || {
                let found = if query.trim().is_empty() {
                    Vec::new()
                } else {
                    engine.search(query.as_str())
                };
                let rows = found
                    .iter()
                    .map(|item| {
                        ResultRowPayload::from(
                            item,
                            show_advanced_details_for_query.load(Ordering::Acquire),
                        )
                    })
                    .collect::<Vec<_>>();
                let detail_panel = found.first().is_some_and(|item| item.detail.is_some());
                let first_detail = found.first().and_then(|item| item.detail.clone());
                let found_for_icons = Arc::new(found);
                let _ = slint::invoke_from_event_loop(move || {
                    if query_generation.load(Ordering::Acquire) != query_id {
                        return;
                    }
                    if let Some(window) = weak.upgrade() {
                        if let Ok(mut state) = icon_items.lock() {
                            *state = (query_id, Arc::clone(&found_for_icons));
                        }
                        if let Ok(mut state) = icon_requests.lock() {
                            *state = (query_id, HashSet::new());
                        }
                        let model = Rc::new(VecModel::from(
                            rows.into_iter().map(ResultRow::from).collect::<Vec<_>>(),
                        ));
                        let count = model.row_count();
                        window.set_results(ModelRc::from(model));
                        window.set_result_count(count as i32);
                        let cached_detail = found_for_icons
                            .first()
                            .and_then(|item| detail_registry_for_query.provider_for(item))
                            .and_then(|provider| provider.cached(&detail_language_for_query));
                        set_detail_view(&window, cached_detail.as_ref().or(first_detail.as_ref()));
                        window.set_detail_panel_visible(detail_panel);
                        window.set_selected_index(-1);
                        window.set_selected_index(if count > 0 { 0 } else { -1 });
                        if let Some(item) = found_for_icons.first().cloned() {
                            request_dynamic_detail(
                                item,
                                detail_registry_for_query.clone(),
                                detail_language_for_query.clone(),
                                Arc::clone(&detail_generation),
                                weak.clone(),
                            );
                        }
                        request_icons(
                            0,
                            count.min(8) as i32,
                            &icon_items,
                            &icon_requests,
                            &icon_service,
                            &window.as_weak(),
                            &query_generation,
                        );
                    }
                });
            });
        });
    }
    {
        let engine = engine.clone();
        let weak = window.as_weak();
        let detail_generation = Arc::clone(&detail_generation);
        let detail_registry = detail_registry.clone();
        let language = config.language.clone();
        window.on_selection_changed(move |index| {
            let item = weak
                .upgrade()
                .and_then(|window| window.get_results().row_data(index as usize))
                .and_then(|row| engine.by_id(row.id.as_str()));
            if let Some(window) = weak.upgrade() {
                set_detail_view(&window, item.as_ref().and_then(|item| item.detail.as_ref()));
                window.set_detail_panel_visible(
                    item.as_ref().is_some_and(|item| item.detail.is_some()),
                );
            }
            let request_id = detail_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let Some(item) = item else { return };
            request_dynamic_detail_with_id(
                item,
                detail_registry.clone(),
                language.clone(),
                Arc::clone(&detail_generation),
                weak.clone(),
                request_id,
            );
        });
    }
    {
        let engine = engine.clone();
        let weak = window.as_weak();
        let visible = Arc::clone(&visible);
        let current_query = Arc::clone(&current_query);
        window.on_launch_requested(move |index| {
            let item = weak.upgrade().and_then(|window| {
                window
                    .get_results()
                    .row_data(index as usize)
                    .and_then(|row| engine.by_id(row.id.as_str()))
            });
            if let Some(item) = item {
                if item.category == ResultCategory::Shortcut && item.actions.is_empty() {
                    return;
                }
                if matches!(item.actions.first(), Some(result::Action::Plugin { .. })) {
                    match engine.execute_action(&item.id) {
                        Ok(output) => {
                            let query = current_query
                                .lock()
                                .map(|query| query.clone())
                                .unwrap_or_default();
                            engine.record_success(&item.id, &query);
                            if let Some(window) = weak.upgrade() {
                                window.set_last_action_id(item.id.clone().into());
                                window.set_last_action_result(output.title.into());
                                window.set_last_action_subtitle(output.subtitle.into());
                                window.set_launch_error(String::new().into());
                            }
                        }
                        Err(error) => {
                            eprintln!("plugin action failed: {error:#}");
                            if let Some(window) = weak.upgrade() {
                                window.set_launch_error(error.to_string().into());
                            }
                        }
                    }
                    return;
                }
                let result = launcher::launch(&item);
                match result {
                    Ok(()) => {
                        let query = current_query
                            .lock()
                            .map(|query| query.clone())
                            .unwrap_or_default();
                        engine.record_success(&item.id, &query);
                        if let Some(window) = weak.upgrade() {
                            window.set_launch_error(String::new().into());
                            let _ = window.hide();
                        }
                        visible.store(false, Ordering::Release);
                    }
                    Err(error) => {
                        eprintln!("launch failed: {error:#}");
                        if let Some(window) = weak.upgrade() {
                            window.set_launch_error(error.to_string().into());
                            let weak = window.as_weak();
                            Timer::single_shot(Duration::from_secs(5), move || {
                                if let Some(window) = weak.upgrade() {
                                    window.set_launch_error(String::new().into());
                                }
                            });
                        }
                    }
                }
            }
        });
    }
    {
        let engine = engine.clone();
        let weak = window.as_weak();
        let context_weak = context_window.as_weak();
        let platform = Arc::clone(&platform);
        let context_visible = Arc::clone(&context_visible);
        let context_generation = Arc::clone(&context_generation);
        let context_item = Arc::clone(&context_item);
        let visible = Arc::clone(&visible);
        let show_advanced_launch_entries = Arc::clone(&show_advanced_launch_entries);
        window.on_context_menu_requested(move |index| {
            let Some(item) = weak.upgrade().and_then(|window| {
                window
                    .get_results()
                    .row_data(index as usize)
                    .and_then(|row| engine.by_id(row.id.as_str()))
            }) else {
                return;
            };
            let actions =
                context_actions(&item, show_advanced_launch_entries.load(Ordering::Acquire));
            if actions.is_empty() {
                hide_context_menu(
                    &context_weak,
                    &weak,
                    &context_visible,
                    &context_generation,
                    &context_item,
                    false,
                    &visible,
                );
                return;
            }
            if let Ok(mut current) = context_item.lock() {
                *current = Some(item);
            }
            context_generation.fetch_add(1, Ordering::AcqRel);
            context_visible.store(true, Ordering::Release);
            if let Some(window) = weak.upgrade() {
                window.set_context_menu_open(true);
            }
            if let Some(menu) = context_weak.upgrade() {
                menu.set_actions(ModelRc::from(Rc::new(VecModel::from(actions))));
                platform.disable_window_transitions(menu.window());
                let _ = menu.show();
                platform.position_context_menu(menu.window());
                platform.activate_context_menu(menu.window());
            }
        });
    }
    {
        let weak = window.as_weak();
        let context_weak = context_window.as_weak();
        let context_visible = Arc::clone(&context_visible);
        let context_generation = Arc::clone(&context_generation);
        let context_item = Arc::clone(&context_item);
        let visible = Arc::clone(&visible);
        if let Some(menu) = context_weak.upgrade() {
            menu.on_dismissed(move || {
                hide_context_menu(
                    &context_weak,
                    &weak,
                    &context_visible,
                    &context_generation,
                    &context_item,
                    false,
                    &visible,
                );
            });
        }
    }
    {
        let weak = window.as_weak();
        let context_weak = context_window.as_weak();
        let context_visible = Arc::clone(&context_visible);
        let context_generation = Arc::clone(&context_generation);
        let context_item = Arc::clone(&context_item);
        let visible = Arc::clone(&visible);
        let engine = Arc::clone(&engine);
        context_window.on_action_requested(move |action_id| {
            let Some(item) = context_item.lock().ok().and_then(|current| current.clone()) else {
                return;
            };
            if action_id == "open-directory" {
                hide_context_menu(
                    &context_weak,
                    &weak,
                    &context_visible,
                    &context_generation,
                    &context_item,
                    false,
                    &visible,
                );
                let weak_for_result = weak.clone();
                std::thread::spawn(move || {
                    let result = launcher::open_containing_directory(&item);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Err(error) = result {
                            if let Some(window) = weak_for_result.upgrade() {
                                window.set_launch_error(error.to_string().into());
                            }
                        }
                    });
                });
                return;
            }
            if let Some(plugin_action_id) = action_id.strip_prefix("plugin-action:") {
                let result = engine.execute_plugin_action(&item.id, plugin_action_id);
                hide_context_menu(
                    &context_weak,
                    &weak,
                    &context_visible,
                    &context_generation,
                    &context_item,
                    false,
                    &visible,
                );
                if let Some(window) = weak.upgrade() {
                    match result {
                        Ok(output) => {
                            window.set_last_action_id(item.id.clone().into());
                            window.set_last_action_result(output.title.into());
                            window.set_last_action_subtitle(output.subtitle.into());
                            window.set_launch_error(String::new().into());
                        }
                        Err(error) => window.set_launch_error(error.to_string().into()),
                    }
                }
                return;
            }
            let result = match action_id.as_str() {
                "run-as-admin" => {
                    let entry = result::LaunchEntry {
                        source: item.source.clone(),
                        path: item.launch_target.clone(),
                        target: item.launch_target.clone(),
                        arguments: item.launch_arguments.clone(),
                        working_directory: item.launch_working_directory.clone(),
                    };
                    launcher::launch_entry(&entry, true)
                }
                value if value.starts_with("launch:") => value[7..]
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| item.launch_entries.get(index))
                    .map(|entry| launcher::launch_entry(entry, false))
                    .unwrap_or_else(|| anyhow::bail!("launch action is unavailable")),
                _ => Err(anyhow::anyhow!("unknown context action")),
            };
            hide_context_menu(
                &context_weak,
                &weak,
                &context_visible,
                &context_generation,
                &context_item,
                false,
                &visible,
            );
            if let Some(window) = weak.upgrade() {
                if let Err(error) = result {
                    window.set_launch_error(error.to_string().into());
                }
            }
        });
    }
    {
        let window = window.as_weak();
        let visible = Arc::clone(&visible);
        let context_weak = context_window.as_weak();
        let context_visible = Arc::clone(&context_visible);
        let context_generation = Arc::clone(&context_generation);
        let context_item = Arc::clone(&context_item);
        if let Some(handle) = window.upgrade() {
            handle.on_dismiss_requested(move || {
                hide_context_menu(
                    &context_weak,
                    &window,
                    &context_visible,
                    &context_generation,
                    &context_item,
                    false,
                    &visible,
                );
                if let Some(window) = window.upgrade() {
                    let _ = window.hide();
                }
                visible.store(false, Ordering::Release);
            });
        }
    }

    {
        let engine = Arc::clone(&engine);
        let icon_service = icon_service.clone();
        let icon_items = Arc::clone(&icon_items);
        let icon_requests = Arc::clone(&icon_requests);
        let weak = window.as_weak();
        let query_generation = Arc::clone(&query_generation);
        let current_query = Arc::clone(&current_query);
        let show_advanced_details = Arc::clone(&show_advanced_details);
        let show_advanced_launch_entries = Arc::clone(&show_advanced_launch_entries);
        let follow_system_theme = Arc::clone(&follow_system_theme);
        let hide_on_focus_loss = Arc::clone(&hide_on_focus_loss);
        let platform_for_config = Arc::clone(&platform);
        std::thread::spawn(move || {
            provider.discover_initial();
            let mut initial_refresh = true;
            loop {
                let mut changed = if initial_refresh {
                    initial_refresh = false;
                    true
                } else {
                    match app_source_events.recv_timeout(Duration::from_millis(200)) {
                        Ok(source) => provider.refresh_source(source),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                };
                if config_events.try_recv().is_ok() {
                    let (updated, valid) = config::Config::load();
                    if valid {
                        let _ = hotkey_updates.send(updated.hotkey.clone());
                        let _ = tray_updates.send(i18n::tr(&updated.language, "Exit").to_owned());
                        follow_system_theme.store(updated.theme == "system", Ordering::Release);
                        hide_on_focus_loss.store(updated.hide_on_focus_loss, Ordering::Release);
                        if let Ok(mut directories) = custom_directories.lock() {
                            *directories = updated.providers.indexed_directories.clone();
                        }
                        provider.apply_config(&updated.providers);
                        engine.replace_shortcuts(updated.shortcuts);
                        engine.replace_fuzzy_search(updated.fuzzy_search);
                        engine.replace_provider_language(updated.language.clone());
                        engine.replace_plugin_config(&updated.plugins);
                        show_advanced_details
                            .store(updated.show_advanced_details, Ordering::Release);
                        show_advanced_launch_entries
                            .store(updated.show_advanced_launch_entries, Ordering::Release);
                        let weak_for_config = weak.clone();
                        let platform_for_config = Arc::clone(&platform_for_config);
                        let theme = updated.theme.clone();
                        let language = updated.language.clone();
                        let window_width = updated.window_width as i32;
                        let _ = slint::invoke_from_event_loop(move || {
                            if language != "system" {
                                let _ = slint::select_bundled_translation(&language);
                            }
                            let dark_mode = match theme.as_str() {
                                "dark" => true,
                                "light" => false,
                                _ => platform_for_config.system_prefers_dark_theme(),
                            };
                            if let Some(window) = weak_for_config.upgrade() {
                                window.set_window_width(window_width);
                                window.set_dark_mode(dark_mode);
                                let accent = platform_for_config.system_accent_color();
                                window.set_accent_background(slint::Color::from_argb_u8(
                                    if dark_mode { 0x42 } else { 0x28 },
                                    ((accent >> 16) & 0xff) as u8,
                                    ((accent >> 8) & 0xff) as u8,
                                    (accent & 0xff) as u8,
                                ));
                            }
                        });
                        changed = true;
                    }
                }
                if changed {
                    let indexed = provider.snapshot();
                    engine.replace_results(indexed);
                    let query = current_query
                        .lock()
                        .map(|value| value.clone())
                        .unwrap_or_default();
                    let query_id = query_generation.fetch_add(1, Ordering::AcqRel) + 1;
                    let found = if query.trim().is_empty() {
                        Vec::new()
                    } else {
                        engine.search(&query)
                    };
                    let rows = found
                        .iter()
                        .map(|item| {
                            ResultRowPayload::from(
                                item,
                                show_advanced_details.load(Ordering::Acquire),
                            )
                        })
                        .collect::<Vec<_>>();
                    let detail_panel = found.first().is_some_and(|item| item.detail.is_some());
                    let first_detail = found.first().and_then(|item| item.detail.clone());
                    let found_for_icons = Arc::new(found);
                    let query_generation_for_ui = Arc::clone(&query_generation);
                    let weak_for_ui = weak.clone();
                    let icon_items_for_ui = Arc::clone(&icon_items);
                    let icon_requests_for_ui = Arc::clone(&icon_requests);
                    let icon_service_for_ui = icon_service.clone();
                    let query_generation_for_icons = Arc::clone(&query_generation);
                    let _ = slint::invoke_from_event_loop(move || {
                        if query_generation_for_ui.load(Ordering::Acquire) != query_id {
                            return;
                        }
                        if let Some(window) = weak_for_ui.upgrade() {
                            if let Ok(mut state) = icon_items_for_ui.lock() {
                                *state = (query_id, Arc::clone(&found_for_icons));
                            }
                            if let Ok(mut state) = icon_requests_for_ui.lock() {
                                *state = (query_id, HashSet::new());
                            }
                            let model = Rc::new(VecModel::from(
                                rows.into_iter().map(ResultRow::from).collect::<Vec<_>>(),
                            ));
                            let count = model.row_count();
                            window.set_results(ModelRc::from(model));
                            window.set_result_count(count as i32);
                            set_detail_view(&window, first_detail.as_ref());
                            window.set_detail_panel_visible(detail_panel);
                            window.set_selected_index(-1);
                            window.set_selected_index(if count > 0 { 0 } else { -1 });
                            request_icons(
                                0,
                                count.min(8) as i32,
                                &icon_items_for_ui,
                                &icon_requests_for_ui,
                                &icon_service_for_ui,
                                &window.as_weak(),
                                &query_generation_for_icons,
                            );
                        }
                    });
                }
            }
        });
    }

    {
        let weak = window.as_weak();
        let visible = Arc::clone(&visible);
        let context_visible = Arc::clone(&context_visible);
        let hide_on_focus_loss = Arc::clone(&hide_on_focus_loss);
        platform.install_focus_listener(
            window.window(),
            Box::new(move || {
                if context_visible.load(Ordering::Acquire) {
                    return;
                }
                if hide_on_focus_loss.load(Ordering::Acquire)
                    && visible.swap(false, Ordering::AcqRel)
                {
                    if let Some(window) = weak.upgrade() {
                        let _ = window.hide();
                    }
                }
            }),
        );
    }
    {
        let weak = window.as_weak();
        let context_weak = context_window.as_weak();
        let context_visible = Arc::clone(&context_visible);
        let context_generation = Arc::clone(&context_generation);
        let context_item = Arc::clone(&context_item);
        let visible = Arc::clone(&visible);
        let hide_on_focus_loss = Arc::clone(&hide_on_focus_loss);
        let platform_for_context_focus = Arc::clone(&platform);
        platform.install_focus_listener(
            context_window.window(),
            Box::new(move || {
                let context_weak = context_weak.clone();
                let context_visible = Arc::clone(&context_visible);
                let context_generation = Arc::clone(&context_generation);
                let context_item = Arc::clone(&context_item);
                let weak = weak.clone();
                let visible = Arc::clone(&visible);
                let platform = Arc::clone(&platform_for_context_focus);
                let hide_on_focus_loss = Arc::clone(&hide_on_focus_loss);
                let observed_generation = context_generation.load(Ordering::Acquire);
                Timer::single_shot(Duration::from_millis(30), move || {
                    if context_generation.load(Ordering::Acquire) == observed_generation {
                        let launcher_focused = weak
                            .upgrade()
                            .is_some_and(|launcher| platform.is_window_focused(launcher.window()));
                        hide_context_menu(
                            &context_weak,
                            &weak,
                            &context_visible,
                            &context_generation,
                            &context_item,
                            !launcher_focused && hide_on_focus_loss.load(Ordering::Acquire),
                            &visible,
                        );
                    }
                });
            }),
        );
    }
    if config.show_on_startup {
        platform.disable_window_transitions(window.window());
        window.show()?;
        platform.position_palette(window.window());
        platform.activate_palette(window.window());
        visible.store(true, Ordering::Release);
    }

    let (platform_event_sender, platform_event_receiver) = std::sync::mpsc::channel();
    {
        let sender = platform_event_sender.clone();
        std::thread::spawn(move || {
            for event in hotkey_registration.events {
                if sender.send(event).is_err() {
                    break;
                }
            }
        });
    }
    std::thread::spawn(move || {
        for event in tray_registration.events {
            if platform_event_sender.send(event).is_err() {
                break;
            }
        }
    });

    let weak = window.as_weak();
    let platform_for_hotkey = Arc::clone(&platform);
    let follow_system_theme_for_hotkey = Arc::clone(&follow_system_theme);
    let visible_for_hotkey = Arc::clone(&visible);
    let context_weak_for_hotkey = context_window.as_weak();
    let context_visible_for_hotkey = Arc::clone(&context_visible);
    let context_generation_for_hotkey = Arc::clone(&context_generation);
    let context_item_for_hotkey = Arc::clone(&context_item);
    std::thread::spawn(move || {
        for event in platform_event_receiver {
            let weak = weak.clone();
            let platform = Arc::clone(&platform_for_hotkey);
            let visible = Arc::clone(&visible_for_hotkey);
            let context_weak = context_weak_for_hotkey.clone();
            let context_visible = Arc::clone(&context_visible_for_hotkey);
            let context_generation = Arc::clone(&context_generation_for_hotkey);
            let context_item = Arc::clone(&context_item_for_hotkey);
            let follow_system_theme = Arc::clone(&follow_system_theme_for_hotkey);
            let _ = slint::invoke_from_event_loop(move || match event {
                platform::PlatformEvent::TogglePalette => {
                    hide_context_menu(
                        &context_weak,
                        &weak,
                        &context_visible,
                        &context_generation,
                        &context_item,
                        false,
                        &visible,
                    );
                    if let Some(window) = weak.upgrade() {
                        toggle_palette(&window, platform, &visible);
                    }
                }
                platform::PlatformEvent::SystemThemeChanged
                    if follow_system_theme.load(Ordering::Acquire) =>
                {
                    if let Some(window) = weak.upgrade() {
                        window.set_dark_mode(platform.system_prefers_dark_theme());
                    }
                }
                platform::PlatformEvent::SystemThemeChanged => {}
                platform::PlatformEvent::Quit => {
                    if let Some(window) = weak.upgrade() {
                        let _ = window.hide();
                    }
                    let _ = slint::quit_event_loop();
                }
            });
        }
    });
    slint::run_event_loop_until_quit()?;
    Ok(())
}

fn request_dynamic_detail(
    item: SearchResult,
    registry: details::DetailRegistry,
    language: String,
    generation: Arc<AtomicU64>,
    weak: slint::Weak<Launcher>,
) {
    let request_id = generation.fetch_add(1, Ordering::AcqRel) + 1;
    request_dynamic_detail_with_id(item, registry, language, generation, weak, request_id);
}

fn request_dynamic_detail_with_id(
    item: SearchResult,
    registry: details::DetailRegistry,
    language: String,
    generation: Arc<AtomicU64>,
    weak: slint::Weak<Launcher>,
    request_id: u64,
) {
    let Some(provider) = registry.provider_for(&item) else {
        return;
    };
    if let Some(cached) = provider.cached(&language) {
        if let Some(window) = weak.upgrade() {
            set_detail_view(&window, Some(&cached));
            window.set_detail_panel_visible(true);
        }
        return;
    }

    std::thread::spawn(move || {
        let result = provider.load(&language);
        let _ = slint::invoke_from_event_loop(move || {
            if generation.load(Ordering::Acquire) != request_id {
                return;
            }
            let Some(window) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(detail) => {
                    set_detail_view(&window, Some(&detail));
                    window.set_detail_panel_visible(true);
                }
                Err(error) => {
                    eprintln!("dynamic detail request failed: {error:#}");
                    let detail = provider.error_view(&language);
                    set_detail_view(&window, Some(&detail));
                    window.set_detail_panel_visible(true);
                }
            }
        });
    });
}

fn set_detail_view(window: &Launcher, view: Option<&DetailView>) {
    if let Some(view) = view {
        window.set_detail_title(view.title.clone().into());
        window.set_detail_subtitle(view.subtitle.clone().into());
        window.set_detail_source(view.source.clone().into());
        window.set_detail_panel_preferred_width(view.preferred_width.unwrap_or(0));
        window.set_detail_panel_scrollable(view.scrollable);
        window.set_detail_blocks(detail_blocks_model(view));
        window.set_detail_actions(detail_actions_model(view));
    } else {
        window.set_detail_title(String::new().into());
        window.set_detail_subtitle(String::new().into());
        window.set_detail_source(String::new().into());
        window.set_detail_panel_preferred_width(0);
        window.set_detail_panel_scrollable(false);
        window.set_detail_blocks(ModelRc::from(Rc::new(VecModel::from(
            Vec::<DetailBlock>::new(),
        ))));
        window.set_detail_actions(ModelRc::from(Rc::new(VecModel::from(Vec::<
            ContextActionRow,
        >::new()))));
    }
}

fn detail_blocks_model(view: &DetailView) -> ModelRc<DetailBlock> {
    let blocks = view
        .blocks
        .iter()
        .map(|block| match block {
            ResultDetailBlock::Text { text, color } => DetailBlock {
                kind: "text".into(),
                label: String::new().into(),
                value: text.clone().into(),
                color: color.clone().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Metric {
                label,
                value,
                color,
            } => DetailBlock {
                kind: "metric".into(),
                label: label.clone().into(),
                value: value.clone().into(),
                color: color.clone().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Icon { glyph, color, icon } => DetailBlock {
                kind: "icon".into(),
                label: String::new().into(),
                value: glyph.clone().into(),
                color: color.clone().into(),
                icon: icon
                    .as_ref()
                    .and_then(|icon| IconService::new().resolve_spec(icon))
                    .map(|icon| icon_to_image(icon.as_ref()))
                    .unwrap_or_default(),
            },
            ResultDetailBlock::Weather { value } => DetailBlock {
                kind: "weather".into(),
                label: String::new().into(),
                value: value.clone().into(),
                color: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Separator => DetailBlock {
                kind: "separator".into(),
                label: String::new().into(),
                value: String::new().into(),
                color: String::new().into(),
                icon: slint::Image::default(),
            },
        })
        .collect::<Vec<_>>();
    ModelRc::from(Rc::new(VecModel::from(blocks)))
}

fn detail_actions_model(view: &DetailView) -> ModelRc<ContextActionRow> {
    let actions = view
        .actions
        .iter()
        .take(3)
        .map(|action| ContextActionRow {
            id: action.id.clone().into(),
            title: action.title.clone().into(),
            subtitle: String::new().into(),
            kind: "detail".into(),
            icon: action
                .icon
                .as_ref()
                .and_then(|icon| IconService::new().resolve_spec(icon))
                .map(|icon| icon_to_image(icon.as_ref()))
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    ModelRc::from(Rc::new(VecModel::from(actions)))
}

#[cfg(any())]
fn weather_detail_view(data: &details::weather::WeatherData, language: &str) -> DetailView {
    DetailView {
        title: i18n::tr(language, "Weather").into(),
        subtitle: data.location.clone(),
        source: format!(
            "{}: Open-Meteo · {}",
            i18n::tr(language, "Source"),
            data.source
        ),
        actions: Vec::new(),
        blocks: vec![
            ResultDetailBlock::Weather {
                value: match data.weather_code {
                    0 => "clear",
                    71..=77 | 85..=86 => "snow",
                    51..=67 | 80..=82 => "rain",
                    95..=99 => "storm",
                    _ => "cloud",
                }
                .into(),
            },
            ResultDetailBlock::Text {
                text: format!("{}  {}", data.temperature, data.condition),
                color: "primary".into(),
            },
            ResultDetailBlock::Separator,
            ResultDetailBlock::Metric {
                label: i18n::tr(language, "Humidity").into(),
                value: data.humidity.clone(),
                color: "secondary".into(),
            },
            ResultDetailBlock::Metric {
                label: i18n::tr(language, "Wind").into(),
                value: data.wind.clone(),
                color: "secondary".into(),
            },
        ],
        preferred_width: None,
        scrollable: false,
    }
}

fn create_window(config: &config::Config) -> Result<Launcher> {
    select_renderer(&config.renderer)?;
    match Launcher::new() {
        Ok(window) => Ok(window),
        Err(error) if config.renderer != "software" => {
            eprintln!(
                "{} renderer could not create a window: {error}; falling back to software renderer",
                config.renderer
            );
            select_renderer("software")?;
            Ok(Launcher::new()?)
        }
        Err(error) => Err(error.into()),
    }
}

fn select_renderer(renderer: &str) -> Result<()> {
    let renderer = match renderer {
        // Keep the old config value working while selecting the explicit
        // OpenGL Skia backend that is compiled for this platform.
        "skia" => "skia-opengl",
        other => other,
    };
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attributes| {
            attributes
                .with_visible(false)
                .with_window_icon(launcher_window_icon())
        })
        .renderer_name(renderer.into())
        .select()?;
    Ok(())
}

fn launcher_window_icon() -> Option<slint::winit_030::winit::window::Icon> {
    let image = image::load_from_memory(include_bytes!("../wowie.png"))
        .ok()?
        .into_rgba8();
    let (width, height) = (image.width(), image.height());
    slint::winit_030::winit::window::Icon::from_rgba(image.into_raw(), width, height).ok()
}

fn toggle_palette(
    window: &Launcher,
    platform: Arc<dyn platform::PlatformIntegration>,
    visible: &AtomicBool,
) {
    if visible.fetch_xor(true, Ordering::AcqRel) {
        let _ = window.hide();
    } else {
        platform.disable_window_transitions(window.window());
        let _ = window.show();
        platform.activate_palette(window.window());
        let weak = window.as_weak();
        Timer::single_shot(Duration::from_millis(10), move || {
            if let Some(window) = weak.upgrade() {
                platform.position_palette(window.window());
                platform.activate_palette(window.window());
                window.window().request_redraw();
            }
        });
    }
}

fn hide_context_menu(
    menu: &slint::Weak<ContextMenu>,
    launcher: &slint::Weak<Launcher>,
    visible: &AtomicBool,
    generation: &AtomicU64,
    item: &Mutex<Option<SearchResult>>,
    hide_launcher: bool,
    launcher_visible: &AtomicBool,
) {
    generation.fetch_add(1, Ordering::AcqRel);
    visible.store(false, Ordering::Release);
    if let Some(menu) = menu.upgrade() {
        let _ = menu.hide();
    }
    if let Ok(mut current) = item.lock() {
        *current = None;
    }
    if let Some(launcher) = launcher.upgrade() {
        launcher.set_context_menu_open(false);
        if hide_launcher && launcher_visible.swap(false, Ordering::AcqRel) {
            let _ = launcher.hide();
        }
    }
}

struct ResultRowPayload {
    id: String,
    title: String,
    location: String,
    location_kind: String,
    icon: Option<crate::platform::IconData>,
    category: String,
}

fn context_actions(
    item: &SearchResult,
    show_advanced_launch_entries: bool,
) -> Vec<ContextActionRow> {
    let is_filesystem_target = is_supported_filesystem_target(&item.launch_target);
    let is_store_shell_target = is_apps_folder_target(&item.launch_target);
    let mut actions = Vec::new();
    let icon_service = IconService::new();
    if is_filesystem_target {
        actions.push(ContextActionRow {
            id: "open-directory".into(),
            title: "Open containing folder".into(),
            subtitle: "".into(),
            kind: "common".into(),
            icon: slint::Image::default(),
        });
    }
    if is_filesystem_target || is_store_shell_target {
        actions.push(ContextActionRow {
            id: "run-as-admin".into(),
            title: "Run as administrator".into(),
            subtitle: "".into(),
            kind: "common".into(),
            icon: slint::Image::default(),
        });
    }
    for action in &item.actions {
        if let result::Action::Plugin {
            action_id, icon, ..
        } = action
        {
            actions.push(ContextActionRow {
                id: format!("plugin-action:{action_id}").into(),
                title: action_id.clone().into(),
                subtitle: "".into(),
                kind: "plugin".into(),
                icon: icon
                    .as_ref()
                    .and_then(|icon| icon_service.resolve_spec(icon))
                    .map(|icon| icon_to_image(icon.as_ref()))
                    .unwrap_or_default(),
            });
        }
    }
    // ShortcutProvider actions are direct query actions and intentionally do
    // not populate the indexed launch_entries field. They must not be shown
    // as alternate application launch entries.
    let other_launches = visible_launch_entries(item, show_advanced_launch_entries);
    if !other_launches.is_empty() {
        actions.push(ContextActionRow {
            id: "section:other-launch-entries".into(),
            title: "".into(),
            subtitle: "".into(),
            kind: "section".into(),
            icon: slint::Image::default(),
        });
        for (index, entry) in other_launches {
            actions.push(ContextActionRow {
                id: format!("launch:{index}").into(),
                title: launch_entry_label(entry).into(),
                subtitle: "".into(),
                kind: "launch".into(),
                icon: slint::Image::default(),
            });
        }
    }
    actions
}

fn visible_launch_entries(
    item: &SearchResult,
    show_advanced_launch_entries: bool,
) -> Vec<(usize, &result::LaunchEntry)> {
    let Some(default) = crate::providers::app::preferred_launch_entry(item) else {
        return Vec::new();
    };
    item.launch_entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if crate::providers::app::same_launch_behavior(default, entry) {
                return None;
            }
            let allowed = show_advanced_launch_entries
                || (crate::platform::windows_rules::is_shortcut_path(&entry.path)
                    && entry.arguments.trim() != default.arguments.trim());
            allowed.then_some((index, entry))
        })
        .collect()
}

fn launch_entry_label(entry: &result::LaunchEntry) -> String {
    if crate::platform::windows_rules::is_shortcut_path(&entry.path) {
        if let Some(name) = entry.path.file_stem().and_then(|name| name.to_str()) {
            if !name.is_empty() {
                return name.to_owned();
            }
        }
    }
    let name = entry
        .target
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| entry.target.to_string_lossy().into_owned());
    if entry.arguments.trim().is_empty() {
        name
    } else {
        format!("{name} {}", entry.arguments.trim())
    }
}

fn is_apps_folder_target(target: &std::path::Path) -> bool {
    target
        .to_string_lossy()
        .trim_start()
        .get(.."shell:AppsFolder\\".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:AppsFolder\\"))
}

fn is_supported_filesystem_target(target: &std::path::Path) -> bool {
    target.is_absolute()
        && target.exists()
        && target.parent().is_some()
        && !crate::platform::windows_rules::is_shell_namespace(target)
        && !target.to_string_lossy().contains("://")
}

impl ResultRowPayload {
    fn from(item: &SearchResult, show_advanced_details: bool) -> Self {
        let location = display_location(item, show_advanced_details);
        Self {
            id: item.id.clone().into(),
            title: item.title.clone().into(),
            location: location.into(),
            location_kind: if !show_advanced_details && is_store_application_shell_target(item) {
                "apps-folder-entry"
            } else {
                ""
            }
            .into(),
            icon: None,
            category: match item.category {
                ResultCategory::Game => "game",
                ResultCategory::Application => "application",
                ResultCategory::Website => "website",
                ResultCategory::Shortcut => "shortcut",
                ResultCategory::Calculator => "calculator",
                ResultCategory::ControlPanel => "control-panel",
                ResultCategory::Document => "document",
                ResultCategory::Other => "other",
                ResultCategory::Unclassified => "application",
            }
            .into(),
        }
    }
}

fn display_location(item: &SearchResult, show_advanced_details: bool) -> String {
    if !show_advanced_details && item.category == ResultCategory::ControlPanel {
        return String::new();
    }
    let Some(entry) = crate::providers::app::preferred_launch_entry(item) else {
        return item.location.clone();
    };
    let target = entry.target.to_string_lossy();
    let is_shell_or_url =
        crate::platform::windows_rules::is_shell_namespace(&entry.target) || target.contains("://");
    let base = if show_advanced_details || is_shell_or_url {
        target.into_owned()
    } else {
        entry
            .target
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| item.location.clone())
    };
    if entry.arguments.trim().is_empty() {
        base
    } else {
        format!("{base} {}", entry.arguments.trim())
    }
}

fn is_store_application_shell_target(item: &SearchResult) -> bool {
    item.location
        .trim_start()
        .get(.."shell:AppsFolder\\".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:AppsFolder\\"))
}

fn request_icons(
    start: i32,
    end: i32,
    icon_items: &Arc<Mutex<(u64, Arc<Vec<SearchResult>>)>>,
    icon_requests: &Arc<Mutex<(u64, HashSet<usize>)>>,
    icon_service: &IconService,
    weak: &slint::Weak<Launcher>,
    query_generation: &Arc<AtomicU64>,
) {
    let (query_id, items) = icon_items
        .lock()
        .map(|state| (state.0, Arc::clone(&state.1)))
        .unwrap_or_else(|_| (0, Arc::new(Vec::new())));
    if query_id == 0 || query_generation.load(Ordering::Acquire) != query_id {
        return;
    }
    let start = start.max(0) as usize;
    let end = (end.max(start as i32) as usize).min(items.len());
    if start >= end {
        return;
    }
    let mut pending = Vec::new();
    if let Ok(mut state) = icon_requests.lock() {
        if state.0 != query_id {
            *state = (query_id, HashSet::new());
        }
        for index in start..end {
            if state.1.insert(index) {
                pending.push((index, items[index].clone()));
            }
        }
    }
    if pending.is_empty() {
        return;
    }
    let icon_service = icon_service.clone();
    let weak = weak.clone();
    let query_generation = Arc::clone(query_generation);
    std::thread::spawn(move || {
        let mut resolved = Vec::with_capacity(pending.len());
        for (index, item) in pending {
            if query_generation.load(Ordering::Acquire) != query_id {
                return;
            }
            let Some(icon) = icon_service.resolve(&item) else {
                continue;
            };
            resolved.push((index, icon));
        }
        if resolved.is_empty() {
            return;
        }
        let _ = slint::invoke_from_event_loop(move || {
            if query_generation.load(Ordering::Acquire) != query_id {
                return;
            }
            let Some(window) = weak.upgrade() else {
                return;
            };
            let model = window.get_results();
            for (index, icon) in resolved {
                let Some(mut row) = model.row_data(index) else {
                    continue;
                };
                row.icon = icon_to_image(icon.as_ref());
                model.set_row_data(index, row);
            }
        });
    });
}

fn icon_to_image(data: &crate::platform::IconData) -> slint::Image {
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &data.rgba,
        data.width,
        data.height,
    );
    slint::Image::from_rgba8(buffer)
}

impl From<ResultRowPayload> for ResultRow {
    fn from(payload: ResultRowPayload) -> Self {
        let icon = payload.icon.as_ref().map(icon_to_image).unwrap_or_default();
        Self {
            id: payload.id.into(),
            title: payload.title.into(),
            location: payload.location.into(),
            location_kind: payload.location_kind.into(),
            icon,
            category: payload.category.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::SourceId;
    use std::path::PathBuf;

    #[test]
    fn shortcut_action_is_not_shown_as_an_alternate_launch_entry() {
        let item = SearchResult {
            id: "shortcut:command".into(),
            identity: "shortcut:command".into(),
            title: "Execute Command \"echo test\"".into(),
            category: ResultCategory::Shortcut,
            aliases: Vec::new(),
            location: "PowerShell".into(),
            launch_target: PathBuf::from("pwsh.exe"),
            launch_arguments: "-Command \"echo test\"".into(),
            launch_working_directory: None,
            source: SourceId::Shortcut,
            sources: vec![SourceId::Shortcut],
            launch_entries: Vec::new(),
            actions: vec![result::Action::Launch(result::LaunchEntry {
                source: SourceId::Shortcut,
                path: PathBuf::new(),
                target: PathBuf::from("pwsh.exe"),
                arguments: "-Command \"echo test\"".into(),
                working_directory: None,
            })],
            detail: None,
            icon: None,
            ranking_bias: 0,
        };

        assert!(context_actions(&item, false)
            .iter()
            .all(|action| action.kind != "section"));
    }

    #[test]
    fn alternate_lnk_entry_uses_shortcut_name_without_target_arguments() {
        let entry = result::LaunchEntry {
            source: SourceId::Desktop,
            path: PathBuf::from(r"C:\Users\User\Desktop\Chrome - Work.lnk"),
            target: PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            arguments: "--profile-directory=Work".into(),
            working_directory: None,
        };

        assert_eq!(launch_entry_label(&entry), "Chrome - Work");
    }

    #[test]
    fn default_mode_only_shows_shortcuts_with_distinct_arguments() {
        let default = result::LaunchEntry {
            source: SourceId::StartMenu,
            path: PathBuf::from(r"C:\Chrome.lnk"),
            target: PathBuf::from(r"C:\chrome.exe"),
            arguments: "--profile-directory=Default".into(),
            working_directory: None,
        };
        let same_arguments = result::LaunchEntry {
            path: PathBuf::from(r"C:\Chrome - Copy.lnk"),
            ..default.clone()
        };
        let different_arguments = result::LaunchEntry {
            path: PathBuf::from(r"C:\Chrome - Work.lnk"),
            arguments: "--profile-directory=Work".into(),
            ..default.clone()
        };
        let item = SearchResult {
            id: "exe:chrome".into(),
            identity: "exe:chrome".into(),
            title: "Chrome".into(),
            category: ResultCategory::Application,
            aliases: Vec::new(),
            location: "chrome.exe".into(),
            launch_target: default.target.clone(),
            launch_arguments: default.arguments.clone(),
            launch_working_directory: None,
            source: SourceId::StartMenu,
            sources: vec![SourceId::StartMenu],
            launch_entries: vec![
                default.clone(),
                same_arguments.clone(),
                different_arguments.clone(),
            ],
            actions: vec![
                result::Action::Launch(default),
                result::Action::Launch(same_arguments),
                result::Action::Launch(different_arguments),
            ],
            detail: None,
            icon: None,
            ranking_bias: 0,
        };
        let visible = visible_launch_entries(&item, false);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].1.arguments, "--profile-directory=Work");
        assert_eq!(visible_launch_entries(&item, true).len(), 1);
    }

    #[test]
    fn alternate_non_shortcut_entry_keeps_target_and_arguments() {
        let entry = result::LaunchEntry {
            source: SourceId::StartMenu,
            path: PathBuf::from(r"C:\Apps\Chrome.exe"),
            target: PathBuf::from(r"C:\Apps\Chrome.exe"),
            arguments: "--profile-directory=Work".into(),
            working_directory: None,
        };

        assert_eq!(
            launch_entry_label(&entry),
            "Chrome.exe --profile-directory=Work"
        );
    }
}
