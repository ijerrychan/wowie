//! Runtime provider registry.
//!
//! SearchCore owns the search algorithm, while this module owns the list of
//! search engines. The default (root) search is an aggregator over engines;
//! entering a session is optional and is never what makes a provider an
//! engine.

use crate::{
    config::{PluginConfig, ShortcutConfig, ShortcutKeywords},
    page::{SearchContext, SearchEngineSession},
    plugin::PluginManager,
    result::SearchResult,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// Default delay for engines that do not opt into an immediate search.
pub const DEFAULT_SEARCH_DEBOUNCE: Duration = Duration::from_millis(30);
const MAX_SEARCH_DEBOUNCE_MS: u64 = 2_000;

/// Behaviour shared by every search engine, including plugin engines.
///
/// `participates_in_root` controls discovery from the root search. It does
/// not affect whether an engine can be entered through a result action or
/// another explicit session entry. `respect_global_fuzzy_search` lets an
/// engine opt out of fuzzy matching while retaining the single matcher and
/// user preference for engines that opt in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SearchEngineSettings {
    pub participates_in_root: bool,
    pub respect_global_fuzzy_search: bool,
    /// A bounded, engine-wide tie breaker. It is intentionally applied only
    /// after lexical relevance, user history and category preference.
    pub ranking_bias: i32,
    /// Number of recent successful session queries retained for this engine.
    /// Zero disables query history. Values are capped by the host.
    pub history_limit: usize,
    /// Delay interactive session queries until input has been idle for this
    /// many milliseconds. This is useful for remote APIs that charge per call.
    pub search_debounce_ms: u64,
}

impl Default for SearchEngineSettings {
    fn default() -> Self {
        Self {
            participates_in_root: true,
            respect_global_fuzzy_search: true,
            ranking_bias: 0,
            history_limit: 0,
            search_debounce_ms: DEFAULT_SEARCH_DEBOUNCE.as_millis() as u64,
        }
    }
}

/// A recent query persisted after a successful action in a search-engine
/// session. `result_id` is stable for that engine/query pair and lets an
/// engine materialize independently actionable history rows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SearchHistoryEntry {
    pub query: String,
    pub result_id: String,
}

#[derive(Debug, Clone)]
pub struct EngineSearchResult {
    pub item: SearchResult,
    pub settings: SearchEngineSettings,
}

/// A search engine may own a query after a shortcut has selected it.
pub trait SearchEngineProvider: Send + Sync {
    /// Stable engine-level behaviour. Engines without a session still use
    /// these settings when the root search invokes them.
    fn settings(&self) -> SearchEngineSettings {
        SearchEngineSettings::default()
    }

    fn search_engine(&self, query: &str) -> Option<SearchEngineSession>;
    /// Results contributed when the root search invokes this engine. These
    /// are ordinary results; none is required to be a session entry.
    fn query_root(&self, query: &str) -> Vec<SearchResult>;

    /// Optional content for an empty result surface. This is presentation
    /// state, not a selectable result row, so engines can describe a session
    /// without manufacturing an intro item.
    fn empty_state(&self, _context: &SearchContext, _query: &str) -> Option<SearchResult> {
        None
    }

    /// Engines that host several independently configured engines (notably
    /// the plugin host) may preserve settings per returned result.
    fn query_root_with_settings(&self, query: &str) -> Vec<EngineSearchResult> {
        let settings = self.settings();
        self.query_root(query)
            .into_iter()
            .map(|item| EngineSearchResult { item, settings })
            .collect()
    }

    /// Delay before dispatching a query to this engine. Providers with local,
    /// low-latency backends may opt out; remote or expensive engines can use
    /// a larger value without teaching the UI about provider identities.
    fn search_debounce(&self, session: &SearchEngineSession) -> Duration {
        Duration::from_millis(
            self.settings_for_session(session)
                .search_debounce_ms
                .min(MAX_SEARCH_DEBOUNCE_MS),
        )
    }

