//! Windows Settings URI provider.

use crate::platform::source_ids;
use crate::result::{IconSpec, LaunchEntry, ResultCategory, SearchResult};

pub fn discover(_config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    crate::platform::discover_settings_items()
        .into_iter()
        .map(|item| {
            let target = item.launch_target;
            SearchResult {
                id: item.identity.clone(),
                identity: item.identity,
                title: item.title,
                category: ResultCategory::Settings,
                aliases: item.aliases,
                location: String::new(),
                launch_target: target.clone(),
                launch_arguments: String::new(),
                launch_working_directory: None,
                source: source_ids::SETTINGS.into(),
                sources: vec![source_ids::SETTINGS.into()],
                launch_entries: vec![LaunchEntry {
                    source: source_ids::SETTINGS.into(),
                    path: target.clone(),
                    target: target.clone(),
                    arguments: String::new(),
                    working_directory: None,
                }],
                actions: Vec::new(),
                // The URI itself has no file icon. Use the Settings executable's
                // icon rather than relying on a version-dependent AppsFolder ID.
                icon: Some(IconSpec::System(crate::platform::settings_icon_target())),
                ranking_bias: 0,
            }
        })
        .collect()
}
