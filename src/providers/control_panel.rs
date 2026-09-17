//! Windows Control Panel settings provider.

use crate::result::{IconSpec, LaunchEntry, ResultCategory, SearchResult};

pub fn discover() -> Vec<SearchResult> {
    crate::platform::discover_control_panel_items()
        .into_iter()
        .map(|item| {
            let target = item.launch_target;
            let mut aliases = item.aliases;
            aliases.push("control panel".into());
            aliases.push("控制面板".into());
            SearchResult {
                id: item.identity.clone(),
                identity: item.identity,
                title: item.title,
                category: ResultCategory::ControlPanel,
                aliases,
                // The Shell parsing name is an implementation detail (often a
                // `::{GUID}` value), useful for launching but not for display.
                location: String::new(),
                launch_target: target.clone(),
                launch_arguments: String::new(),
                launch_working_directory: None,
                source: "ControlPanel".into(),
                sources: vec!["ControlPanel".into()],
                launch_entries: vec![LaunchEntry {
                    source: "ControlPanel".into(),
                    path: target.clone(),
                    target: target.clone(),
                    arguments: String::new(),
                    working_directory: None,
                }],
                actions: Vec::new(),
                icon: Some(IconSpec::System(target)),
            }
        })
        .collect()
}
