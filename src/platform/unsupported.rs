use super::PlatformIntegration;
use anyhow::Result;
use std::sync::mpsc::Receiver;

pub struct UnsupportedPlatform;

impl PlatformIntegration for UnsupportedPlatform {
    fn system_prefers_dark_theme(&self) -> bool {
        false
    }

    fn system_accent_color(&self) -> u32 {
        0x0067c0
    }

    fn register_hotkey(
        &self,
        hotkey: &str,
        _watch_system_theme: bool,
    ) -> Result<Receiver<super::PlatformEvent>> {
        eprintln!("global hotkey '{hotkey}' is only available on Windows");
        let (_sender, receiver) = std::sync::mpsc::channel();
        Ok(receiver)
    }

    fn start_tray(&self, _exit_label: String) -> Receiver<super::PlatformEvent> {
        let (_sender, receiver) = std::sync::mpsc::channel();
        receiver
    }

    fn position_palette(&self, _window: &slint::Window) {}
    fn position_context_menu(&self, _window: &slint::Window) {}
    fn activate_context_menu(&self, _window: &slint::Window) {}
    fn is_window_focused(&self, _window: &slint::Window) -> bool {
        false
    }
    fn activate_palette(&self, _window: &slint::Window) {}
    fn install_focus_listener(&self, _window: &slint::Window, _on_lost_focus: Box<dyn Fn()>) {}
    fn disable_window_transitions(&self, _window: &slint::Window) {}
}
