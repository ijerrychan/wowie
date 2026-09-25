use crate::plugin::PluginManager;
use crate::provider::{EngineSearchResult, ProviderRegistry, SearchEngineSettings};
use crate::ranking::Ranker;
use crate::result::{ResultVisibility, SearchResult};
use anyhow::Result;
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

pub struct SearchCore {
    providers: ProviderRegistry,
    visible_results: Mutex<Arc<Vec<SearchResult>>>,
    separate_launch_entries: AtomicBool,
    show_advanced_launch_entries: AtomicBool,
    category_order: Mutex<HashMap<String, usize>>,
    plugin_order: Mutex<HashMap<String, usize>>,
    ranker: Ranker,
    fuzzy_search: AtomicBool,
}

impl SearchCore {
    #[cfg(test)]
    pub fn new(results: Vec<SearchResult>) -> Self {
        Self {
            providers: ProviderRegistry::without_dynamic(PluginManager::discover_with_config(
                "en",
                &std::collections::BTreeMap::new(),
            )),
            visible_results: Mutex::new(Arc::new(Vec::new())),
            separate_launch_entries: AtomicBool::new(false),
            show_advanced_launch_entries: AtomicBool::new(false),
            category_order: Mutex::new(HashMap::new()),
            plugin_order: Mutex::new(HashMap::new()),
            ranker: Ranker::in_memory(),
            fuzzy_search: AtomicBool::new(true),
        }
        .with_indexed_results(results)
    }

    #[cfg(test)]
    pub fn with_plugins_and_shortcuts(
        results: Vec<SearchResult>,
        plugins: PluginManager,
        shortcuts: Vec<crate::config::ShortcutConfig>,
    ) -> Self {
        Self::with_plugins_and_shortcuts_and_keywords(
            results,
            plugins,
            shortcuts,
            crate::config::ShortcutKeywords::default(),
        )
    }

    pub fn with_plugins_and_shortcuts_and_keywords(
        results: Vec<SearchResult>,
        plugins: PluginManager,
        shortcuts: Vec<crate::config::ShortcutConfig>,
        shortcut_keywords: crate::config::ShortcutKeywords,
    ) -> Self {
        Self {
            providers: ProviderRegistry::default_providers_with_keywords(
                plugins,
                shortcuts,
                shortcut_keywords,
            ),
            visible_results: Mutex::new(Arc::new(Vec::new())),
            separate_launch_entries: AtomicBool::new(false),
            show_advanced_launch_entries: AtomicBool::new(false),
            category_order: Mutex::new(HashMap::new()),
            plugin_order: Mutex::new(HashMap::new()),
            ranker: Ranker::load(),
            fuzzy_search: AtomicBool::new(true),
        }
        .with_indexed_results(results)
    }

    fn with_indexed_results(self, results: Vec<SearchResult>) -> Self {
        self.providers.replace_indexed_results(Arc::new(results));
        self
    }

    pub fn replace_shortcut_config(
        &self,
        shortcuts: Vec<crate::config::ShortcutConfig>,
        shortcut_keywords: crate::config::ShortcutKeywords,
    ) {
        self.providers
            .replace_shortcut_config(shortcuts, shortcut_keywords);
    }

    pub fn replace_fuzzy_search(&self, enabled: bool) {
        self.fuzzy_search.store(enabled, Ordering::Release);
    }

    pub fn replace_separate_launch_entries(&self, enabled: bool) {
        self.separate_launch_entries
            .store(enabled, Ordering::Release);
    }

    pub fn replace_show_advanced_launch_entries(&self, enabled: bool) {
        self.show_advanced_launch_entries
            .store(enabled, Ordering::Release);
    }

    pub fn replace_priority_order(&self, categories: &[String], plugins: &[String]) {
        if let Ok(mut current) = self.category_order.lock() {
            *current = categories
                .iter()
                .enumerate()
                .map(|(index, id)| (id.clone(), index))
                .collect();
        }
        if let Ok(mut current) = self.plugin_order.lock() {
            *current = plugins
                .iter()
                .enumerate()
                .map(|(index, id)| (id.clone(), index))
                .collect();
        }
    }

    pub fn replace_provider_language(&self, language: String) {
        self.providers.set_language(language);
    }

    pub fn search_debounce(&self, context: &crate::page::SearchContext) -> std::time::Duration {
        self.providers.search_debounce(context)
    }

    pub fn search_context(&self, query: &str) -> crate::page::SearchContext {
        self.providers.search_context(query)
    }

