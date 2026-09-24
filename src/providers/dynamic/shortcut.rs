//! Search-driven shortcut provider.
//!
//! This module owns provider lifecycle and dispatch only. Each shortcut
//! behavior lives in its own sibling module so adding a new built-in shortcut
//! does not make this provider a second command parser.

#[path = "shortcut/application.rs"]
mod application;
#[path = "shortcut/command.rs"]
mod command;
#[path = "shortcut/web.rs"]
mod web;

use crate::{
    config::{ShortcutConfig, ShortcutKeywords},
    page::{SearchEngineMode, SearchEngineSession},
    platform::SourceId,
    provider::{ProviderRegistry, SearchEngineProvider, DEFAULT_SEARCH_DEBOUNCE},
    result::{Action, IconSpec, LaunchEntry, ResultCategory, SearchResult},
};
use std::path::PathBuf;

pub(super) const COMMAND_PREFIX: &str = "> ";
pub(super) const WEB_PREFIX: &str = "w ";

/// The common query state shared by every shortcut implementation.
#[derive(Clone, Copy)]
pub(super) struct ShortcutQuery<'a> {
    pub(super) raw: &'a str,
    pub(super) text: &'a str,
    pub(super) language: &'a str,
}

impl<'a> ShortcutQuery<'a> {
    fn new(raw: &'a str, language: &'a str) -> Option<Self> {
        let text = raw.trim();
        Some(Self {
            raw,
            text,
            language,
        })
    }

    pub(super) fn resolve_argument(self, prefix: &str) -> ShortcutArgument<'a> {
        match shortcut_argument(self.raw, prefix) {
            Some(value) => ShortcutArgument {
                value: value.trim(),
                explicit: true,
            },
            None => ShortcutArgument {
                value: self.text,
                explicit: false,
            },
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ShortcutArgument<'a> {
    pub(super) value: &'a str,
    pub(super) explicit: bool,
}

/// The normalized data needed to materialize a launch-oriented shortcut.
///
/// Shortcut-specific modules calculate targets and display details; this
/// builder keeps SearchResult and Action construction in one place.
pub(super) struct ShortcutSpec {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) prefix: String,
    pub(super) argument: String,
    pub(super) display_argument: String,
    pub(super) location: String,
    pub(super) target: PathBuf,
    pub(super) launch_arguments: String,
    pub(super) preview: bool,
    pub(super) include_query_alias: bool,
    pub(super) icon: Option<IconSpec>,
}

pub(super) fn build_result(query: ShortcutQuery<'_>, spec: ShortcutSpec) -> SearchResult {
    let icon = spec
        .icon
        .or_else(|| Some(IconSpec::System(spec.target.clone())));
    let preview = spec.preview || query.text.is_empty();
    let title = if preview {
        spec.name.clone()
    } else {
        format!("{} \"{}\"", spec.name, spec.display_argument)
    };
    let alias = if spec.prefix.ends_with(char::is_whitespace) {
        format!("{}{}", spec.prefix, spec.argument)
    } else {
        format!("{} {}", spec.prefix, spec.argument)
    };
    let mut aliases = vec![alias];
    if spec.include_query_alias {
        aliases.push(query.text.to_owned());
    }

    let actions = if query.text.is_empty() {
        // Empty-query rows are engine entry points, not executable previews.
        // This keeps command, URL, and configured shortcuts consistent with
        // other root-search engines such as Everything.
        vec![Action::EnterSearchEngine {
            prefix: spec.prefix.clone(),
        }]
    } else if preview {
        Vec::new()
    } else {
        vec![Action::Launch(LaunchEntry {
            source: SourceId::Shortcut,
            path: PathBuf::new(),
            target: spec.target.clone(),
            arguments: spec.launch_arguments.clone(),
            working_directory: None,
        })]
    };

    SearchResult {
        id: spec.id.clone(),
        identity: spec.id,
        title,
        // The shortcut selected this interaction, but the generated row is a
        // normal custom result. Result categories describe the result itself,
        // never the syntax used to reach it.
        category: ResultCategory::Other,
        aliases,
        location: spec.location,
        launch_target: spec.target,
        launch_arguments: spec.launch_arguments,
        launch_working_directory: None,
        source: SourceId::Shortcut,
        sources: vec![SourceId::Shortcut],
        launch_entries: Vec::new(),
        actions,
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon,
        // A result reached through its shortcut is an explicit request. The
        // same custom result remains visible from default search as a fallback.
        ranking_bias: if spec.include_query_alias { 0 } else { -1000 },
        allow_favorite: true,
        visibility: if query.text.is_empty() {
            crate::result::ResultVisibility::EmptyQueryOnly
        } else {
            crate::result::ResultVisibility::Always
        },
    }
}

