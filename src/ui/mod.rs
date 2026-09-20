mod actions;
mod details;
mod results;
mod window;

slint::include_modules!();

use crate::{
    config,
    icon_service::{IconService, DISPLAY_ICON_SIZE},
    launcher, platform, plugin,
    providers::app::AppProvider,
    result,
    result::SearchResult,
    search,
    utils::i18n,
};
use actions::{context_actions, context_menu_width, resolve_plugin_action_icons};
use anyhow::Result;
use details::{request_dynamic_detail, set_detail_view};
use results::{
    isolate_shortcut_results, matches_model, request_icons, static_icon_kind, ResultRowPayload,
};
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
use window::{create_window, hide_context_menu, save_window_size, toggle_palette};

fn update_search_icon(
    window: &Launcher,
    icon: Option<&crate::platform::IconData>,
    kind: String,
    is_shortcut: bool,
    animate: bool,
    transition_generation: Arc<AtomicU64>,
) {
    let transition_id = transition_generation.fetch_add(1, Ordering::AcqRel) + 1;
    let was_shortcut = window.get_search_icon_is_shortcut();
    let should_transition = animate && (was_shortcut || is_shortcut);
    let previous_icon = window.get_search_icon();
    let previous_kind = window.get_search_icon_kind();
    if should_transition {
        window.set_search_icon_animation_enabled(false);
        window.set_search_icon_transition_progress(0.0);
        window.set_search_icon_previous(previous_icon);
        window.set_search_icon_previous_kind(previous_kind);
    }
    window.set_search_icon(icon.map(results::icon_to_image).unwrap_or_default());
    window.set_search_icon_kind(kind.into());
    window.set_search_icon_is_shortcut(is_shortcut);

    if should_transition {
        // Let the reset state render for one frame. Without this separation,
        // Slint can coalesce reset and activation into one frame.
        let weak = window.as_weak();
        Timer::single_shot(Duration::from_millis(16), move || {
            if let Some(window) = weak.upgrade() {
                if transition_generation.load(Ordering::Acquire) != transition_id {
                    return;
                }
                window.set_search_icon_animation_enabled(true);
                window.set_search_icon_transition_progress(1.0);
            }
        });
    } else {
        window.set_search_icon_animation_enabled(false);
        window.set_search_icon_transition_progress(1.0);
        window.set_search_icon_animation_enabled(true);
    }
}

