//! Runtime provider registry.
//!
//! SearchCore owns the search algorithm, while this module owns the list of
//! dynamic result providers and explicitly entered search engines. Adding a
//! result provider requires `DynamicProvider`; adding a shortcut-owned engine
//! requires `SearchEngineProvider`.

use crate::{
    config::{PluginConfig, ShortcutConfig},
    page::{SearchContext, SearchEngineSession},
    plugin::PluginManager,
    result::SearchResult,
};
use anyhow::{Context, Result};
use std::sync::Mutex;

pub trait DynamicProvider: Send + Sync {
    fn query(&self, query: &str) -> Vec<SearchResult>;

    /// Whether this provider exposes candidates that SearchCore may match
    /// fuzzily. The matching algorithm and the user setting remain central.
    fn supports_fuzzy_search(&self) -> bool {
        false
    }

    /// Results used only as candidates for fuzzy matching. Providers should
    /// keep query-specific generation in `query` and return stable searchable
    /// entries here.
    fn fuzzy_candidates(&self) -> Vec<SearchResult> {
        Vec::new()
    }

    fn replace_config(&mut self, _shortcuts: Vec<ShortcutConfig>) {}

    fn set_language(&mut self, _language: String) {}
}

/// A search engine owns a query only after a shortcut has selected it.
///
/// Keeping this distinct from `DynamicProvider` prevents a result-producing
/// provider from accidentally becoming a routing mechanism.
pub trait SearchEngineProvider: Send + Sync {
    fn search_engine(&self, query: &str) -> Option<SearchEngineSession>;
    /// Custom results contributed to the default search engine.
    fn query(&self, query: &str) -> Vec<SearchResult>;

    /// Results owned after this engine has been entered. Most search engines
    /// return their normal query results; an action-style engine can narrow
    /// the surface to the result selected by its shortcut.
    fn query_active(&self, _session: &SearchEngineSession, query: &str) -> Vec<SearchResult> {
        self.query(query)
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

    fn replace_config(&mut self, _shortcuts: Vec<ShortcutConfig>) {}

    fn set_language(&mut self, _language: String) {}
}

pub(crate) fn find_plugin_action<'a>(
    item: &'a SearchResult,
    requested_action_id: &str,
) -> Result<(&'a str, &'a str)> {
    if let Some(action) = item.actions.iter().find_map(|action| match action {
        crate::result::Action::Plugin {
            plugin_id,
            action_id,
            ..
        } if action_id == requested_action_id => Some((plugin_id.as_str(), action_id.as_str())),
        _ => None,
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
    dynamic: Vec<Mutex<Box<dyn DynamicProvider>>>,
    engines: Vec<Mutex<Box<dyn SearchEngineProvider>>>,
}

impl ProviderRegistry {
    #[cfg(test)]
    pub fn without_dynamic(plugins: PluginManager) -> Self {
        Self {
            plugins,
            dynamic: Vec::new(),
            engines: Vec::new(),
        }
    }

    pub fn default_providers(plugins: PluginManager, shortcuts: Vec<ShortcutConfig>) -> Self {
        let mut registry = Self {
            plugins,
            dynamic: Vec::new(),
            engines: Vec::new(),
        };
        crate::providers::dynamic::register_all(&mut registry, shortcuts);
        registry
    }

    pub fn register<P>(&mut self, provider: P)
    where
        P: DynamicProvider + 'static,
    {
        self.dynamic.push(Mutex::new(Box::new(provider)));
    }

    pub fn register_search_engine<P>(&mut self, engine: P)
    where
        P: SearchEngineProvider + 'static,
    {
        self.engines.push(Mutex::new(Box::new(engine)));
    }

    pub fn query(
        &self,
        context: &SearchContext,
        query: &str,
        fuzzy_search: bool,
    ) -> Vec<SearchResult> {
        if let SearchContext::Engine(session) = context {
            if self.plugins.owns_search_engine_session(session) {
                return self.plugins.query_active_search_engine(session, query);
            }
            for engine in &self.engines {
                if let Ok(engine) = engine.lock() {
                    if engine.owns_session(session) {
                        return engine.query_active(session, query);
                    }
                }
            }
            return Vec::new();
        }

        let mut results = self.plugins.query(query);
        // Engine-owned custom results remain discoverable from the default
        // engine. A matching shortcut merely changes the query's owner.
        for engine in &self.engines {
            if let Ok(engine) = engine.lock() {
                results.extend(engine.query(query));
            }
        }
        for provider in &self.dynamic {
            if let Ok(provider) = provider.lock() {
                results.extend(provider.query(query));
                if fuzzy_search && !query.trim().is_empty() && provider.supports_fuzzy_search() {
                    for candidate in provider.fuzzy_candidates() {
                        if !results.iter().any(|result| result.id == candidate.id) {
                            results.push(candidate);
                        }
                    }
                }
            }
        }
        results
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

    pub fn preferred_view_mode(&self, context: &SearchContext) -> &'static str {
        context
            .session()
            .map(|session| self.plugins.preferred_view_mode_for_session(session))
            .unwrap_or("list")
    }

    pub fn search_context(&self, query: &str) -> SearchContext {
        if let Some(session) = self.plugins.search_engine(query) {
            return SearchContext::Engine(session);
        }
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

    pub fn is_fallback_result(&self, item: &SearchResult) -> bool {
        item.ranking_bias < 0 && self.shortcut_hint(&item.id).is_some()
    }

    pub fn replace_shortcuts(&self, shortcuts: Vec<ShortcutConfig>) {
        for provider in &self.dynamic {
            if let Ok(mut provider) = provider.lock() {
                provider.replace_config(shortcuts.clone());
            }
        }
        for engine in &self.engines {
            if let Ok(mut engine) = engine.lock() {
                engine.replace_config(shortcuts.clone());
            }
        }
    }

    pub fn set_language(&self, language: String) {
        self.plugins.set_locale(language.clone());
        for provider in &self.dynamic {
            if let Ok(mut provider) = provider.lock() {
                provider.set_language(language.clone());
            }
        }
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
