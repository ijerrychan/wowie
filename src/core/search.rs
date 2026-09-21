use crate::plugin::PluginManager;
use crate::provider::ProviderRegistry;
use crate::ranking::Ranker;
use crate::result::SearchResult;
use anyhow::{Context, Result};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub struct SearchCore {
    results: Mutex<Arc<Vec<SearchResult>>>,
    providers: ProviderRegistry,
    dynamic_results: Mutex<Vec<SearchResult>>,
    ranker: Ranker,
    fuzzy_search: AtomicBool,
}

impl SearchCore {
    #[cfg(test)]
    pub fn new(results: Vec<SearchResult>) -> Self {
        Self {
            results: Mutex::new(Arc::new(results)),
            providers: ProviderRegistry::without_dynamic(PluginManager::discover_with_config(
                "en",
                &std::collections::BTreeMap::new(),
            )),
            dynamic_results: Mutex::new(Vec::new()),
            ranker: Ranker::in_memory(),
            fuzzy_search: AtomicBool::new(true),
        }
    }

    pub fn with_plugins_and_shortcuts(
        results: Vec<SearchResult>,
        plugins: PluginManager,
        shortcuts: Vec<crate::config::ShortcutConfig>,
    ) -> Self {
        Self {
            results: Mutex::new(Arc::new(results)),
            providers: ProviderRegistry::default_providers(plugins, shortcuts),
            dynamic_results: Mutex::new(Vec::new()),
            ranker: Ranker::load(),
            fuzzy_search: AtomicBool::new(true),
        }
    }

    pub fn replace_shortcuts(&self, shortcuts: Vec<crate::config::ShortcutConfig>) {
        self.providers.replace_shortcuts(shortcuts);
    }

    pub fn replace_fuzzy_search(&self, enabled: bool) {
        self.fuzzy_search.store(enabled, Ordering::Release);
    }

    pub fn replace_provider_language(&self, language: String) {
        self.providers.set_language(language);
    }

