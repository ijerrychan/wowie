use super::{results::static_icon_kind, ContextActionRow};
use crate::{
    icon_service::IconService,
    result::{self, SearchResult},
};

pub(super) fn context_actions(
    item: &SearchResult,
    show_advanced_launch_entries: bool,
) -> Vec<ContextActionRow> {
    let is_filesystem_target = is_supported_filesystem_target(&item.launch_target);
    let is_store_shell_target = is_apps_folder_target(&item.launch_target);
    let mut actions = Vec::new();
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
    for action in &item.actions {
        if let result::Action::Plugin {
            action_id, icon, ..
        } = action
        {
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
                "open-directory" => crate::utils::i18n::tr(language, "Open containing folder"),
                "run-as-admin" => crate::utils::i18n::tr(language, "Run as administrator"),
                _ if action.kind == "section" => {
                    crate::utils::i18n::tr(language, "Other launch entries")
                }
                _ => action.title.as_str(),
            };
            let title_width = approximate_text_width(title, 13);
            let subtitle_width = approximate_text_width(action.subtitle.as_str(), 10);
            let content_width = title_width.max(subtitle_width);
            // The label starts at x=36px. Keep a safety margin for font
            // rasterization and the two-pixel menu border; otherwise a label
            // that is only a few pixels wider than the estimate is elided.
            if action.shortcut.is_empty() {
                content_width + 68
            } else {
                // Shortcut hints occupy the rightmost 120px and need a
                // six-pixel gap from the label.
                content_width + 180
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
        .filter_map(|action| {
            let result::Action::Plugin {
                action_id,
                icon: Some(icon),
                ..
            } = action
            else {
                return None;
            };
            static_icon_kind(Some(icon)).is_empty().then(|| {
                icon_service
                    .resolve_spec_at(icon, 16)
                    .map(|icon| (format!("plugin-action:{action_id}"), icon.as_ref().clone()))
            })?
        })
        .collect()
}

pub(super) fn visible_launch_entries(
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
            // Windows installer-generated shortcuts can use an .ico file as
            // an icon placeholder while launching the real application. It
            // is useful for association provenance, but it is not a valid
            // alternate launch target for the user-facing menu.
            if crate::platform::windows_rules::is_icon_path(&entry.target) {
                return None;
            }
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

pub(super) fn launch_entry_label(entry: &result::LaunchEntry) -> String {
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

fn is_supported_admin_target(target: &std::path::Path) -> bool {
    target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "exe" | "lnk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(title: &str, shortcut: &str) -> ContextActionRow {
        ContextActionRow {
            id: "test-action".into(),
            title: title.into(),
            subtitle: String::new().into(),
            kind: "common".into(),
            icon: slint::Image::default(),
            icon_kind: String::new().into(),
            shortcut: shortcut.into(),
        }
    }

    #[test]
    fn long_action_titles_get_width_with_a_rendering_safety_margin() {
        let title = "A very long action name that must remain visible";
        let width = context_menu_width(&[action(title, "")], "en");

        assert!(width >= approximate_text_width(title, 13) + 68);
    }

    #[test]
    fn shortcut_area_is_reserved_without_overlapping_long_titles() {
        let title = "A very long action name with a shortcut";
        let width = context_menu_width(&[action(title, "Ctrl+Shift+Enter")], "en");

        assert!(width >= approximate_text_width(title, 13) + 180);
    }

    #[test]
    fn localized_section_title_participates_in_width_measurement() {
        let mut section = action("", "");
        section.id = "section:other-launch-entries".into();
        section.kind = "section".into();

        let width = context_menu_width(&[section], "en");

        assert!(width >= approximate_text_width("Other launch entries", 13) + 68);
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
}
