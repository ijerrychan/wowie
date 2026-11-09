use crate::plugin::PluginManager;
use crate::provider::{EngineSearchResult, MatchingPolicy, ProviderRegistry, SearchEngineSettings};
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

    pub fn cancel_active_queries(&self) {
        self.providers.cancel_active_queries();
    }

    /// Generation of provider-owned dynamic content snapshots.
    pub fn provider_content_generation(&self) -> u64 {
        self.providers.content_generation()
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

    /// Index of the first candidate with strong lexical evidence for `query`.
    ///
    /// Best match is deliberately stricter than ordinary matching: exact,
    /// prefix and word-start evidence qualify, while a lone character buried
    /// inside a word or a scattered fuzzy match does not. Fallback rows and
    /// command/session entries never qualify. Weak matches are not removed
    /// from the result list; this only decides whether any row may headline it.
    pub fn best_match_index(&self, query: &str, items: &[SearchResult]) -> Option<usize> {
        let query = crate::ranking::normalize_query(query);
        if query.is_empty() {
            return None;
        }
        let fuzzy_search = self.fuzzy_search.load(Ordering::Acquire);
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let mut matcher_buffer = Vec::new();
        let mut needle_buffer = Vec::new();
        let mut lowercase_buffer = String::new();
        let mut fuzzy_indices_buffer = Vec::new();
        let common_keywords = common_keywords(items.iter());
        for (index, item) in items.iter().enumerate() {
            if self.providers.is_fallback_result(item)
                || item.effective_category() == crate::result::ResultCategory::Command
            {
                continue;
            }
            if result_match_score(
                &query,
                &item.title,
                &item.aliases,
                &item.metadata,
                &common_keywords,
                fuzzy_search,
                true,
                &mut matcher,
                &mut matcher_buffer,
                &mut needle_buffer,
                &mut lowercase_buffer,
                &mut fuzzy_indices_buffer,
            )
            .is_some_and(|score| score.strong)
            {
                return Some(index);
            }
        }
        None
    }

    pub fn is_favorite(&self, item_id: &str) -> bool {
        self.ranker.is_favorite(item_id)
    }

    pub fn is_fallback_result(&self, item: &SearchResult) -> bool {
        self.providers.is_fallback_result(item)
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

    pub fn replace_file_search_preferences(&self, backend: &str, enabled: bool, keyword: &str) {
        self.providers
            .replace_file_search_preferences(backend, enabled, keyword);
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

    pub fn flush_history(&self) {
        self.ranker.flush();
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
        let results = self.providers.query_more(context, query, offset);
        if self.providers.settings_for_context(context).matching_policy
            == MatchingPolicy::ProviderOwned
            && !query.trim().is_empty()
        {
            return results
                .into_iter()
                .filter(|item| item.visibility.allows(query))
                .collect();
        }
        results
    }

    /// Search a context resolved by the caller. UI code should resolve once
    /// and reuse this value for rendering, navigation and result retrieval.
    pub fn search_in_context(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
    ) -> Vec<SearchResult> {
        self.search_in_context_with_limit(context, query, None)
    }

    /// Search a context while retaining only the best `limit` results.
    ///
    /// The launcher uses this for its bounded result surface. Matching and
    /// ranking still consider every candidate so this returns the same prefix
    /// as an unrestricted search, without sorting or cloning every match.
    pub fn search_in_context_limited(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
        limit: usize,
    ) -> Vec<SearchResult> {
        self.search_in_context_with_limit(context, query, Some(limit))
    }

    fn search_in_context_with_limit(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
        result_limit: Option<usize>,
    ) -> Vec<SearchResult> {
        let input = query.to_owned();
        // An active engine receives the text currently shown in the input
        // field. The UI stores the selected engine separately, so the field
        // no longer needs to contain or be parsed for its shortcut prefix.
        let query = crate::ranking::normalize_query(&input);
        let root_empty_query =
            matches!(context, crate::page::SearchContext::Default) && query.is_empty();
        let favorites = self.ranker.favorite_snapshot(root_empty_query);
        let fuzzy_search = self.fuzzy_search.load(Ordering::Acquire);
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let mut matcher_buffer = Vec::new();
        let mut needle_buffer = Vec::new();
        let mut lowercase_buffer = String::new();
        let mut fuzzy_indices_buffer = Vec::new();
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
        let provider_query =
            if matches!(context, crate::page::SearchContext::Default) && query.is_empty() {
                ""
            } else {
                input.as_str()
            };
        let visibility_query = if matches!(context, crate::page::SearchContext::Default) {
            query.as_str()
        } else {
            input.as_str()
        };
        let mut dynamic = self
            .providers
            .query(
                context,
                provider_query,
                history.as_deref().unwrap_or_default(),
            )
            .into_iter()
            .filter(|candidate| {
                candidate.item.visibility.allows(visibility_query)
                    && (!matches!(context, crate::page::SearchContext::Default)
                        || !query.is_empty()
                        || candidate.item.visibility == ResultVisibility::EmptyQueryOnly)
            })
            .collect::<Vec<_>>();
        if !query.is_empty()
            && matches!(context, crate::page::SearchContext::Engine(_))
            && self.providers.settings_for_context(context).matching_policy
                == MatchingPolicy::ProviderOwned
        {
            let mut results = dynamic
                .into_iter()
                .map(|candidate| candidate.item)
                .collect::<Vec<_>>();
            if let Some(limit) = result_limit {
                results.truncate(limit);
            }
            return results;
        }
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
                    item.icon = entry.icon.clone();
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
                    if favorite_requires_current_app_index(&favorite.item) {
                        continue;
                    }
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
        let common_keywords = common_keywords(dynamic.iter().map(|candidate| &candidate.item));
        let mut ranked: Vec<RankedResult<'_>> = dynamic.iter().enumerate().filter_map(|(candidate_index, candidate)| {
                let item = &candidate.item;
                // Every provider contributes ordinary SearchResults. The
                // central matcher is the single place that decides whether a
                // result is visible and how strongly it matches.
                let engine_fuzzy_search = candidate.settings.respect_global_fuzzy_search && fuzzy_search;
                let score = result_match_score(
                    &query,
                    &item.title,
                    &item.aliases,
                    &item.metadata,
                    &common_keywords,
                    engine_fuzzy_search,
                    false,
                    &mut matcher,
                    &mut matcher_buffer,
                    &mut needle_buffer,
                    &mut lowercase_buffer,
                    &mut fuzzy_indices_buffer,
                );
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
                    Some(MatchScore {
                        class: 0,
                        role: EvidenceRole::Auxiliary,
                        quality: 0,
                        strong: false,
                    })
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
                            evidence_role: score.role,
                            match_quality: score.quality,
                            composite_score: score.quality,
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
                            engine_bias: candidate.settings.ranking_bias.clamp(-100, 100),
                            provider_bias: item.ranking_bias,
                        },
                        item,
                        candidate_index,
                    )
                })
            })
            .collect();
        let behavior_scores = self
            .ranker
            .scores(ranked.iter().map(|(_, item, _)| item.id.as_str()), &query);
        for ((key, _, _), behavior) in ranked.iter_mut().zip(behavior_scores) {
            key.composite_score = key.match_quality.saturating_add(behavior.clamp(0, 100));
        }
        if let Some(limit) = result_limit {
            if ranked.len() > limit {
                ranked.select_nth_unstable_by(limit, compare_ranked_results);
                ranked.truncate(limit);
            }
        }
        ranked.sort_by(compare_ranked_results);
        let found: Vec<SearchResult> = ranked
            .into_iter()
            .map(|(_, item, _)| item.clone())
            .collect();
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