    /// Results owned after this engine has been entered. Most search engines
    /// return their normal query results; an action-style engine can narrow
    /// the surface to the result selected by its shortcut.
    fn query_active(&self, _session: &SearchEngineSession, query: &str) -> Vec<SearchResult> {
        self.query_root(query)
    }

    /// Optional results shown for an empty session query. Providers that
    /// support persistent history can turn the stored queries into ordinary,
    /// actionable SearchResults. Plugin engines receive the entries in the
    /// query callback's `search_history` field instead.
    fn query_history(
        &self,
        _session: &SearchEngineSession,
        _history: &[SearchHistoryEntry],
    ) -> Vec<SearchResult> {
        Vec::new()
    }

    fn query_active_with_history(
        &self,
        session: &SearchEngineSession,
        query: &str,
        history: &[SearchHistoryEntry],
    ) -> Vec<SearchResult> {
        let mut results = self.query_active(session, query);
        if query.trim().is_empty() && !history.is_empty() {
            results.extend(self.query_history(session, history));
        }
        results
    }

    /// Engine-specific settings may vary by selected session (for example,
    /// each plugin can declare a different history limit).
    fn settings_for_session(&self, _session: &SearchEngineSession) -> SearchEngineSettings {
        self.settings()
    }

    /// Load a subsequent page for an entered engine. `offset` is the number
    /// of rows already delivered for this session. Engines without paged
    /// sources keep the default empty response.
    fn query_active_page(
        &self,
        _session: &SearchEngineSession,
        _query: &str,
        _offset: usize,
    ) -> Vec<SearchResult> {
        Vec::new()
    }

    /// Confirms that a previously resolved session belongs to this engine.
    fn owns_session(&self, _session: &SearchEngineSession) -> bool {
        false
    }

    /// Return the shortcut bound to a result owned by this engine. The binding
    /// belongs to the shortcut registry, not to `SearchResult` or its aliases.
    fn shortcut_hint(&self, _result_id: &str) -> Option<String> {
        None
    }

    /// Return the default-engine result that entered this session. The UI
    /// uses this result's icon for the search field while the session owns it.
    fn search_engine_result(&self, _session: &SearchEngineSession) -> Option<SearchResult> {
        None
    }

    fn replace_config(
        &mut self,
        _shortcuts: Vec<ShortcutConfig>,
        _shortcut_keywords: ShortcutKeywords,
    ) {
    }

    fn set_language(&mut self, _language: String) {}
}

pub(crate) fn find_plugin_action<'a>(
    item: &'a SearchResult,
    requested_action_id: &str,
) -> Result<(&'a str, &'a str)> {
    if let Some(context_action_id) = requested_action_id.strip_prefix("context-menu:") {
        if let Some((plugin_id, action_id)) = item.actions.iter().find_map(|action| {
            let crate::result::Action::ContextMenu(action) = action else {
                return None;
            };
            if action.id != context_action_id {
                return None;
            }
            match &action.behavior {
                crate::result::ContextMenuBehavior::Plugin {
                    plugin_id,
                    action_id,
                } => Some((plugin_id.as_str(), action_id.as_str())),
                _ => None,
            }
        }) {
            return Ok((plugin_id, action_id));
        }
    }

    if let Some(action) = item.actions.iter().find_map(|action| {
        if action.domain() != crate::result::ActionDomain::Plugin {
            return None;
        }
        action
            .plugin_parts()
            .filter(|(_, action_id)| *action_id == requested_action_id)
    }) {
        return Ok(action);
    }

    let plugin_id = item
        .id
        .strip_prefix("plugin:")
        .and_then(|value| value.split_once(':').map(|(plugin_id, _)| plugin_id))
        .context("result has no plugin action")?;
    item.detail
        .as_ref()
        .and_then(|detail| {
            detail
                .actions
                .iter()
                .find(|action| action.id == requested_action_id)
                .map(|action| (plugin_id, action.id.as_str()))
        })
        .context("result has no plugin action")
}