pub(super) fn result_id(_index: usize, prefix: &str) -> String {
    // A binding survives configuration reordering. Prefix conflicts are
    // rejected during config normalization, so the normalized prefix is a
    // stable identity for a shortcut-owned custom result.
    let encoded =
        url::form_urlencoded::byte_serialize(prefix.trim_end().as_bytes()).collect::<String>();
    format!("shortcut:{encoded}")
}

pub(super) fn effective_type<'a>(
    shortcut: &'a ShortcutConfig,
    keywords: &ShortcutKeywords,
) -> &'a str {
    match shortcut.prefix.trim_end() {
        prefix if prefix.eq_ignore_ascii_case(&keywords.command) => "command",
        prefix if prefix.eq_ignore_ascii_case(&keywords.web_search) => "url",
        _ => shortcut.shortcut_type.as_str(),
    }
}

pub(super) fn configured_description(query: ShortcutQuery<'_>, shortcut: &ShortcutConfig) -> String {
    if shortcut.description.is_empty() {
        crate::utils::i18n::tr(query.language, "Custom shortcut").into()
    } else {
        expand_raw_template(&shortcut.description, query.resolve_argument(&shortcut.prefix).value)
    }
}

/// Dynamic results generated from the current query rather than an index.
pub fn results(query: &str, configured: &[ShortcutConfig], language: &str) -> Vec<SearchResult> {
    results_with_keywords(query, configured, language, &ShortcutKeywords::default())
}

pub fn results_with_keywords(
    query: &str,
    configured: &[ShortcutConfig],
    language: &str,
    keywords: &ShortcutKeywords,
) -> Vec<SearchResult> {
    let Some(query) = ShortcutQuery::new(query, language) else {
        return Vec::new();
    };
    let command_prefix = keywords.command_prefix();
    let web_search_prefix = keywords.web_search_prefix();

    let mut results = Vec::with_capacity(configured.len() + 2);
    if let Some(result) = command::builtin_with_prefix(query, &command_prefix) {
        results.push(result);
    }
    if let Some(result) = web::builtin_with_prefix(query, &web_search_prefix) {
        results.push(result);
    }

    // Keep configured shortcuts in their original order. The index is part of
    // the existing result identity, so preserve its offset for rank history.
    for (index, shortcut) in configured.iter().enumerate() {
        let index = index + 2;
        let result = match effective_type(shortcut, keywords) {
            "url" => web::configured_with_builtin(query, shortcut, index, &web_search_prefix),
            "application" => application::configured(query, shortcut, index),
            "command" => {
                command::configured_with_builtin(query, shortcut, index, &command_prefix)
            }
            _ => None,
        };
        if let Some(result) = result {
            results.push(result);
        }
    }

    results
}

/// Resolve the owning shortcut page without constructing a result list.
///
/// This is deliberately separate from `results`: a provider may expose a
/// session even when its current query has no result yet (for example, an
/// empty provider search or a form-like panel).
pub fn session(
    query: &str,
    configured: &[ShortcutConfig],
    language: &str,
) -> Option<SearchEngineSession> {
    session_with_keywords(query, configured, language, &ShortcutKeywords::default())
}

