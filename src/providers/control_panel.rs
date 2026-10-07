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
                // Tasks expose their localized applet area; Shell identities
                // remain launch details and never become searchable locations.
                location: item.area.unwrap_or_default(),
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
            crate::providers::windows_settings::enrich_shell_result(&mut result);
            crate::providers::windows_settings::apply_compatible_destination(&mut result);
            result
        })
        .collect::<Vec<_>>();

    #[cfg(windows)]
    {
        let mut existing_settings_targets = results
            .iter()
            .filter(|item| item.category == crate::result::ResultCategory::Settings)
            .map(|item| item.launch_target.to_string_lossy().to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        for item in crate::providers::windows_settings::powertoys_results() {
            if item.category == ResultCategory::Settings
                && !existing_settings_targets
                    .insert(item.launch_target.to_string_lossy().to_ascii_lowercase())
            {
                continue;
            }
            results.push(item);
        }
    }

    results
}
