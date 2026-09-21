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
    config::ShortcutConfig,
    page::{SearchEngineMode, SearchEngineSession},
    platform::SourceId,
    provider::{ProviderRegistry, SearchEngineProvider},
    result::{
        Action, IconSpec, LaunchEntry, ResultCategory, SearchResult,
    },
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
        (!text.is_empty()).then_some(Self {
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
    let title = if spec.preview {
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

    let actions = if spec.preview {
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
    }
}

pub(super) fn result_id(_index: usize, prefix: &str) -> String {
    // A binding survives configuration reordering. Prefix conflicts are
    // rejected during config normalization, so the normalized prefix is a
    // stable identity for a shortcut-owned custom result.
    let encoded = url::form_urlencoded::byte_serialize(prefix.trim_end().as_bytes())
        .collect::<String>();
    format!("shortcut:{encoded}")
}

pub(super) fn effective_type(shortcut: &ShortcutConfig) -> &str {
    match shortcut.prefix.trim_end() {
        prefix if prefix == COMMAND_PREFIX.trim_end() => "command",
        prefix if prefix == WEB_PREFIX.trim_end() => "url",
        _ => shortcut.shortcut_type.as_str(),
    }
}

/// Dynamic results generated from the current query rather than an index.
pub fn results(query: &str, configured: &[ShortcutConfig], language: &str) -> Vec<SearchResult> {
    let Some(query) = ShortcutQuery::new(query, language) else {
        return Vec::new();
    };

    let mut results = Vec::with_capacity(configured.len() + 2);
    if let Some(result) = command::builtin(query) {
        results.push(result);
    }
    if let Some(result) = web::builtin(query) {
        results.push(result);
    }

    // Keep configured shortcuts in their original order. The index is part of
    // the existing result identity, so preserve its offset for rank history.
    for (index, shortcut) in configured.iter().enumerate() {
        let index = index + 2;
        let result = match effective_type(shortcut) {
            "url" => web::configured(query, shortcut, index),
            "application" => application::configured(query, shortcut, index),
            "command" => command::configured(query, shortcut, index),
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
    let raw = query.trim_start();
    // Keep a bare `>` as a normal search query so the command and configured
    // shortcut results remain visible as previews. Built-in prefixes follow
    // the same literal matching rule as configured prefixes: `> ` is required.
    if raw.starts_with(COMMAND_PREFIX) {
        let argument = raw
            .strip_prefix(COMMAND_PREFIX)
            .unwrap_or_default()
            .trim_start();
        return Some(SearchEngineSession {
            id: result_id(0, COMMAND_PREFIX),
            title: crate::utils::i18n::tr(language, "Execute Command").into(),
            prefix: COMMAND_PREFIX.into(),
            argument: argument.into(),
            mode: SearchEngineMode::Execute,
        });
    }
    if raw.starts_with(WEB_PREFIX) {
        return Some(SearchEngineSession {
            id: result_id(1, WEB_PREFIX),
            title: crate::utils::i18n::tr(language, "Open website").into(),
            prefix: WEB_PREFIX.into(),
            argument: raw
                .strip_prefix(WEB_PREFIX)
                .unwrap_or_default()
                .trim_start()
                .into(),
            mode: SearchEngineMode::Execute,
        });
    }

    configured.iter().enumerate().find_map(|(index, shortcut)| {
        let argument = shortcut_argument(raw, &shortcut.prefix)?;
        Some(SearchEngineSession {
            id: result_id(index + 2, &shortcut.prefix),
            title: shortcut.name.clone(),
            prefix: shortcut.prefix.clone(),
            argument: argument.trim_start().into(),
            mode: SearchEngineMode::from_config(&shortcut.mode),
        })
    })
}

pub fn register(registry: &mut ProviderRegistry, shortcuts: Vec<ShortcutConfig>) {
    registry.register_search_engine(ShortcutProvider::new(shortcuts));
}

pub struct ShortcutProvider {
    configured: Vec<ShortcutConfig>,
    language: String,
}

impl ShortcutProvider {
    pub fn new(configured: Vec<ShortcutConfig>) -> Self {
        Self {
            configured,
            language: "system".into(),
        }
    }

    pub fn replace_config(&mut self, configured: Vec<ShortcutConfig>) {
        self.configured = configured;
    }

    pub fn set_language(&mut self, language: String) {
        self.language = language;
    }

    pub fn results(&self, query: &str) -> Vec<SearchResult> {
        results(query, &self.configured, &self.language)
    }

    pub fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        session(query, &self.configured, &self.language)
    }

    fn shortcut_hint(&self, item_id: &str) -> Option<String> {
        if item_id == result_id(0, COMMAND_PREFIX) {
            return Some(COMMAND_PREFIX.into());
        }
        if item_id == result_id(1, WEB_PREFIX) {
            return Some(WEB_PREFIX.into());
        }
        self.configured.iter().enumerate().find_map(|(index, shortcut)| {
            (item_id == result_id(index + 2, &shortcut.prefix))
                .then(|| shortcut.prefix.clone())
        })
    }
}

impl SearchEngineProvider for ShortcutProvider {
    fn query(&self, query: &str) -> Vec<SearchResult> {
        self.results(query)
    }

    fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        self.search_engine(query)
    }

    fn query_active(&self, session: &SearchEngineSession, query: &str) -> Vec<SearchResult> {
        self.results(query)
            .into_iter()
            .filter(|result| result.id == session.id)
            .collect()
    }

    fn owns_session(&self, session: &SearchEngineSession) -> bool {
        session.id.starts_with("shortcut:")
    }

    fn shortcut_hint(&self, item_id: &str) -> Option<String> {
        self.shortcut_hint(item_id)
    }

    fn replace_config(&mut self, shortcuts: Vec<ShortcutConfig>) {
        ShortcutProvider::replace_config(self, shortcuts);
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
            shortcut_type: "url".into(),
            mode: "search".into(),
            template: "https://example.com/?q={query}".into(),
            target: String::new(),
            arguments: String::new(),
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
            shortcut_type: "url".into(),
            mode: "panel".into(),
            template: "https://example.com/?q={query}".into(),
            target: String::new(),
            arguments: String::new(),
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
            shortcut_type: "url".into(),
            mode: "execute".into(),
            template: "https://example.com/?q={query}".into(),
            target: String::new(),
            arguments: String::new(),
        }];
        let provider = ShortcutProvider::new(configured);
        let session = provider.search_engine("d rust").expect("search engine");
        let active = SearchEngineProvider::query_active(&provider, &session, "d rust");

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, session.id);
        assert!(active[0].title.starts_with("Search docs"));
    }
}