pub struct ProviderRegistry {
    plugins: PluginManager,
    engines: Vec<Mutex<Box<dyn SearchEngineProvider>>>,
    indexed: Arc<Mutex<Arc<Vec<SearchResult>>>>,
}

impl ProviderRegistry {
    #[cfg(test)]
    pub fn without_dynamic(plugins: PluginManager) -> Self {
        let indexed = Arc::new(Mutex::new(Arc::new(Vec::new())));
        let mut registry = Self {
            plugins,
            engines: Vec::new(),
            indexed,
        };
        registry.register_indexed_engines();
        registry
    }

    #[cfg(test)]
    pub fn default_providers(plugins: PluginManager, shortcuts: Vec<ShortcutConfig>) -> Self {
        Self::default_providers_with_keywords(plugins, shortcuts, ShortcutKeywords::default())
    }

    pub fn default_providers_with_keywords(
        plugins: PluginManager,
        shortcuts: Vec<ShortcutConfig>,
        shortcut_keywords: ShortcutKeywords,
    ) -> Self {
        let plugin_engine = plugins.clone();
        let mut registry = Self {
            plugins,
            engines: Vec::new(),
            indexed: Arc::new(Mutex::new(Arc::new(Vec::new()))),
        };
        registry.register_indexed_engines();
        registry.register_search_engine(plugin_engine);
        crate::providers::dynamic::register_all(&mut registry, shortcuts, shortcut_keywords);
        registry
    }

    pub fn register_search_engine<P>(&mut self, engine: P)
    where
        P: SearchEngineProvider + 'static,
    {
        self.engines.push(Mutex::new(Box::new(engine)));
    }

    fn register_indexed_engines(&mut self) {
        // These engines own independent search domains but share the app
        // index's immutable snapshot. Their lack of a session entry is
        // intentional: an engine need not manufacture a custom result merely
        // to exist.
        self.register_search_engine(IndexedSearchEngine::apps(Arc::clone(&self.indexed)));
        self.register_search_engine(IndexedSearchEngine::control_panel(Arc::clone(
            &self.indexed,
        )));
    }

    pub fn replace_indexed_results(&self, results: Arc<Vec<SearchResult>>) {
        if let Ok(mut indexed) = self.indexed.lock() {
            *indexed = results;
        }
    }

    pub fn indexed_len(&self) -> usize {
        self.indexed.lock().map(|items| items.len()).unwrap_or(0)
    }

    pub fn indexed_by_id(&self, id: &str) -> Option<SearchResult> {
        self.indexed
            .lock()
            .ok()?
            .iter()
            .find(|item| item.id == id)
            .cloned()
    }

    pub fn query(
        &self,
        context: &SearchContext,
        query: &str,
        history: &[SearchHistoryEntry],
    ) -> Vec<EngineSearchResult> {
        if let SearchContext::Engine(session) = context {
            for engine in &self.engines {
                if let Ok(engine) = engine.lock() {
                    if engine.owns_session(session) {
                        return engine
                            .query_active_with_history(session, query, history)
                            .into_iter()
                            .map(|item| EngineSearchResult {
                                item,
                                settings: engine.settings(),
                            })
                            .collect();
                    }
                }
            }
            return Vec::new();
        }

        let mut results = Vec::new();
        // Root search merely aggregates opted-in engines. A result that
        // enters a session is therefore optional, not a special result type.
        for engine in &self.engines {
            if let Ok(engine) = engine.lock() {
                let settings = engine.settings();
                if settings.participates_in_root {
                    results.extend(engine.query_root_with_settings(query));
                }
            }
        }
        results
    }

    pub fn history_limit(&self, session: &SearchEngineSession) -> usize {
        self.engines
            .iter()
            .find_map(|engine| {
                engine.lock().ok().and_then(|engine| {
                    engine
                        .owns_session(session)
                        .then(|| engine.settings_for_session(session).history_limit)
                })
            })
            .unwrap_or_default()
            .min(crate::ranking::MAX_ENGINE_SEARCH_HISTORY)
    }

