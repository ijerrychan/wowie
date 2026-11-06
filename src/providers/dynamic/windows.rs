use crate::{
    page::{SearchEngineMode, SearchEngineSession},
    platform::{self, SourceId},
    provider::{ProviderRegistry, SearchEngineProvider, SearchEngineSettings},
    result::{Action, IconSpec, ResultCategory, ResultVisibility, SearchResult},
};
use std::path::PathBuf;

pub struct WindowsProvider {
    language: String,
}
const PREFIX: &str = "< ";
pub fn register(
    registry: &mut ProviderRegistry,
    _: Vec<crate::config::ShortcutConfig>,
    _: crate::config::ShortcutKeywords,
) {
    registry.register_search_engine(WindowsProvider {
        language: "system".into(),
    });
}

impl SearchEngineProvider for WindowsProvider {
    fn settings(&self) -> SearchEngineSettings {
        SearchEngineSettings::default()
    }
    fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        let raw = query.trim_start();
        raw.starts_with(PREFIX).then(|| SearchEngineSession {
            id: "windows".into(),
            title: crate::utils::i18n::tr(&self.language, "Open windows").into(),
            placeholder: crate::utils::i18n::tr(&self.language, "Search open windows").into(),
            prefix: PREFIX.into(),
            argument: raw[PREFIX.len()..].into(),
            mode: SearchEngineMode::Search,
            state: Default::default(),
        })
    }
    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        let needle = query.trim();
        if needle.is_empty() {
            return vec![entry_result(&self.language)];
        }
        window_results()
    }

    fn query_active(&self, session: &SearchEngineSession, _query: &str) -> Vec<SearchResult> {
        if self.owns_session(session) {
            window_results()
        } else {
            Vec::new()
        }
    }

    fn owns_session(&self, session: &SearchEngineSession) -> bool {
        session.id == "windows" && session.prefix == PREFIX
    }

    fn search_engine_result(&self, session: &SearchEngineSession) -> Option<SearchResult> {
        self.owns_session(session)
            .then(|| entry_result(&self.language))
    }

    fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        (result_id == "windows:entry").then(|| PREFIX.to_owned())
    }

    fn empty_state(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
    ) -> Option<SearchResult> {
        let session = context.session()?;
        self.owns_session(session)
            .then(|| empty_state_result(&self.language, query.trim().is_empty()))
    }

    fn set_language(&mut self, language: String) {
        self.language = language;
    }
}

fn entry_result(language: &str) -> SearchResult {
    let source = SourceId::Custom("Windows".into());
    SearchResult {
        id: "windows:entry".into(),
        identity: "windows:entry".into(),
        title: crate::utils::i18n::tr(language, "Open windows").into(),
        category: ResultCategory::Other,
        aliases: vec![PREFIX.trim_end().into()],
        location: crate::utils::i18n::tr(language, "Search and switch between open windows").into(),
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: source.clone(),
        sources: vec![source],
        launch_entries: Vec::new(),
        actions: vec![Action::EnterSearchEngine {
            prefix: PREFIX.into(),
        }],
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: Some(IconSpec::Named("window".into())),
        ranking_bias: 0,
        allow_favorite: false,
        visibility: ResultVisibility::EmptyQueryOnly,
    }
}

fn empty_state_result(language: &str, empty_query: bool) -> SearchResult {
    let mut result = entry_result(language);
    result.id = "windows:empty".into();
    result.identity = result.id.clone();
    result.title = crate::utils::i18n::tr(
        language,
        if empty_query {
            "No open windows"
        } else {
            "No matching windows"
        },
    )
    .into();
    result.description =
        Some(crate::utils::i18n::tr(language, "Open windows will appear here").into());
    result.actions.clear();
    result.visibility = ResultVisibility::Always;
    result
}

fn window_results() -> Vec<SearchResult> {
    platform::visible_windows()
        .into_iter()
        .filter_map(make_result)
        .collect()
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
        category: ResultCategory::Window,
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
        icon: Some(
            window
                .process_path
                .map(IconSpec::System)
                .unwrap_or_else(|| IconSpec::Named("window".into())),
        ),
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
            process_path: None,
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
    #[test]
    fn window_with_executable_uses_the_real_system_icon_target() {
        let mut window = win("Editor", 7, 9);
        window.process_path = Some(PathBuf::from(r"C:\Apps\Editor\editor.exe"));
        let result = make_result(window).unwrap();
        assert_eq!(result.category, ResultCategory::Window);
        assert_eq!(
            WindowsProvider {
                language: "en".into()
            }
            .shortcut_hint("window:7:9"),
            None
        );
        assert_eq!(
            result.icon,
            Some(IconSpec::System(PathBuf::from(
                r"C:\Apps\Editor\editor.exe"
            )))
        );
    }
    #[test]
    fn empty_root_entry_enters_the_window_session_without_being_favoritable() {
        let result = entry_result("en");
        assert_eq!(result.visibility, ResultVisibility::EmptyQueryOnly);
        assert_eq!(result.category, ResultCategory::Other);
        assert!(!result.allow_favorite);
        assert_eq!(
            WindowsProvider {
                language: "en".into()
            }
            .shortcut_hint("windows:entry")
            .as_deref(),
            Some(PREFIX)
        );
        assert!(
            matches!(result.primary_action(), Some(Action::EnterSearchEngine { prefix }) if prefix == PREFIX)
        );
    }

    #[test]
    fn provider_owns_the_open_windows_session_and_supplies_empty_copy() {
        let provider = WindowsProvider {
            language: "en".into(),
        };
        let session = provider.search_engine("< ").unwrap();
        assert_eq!(session.mode, SearchEngineMode::Search);
        assert!(provider.owns_session(&session));
        assert!(provider.search_engine_result(&session).is_some());
        let empty = provider
            .empty_state(&crate::page::SearchContext::Engine(session), "")
            .unwrap();
        assert_eq!(empty.title, "No open windows");
        let session = provider.search_engine("< query").unwrap();
        let no_match = provider
            .empty_state(&crate::page::SearchContext::Engine(session), "query")
            .unwrap();
        assert_eq!(no_match.title, "No matching windows");
    }

    #[test]
    fn registry_routes_root_entry_and_open_windows_session() {
        let plugins = crate::plugin::PluginManager::discover_with_config("en", &Default::default());
        let core =
            crate::search::SearchCore::with_plugins_and_shortcuts(Vec::new(), plugins, Vec::new());
        let root = core.search("");
        assert!(root.iter().any(|item| item.id == "windows:entry"));
        let context = core.search_context("< ");
        let session = context.session().expect("window session");
        assert_eq!(session.id, "windows");
        let active = core.search_in_context(&context, "");
        assert!(active
            .iter()
            .all(|item| item.category == ResultCategory::Window));
    }
}
