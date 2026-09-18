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
    let (config, config_was_valid) = config::Config::load();
    let window = create_window(&config)?;
    if config.language != "system" {
        slint::select_bundled_translation(&config.language)
            .map_err(|error| anyhow::anyhow!("could not select language: {error}"))?;
    }
    let platform = platform::current();
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
    let engine = search::SearchCore::with_plugins(
        provider.snapshot(),
        plugin::PluginManager::discover_with_locale(&config.language),
    );
    let engine = Arc::new(engine);
    let icon_service = IconService::new();
    let results = Rc::new(VecModel::<ResultRow>::default());
    let visible = Arc::new(AtomicBool::new(false));
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
        window.on_query_changed(move |query| {
            if let Ok(mut current) = current_query.lock() {
                *current = query.to_string();
            }
            if let Some(window) = weak.upgrade() {
                window.set_last_action_id(String::new().into());
                window.set_last_action_result(String::new().into());
                window.set_last_action_subtitle(String::new().into());
            }
            let query_id = query_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let engine = Arc::clone(&engine);
            let icon_service = icon_service.clone();
            let icon_items = Arc::clone(&icon_items);
            let icon_requests = Arc::clone(&icon_requests);
            let weak = weak.clone();
            let query_generation = Arc::clone(&query_generation);
            std::thread::spawn(move || {
                let found = if query.trim().is_empty() {
                    Vec::new()
                } else {
                    engine.search(query.as_str())
                };
                let rows = found.iter().map(ResultRowPayload::from).collect::<Vec<_>>();
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
        let window = window.as_weak();
        let visible = Arc::clone(&visible);
        if let Some(handle) = window.upgrade() {
            handle.on_dismiss_requested(move || {
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
                    changed |= provider.apply_config(&updated.providers);
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
                let rows = found.iter().map(ResultRowPayload::from).collect::<Vec<_>>();
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
        platform.install_focus_listener(
            window.window(),
            Box::new(move || {
                if visible.swap(false, Ordering::AcqRel) {
                    if let Some(window) = weak.upgrade() {
                        let _ = window.hide();
                    }
                }
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

    let weak = window.as_weak();
    let platform_for_hotkey = Arc::clone(&platform);
    let visible_for_hotkey = Arc::clone(&visible);
    std::thread::spawn(move || {
        for event in hotkey_events {
            let weak = weak.clone();
            let platform = Arc::clone(&platform_for_hotkey);
            let visible = Arc::clone(&visible_for_hotkey);
            let _ = slint::invoke_from_event_loop(move || match event {
                platform::PlatformEvent::TogglePalette => {
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
        .with_winit_window_attributes_hook(|attributes| attributes.with_visible(false))
        .renderer_name(renderer.into())
        .select()?;
    Ok(())
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

struct ResultRowPayload {
    id: String,
    title: String,
    location: String,
    icon: Option<crate::platform::IconData>,
    category: String,
}

impl ResultRowPayload {
    fn from(item: &SearchResult) -> Self {
        Self {
            id: item.id.clone().into(),
            title: item.title.clone().into(),
            location: item.location.clone().into(),
            icon: None,
            category: match item.category {
                ResultCategory::Game => "game",
                ResultCategory::Application => "application",
                ResultCategory::Website => "website",
                ResultCategory::ControlPanel => "control-panel",
                ResultCategory::Settings => "settings",
                ResultCategory::Document => "document",
                ResultCategory::Other => "other",
                ResultCategory::Unclassified => "application",
            }
            .into(),
        }
    }
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
            icon,
            category: payload.category.into(),
        }
    }
}