pub fn session_with_keywords(
    query: &str,
    configured: &[ShortcutConfig],
    language: &str,
    keywords: &ShortcutKeywords,
) -> Option<SearchEngineSession> {
    let raw = query.trim_start();
    let command_prefix = keywords.command_prefix();
    let web_search_prefix = keywords.web_search_prefix();
    // Keep a bare `>` as a normal search query so the command and configured
    // shortcut results remain visible as previews. Built-in prefixes follow
    // the same literal matching rule as configured prefixes: `> ` is required.
    if raw.starts_with(&command_prefix) {
        let argument = raw
            .strip_prefix(&command_prefix)
            .unwrap_or_default()
            .trim_start();
        return Some(SearchEngineSession {
            id: result_id(0, &command_prefix),
            title: crate::utils::i18n::tr(language, "Execute Command").into(),
            placeholder: crate::utils::i18n::tr(language, "Enter a command").into(),
            prefix: command_prefix.clone(),
            argument: argument.into(),
            mode: SearchEngineMode::Execute,
            state: Default::default(),
        });
    }
    if raw.starts_with(&web_search_prefix) {
        return Some(SearchEngineSession {
            id: result_id(1, &web_search_prefix),
            title: crate::utils::i18n::tr(language, "Open website").into(),
            placeholder: crate::utils::i18n::tr(language, "Enter a URL or search term").into(),
            prefix: web_search_prefix.clone(),
            argument: raw
                .strip_prefix(&web_search_prefix)
                .unwrap_or_default()
                .trim_start()
                .into(),
            mode: SearchEngineMode::Execute,
            state: Default::default(),
        });
    }

    configured.iter().enumerate().find_map(|(index, shortcut)| {
        let argument = shortcut_argument(raw, &shortcut.prefix)?;
        Some(SearchEngineSession {
            id: result_id(index + 2, &shortcut.prefix),
            title: shortcut.name.clone(),
            placeholder: if shortcut.placeholder.is_empty() {
                shortcut.name.clone()
            } else {
                shortcut.placeholder.clone()
            },
            prefix: shortcut.prefix.clone(),
            argument: argument.trim_start().into(),
            mode: SearchEngineMode::from_config(&shortcut.mode),
            state: Default::default(),
        })
    })
}

pub fn register(
    registry: &mut ProviderRegistry,
    shortcuts: Vec<ShortcutConfig>,
    keywords: ShortcutKeywords,
) {
    registry.register_search_engine(ShortcutProvider::new_with_keywords(shortcuts, keywords));
}

pub struct ShortcutProvider {
    configured: Vec<ShortcutConfig>,
    keywords: ShortcutKeywords,
    language: String,
}

impl ShortcutProvider {
    pub fn new(configured: Vec<ShortcutConfig>) -> Self {
        Self::new_with_keywords(configured, ShortcutKeywords::default())
    }

    pub fn new_with_keywords(
        configured: Vec<ShortcutConfig>,
        keywords: ShortcutKeywords,
    ) -> Self {
        Self {
            configured,
            keywords,
            language: "system".into(),
        }
    }

    pub fn replace_config(&mut self, configured: Vec<ShortcutConfig>, keywords: ShortcutKeywords) {
        self.configured = configured;
        self.keywords = keywords;
    }

    pub fn set_language(&mut self, language: String) {
        self.language = language;
    }

    pub fn results(&self, query: &str) -> Vec<SearchResult> {
        results_with_keywords(query, &self.configured, &self.language, &self.keywords)
    }

    pub fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        session_with_keywords(query, &self.configured, &self.language, &self.keywords)
    }

    fn shortcut_hint(&self, item_id: &str) -> Option<String> {
        let command_prefix = self.keywords.command_prefix();
        let web_search_prefix = self.keywords.web_search_prefix();
        if item_id == result_id(0, &command_prefix) {
            return Some(command_prefix);
        }
        if item_id == result_id(1, &web_search_prefix) {
            return Some(web_search_prefix);
        }
        self.configured
            .iter()
            .enumerate()
            .find_map(|(index, shortcut)| {
                (item_id == result_id(index + 2, &shortcut.prefix)).then(|| shortcut.prefix.clone())
            })
    }

    fn search_engine_result(&self, session: &SearchEngineSession) -> Option<SearchResult> {
        self.results(&session.prefix)
            .into_iter()
            .find(|result| result.id == session.id)
    }
}

