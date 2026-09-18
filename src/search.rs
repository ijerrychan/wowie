use crate::plugin::PluginManager;
use crate::ranking::Ranker;
use crate::result::SearchResult;
use anyhow::{Context, Result};
use std::sync::Mutex;

pub struct SearchCore {
    results: Mutex<Vec<SearchResult>>,
    plugins: PluginManager,
    dynamic_results: Mutex<Vec<SearchResult>>,
    shortcuts: Option<Mutex<crate::shortcuts::ShortcutProvider>>,
    ranker: Ranker,
}

impl SearchCore {
    #[cfg(test)]
    pub fn new(results: Vec<SearchResult>) -> Self {
        Self {
            results: Mutex::new(results),
            plugins: PluginManager::discover_with_locale("en"),
            dynamic_results: Mutex::new(Vec::new()),
            shortcuts: None,
            ranker: Ranker::in_memory(),
        }
    }

    pub fn with_plugins_and_shortcuts(
        results: Vec<SearchResult>,
        plugins: PluginManager,
        shortcuts: Vec<crate::config::ShortcutConfig>,
    ) -> Self {
        Self {
            results: Mutex::new(results),
            plugins,
            dynamic_results: Mutex::new(Vec::new()),
            shortcuts: Some(Mutex::new(crate::shortcuts::ShortcutProvider::new(
                shortcuts,
            ))),
            ranker: Ranker::load(),
        }
    }

    pub fn replace_shortcuts(&self, shortcuts: Vec<crate::config::ShortcutConfig>) {
        if let Some(provider) = &self.shortcuts {
            if let Ok(mut provider) = provider.lock() {
                provider.replace_config(shortcuts);
            }
        }
    }

    pub fn replace_shortcut_language(&self, language: String) {
        if let Some(provider) = &self.shortcuts {
            if let Ok(mut provider) = provider.lock() {
                provider.set_language(language);
            }
        }
    }

    pub fn replace_plugin_language(&self, language: String) {
        self.plugins.set_locale(language);
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

    pub fn replace_results(&self, results: Vec<SearchResult>) {
        if let Ok(mut indexed) = self.results.lock() {
            *indexed = results;
        }
    }

    pub fn execute_action(&self, item_id: &str) -> Result<crate::plugin::ExecuteResponse> {
        let item = self.by_id(item_id).context("result not found")?;
        let Some(crate::result::Action::Plugin {
            plugin_id,
            action_id,
        }) = item.actions.first()
        else {
            anyhow::bail!("result has no plugin action")
        };
        let output = self.plugins.execute(
            plugin_id,
            item.id.rsplit(':').next().unwrap_or_default(),
            action_id,
        )?;
        serde_json::from_str(&output).context("decode plugin execute response")
    }

    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let input = query.to_owned();
        let raw_query = input.trim().to_owned();
        let query = raw_query.to_lowercase();
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
        let mut all = self
            .results
            .lock()
            .map(|results| results.clone())
            .unwrap_or_default();
        let dynamic = self.plugins.query(&query);
        all.extend(dynamic.iter().cloned());
        let mut generated_shortcuts = Vec::new();
        if let Some(provider) = &self.shortcuts {
            if let Ok(provider) = provider.lock() {
                generated_shortcuts = provider.results(&input);
            }
            all.extend(generated_shortcuts.iter().cloned());
        }
        // Launch callbacks resolve rows by id after the search worker returns.
        // Keep only the current query's dynamic records so a repeated shortcut
        // id cannot resolve to an older command or URL.
        if let Ok(mut cached) = self.dynamic_results.lock() {
            cached.clear();
            cached.extend(dynamic);
            cached.extend(generated_shortcuts);
        }
        let mut ranked: Vec<(RankKey, &SearchResult)> = all
            .iter()
            .filter_map(|item| {
                let title_score = match_score(&query, &item.title);
                let alias_score = item
                    .aliases
                    .iter()
                    .filter_map(|alias| {
                        let score = match_score(&query, alias)?;
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
                score.map(|score| {
                    (
                        RankKey {
                            intent_priority: intent_priority(item, &query),
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
            b.0.intent_priority
                .cmp(&a.0.intent_priority)
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
    intent_priority: u8,
    match_class: u8,
    match_quality: i32,
    behavior: i32,
    provider_bias: i32,
}

/// Explicit shortcut syntax expresses direct intent. An unprefixed shortcut
/// is only a fallback and must not outrank ordinary indexed results.
fn intent_priority(item: &SearchResult, query: &str) -> u8 {
    if item.category != crate::result::ResultCategory::Shortcut {
        return 1;
    }
    if item
        .aliases
        .iter()
        .any(|alias| alias.eq_ignore_ascii_case(query))
    {
        2
    } else {
        0
    }
}

fn match_score(query: &str, title: &str) -> Option<MatchScore> {
    if query.is_empty() {
        return Some(MatchScore {
            class: 0,
            quality: 0,
        });
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
    None
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
            icon: None,
            ranking_bias: 0,
        };
        let core = SearchCore::new(vec![make("1", "Calculator"), make("2", "Calendar")]);
        assert_eq!(core.search("calc")[0].title, "Calculator");
    }
    #[test]
    fn does_not_support_fuzzy_typo() {
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
            icon: None,
            ranking_bias: 0,
        };
        assert!(SearchCore::new(vec![item]).search("ntpd").is_empty());
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
    fn implicit_shortcuts_stay_after_ordinary_results() {
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
            icon: None,
            ranking_bias: 0,
        };
        let core = SearchCore::with_plugins_and_shortcuts(
            vec![notepad],
            PluginManager::discover_with_locale("en"),
            Vec::new(),
        );
        let found = core.search("notepad");
        assert_eq!(found[0].title, "记事本");
        assert!(found
            .iter()
            .any(|item| item.category == ResultCategory::Shortcut));
    }

    #[test]
    fn explicit_shortcut_prefix_is_available_before_an_argument() {
        let core = SearchCore::with_plugins_and_shortcuts(
            Vec::new(),
            PluginManager::discover_with_locale("en"),
            Vec::new(),
        );
        core.replace_shortcut_language("en".into());
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
            PluginManager::discover_with_locale("en"),
            Vec::new(),
        );
        let found = core.search("> ipconfig");
        let item = core.by_id(&found[0].id).expect("shortcut result by id");
        assert!(item.title.ends_with("\"ipconfig\""));
    }
}
