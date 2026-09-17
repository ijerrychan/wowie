use crate::result::SearchResult;

pub struct SearchCore {
    results: Vec<SearchResult>,
}

impl SearchCore {
    pub fn new(results: Vec<SearchResult>) -> Self {
        Self { results }
    }
    pub fn by_id(&self, id: &str) -> Option<SearchResult> {
        self.results.iter().find(|x| x.id == id).cloned()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query = query.trim().to_lowercase();
        let debug = std::env::var_os("WOWIE_SEARCH_DEBUG").is_some_and(|value| value != "0");
        if debug {
            eprintln!(
                "[search] query={:?} limit={} indexed_records={}",
                query,
                limit,
                self.results.len()
            );
        }
        let mut ranked: Vec<(i32, &SearchResult)> = self
            .results
            .iter()
            .filter_map(|item| {
                let title_score = match_score(&query, &item.title);
                let alias_score = item
                    .aliases
                    .iter()
                    .filter_map(|alias| match_score(&query, alias))
                    .max();
                let score = [title_score, alias_score]
                    .into_iter()
                    .flatten()
                    .max()
                    .filter(|score| *score > 0);
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
                score.map(|score| (score, item))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        let found: Vec<SearchResult> = ranked
            .into_iter()
            .take(limit)
            .map(|(_, item)| item.clone())
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
}

fn match_score(query: &str, title: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let text = title.to_lowercase();
    if text == query {
        return Some(1000);
    }
    if text.starts_with(query) {
        return Some((800 - text.chars().count() as i32).max(1));
    }
    if text.contains(query) {
        return Some((600 - text.chars().count() as i32).max(1));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranks_prefix_first() {
        let make = |id: &str, title: &str| SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            aliases: vec![],
            location: "".into(),
            launch_target: title.into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
        };
        let core = SearchCore::new(vec![make("1", "Calculator"), make("2", "Calendar")]);
        assert_eq!(core.search("calc", 2)[0].title, "Calculator");
    }
    #[test]
    fn does_not_support_fuzzy_typo() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "Notepad".into(),
            aliases: vec![],
            location: "".into(),
            launch_target: "notepad.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
        };
        assert!(SearchCore::new(vec![item]).search("ntpd", 1).is_empty());
    }

    #[test]
    fn searches_aliases() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "A very long display name".into(),
            aliases: vec!["calc".into()],
            location: "".into(),
            launch_target: "calculator.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
        };
        assert_eq!(SearchCore::new(vec![item]).search("calc", 1).len(), 1);
    }

    #[test]
    fn alias_score_is_not_hidden_by_a_lower_title_score() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "A long calculator display name".into(),
            aliases: vec!["calc".into()],
            location: "".into(),
            launch_target: "calculator.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
        };
        assert_eq!(SearchCore::new(vec![item]).search("calc", 1).len(), 1);
    }

    #[test]
    fn unrelated_chinese_title_does_not_match_pinyin_query() {
        let item = SearchResult {
            id: "settings".into(),
            identity: "settings".into(),
            title: "设置".into(),
            aliases: vec![],
            location: "".into(),
            launch_target: "settings.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
        };
        assert!(SearchCore::new(vec![item]).search("weixin", 12).is_empty());
    }

    #[test]
    fn contains_match_is_ranked_before_longer_contains_match() {
        let make = |id: &str, title: &str| SearchResult {
            id: id.into(),
            identity: id.into(),
            title: title.into(),
            aliases: vec![],
            location: String::new(),
            launch_target: title.into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
        };
        let core = SearchCore::new(vec![
            make("long", "My Calculator Tool"),
            make("short", "Calculator"),
        ]);
        assert_eq!(core.search("calc", 2)[0].id, "short");
    }

    #[test]
    fn search_respects_zero_limit() {
        let item = SearchResult {
            id: "1".into(),
            identity: "1".into(),
            title: "Calculator".into(),
            aliases: vec![],
            location: String::new(),
            launch_target: "calculator.exe".into(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "test".into(),
            sources: vec!["test".into()],
            launch_entries: vec![],
            actions: vec![],
        };
        assert!(SearchCore::new(vec![item]).search("calc", 0).is_empty());
    }
}