    pub fn preferred_view_mode(&self, context: &crate::page::SearchContext) -> &'static str {
        self.providers.preferred_view_mode(context)
    }

    pub fn search_context(&self, query: &str) -> crate::page::SearchContext {
        self.providers.search_context(query)
    }

    pub fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        self.providers.shortcut_hint(result_id)
    }

    pub fn replace_plugin_config(
        &self,
        plugin_configs: &std::collections::BTreeMap<String, crate::config::PluginConfig>,
    ) {
        self.providers.replace_plugin_config(plugin_configs);
    }

    pub fn record_success(&self, item_id: &str, query: &str) {
        self.ranker.record_success(item_id, query);
    }
    pub fn by_id(&self, id: &str) -> Option<SearchResult> {
        self.results
            .lock()
            .ok()?
            .iter()
            .find(|x| x.id == id)
            .cloned()
            .or_else(|| {
                self.dynamic_results
                    .lock()
                    .ok()?
                    .iter()
                    .find(|x| x.id == id)
                    .cloned()
            })
    }

    pub fn replace_results(&self, results: Arc<Vec<SearchResult>>) {
        if let Ok(mut indexed) = self.results.lock() {
            *indexed = results;
        }
    }

    pub fn execute_action(&self, item_id: &str) -> Result<crate::plugin::ExecuteResponse> {
        let item = self.by_id(item_id).context("result not found")?;
        let action_id = item
            .actions
            .iter()
            .find_map(|action| match action {
                crate::result::Action::Plugin { action_id, .. } => Some(action_id.as_str()),
                crate::result::Action::Launch(_)
                | crate::result::Action::EnterSearchEngine { .. } => None,
            })
            .context("result has no plugin action")?;
        self.execute_plugin_action(item_id, action_id)
    }

    pub fn execute_plugin_action(
        &self,
        item_id: &str,
        requested_action_id: &str,
    ) -> Result<crate::plugin::ExecuteResponse> {
        let item = self.by_id(item_id).context("result not found")?;
        let (_, action_id) = crate::provider::find_plugin_action(&item, requested_action_id)?;
        self.providers.execute_plugin_action(&item, action_id)
    }

    /// Convenience wrapper retained for unit tests and small in-process
    /// callers. The UI uses the explicit context-aware methods below.
    #[cfg(test)]
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let context = self.search_context(query);
        self.search_in_context(&context, query)
    }

    /// Load the next page for an already-resolved search-engine session.
    /// Accepted rows are retained with the current dynamic result cache so
    /// later launch callbacks can resolve them by id.
    pub fn load_more_in_context(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
        offset: usize,
    ) -> Vec<SearchResult> {
        let more = self.providers.query_more(context, query, offset);
        let Ok(mut cached) = self.dynamic_results.lock() else {
            return Vec::new();
        };
        let mut accepted = Vec::new();
        for item in more {
            if cached.iter().any(|existing| existing.id == item.id) {
                continue;
            }
            cached.push(item.clone());
            accepted.push(item);
        }
        accepted
    }

    /// Search a context resolved by the caller. UI code should resolve once
    /// and reuse this value for rendering, navigation and result retrieval.
    pub fn search_in_context(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
    ) -> Vec<SearchResult> {
        let input = query.to_owned();
        // An active engine owns the prefix. Its result titles are matched
        // against the argument only (`e report` searches `report`), rather
        // than against the launcher syntax including `e `.
        let raw_query = context
            .session()
            .map(|session| session.argument.trim())
            .unwrap_or_else(|| input.trim())
            .to_owned();
        let query = raw_query.to_lowercase();
        let search_engine_active = context.session().is_some();
        let fuzzy_search = self.fuzzy_search.load(Ordering::Acquire);
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let mut matcher_buffer = Vec::new();
        let mut needle_buffer = Vec::new();
        let debug = std::env::var_os("WOWIE_SEARCH_DEBUG").is_some_and(|value| value != "0");
        if debug {
            eprintln!(
                "[search] query={:?} indexed_records={}",
                query,
                self.results
                    .lock()
                    .map(|results| results.len())
                    .unwrap_or(0)
            );
        }
        let dynamic = self.providers.query(context, &input, fuzzy_search);
        // Launch callbacks resolve rows by id after the search worker returns.
        // Keep only the current query's dynamic records so a repeated shortcut
        // id cannot resolve to an older command or URL.
        if let Ok(mut cached) = self.dynamic_results.lock() {
            cached.clear();
            cached.extend(dynamic.iter().cloned());
        }
        let indexed = if search_engine_active {
            None
        } else {
            self.results
                .lock()
                .ok()
                .map(|indexed| Arc::clone(&*indexed))
        };
        let mut ranked: Vec<(RankKey, &SearchResult)> = indexed
            .as_ref()
            .map(|results| results.iter())
            .into_iter()
            .flatten()
            .chain(dynamic.iter())
            .filter_map(|item| {
                let title_score = match_score(
                    &query,
                    &item.title,
                    fuzzy_search,
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
                            fuzzy_search,
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
                        "[search] candidate title={:?} title_score={:?} alias_score={:?} matched={} identity={:?} sources={:?} location={:?} launch_target={:?} args={:?}",
                        item.title,
                        title_score,
                        alias_score,
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
                score.or_else(|| {
                    fallback.then_some(MatchScore {
                        class: 0,
                        quality: 0,
                    })
                })
                .map(|score| {
                    (
                        RankKey {
                            fallback,
                            match_class: score.class,
                            match_quality: score.quality,
                            behavior: self.ranker.score(&item.id, &query),
                            provider_bias: item.ranking_bias,
                        },
                        item,
                    )
                })
            })
            .collect();
        ranked.sort_by(|a, b| {
            a.0.fallback
                .cmp(&b.0.fallback)
                .then_with(|| b.0.match_class.cmp(&a.0.match_class))
                .then_with(|| b.0.match_quality.cmp(&a.0.match_quality))
                .then_with(|| b.0.behavior.cmp(&a.0.behavior))
                .then_with(|| b.0.provider_bias.cmp(&a.0.provider_bias))
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MatchScore {
    class: u8,
    quality: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RankKey {
    fallback: bool,
    match_class: u8,
    match_quality: i32,
    behavior: i32,
    provider_bias: i32,
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
        };
        let core = SearchCore::new(vec![make("1", "Calculator"), make("2", "Calendar")]);
        assert_eq!(core.search("calc")[0].title, "Calculator");
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
        };
        let core = SearchCore::with_plugins_and_shortcuts(
            vec![notepad],
            PluginManager::discover_with_config("en", &std::collections::BTreeMap::new()),
            Vec::new(),
        );
        let found = core.search("notepad");
        assert_eq!(found[0].title, "记事本");
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