    pub fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        self.providers.shortcut_hint(result_id)
    }

    pub fn search_engine_result(
        &self,
        session: &crate::page::SearchEngineSession,
    ) -> Option<SearchResult> {
        self.providers.search_engine_result(session)
    }

    pub fn is_favorite(&self, item_id: &str) -> bool {
        self.ranker.is_favorite(item_id)
    }

    /// Add or remove a persistent favorite. Returns the new state, or `None`
    /// if persistence state is unavailable or the favorite limit was reached.
    pub fn toggle_favorite(
        &self,
        item: &SearchResult,
        session: Option<&crate::page::SearchEngineSession>,
    ) -> Option<bool> {
        self.ranker.toggle_favorite(item, session)
    }

    pub fn favorite_session(&self, item_id: &str) -> Option<crate::page::SearchEngineSession> {
        self.ranker.favorite_session(item_id)
    }

    pub fn replace_plugin_config(
        &self,
        plugin_configs: &std::collections::BTreeMap<String, crate::config::PluginConfig>,
    ) {
        self.providers.replace_plugin_config(plugin_configs);
    }

    #[cfg(test)]
    pub fn record_success(&self, item_id: &str, query: &str) {
        self.ranker.record_success(item_id, query);
    }

    pub fn record_action_success(
        &self,
        item_id: &str,
        query: &str,
        session: Option<&crate::page::SearchEngineSession>,
    ) {
        let engine =
            session.map(|session| (session.id.as_str(), self.providers.history_limit(session)));
        self.ranker.record_action_success(item_id, query, engine);
    }

    /// Publish only the result snapshot accepted by the UI after its generation
    /// check. Background searches and rejected pages must never change actions
    /// associated with rows that are currently visible.
    pub fn publish_results(&self, results: Arc<Vec<SearchResult>>) {
        if let Ok(mut visible) = self.visible_results.lock() {
            *visible = results;
        }
    }

    pub fn by_id(&self, id: &str) -> Option<SearchResult> {
        self.visible_results
            .lock()
            .ok()
            .and_then(|visible| visible.iter().find(|item| item.id == id).cloned())
            .or_else(|| self.providers.indexed_by_id(id))
    }

    pub fn replace_results(&self, results: Arc<Vec<SearchResult>>) {
        self.providers.replace_indexed_results(results);
    }

    /// Execute an action against the result captured when the user invoked it.
    /// This also supports transient results that are absent from `by_id`.
    pub fn execute_transient_plugin_action(
        &self,
        item: &SearchResult,
        requested_action_id: &str,
    ) -> Result<crate::plugin::ExecuteResponse> {
        let (_, action_id) = crate::provider::find_plugin_action(item, requested_action_id)?;
        self.providers.execute_plugin_action(item, action_id)
    }

    /// Convenience wrapper retained for unit tests and small in-process
    /// callers. The UI uses the explicit context-aware methods below.
    #[cfg(test)]
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let context = self.search_context(query);
        let results = self.search_in_context(&context, query);
        self.publish_results(Arc::new(results.clone()));
        results
    }

    /// Fetch a page without publishing it. The caller merges and publishes only
    /// after verifying that the page still belongs to the displayed query.
    pub fn load_more_in_context(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
        offset: usize,
    ) -> Vec<SearchResult> {
        self.providers.query_more(context, query, offset)
    }

    /// Search a context resolved by the caller. UI code should resolve once
    /// and reuse this value for rendering, navigation and result retrieval.
    pub fn search_in_context(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
    ) -> Vec<SearchResult> {
        let input = query.to_owned();
        // An active engine receives the text currently shown in the input
        // field. The UI stores the selected engine separately, so the field
        // no longer needs to contain or be parsed for its shortcut prefix.
        let query = input.trim().to_lowercase();
        let root_empty_query =
            matches!(context, crate::page::SearchContext::Default) && query.is_empty();
        let favorites = self.ranker.favorite_snapshot(root_empty_query);
        let fuzzy_search = self.fuzzy_search.load(Ordering::Acquire);
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let mut matcher_buffer = Vec::new();
        let mut needle_buffer = Vec::new();
        let debug = std::env::var_os("WOWIE_SEARCH_DEBUG").is_some_and(|value| value != "0");
        if debug {
            eprintln!(
                "[search] query={:?} indexed_records={}",
                query,
                self.providers.indexed_len()
            );
        }
        let history = if query.is_empty() {
            context.session().and_then(|session| {
                let limit = self.providers.history_limit(session);
                (limit > 0).then(|| self.ranker.recent_engine_searches(&session.id, limit))
            })
        } else {
            None
        };
        // Preserve recency regardless of learned usage scores or title order.
        let history_order = history
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.result_id.as_str(), index))
            .collect::<std::collections::HashMap<_, _>>();
        let mut dynamic = self
            .providers
            .query(context, &input, history.as_deref().unwrap_or_default())
            .into_iter()
            .filter(|candidate| {
                candidate.item.visibility.allows(&input)
                    && (!matches!(context, crate::page::SearchContext::Default)
                        || !input.is_empty()
                        || candidate.item.visibility == ResultVisibility::EmptyQueryOnly)
            })
            .collect::<Vec<_>>();
        if matches!(context, crate::page::SearchContext::Default)
            && !query.is_empty()
            && self.separate_launch_entries.load(Ordering::Acquire)
        {
            let show_advanced_launch_entries =
                self.show_advanced_launch_entries.load(Ordering::Acquire);
            let mut separated = Vec::with_capacity(dynamic.len());
            for candidate in dynamic {
                if crate::providers::app::preferred_launch_entry(&candidate.item).is_none() {
                    separated.push(candidate);
                    continue;
                }
                let other_entries = crate::providers::app::visible_launch_entries(
                    &candidate.item,
                    show_advanced_launch_entries,
                )
                .into_iter()
                .map(|(_, entry)| entry.clone())
                .collect::<Vec<_>>();
                separated.push(candidate.clone());
                separated.extend(other_entries.into_iter().map(|entry| {
                    let mut item = candidate.item.clone();
                    let label = crate::providers::app::launch_entry_label(&entry);
                    let launch_hash = stable_launch_entry_hash(&entry);
                    item.id = format!("{}:launch:{launch_hash:016x}", item.id);
                    item.identity = format!("{}:launch:{launch_hash:016x}", item.identity);
                    item.title = format!("{} — {}", item.title, label);
                    if !label.is_empty() && !item.aliases.iter().any(|alias| alias == &label) {
                        item.aliases.push(label);
                    }
                    item.source = entry.source.clone();
                    item.location = entry.path.to_string_lossy().into_owned();
                    item.launch_target = entry.target.clone();
                    item.launch_arguments = entry.arguments.clone();
                    item.launch_working_directory = entry.working_directory.clone();
                    item.launch_entries = vec![entry.clone()];
                    item.actions
                        .retain(|action| !matches!(action, crate::result::Action::Launch(_)));
                    item.actions.insert(0, crate::result::Action::Launch(entry));
                    EngineSearchResult {
                        item,
                        settings: candidate.settings,
                    }
                }));
            }
            dynamic = separated;
        }
        if root_empty_query {
            let mut candidate_ids = dynamic
                .iter()
                .enumerate()
                .map(|(index, candidate)| (candidate.item.id.clone(), index))
                .collect::<std::collections::HashMap<_, _>>();
            for favorite in &favorites.items {
                if let Some(index) = candidate_ids.get(&favorite.item.id).copied() {
                    // Earlier versions reused a session ID for its generated
                    // command result and the root entry. Prefer the saved
                    // session result to preserve those existing favorites.
                    if favorite.session.is_some() {
                        dynamic[index].item = favorite.item.clone();
                    }
                } else {
                    candidate_ids.insert(favorite.item.id.clone(), dynamic.len());
                    dynamic.push(EngineSearchResult {
                        item: favorite.item.clone(),
                        settings: SearchEngineSettings::default(),
                    });
                }
            }
        }
        let category_order = self
            .category_order
            .lock()
            .map(|priorities| priorities.clone())
            .unwrap_or_default();
        let plugin_order = self
            .plugin_order
            .lock()
            .map(|priorities| priorities.clone())
            .unwrap_or_default();
        let mut ranked: Vec<(RankKey, &SearchResult)> = dynamic.iter().filter_map(|candidate| {
                let item = &candidate.item;
                // Every provider contributes ordinary SearchResults. The
                // central matcher is the single place that decides whether a
                // result is visible and how strongly it matches.
                let engine_fuzzy_search = candidate.settings.respect_global_fuzzy_search && fuzzy_search;
                let title_score = match_score(
                    &query,
                    &item.title,
                    engine_fuzzy_search,
                    &mut matcher,
                    &mut matcher_buffer,
                    &mut needle_buffer,
                );
                let alias_score = item
                    .aliases
                    .iter()
                    .filter_map(|alias| {
                        let score = match_score(
                            &query,
                            alias,
                            engine_fuzzy_search,
                            &mut matcher,
                            &mut matcher_buffer,
                            &mut needle_buffer,
                        )?;
                        Some(score)
                    })
                    .max_by_key(|score| (score.class, score.quality));
                let score = [title_score, alias_score]
                    .into_iter()
                    .flatten()
                    .max_by_key(|score| (score.class, score.quality));
                if debug && score.is_some() {
                    eprintln!(
                        "[search] candidate title={:?} score={:?} matched={} identity={:?} sources={:?} location={:?} launch_target={:?} args={:?}",
                        item.title,
                        score,
                        score.is_some(),
                        item.identity,
                        item.sources,
                        item.location,
                        item.launch_target,
                        item.launch_arguments
                    );
                }
                // Custom results that have a registered shortcut are always
                // visible as a fallback in the default engine, even when the
                // current text matches neither their title nor aliases.
                let fallback = self.providers.is_fallback_result(item);
                let favorite_order = favorites.order.get(&item.id).copied();
                // Generated fallback titles contain the query itself and must
                // not compete with matches against actual indexed names.
                (if fallback {
                    Some(MatchScore { class: 0, quality: 0 })
                } else {
                    score
                })
                .map(|score| {
                    (
                        RankKey {
                            history_order: history_order.get(item.id.as_str()).copied(),
                            favorite_order,
                            fallback,
                            match_class: score.class,
                            match_quality: score.quality,
                            category_order: category_order
                                .get(category_priority_key(item.category))
                                .copied()
                                .unwrap_or(usize::MAX),
                            plugin_order: item
                                .source
                                .as_str()
                                .strip_prefix("plugin:")
                                .and_then(|plugin_id| plugin_order.get(plugin_id).copied())
                                .unwrap_or(usize::MAX),
                            behavior: 0,
                            engine_bias: candidate.settings.ranking_bias.clamp(-100, 100),
                            provider_bias: item.ranking_bias,
                        },
                        item,
                    )
                })
            })
            .collect();
        let behavior_scores = self
            .ranker
            .scores(ranked.iter().map(|(_, item)| item.id.as_str()), &query);
        for ((key, _), behavior) in ranked.iter_mut().zip(behavior_scores) {
            key.behavior = behavior;
        }
        ranked.sort_by(|a, b| {
            a.0.compare(&b.0)
                .then_with(|| a.1.title.cmp(&b.1.title))
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        let found: Vec<SearchResult> = ranked.into_iter().map(|(_, item)| item.clone()).collect();
        if debug {
            eprintln!("[search] final_results={}", found.len());
            for (index, item) in found.iter().enumerate() {
                eprintln!(
                    "[search] result[{}] title={:?} identity={:?} sources={:?} location={:?} launch_target={:?} args={:?} working_directory={:?} launch_entries={:?}",
                    index,
                    item.title,
                    item.identity,
                    item.sources,
                    item.location,
                    item.launch_target,
                    item.launch_arguments,
                    item.launch_working_directory,
                    item.launch_entries
                );
            }
        }
        found
    }

    pub fn empty_state(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
    ) -> Option<SearchResult> {
        self.providers.empty_state(context, query)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MatchScore {
    class: u8,
    quality: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RankKey {
    history_order: Option<usize>,
    favorite_order: Option<usize>,
    fallback: bool,
    match_class: u8,
    match_quality: i32,
    category_order: usize,
    plugin_order: usize,
    behavior: i32,
    engine_bias: i32,
    provider_bias: i32,
}

impl RankKey {
    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Reverse;
        // Every field has a total order. In particular, unlisted providers use
        // the same trailing plugin priority instead of comparing equal to all
        // configured plugins, which would break comparison transitivity.
        let key = |value: &Self| {
            (
                value.history_order.unwrap_or(usize::MAX),
                value.favorite_order.unwrap_or(usize::MAX),
                Reverse(value.match_class),
                Reverse(value.match_quality),
                value.fallback,
                value.category_order,
                value.plugin_order,
                Reverse(value.behavior),
                Reverse(value.engine_bias),
                Reverse(value.provider_bias),
            )
        };
        key(self).cmp(&key(other))
    }
}

fn category_priority_key(category: crate::result::ResultCategory) -> &'static str {
    use crate::result::ResultCategory;
    match category {
        ResultCategory::Unclassified => "unclassified",
        ResultCategory::Application => "application",
        ResultCategory::Game => "game",
        ResultCategory::Website => "website",
        ResultCategory::Calculator => "calculator",
        ResultCategory::ControlPanel => "control_panel",
        ResultCategory::Document => "document",
        ResultCategory::Other => "other",
    }
}

fn stable_launch_entry_hash(entry: &crate::result::LaunchEntry) -> u64 {
    let fields = [
        entry.source.as_str().into_owned(),
        entry.path.to_string_lossy().into_owned(),
        entry.target.to_string_lossy().into_owned(),
        entry.arguments.clone(),
        entry
            .working_directory
            .as_ref()
            .map(|directory| directory.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ];
    let mut hash = 0xcbf29ce484222325_u64;
    for field in fields {
        for byte in field.bytes().chain(std::iter::once(0xff)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

fn match_score(
    query: &str,
    title: &str,
    fuzzy_search: bool,
    matcher: &mut Matcher,
    matcher_buffer: &mut Vec<char>,
    needle_buffer: &mut Vec<char>,
) -> Option<MatchScore> {
    if query.is_empty() {
        return Some(MatchScore {
            class: 0,
            quality: 0,
        });
    }
    if query.is_ascii() && title.is_ascii() {
        let title_len = title.chars().count() as i32;
        if ascii_eq_ignore_case(title, query) {
            return Some(MatchScore {
                class: 4,
                quality: 1000,
            });
        }
        if ascii_starts_with_ignore_case(title, query) {
            return Some(MatchScore {
                class: 3,
                quality: (1000 - title_len).max(1),
            });
        }
        if ascii_contains_ignore_case(title, query) {
            return Some(MatchScore {
                class: 2,
                quality: (1000 - title_len).max(1),
            });
        }
    }

    let text = title.to_lowercase();
    if text == query {
        return Some(MatchScore {
            class: 4,
            quality: 1000,
        });
    }
    if text.starts_with(query) {
        return Some(MatchScore {
            class: 3,
            quality: (1000 - text.chars().count() as i32).max(1),
        });
    }
    if text.contains(query) {
        return Some(MatchScore {
            class: 2,
            quality: (1000 - text.chars().count() as i32).max(1),
        });
    }
    if fuzzy_search {
        let haystack = Utf32Str::new(&text, matcher_buffer);
        let needle = Utf32Str::new(query, needle_buffer);
        if let Some(score) = matcher.fuzzy_match(haystack, needle) {
            return Some(MatchScore {
                class: 1,
                quality: score as i32,
            });
        }
    }
    None
}

fn ascii_eq_ignore_case(left: &str, right: &str) -> bool {
    ascii_bytes_eq_ignore_case(left.as_bytes(), right.as_bytes())
}

fn ascii_starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && ascii_bytes_eq_ignore_case(&text.as_bytes()[..prefix.len()], prefix.as_bytes())
}

fn ascii_contains_ignore_case(text: &str, needle: &str) -> bool {
    text.as_bytes()
        .windows(needle.len())
        .any(|window| ascii_bytes_eq_ignore_case(window, needle.as_bytes()))
}

fn ascii_bytes_eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::ResultCategory;

    fn result(id: &str, title: &str, category: ResultCategory) -> SearchResult {
        SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            category,
            aliases: vec![],
            location: String::new(),
            launch_target: title.into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: None,
            ranking_bias: 0,
            allow_favorite: true,
            visibility: ResultVisibility::Always,
        }
    }

    #[test]
    fn rejected_search_does_not_replace_visible_actions() {
        let core = SearchCore::new(Vec::new());
        let mut visible = result("dynamic", "Accepted", ResultCategory::Other);
        visible.launch_arguments = "accepted arguments".into();
        let snapshot = Arc::new(vec![visible.clone()]);
        core.publish_results(Arc::clone(&snapshot));
        assert!(core
            .search_in_context(&crate::page::SearchContext::Default, "stale query")
            .is_empty());
        assert_eq!(core.by_id("dynamic"), Some(visible));
        assert!(Arc::ptr_eq(
            &core.visible_results.lock().unwrap(),
            &snapshot
        ));
        core.publish_results(Arc::new(Vec::new()));
        assert!(core.by_id("dynamic").is_none());
    }

    #[test]
    fn visible_snapshot_wins_over_a_refreshed_index() {
        let indexed = result("app", "New index entry", ResultCategory::Application);
        let core = SearchCore::new(vec![indexed.clone()]);
        let mut displayed = indexed.clone();
        displayed.title = "Currently displayed entry".into();
        displayed.launch_arguments = "displayed arguments".into();
        core.publish_results(Arc::new(vec![displayed.clone()]));
        core.replace_results(Arc::new(vec![indexed.clone()]));
        assert_eq!(core.by_id("app"), Some(displayed));
        core.publish_results(Arc::new(vec![indexed.clone()]));
        assert_eq!(core.by_id("app"), Some(indexed));
    }

    #[test]
    fn mixed_provider_priorities_are_transitive() {
        let base = RankKey {
            history_order: None,
            favorite_order: None,
            fallback: false,
            match_class: 3,
            match_quality: 100,
            category_order: 0,
            plugin_order: usize::MAX,
            behavior: 0,
            engine_bias: 0,
            provider_bias: 0,
        };
        let keys = [
            RankKey {
                plugin_order: 0,
                behavior: 0,
                ..base
            },
            RankKey {
                plugin_order: 1,
                behavior: 20,
                ..base
            },
            RankKey {
                behavior: 10,
                ..base
            },
            RankKey {
                favorite_order: Some(0),
                ..base
            },
            RankKey {
                history_order: Some(0),
                ..base
            },
            RankKey {
                match_quality: 200,
                ..base
            },
        ];
        for a in keys {
            for b in keys {
                assert_eq!(a.compare(&b), b.compare(&a).reverse());
                for c in keys {
                    if a.compare(&b).is_le() && b.compare(&c).is_le() {
                        assert!(a.compare(&c).is_le(), "{a:?}, {b:?}, {c:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn plugin_priority_is_stable_when_mixed_with_unlisted_providers() {
        let mut first = result("first", "alpha", ResultCategory::Application);
        first.source = "plugin:first".into();
        let mut second = result("second", "alpha", ResultCategory::Application);
        second.source = "plugin:second".into();
        second.ranking_bias = 20;
        let mut ordinary = result("ordinary", "alpha", ResultCategory::Application);
        ordinary.ranking_bias = 10;
        let items = [first, second, ordinary];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let core = SearchCore::new(
                order
                    .into_iter()
                    .map(|index| items[index].clone())
                    .collect(),
            );
            core.replace_priority_order(&[], &["first".into(), "second".into()]);
            let ids = core
                .search("alpha")
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>();
            assert_eq!(ids, ["first", "second", "ordinary"]);
        }
    }

    #[test]
    fn ranks_prefix_first() {
        let make = |id: &str, title: &str| SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: "".into(),
            launch_target: title.into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        let core = SearchCore::new(vec![make("1", "Calculator"), make("2", "Calendar")]);
        assert_eq!(core.search("calc")[0].title, "Calculator");
    }

    #[test]
    fn stronger_match_beats_category_preference() {
        let app = result("app", "Alphabet", ResultCategory::Application);
        let website = result("site", "alpha", ResultCategory::Website);
        assert_eq!(
            SearchCore::new(vec![app, website]).search("alpha")[0].id,
            "site"
        );
    }

    #[test]
    fn application_breaks_an_equal_match_tie_before_website() {
        let app = result("app", "alpha", ResultCategory::Application);
        let website = result("site", "alpha", ResultCategory::Website);
        assert_eq!(
            SearchCore::new(vec![website, app]).search("alpha")[0].id,
            "app"
        );
    }

    #[test]
    fn favorites_are_injected_into_empty_root_search_and_can_be_removed() {
        let favorite = result("favorite", "Weather", ResultCategory::Other);
        let core = SearchCore::new(vec![favorite.clone()]);
        assert_eq!(core.toggle_favorite(&favorite, None), Some(true));

        let results = core.search("");
        assert_eq!(
            results
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["favorite"]
        );
        assert_eq!(core.toggle_favorite(&favorite, None), Some(false));
        assert!(core.search("").is_empty());
    }

    #[test]
    fn favorite_matches_are_pinned_above_stronger_non_favorites() {
        let favorite = result("favorite", "alpha calculator", ResultCategory::Other);
        let exact = result("exact", "alpha", ResultCategory::Other);
        let core = SearchCore::new(vec![exact, favorite.clone()]);
        assert_eq!(core.toggle_favorite(&favorite, None), Some(true));

        let results = core.search("alpha");
        assert_eq!(results[0].id, "favorite");
    }

    #[test]
    fn favorites_retain_their_search_engine_session_context() {
        let favorite = result("language", "Chinese", ResultCategory::Other);
        let session = crate::page::SearchEngineSession {
            id: "plugin:translate".into(),
            title: "Translate".into(),
            placeholder: "Text to translate".into(),
            prefix: "t ".into(),
            argument: String::new(),
            mode: crate::page::SearchEngineMode::Panel,
            state: Default::default(),
        };
        let core = SearchCore::new(vec![favorite.clone()]);
        assert_eq!(core.toggle_favorite(&favorite, Some(&session)), Some(true));

        assert_eq!(core.favorite_session(&favorite.id), Some(session));
    }

    #[test]
    fn engine_search_history_stays_newest_first_after_ranking() {
        let plugins = crate::plugin::PluginManager::discover_with_config("en", &Default::default());
        let core = SearchCore {
            providers: ProviderRegistry::default_providers(plugins, Vec::new()),
            visible_results: Mutex::new(Arc::new(Vec::new())),
            ranker: Ranker::in_memory(),
            fuzzy_search: AtomicBool::new(true),
            separate_launch_entries: AtomicBool::new(false),
            show_advanced_launch_entries: AtomicBool::new(false),
            category_order: Mutex::new(HashMap::new()),
            plugin_order: Mutex::new(HashMap::new()),
        };
        let session = core
            .search_context("> ")
            .session()
            .cloned()
            .expect("command session");
        core.record_action_success("older", "ipconfig", Some(&session));
        core.record_action_success("newer", "whoami", Some(&session));

        let results = core.search_in_context(&crate::page::SearchContext::Engine(session), "");

        assert_eq!(results.len(), 2);
        assert!(results[0].launch_arguments.contains("whoami"));
        assert!(results[1].launch_arguments.contains("ipconfig"));
    }

    #[test]
    fn older_session_favorites_replace_a_colliding_root_entry_snapshot() {
        let mut root_entry = result("legacy-shortcut", "Run command", ResultCategory::Other);
        root_entry.visibility = ResultVisibility::EmptyQueryOnly;
        let mut generated = result(
            "legacy-shortcut",
            "Run command \"ipconfig\"",
            ResultCategory::Other,
        );
        generated
            .actions
            .push(crate::result::Action::Launch(crate::result::LaunchEntry {
                source: "test".into(),
                path: "ipconfig".into(),
                target: "ipconfig".into(),
                arguments: String::new(),
                working_directory: None,
            }));
        let session = crate::page::SearchEngineSession {
            id: "legacy-shortcut".into(),
            title: "Run command".into(),
            placeholder: "Enter a command".into(),
            prefix: "> ".into(),
            argument: "ipconfig".into(),
            mode: crate::page::SearchEngineMode::Execute,
            state: Default::default(),
        };
        let core = SearchCore::new(vec![root_entry]);
        core.toggle_favorite(&generated, Some(&session));

        let results = core.search("");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Run command \"ipconfig\"");
        assert!(matches!(
            results[0].primary_action(),
            Some(crate::result::Action::Launch(_))
        ));
    }
    #[test]
    fn fuzzy_search_matches_typo_by_default() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "Notepad".into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: "".into(),
            launch_target: "notepad.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
            SearchCore::new(vec![item]).search("ntpd")[0].title,
            "Notepad"
        );
    }

    #[test]
    fn fuzzy_search_can_be_disabled() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "Notepad".into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: "".into(),
            launch_target: "notepad.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        let core = SearchCore::new(vec![item]);
        core.replace_fuzzy_search(false);
        assert!(core.search("ntpd").is_empty());
    }

    #[test]
    fn exact_match_is_ranked_before_fuzzy_match() {
        let make = |id: &str, title: &str| SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: "".into(),
            launch_target: title.into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        let core = SearchCore::new(vec![make("fuzzy", "Notepad"), make("exact", "ntpd")]);
        assert_eq!(core.search("ntpd")[0].id, "exact");
    }

    #[test]
    fn searches_aliases() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "A very long display name".into(),
            category: ResultCategory::Application,
            aliases: vec!["calc".into()],
            location: "".into(),
            launch_target: "calculator.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        assert_eq!(SearchCore::new(vec![item]).search("calc").len(), 1);
    }

    #[test]
    fn steam_source_alias_is_lower_priority_than_exact_application_title() {
        let make = |id: &str, title: &str, source: &str, aliases: Vec<&str>| -> SearchResult {
            SearchResult {
                id: id.into(),
                identity: id.into(),
                title: title.into(),
                category: ResultCategory::Application,
                aliases: aliases.into_iter().map(str::to_owned).collect(),
                location: String::new(),
                launch_target: title.into(),
                launch_arguments: String::new(),
                launch_working_directory: None,
                source: source.into(),
                sources: vec![source.into()],
                launch_entries: vec![],
                actions: vec![],
                detail: None,
                description: None,
                app_user_model_id: None,
                package_family_name: None,
                package_metadata: None,
                icon: None,
                ranking_bias: 0,
                allow_favorite: true,
                visibility: ResultVisibility::Always,
            }
        };
        let mut game = make("game", "Portal 2", "Steam", vec!["steam"]);
        game.ranking_bias = -600;
        let core = SearchCore::new(vec![game, make("steam", "Steam", "AppsFolder", vec![])]);
        let results = core.search("steam");
        assert_eq!(results[0].title, "Steam");
        assert_eq!(results[1].title, "Portal 2");
    }

    #[test]
    fn alias_score_is_not_hidden_by_a_lower_title_score() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "A long calculator display name".into(),
            category: ResultCategory::Application,
            aliases: vec!["calc".into()],
            location: "".into(),
            launch_target: "calculator.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        assert_eq!(SearchCore::new(vec![item]).search("calc").len(), 1);
    }

    #[test]
    fn unrelated_chinese_title_does_not_match_pinyin_query() {
        let item = SearchResult {
            id: "settings".into(),
            identity: "settings".into(),
            title: "设置".into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: "".into(),
            launch_target: "settings.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        assert!(SearchCore::new(vec![item]).search("weixin").is_empty());
    }

    #[test]
    fn contains_match_is_ranked_before_longer_contains_match() {
        let make = |id: &str, title: &str| SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: String::new(),
            launch_target: title.into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        let core = SearchCore::new(vec![
            make("long", "My Calculator Tool"),
            make("short", "Calculator"),
        ]);
        assert_eq!(core.search("calc")[0].id, "short");
    }

    #[test]
    fn successful_use_breaks_a_matching_score_tie() {
        let make = |id: &str, title: &str| SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: String::new(),
            launch_target: title.into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        let core = SearchCore::new(vec![make("a", "Calculator A"), make("b", "Calculator B")]);
        core.record_success("b", "calculator");
        assert_eq!(core.search("calculator")[0].id, "b");
    }

    #[test]
    fn behavior_does_not_cross_match_classes() {
        let make = |id: &str, title: &str| SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: String::new(),
            launch_target: title.into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        let core = SearchCore::new(vec![
            make("exact", "calc"),
            make("contains", "my calculator"),
        ]);
        for _ in 0..20 {
            core.record_success("contains", "calc");
        }
        assert_eq!(core.search("calc")[0].id, "exact");
    }

    #[test]
    fn unprefixed_custom_command_results_remain_fallbacks() {
        let notepad = SearchResult {
            id: "aumid:notepad".into(),
            identity: "aumid:notepad".into(),
            title: "记事本".into(),
            category: ResultCategory::Application,
            aliases: vec!["Microsoft WindowsNotepad".into()],
            location: String::new(),
            launch_target: "shell:AppsFolder\\Microsoft.WindowsNotepad!App".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "AppsFolder".into(),
            sources: vec!["AppsFolder".into()],
            launch_entries: vec![],
            actions: vec![],
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
        let core = SearchCore::with_plugins_and_shortcuts(
            vec![notepad],
            PluginManager::discover_with_config("en", &std::collections::BTreeMap::new()),
            Vec::new(),
        );
        let found = core.search("notepad");
        assert_eq!(found[0].title, "记事本");
        assert!(found.iter().any(|item| item.id.starts_with("shortcut:")));
    }

    #[test]
    fn ordinary_exact_match_outranks_a_command_result_name() {
        let ordinary = SearchResult {
            id: "ordinary:command".into(),
            identity: "ordinary:command".into(),
            title: "Execute Command".into(),
            category: ResultCategory::Application,
            aliases: Vec::new(),
            location: String::new(),
            launch_target: "command.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: Vec::new(),
            actions: Vec::new(),
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
        let core = SearchCore::with_plugins_and_shortcuts(
            vec![ordinary],
            PluginManager::discover_with_config("en", &std::collections::BTreeMap::new()),
            Vec::new(),
        );
        core.replace_provider_language("en".into());
        let found = core.search("Execute Command");
        assert_eq!(found[0].id, "ordinary:command");
    }

    #[test]
    fn bare_command_marker_keeps_custom_result_previews_visible() {
        let core = SearchCore::with_plugins_and_shortcuts(
            Vec::new(),
            PluginManager::discover_with_config("en", &std::collections::BTreeMap::new()),
            crate::config::Config::default().shortcuts,
        );
        core.replace_provider_language("en".into());
        let found = core.search(">");
        assert!(found
            .iter()
            .any(|item| item.title.starts_with("Execute Command")));
        assert!(found
            .iter()
            .any(|item| item.title.starts_with("Search with Google")));
    }

    #[test]
    fn explicit_shortcut_prefix_is_available_before_an_argument() {
        let core = SearchCore::with_plugins_and_shortcuts(
            Vec::new(),
            PluginManager::discover_with_config("en", &std::collections::BTreeMap::new()),
            Vec::new(),
        );
        core.replace_provider_language("en".into());
        let found = core.search("> ");
        assert_eq!(found[0].title, "Execute Command");
        assert!(found[0].actions.is_empty());
    }

    #[test]
    fn search_returns_all_matches_without_a_limit() {
        let first = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "Calculator".into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: String::new(),
            launch_target: "calculator.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
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
        let second = SearchResult {
            id: "2".into(),
            identity: "2".into(),
            title: "Calculator Settings".into(),
            ..first.clone()
        };
        assert_eq!(SearchCore::new(vec![first, second]).search("calc").len(), 2);
    }

    #[test]
    fn generated_shortcuts_can_be_resolved_for_launch() {
        let core = SearchCore::with_plugins_and_shortcuts(
            Vec::new(),
            PluginManager::discover_with_config("en", &std::collections::BTreeMap::new()),
            Vec::new(),
        );
        let found = core.search("> ipconfig");
        let item = core.by_id(&found[0].id).expect("shortcut result by id");
        assert!(item.title.ends_with("\"ipconfig\""));
    }
}
