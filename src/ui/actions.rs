use super::{results::static_icon_kind, ContextActionRow};
pub(super) use crate::providers::app::{launch_entry_label, visible_launch_entries};
use crate::{
    icon_service::IconService,
    result::{self, SearchResult},
};

pub(super) fn context_actions(
    item: &SearchResult,
    show_advanced_launch_entries: bool,
    is_favorite: bool,
    can_add_favorite: bool,
) -> Vec<ContextActionRow> {
    let is_filesystem_target = is_supported_filesystem_target(&item.launch_target);
    let is_store_shell_target = is_apps_folder_target(&item.launch_target);
    let mut actions = Vec::new();
    if is_favorite || can_add_favorite {
        actions.push(ContextActionRow {
            id: if is_favorite {
                "remove-favorite"
            } else {
                "add-favorite"
            }
            .into(),
            title: if is_favorite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            }
            .into(),
            subtitle: "".into(),
            kind: "common".into(),
            icon: slint::Image::default(),
            icon_kind: "star".into(),
            shortcut: String::new().into(),
        });
    }
    if is_filesystem_target {
        actions.push(ContextActionRow {
            id: "open-directory".into(),
            title: "Open containing folder".into(),
            subtitle: "".into(),
            kind: "common".into(),
            icon: slint::Image::default(),
            icon_kind: "folder-open".into(),
            shortcut: "Ctrl+O".into(),
        });
    }
    if is_store_shell_target
        || (is_filesystem_target && is_supported_admin_target(&item.launch_target))
    {
        actions.push(ContextActionRow {
            id: "run-as-admin".into(),
            title: "Run as administrator".into(),
            subtitle: "".into(),
            kind: "common".into(),
            icon: slint::Image::default(),
            icon_kind: "shield-account".into(),
            shortcut: "Ctrl+Shift+Enter".into(),
        });
    }
    actions.push(ContextActionRow {
        id: "copy-title".into(),
        title: "Copy title".into(),
        subtitle: "".into(),
        kind: "common".into(),
        icon: slint::Image::default(),
        icon_kind: "content-copy".into(),
        shortcut: String::new().into(),
    });
    if is_http_url(&item.launch_target) {
        actions.push(ContextActionRow {
            id: "copy-url".into(),
            title: "Copy URL".into(),
            subtitle: "".into(),
            kind: "common".into(),
            icon: slint::Image::default(),
            icon_kind: "content-copy".into(),
            shortcut: String::new().into(),
        });
    } else if is_copyable_path(&item.launch_target) {
        actions.push(ContextActionRow {
            id: "copy-path".into(),
            title: "Copy path".into(),
            subtitle: "".into(),
            kind: "common".into(),
            icon: slint::Image::default(),
            icon_kind: "content-copy".into(),
            shortcut: String::new().into(),
        });
    }
    for action in &item.actions {
        match action {
            result::Action::Plugin {
                action_id, icon, ..
            } => {
                let icon_kind = static_icon_kind(icon.as_ref()).to_owned();
                actions.push(ContextActionRow {
                    id: format!("plugin-action:{action_id}").into(),
                    title: action_id.clone().into(),
                    subtitle: "".into(),
                    kind: "plugin".into(),
                    icon: slint::Image::default(),
                    icon_kind: icon_kind.into(),
                    shortcut: String::new().into(),
                });
            }
            result::Action::ContextMenu(action) => {
                let icon_kind = static_icon_kind(action.icon.as_ref()).to_owned();
                actions.push(ContextActionRow {
                    id: format!("custom-context:{}", action.id).into(),
                    title: action.title.clone().into(),
                    subtitle: action.subtitle.clone().into(),
                    kind: "custom".into(),
                    icon: slint::Image::default(),
                    icon_kind: icon_kind.into(),
                    shortcut: String::new().into(),
                });
            }
            _ => {}
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
            icon_kind: "".into(),
            shortcut: String::new().into(),
        });
        for (index, entry) in other_launches {
            actions.push(ContextActionRow {
                id: format!("launch:{index}").into(),
                title: launch_entry_label(entry).into(),
                subtitle: "".into(),
                kind: "launch".into(),
                icon: slint::Image::default(),
                icon_kind: "".into(),
                shortcut: String::new().into(),
            });
        }
    }
    actions
}

