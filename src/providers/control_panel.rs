//! Windows Control Panel settings provider.

use crate::platform::source_ids;
use crate::result::{IconSpec, LaunchEntry, ResultCategory, SearchResult};

pub fn discover(_config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    let mut results = crate::platform::discover_control_panel_items()
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
        .collect::<Vec<_>>();

    #[cfg(windows)]
    {
        let existing_settings_targets = results
            .iter()
            .filter(|item| item.category == crate::result::ResultCategory::Settings)
            .map(|item| item.launch_target.to_string_lossy().to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        let mut known_names = results
            .iter()
            .flat_map(|item| {
                std::iter::once(item.title.as_str()).chain(
                    item.aliases
                        .iter()
                        .filter(|alias| {
                            alias
                                .get(..10)
                                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Microsoft."))
                        })
                        .map(String::as_str),
                )
            })
            .map(normalize_setting_name)
            .collect::<std::collections::HashSet<_>>();
        let mut powertoys_titles = std::collections::HashSet::new();
        for item in crate::providers::windows_settings::powertoys_results() {
            let canonical_names = crate::providers::windows_settings::powertoys_name_aliases(&item)
                .into_iter()
                .map(|name| normalize_setting_name(&name))
                .collect::<Vec<_>>();
            if existing_settings_targets
                .contains(&item.launch_target.to_string_lossy().to_ascii_lowercase())
                || canonical_names
                    .iter()
                    .any(|name| known_names.contains(name))
                || !powertoys_titles.insert(normalize_setting_name(&item.title))
            {
                continue;
            }
            known_names.extend(canonical_names);
            known_names.insert(normalize_setting_name(&item.title));
            results.push(item);
        }
    }

    results
}

fn normalize_setting_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Normalize known destinations for newly discovered rows and cached rows
/// restored from an older build. Identity stays tied to the Shell item.
fn apply_compatible_destination(item: &mut SearchResult) {
    if item.category != ResultCategory::ControlPanel {
        return;
    }

    let is_compatible = item.aliases.iter().any(|alias| {
        crate::providers::windows_settings::compatible_settings_uri_for_canonical_name(alias)
            .is_some()
    });
    if !is_compatible {
        return;
    }
    // The legacy Control Panel entry now launches the supported Settings URI.
    crate::providers::windows_settings::apply_compatible_destination(item);
}

pub(crate) fn normalize_cached_destinations(items: &mut [SearchResult]) {
    for item in items {
        apply_compatible_destination(item);
    }
}
