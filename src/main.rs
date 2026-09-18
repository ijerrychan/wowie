mod classifier;
mod config;
mod i18n;
mod icon_service;
mod launcher;
mod platform;
mod plugin;
mod providers;
mod result;
mod search;
mod shortcuts;

use anyhow::Result;
use icon_service::IconService;
use providers::app::AppProvider;
use result::{ResultCategory, SearchResult};
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
    let tray_events = platform.start_tray(i18n::exit_name(&config.language).to_owned());
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
    let mut provider = AppProvider::discover_with_config(&config.providers);
    let custom_directories = Arc::new(Mutex::new(config.providers.indexed_directories.clone()));
    let app_source_events = AppProvider::watch_sources(Arc::clone(&custom_directories));
    let config_events = config::watch_changes();
    let engine = search::SearchCore::with_plugins_and_shortcuts(
        provider.snapshot(),
        plugin::PluginManager::discover_with_locale(&config.language),
        config.shortcuts.clone(),
    );
    let engine = Arc::new(engine);
    engine.replace_shortcut_language(config.language.clone());
    let icon_service = IconService::new();
    let results = Rc::new(VecModel::<ResultRow>::default());
    let visible = Arc::new(AtomicBool::new(false));
    let context_visible = Arc::new(AtomicBool::new(false));
    let context_generation = Arc::new(AtomicU64::new(0));
    let context_item = Arc::new(Mutex::new(None::<SearchResult>));
    let show_advanced_details = Arc::new(AtomicBool::new(config.show_advanced_details));
    let follow_system_theme = config.theme == "system";
    let query_generation = Arc::new(AtomicU64::new(0));
    let current_query = Arc::new(Mutex::new(String::new()));
    let icon_items = Arc::new(Mutex::new((0_u64, Vec::<SearchResult>::new())));
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
        let icon_service = icon_service.clone();
        let icon_items = Arc::clone(&icon_items);
        let icon_requests = Arc::clone(&icon_requests);
        let weak = window.as_weak();
        let query_generation = Arc::clone(&query_generation);
        let current_query = Arc::clone(&current_query);
        let show_advanced_details = Arc::clone(&show_advanced_details);
        window.on_query_changed(move |query| {
            if let Ok(mut current) = current_query.lock() {
                *current = query.to_string();
            }
            if let Some(window) = weak.upgrade() {
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
            let show_advanced_details_for_query = Arc::clone(&show_advanced_details);
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
                let found_for_icons = found;
                let _ = slint::invoke_from_event_loop(move || {
                    if query_generation.load(Ordering::Acquire) != query_id {
                        return;
                    }
                    if let Some(window) = weak.upgrade() {
                        if let Ok(mut state) = icon_items.lock() {
                            *state = (query_id, found_for_icons);
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
                        window.set_selected_index(if count > 0 { 0 } else { -1 });
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
        let visible = Arc::clone(&visible);
        window.on_launch_requested(move |index| {
            let item = weak.upgrade().and_then(|window| {
                window
                    .get_results()
                    .row_data(index as usize)
                    .and_then(|row| engine.by_id(row.id.as_str()))
            });
            if let Some(item) = item {
                if matches!(item.actions.first(), Some(result::Action::Plugin { .. })) {
                    match engine.execute_action(&item.id) {
                        Ok(output) => {
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
        window.on_context_menu_requested(move |index| {
            let Some(item) = weak.upgrade().and_then(|window| {
                window
                    .get_results()
                    .row_data(index as usize)
                    .and_then(|row| engine.by_id(row.id.as_str()))
            }) else {
                return;
            };
            let actions = context_actions(&item);
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
        context_window.on_action_requested(move |action_id| {
            let Some(item) = context_item.lock().ok().and_then(|current| current.clone()) else {
                return;
            };
            let result = match action_id.as_str() {
                "open-directory" => launcher::open_containing_directory(&item),
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
                    .and_then(|index| item.actions.get(index))
                    .and_then(|action| match action {
                        result::Action::Launch(entry) => Some(launcher::launch_entry(entry, false)),
                        result::Action::Plugin { .. } => None,
                    })
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
        std::thread::spawn(move || loop {
            let mut changed = match app_source_events.recv_timeout(Duration::from_millis(200)) {
                Ok(source) => provider.refresh_source(source),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if config_events.try_recv().is_ok() {
                let (updated, valid) = config::Config::load();
                if valid {
                    if let Ok(mut directories) = custom_directories.lock() {
                        *directories = updated.providers.indexed_directories.clone();
                    }
                    provider.apply_config(&updated.providers);
                    engine.replace_shortcuts(updated.shortcuts);
                    engine.replace_shortcut_language(updated.language);
                    show_advanced_details.store(updated.show_advanced_details, Ordering::Release);
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
                        ResultRowPayload::from(item, show_advanced_details.load(Ordering::Acquire))
                    })
                    .collect::<Vec<_>>();
                let found_for_icons = found;
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
                            *state = (query_id, found_for_icons);
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
        });
    }

    let hotkey_events = platform.register_hotkey(&config.hotkey, follow_system_theme)?;
    if config.hide_on_focus_loss {
        let weak = window.as_weak();
        let visible = Arc::clone(&visible);
        let context_visible = Arc::clone(&context_visible);
        platform.install_focus_listener(
            window.window(),
            Box::new(move || {
                if context_visible.load(Ordering::Acquire) {
                    return;
                }
                if visible.swap(false, Ordering::AcqRel) {
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
        let hide_on_focus_loss = config.hide_on_focus_loss;
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
                            !launcher_focused && hide_on_focus_loss,
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
            for event in hotkey_events {
                if sender.send(event).is_err() {
                    break;
                }
            }
        });
    }
    std::thread::spawn(move || {
        for event in tray_events {
            if platform_event_sender.send(event).is_err() {
                break;
            }
        }
    });

    let weak = window.as_weak();
    let platform_for_hotkey = Arc::clone(&platform);
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
                platform::PlatformEvent::SystemThemeChanged if follow_system_theme => {
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

fn create_window(config: &config::Config) -> Result<Launcher> {
    select_renderer(&config.renderer)?;
    match Launcher::new() {
        Ok(window) => Ok(window),
        Err(error) if config.renderer == "skia" => {
            eprintln!("Skia renderer could not create a window: {error}; falling back to software renderer");
            select_renderer("software")?;
            Ok(Launcher::new()?)
        }
        Err(error) => Err(error.into()),
    }
}

fn select_renderer(renderer: &str) -> Result<()> {
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

fn context_actions(item: &SearchResult) -> Vec<ContextActionRow> {
    let is_filesystem_target = is_supported_filesystem_target(&item.launch_target);
    let is_store_shell_target = is_apps_folder_target(&item.launch_target);
    let mut actions = Vec::new();
    if is_filesystem_target {
        actions.push(ContextActionRow {
            id: "open-directory".into(),
            title: "Open containing folder".into(),
            subtitle: "".into(),
            kind: "common".into(),
        });
    }
    if is_filesystem_target || is_store_shell_target {
        actions.push(ContextActionRow {
            id: "run-as-admin".into(),
            title: "Run as administrator".into(),
            subtitle: "".into(),
            kind: "common".into(),
        });
    }
    // ShortcutProvider actions are direct query actions and intentionally do
    // not populate the indexed launch_entries field. They must not be shown
    // as alternate application launch entries.
    let other_launches = if item.launch_entries.is_empty() {
        Vec::new()
    } else {
        item.actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                let result::Action::Launch(entry) = action else {
                    return None;
                };
                (!crate::providers::app::is_default_launch_entry(item, entry))
                    .then_some((index, entry))
            })
            .collect::<Vec<_>>()
    };
    if !other_launches.is_empty() {
        actions.push(ContextActionRow {
            id: "section:other-launch-entries".into(),
            title: "".into(),
            subtitle: "".into(),
            kind: "section".into(),
        });
        for (index, entry) in other_launches {
            actions.push(ContextActionRow {
                id: format!("launch:{index}").into(),
                title: launch_entry_label(entry).into(),
                subtitle: "".into(),
                kind: "launch".into(),
            });
        }
    }
    actions
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
                "store-application"
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
    icon_items: &Arc<Mutex<(u64, Vec<SearchResult>)>>,
    icon_requests: &Arc<Mutex<(u64, HashSet<usize>)>>,
    icon_service: &IconService,
    weak: &slint::Weak<Launcher>,
    query_generation: &Arc<AtomicU64>,
) {
    let (query_id, items) = icon_items
        .lock()
        .map(|state| (state.0, state.1.clone()))
        .unwrap_or_default();
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
        for (index, item) in pending {
            if query_generation.load(Ordering::Acquire) != query_id {
                return;
            }
            let Some(icon) = icon_service.resolve(&item) else {
                continue;
            };
            let weak_for_callback = weak.clone();
            let query_generation_for_callback = Arc::clone(&query_generation);
            let _ = slint::invoke_from_event_loop(move || {
                if query_generation_for_callback.load(Ordering::Acquire) != query_id {
                    return;
                }
                let Some(window) = weak_for_callback.upgrade() else {
                    return;
                };
                let model = window.get_results();
                let Some(mut row) = model.row_data(index) else {
                    return;
                };
                row.icon = icon_to_image(&icon);
                model.set_row_data(index, row);
            });
        }
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
            icon: None,
            ranking_bias: 0,
        };

        assert!(context_actions(&item)
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