/// Size the native-looking menu from the widest visible label. Slint does not
/// expose the preferred width of items inside a repeated element, so keep the
/// measurement here next to the labels that populate the menu.
pub(super) fn context_menu_width(actions: &[ContextActionRow], language: &str) -> i32 {
    let widest = actions
        .iter()
        .map(|action| {
            let title = match action.id.as_str() {
                "add-favorite" => crate::utils::i18n::tr(language, "Add to favorites"),
                "remove-favorite" => crate::utils::i18n::tr(language, "Remove from favorites"),
                "open-directory" => crate::utils::i18n::tr(language, "Open containing folder"),
                "run-as-admin" => crate::utils::i18n::tr(language, "Run as administrator"),
                "copy-title" => crate::utils::i18n::tr(language, "Copy title"),
                "copy-path" => crate::utils::i18n::tr(language, "Copy path"),
                "copy-url" => crate::utils::i18n::tr(language, "Copy URL"),
                _ if action.kind == "section" => {
                    crate::utils::i18n::tr(language, "Other launch entries")
                }
                _ => action.title.as_str(),
            };
            let title_width = approximate_text_width(title, 13);
            let subtitle_width = approximate_text_width(action.subtitle.as_str(), 10);
            let content_width = title_width.max(subtitle_width);
            // The label starts at x=36px. Keep enough room for font metric
            // differences and the menu border; long localized labels such as
            // "Remove from favorites" can otherwise be elided by a few pixels.
            if action.shortcut.is_empty() {
                content_width + 84
            } else {
                // Shortcut hints occupy the rightmost 120px; keep a wider
                // gap there for text metric differences as well.
                content_width + 196
            }
        })
        .max()
        .unwrap_or_default();
    widest.max(180)
}

fn approximate_text_width(text: &str, font_size: i32) -> i32 {
    text.chars()
        .map(|character| match character {
            ' ' => (font_size * 3 + 5) / 10,
            'i' | 'j' | 'l' | 'I' | 't' | 'f' | 'r' => (font_size * 4 + 5) / 10,
            'm' | 'w' | 'M' | 'W' => (font_size * 8 + 5) / 10,
            character if character.is_ascii_uppercase() => (font_size * 6 + 5) / 10,
            character if character.is_ascii() => (font_size * 5 + 5) / 10,
            _ => font_size,
        })
        .sum()
}