    pub fn empty_state(&self, context: &SearchContext, query: &str) -> Option<SearchResult> {
        for engine in &self.engines {
            if let Ok(engine) = engine.lock() {
                if context
                    .session()
                    .is_none_or(|session| engine.owns_session(session))
                {
                    if let Some(state) = engine.empty_state(context, query) {
                        return Some(state);
                    }
                }
            }
        }
        None
    }

    /// Request more rows only from the provider that owns an entered search
    /// engine. A default query has no continuation page.
    pub fn query_more(
        &self,
        context: &SearchContext,
        query: &str,
        offset: usize,
    ) -> Vec<SearchResult> {
        let SearchContext::Engine(session) = context else {
            return Vec::new();
        };
        for engine in &self.engines {
            if let Ok(engine) = engine.lock() {
                if engine.owns_session(session) {
                    return engine.query_active_page(session, query, offset);
                }
            }
        }
        Vec::new()
    }

    pub fn search_debounce(&self, context: &SearchContext) -> Duration {
        let Some(session) = context.session() else {
            return DEFAULT_SEARCH_DEBOUNCE;
        };
        for engine in &self.engines {
            if let Ok(engine) = engine.lock() {
                if engine.owns_session(session) {
                    return engine.search_debounce(session);
                }
            }
        }
        DEFAULT_SEARCH_DEBOUNCE
    }

    pub fn search_context(&self, query: &str) -> SearchContext {
        self.engines
            .iter()
            .find_map(|engine| {
                engine
                    .lock()
                    .ok()
                    .and_then(|engine| engine.search_engine(query))
            })
            .map(SearchContext::Engine)
            .unwrap_or(SearchContext::Default)
    }

    pub fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        self.engines.iter().find_map(|engine| {
            engine
                .lock()
                .ok()
                .and_then(|engine| engine.shortcut_hint(result_id))
        })
    }

    pub fn search_engine_result(&self, session: &SearchEngineSession) -> Option<SearchResult> {
        self.engines.iter().find_map(|engine| {
            engine.lock().ok().and_then(|engine| {
                engine
                    .owns_session(session)
                    .then(|| engine.search_engine_result(session))
                    .flatten()
            })
        })
    }

    pub fn is_fallback_result(&self, item: &SearchResult) -> bool {
        if item.ranking_bias >= 0 {
            return false;
        }
        self.shortcut_hint(&item.id).is_some()
    }

    pub fn replace_shortcut_config(
        &self,
        shortcuts: Vec<ShortcutConfig>,
        shortcut_keywords: ShortcutKeywords,
    ) {
        for engine in &self.engines {
            if let Ok(mut engine) = engine.lock() {
                engine.replace_config(shortcuts.clone(), shortcut_keywords.clone());
            }
        }
    }

    pub fn set_language(&self, language: String) {
        self.plugins.set_locale(language.clone());
        for engine in &self.engines {
            if let Ok(mut engine) = engine.lock() {
                engine.set_language(language.clone());
            }
        }
    }

    pub fn replace_plugin_config(
        &self,
        plugin_configs: &std::collections::BTreeMap<String, PluginConfig>,
    ) {
        self.plugins.replace_config(plugin_configs);
    }

    pub fn execute_plugin_action(
        &self,
        item: &SearchResult,
        requested_action_id: &str,
    ) -> Result<crate::plugin::ExecuteResponse> {
        let (plugin_id, action_id) = find_plugin_action(item, requested_action_id)?;
        let item_id = item
            .id
            .strip_prefix(&format!("plugin:{plugin_id}:"))
            .unwrap_or_else(|| item.id.rsplit(':').next().unwrap_or_default());
        let output = self.plugins.execute(plugin_id, item_id, action_id)?;
        serde_json::from_str(&output).context("decode plugin execute response")
    }
}

