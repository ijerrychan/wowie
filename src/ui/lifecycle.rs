use super::*;

pub fn run() -> Result<()> {
    #[cfg(not(debug_assertions))]
    let _single_instance = match platform::try_acquire_single_instance()? {
        Some(instance) => instance,
        None => return Ok(()),
    };

    let (config, config_was_valid) = config::Config::load();
    let window = create_window(&config)?;
    let settings_window = SettingsWindow::new()?;
    let context_window = ContextMenu::new()?;
    // Native Windows frames own the outer contour; keep the Slint surface opaque
    // and flush with it. Other platforms retain the existing transparent shell.
    window.set_native_window_frame(cfg!(windows));
    settings_window.set_native_window_frame(cfg!(windows));
    context_window.set_native_window_frame(cfg!(windows));
    settings::preload_font_families(settings_window.as_weak());
    let slint_language = i18n::effective_language(&config.language);
    slint::select_bundled_translation(&slint_language)
        .map_err(|error| anyhow::anyhow!("could not select language: {error}"))?;
    let platform = platform::current();
    let platform_shutdown = platform::PlatformShutdown::new();
    let tray_registration = platform.start_tray(
        platform::TrayMenuLabels {
            settings: i18n::tr(&config.language, "Settings").to_owned(),
            exit: i18n::tr(&config.language, "Exit").to_owned(),
        },
        &platform_shutdown,
    );
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
    let app_source_events =
        AppProvider::watch_sources(Arc::clone(&custom_directories), &platform_shutdown);
    let config_events = config::watch_changes();
    let plugins = plugin::PluginManager::discover_with_config(&config.language, &config.plugins);
    let plugin_settings = plugins.settings_definitions();
    settings_window.set_dark_mode(dark_mode);
    settings_window.set_categories(settings::categories(&config.language, &plugin_settings));
    settings_window.set_selected_category("general".into());
    settings_window.set_selected_category_title(
        settings::category_title("general", &config.language, &plugin_settings).into(),
    );
    settings_window.set_entries(settings::entries(
        &config,
        &plugin_settings,
        "general",
        &config.language,
    ));
    let engine = search::SearchCore::with_plugins_and_shortcuts_and_keywords(
        provider.snapshot().as_ref().clone(),
        plugins.clone(),
        config.shortcuts.clone(),
        config.shortcut_keywords.clone(),
    );
    let engine = Arc::new(engine);
    engine.replace_fuzzy_search(config.fuzzy_search);
    engine.replace_separate_launch_entries(config.separate_launch_entries);
    engine.replace_show_advanced_launch_entries(config.show_advanced_launch_entries);
    let plugin_order = settings::visible_plugin_order(&config, &plugin_settings);
    engine.replace_priority_order(&config.result_order, &plugin_order);
    engine.replace_provider_language(config.language.clone());
    let icon_service = IconService::new();
    let results = Rc::new(VecModel::<ResultRow>::default());
    let visible = Arc::new(AtomicBool::new(false));
    let context_visible = Arc::new(AtomicBool::new(false));
    let context_generation = Arc::new(AtomicU64::new(0));
    let context_item = Arc::new(Mutex::new(None::<SearchResult>));
    let empty_state_item = Arc::new(Mutex::new(None::<SearchResult>));
    let show_advanced_details = Arc::new(AtomicBool::new(config.show_advanced_details));
    let show_advanced_launch_entries =
        Arc::new(AtomicBool::new(config.show_advanced_launch_entries));
    let follow_system_theme = Arc::new(AtomicBool::new(config.theme == "system"));
    let hide_on_focus_loss = Arc::new(AtomicBool::new(config.hide_on_focus_loss));
    let query_generation = Arc::new(AtomicU64::new(0));
    let search_icon_generation = Arc::new(AtomicU64::new(0));
    let detail_generation = Arc::new(AtomicU64::new(0));
    let current_language = Arc::new(Mutex::new(config.language.clone()));
    let explicit_detail_item = Arc::new(Mutex::new(None::<String>));
    let mut detail_registry = crate::details::DetailRegistry::default_providers();
    detail_registry.register(Arc::new(plugins));
    let current_query = Arc::new(Mutex::new(String::new()));
    let query_history = Arc::new(Mutex::new((Vec::<QueryHistoryEntry>::new(), 0_usize)));
    let active_session = Arc::new(Mutex::new(None::<crate::page::SearchEngineSession>));
    // Keep one shared result snapshot for icon loading. Cloning the complete
    // result list on every visible-range notification caused avoidable memory
    // spikes while scrolling.
    let icon_items = Arc::new(Mutex::new((0_u64, Arc::new(Vec::<SearchResult>::new()))));
    let icon_requests = Arc::new(Mutex::new((0_u64, HashSet::<usize>::new())));
    let result_filter = Arc::new(Mutex::new(results::ResultFilterState::default()));
    let page_load = Arc::new(Mutex::new(PageLoadState::default()));

    window.set_results(ModelRc::from(results.clone()));
    window.set_window_width(config.window_width as i32);
    window.set_ui_font_family(config.font_family.clone().into());
    window.set_ui_font_size(config.font_size as f32);
    window.set_result_window_height(config.window_height as f32);
    window.set_compact_search(config.compact_search);
    window.set_root_query_empty(true);
    window.set_empty_query_results_visible(false);
    window.set_list_item_height(config.list_item_height as f32);
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
    window::install_native_background(window.as_weak(), Launcher::get_native_background_color);
    window::install_native_background(
        settings_window.as_weak(),
        SettingsWindow::get_native_background_color,
    );
    window::install_native_background(
        context_window.as_weak(),
        ContextMenu::get_native_background_color,
    );
    context_window.set_ui_font_family(config.font_family.clone().into());
    context_window.set_ui_font_size(config.font_size as f32);
    context_window.set_accent_color(slint::Color::from_rgb_u8(
        ((accent >> 16) & 0xff) as u8,
        ((accent >> 8) & 0xff) as u8,
        (accent & 0xff) as u8,
    ));
    settings_window.set_accent_color(slint::Color::from_rgb_u8(
        ((accent >> 16) & 0xff) as u8,
        ((accent >> 8) & 0xff) as u8,
        (accent & 0xff) as u8,
    ));
    settings_window.set_ui_font_family(config.font_family.clone().into());
    settings_window.set_ui_font_size(config.font_size as f32);

    settings_callbacks::register_settings_callbacks(
        &settings_window,
        &window,
        &context_window,
        plugin_settings.clone(),
    );

    let hotkey_registration = platform.register_hotkey(&config.hotkey, true, &platform_shutdown)?;
    let hotkey_updates = hotkey_registration.updates.clone();

    {
        let engine = Arc::clone(&engine);
        let result_filter = Arc::clone(&result_filter);
        let icon_items = Arc::clone(&icon_items);
        let icon_requests = Arc::clone(&icon_requests);
        let icon_service = icon_service.clone();
        let query_generation = Arc::clone(&query_generation);
        let weak = window.as_weak();
        window.on_category_filter_selected(move |key| {
            let Some(window) = weak.upgrade() else { return };
            let Ok(mut filter) = result_filter.lock() else {
                return;
            };
            if filter.filters().is_empty()
                || !filter.filters().iter().any(|option| option.key == key)
                || filter.selected == key.as_str()
            {
                return;
            }
            let selected_id = window
                .get_results()
                .row_data(window.get_selected_index().max(0) as usize)
                .map(|row| row.id.to_string());
            filter.selected = key.to_string();
            let (changed, count) =
                apply_result_filter(&window, &engine, &filter, &icon_items, &icon_requests);
            if changed {
                let index = selected_id
                    .and_then(|id| {
                        (0..count).find(|index| {
                            window
                                .get_results()
                                .row_data(*index)
                                .is_some_and(|row| row.id == id)
                        })
                    })
                    .map(|index| index as i32)
                    .unwrap_or(if count > 0 { 0 } else { -1 });
                window.set_selected_index(index);
                window.invoke_ensure_selected_visible();
                window.invoke_selection_changed(index);
                schedule_visible_range_refresh(&window, count.min(8) as i32);
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
        });
    }

    {
        let engine = engine.clone();
        let icon_items = Arc::clone(&icon_items);
        let icon_requests = Arc::clone(&icon_requests);
        let result_filter = Arc::clone(&result_filter);
        let icon_service = icon_service.clone();
        let weak = window.as_weak();
        let query_generation = Arc::clone(&query_generation);
        let current_query = Arc::clone(&current_query);
        let active_session = Arc::clone(&active_session);
        let show_advanced_details = Arc::clone(&show_advanced_details);
        let page_load = Arc::clone(&page_load);
        let pending_range = Rc::new(Cell::new(None::<(i32, i32)>));
        let range_queued = Rc::new(Cell::new(false));
        window.on_visible_range_changed(move |start, end| {
            pending_range.set(Some((start, end)));
            if range_queued.replace(true) {
                return;
            }
            let pending_range = Rc::clone(&pending_range);
            let range_queued = Rc::clone(&range_queued);
            let engine = engine.clone();
            let icon_items = Arc::clone(&icon_items);
            let icon_requests = Arc::clone(&icon_requests);
            let result_filter = Arc::clone(&result_filter);
            let icon_service = icon_service.clone();
            let weak = weak.clone();
            let query_generation = Arc::clone(&query_generation);
            let current_query = Arc::clone(&current_query);
            let active_session = Arc::clone(&active_session);
            let show_advanced_details = Arc::clone(&show_advanced_details);
            let page_load = Arc::clone(&page_load);
            Timer::single_shot(Duration::ZERO, move || {
                range_queued.set(false);
                let Some((start, end)) = pending_range.take() else {
                    return;
                };
                request_icons(
                    start,
                    end,
                    &icon_items,
                    &icon_requests,
                    &icon_service,
                    &weak,
                    &query_generation,
                );

                let (query_id, items, visible_count) = {
                    let Ok(state) = result_filter.lock() else {
                        return;
                    };
                    let visible_count = icon_items.lock().map(|items| items.1.len()).unwrap_or(0);
                    (state.query_id, Arc::clone(&state.all_items), visible_count)
                };
                if query_id == 0
                    || query_generation.load(Ordering::Acquire) != query_id
                    || items.len() >= MAX_RENDERED_RESULTS
                    || end.max(0) as usize + SEARCH_PAGE_PREFETCH_DISTANCE < visible_count
                {
                    return;
                }
                let query = current_query
                    .lock()
                    .map(|query| query.clone())
                    .unwrap_or_default();
                let context = active_session
                    .lock()
                    .ok()
                    .and_then(|session| session.clone())
                    .map(crate::page::SearchContext::Engine)
                    .unwrap_or_else(|| engine.search_context(&query));
                if context.session().is_none() {
                    return;
                }
                if let Ok(mut state) = page_load.lock() {
                    if state.query_id != query_id {
                        *state = PageLoadState {
                            query_id,
                            ..Default::default()
                        };
                    }
                    if state.loading || state.exhausted {
                        return;
                    }
                    state.loading = true;
                } else {
                    return;
                }

                let engine = Arc::clone(&engine);
                let icon_items = Arc::clone(&icon_items);
                let icon_requests = Arc::clone(&icon_requests);
                let result_filter = Arc::clone(&result_filter);
                let icon_service = icon_service.clone();
                let weak = weak.clone();
                let query_generation = Arc::clone(&query_generation);
                let page_load = Arc::clone(&page_load);
                let page_load_on_rejection = Arc::clone(&page_load);
                let show_advanced_details = Arc::clone(&show_advanced_details);
                if !crate::utils::background::spawn(move || {
                    let more = accepted_page_items(
                        &items,
                        engine.load_more_in_context(&context, &query, items.len()),
                    );
                    let _ = slint::invoke_from_event_loop(move || {
                        if query_generation.load(Ordering::Acquire) != query_id {
                            return;
                        }
                        if let Ok(mut state) = page_load.lock() {
                            if state.query_id == query_id {
                                state.loading = false;
                                state.exhausted = more.is_empty();
                            }
                        }
                        if more.is_empty() {
                            return;
                        }
                        let Some(window) = weak.upgrade() else {
                            return;
                        };
                        let (combined, rows, selected_filter) = {
                            let Ok(filter) = result_filter.lock() else {
                                return;
                            };
                            if filter.query_id != query_id || filter.all_items.len() != items.len()
                            {
                                return;
                            }
                            let mut combined = (*items).clone();
                            combined.extend(more.iter().cloned());
                            let mut rows = filter.all_rows.clone();
                            rows.extend(more.iter().map(|item| {
                                let shortcut_hint = (!window.get_shortcut_session_active())
                                    .then(|| engine.shortcut_hint(&item.id))
                                    .flatten();
                                ResultRowPayload::from(
                                    item,
                                    show_advanced_details.load(Ordering::Acquire),
                                    shortcut_hint.as_deref(),
                                    engine.is_favorite(&item.id),
                                    engine.is_fallback_result(item),
                                )
                            }));
                            let selected_filter = filter.selected != "all";
                            (Arc::new(combined), rows, selected_filter)
                        };
                        let Some((_, count)) = apply_result_snapshot(
                            &window,
                            ResultSnapshot {
                                query_id,
                                items: combined,
                                rows,
                                expected_previous_len: Some(items.len()),
                                selection: SelectionPolicy::KeepIndex,
                                show_results: true,
                                mode: None,
                            },
                            SnapshotTargets {
                                engine: &engine,
                                filter: &result_filter,
                                icon_items: &icon_items,
                                icon_requests: &icon_requests,
                                query_generation: &query_generation,
                            },
                        ) else {
                            return;
                        };
                        request_icons(
                            start,
                            end,
                            &icon_items,
                            &icon_requests,
                            &icon_service,
                            &window.as_weak(),
                            &query_generation,
                        );
                        if selected_filter && items.len() + more.len() < MAX_RENDERED_RESULTS {
                            schedule_visible_range_refresh(&window, count.min(8) as i32);
                        }
                    });
                }) {
                    if let Ok(mut state) = page_load_on_rejection.lock() {
                        if state.query_id == query_id {
                            state.loading = false;
                        }
                    }
                }
            });
        });
    }

    {
        let engine = engine.clone();
        let weak = window.as_weak();
        let active_session = Arc::clone(&active_session);
        let icon_items = Arc::clone(&icon_items);
        let action_generation = Arc::clone(&query_generation);
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
            // Session introductions are transient detail rows, so they are
            // intentionally absent from the engine's persistent index.
            let item = engine.by_id(row.id.as_str()).or_else(|| {
                icon_items
                    .lock()
                    .ok()
                    .and_then(|state| state.1.iter().find(|item| row.id == item.id).cloned())
            });
            let Some(item) = item else {
                return;
            };
            let Some(detail_action) = item
                .detail
                .as_ref()
                .and_then(|view| view.actions.get(index as usize))
            else {
                return;
            };
            let item_id = item.id.clone();
            let action_id = detail_action.id.clone();
            let query = window.get_query_text().to_string();
            let session = active_session
                .lock()
                .ok()
                .and_then(|session| session.clone());
            let request_generation = action_generation.load(Ordering::Acquire);
            let engine = Arc::clone(&engine);
            let weak = weak.clone();
            let action_generation = Arc::clone(&action_generation);
            crate::utils::background::spawn(move || {
                let result = engine.execute_transient_plugin_action(&item, &action_id);
                if result.is_ok() {
                    engine.record_action_success(&item_id, &query, session.as_ref());
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if action_generation.load(Ordering::Acquire) != request_generation {
                        return;
                    }
                    let Some(window) = weak.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(output) => {
                            window.set_last_action_id(item_id.into());
                            window.set_last_action_result(output.title.into());
                            window.set_last_action_subtitle(output.subtitle.into());
                            window.set_launch_error(String::new().into());
                        }
                        Err(error) => window.set_launch_error(error.to_string().into()),
                    }
                });
            });
        });
    }
    {
        let weak = window.as_weak();
        let icon_service = icon_service.clone();
        let detail_generation = Arc::clone(&detail_generation);
        let explicit_detail_item = Arc::clone(&explicit_detail_item);
        window.on_detail_back_requested(move || {
            // Invalidate an in-flight dynamic preview before hiding it. A
            // late provider response must not reopen the detail surface.
            let close_generation = detail_generation.fetch_add(1, Ordering::AcqRel) + 1;
            if let Ok(mut open_item) = explicit_detail_item.lock() {
                *open_item = None;
            }
            if let Some(window) = weak.upgrade() {
                window.set_detail_panel_visible(false);
                window.set_empty_state_detail_active(false);
                let delayed_window = window.as_weak();
                let detail_generation = Arc::clone(&detail_generation);
                let icon_service = icon_service.clone();
                // Leave one render interval after the 180ms Slint animation
                // before clearing its content and detail layout state.
                Timer::single_shot(Duration::from_millis(200), move || {
                    if detail_generation.load(Ordering::Acquire) != close_generation {
                        return;
                    }
                    if let Some(window) = delayed_window.upgrade() {
                        if !window.get_detail_panel_visible() {
                            set_detail_view(&window, None, &icon_service);
                        }
                    }
                });
            }
        });
    }
    {
        let weak = window.as_weak();
        let icon_service = icon_service.clone();
        let empty_state_item = Arc::clone(&empty_state_item);
        let detail_generation = Arc::clone(&detail_generation);
        window.on_empty_state_activated(move || {
            let Some(item) = empty_state_item.lock().ok().and_then(|state| state.clone()) else {
                return;
            };
            let Some(detail) = item.detail.as_ref() else {
                return;
            };
            detail_generation.fetch_add(1, Ordering::AcqRel);
            if let Some(window) = weak.upgrade() {
                set_detail_view(&window, Some(detail), &icon_service);
                window.set_empty_state_detail_active(true);
                window.set_detail_panel_visible(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        let active_session = Arc::clone(&active_session);
        window.on_shortcut_back_requested(move || {
            if let Some(window) = weak.upgrade() {
                // A search engine only changes the search context. Its text
                // field is shared with the default engine, so retain the
                // current query when returning.
                window.set_shortcut_session_active(false);
                window.set_shortcut_session_execute_mode(false);
                window.set_shortcut_session_prefix(slint::SharedString::default());
                window.set_shortcut_session_placeholder(slint::SharedString::default());
                if let Ok(mut session) = active_session.lock() {
                    *session = None;
                }
                let query = window.get_query_text();
                let delayed_window = weak.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    if let Some(window) = delayed_window.upgrade() {
                        window.set_query_text(query.clone());
                        window.invoke_query_changed(query);
                    }
                });
            }
        });
    }
    {
        let weak = window.as_weak();
        let query_history = Arc::clone(&query_history);
        let active_session = Arc::clone(&active_session);
        window.on_query_history_up_requested(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            let Ok(mut history) = query_history.lock() else {
                return;
            };
            if history.0.is_empty() {
                return;
            }
            history.1 = history.1.saturating_sub(1);
            let Some(entry) = history.0.get(history.1).cloned() else {
                return;
            };
            if let Ok(mut active) = active_session.lock() {
                *active = entry.session.clone();
            }
            apply_search_engine(&window, entry.session.as_ref());
            let cursor = entry.query.len() as i32;
            let query = slint::SharedString::from(entry.query);
            let delayed_window = window.as_weak();
            Timer::single_shot(Duration::ZERO, move || {
                if let Some(window) = delayed_window.upgrade() {
                    window.set_query_text(query.clone());
                    window.set_query_history_cursor(cursor);
                    window.set_query_history_restore_id(
                        window.get_query_history_restore_id().wrapping_add(1),
                    );
                    window.invoke_query_changed(query);
                }
            });
        });
    }
    {
        let weak = window.as_weak();
        window.on_search_back_requested(move || {
            if let Some(window) = weak.upgrade() {
                window.invoke_shortcut_back_requested();
            }
        });
    }
    {
        let engine = Arc::clone(&engine);
        let weak = window.as_weak();
        let visible = Arc::clone(&visible);
        let active_session = Arc::clone(&active_session);
        let query_history = Arc::clone(&query_history);
        let action_generation = Arc::clone(&query_generation);
        let executing = Arc::new(AtomicBool::new(false));
        window.on_session_execute_requested(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            let Some(mut session) = active_session
                .lock()
                .ok()
                .and_then(|session| session.clone())
            else {
                return;
            };
            if session.mode != crate::page::SearchEngineMode::Execute {
                return;
            }
            if executing.swap(true, Ordering::AcqRel) {
                return;
            }
            let query = window.get_query_text().to_string();
            session.argument = query.clone();
            let context = crate::page::SearchContext::Engine(session.clone());
            let request_generation = action_generation.load(Ordering::Acquire);
            let engine = Arc::clone(&engine);
            let weak = weak.clone();
            let active_session = Arc::clone(&active_session);
            let query_history = Arc::clone(&query_history);
            let visible = Arc::clone(&visible);
            let action_generation = Arc::clone(&action_generation);
            let executing = Arc::clone(&executing);
            crate::utils::background::spawn(move || {
                let result = engine
                    .search_in_context(&context, &query)
                    .into_iter()
                    .find(|item| item.primary_action().is_some())
                    .map(|item| launcher::launch(&item).map(|()| item));
                if let Some(Ok(item)) = &result {
                    engine.record_action_success(&item.id, &query, Some(&session));
                    record_query_history(&query_history, &query, Some(session));
                }
                let reset_executing = Arc::clone(&executing);
                if slint::invoke_from_event_loop(move || {
                    reset_executing.store(false, Ordering::Release);
                    if action_generation.load(Ordering::Acquire) != request_generation {
                        return;
                    }
                    let Some(window) = weak.upgrade() else {
                        return;
                    };
                    match result {
                        Some(Ok(_)) => {
                            window.set_launch_error(String::new().into());
                            clear_query(&window, &active_session);
                            let _ = window.hide();
                            visible.store(false, Ordering::Release);
                        }
                        Some(Err(error)) => window.set_launch_error(error.to_string().into()),
                        None => {}
                    }
                })
                .is_err()
                {
                    executing.store(false, Ordering::Release);
                }
            });
        });
    }
    {
        let weak = window.as_weak();
        let current_language = Arc::clone(&current_language);
        window.on_detail_link_requested(move |url| {
            let result = launcher::open_url(url.as_str());
            if result.is_err() {
                if let Some(window) = weak.upgrade() {
                    let language = current_language
                        .lock()
                        .map(|language| language.clone())
                        .unwrap_or_else(|_| "system".to_owned());
                    window.set_launch_error(
                        i18n::tr(&language, "Could not open preview link").into(),
                    );
                }
            }
        });
    }
    {
        let engine = engine.clone();
        let weak = window.as_weak();
        let active_session = Arc::clone(&active_session);
        let query_history = Arc::clone(&query_history);
        window.on_normalize_query(move |query| {
            engine.cancel_active_queries();
            let query = query.to_string();
            if let Ok(mut history) = query_history.lock() {
                history.1 = history.0.len();
            }
            let session = active_session
                .lock()
                .ok()
                .and_then(|session| session.clone());
            if let Some(mut session) = session {
                session.argument = query.trim().to_owned();
                if let Ok(mut active) = active_session.lock() {
                    *active = Some(session);
                }
                return query.into();
            }
            let Some(session) = engine.search_context(&query).session().cloned() else {
                return query.into();
            };
            if let Ok(mut active) = active_session.lock() {
                *active = Some(session.clone());
            }
            if let Some(window) = weak.upgrade() {
                apply_search_engine(&window, Some(&session));
            }
            session.argument.into()
        });
    }
    {
        let engine = engine.clone();
        let icon_service = icon_service.clone();
        let icon_items = Arc::clone(&icon_items);
        let icon_requests = Arc::clone(&icon_requests);
        let result_filter = Arc::clone(&result_filter);
        let query_generation = Arc::clone(&query_generation);
        let current_query = Arc::clone(&current_query);
        let active_session = Arc::clone(&active_session);
        let show_advanced_details = Arc::clone(&show_advanced_details);
        let search_icon_generation = Arc::clone(&search_icon_generation);
        let detail_registry = detail_registry.clone();
        let detail_generation = Arc::clone(&detail_generation);
        let explicit_detail_item = Arc::clone(&explicit_detail_item);
        let empty_state_item = Arc::clone(&empty_state_item);
        let platform_for_compact = Arc::clone(&platform);
        let weak = window.as_weak();
        window.on_compact_results_requested(move || {
            if let Some(window) = weak.upgrade() {
                if restore_result_window_height(&window) {
                    platform_for_compact
                        .position_palette(window.window(), window.get_result_window_height());
                }
            }
        });
        let platform_for_query = Arc::clone(&platform);
        let weak = window.as_weak();
        window.on_query_changed(move |query| {
            engine.cancel_active_queries();
            // Invalidate an in-flight detail request immediately. The result
            // list for the new query may arrive slightly later on the worker
            // thread, so waiting for selection changes here can let stale
            // weather data briefly overwrite the new query's detail panel.
            detail_generation.fetch_add(1, Ordering::AcqRel);
            if let Ok(mut open_item) = explicit_detail_item.lock() {
                *open_item = None;
            }
            if let Ok(mut current) = current_query.lock() {
                *current = query.to_string();
            }
            if let Some(window) = weak.upgrade() {
                window.set_empty_state_icon_generation(
                    window.get_empty_state_icon_generation().wrapping_add(1),
                );
                window.set_empty_state_icon(slint::Image::default());
                window.set_detail_content_generation(
                    window.get_detail_content_generation().wrapping_add(1),
                );
                let native_window = window.window();
                let current_height = native_window
                    .size()
                    .to_logical(native_window.scale_factor())
                    .height;
                let root_query_empty = query.trim().is_empty();
                window.set_root_query_empty(root_query_empty);
                if root_query_empty && !window.get_shortcut_session_active() {
                    window.set_empty_query_results_visible(false);
                }
                window.set_query_results_ready(false);
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
                if restore_result_window_height(&window) {
                    platform_for_query
                        .position_palette(window.window(), window.get_result_window_height());
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
            let result_filter = Arc::clone(&result_filter);
            let weak = weak.clone();
            let query_generation = Arc::clone(&query_generation);
            let search_icon_generation = Arc::clone(&search_icon_generation);
            let show_advanced_details_for_query = Arc::clone(&show_advanced_details);
            let detail_registry_for_query = detail_registry.clone();
            let active_session_for_query = active_session
                .lock()
                .ok()
                .and_then(|session| session.clone());
            let search_context_for_query = active_session_for_query
                .clone()
                .map(crate::page::SearchContext::Engine)
                .unwrap_or_else(|| engine.search_context(query.as_str()));
            // Empty-session menus have no user query to debounce, so display
            // them as soon as their worker can run.
            let debounce_delay = if query.trim().is_empty() && active_session_for_query.is_some() {
                Duration::ZERO
            } else {
                engine.search_debounce(&search_context_for_query)
            };
            let empty_state_item_for_query = Arc::clone(&empty_state_item);
            // Debounce before creating a worker. This avoids one sleeping OS
            // thread per keystroke while a plugin-backed search is settling.
            Timer::single_shot(debounce_delay, move || {
                if query_generation.load(Ordering::Acquire) != query_id {
                    return;
                }
                crate::utils::background::spawn(move || {
                    let presentation = build_search_presentation(
                        &engine,
                        query.as_str(),
                        active_session_for_query.as_ref(),
                        show_advanced_details_for_query.load(Ordering::Acquire),
                        &icon_service,
                    );
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak.upgrade() {
                            apply_search_presentation(
                                &window,
                                query_id,
                                &query,
                                presentation,
                                SelectionPolicy::First,
                                PresentationTargets {
                                    engine: &engine,
                                    icon_service: &icon_service,
                                    filter: &result_filter,
                                    icon_items: &icon_items,
                                    icon_requests: &icon_requests,
                                    empty_state_item: &empty_state_item_for_query,
                                    detail_registry: &detail_registry_for_query,
                                    query_generation: &query_generation,
                                    search_icon_generation: &search_icon_generation,
                                },
                            );
                        }
                    });
                });
            });
        });
    }
    {
        let engine = engine.clone();
        let icon_service = icon_service.clone();
        let weak = window.as_weak();
        let detail_registry = detail_registry.clone();
        let icon_items = Arc::clone(&icon_items);
        let show_advanced_launch_entries = Arc::clone(&show_advanced_launch_entries);
        let detail_generation = Arc::clone(&detail_generation);
        let active_session = Arc::clone(&active_session);
        let explicit_detail_item = Arc::clone(&explicit_detail_item);
        let pending_selection = Rc::new(Cell::new(None::<i32>));
        let selection_queued = Rc::new(Cell::new(false));
        window.on_selection_changed(move |index| {
            pending_selection.set(Some(index));
            if selection_queued.replace(true) {
                return;
            }
            let pending_selection = Rc::clone(&pending_selection);
            let selection_queued = Rc::clone(&selection_queued);
            let engine = engine.clone();
            let icon_service = icon_service.clone();
            let weak = weak.clone();
            let detail_registry = detail_registry.clone();
            let icon_items = Arc::clone(&icon_items);
            let show_advanced_launch_entries = Arc::clone(&show_advanced_launch_entries);
            let detail_generation = Arc::clone(&detail_generation);
            let active_session = Arc::clone(&active_session);
            let explicit_detail_item = Arc::clone(&explicit_detail_item);
            Timer::single_shot(Duration::ZERO, move || {
                selection_queued.set(false);
                let Some(index) = pending_selection.take() else {
                    return;
                };
                let item = weak
                    .upgrade()
                    .and_then(|window| window.get_results().row_data(index as usize))
                    .and_then(|row| {
                        engine.by_id(row.id.as_str()).or_else(|| {
                            icon_items.lock().ok().and_then(|state| {
                                state.1.iter().find(|item| row.id == item.id).cloned()
                            })
                        })
                    });
                let selection_keeps_explicit_detail =
                    explicit_detail_item.lock().ok().is_some_and(|open_item| {
                        explicit_detail_matches(
                            open_item.as_deref(),
                            item.as_ref().map(|item| item.id.as_str()),
                        )
                    });
                if !selection_keeps_explicit_detail {
                    // Selection only changes local preview state. It also
                    // invalidates an in-flight detail request for the previous
                    // result, unless this selection was initiated by opening
                    // that exact detail panel.
                    detail_generation.fetch_add(1, Ordering::AcqRel);
                    if let Ok(mut open_item) = explicit_detail_item.lock() {
                        *open_item = None;
                    }
                }
                let has_dynamic_detail = item
                    .as_ref()
                    .is_some_and(|item| detail_registry.provider_for(item).is_some());
                if let Some(window) = weak.upgrade() {
                    let context_menu_available = item.as_ref().is_some_and(|item| {
                        let session = active_session
                            .lock()
                            .ok()
                            .and_then(|session| session.clone());
                        !context_actions(
                            item,
                            show_advanced_launch_entries.load(Ordering::Acquire),
                            engine.is_favorite(&item.id),
                            can_favorite_item(
                                item,
                                session.as_ref(),
                                window.get_shortcut_session_active(),
                                window.get_query_text().as_str(),
                            ),
                        )
                        .is_empty()
                    });
                    let detail = item
                        .as_ref()
                        .filter(|item| {
                            !has_dynamic_detail
                                && item.visibility != result::ResultVisibility::EmptyQueryOnly
                        })
                        .and_then(|item| item.detail.as_ref());
                    window.set_context_menu_available(context_menu_available);
                    if !selection_keeps_explicit_detail {
                        apply_search_detail_state(&window, detail, detail.is_some(), &icon_service);
                    }
                }
            });
        });
    }
    {
        let engine = engine.clone();
        let icon_service = icon_service.clone();
        let weak = window.as_weak();
        let visible = Arc::clone(&visible);
        let current_query = Arc::clone(&current_query);
        let active_session = Arc::clone(&active_session);
        let query_history = Arc::clone(&query_history);
        let detail_generation = Arc::clone(&detail_generation);
        let explicit_detail_item = Arc::clone(&explicit_detail_item);
        let detail_registry = detail_registry.clone();
        let detail_language = Arc::clone(&current_language);
        let icon_items = Arc::clone(&icon_items);
        let action_generation = Arc::clone(&query_generation);
        let action_in_flight = Arc::new(AtomicBool::new(false));
        window.on_launch_requested(move |index| {
            let item = weak.upgrade().and_then(|window| {
                window
                    .get_results()
                    .row_data(index as usize)
                    .and_then(|row| {
                        engine.by_id(row.id.as_str()).or_else(|| {
                            icon_items.lock().ok().and_then(|state| {
                                state.1.iter().find(|item| row.id == item.id).cloned()
                            })
                        })
                    })
            });
            if let Some(item) = item {
                if let Some(result::Action::EnterSearchEngine { prefix }) = item.primary_action() {
                    let query = weak
                        .upgrade()
                        .map(|window| window.get_query_text().to_string())
                        .or_else(|| current_query.lock().ok().map(|query| query.clone()))
                        .unwrap_or_default();
                    let session = engine
                        .search_context(&format!("{}{}", prefix, query.trim()))
                        .session()
                        .cloned();
                    if let Some(window) = weak.upgrade() {
                        if let Ok(mut active) = active_session.lock() {
                            *active = session.clone();
                        }
                        let argument = query
                            .strip_prefix(prefix.as_str())
                            .unwrap_or(query.as_str())
                            .trim();
                        if let Some(session) = session.as_ref() {
                            apply_search_engine(&window, Some(session));
                        } else {
                            window.set_shortcut_session_active(true);
                            window.set_shortcut_session_prefix(prefix.clone().into());
                            window.set_shortcut_session_placeholder(slint::SharedString::default());
                        }
                        let query = slint::SharedString::from(argument.to_owned());
                        let weak = window.as_weak();
                        Timer::single_shot(Duration::ZERO, move || {
                            if let Some(window) = weak.upgrade() {
                                window.set_query_text(query.clone());
                                window.invoke_query_changed(query);
                            }
                        });
                    }
                    return;
                }
                if let Some(result::Action::UpdateSession { updates }) = item.primary_action() {
                    if active_session
                        .lock()
                        .ok()
                        .is_some_and(|session| session.is_none())
                    {
                        if let Some(session) = engine.favorite_session(&item.id) {
                            if let Ok(mut active) = active_session.lock() {
                                *active = Some(session.clone());
                            }
                            if let Some(window) = weak.upgrade() {
                                apply_search_engine(&window, Some(&session));
                            }
                        }
                    }
                    let Some(mut session) = active_session
                        .lock()
                        .ok()
                        .and_then(|session| session.clone())
                    else {
                        return;
                    };
                    for (key, value) in updates {
                        if value.is_empty() {
                            session.state.remove(key);
                        } else {
                            session.state.insert(key.clone(), value.clone());
                        }
                    }
                    if let Ok(mut active) = active_session.lock() {
                        *active = Some(session);
                    }
                    if let Some(window) = weak.upgrade() {
                        let query = window.get_query_text();
                        window.invoke_query_changed(query);
                    }
                    return;
                }
                let open_shortcut_panel = current_query
                    .lock()
                    .ok()
                    .map(|query| engine.search_context(&query))
                    .and_then(|context| context.session().cloned())
                    .is_some_and(|session| session.mode == crate::page::SearchEngineMode::Panel);
                let has_dynamic_detail = detail_registry.provider_for(&item).is_some();
                let open_detail_action = item
                    .primary_action()
                    .is_some_and(result::Action::is_open_detail);
                if (open_shortcut_panel || item.primary_action().is_none())
                    && (item.detail.is_some() || has_dynamic_detail)
                    || open_detail_action
                {
                    // Cancel a pending visual-close cleanup if another
                    // detail was opened before its fade-out completed.
                    detail_generation.fetch_add(1, Ordering::AcqRel);
                    if let Ok(mut open_item) = explicit_detail_item.lock() {
                        *open_item = Some(item.id.clone());
                    }
                    if let Some(window) = weak.upgrade() {
                        window.set_selected_index(index);
                        if let Some(detail) = item.detail.as_ref() {
                            set_detail_view(&window, Some(detail), &icon_service);
                        }
                        window.set_detail_panel_visible(true);
                    }
                    if has_dynamic_detail {
                        let language = detail_language
                            .lock()
                            .map(|language| language.clone())
                            .unwrap_or_else(|_| "system".to_owned());
                        request_dynamic_detail(
                            item,
                            detail_registry.clone(),
                            language,
                            Arc::clone(&detail_generation),
                            weak.clone(),
                            icon_service.clone(),
                        );
                    }
                    return;
                }
                if item.primary_action().is_some_and(result::Action::is_plugin) {
                    let Some((_, action_id)) =
                        item.primary_action().and_then(result::Action::plugin_parts)
                    else {
                        return;
                    };
                    if action_in_flight.swap(true, Ordering::AcqRel) {
                        return;
                    }
                    let action_id = action_id.to_owned();
                    let query = weak
                        .upgrade()
                        .map(|window| window.get_query_text().to_string())
                        .unwrap_or_default();
                    let session = active_session
                        .lock()
                        .ok()
                        .and_then(|session| session.clone());
                    let request_generation = action_generation.load(Ordering::Acquire);
                    let engine = Arc::clone(&engine);
                    let weak = weak.clone();
                    let action_generation = Arc::clone(&action_generation);
                    let action_in_flight = Arc::clone(&action_in_flight);
                    crate::utils::background::spawn(move || {
                        let output = engine.execute_transient_plugin_action(&item, &action_id);
                        if output.is_ok() {
                            engine.record_action_success(&item.id, &query, session.as_ref());
                        }
                        let reset_in_flight = Arc::clone(&action_in_flight);
                        if slint::invoke_from_event_loop(move || {
                            reset_in_flight.store(false, Ordering::Release);
                            if action_generation.load(Ordering::Acquire) != request_generation {
                                return;
                            }
                            let Some(window) = weak.upgrade() else {
                                return;
                            };
                            match output {
                                Ok(output) => {
                                    window.set_last_action_id(item.id.into());
                                    window.set_last_action_result(output.title.into());
                                    window.set_last_action_subtitle(output.subtitle.into());
                                    window.set_launch_error(String::new().into());
                                }
                                Err(error) => {
                                    eprintln!("plugin action failed: {error:#}");
                                    window.set_launch_error(error.to_string().into());
                                }
                            }
                        })
                        .is_err()
                        {
                            action_in_flight.store(false, Ordering::Release);
                        }
                    });
                    return;
                }
                if action_in_flight.swap(true, Ordering::AcqRel) {
                    return;
                }
                let query = weak
                    .upgrade()
                    .map(|window| window.get_query_text().to_string())
                    .or_else(|| current_query.lock().ok().map(|query| query.clone()))
                    .unwrap_or_default();
                let session = active_session
                    .lock()
                    .ok()
                    .and_then(|session| session.clone());
                let request_generation = action_generation.load(Ordering::Acquire);
                let engine = Arc::clone(&engine);
                let weak = weak.clone();
                let query_history = Arc::clone(&query_history);
                let active_session = Arc::clone(&active_session);
                let visible = Arc::clone(&visible);
                let action_generation = Arc::clone(&action_generation);
                let action_in_flight = Arc::clone(&action_in_flight);
                let error_language = Arc::clone(&detail_language);
                crate::utils::background::spawn(move || {
                    let result = launcher::launch(&item);
                    if result.is_ok() {
                        engine.record_action_success(&item.id, &query, session.as_ref());
                        record_query_history(&query_history, &query, session);
                    }
                    let reset_in_flight = Arc::clone(&action_in_flight);
                    if slint::invoke_from_event_loop(move || {
                        reset_in_flight.store(false, Ordering::Release);
                        if action_generation.load(Ordering::Acquire) != request_generation {
                            return;
                        }
                        let Some(window) = weak.upgrade() else {
                            return;
                        };
                        match result {
                            Ok(()) => {
                                window.set_launch_error(String::new().into());
                                clear_query(&window, &active_session);
                                let _ = window.hide();
                                visible.store(false, Ordering::Release);
                            }
                            Err(error) => {
                                eprintln!("launch failed: {error:#}");
                                let language = error_language
                                    .lock()
                                    .map(|language| language.clone())
                                    .unwrap_or_else(|_| "system".to_owned());
                                let message = error.to_string();
                                window.set_launch_error(
                                    i18n::tr_owned_in_context(&language, "Launcher", &message)
                                        .into(),
                                );
                                let weak = window.as_weak();
                                let generation = Arc::clone(&action_generation);
                                Timer::single_shot(LAUNCH_ERROR_DURATION, move || {
                                    if generation.load(Ordering::Acquire) == request_generation {
                                        if let Some(window) = weak.upgrade() {
                                            window.set_launch_error(String::new().into());
                                        }
                                    }
                                });
                            }
                        }
                    })
                    .is_err()
                    {
                        action_in_flight.store(false, Ordering::Release);
                    }
                });
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
        let context_language = Arc::clone(&current_language);
        let icon_items = Arc::clone(&icon_items);
        let active_session = Arc::clone(&active_session);
        window.on_context_menu_requested(move |index, keyboard| {
            let Some(item) = weak.upgrade().and_then(|window| {
                window
                    .get_results()
                    .row_data(index as usize)
                    .and_then(|row| {
                        engine.by_id(row.id.as_str()).or_else(|| {
                            icon_items.lock().ok().and_then(|state| {
                                state.1.iter().find(|item| row.id == item.id).cloned()
                            })
                        })
                    })
            }) else {
                return;
            };
            let session = active_session
                .lock()
                .ok()
                .and_then(|session| session.clone());
            let can_add_favorite = weak.upgrade().is_some_and(|window| {
                can_favorite_item(
                    &item,
                    session.as_ref(),
                    window.get_shortcut_session_active(),
                    window.get_query_text().as_str(),
                )
            });
            let actions = context_actions(
                &item,
                show_advanced_launch_entries.load(Ordering::Acquire),
                engine.is_favorite(&item.id),
                can_add_favorite,
            );
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
                let language = context_language
                    .lock()
                    .map(|language| language.clone())
                    .unwrap_or_else(|_| "system".to_owned());
                menu.set_menu_width(context_menu_width(&actions, &language) as f32);
                menu.set_keyboard_invoked(keyboard);
                menu.set_actions(ModelRc::from(Rc::new(VecModel::from(actions))));
                platform::set_window_background(menu.window(), menu.get_native_background_color());
                platform.disable_window_transitions(menu.window());
                let _ = menu.show();
                menu.window().request_redraw();
                let launcher = weak.upgrade();
                let anchor = launcher.as_ref().and_then(|launcher| {
                    keyboard.then(|| {
                        (
                            launcher.get_context_menu_anchor_x(),
                            launcher.get_context_menu_anchor_y(),
                        )
                    })
                });
                if let Some(launcher) = launcher {
                    platform.position_context_menu(menu.window(), launcher.window(), anchor);
                }
                platform.activate_context_menu(menu.window());
                if let Some(anchor) = anchor {
                    // The native window can still report its previous size
                    // on the first show. Reposition once after Slint applies
                    // the new actions and resizes the menu.
                    let platform_for_reposition = Arc::clone(&platform);
                    let menu_for_reposition = context_weak.clone();
                    let launcher_for_reposition = weak.clone();
                    Timer::single_shot(CONTEXT_MENU_LAYOUT_DELAY, move || {
                        if let (Some(menu), Some(launcher)) = (
                            menu_for_reposition.upgrade(),
                            launcher_for_reposition.upgrade(),
                        ) {
                            platform_for_reposition.position_context_menu(
                                menu.window(),
                                launcher.window(),
                                Some(anchor),
                            );
                        }
                    });
                }
            }

            let icon_service = icon_service.clone();
            let context_weak = context_weak.clone();
            let context_generation = Arc::clone(&context_generation);
            crate::utils::background::spawn(move || {
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
        let active_session_for_favorite = Arc::clone(&active_session);
        let action_generation = Arc::clone(&query_generation);
        let favorite_limit_language = Arc::clone(&current_language);
        context_window.on_action_requested(move |action_id| {
            let Some(item) = context_item.lock().ok().and_then(|current| current.clone()) else {
                return;
            };
            if matches!(action_id.as_str(), "add-favorite" | "remove-favorite") {
                let session = active_session_for_favorite
                    .lock()
                    .ok()
                    .and_then(|session| session.clone());
                if action_id == "add-favorite"
                    && !weak.upgrade().is_some_and(|window| {
                        can_favorite_item(
                            &item,
                            session.as_ref(),
                            window.get_shortcut_session_active(),
                            window.get_query_text().as_str(),
                        )
                    })
                {
                    return;
                }
                let favorite_result = engine.toggle_favorite(&item, session.as_ref());
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
                    match favorite_result {
                        Some(_) => {
                            window.set_launch_error(String::new().into());
                            window.invoke_query_changed(window.get_query_text());
                        }
                        None => {
                            let language = favorite_limit_language
                                .lock()
                                .map(|language| language.clone())
                                .unwrap_or_else(|_| "system".to_owned());
                            window.set_launch_error(
                                i18n::tr(&language, "Maximum number of favorites reached").into(),
                            );
                        }
                    }
                }
                return;
            }
            let copy_text = match action_id.as_str() {
                "copy-title" => Some(item.title.clone()),
                "copy-path" | "copy-url" => Some(item.launch_target.to_string_lossy().into_owned()),
                _ => None,
            };
            if let Some(text) = copy_text {
                let result = crate::utils::clipboard::set_text(&text);
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
                        Ok(()) => window.set_launch_error(String::new().into()),
                        Err(error) => window.set_launch_error(error.to_string().into()),
                    }
                }
                return;
            }
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
                crate::utils::background::spawn(move || {
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
            if let Some(context_action_id) = action_id.strip_prefix("custom-context:") {
                let Some(context_action) = item.actions.iter().find_map(|action| {
                    let result::Action::ContextMenu(action) = action else {
                        return None;
                    };
                    (action.id == context_action_id).then(|| action.clone())
                }) else {
                    return;
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
                let result = match context_action.behavior {
                    result::ContextMenuBehavior::CopyText { text } => {
                        crate::utils::clipboard::set_text(&text)
                    }
                    result::ContextMenuBehavior::OpenUrl { url } => launcher::open_url(&url),
                    result::ContextMenuBehavior::Launch { entry, as_admin } => {
                        launcher::launch_entry(&entry, as_admin)
                    }
                    result::ContextMenuBehavior::Plugin { .. } => {
                        execute_context_plugin_action(
                            Arc::clone(&engine),
                            item,
                            format!("context-menu:{context_action_id}"),
                            weak.clone(),
                            Arc::clone(&action_generation),
                        );
                        return;
                    }
                };
                if let Some(window) = weak.upgrade() {
                    match result {
                        Ok(()) => window.set_launch_error(String::new().into()),
                        Err(error) => window.set_launch_error(error.to_string().into()),
                    }
                }
                return;
            }
            if let Some(plugin_action_id) = action_id.strip_prefix("plugin-action:") {
                execute_context_plugin_action(
                    Arc::clone(&engine),
                    item,
                    plugin_action_id.to_owned(),
                    weak.clone(),
                    Arc::clone(&action_generation),
                );
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
            let result = match action_id.as_str() {
                "run-as-admin" => {
                    let entry = result::LaunchEntry {
                        source: item.source.clone(),
                        path: item.launch_target.clone(),
                        target: item.launch_target.clone(),
                        arguments: item.launch_arguments.clone(),
                        working_directory: item.launch_working_directory.clone(),
                        icon: None,
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
        let result_filter = Arc::clone(&result_filter);
        let weak = window.as_weak();
        let query_generation = Arc::clone(&query_generation);
        let current_query = Arc::clone(&current_query);
        let show_advanced_details = Arc::clone(&show_advanced_details);
        let show_advanced_launch_entries = Arc::clone(&show_advanced_launch_entries);
        let active_session = Arc::clone(&active_session);
        let config_reload = settings_reload::SettingsReloadContext {
            engine: Arc::clone(&engine),
            hotkey_updates: hotkey_updates.clone(),
            tray_updates: tray_updates.clone(),
            follow_system_theme: Arc::clone(&follow_system_theme),
            hide_on_focus_loss: Arc::clone(&hide_on_focus_loss),
            custom_directories: Arc::clone(&custom_directories),
            show_advanced_details: Arc::clone(&show_advanced_details),
            show_advanced_launch_entries: Arc::clone(&show_advanced_launch_entries),
            platform: Arc::clone(&platform),
            window: window.as_weak(),
            settings_window: settings_window.as_weak(),
            context_window: context_window.as_weak(),
            plugin_settings: plugin_settings.clone(),
            current_query: Arc::clone(&current_query),
            current_language: Arc::clone(&current_language),
        };
        let detail_registry = detail_registry.clone();
        let search_icon_generation = Arc::clone(&search_icon_generation);
        std::thread::spawn(move || {
            provider.discover_initial();
            let mut next_shell_reconciliation_check =
                std::time::Instant::now() + Duration::from_secs(60);
            let mut provider_content_generation = engine.provider_content_generation();
            let mut initial_refresh = true;
            loop {
                let mut changed = if initial_refresh {
                    initial_refresh = false;
                    true
                } else {
                    match app_source_events.recv_timeout(PROVIDER_REFRESH_INTERVAL) {
                        Ok(source) => {
                            let mut sources = BTreeSet::from([source]);
                            while let Ok(source) = app_source_events.try_recv() {
                                sources.insert(source);
                            }
                            let mut changed = false;
                            for source in sources {
                                changed |= provider.refresh_source(source);
                            }
                            changed
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let now = std::time::Instant::now();
                            if now < next_shell_reconciliation_check {
                                false
                            } else {
                                next_shell_reconciliation_check = now + Duration::from_secs(60);
                                let mut changed = false;
                                for source in provider.shell_sources_due_for_reconciliation() {
                                    changed |= provider.refresh_source(source);
                                }
                                changed
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                };
                let current_provider_generation = engine.provider_content_generation();
                if current_provider_generation != provider_content_generation {
                    provider_content_generation = current_provider_generation;
                    changed = true;
                }
                if config_events.try_recv().is_ok() {
                    let (updated, valid) = config::Config::load();
                    changed |= settings_reload::apply_if_valid(valid, || {
                        config_reload.apply(&updated, &mut provider)
                    });
                }
                if changed {
                    let indexed = provider.snapshot();
                    engine.replace_results(indexed);
                    let query_id = query_generation.fetch_add(1, Ordering::AcqRel) + 1;
                    let query = current_query
                        .lock()
                        .map(|value| value.clone())
                        .unwrap_or_default();
                    let presentation = build_search_presentation(
                        &engine,
                        &query,
                        active_session
                            .lock()
                            .ok()
                            .and_then(|session| session.clone())
                            .as_ref(),
                        show_advanced_details.load(Ordering::Acquire),
                        &icon_service,
                    );
                    let engine_for_ui = Arc::clone(&engine);
                    let query_generation_for_ui = Arc::clone(&query_generation);
                    let weak_for_ui = weak.clone();
                    let icon_items_for_ui = Arc::clone(&icon_items);
                    let icon_requests_for_ui = Arc::clone(&icon_requests);
                    let result_filter_for_ui = Arc::clone(&result_filter);
                    let detail_registry_for_ui = detail_registry.clone();
                    let icon_service_for_ui = icon_service.clone();
                    let empty_state_item_for_results = Arc::clone(&empty_state_item);
                    let search_icon_generation_for_ui = Arc::clone(&search_icon_generation);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak_for_ui.upgrade() {
                            apply_search_presentation(
                                &window,
                                query_id,
                                &query,
                                presentation,
                                SelectionPolicy::Preserve,
                                PresentationTargets {
                                    engine: &engine_for_ui,
                                    icon_service: &icon_service_for_ui,
                                    filter: &result_filter_for_ui,
                                    icon_items: &icon_items_for_ui,
                                    icon_requests: &icon_requests_for_ui,
                                    empty_state_item: &empty_state_item_for_results,
                                    detail_registry: &detail_registry_for_ui,
                                    query_generation: &query_generation_for_ui,
                                    search_icon_generation: &search_icon_generation_for_ui,
                                },
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
                    let weak = weak.clone();
                    let visible = Arc::clone(&visible);
                    // Window focus events arrive from inside Slint's winit
                    // event dispatch. Defer hiding until that dispatch has
                    // returned so hiding cannot re-enter tree instantiation.
                    Timer::single_shot(Duration::ZERO, move || {
                        if !visible.load(Ordering::Acquire) {
                            if let Some(window) = weak.upgrade() {
                                let _ = window.hide();
                            }
                        }
                    });
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
                Timer::single_shot(FOCUS_LOSS_DELAY, move || {
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
        platform::set_window_background(window.window(), window.get_native_background_color());
        platform.disable_window_transitions(window.window());
        window.show()?;
        window.window().request_redraw();
        visible.store(true, Ordering::Release);
        let weak = window.as_weak();
        let platform = Arc::clone(&platform);
        Timer::single_shot(PALETTE_POSITION_DELAY, move || {
            if let Some(window) = weak.upgrade() {
                platform.position_palette(window.window(), window.get_result_window_height());
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
    let settings_weak_for_tray = settings_window.as_weak();
    let plugin_settings_for_tray = plugin_settings.clone();
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
            let settings_weak = settings_weak_for_tray.clone();
            let plugin_settings = plugin_settings_for_tray.clone();
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
                    let dark_mode = platform.system_prefers_dark_theme();
                    let window = weak.upgrade();
                    let settings = settings_weak.upgrade();
                    let context = context_weak.upgrade();
                    apply_theme_to_windows(
                        dark_mode,
                        platform.as_ref(),
                        window.as_ref(),
                        settings.as_ref(),
                        context.as_ref(),
                    );
                }
                platform::PlatformEvent::SystemThemeChanged => {}
                platform::PlatformEvent::OpenSettings => {
                    hide_context_menu(
                        &context_weak,
                        &weak,
                        &context_visible,
                        &context_generation,
                        &context_item,
                        false,
                        &visible,
                    );
                    if visible.swap(false, Ordering::AcqRel) {
                        if let Some(window) = weak.upgrade() {
                            save_window_size(&window);
                            let _ = window.hide();
                        }
                    }
                    if let Some(settings) = settings_weak.upgrade() {
                        open_settings_window(&settings, &plugin_settings, platform);
                    }
                }
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
    platform_shutdown.stop_and_join();
    engine.flush_history();
    Ok(())
}