pub(super) fn resolve_plugin_action_icons(
    item: &SearchResult,
    icon_service: &IconService,
) -> Vec<(String, crate::platform::IconData)> {
    item.actions
        .iter()
        .filter_map(|action| match action {
            result::Action::Plugin {
                action_id,
                icon: Some(icon),
                ..
            } if static_icon_kind(Some(icon)).is_empty() => icon_service
                .resolve_spec_at(icon, 16)
                .map(|icon| (format!("plugin-action:{action_id}"), icon.as_ref().clone())),
            result::Action::ContextMenu(action)
                if action
                    .icon
                    .as_ref()
                    .is_some_and(|icon| static_icon_kind(Some(icon)).is_empty()) =>
            {
                let icon = action.icon.as_ref()?;
                icon_service.resolve_spec_at(icon, 16).map(|icon| {
                    (
                        format!("custom-context:{}", action.id),
                        icon.as_ref().clone(),
                    )
                })
            }
            _ => None,
        })
        .collect()
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

fn is_copyable_path(target: &std::path::Path) -> bool {
    target.is_absolute()
        && !crate::platform::windows_rules::is_shell_namespace(target)
        && !target.to_string_lossy().contains("://")
}

fn is_http_url(target: &std::path::Path) -> bool {
    let target = target.to_string_lossy();
    (target.starts_with("https://") || target.starts_with("http://"))
        && !target.chars().any(char::is_whitespace)
}

fn is_supported_admin_target(target: &std::path::Path) -> bool {
    target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "exe" | "lnk"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        platform::SourceId,
        result::{ContextMenuAction, ContextMenuBehavior},
    };
    use std::path::PathBuf;

    fn result(target: PathBuf) -> SearchResult {
        SearchResult {
            id: "test-result".into(),
            identity: "test-result".into(),
            title: "Test result".into(),
            category: result::ResultCategory::Other,
            metadata: Default::default(),
            aliases: Vec::new(),
            location: target.to_string_lossy().into_owned(),
            launch_target: target,
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: SourceId::Shortcut,
            sources: vec![SourceId::Shortcut],
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
        }
    }

    #[test]
    fn common_context_menu_can_copy_result_name_and_filesystem_path() {
        let item = result(std::env::current_exe().unwrap());
        let actions = context_actions(&item, false, false, true);
        let ids = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();

        assert!(ids.contains(&"copy-title"));
        assert!(ids.contains(&"copy-path"));
        assert!(ids.contains(&"open-directory"));
    }

    #[test]
    fn favorite_context_action_reflects_current_state() {
        let item = result(PathBuf::from("https://example.com"));
        let actions = context_actions(&item, false, false, true);
        assert_eq!(actions[0].id, "add-favorite");
        let actions = context_actions(&item, false, true, false);
        assert_eq!(actions[0].id, "remove-favorite");
    }

    #[test]
    fn generated_execute_results_can_hide_add_favorite() {
        let item = result(PathBuf::from("https://example.com"));
        let actions = context_actions(&item, false, false, false);
        assert!(actions.iter().all(|action| action.id != "add-favorite"));
    }

    #[test]
    fn common_context_menu_copies_web_targets_as_urls() {
        let item = result(PathBuf::from("https://example.com/search?q=test"));
        let actions = context_actions(&item, false, false, true);
        let ids = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();

        assert!(ids.contains(&"copy-title"));
        assert!(ids.contains(&"copy-url"));
        assert!(!ids.contains(&"copy-path"));
    }

    #[test]
    fn provider_context_actions_appear_separately_from_primary_actions() {
        let mut item = result(PathBuf::new());
        item.actions
            .push(result::Action::ContextMenu(ContextMenuAction {
                id: "copy-location".into(),
                title: "Copy location".into(),
                subtitle: "Copy the resolved address".into(),
                icon: None,
                behavior: ContextMenuBehavior::CopyText {
                    text: "https://example.com".into(),
                },
            }));

        let actions = context_actions(&item, false, false, true);
        let custom = actions
            .iter()
            .find(|action| action.id == "custom-context:copy-location")
            .unwrap();
        assert_eq!(custom.title, "Copy location");
        assert_eq!(custom.subtitle, "Copy the resolved address");
        assert!(item.primary_action().is_none());
    }

    #[test]
    fn administrator_action_only_supports_launchable_file_types() {
        assert!(is_supported_admin_target(std::path::Path::new("tool.exe")));
        assert!(is_supported_admin_target(std::path::Path::new("tool.LNK")));
        assert!(!is_supported_admin_target(std::path::Path::new(
            "notes.txt"
        )));
        assert!(!is_supported_admin_target(std::path::Path::new("folder")));
    }

    #[test]
    fn window_like_empty_targets_do_not_offer_path_admin_or_favorite_actions() {
        let item = result(PathBuf::new());
        let actions = context_actions(&item, false, false, false);
        let ids = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"copy-title"));
        assert!(!ids.contains(&"copy-path"));
        assert!(!ids.contains(&"open-directory"));
        assert!(!ids.contains(&"run-as-admin"));
        assert!(!ids.contains(&"add-favorite"));
    }
}
