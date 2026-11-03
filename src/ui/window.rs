use super::{ContextMenu, Launcher, PALETTE_POSITION_DELAY};
use crate::{config, platform, result::SearchResult};
use anyhow::Result;
use slint::{ComponentHandle, Timer};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
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
    let Ok(mut config) = config::Config::load_for_update() else {
        eprintln!("could not save window size: configuration is unreadable or invalid");
        return;
    };
    let native_window = window.window();
    let logical_size = native_window
        .size()
        .to_logical(native_window.scale_factor());
    config.window_width = logical_size.width.round().max(0.0) as u32;
    config.window_height = if logical_size.height >= config::MIN_WINDOW_HEIGHT as f32 {
        logical_size.height.round().max(0.0) as u32
    } else {
        window.get_result_window_height().round().max(0.0) as u32
    };
    if let Err(error) = config.save() {
        eprintln!("could not save window size: {error:#}");
    }
}

fn select_renderer(renderer: &str) -> Result<()> {
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attributes| {
            let attributes = attributes
                .with_visible(false)
                .with_resizable(true)
                .with_window_icon(launcher_window_icon());
            #[cfg(windows)]
            let attributes = {
                use slint::winit_030::winit::platform::windows::{
                    CornerPreference, WindowAttributesExtWindows,
                };
                // Apply to every frameless window, including settings and menus.
                // Winit manages the native DWM shadow across window changes.
                attributes
                    .with_transparent(false)
                    .with_undecorated_shadow(true)
                    .with_corner_preference(CornerPreference::Round)
            };
            attributes
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

pub(super) fn install_native_background<C: ComponentHandle + 'static>(
    weak: slint::Weak<C>,
    color: fn(&C) -> slint::Color,
) {
    #[cfg(windows)]
    {
        use slint::winit_030::WinitWindowAccessor;
        let _ = slint::spawn_local(async move {
            let Some(component) = weak.upgrade() else {
                return;
            };
            if component.window().winit_window().await.is_ok() {
                platform::set_window_background(component.window(), color(&component));
            }
        });
    }
    #[cfg(not(windows))]
    let _ = (weak, color);
}

pub(super) fn toggle_palette(
    window: &Launcher,
    platform: Arc<dyn platform::PlatformIntegration>,
    visible: &AtomicBool,
) {
    if visible.fetch_xor(true, Ordering::AcqRel) {
        let _ = window.hide();
    } else {
        platform::set_window_background(window.window(), window.get_native_background_color());
        platform.disable_window_transitions(window.window());
        if window.get_compact_search()
            && window.get_root_query_empty()
            && !window.get_shortcut_session_active()
        {
            window.set_empty_query_results_visible(false);
        }
        let native_window = window.window();
        let logical_size = native_window
            .size()
            .to_logical(native_window.scale_factor());
        let target_height = window.get_effective_window_height();
        if (logical_size.height - target_height).abs() > 0.5 {
            native_window.set_size(slint::LogicalSize::new(logical_size.width, target_height));
        }
        let _ = window.show();
        window.window().request_redraw();
        platform.activate_palette(window.window());
        let weak = window.as_weak();
        Timer::single_shot(PALETTE_POSITION_DELAY, move || {
            if let Some(window) = weak.upgrade() {
                // Center using the full result height even while the compact
                // window is shorter, so the search row keeps the same Y.
                platform.position_palette(window.window(), window.get_result_window_height());
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
        menu.set_keyboard_invoked(false);
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