impl SearchEngineProvider for ShortcutProvider {
    fn settings(&self) -> crate::provider::SearchEngineSettings {
        crate::provider::SearchEngineSettings {
            history_limit: 10,
            ..crate::provider::SearchEngineSettings::default()
        }
    }

    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        self.results(query)
    }

    fn search_debounce(&self, session: &SearchEngineSession) -> std::time::Duration {
        if session.mode == SearchEngineMode::Execute {
            std::time::Duration::ZERO
        } else {
            DEFAULT_SEARCH_DEBOUNCE
        }
    }

    fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        self.search_engine(query)
    }

    fn empty_state(
        &self,
        context: &crate::page::SearchContext,
        _query: &str,
    ) -> Option<SearchResult> {
        let session = context.session()?;
        if !self.owns_session(session) {
            return None;
        }
        let entry = self.search_engine_result(session)?;
        Some(SearchResult {
            id: format!("{}:empty-state", session.id),
            identity: format!("{}:empty-state", session.id),
            title: session.title.clone(),
            category: ResultCategory::Other,
            aliases: Vec::new(),
            location: entry.location.clone(),
            launch_target: PathBuf::new(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: SourceId::Shortcut,
            sources: vec![SourceId::Shortcut],
            launch_entries: Vec::new(),
            actions: Vec::new(),
            detail: None,
            description: Some(entry.location),
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: entry.icon,
            ranking_bias: 0,
            allow_favorite: true,
            visibility: crate::result::ResultVisibility::Always,
        })
    }

    fn query_active(&self, session: &SearchEngineSession, query: &str) -> Vec<SearchResult> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        // Reconstruct the explicit prefix so matching applies to the selected
        // shortcut implementation.
        let explicit_query = if query.starts_with(session.prefix.as_str()) {
            query.to_owned()
        } else {
            format!("{}{}", session.prefix, query)
        };
        self.results(&explicit_query)
            .into_iter()
            .filter(|result| result.id == session.id)
            .map(|mut result| {
                // A query-derived command/URL is an executable result, not
                // the shortcut row that enters this session. Give it a
                // stable query-specific identity so favorites preserve the
                // actual action instead of colliding with the root entry.
                let history_id =
                    crate::ranking::engine_history_result_id(&session.id, query.trim());
                let suffix = history_id.rsplit(':').next().unwrap_or_default();
                let id = format!("{}:active:{suffix}", session.id);
                result.id = id.clone();
                result.identity = id;
                result
            })
            .collect()
    }

    fn query_history(
        &self,
        session: &SearchEngineSession,
        history: &[crate::provider::SearchHistoryEntry],
    ) -> Vec<SearchResult> {
        history
            .iter()
            .filter_map(|entry| {
                let explicit_query = format!("{}{}", session.prefix, entry.query);
                let mut item = self
                    .results(&explicit_query)
                    .into_iter()
                    .find(|result| result.id == session.id)?;
                item.id = entry.result_id.clone();
                item.identity = entry.result_id.clone();
                item.visibility = crate::result::ResultVisibility::Always;
                Some(item)
            })
            .collect()
    }

    fn owns_session(&self, session: &SearchEngineSession) -> bool {
        session.id.starts_with("shortcut:")
    }

    fn shortcut_hint(&self, item_id: &str) -> Option<String> {
        self.shortcut_hint(item_id)
    }

    fn search_engine_result(&self, session: &SearchEngineSession) -> Option<SearchResult> {
        self.search_engine_result(session)
    }

    fn replace_config(&mut self, shortcuts: Vec<ShortcutConfig>, keywords: ShortcutKeywords) {
        ShortcutProvider::replace_config(self, shortcuts, keywords);
    }

    fn set_language(&mut self, language: String) {
        ShortcutProvider::set_language(self, language);
    }
}

pub(super) fn shortcut_argument<'a>(query: &'a str, prefix: &str) -> Option<&'a str> {
    query.strip_prefix(prefix).map(str::trim_start)
}

pub(super) fn expand_url_template(template: &str, query: &str) -> String {
    template.replace(
        "{query}",
        &url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>(),
    )
}

pub(super) fn expand_raw_template(template: &str, query: &str) -> String {
    template.replace("{query}", query)
}