/// Match classes are ordered from strongest to weakest evidence. They are
/// intentionally separate from the fuzzy score: a class describes *why* the
/// candidate matched, while `quality` only breaks ties inside that class.
const MATCH_CLASS_EMPTY: u8 = 0;
const MATCH_CLASS_FUZZY: u8 = 1;
const MATCH_CLASS_CONTAINS: u8 = 2;
const MATCH_CLASS_WORD_START: u8 = 3;
const MATCH_CLASS_PREFIX: u8 = 4;
const MATCH_CLASS_EXACT: u8 = 5;
const TITLE_MATCH_BONUS: i32 = 16;
const UNORDERED_MULTIWORD_PENALTY: i32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MatchScore {
    class: u8,
    role: EvidenceRole,
    quality: i32,
    /// Whether the evidence is strong enough to headline the result list.
    /// This never removes a result; it only gates the best-match slot.
    strong: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EvidenceRole {
    Auxiliary,
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RankKey {
    history_order: Option<usize>,
    favorite_order: Option<usize>,
    fallback: bool,
    match_class: u8,
    evidence_role: EvidenceRole,
    match_quality: i32,
    composite_score: i32,
    category_order: usize,
    plugin_order: usize,
    engine_bias: i32,
    provider_bias: i32,
}

type RankedResult<'a> = (RankKey, &'a SearchResult, usize);

fn compare_ranked_results(left: &RankedResult<'_>, right: &RankedResult<'_>) -> std::cmp::Ordering {
    left.0
        .compare(&right.0)
        .then_with(|| left.1.title.cmp(&right.1.title))
        .then_with(|| left.1.id.cmp(&right.1.id))
        // The previous stable sort preserved provider order for otherwise
        // identical rows. Keep that tie-breaker when partial selection reorders
        // the full candidate set.
        .then_with(|| left.2.cmp(&right.2))
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
                Reverse(value.evidence_role),
                Reverse(value.match_class),
                Reverse(value.composite_score),
                value.fallback,
                value.category_order,
                value.plugin_order,
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
        ResultCategory::Bookmark => "bookmark",
        ResultCategory::Window => "window",
        ResultCategory::Command => "command",
        ResultCategory::Calculator => "calculator",
        ResultCategory::ControlPanel => "control_panel",
        ResultCategory::Settings => "settings",
        ResultCategory::Document => "document",
        ResultCategory::Other => "other",
    }
}

fn favorite_requires_current_app_index(item: &SearchResult) -> bool {
    fn is_indexed_application_source(source: &crate::platform::SourceId) -> bool {
        matches!(
            source,
            crate::platform::SourceId::AppsFolder
                | crate::platform::SourceId::Package
                | crate::platform::SourceId::StartMenu
                | crate::platform::SourceId::Desktop
                | crate::platform::SourceId::ControlPanel
                | crate::platform::SourceId::CustomDirectories
        )
    }

    is_indexed_application_source(&item.source)
        || item.sources.iter().any(is_indexed_application_source)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Reuse caller-owned matching buffers without allocating in the search hot path."
)]
fn result_match_score(
    query: &str,
    title: &str,
    aliases: &[String],
    metadata: &crate::result::SearchMetadata,
    common_keywords: &std::collections::HashSet<String>,
    fuzzy_search: bool,
    evaluate_evidence: bool,
    matcher: &mut Matcher,
    matcher_buffer: &mut Vec<char>,
    needle_buffer: &mut Vec<char>,
    lowercase_buffer: &mut String,
    fuzzy_indices_buffer: &mut Vec<u32>,
) -> Option<MatchScore> {
    let title_score = match_score(
        query,
        title,
        fuzzy_search,
        evaluate_evidence,
        matcher,
        matcher_buffer,
        needle_buffer,
        lowercase_buffer,
        fuzzy_indices_buffer,
    )
    .map(|mut score| {
        score.quality = score.quality.saturating_add(TITLE_MATCH_BONUS);
        score
    });
    let alias_score = aliases
        .iter()
        .filter_map(|alias| {
            match_score(
                query,
                alias,
                fuzzy_search,
                evaluate_evidence,
                matcher,
                matcher_buffer,
                needle_buffer,
                lowercase_buffer,
                fuzzy_indices_buffer,
            )
        })
        .max_by_key(|score| (score.class, score.quality));
    let keyword_score = metadata
        .keywords
        .iter()
        .filter_map(|keyword| {
            let normalized = crate::ranking::normalize_query(keyword);
            if is_numeric_token(query)
                || !keyword_query_evidence(query, &normalized)
                || !has_keyword_anchor(query, common_keywords)
            {
                return None;
            }
            Some(MatchScore {
                class: MATCH_CLASS_FUZZY,
                role: EvidenceRole::Auxiliary,
                quality: -1000,
                strong: false,
            })
        })
        .next();
    let technical_score = metadata
        .technical
        .iter()
        .any(|value| crate::ranking::normalize_query(value) == query)
        .then_some(MatchScore {
            class: MATCH_CLASS_FUZZY,
            role: EvidenceRole::Auxiliary,
            quality: -1500,
            strong: false,
        });
    let multiword_score = unordered_multiword_score(
        query,
        title,
        aliases,
        metadata,
        common_keywords,
        evaluate_evidence,
        matcher,
        matcher_buffer,
        needle_buffer,
        lowercase_buffer,
        fuzzy_indices_buffer,
    );
    [
        title_score,
        alias_score,
        keyword_score,
        technical_score,
        multiword_score,
    ]
    .into_iter()
    .flatten()
    .max_by_key(|score| (score.role, score.class, score.quality))
}

fn keyword_query_evidence(query: &str, keyword: &str) -> bool {
    if query == keyword {
        return true;
    }
    let query_tokens = query.split_whitespace().collect::<Vec<_>>();
    let keyword_tokens = keyword.split_whitespace().collect::<Vec<_>>();
    if query_tokens.is_empty() || keyword_tokens.is_empty() {
        return false;
    }
    if query_tokens.len() == 1 {
        return keyword_tokens.contains(&query_tokens[0])
            || (cjk_char_count(query) >= 2 && keyword.contains(query));
    }
    keyword_tokens
        .windows(query_tokens.len())
        .any(|window| window == query_tokens)
}

