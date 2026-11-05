use crate::{
    platform::{self, SourceId},
    provider::{ProviderRegistry, SearchEngineProvider, SearchEngineSettings},
    result::{Action, ResultCategory, SearchResult},
};
use std::path::PathBuf;

pub struct WindowsProvider;
pub fn register(
    registry: &mut ProviderRegistry,
    _: Vec<crate::config::ShortcutConfig>,
    _: crate::config::ShortcutKeywords,
) {
    registry.register_search_engine(WindowsProvider);
}

impl SearchEngineProvider for WindowsProvider {
    fn settings(&self) -> SearchEngineSettings {
        SearchEngineSettings::default()
    }
    fn search_engine(&self, _: &str) -> Option<crate::page::SearchEngineSession> {
        None
    }
    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        let needle = query.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        platform::visible_windows()
            .into_iter()
            .filter_map(make_result)
            .collect()
    }
}

fn make_result(window: platform::VisibleWindow) -> Option<SearchResult> {
    if window.title.trim().is_empty() || window.hwnd == 0 || window.process_id == 0 {
        return None;
    }
    let source = SourceId::Custom("Windows".into());
    let id = format!("window:{}:{}", window.hwnd, window.process_id);
    Some(SearchResult {
        id: id.clone(),
        identity: id,
        title: window.title.clone(),
        category: ResultCategory::Other,
        aliases: vec![window.process_name.clone()],
        location: window.process_name,
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: source.clone(),
        sources: vec![source],
        launch_entries: Vec::new(),
        actions: vec![Action::SwitchWindow {
            hwnd: window.hwnd,
            process_id: window.process_id,
        }],
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: None,
        ranking_bias: 0,
        allow_favorite: false,
        visibility: crate::result::ResultVisibility::Always,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn win(title: &str, hwnd: u64, pid: u32) -> platform::VisibleWindow {
        platform::VisibleWindow {
            hwnd,
            process_id: pid,
            title: title.into(),
            process_name: "editor.exe".into(),
        }
    }
    #[test]
    fn filters_invalid_targets_and_keeps_valid_typed_action() {
        assert!(make_result(win("  ", 1, 2)).is_none());
        assert!(make_result(win("Title", 0, 2)).is_none());
        let item = make_result(win("Title", 10, 2)).unwrap();
        assert!(!item.allow_favorite);
        assert_eq!(item.launch_target, PathBuf::new());
        assert!(matches!(
            item.primary_action(),
            Some(Action::SwitchWindow {
                hwnd: 10,
                process_id: 2
            })
        ));
    }
    #[test]
    fn action_round_trips_json() {
        let action = Action::SwitchWindow {
            hwnd: 1234,
            process_id: 42,
        };
        assert_eq!(
            serde_json::from_str::<Action>(&serde_json::to_string(&action).unwrap()).unwrap(),
            action
        );
    }
}
