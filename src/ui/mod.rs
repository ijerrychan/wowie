mod actions;
mod details;
mod results;
mod settings;
mod window;

slint::include_modules!();

mod settings_ui {
    include!(concat!(env!("OUT_DIR"), "/settings.rs"));
}

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
use results::{matches_model, request_icons, static_icon_kind, ResultRowPayload};
use settings_ui::{SettingsCategory, SettingsEntry, SettingsShortcut, SettingsWindow};
use slint::{ComponentHandle, Model, ModelRc, Timer, VecModel};
use std::{
    cell::Cell,
    collections::{BTreeSet, HashSet},
    rc::Rc,
    sync::{
        atomic::AtomicU64,
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use window::{create_window, hide_context_menu, save_window_size, toggle_palette};

const SEARCH_ICON_FRAME_DELAY: Duration = Duration::from_millis(16);
const CONTEXT_MENU_LAYOUT_DELAY: Duration = Duration::from_millis(16);
const LAUNCH_ERROR_DURATION: Duration = Duration::from_secs(5);
const FOCUS_LOSS_DELAY: Duration = Duration::from_millis(30);
const PROVIDER_REFRESH_INTERVAL: Duration = Duration::from_millis(200);
pub(super) const PALETTE_POSITION_DELAY: Duration = Duration::from_millis(10);
const SEARCH_PAGE_PREFETCH_DISTANCE: usize = 12;
/// Keep the Slint model bounded even when several providers return broad
/// matches. SearchCore shares exactly the snapshot accepted by this surface.
const MAX_RENDERED_RESULTS: usize = 256;

fn apply_theme_to_windows(
    dark_mode: bool,
    platform: &dyn platform::PlatformIntegration,
    launcher: Option<&Launcher>,
    settings: Option<&SettingsWindow>,
    context: Option<&ContextMenu>,
) {
    let accent = platform.system_accent_color();
    let accent_color = || {
        slint::Color::from_rgb_u8(
            ((accent >> 16) & 0xff) as u8,
            ((accent >> 8) & 0xff) as u8,
            (accent & 0xff) as u8,
        )
    };

    if let Some(launcher) = launcher {
        launcher.set_dark_mode(dark_mode);
        launcher.set_accent_color(accent_color());
        launcher.set_accent_background(slint::Color::from_argb_u8(
            if dark_mode { 0x42 } else { 0x28 },
            ((accent >> 16) & 0xff) as u8,
            ((accent >> 8) & 0xff) as u8,
            (accent & 0xff) as u8,
        ));
        platform::set_window_background(launcher.window(), launcher.get_native_background_color());
        launcher.window().request_redraw();
    }
    if let Some(settings) = settings {
        settings.set_dark_mode(dark_mode);
        settings.set_accent_color(accent_color());
        platform::set_window_background(settings.window(), settings.get_native_background_color());
        settings.window().request_redraw();
    }
    if let Some(context) = context {
        context.set_dark_mode(dark_mode);
        context.set_accent_color(accent_color());
        platform::set_window_background(context.window(), context.get_native_background_color());
        context.window().request_redraw();
    }
}

mod search_coordinator;
mod settings_callbacks;
mod settings_reload;
use search_coordinator::*;

fn open_settings_window(
    settings: &SettingsWindow,
    plugin_settings: &[plugin::PluginSettingsDefinition],
    platform: Arc<dyn platform::PlatformIntegration>,
) {
    let (config, _) = config::Config::load();
    settings::refresh(settings, &config, plugin_settings, &config.language);
    settings.set_status_message(String::new().into());
    platform::set_window_background(settings.window(), settings.get_native_background_color());
    let _ = settings.show();
    settings.window().request_redraw();
    platform.activate_palette(settings.window());
    let weak = settings.as_weak();
    Timer::single_shot(PALETTE_POSITION_DELAY, move || {
        if let Some(settings) = weak.upgrade() {
            let native_window = settings.window();
            let logical_height = native_window
                .size()
                .to_logical(native_window.scale_factor())
                .height;
            platform.position_palette(native_window, logical_height);
            platform.activate_palette(native_window);
        }
    });
}

mod lifecycle;
pub use lifecycle::run;

#[cfg(test)]
mod layout_tests;

#[cfg(test)]
mod tests {
    use super::actions::{launch_entry_label, visible_launch_entries};
    use super::*;
    use crate::platform::SourceId;
    use std::path::PathBuf;

    #[test]
    fn pagination_deduplicates_and_respects_the_surface_limit() {
        let make = |id: usize| SearchResult {
            id: id.to_string(),
            identity: id.to_string(),
            title: id.to_string(),
            category: result::ResultCategory::Other,
            aliases: Vec::new(),
            location: String::new(),
            launch_target: PathBuf::new(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: SourceId::Shortcut,
            sources: Vec::new(),
            launch_entries: Vec::new(),
            actions: Vec::new(),
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: None,
            ranking_bias: 0,
            allow_favorite: true,
            visibility: result::ResultVisibility::Always,
        };
        let existing = (0..MAX_RENDERED_RESULTS - 2).map(make).collect::<Vec<_>>();
        let accepted = accepted_page_items(
            &existing,
            vec![make(0), make(254), make(254), make(255), make(256)],
        );
        assert_eq!(
            accepted
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["254", "255"]
        );
        let full = (0..MAX_RENDERED_RESULTS).map(make).collect::<Vec<_>>();
        assert!(accepted_page_items(&full, vec![make(256)]).is_empty());
        assert!(accepted_page_items(&existing, vec![make(0)]).is_empty());
    }

    #[test]
    fn execute_sessions_only_allow_favorites_when_the_query_is_empty() {
        let session = crate::page::SearchEngineSession {
            id: "shortcut:command".into(),
            title: "Execute Command".into(),
            placeholder: "Enter a command".into(),
            prefix: "> ".into(),
            argument: String::new(),
            mode: crate::page::SearchEngineMode::Execute,
            state: Default::default(),
        };
        assert!(!allow_favorite_in_context(Some(&session), true, "ipconfig"));
        assert!(allow_favorite_in_context(Some(&session), true, "   "));

        let mut search_session = session;
        search_session.mode = crate::page::SearchEngineMode::Search;
        assert!(allow_favorite_in_context(
            Some(&search_session),
            true,
            "weather"
        ));
        assert!(allow_favorite_in_context(None, false, "weather"));
    }

    #[test]
    fn explicit_detail_survives_the_selection_change_that_opens_it() {
        assert!(explicit_detail_matches(Some("weather"), Some("weather")));
        assert!(!explicit_detail_matches(
            Some("weather"),
            Some("calculator")
        ));
        assert!(!explicit_detail_matches(Some("weather"), None));
        assert!(!explicit_detail_matches(None, Some("weather")));
    }

    #[test]
    fn shortcut_action_is_not_shown_as_an_alternate_launch_entry() {
        let item = SearchResult {
            id: "shortcut:command".into(),
            identity: "shortcut:command".into(),
            title: "Execute Command \"echo test\"".into(),
            category: result::ResultCategory::Other,
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
                icon: None,
            })],
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: None,
            ranking_bias: 0,
            allow_favorite: true,
            visibility: result::ResultVisibility::Always,
        };

        assert!(context_actions(&item, false, false, true)
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
            icon: None,
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
            icon: None,
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
            allow_favorite: true,
            visibility: result::ResultVisibility::Always,
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
            icon: None,
        };

        assert_eq!(
            launch_entry_label(&entry),
            "Chrome.exe --profile-directory=Work"
        );
    }
}