fn has_keyword_anchor(query: &str, common_keywords: &std::collections::HashSet<String>) -> bool {
    if query
        .split_whitespace()
        .any(|token| !is_numeric_token(token) && !common_keywords.contains(token))
    {
        return true;
    }
    !query.chars().any(char::is_whitespace)
        && cjk_char_count(query) >= 2
        && !common_keywords.contains(query)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Reuse caller-owned matching buffers without allocating in the search hot path."
)]
fn unordered_multiword_score(
    query: &str,
    title: &str,
    aliases: &[String],
    metadata: &crate::result::SearchMetadata,
    common_keywords: &std::collections::HashSet<String>,
    evaluate_evidence: bool,
    matcher: &mut Matcher,
    matcher_buffer: &mut Vec<char>,
    needle_buffer: &mut Vec<char>,
    lowercase_buffer: &mut String,
    fuzzy_indices_buffer: &mut Vec<u32>,
) -> Option<MatchScore> {
    let mut tokens = query.split_whitespace();
    let first = tokens.next()?;
    tokens.clone().next()?;
    let mut weakest_class = u8::MAX;
    let mut weakest_quality = i32::MAX;
    let mut all_strong = true;
    let mut all_tokens_in_title = true;
    let mut has_name_anchor = false;
    let mut has_rare_keyword_anchor = false;
    let mut all_tokens_name = true;
    for token in std::iter::once(first).chain(tokens) {
        let title_score = match_score(
            token,
            title,
            false,
            evaluate_evidence,
            matcher,
            matcher_buffer,
            needle_buffer,
            lowercase_buffer,
            fuzzy_indices_buffer,
        );
        all_tokens_in_title &= title_score.is_some();
        has_name_anchor |= title_score.is_some_and(|score| score.strong);
        let mut token_score = title_score;
        for field in aliases.iter().map(String::as_str) {
            if let Some(score) = match_score(
                token,
                field,
                false,
                evaluate_evidence,
                matcher,
                matcher_buffer,
                needle_buffer,
                lowercase_buffer,
                fuzzy_indices_buffer,
            ) {
                if token_score.is_none_or(|best: MatchScore| {
                    (score.class, score.quality) > (best.class, best.quality)
                }) {
                    token_score = Some(score);
                }
            }
        }
        let name_score = token_score;
        all_tokens_name &= name_score.is_some();
        has_name_anchor |= name_score.is_some_and(|score| score.strong);
        let mut keyword_match = false;
        for keyword in &metadata.keywords {
            let normalized = crate::ranking::normalize_query(keyword);
            if normalized.split_whitespace().any(|word| word == token) {
                keyword_match = true;
                if !is_numeric_token(token) && !common_keywords.contains(token) {
                    has_rare_keyword_anchor = true;
                }
            }
        }
        if token_score.is_none() && keyword_match {
            token_score = Some(MatchScore {
                class: MATCH_CLASS_FUZZY,
                role: EvidenceRole::Auxiliary,
                quality: -1000,
                strong: false,
            });
        }
        if token_score.is_none()
            && metadata.context.iter().any(|field| {
                crate::ranking::normalize_query(field)
                    .split_whitespace()
                    .any(|word| word == token)
            })
        {
            token_score = Some(MatchScore {
                class: MATCH_CLASS_FUZZY,
                role: EvidenceRole::Auxiliary,
                quality: -1500,
                strong: false,
            });
        }
        let score = token_score?;
        weakest_class = weakest_class.min(score.class);
        weakest_quality = weakest_quality.min(score.quality);
        all_strong &= score.strong;
    }
    if !has_name_anchor && !has_rare_keyword_anchor {
        return None;
    }
    Some(MatchScore {
        // Unordered token coverage remains below exact, prefix and word-start
        // phrase evidence, even if each token is an exact alias by itself.
        class: weakest_class.min(if has_name_anchor {
            MATCH_CLASS_CONTAINS
        } else {
            MATCH_CLASS_FUZZY
        }),
        role: if all_tokens_name {
            EvidenceRole::Name
        } else {
            EvidenceRole::Auxiliary
        },
        quality: weakest_quality
            .saturating_sub(UNORDERED_MULTIWORD_PENALTY)
            .saturating_add(if all_tokens_in_title {
                TITLE_MATCH_BONUS
            } else {
                0
            }),
        strong: all_strong && weakest_class >= MATCH_CLASS_WORD_START && has_name_anchor,
    })
}

