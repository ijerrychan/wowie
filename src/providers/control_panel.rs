//! Windows Control Panel settings provider.

use crate::platform::source_ids;
use crate::result::{IconSpec, LaunchEntry, ResultCategory, SearchResult};

// Control Panel's parsing names can become stale when Windows moves a page to
// the Settings app. Use the language-independent Shell canonical name instead
// of matching a localized title.
const MODERN_SETTINGS_DESTINATIONS: &[(&str, &str)] = &[("Microsoft.Fonts", "ms-settings:fonts")];

pub fn discover(_config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    crate::platform::discover_control_panel_items()
        .into_iter()
        .map(|item| {
            let shell_target = item.launch_target;
            let mut result = SearchResult {
                id: item.identity.clone(),
                identity: item.identity,
                title: item.title,
                category: ResultCategory::ControlPanel,
                // The platform provider supplies localized titles and the
                // language-independent canonical name as a searchable alias.
                aliases: item.aliases,
                // The Shell parsing name is an implementation detail (often a
                // `::{GUID}` value), useful for launching but not for search.
                location: String::new(),
                launch_target: shell_target.clone(),
                launch_arguments: String::new(),
                launch_working_directory: None,
                source: source_ids::CONTROL_PANEL.into(),
                sources: vec![source_ids::CONTROL_PANEL.into()],
                launch_entries: vec![LaunchEntry {
                    source: source_ids::CONTROL_PANEL.into(),
                    path: shell_target.clone(),
                    target: shell_target.clone(),
                    arguments: String::new(),
                    working_directory: None,
                    icon: None,
                }],
                actions: Vec::new(),
                detail: None,
                description: None,
                app_user_model_id: None,
                package_family_name: None,
                package_metadata: None,
                // Keep the Shell item as the icon source even when its launch
                // destination has moved to the Settings app.
                icon: Some(IconSpec::System(shell_target)),
                ranking_bias: 0,
                allow_favorite: true,
                visibility: crate::result::ResultVisibility::Always,
            };
            apply_compatible_destination(&mut result);
            result
        })
        .collect()
}

/// Normalize known destinations for newly discovered rows and cached rows
/// restored from an older build. Identity stays tied to the Shell item.
fn apply_compatible_destination(item: &mut SearchResult) {
    if item.category != ResultCategory::ControlPanel {
        return;
    }

    let Some(target) = item.aliases.iter().find_map(|alias| {
        MODERN_SETTINGS_DESTINATIONS
            .iter()
            .find(|(canonical_name, _)| alias.eq_ignore_ascii_case(canonical_name))
            .map(|(_, uri)| std::path::PathBuf::from(*uri))
    }) else {
        return;
    };

    item.launch_target = target.clone();
    item.launch_arguments.clear();
    item.launch_working_directory = None;
    for entry in item
        .launch_entries
        .iter_mut()
        .filter(|entry| entry.source.as_str() == source_ids::CONTROL_PANEL)
    {
        entry.path = target.clone();
        entry.target = target.clone();
        entry.arguments.clear();
        entry.working_directory = None;
    }
}

pub(crate) fn normalize_cached_destinations(items: &mut [SearchResult]) {
    for item in items {
        apply_compatible_destination(item);
    }
}