pub(super) fn join_command_and_arguments(command: &str, arguments: &str) -> String {
    if arguments.trim().is_empty() {
        command.trim().to_string()
    } else {
        format!("{} {}", command.trim(), arguments.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_and_configured_prefixes_create_owned_sessions() {
        let configured = vec![ShortcutConfig {
            name: "Search docs".into(),
            prefix: "d ".into(),
            placeholder: String::new(),
            shortcut_type: "url".into(),
            mode: "search".into(),
            template: "https://example.com/?q={query}".into(),
            target: String::new(),
            arguments: String::new(),
            description: String::new(),
        }];

        let command = session("> cargo test", &configured, "en").expect("command session");
        assert_eq!(command.mode, SearchEngineMode::Execute);
        assert_eq!(command.argument, "cargo test");
        assert!(session(">", &configured, "en").is_none());
        assert!(session(">cargo test", &configured, "en").is_none());

        let search = session("d rust", &configured, "en").expect("search session");
        assert_eq!(search.mode, SearchEngineMode::Search);
        assert_eq!(search.prefix, "d ");
        assert_eq!(search.argument, "rust");
    }

    #[test]
    fn a_panel_session_is_available_before_results_exist() {
        let configured = vec![ShortcutConfig {
            name: "Open panel".into(),
            prefix: "p ".into(),
            placeholder: String::new(),
            shortcut_type: "url".into(),
            mode: "panel".into(),
            template: "https://example.com/?q={query}".into(),
            target: String::new(),
            arguments: String::new(),
            description: String::new(),
        }];

        let panel = session("p ", &configured, "en").expect("panel session");
        assert_eq!(panel.mode, SearchEngineMode::Panel);
        assert!(panel.argument.is_empty());
    }

    #[test]
    fn entered_shortcut_engine_hides_other_custom_results() {
        let configured = vec![ShortcutConfig {
            name: "Search docs".into(),
            prefix: "d ".into(),
            placeholder: String::new(),
            shortcut_type: "url".into(),
            mode: "execute".into(),
            template: "https://example.com/?q={query}".into(),
            target: String::new(),
            arguments: String::new(),
            description: String::new(),
        }];
        let provider = ShortcutProvider::new(configured);
        let session = provider.search_engine("d rust").expect("search engine");
        let active = SearchEngineProvider::query_active(&provider, &session, "d rust");

        assert_eq!(active.len(), 1);
        assert_ne!(active[0].id, session.id);
        assert!(active[0].id.starts_with(&format!("{}:active:", session.id)));
        assert!(active[0].title.starts_with("Search docs"));
    }

    #[test]
    fn empty_command_and_web_sessions_materialize_recent_history_rows() {
        let provider = ShortcutProvider::new(Vec::new());
        let command = provider.search_engine("> ").expect("command session");
        let command_history = ["ipconfig", "whoami"]
            .into_iter()
            .map(|query| crate::provider::SearchHistoryEntry {
                query: query.into(),
                result_id: crate::ranking::engine_history_result_id(&command.id, query),
            })
            .collect::<Vec<_>>();
        let command_rows = provider.query_history(&command, &command_history);
        assert_eq!(command_rows.len(), 2);
        assert_ne!(command_rows[0].id, command_rows[1].id);
        assert!(command_rows[0].title.contains("ipconfig"));
        assert!(command_rows[0].launch_arguments.contains("ipconfig"));

        let web = provider.search_engine("w ").expect("web session");
        let web_history = ["https://example.com", "https://example.org"]
            .into_iter()
            .map(|query| crate::provider::SearchHistoryEntry {
                query: query.into(),
                result_id: crate::ranking::engine_history_result_id(&web.id, query),
            })
            .collect::<Vec<_>>();
        let web_rows = provider.query_history(&web, &web_history);
        assert_eq!(web_rows.len(), 2);
        assert_eq!(
            web_rows[0].launch_target,
            std::path::PathBuf::from("https://example.com/")
        );
        assert_eq!(web_rows[1].launch_target, std::path::PathBuf::from("https://example.org/"));
    }

    #[test]
    fn empty_root_shortcuts_use_the_session_empty_state() {
        let configured = vec![ShortcutConfig {
            name: "Open notes".into(),
            prefix: "notes ".into(),
            placeholder: String::new(),
            shortcut_type: "application".into(),
            mode: "execute".into(),
            template: String::new(),
            target: "notepad.exe".into(),
            arguments: "{query}".into(),
            description: String::new(),
        }];
        let mut provider = ShortcutProvider::new(configured);
        provider.set_language("en".into());

        for result in provider.results("") {
            assert!(matches!(
                result.actions.first(),
                Some(Action::EnterSearchEngine { .. })
            ));
        }

        let session = session("notes ", &provider.configured, "en").expect("notes session");
        let active = provider.query_active(&session, "");
        assert!(active.is_empty());
        let empty_state = SearchEngineProvider::empty_state(
            &provider,
            &crate::page::SearchContext::Engine(session),
            "",
        )
        .expect("shortcut page");
        assert_eq!(empty_state.title, "Open notes");
        assert!(empty_state
            .description
            .unwrap_or_default()
            .contains("Custom shortcut"));
    }
}
