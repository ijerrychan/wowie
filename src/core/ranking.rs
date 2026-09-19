use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

const HALF_LIFE_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_BEHAVIOR_SCORE: i32 = 180;

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

    pub fn score(&self, item_id: &str, query: &str) -> i32 {
        let Ok(history) = self.history.lock() else {
            return 0;
        };
        let Some(record) = history.items.get(item_id) else {
            return 0;
        };
        let now = unix_seconds();
        let global = frecency(record.launches, record.last_used, now);
        let query = normalize_query(query);
        let query_score = record
            .query_launches
            .get(&query)
            .zip(record.query_last_used.get(&query))
            .map(|(count, last_used)| frecency(*count, *last_used, now) * 2)
            .unwrap_or(0);
        (global + query_score).min(MAX_BEHAVIOR_SCORE)
    }

    pub fn record_success(&self, item_id: &str, query: &str) {
        let Ok(mut history) = self.history.lock() else {
            return;
        };
        let now = unix_seconds();
        let query = normalize_query(query);
        let record = history.items.entry(item_id.to_owned()).or_default();
        record.launches = record.launches.saturating_add(1);
        record.last_used = now;
        if !query.is_empty() {
            *record.query_launches.entry(query.clone()).or_default() = record
                .query_launches
                .get(&query)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            record.query_last_used.insert(query, now);
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
            let json = serde_json::to_string_pretty(history)?;
            fs::write(path, format!("{json}\n"))?;
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
}