pub fn run() -> Result<()> {
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
    let search_icon_generation = Arc::new(AtomicU64::new(0));
    let detail_generation = Arc::new(AtomicU64::new(0));
    let detail_registry = crate::details::DetailRegistry::default_providers();
    let current_query = Arc::new(Mutex::new(String::new()));
    // Keep one shared result snapshot for icon loading. Cloning the complete
    // result list on every visible-range notification caused avoidable memory
    // spikes while scrolling.
    let icon_items = Arc::new(Mutex::new((0_u64, Arc::new(Vec::<SearchResult>::new()))));
    let icon_requests = Arc::new(Mutex::new((0_u64, HashSet::<usize>::new())));

    window.set_results(ModelRc::from(results.clone()));
    window.set_window_width(config.window_width as i32);
    window.set_result_window_height(config.window_height as f32);
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
    window.set_search_icon(slint::Image::default());
    window.set_search_icon_previous(slint::Image::default());
    window.set_search_icon_previous_kind(String::new().into());
    window.set_search_icon_transition_progress(1.0);
    window.set_search_icon_animation_enabled(true);
    window.set_search_icon_is_shortcut(false);
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
        let search_icon_generation = Arc::clone(&search_icon_generation);
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
                let native_window = window.window();
                let current_height = native_window
                    .size()
                    .to_logical(native_window.scale_factor())
                    .height;
                let result_height = if current_height >= config::MIN_WINDOW_HEIGHT as f32 {
                    current_height
                } else {
                    window
                        .get_result_window_height()
                        .max(config::MIN_WINDOW_HEIGHT as f32)
                };
                if (window.get_result_window_height() - result_height).abs() > 0.5 {
                    window.set_result_window_height(result_height);
                }
                if !query.trim().is_empty() && (current_height - result_height).abs() > 0.5 {
                    let logical_size = native_window
                        .size()
                        .to_logical(native_window.scale_factor());
                    native_window
                        .set_size(slint::LogicalSize::new(logical_size.width, result_height));
                }
                window.set_last_action_id(String::new().into());
                window.set_last_action_result(String::new().into());
                window.set_last_action_subtitle(String::new().into());
                window.set_launch_error(String::new().into());
            }
            let query_id = query_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let engine = Arc::clone(&engine);
            let icon_service = icon_service.clone();
            let icon_items = Arc::clone(&icon_items);
            let icon_requests = Arc::clone(&icon_requests);
            let weak = weak.clone();
            let query_generation = Arc::clone(&query_generation);
            let search_icon_generation = Arc::clone(&search_icon_generation);
            let show_advanced_details_for_query = Arc::clone(&show_advanced_details);
            let detail_registry_for_query = detail_registry.clone();
            let detail_language_for_query = detail_language.clone();
            std::thread::spawn(move || {
                let found = if query.trim().is_empty() {
                    Vec::new()
                } else {
                    engine.search(query.as_str())
                };
                let shortcut_results = isolate_shortcut_results(query.as_str(), &found);
                let shortcut_icon = shortcut_results
                    .as_ref()
                    .and_then(|results| results.first())
                    .filter(|item| static_icon_kind(item.icon.as_ref()).is_empty())
                    .and_then(|item| icon_service.resolve_at(item, DISPLAY_ICON_SIZE));
                let shortcut_icon_kind = shortcut_results
                    .as_ref()
                    .and_then(|results| results.first())
                    .map(|item| static_icon_kind(item.icon.as_ref()).to_owned())
                    .unwrap_or_default();
                let is_shortcut_result = shortcut_results.is_some();
                let found = shortcut_results.unwrap_or(found);
                let rows = found
                    .iter()
                    .map(|item| {
                        ResultRowPayload::from(
                            item,
                            show_advanced_details_for_query.load(Ordering::Acquire),
                            query.as_str(),
                        )
                    })
                    .collect::<Vec<_>>();
                let has_dynamic_detail = found
                    .first()
                    .is_some_and(|item| detail_registry_for_query.provider_for(item).is_some());
                let detail_panel = found
                    .first()
                    .is_some_and(|item| item.detail.is_some() && !has_dynamic_detail);
                let first_detail = (!has_dynamic_detail)
                    .then(|| found.first().and_then(|item| item.detail.clone()))
                    .flatten();
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
                        let results_changed = !matches_model(&window.get_results(), &rows);
                        let icon_mode_changed =
                            window.get_search_icon_is_shortcut() != is_shortcut_result;
                        let search_icon_changed =
                            icon_mode_changed || (results_changed && is_shortcut_result);
                        let count = rows.len();
                        if results_changed {
                            let model = Rc::new(VecModel::from(
                                rows.into_iter().map(ResultRow::from).collect::<Vec<_>>(),
                            ));
                            window.set_results(ModelRc::from(model));
                        }
                        if search_icon_changed {
                            update_search_icon(
                                &window,
                                shortcut_icon.as_ref().map(|icon| icon.as_ref()),
                                shortcut_icon_kind,
                                is_shortcut_result,
                                icon_mode_changed,
                                search_icon_generation,
                            );
                        }
                        let cached_detail = (!has_dynamic_detail)
                            .then(|| {
                                found_for_icons
                                    .first()
                                    .and_then(|item| detail_registry_for_query.provider_for(item))
                                    .and_then(|provider| {
                                        provider.cached(&detail_language_for_query)
                                    })
                            })
                            .flatten();
                        if results_changed {
                            set_detail_view(
                                &window,
                                cached_detail.as_ref().or(first_detail.as_ref()),
                                &icon_service,
                            );
                            window.set_detail_panel_visible(detail_panel);
                            window.set_selected_index(-1);
                            window.set_selected_index(if count > 0 { 0 } else { -1 });
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
        let icon_service = icon_service.clone();
        let weak = window.as_weak();
        let detail_registry = detail_registry.clone();
        let detail_generation = Arc::clone(&detail_generation);
        window.on_selection_changed(move |index| {
            // Selection only changes local preview state. It also invalidates
            // a detail request that was explicitly started for the previous
            // result, so a late response cannot overwrite the new selection.
            detail_generation.fetch_add(1, Ordering::AcqRel);
            let item = weak
                .upgrade()
                .and_then(|window| window.get_results().row_data(index as usize))
                .and_then(|row| engine.by_id(row.id.as_str()));
            let has_dynamic_detail = item
                .as_ref()
                .is_some_and(|item| detail_registry.provider_for(item).is_some());
            if let Some(window) = weak.upgrade() {
                set_detail_view(
                    &window,
                    item.as_ref()
                        .filter(|_| !has_dynamic_detail)
                        .and_then(|item| item.detail.as_ref()),
                    &icon_service,
                );
                window.set_detail_panel_visible(
                    !has_dynamic_detail && item.as_ref().is_some_and(|item| item.detail.is_some()),
                );
            }
        });
    }
    {
        let engine = engine.clone();
        let icon_service = icon_service.clone();
        let weak = window.as_weak();
        let visible = Arc::clone(&visible);
        let current_query = Arc::clone(&current_query);
        let detail_generation = Arc::clone(&detail_generation);
        let detail_registry = detail_registry.clone();
        let detail_language = config.language.clone();
        window.on_launch_requested(move |index| {
            let item = weak.upgrade().and_then(|window| {
                window
                    .get_results()
                    .row_data(index as usize)
                    .and_then(|row| engine.by_id(row.id.as_str()))
            });
            if let Some(item) = item {
                if item.actions.is_empty() && item.detail.is_some() {
                    let has_dynamic_detail = detail_registry.provider_for(&item).is_some();
                    if let Some(window) = weak.upgrade() {
                        window.set_selected_index(index);
                        set_detail_view(&window, item.detail.as_ref(), &icon_service);
                        window.set_detail_panel_visible(true);
                    }
                    if has_dynamic_detail {
                        request_dynamic_detail(
                            item,
                            detail_registry.clone(),
                            detail_language.clone(),
                            Arc::clone(&detail_generation),
                            weak.clone(),
                            icon_service.clone(),
                        );
                    }
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
        let icon_service = icon_service.clone();
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
            let item_for_icons = item.clone();
            if let Ok(mut current) = context_item.lock() {
                *current = Some(item);
            }
            let menu_generation = context_generation.fetch_add(1, Ordering::AcqRel) + 1;
            context_visible.store(true, Ordering::Release);
            if let Some(window) = weak.upgrade() {
                window.set_context_menu_open(true);
            }
            if let Some(menu) = context_weak.upgrade() {
                menu.set_menu_width(context_menu_width(&actions) as f32);
                menu.set_actions(ModelRc::from(Rc::new(VecModel::from(actions))));
                platform.disable_window_transitions(menu.window());
                let _ = menu.show();
                platform.position_context_menu(menu.window());
                platform.activate_context_menu(menu.window());
            }

            let icon_service = icon_service.clone();
            let context_weak = context_weak.clone();
            let context_generation = Arc::clone(&context_generation);
            std::thread::spawn(move || {
                let icons = resolve_plugin_action_icons(&item_for_icons, &icon_service);
                if icons.is_empty() {
                    return;
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if context_generation.load(Ordering::Acquire) != menu_generation {
                        return;
                    }
                    let Some(menu) = context_weak.upgrade() else {
                        return;
                    };
                    let model = menu.get_actions();
                    for (action_id, icon) in icons {
                        for index in 0..model.row_count() {
                            let Some(mut row) = model.row_data(index) else {
                                continue;
                            };
                            if row.id == action_id && row.icon_kind.is_empty() {
                                row.icon = results::icon_to_image(&icon);
                                model.set_row_data(index, row);
                                break;
                            }
                        }
                    }
                });
            });
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
        let detail_registry = detail_registry.clone();
        let search_icon_generation = Arc::clone(&search_icon_generation);
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
                    let shortcut_results = isolate_shortcut_results(&query, &found);
                    let shortcut_icon = shortcut_results
                        .as_ref()
                        .and_then(|results| results.first())
                        .filter(|item| static_icon_kind(item.icon.as_ref()).is_empty())
                        .and_then(|item| icon_service.resolve_at(item, DISPLAY_ICON_SIZE));
                    let shortcut_icon_kind = shortcut_results
                        .as_ref()
                        .and_then(|results| results.first())
                        .map(|item| static_icon_kind(item.icon.as_ref()).to_owned())
                        .unwrap_or_default();
                    let is_shortcut_result = shortcut_results.is_some();
                    let found = shortcut_results.unwrap_or(found);
                    let rows = found
                        .iter()
                        .map(|item| {
                            ResultRowPayload::from(
                                item,
                                show_advanced_details.load(Ordering::Acquire),
                                &query,
                            )
                        })
                        .collect::<Vec<_>>();
                    let has_dynamic_detail = found
                        .first()
                        .is_some_and(|item| detail_registry.provider_for(item).is_some());
                    let detail_panel = found
                        .first()
                        .is_some_and(|item| item.detail.is_some() && !has_dynamic_detail);
                    let first_detail = (!has_dynamic_detail)
                        .then(|| found.first().and_then(|item| item.detail.clone()))
                        .flatten();
                    let found_for_icons = Arc::new(found);
                    let query_generation_for_ui = Arc::clone(&query_generation);
                    let weak_for_ui = weak.clone();
                    let icon_items_for_ui = Arc::clone(&icon_items);
                    let icon_requests_for_ui = Arc::clone(&icon_requests);
                    let icon_service_for_ui = icon_service.clone();
                    let query_generation_for_icons = Arc::clone(&query_generation);
                    let search_icon_generation_for_ui = Arc::clone(&search_icon_generation);
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
                            let results_changed = !matches_model(&window.get_results(), &rows);
                            let icon_mode_changed =
                                window.get_search_icon_is_shortcut() != is_shortcut_result;
                            let search_icon_changed =
                                icon_mode_changed || (results_changed && is_shortcut_result);
                            let count = rows.len();
                            if results_changed {
                                let model = Rc::new(VecModel::from(
                                    rows.into_iter().map(ResultRow::from).collect::<Vec<_>>(),
                                ));
                                window.set_results(ModelRc::from(model));

                                set_detail_view(
                                    &window,
                                    first_detail.as_ref(),
                                    &icon_service_for_ui,
                                );
                                window.set_detail_panel_visible(detail_panel);
                                window.set_selected_index(-1);
                                window.set_selected_index(if count > 0 { 0 } else { -1 });
                            }
                            if search_icon_changed {
                                update_search_icon(
                                    &window,
                                    shortcut_icon.as_ref().map(|icon| icon.as_ref()),
                                    shortcut_icon_kind,
                                    is_shortcut_result,
                                    icon_mode_changed,
                                    search_icon_generation_for_ui,
                                );
                            }
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
        visible.store(true, Ordering::Release);
        let weak = window.as_weak();
        let platform = Arc::clone(&platform);
        Timer::single_shot(Duration::from_millis(10), move || {
            if let Some(window) = weak.upgrade() {
                platform.position_palette(
                    window.window(),
                    window.get_result_mode(),
                    window.get_result_window_height(),
                );
                platform.activate_palette(window.window());
            }
        });
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
                        save_window_size(&window);
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

#[cfg(test)]
mod tests {
    use super::actions::{launch_entry_label, visible_launch_entries};
    use super::*;
    use crate::platform::SourceId;
    use std::path::PathBuf;

    #[test]
    fn shortcut_action_is_not_shown_as_an_alternate_launch_entry() {
        let item = SearchResult {
            id: "shortcut:command".into(),
            identity: "shortcut:command".into(),
            title: "Execute Command \"echo test\"".into(),
            category: result::ResultCategory::Shortcut,
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
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
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
        let icon_placeholder = result::LaunchEntry {
            path: PathBuf::from(r"C:\Start Menu\WSL Settings.lnk"),
            target: PathBuf::from(r"C:\Windows\Installer\{guid}\wsl.ico"),
            ..default.clone()
        };
        let item = SearchResult {
            id: "exe:chrome".into(),
            identity: "exe:chrome".into(),
            title: "Chrome".into(),
            category: result::ResultCategory::Application,
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
                icon_placeholder,
            ],
            actions: vec![
                result::Action::Launch(default),
                result::Action::Launch(same_arguments),
                result::Action::Launch(different_arguments),
            ],
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
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