fn common_keywords<'a>(
    items: impl Iterator<Item = &'a SearchResult>,
) -> std::collections::HashSet<String> {
    let mut documents = 0usize;
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for item in items {
        let mut terms = std::collections::HashSet::new();
        for keyword in &item.metadata.keywords {
            let normalized = crate::ranking::normalize_query(keyword);
            if !normalized.is_empty() && !is_numeric_token(&normalized) {
                terms.insert(normalized.clone());
            }
            for term in normalized.split_whitespace() {
                if !is_numeric_token(term) {
                    terms.insert(term.to_owned());
                }
            }
        }
        if !terms.is_empty() {
            documents += 1;
            for term in terms {
                *counts.entry(term).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .filter_map(|(term, count)| {
            (count >= 3 && count.saturating_mul(10) >= documents).then_some(term)
        })
        .collect()
}

fn is_numeric_token(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|character| character.is_ascii_digit())
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
    if let Some(icon) = &entry.icon {
        for byte in format!("{icon:?}").bytes().chain(std::iter::once(0xff)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

#[expect(
    clippy::too_many_arguments,
    reason = "Reuse caller-owned matching buffers without allocating in the search hot path."
)]
fn match_score(
    query: &str,
    title: &str,
    fuzzy_search: bool,
    evaluate_evidence: bool,
    matcher: &mut Matcher,
    matcher_buffer: &mut Vec<char>,
    needle_buffer: &mut Vec<char>,
    lowercase_buffer: &mut String,
    fuzzy_indices_buffer: &mut Vec<u32>,
) -> Option<MatchScore> {
    if query.is_empty() {
        return Some(MatchScore {
            class: MATCH_CLASS_EMPTY,
            role: EvidenceRole::Name,
            quality: 0,
            strong: false,
        });
    }
    let query_chars = query.chars().count();
    if query.is_ascii() && ascii_text_is_canonical(title) {
        if ascii_eq_ignore_case(title, query) {
            return Some(MatchScore {
                class: MATCH_CLASS_EXACT,
                role: EvidenceRole::Name,
                quality: 1000,
                strong: true,
            });
        }
        if ascii_starts_with_ignore_case(title, query) {
            return Some(MatchScore {
                class: MATCH_CLASS_PREFIX,
                role: EvidenceRole::Name,
                quality: title_quality(title),
                strong: true,
            });
        }
        if let Some((_, at_word_start)) = ascii_contains_position(title, query) {
            return Some(MatchScore {
                class: if at_word_start {
                    MATCH_CLASS_WORD_START
                } else {
                    MATCH_CLASS_CONTAINS
                },
                role: EvidenceRole::Name,
                quality: title_quality(title),
                // A single character buried at a word start is still weak
                // evidence; leading characters are covered by prefix matches.
                strong: at_word_start && query_chars >= 2,
            });
        }
        if !fuzzy_search {
            return None;
        }
        // The matcher is case-insensitive, so the original ASCII title can be
        // used directly without building a lowercase copy.
        return fuzzy_match_score(
            title,
            query,
            query_chars,
            evaluate_evidence,
            matcher,
            matcher_buffer,
            needle_buffer,
            fuzzy_indices_buffer,
        );
    }

    crate::ranking::normalize_query_into(title, lowercase_buffer);
    let text = lowercase_buffer.as_str();
    if text == query {
        return Some(MatchScore {
            class: MATCH_CLASS_EXACT,
            role: EvidenceRole::Name,
            quality: 1000,
            strong: true,
        });
    }
    if text.starts_with(query) {
        return Some(MatchScore {
            class: MATCH_CLASS_PREFIX,
            role: EvidenceRole::Name,
            quality: title_quality(text),
            strong: true,
        });
    }
    // Scripts without spaces (CJK, kana, Hangul) do not expose word
    // boundaries to a substring check. A phrase of at least two such
    // characters is meaningful evidence in the same way a word start is.
    let wide_phrase = cjk_char_count(query) >= 2;
    if let Some((_, at_word_start)) = unicode_contains_position(text, query) {
        return Some(MatchScore {
            class: if at_word_start || wide_phrase {
                MATCH_CLASS_WORD_START
            } else {
                MATCH_CLASS_CONTAINS
            },
            role: EvidenceRole::Name,
            quality: title_quality(text),
            strong: wide_phrase || (at_word_start && query_chars >= 2),
        });
    }
    if !fuzzy_search {
        return None;
    }
    fuzzy_match_score(
        text,
        query,
        query_chars,
        evaluate_evidence,
        matcher,
        matcher_buffer,
        needle_buffer,
        fuzzy_indices_buffer,
    )
}

fn ascii_text_is_canonical(text: &str) -> bool {
    text.is_ascii()
        && text.trim() == text
        && !text.as_bytes().windows(2).any(|pair| pair == b"  ")
        && !text
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() && byte != b' ')
}

fn title_quality(text: &str) -> i32 {
    (1000 - text.chars().count() as i32).max(1)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Reuse caller-owned matching buffers without allocating in the search hot path."
)]
fn fuzzy_match_score(
    text: &str,
    query: &str,
    query_chars: usize,
    evaluate_evidence: bool,
    matcher: &mut Matcher,
    matcher_buffer: &mut Vec<char>,
    needle_buffer: &mut Vec<char>,
    indices_buffer: &mut Vec<u32>,
) -> Option<MatchScore> {
    let haystack = Utf32Str::new(text, matcher_buffer);
    let needle = Utf32Str::new(query, needle_buffer);
    if !evaluate_evidence {
        // Ranking only needs the class. Avoid the index-tracking matcher,
        // which is more expensive, unless best-match evidence is requested.
        let score = matcher.fuzzy_match(haystack, needle)?;
        return Some(MatchScore {
            class: MATCH_CLASS_FUZZY,
            role: EvidenceRole::Name,
            quality: score as i32,
            strong: false,
        });
    }
    indices_buffer.clear();
    let score = matcher.fuzzy_indices(haystack, needle, indices_buffer)?;
    Some(MatchScore {
        class: MATCH_CLASS_FUZZY,
        role: EvidenceRole::Name,
        quality: score as i32,
        // A scattered subsequence is the weakest possible evidence. Require a
        // word acronym or an almost-contiguous single-word typo.
        strong: query_chars >= 3 && fuzzy_match_is_strong(text, indices_buffer),
    })
}

/// `indices` are ascending character positions returned by `fuzzy_indices`.
fn fuzzy_match_is_strong(text: &str, indices: &[u32]) -> bool {
    if indices.is_empty() {
        return false;
    }
    let first = indices[0] as usize;
    let last = indices[indices.len() - 1] as usize;
    let mut acronym = true;
    let mut previous_character = None;
    let mut next = 0usize;
    for (position, character) in text.chars().enumerate() {
        while next < indices.len() && indices[next] as usize == position {
            if previous_character.is_some_and(char::is_alphanumeric) {
                acronym = false;
            }
            next += 1;
        }
        if next >= indices.len() {
            break;
        }
        previous_character = Some(character);
    }
    if next < indices.len() {
        return false;
    }
    if acronym {
        return true;
    }
    // Accept a typo-level match that stays inside one word while skipping at
    // most one character. Counting skipped characters (not gap runs) prevents
    // a long span such as "aXXXXXXXXbc" from posing as a typo.
    let span = last.saturating_sub(first) + 1;
    let skipped = span.saturating_sub(indices.len());
    skipped <= 1
        && text
            .chars()
            .skip(first)
            .take(span)
            .all(|character| character.is_alphanumeric())
}

fn ascii_eq_ignore_case(left: &str, right: &str) -> bool {
    ascii_bytes_eq_ignore_case(left.as_bytes(), right.as_bytes())
}

fn ascii_starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && ascii_bytes_eq_ignore_case(&text.as_bytes()[..prefix.len()], prefix.as_bytes())
}

/// Locate the first occurrence of `needle`, preferring a word-start
/// occurrence so `Reset settings` still provides word-start evidence for
/// "set". Returns the byte offset and whether the chosen occurrence starts a
/// word.
fn ascii_contains_position(text: &str, needle: &str) -> Option<(usize, bool)> {
    let haystack = text.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let mut first = None;
    for index in 0..=haystack.len() - needle.len() {
        if ascii_bytes_eq_ignore_case(&haystack[index..index + needle.len()], needle) {
            if first.is_none() {
                first = Some(index);
            }
            if index == 0 || !haystack[index - 1].is_ascii_alphanumeric() {
                return Some((index, true));
            }
        }
    }
    first.map(|index| (index, false))
}

/// Character-level counterpart of [`ascii_contains_position`].
fn unicode_contains_position(text: &str, needle: &str) -> Option<(usize, bool)> {
    let mut first = None;
    for (index, _) in text.match_indices(needle) {
        if first.is_none() {
            first = Some(index);
        }
        let at_word_start = index == 0
            || text[..index]
                .chars()
                .next_back()
                .is_some_and(|previous| !previous.is_alphanumeric());
        if at_word_start {
            return Some((index, true));
        }
    }
    first.map(|index| (index, false))
}

