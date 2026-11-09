//! Windows Control Panel settings provider.

use crate::platform::source_ids;
use crate::result::{IconSpec, LaunchEntry, ResultCategory, SearchMetadata, SearchResult};

pub fn discover(_config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    #[cfg(windows)]
    let availability = crate::platform::SettingsAvailability::discover();
    let results = crate::platform::discover_control_panel_items()
        .into_iter()
        .map(|item| {
            let shell_target = item.launch_target;
            let icon = Some(IconSpec::System(item.icon_target.into()));
            let mut result = SearchResult {
                id: item.identity.clone(),
                identity: item.identity,
                title: item.title,
                category: ResultCategory::ControlPanel,
                // Canonical Shell identifiers support internal catalog mapping
                // but are technical evidence, not user-facing names.
                aliases: Vec::new(),
                metadata: SearchMetadata {
                    keywords: item.keywords,
                    context: item.area.clone().into_iter().collect(),
                    technical: item.aliases,
                },
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
                    icon: icon.clone(),
                }],
                actions: Vec::new(),
                detail: None,
                description: None,
                app_user_model_id: None,
                package_family_name: None,
                package_metadata: None,
                // Use the captured Shell identity rather than re-parsing its
                // display/launch name to obtain the native icon.
                icon,
                ranking_bias: 0,
                allow_favorite: true,
                visibility: crate::result::ResultVisibility::Always,
            };
            crate::providers::windows_settings::enrich_shell_result(&mut result);
            crate::providers::windows_settings::apply_compatible_destination(
                &mut result,
                #[cfg(windows)]
                &availability,
            );
            result
        })
        .collect::<Vec<_>>();

    #[cfg(windows)]
    let results = results
        .into_iter()
        .chain(crate::providers::windows_settings::powertoys_results(
            &availability,
        ));
    crate::providers::windows_settings::merge_results(results)
}
