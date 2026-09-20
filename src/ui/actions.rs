use super::{
    results::{icon_to_image, static_icon_kind},
    ContextActionRow,
};
use crate::{
    icon_service::IconService,
    result::{self, SearchResult},
};

pub(super) fn context_actions(
    item: &SearchResult,
    show_advanced_launch_entries: bool,
    icon_service: &IconService,
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
        });
    }
    if is_filesystem_target || is_store_shell_target {
        actions.push(ContextActionRow {
            id: "run-as-admin".into(),
            title: "Run as administrator".into(),
            subtitle: "".into(),
            kind: "common".into(),
            icon: slint::Image::default(),
            icon_kind: "shield-account".into(),
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
                icon: icon
                    .as_ref()
                    .filter(|_| icon_kind.is_empty())
                    .and_then(|icon| icon_service.resolve_spec_at(icon, 16))
                    .map(|icon| icon_to_image(icon.as_ref()))
                    .unwrap_or_default(),
                icon_kind: icon_kind.into(),
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
        });
        for (index, entry) in other_launches {
            actions.push(ContextActionRow {
                id: format!("launch:{index}").into(),
                title: launch_entry_label(entry).into(),
                subtitle: "".into(),
                kind: "launch".into(),
                icon: slint::Image::default(),
                icon_kind: "".into(),
            });
        }
    }
    actions
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
