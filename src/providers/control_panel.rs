//! Windows Control Panel settings provider.

use crate::platform::source_ids;
use crate::result::{IconSpec, LaunchEntry, ResultCategory, SearchResult};

pub fn discover(_config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    crate::platform::discover_control_panel_items()
        .into_iter()
        .map(|item| {
            let target = item.launch_target;
            SearchResult {
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
                launch_target: target.clone(),
                launch_arguments: String::new(),
                launch_working_directory: None,
                source: source_ids::CONTROL_PANEL.into(),
                sources: vec![source_ids::CONTROL_PANEL.into()],
                launch_entries: vec![LaunchEntry {
                    source: source_ids::CONTROL_PANEL.into(),
                    path: target.clone(),
                    target: target.clone(),
                    arguments: String::new(),
                    working_directory: None,
                }],
                actions: Vec::new(),
                icon: Some(IconSpec::System(target)),
                ranking_bias: 0,
            }
        })
        .collect()
}