struct IndexedSearchEngine {
    indexed: Arc<Mutex<Arc<Vec<SearchResult>>>>,
    control_panel: bool,
    language: String,
}

impl IndexedSearchEngine {
    fn apps(indexed: Arc<Mutex<Arc<Vec<SearchResult>>>>) -> Self {
        Self {
            indexed,
            control_panel: false,
            language: "en".into(),
        }
    }

    fn control_panel(indexed: Arc<Mutex<Arc<Vec<SearchResult>>>>) -> Self {
        Self {
            indexed,
            control_panel: true,
            language: "en".into(),
        }
    }
}

impl SearchEngineProvider for IndexedSearchEngine {
    fn settings(&self) -> SearchEngineSettings {
        SearchEngineSettings {
            // In a launcher, an installed application or a system setting is
            // usually a more useful tie-break than a filesystem document.
            ranking_bias: if self.control_panel { 15 } else { 25 },
            ..SearchEngineSettings::default()
        }
    }

    fn search_engine(&self, _query: &str) -> Option<SearchEngineSession> {
        None
    }

    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        self.indexed
            .lock()
            .map(|items| {
                items
                    .iter()
                    .filter(|item| {
                        let is_control_panel = matches!(
                            item.category,
                            crate::result::ResultCategory::ControlPanel
                                | crate::result::ResultCategory::Settings
                        );
                        is_control_panel == self.control_panel
                    })
                    .cloned()
                    .map(|item| {
                        if matches!(
                            item.category,
                            crate::result::ResultCategory::Settings
                                | crate::result::ResultCategory::ControlPanel
                        ) {
                            crate::providers::windows_settings::localized_result(
                                item,
                                &self.language,
                            )
                        } else {
                            item
                        }
                    })
                    .filter(|item| {
                        if query.contains('>')
                            && matches!(
                                item.category,
                                crate::result::ResultCategory::ControlPanel
                                    | crate::result::ResultCategory::Settings
                            )
                            && !crate::providers::windows_settings::matches_settings_path(
                                item, query,
                            )
                        {
                            return false;
                        }
                        true
                    })
                    .map(|mut item| {
                        if query.contains('>')
                            && crate::providers::windows_settings::matches_settings_path(
                                &item, query,
                            )
                        {
                            item.aliases.push(query.trim().to_owned());
                        }
                        item
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_language(&mut self, language: String) {
        self.language = language;
    }
}

#[cfg(test)]
mod context_action_tests {
    use super::find_plugin_action;
    use crate::result::{
        Action, ContextMenuAction, ContextMenuBehavior, ResultCategory, ResultVisibility,
        SearchResult,
    };

    #[test]
    fn plugin_context_actions_use_a_separate_action_id_namespace() {
        let item = SearchResult {
            id: "plugin:weather:current".into(),
            identity: "plugin:weather:current".into(),
            title: "Weather".into(),
            category: ResultCategory::Other,
            aliases: Vec::new(),
            location: String::new(),
            launch_target: Default::default(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "plugin:weather".into(),
            sources: vec!["plugin:weather".into()],
            launch_entries: Vec::new(),
            actions: vec![
                Action::Plugin {
                    plugin_id: "other-plugin".into(),
                    action_id: "refresh".into(),
                    icon: None,
                },
                Action::ContextMenu(ContextMenuAction {
                    id: "refresh-weather".into(),
                    title: "Refresh".into(),
                    subtitle: String::new(),
                    icon: None,
                    behavior: ContextMenuBehavior::Plugin {
                        plugin_id: "weather".into(),
                        action_id: "refresh".into(),
                    },
                }),
            ],
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: None,
            ranking_bias: 0,
            allow_favorite: true,
            visibility: ResultVisibility::Always,
        };

        assert_eq!(
            find_plugin_action(&item, "context-menu:refresh-weather").unwrap(),
            ("weather", "refresh")
        );
        assert_eq!(
            find_plugin_action(&item, "refresh").unwrap(),
            ("other-plugin", "refresh")
        );
    }
}