fn cjk_char_count(text: &str) -> usize {
    text.chars().filter(|character| is_cjk(*character)).count()
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3040..=0x30FF     // Hiragana and Katakana
            | 0x3400..=0x4DBF // CJK Extension A
            | 0x4E00..=0x9FFF // CJK Unified Ideographs
            | 0xAC00..=0xD7AF // Hangul syllables
            | 0xF900..=0xFAFF // CJK Compatibility Ideographs
            | 0x20000..=0x2FA1F // CJK Extensions B-F and compatibility supplement
    )
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

    struct NativeQueryEngine {
        session: crate::page::SearchEngineSession,
        first_page: Vec<SearchResult>,
        second_page: Vec<SearchResult>,
        policy: MatchingPolicy,
        received_queries: Option<Arc<Mutex<Vec<String>>>>,
    }

    impl crate::provider::SearchEngineProvider for NativeQueryEngine {
        fn settings(&self) -> SearchEngineSettings {
            SearchEngineSettings {
                matching_policy: self.policy,
                ..SearchEngineSettings::default()
            }
        }

        fn search_engine(&self, _query: &str) -> Option<crate::page::SearchEngineSession> {
            Some(self.session.clone())
        }

        fn query_root(&self, query: &str) -> Vec<SearchResult> {
            if let Some(received) = &self.received_queries {
                received.lock().unwrap().push(query.to_owned());
            }
            self.first_page.clone()
        }

        fn query_active(
            &self,
            _session: &crate::page::SearchEngineSession,
            query: &str,
        ) -> Vec<SearchResult> {
            if let Some(received) = &self.received_queries {
                received.lock().unwrap().push(query.to_owned());
            }
            self.first_page.clone()
        }

        fn query_active_page(
            &self,
            _session: &crate::page::SearchEngineSession,
            query: &str,
            offset: usize,
        ) -> Vec<SearchResult> {
            if let Some(received) = &self.received_queries {
                received.lock().unwrap().push(query.to_owned());
            }
            if offset == self.first_page.len() {
                self.second_page.clone()
            } else {
                Vec::new()
            }
        }

        fn owns_session(&self, session: &crate::page::SearchEngineSession) -> bool {
            session.id == self.session.id
        }
    }

    fn native_query_session() -> crate::page::SearchEngineSession {
        crate::page::SearchEngineSession {
            id: "test:native-files".into(),
            title: "Native files".into(),
            placeholder: "Everything query".into(),
            prefix: "e ".into(),
            argument: String::new(),
            mode: crate::page::SearchEngineMode::Panel,
            state: Default::default(),
        }
    }

    fn result(id: &str, title: &str, category: ResultCategory) -> SearchResult {
        SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            category,
            metadata: Default::default(),
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

    #[derive(serde::Deserialize)]
    struct SearchEvaluationFixture {
        candidates: Vec<SearchEvaluationCandidate>,
        queries: Vec<SearchEvaluationQuery>,
    }

    #[derive(serde::Deserialize)]
    struct SearchEvaluationCandidate {
        id: String,
        title: String,
        category: String,
        #[serde(default)]
        aliases: Vec<String>,
        #[serde(default)]
        keywords: Vec<String>,
        #[serde(default)]
        location: String,
        #[serde(default)]
        description: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct SearchEvaluationQuery {
        query: String,
        relevant: Vec<String>,
        #[serde(default)]
        expected_best: Option<String>,
        #[serde(default)]
        check_best: bool,
    }

    fn evaluation_category(value: &str) -> ResultCategory {
        match value {
            "Application" => ResultCategory::Application,
            "Game" => ResultCategory::Game,
            "Settings" => ResultCategory::Settings,
            "Document" => ResultCategory::Document,
            "ControlPanel" => ResultCategory::ControlPanel,
            _ => panic!("unknown fixture category: {value}"),
        }
    }

    #[test]
    fn synthetic_search_relevance_fixture_reports_top1_recall_and_mrr() {
        let fixture: SearchEvaluationFixture =
            serde_json::from_str(include_str!("../../tests/fixtures/search_relevance.json"))
                .expect("valid synthetic search evaluation fixture");
        assert!(fixture.queries.len() >= 20);
        let mut candidates = fixture
            .candidates
            .into_iter()
            .map(|candidate| {
                let mut item = result(
                    &candidate.id,
                    &candidate.title,
                    evaluation_category(&candidate.category),
                );
                item.aliases = candidate.aliases;
                item.metadata.keywords = candidate.keywords;
                item.location = candidate.location;
                item.description = candidate.description;
                item
            })
            .collect::<Vec<_>>();
        let core = SearchCore::new(std::mem::take(&mut candidates));
        let (mut top1, mut recall_at_5, mut reciprocal_rank, mut positive_queries) =
            (0usize, 0.0f64, 0.0f64, 0usize);
        let mut negative_false_positives = 0usize;
        let mut expected_best_checks = 0usize;
        let mut correct_best = 0usize;
        let mut wrong_expected_best = Vec::new();
        for case in &fixture.queries {
            let results = core.search(&case.query);
            let ids = results
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>();
            if case.check_best {
                expected_best_checks += 1;
                let best = core
                    .best_match_index(&case.query, &results)
                    .map(|index| results[index].id.as_str());
                if best == case.expected_best.as_deref() {
                    correct_best += 1;
                } else {
                    wrong_expected_best
                        .push((case.query.clone(), best.unwrap_or("<none>").to_owned()));
                }
            }
            if case.relevant.is_empty() {
                negative_false_positives += usize::from(!ids.is_empty());
                continue;
            }
            positive_queries += 1;
            if ids.first().is_some_and(|id| case.relevant.contains(id)) {
                top1 += 1;
            }
            let retrieved = ids
                .iter()
                .take(5)
                .filter(|id| case.relevant.contains(id))
                .collect::<std::collections::HashSet<_>>()
                .len();
            recall_at_5 += retrieved as f64 / case.relevant.len() as f64;
            if let Some(rank) = ids.iter().position(|id| case.relevant.contains(id)) {
                reciprocal_rank += 1.0 / (rank + 1) as f64;
            }
        }
        assert!(positive_queries > 0);
        assert_eq!(
            negative_false_positives, 0,
            "negative fixture queries returned candidates"
        );
        assert_eq!(
            correct_best, expected_best_checks,
            "best-match fixture expectations changed: {wrong_expected_best:?}"
        );
        assert!(top1 as f64 / positive_queries as f64 >= 0.64);
        assert!(recall_at_5 / positive_queries as f64 >= 0.70);
        assert!(reciprocal_rank / positive_queries as f64 >= 0.68);
        eprintln!(
            "[synthetic-search-eval] queries={} top1={:.3} recall@5={:.3} mrr={:.3} negatives_with_results={} best_match={}/{} errors={:?} (synthetic fixture, not measured user accuracy)",
            positive_queries,
            top1 as f64 / positive_queries as f64,
            recall_at_5 / positive_queries as f64,
            reciprocal_rank / positive_queries as f64,
            negative_false_positives,
            correct_best,
            expected_best_checks,
            wrong_expected_best,
        );
    }

    #[test]
    fn short_numeric_queries_require_title_evidence_not_control_panel_keywords() {
        let mut control = result(
            "control:date-format",
            "\u{66F4}\u{6539}\u{65E5}\u{671F}\u{3001}\u{65F6}\u{95F4}\u{6216}\u{6570}\u{5B57}\u{683C}\u{5F0F}",
            ResultCategory::ControlPanel,
        );
        control.metadata.keywords = [
            "1",
            "2",
            "3",
            "12 \u{5C0F}\u{65F6}",
            "12\u{5C0F}\u{65F6}",
            "24 \u{5C0F}\u{65F6}",
            "12-hour",
            "format",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let one_password = result("app:onepassword", "1Password", ResultCategory::Application);
        let windows_11 = result("settings:windows11", "Windows 11", ResultCategory::Settings);
        let core = SearchCore::new(vec![control, one_password, windows_11]);

        let numeric = core.search("1");
        assert!(numeric.iter().all(|item| item.id != "control:date-format"));
        assert_eq!(
            numeric.first().map(|item| item.id.as_str()),
            Some("app:onepassword")
        );
        assert_eq!(core.best_match_index("1", &numeric), Some(0));
        assert_eq!(core.search("\u{FF11}")[0].id, "app:onepassword");

        let eleven = core.search("11");
        assert_eq!(
            eleven.first().map(|item| item.id.as_str()),
            Some("settings:windows11")
        );
        assert_eq!(core.best_match_index("11", &eleven), Some(0));
    }

    #[test]
    fn typed_metadata_is_weak_generic_and_context_needs_a_name_anchor() {
        let make = |id: &str, title: &str| result(id, title, ResultCategory::Application);
        let mut common = (0..3)
            .map(|index| {
                let category = match index {
                    1 => ResultCategory::Bookmark,
                    2 => ResultCategory::ControlPanel,
                    _ => ResultCategory::Application,
                };
                let mut item = result(&format!("common-{index}"), "Device Manager", category);
                item.source = crate::platform::SourceId::Custom(format!("Synthetic{index}"));
                item.metadata.keywords = vec![
                    format!("protocol channel {index}"),
                    "protocol".into(),
                    "1394".into(),
                    "0".into(),
                    "42".into(),
                    "98765".into(),
                ];
                item
            })
            .collect::<Vec<_>>();
        let mut unique = make("unique-keyword", "Peripheral Utility");
        unique.metadata.keywords = vec!["quartz signal".into()];
        let mut context_only = make("context-only", "Unrelated Utility");
        context_only.metadata.context = vec!["Parent Category".into()];
        let mut anchored_context = make("context-anchored", "Device Settings");
        anchored_context.metadata.context = vec!["Parent Category".into()];
        let mut technical = make("technical", "Package Viewer");
        technical.metadata.technical = vec!["urn:vendor:package".into()];
        let mut numeric_name = make("numeric-alias", "Version Utility");
        numeric_name.aliases = vec!["256".into()];
        common.extend([
            unique,
            context_only,
            anchored_context,
            technical,
            numeric_name,
        ]);
        let core = SearchCore::new(common);

        assert!(core.search("protocol").is_empty());
        assert!(core.search("protocol channel").is_empty());
        assert!(core
            .search("device protocol")
            .iter()
            .any(|item| item.id == "common-0"));
        assert_eq!(
            core.search("quartz").first().map(|item| item.id.as_str()),
            Some("unique-keyword")
        );
        assert_eq!(
            core.best_match_index("quartz", &core.search("quartz")),
            None
        );
        assert!(core.search("parent").is_empty());
        assert_eq!(
            core.search("device parent")
                .first()
                .map(|item| item.id.as_str()),
            Some("context-anchored")
        );
        assert!(core.search("urn:vendor").is_empty());
        let technical_exact = core.search("urn:vendor:package");
        assert_eq!(
            technical_exact.first().map(|item| item.id.as_str()),
            Some("technical")
        );
        assert_eq!(
            core.best_match_index("urn:vendor:package", &technical_exact),
            None
        );
        assert_eq!(
            core.search("1394")
                .iter()
                .filter(|item| item.id.starts_with("common-"))
                .count(),
            0
        );
        assert!(core
            .search("device 1394")
            .iter()
            .any(|item| item.id == "common-0"));
        assert!(core
            .search("1394 device")
            .iter()
            .any(|item| item.id == "common-0"));
        for number in ["0", "42", "98765"] {
            assert!(core
                .search(number)
                .iter()
                .all(|item| !item.id.starts_with("common-")));
        }
        assert_eq!(core.best_match_index("256", &core.search("256")), Some(0));
    }

    #[test]
    fn persisted_results_without_typed_metadata_load_with_empty_roles() {
        let item = result("legacy", "Legacy Application", ResultCategory::Application);
        let mut json = serde_json::to_value(item).unwrap();
        json.as_object_mut().unwrap().remove("metadata");
        let restored: SearchResult = serde_json::from_value(json).unwrap();
        assert!(restored.metadata.is_empty());
    }

    #[test]
    fn root_whitespace_normalization_and_multiword_and_matching_are_consistent() {
        let mut chrome = result("chrome", "Google Chrome", ResultCategory::Application);
        chrome.aliases = vec!["browser".into()];
        let mut visual = result("visual", "Visual Utility", ResultCategory::Application);
        visual.aliases = vec!["Studio Toolkit".into()];
        let unrelated = result("unrelated", "Unrelated", ResultCategory::Application);
        let core = SearchCore::new(vec![chrome, visual, unrelated]);

        assert!(core.search("\u{3000}\u{3000}\t").is_empty());
        assert_eq!(core.search("\u{FF27}\u{FF4F}\u{FF4F}\u{FF47}\u{FF4C}\u{FF45}\u{3000} \u{FF23}\u{FF48}\u{FF52}\u{FF4F}\u{FF4D}\u{FF45}")[0].id, "chrome");
        assert_eq!(core.search("chrome google")[0].id, "chrome");
        assert_eq!(core.search("studio visual")[0].id, "visual");
        assert!(core.search("visual chrome unrelated").is_empty());
        core.replace_fuzzy_search(false);
        assert_eq!(core.search("chrome google")[0].id, "chrome");
    }
    #[test]
    fn provider_owned_query_keeps_native_hits_and_page_order() {
        let session = native_query_session();
        let first = vec![
            result("file:one", "zzzz.pdf", ResultCategory::Document),
            result("file:two", "a.pdf", ResultCategory::Document),
        ];
        let second = vec![
            result("file:three", "yyyy.pdf", ResultCategory::Document),
            result("file:four", "b.pdf", ResultCategory::Document),
        ];
        let mut core = SearchCore::new(Vec::new());
        core.providers.register_search_engine(NativeQueryEngine {
            session: session.clone(),
            first_page: first,
            second_page: second,
            policy: MatchingPolicy::ProviderOwned,
            received_queries: None,
        });

        let context = crate::page::SearchContext::Engine(session);
        let query = "ext:pdf report";
        let page_one = core.search_in_context(&context, query);
        let page_two = core.load_more_in_context(&context, query, page_one.len());
        assert_eq!(
            page_one
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["file:one", "file:two"]
        );
        assert_eq!(
            page_two
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["file:three", "file:four"]
        );
    }

    #[test]
    fn default_matching_policy_still_filters_root_candidates() {
        let session = native_query_session();
        let mut core = SearchCore::new(Vec::new());
        core.providers.register_search_engine(NativeQueryEngine {
            session,
            first_page: vec![result("file", "document.pdf", ResultCategory::Document)],
            second_page: Vec::new(),
            policy: MatchingPolicy::Central,
            received_queries: None,
        });
        assert!(core
            .search_in_context(&crate::page::SearchContext::Default, "ext:pdf report")
            .is_empty());
    }

    #[test]
    fn provider_owned_policy_is_limited_to_nonempty_active_session() {
        let session = native_query_session();
        let mut core = SearchCore::new(Vec::new());
        core.providers.register_search_engine(NativeQueryEngine {
            session: session.clone(),
            first_page: vec![result("file", "document.pdf", ResultCategory::Document)],
            second_page: Vec::new(),
            policy: MatchingPolicy::ProviderOwned,
            received_queries: None,
        });
        let query = "ext:pdf report";
        assert_eq!(
            core.search_in_context(&crate::page::SearchContext::Engine(session.clone()), query)
                .len(),
            1
        );
        assert!(core
            .search_in_context(&crate::page::SearchContext::Default, query)
            .is_empty());

        let mut central = SearchCore::new(Vec::new());
        central.providers.register_search_engine(NativeQueryEngine {
            session: session.clone(),
            first_page: vec![result("file", "document.pdf", ResultCategory::Document)],
            second_page: Vec::new(),
            policy: MatchingPolicy::Central,
            received_queries: None,
        });
        assert!(central
            .search_in_context(&crate::page::SearchContext::Engine(session), query)
            .is_empty());
    }

    #[test]
    fn central_normalization_does_not_rewrite_provider_query_payloads() {
        let session = native_query_session();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut core = SearchCore::new(Vec::new());
        core.providers.register_search_engine(NativeQueryEngine {
            session: session.clone(),
            first_page: vec![result("file", "unmatched", ResultCategory::Document)],
            second_page: Vec::new(),
            policy: MatchingPolicy::ProviderOwned,
            received_queries: Some(received.clone()),
        });
        let raw = "  ＧＯＯgle\tTest  ";
        core.search_in_context(&crate::page::SearchContext::Engine(session), raw);
        assert_eq!(received.lock().unwrap().as_slice(), [raw]);

        received.lock().unwrap().clear();
        let root_raw = "  RUN  WhoAmI  ";
        core.search(root_raw);
        assert!(received
            .lock()
            .unwrap()
            .iter()
            .any(|query| query == root_raw));
    }

    #[test]
    fn learned_behavior_can_reorder_different_lengths_inside_one_match_class() {
        let core = SearchCore::new(vec![
            result("short", "Calculator", ResultCategory::Application),
            result("long", "Calculator Extended", ResultCategory::Application),
        ]);
        assert_eq!(core.search("calc")[0].id, "short");
        for _ in 0..20 {
            core.record_success("long", "calc");
        }
        assert_eq!(core.search("calc")[0].id, "long");
    }

    #[test]
    fn limited_search_matches_unlimited_prefix() {
        let core = SearchCore::new(
            (0..80)
                .map(|index| {
                    result(
                        &format!("app:{index}"),
                        &format!("Calculator Tool {index}"),
                        ResultCategory::Application,
                    )
                })
                .collect(),
        );
        let context = crate::page::SearchContext::Default;
        let all = core.search_in_context(&context, "calc");
        for limit in [1, 7, 31] {
            assert_eq!(
                core.search_in_context_limited(&context, "calc", limit),
                all[..limit].to_vec()
            );
        }
    }

    #[test]
    #[ignore = "repeatable performance benchmark; run with --ignored --nocapture --test-threads=1"]
    fn first_and_rapid_query_benchmark() {
        use std::time::Instant;

        let core = SearchCore::new(
            (0..10_000)
                .map(|index| {
                    result(
                        &format!("app:{index}"),
                        &format!("Application {index} Calculator"),
                        ResultCategory::Application,
                    )
                })
                .collect(),
        );
        let ((first, first_result_count, mut rapid_samples), peak_threads) =
            crate::perf_support::measure_peak_threads(|| {
                let started = Instant::now();
                let first = core.search("application 3214");
                let first_elapsed = started.elapsed();
                let mut rapid_samples = Vec::with_capacity(100);
                for index in 0..100 {
                    let started = Instant::now();
                    let _ = core.search(&format!("app {index}"));
                    rapid_samples.push(started.elapsed());
                }
                (first_elapsed, first.len(), rapid_samples)
            });
        let (rapid_p50, rapid_p95) = crate::perf_support::percentiles(&mut rapid_samples);
        let stats = crate::utils::background::stats();
        eprintln!(
            "[perf] search index_records=10000 first_query_us={} first_results={} rapid_queries=100 rapid_p50_us={} rapid_p95_us={} process_peak_threads={} pending_tasks={} pending_peak={}",
            first.as_micros(),
            first_result_count,
            rapid_p50.as_micros(),
            rapid_p95.as_micros(),
            peak_threads,
            stats.pending,
            stats.peak_pending,
        );
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
            evidence_role: EvidenceRole::Name,
            match_quality: 100,
            composite_score: 100,
            category_order: 0,
            plugin_order: usize::MAX,
            engine_bias: 0,
            provider_bias: 0,
        };
        let keys = [
            RankKey {
                plugin_order: 0,
                ..base
            },
            RankKey {
                plugin_order: 1,
                ..base
            },
            RankKey { ..base },
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
                composite_score: 200,
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
    fn auxiliary_evidence_cannot_beat_name_evidence_with_behavior_bonus() {
        let name = RankKey {
            evidence_role: EvidenceRole::Name,
            match_class: MATCH_CLASS_FUZZY,
            composite_score: 0,
            ..rank_key_for_test()
        };
        let auxiliary = RankKey {
            evidence_role: EvidenceRole::Auxiliary,
            match_class: MATCH_CLASS_EXACT,
            composite_score: 100,
            ..rank_key_for_test()
        };
        assert_eq!(name.compare(&auxiliary), std::cmp::Ordering::Less);
    }

    fn rank_key_for_test() -> RankKey {
        RankKey {
            history_order: None,
            favorite_order: None,
            fallback: false,
            match_class: MATCH_CLASS_CONTAINS,
            evidence_role: EvidenceRole::Name,
            match_quality: 0,
            composite_score: 0,
            category_order: 0,
            plugin_order: usize::MAX,
            engine_bias: 0,
            provider_bias: 0,
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
            metadata: Default::default(),
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
        generated.source = crate::platform::SourceId::Shortcut;
        generated.sources = vec![crate::platform::SourceId::Shortcut];
        generated
            .actions
            .push(crate::result::Action::Launch(crate::result::LaunchEntry {
                source: crate::platform::SourceId::Shortcut,
                path: "ipconfig".into(),
                target: "ipconfig".into(),
                arguments: String::new(),
                working_directory: None,
                icon: None,
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
        assert_eq!(results[0].effective_category(), ResultCategory::Command);
    }
    #[test]
    fn fuzzy_search_matches_typo_by_default() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "Notepad".into(),
            category: ResultCategory::Application,
            metadata: Default::default(),
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
            metadata: Default::default(),
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
            metadata: Default::default(),
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
            metadata: Default::default(),
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
                metadata: Default::default(),
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
            metadata: Default::default(),
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
            metadata: Default::default(),
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
            metadata: Default::default(),
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
            metadata: Default::default(),
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
            metadata: Default::default(),
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
            metadata: Default::default(),
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
            metadata: Default::default(),
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
    fn root_session_entries_share_the_command_category() {
        let core = SearchCore::with_plugins_and_shortcuts(
            Vec::new(),
            PluginManager::discover_with_config("en", &std::collections::BTreeMap::new()),
            Vec::new(),
        );
        core.replace_provider_language("en".into());

        let roots = core.search("");
        let entries = roots
            .iter()
            .filter(|item| item.is_session_entry())
            .collect::<Vec<_>>();
        assert!(entries
            .iter()
            .any(|item| item.id.starts_with("shortcut:") && item.title == "Execute Command"));
        assert!(entries.iter().any(|item| item.id == "bookmarks:entry"));
        assert!(entries.iter().any(|item| item.id == "windows:entry"));
        for entry in entries {
            assert_eq!(entry.effective_category(), ResultCategory::Command);
        }
        assert!(roots
            .iter()
            .filter(|item| !item.is_session_entry())
            .all(|item| item.effective_category() == item.category));

        // Entering the window session produces real window rows, not more
        // command entries.
        let context = core.search_context("< ");
        let windows = core.search_in_context(&context, "");
        assert!(windows.iter().all(|item| !item.is_session_entry()));
        assert!(windows
            .iter()
            .all(|item| item.category == ResultCategory::Window));
        assert!(windows
            .iter()
            .all(|item| item.effective_category() == ResultCategory::Window));
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
            metadata: Default::default(),
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
    fn weak_single_character_hits_stay_ordinary_rows() {
        let item = result("calculator", "Calculator", ResultCategory::Application);
        let core = SearchCore::new(vec![item.clone()]);
        // The character only appears inside the word, never at its start.
        assert_eq!(core.search("a").len(), 1);
        assert_eq!(
            core.best_match_index("a", std::slice::from_ref(&item)),
            None
        );
    }

    #[test]
    fn exact_prefix_and_word_start_evidence_keeps_best_match() {
        let core = SearchCore::new(Vec::new());
        let exact = result("exact", "Calculator", ResultCategory::Application);
        assert_eq!(
            core.best_match_index("calculator", std::slice::from_ref(&exact)),
            Some(0)
        );
        assert_eq!(
            core.best_match_index("calc", std::slice::from_ref(&exact)),
            Some(0)
        );
        // A single-character prefix is a reasonable query and stays strong.
        assert_eq!(
            core.best_match_index("c", std::slice::from_ref(&exact)),
            Some(0)
        );
        let word = result("word", "My Calculator Tool", ResultCategory::Application);
        assert_eq!(
            core.best_match_index("calc", std::slice::from_ref(&word)),
            Some(0)
        );
        // A lone character at an internal word start is not enough.
        assert_eq!(
            core.best_match_index("c", std::slice::from_ref(&word)),
            None
        );
    }

    #[test]
    fn best_match_scans_past_an_earlier_weak_match() {
        let weak = result("weak", "Xylophone", ResultCategory::Application);
        let strong = result("strong", "Photo", ResultCategory::Application);
        let core = SearchCore::new(Vec::new());
        assert_eq!(core.best_match_index("pho", &[weak, strong]), Some(1));
    }

    #[test]
    fn chinese_evidence_requires_a_phrase_or_prefix() {
        let core = SearchCore::new(Vec::new());
        let wechat = result("wechat", "微信电脑版", ResultCategory::Application);
        assert_eq!(
            core.best_match_index("微信", std::slice::from_ref(&wechat)),
            Some(0)
        );
        // Two CJK characters are a meaningful phrase even without spaces.
        assert_eq!(
            core.best_match_index("电脑", std::slice::from_ref(&wechat)),
            Some(0)
        );
        assert_eq!(
            core.best_match_index("信", std::slice::from_ref(&wechat)),
            None
        );
        let settings = result("settings", "Windows 设置", ResultCategory::Application);
        assert_eq!(
            core.best_match_index("设置", std::slice::from_ref(&settings)),
            Some(0)
        );
    }

    #[test]
    fn only_typo_level_fuzzy_matches_can_headline_the_list() {
        let notepad = result("notepad", "Notepad", ResultCategory::Application);
        let core = SearchCore::new(vec![notepad.clone()]);
        let items = std::slice::from_ref(&notepad);
        // A scattered subsequence is still a visible ordinary result.
        assert_eq!(core.search("ntpd").len(), 1);
        assert_eq!(core.best_match_index("ntpd", items), None);
        // One skipped letter is typo-level evidence.
        assert_eq!(core.best_match_index("notpad", items), Some(0));
    }

    #[test]
    fn fuzzy_evidence_counts_skipped_characters_not_gap_runs() {
        let core = SearchCore::new(Vec::new());
        // One non-consecutive transition but eight skipped characters.
        let spaced = result("spaced", "aXXXXXXXXbc", ResultCategory::Application);
        assert_eq!(
            core.best_match_index("abc", std::slice::from_ref(&spaced)),
            None
        );
        // A real single-character typo still qualifies.
        let notepad = result("notepad", "Notepad", ResultCategory::Application);
        assert_eq!(
            core.best_match_index("notpad", std::slice::from_ref(&notepad)),
            Some(0)
        );
    }

    #[test]
    fn aliases_supply_best_match_evidence() {
        let core = SearchCore::new(Vec::new());
        let mut item = result(
            "tool",
            "Unrelated display name",
            ResultCategory::Application,
        );
        item.aliases = vec!["calc".into()];
        let items = std::slice::from_ref(&item);
        assert_eq!(core.best_match_index("calc", items), Some(0));
        assert_eq!(core.best_match_index("ca", items), Some(0));
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
