use crate::result::SearchResult;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

const HALF_LIFE_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_BEHAVIOR_SCORE: i32 = 180;
pub(crate) const MAX_ENGINE_SEARCH_HISTORY: usize = 50;
pub(crate) const MAX_FAVORITES: usize = 256;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct UsageRecord {
    launches: u32,
    last_used: u64,
    query_launches: HashMap<String, u32>,
    query_last_used: HashMap<String, u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct HistoryFile {
    items: HashMap<String, UsageRecord>,
    engine_searches: HashMap<String, Vec<EngineSearchRecord>>,
    favorites: Vec<FavoriteRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FavoriteRecord {
    pub(crate) item: SearchResult,
    #[serde(default)]
    pub(crate) session: Option<crate::page::SearchEngineSession>,
}

#[derive(Default)]
pub(crate) struct FavoriteSnapshot {
    pub(crate) items: Vec<FavoriteRecord>,
    pub(crate) order: HashMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct EngineSearchRecord {
    query: String,
    result_id: String,
}

/// Local, opt-in-through-use ranking state. It only learns after a successful
/// action and remains independent from provider data and the user config.
pub struct Ranker {
    history: std::sync::Mutex<HistoryFile>,
    path: Option<std::path::PathBuf>,
}

impl Ranker {
    pub fn load() -> Self {
        let path = crate::config::history_path();
        let history = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            history: std::sync::Mutex::new(history),
            path: Some(path),
        }
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            history: std::sync::Mutex::new(HistoryFile::default()),
            path: None,
        }
    }

    #[cfg(test)]
    pub fn score(&self, item_id: &str, query: &str) -> i32 {
        self.scores([item_id], query)[0]
    }

    /// Score one candidate batch against a consistent history and timestamp.
    pub(crate) fn scores<'a>(
        &self,
        item_ids: impl IntoIterator<Item = &'a str>,
        query: &str,
    ) -> Vec<i32> {
        let history = self.history.lock().ok();
        let now = unix_seconds();
        let query = normalize_query(query);
        item_ids
            .into_iter()
            .map(|item_id| {
                let Some(record) = history
                    .as_ref()
                    .and_then(|history| history.items.get(item_id))
                else {
                    return 0;
                };
                let global = frecency(record.launches, record.last_used, now);
                let query_score = record
                    .query_launches
                    .get(&query)
                    .zip(record.query_last_used.get(&query))
                    .map(|(count, last_used)| frecency(*count, *last_used, now) * 2)
                    .unwrap_or(0);
                (global + query_score).min(MAX_BEHAVIOR_SCORE)
            })
            .collect()
    }

    pub fn is_favorite(&self, item_id: &str) -> bool {
        self.history.lock().is_ok_and(|history| {
            history
                .favorites
                .iter()
                .any(|favorite| favorite.item.id == item_id)
        })
    }

    /// Toggle a persistent favorite, returning its new state. `None` means
    /// the history lock was poisoned or the favorite limit was reached.
    pub fn toggle_favorite(
        &self,
        item: &SearchResult,
        session: Option<&crate::page::SearchEngineSession>,
    ) -> Option<bool> {
        let Ok(mut history) = self.history.lock() else {
            return None;
        };
        if let Some(index) = history
            .favorites
            .iter()
            .position(|favorite| favorite.item.id == item.id)
        {
            history.favorites.remove(index);
            self.save_locked(&history);
            return Some(false);
        }
        if !item.allow_favorite {
            return Some(false);
        }
        if history.favorites.len() >= MAX_FAVORITES {
            return None;
        }
        history.favorites.insert(
            0,
            FavoriteRecord {
                item: item.clone(),
                session: session.cloned(),
            },
        );
        self.save_locked(&history);
        Some(true)
    }

    pub fn favorite_session(&self, item_id: &str) -> Option<crate::page::SearchEngineSession> {
        let history = self.history.lock().ok()?;
        history
            .favorites
            .iter()
            .find(|favorite| favorite.item.id == item_id)
            .and_then(|favorite| favorite.session.clone())
    }

    pub(crate) fn favorite_snapshot(&self, include_items: bool) -> FavoriteSnapshot {
        let Ok(history) = self.history.lock() else {
            return FavoriteSnapshot::default();
        };
        let mut snapshot = FavoriteSnapshot::default();
        let mut next_order = 0;
        for favorite in &history.favorites {
            if !snapshot.order.contains_key(&favorite.item.id) {
                let order = next_order;
                next_order += 1;
                snapshot.order.insert(favorite.item.id.clone(), order);
                if include_items {
                    snapshot.items.push(favorite.clone());
                }
            }
        }
        snapshot
    }

    #[cfg(test)]
    pub fn record_success(&self, item_id: &str, query: &str) {
        self.record_action_success(item_id, query, None);
    }

    pub fn record_action_success(&self, item_id: &str, query: &str, engine: Option<(&str, usize)>) {
        let Ok(mut history) = self.history.lock() else {
            return;
        };
        let now = unix_seconds();
        let raw_query = query;
        let query = normalize_query(raw_query);
        let record = history.items.entry(item_id.to_owned()).or_default();
        record.launches = record.launches.saturating_add(1);
        record.last_used = now;
        if !query.is_empty() {
            let launches = record.query_launches.entry(query.clone()).or_default();
            *launches = launches.saturating_add(1);
            record.query_last_used.insert(query, now);
        }
        if let Some((engine_id, limit)) = engine {
            record_engine_search_in(&mut history, engine_id, item_id, raw_query, limit);
        }
        self.save_locked(&history);
    }

    pub fn recent_engine_searches(
        &self,
        engine_id: &str,
        limit: usize,
    ) -> Vec<crate::provider::SearchHistoryEntry> {
        if limit == 0 {
            return Vec::new();
        }
        let Ok(history) = self.history.lock() else {
            return Vec::new();
        };
        history
            .engine_searches
            .get(engine_id)
            .into_iter()
            .flatten()
            .take(limit.min(MAX_ENGINE_SEARCH_HISTORY))
            .map(|record| crate::provider::SearchHistoryEntry {
                query: record.query.clone(),
                result_id: record.result_id.clone(),
            })
            .collect()
    }

    #[cfg(test)]
    pub fn record_engine_search_success(
        &self,
        engine_id: &str,
        result_id: &str,
        query: &str,
        limit: usize,
    ) {
        let limit = limit.min(MAX_ENGINE_SEARCH_HISTORY);
        if limit == 0 {
            return;
        }
        let Ok(mut history) = self.history.lock() else {
            return;
        };
        if !record_engine_search_in(&mut history, engine_id, result_id, query, limit) {
            return;
        }
        self.save_locked(&history);
    }

    fn save_locked(&self, history: &HistoryFile) {
        let Some(path) = &self.path else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        let result = (|| -> anyhow::Result<()> {
            fs::create_dir_all(parent)?;
            let mut json = serde_json::to_vec_pretty(history)?;
            json.push(b'\n');
            crate::utils::atomic_write(path, &json)?;
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!(
                "could not persist ranking history {}: {error:#}",
                path.display()
            );
        }
    }
}

fn record_engine_search_in(
    history: &mut HistoryFile,
    engine_id: &str,
    result_id: &str,
    query: &str,
    limit: usize,
) -> bool {
    let limit = limit.min(MAX_ENGINE_SEARCH_HISTORY);
    if limit == 0 {
        return false;
    }
    if query.trim().is_empty() && !history.engine_searches.contains_key(engine_id) {
        return false;
    }
    let records = history
        .engine_searches
        .entry(engine_id.to_owned())
        .or_default();
    let query = query.trim();
    let index = if query.is_empty() {
        records.iter().position(|record| {
            result_id == record.result_id || result_id.ends_with(&record.result_id)
        })
    } else {
        let normalized = normalize_query(query);
        records
            .iter()
            .position(|record| normalize_query(&record.query) == normalized)
    };
    let Some(index) = index else {
        if query.is_empty() {
            return false;
        }
        let query = query.to_owned();
        records.insert(
            0,
            EngineSearchRecord {
                result_id: engine_history_result_id(engine_id, &query),
                query,
            },
        );
        records.truncate(limit);
        return true;
    };
    let mut record = records.remove(index);
    if !query.is_empty() {
        record.query = query.to_owned();
    }
    records.insert(0, record);
    records.truncate(limit);
    true
}

pub(crate) fn engine_history_result_id(engine_id: &str, query: &str) -> String {
    let normalized = normalize_query(query);
    let hash = (engine_id
        .as_bytes()
        .iter()
        .copied()
        .chain([0xff])
        .chain(normalized.bytes()))
    .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{engine_id}:history:{hash:016x}")
}

fn normalize_query(query: &str) -> String {
    query.trim().to_lowercase()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn frecency(count: u32, last_used: u64, now: u64) -> i32 {
    if count == 0 || last_used == 0 {
        return 0;
    }
    let age = now.saturating_sub(last_used);
    let decay = if age >= HALF_LIFE_SECONDS {
        0
    } else {
        ((HALF_LIFE_SECONDS - age) * 100 / HALF_LIFE_SECONDS) as i32
    };
    (count.min(20) as i32 * decay / 10).min(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_scores_preserve_candidate_order_and_query_normalization() {
        let ranker = Ranker::in_memory();
        ranker.record_success("first", "Calc");
        ranker.record_success("second", "other");
        let scores = ranker.scores(["second", "missing", "first", "first"], "  CALC ");
        assert_eq!(scores[1], 0);
        assert!(scores[2] > scores[0]);
        assert_eq!(scores[2], scores[3]);
        assert_eq!(scores[2], ranker.score("first", "calc"));
        assert!(ranker.scores([], "calc").is_empty());
    }

    #[test]
    fn successful_use_increases_query_score() {
        let ranker = Ranker::in_memory();
        assert_eq!(ranker.score("app", "calc"), 0);
        ranker.record_success("app", "calc");
        assert!(ranker.score("app", "calc") > ranker.score("app", "other"));
    }

    #[test]
    fn queries_are_case_insensitive_and_trimmed() {
        let ranker = Ranker::in_memory();
        ranker.record_success("app", "  Calc ");
        assert!(ranker.score("app", "calc") > 0);
    }

    #[test]
    fn engine_search_history_is_deduplicated_ordered_and_selectable() {
        let ranker = Ranker::in_memory();
        ranker.record_engine_search_success("engine", "", "first query", 10);
        let first = ranker.recent_engine_searches("engine", 10);
        assert_eq!(first[0].query, "first query");

        ranker.record_engine_search_success("engine", "", "second query", 10);
        ranker.record_engine_search_success("engine", "", "FIRST QUERY", 10);
        let recent = ranker.recent_engine_searches("engine", 10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].query, "FIRST QUERY");
        assert_eq!(recent[1].query, "second query");

        ranker.record_engine_search_success("engine", &recent[1].result_id, "", 10);
        let selected = ranker.recent_engine_searches("engine", 10);
        assert_eq!(selected[0].query, "second query");
    }
}
