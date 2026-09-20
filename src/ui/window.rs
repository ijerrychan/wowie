use super::{ContextMenu, Launcher};
use crate::{config, platform, result::SearchResult};
use anyhow::Result;
use slint::{ComponentHandle, Timer};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

pub(super) fn create_window(config: &config::Config) -> Result<Launcher> {
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

pub(super) fn save_window_size(window: &Launcher) {
    let (mut config, _) = config::Config::load();
    let native_window = window.window();
    let logical_size = native_window
        .size()
        .to_logical(native_window.scale_factor());
    config.window_width = logical_size.width.round().max(0.0) as u32;
    config.window_height = if window.get_result_mode() {
        logical_size.height.round().max(0.0) as u32
    } else {
        window.get_result_window_height().round().max(0.0) as u32
    };
    if let Err(error) = config.save() {
        eprintln!("could not save window size: {error:#}");
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
                .with_resizable(true)
                .with_window_icon(launcher_window_icon())
        })
        .renderer_name(renderer.into())
        .select()?;
    Ok(())
}

fn launcher_window_icon() -> Option<slint::winit_030::winit::window::Icon> {
    let image = image::load_from_memory(include_bytes!("../../wowie.png"))
        .ok()?
        .into_rgba8();
    let (width, height) = (image.width(), image.height());
    slint::winit_030::winit::window::Icon::from_rgba(image.into_raw(), width, height).ok()
}

pub(super) fn toggle_palette(
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
                platform.position_palette(
                    window.window(),
                    window.get_result_mode(),
                    window.get_result_window_height(),
                );
                platform.activate_palette(window.window());
                window.window().request_redraw();
            }
        });
    }
}

pub(super) fn hide_context_menu(
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
