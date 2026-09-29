use crate::result::SearchResult;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const HALF_LIFE_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_BEHAVIOR_SCORE: i32 = 180;
const WRITE_DEBOUNCE: Duration = Duration::from_millis(150);
const HISTORY_MAX_AGE: u64 = 90 * 24 * 60 * 60;
const MAX_HISTORY_ITEMS: usize = 2048;
const MAX_QUERIES_PER_ITEM: usize = 32;
const MAX_HISTORY_QUERIES: usize = 8192;
const COMPACT_EVERY_ACTIONS: u64 = 64;
pub(crate) const MAX_ENGINE_SEARCH_HISTORY: usize = 50;
pub(crate) const MAX_FAVORITES: usize = 256;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct UsageRecord {
    launches: u32,
    last_used: u64,
    query_launches: HashMap<String, u32>,
    query_last_used: HashMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    history: Mutex<HistoryFile>,
    writer: Option<HistoryWriter>,
    actions_since_compaction: AtomicU64,
    query_count: AtomicUsize,
}

#[derive(Default)]
struct WriterState {
    pending: Option<(u64, HistoryFile)>,
    queued: u64,
    written: u64,
    shutdown: bool,
}

struct HistoryWriter {
    state: Arc<(Mutex<WriterState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl HistoryWriter {
    fn new(path: PathBuf) -> Self {
        let state = Arc::new((Mutex::new(WriterState::default()), Condvar::new()));
        let worker_state = Arc::clone(&state);
        let thread = thread::Builder::new()
            .name("ranking-history-writer".into())
            .spawn(move || write_history_loop(&path, &worker_state))
            .expect("could not start ranking history writer");
        Self {
            state,
            thread: Some(thread),
        }
    }

    fn queue(&self, history: &HistoryFile) {
        let snapshot = history.clone();
        let (lock, wake) = &*self.state;
        if let Ok(mut state) = lock.lock() {
            state.queued = state.queued.saturating_add(1);
            state.pending = Some((state.queued, snapshot));
            wake.notify_one();
        }
    }

    fn flush(&self) {
        let (lock, wake) = &*self.state;
        if let Ok(state) = lock.lock() {
            let target = state.queued;
            drop(wake.wait_while(state, |state| state.written < target));
        }
    }
}

impl Drop for HistoryWriter {
    fn drop(&mut self) {
        let (lock, wake) = &*self.state;
        if let Ok(mut state) = lock.lock() {
            state.shutdown = true;
            wake.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn write_history_loop(path: &Path, shared: &Arc<(Mutex<WriterState>, Condvar)>) {
    let (lock, wake) = &**shared;
    loop {
        let Ok(mut state) = lock.lock() else { return };
        while state.pending.is_none() && !state.shutdown {
            let Ok(next) = wake.wait(state) else { return };
            state = next;
        }
        if state.pending.is_none() {
            return;
        }
        let deadline = Instant::now() + WRITE_DEBOUNCE;
        while !state.shutdown {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let Ok((next, _)) = wake.wait_timeout(state, remaining) else {
                return;
            };
            state = next;
        }
        let Some((generation, history)) = state.pending.take() else {
            continue;
        };
        drop(state);
        if let Err(error) = persist_history(path, &history) {
            eprintln!(
                "could not persist ranking history {}: {error:#}",
                path.display()
            );
        }
        let Ok(mut state) = lock.lock() else { return };
        state.written = generation;
        wake.notify_all();
    }
}

fn persist_history(path: &Path, history: &HistoryFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut json = serde_json::to_vec_pretty(history)?;
    json.push(b'\n');
    crate::utils::atomic_write(path, &json)?;
    Ok(())
}

impl Ranker {
    pub fn load() -> Self {
        let path = crate::config::history_path();
        let mut history: HistoryFile = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let changed = compact_behavior_history(&mut history.items, unix_seconds());
        let query_count = count_queries(&history.items);
        let ranker = Self {
            history: Mutex::new(history),
            writer: Some(HistoryWriter::new(path)),
            actions_since_compaction: AtomicU64::new(0),
            query_count: AtomicUsize::new(query_count),
        };
        if changed {
            if let Ok(history) = ranker.history.lock() {
                ranker.queue_save(&history);
            }
        }
        ranker
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            history: Mutex::new(HistoryFile::default()),
            writer: None,
            actions_since_compaction: AtomicU64::new(0),
            query_count: AtomicUsize::new(0),
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
            self.queue_save(&history);
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
        self.queue_save(&history);
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
        let mut new_query = false;
        if !query.is_empty() {
            new_query = !record.query_launches.contains_key(&query);
            let launches = record.query_launches.entry(query.clone()).or_default();
            *launches = launches.saturating_add(1);
            record.query_last_used.insert(query, now);
        }
        let query_count = if new_query {
            self.query_count.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            self.query_count.load(Ordering::Relaxed)
        };
        if let Some((engine_id, limit)) = engine {
            record_engine_search_in(&mut history, engine_id, item_id, raw_query, limit);
        }
        let actions = self
            .actions_since_compaction
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if actions >= COMPACT_EVERY_ACTIONS
            || history.items.len() > MAX_HISTORY_ITEMS
            || history.items[item_id].query_launches.len() > MAX_QUERIES_PER_ITEM
            || query_count > MAX_HISTORY_QUERIES
        {
            compact_behavior_history(&mut history.items, now);
            self.actions_since_compaction.store(0, Ordering::Relaxed);
            self.query_count
                .store(count_queries(&history.items), Ordering::Relaxed);
        }
        self.queue_save(&history);
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
        self.queue_save(&history);
    }

    fn queue_save(&self, history: &HistoryFile) {
        if let Some(writer) = &self.writer {
            writer.queue(history);
        }
    }

    pub fn flush(&self) {
        if let Some(writer) = &self.writer {
            writer.flush();
        }
    }
}

fn compact_behavior_history(items: &mut HashMap<String, UsageRecord>, now: u64) -> bool {
    let before = items.clone();
    let cutoff = now.saturating_sub(HISTORY_MAX_AGE);
    items.retain(|_, record| record.launches > 0 && record.last_used >= cutoff);

    for record in items.values_mut() {
        let mut queries: HashMap<String, (u32, u64)> = HashMap::new();
        for (query, launches) in record.query_launches.drain() {
            let normalized = normalize_query(&query);
            let last_used = record
                .query_last_used
                .get(&query)
                .copied()
                .unwrap_or(record.last_used);
            if normalized.is_empty() || launches == 0 || last_used < cutoff {
                continue;
            }
            let entry = queries.entry(normalized).or_default();
            entry.0 = entry.0.saturating_add(launches);
            entry.1 = entry.1.max(last_used);
        }
        let mut queries: Vec<_> = queries.into_iter().collect();
        queries.sort_unstable_by(|a, b| b.1 .1.cmp(&a.1 .1).then_with(|| a.0.cmp(&b.0)));
        queries.truncate(MAX_QUERIES_PER_ITEM);
        record.query_last_used.clear();
        for (query, (launches, last_used)) in queries {
            record.query_launches.insert(query.clone(), launches);
            record.query_last_used.insert(query, last_used);
        }
    }

    if items.len() > MAX_HISTORY_ITEMS {
        let mut recent: Vec<_> = items
            .iter()
            .map(|(id, record)| (id.clone(), record.last_used))
            .collect();
        recent.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let keep: std::collections::HashSet<_> = recent
            .into_iter()
            .take(MAX_HISTORY_ITEMS)
            .map(|(id, _)| id)
            .collect();
        items.retain(|id, _| keep.contains(id));
    }

    let query_count: usize = items
        .values()
        .map(|record| record.query_launches.len())
        .sum();
    if query_count > MAX_HISTORY_QUERIES {
        let mut recent = Vec::with_capacity(query_count);
        for (id, record) in items.iter() {
            for (query, last_used) in &record.query_last_used {
                recent.push((id.clone(), query.clone(), *last_used));
            }
        }
        recent.sort_unstable_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        for (id, query, _) in recent.into_iter().skip(MAX_HISTORY_QUERIES) {
            if let Some(record) = items.get_mut(&id) {
                record.query_launches.remove(&query);
                record.query_last_used.remove(&query);
            }
        }
    }
    *items != before
}

fn count_queries(items: &HashMap<String, UsageRecord>) -> usize {
    items
        .values()
        .map(|record| record.query_launches.len())
        .sum()
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

    #[test]
    fn writer_coalesces_snapshots_and_flushes_latest_on_drop() {
        let path = std::env::temp_dir().join(format!(
            "wowie-ranking-{}-{}.json",
            std::process::id(),
            unix_seconds()
        ));
        let ranker = Ranker {
            history: Mutex::new(HistoryFile::default()),
            writer: Some(HistoryWriter::new(path.clone())),
            actions_since_compaction: AtomicU64::new(0),
            query_count: AtomicUsize::new(0),
        };
        for _ in 0..99 {
            ranker.record_success("app", "Calc");
        }
        ranker.flush();
        ranker.record_success("app", "Calc");
        drop(ranker);
        let history: HistoryFile = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(history.items["app"].launches, 100);
        assert_eq!(history.items["app"].query_launches["calc"], 100);
        fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "repeatable performance benchmark; run with --ignored --nocapture --test-threads=1"]
    fn history_writer_benchmark() {
        let path = std::env::temp_dir().join(format!(
            "wowie-ranking-bench-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let writer = HistoryWriter::new(path.clone());
        let history = HistoryFile {
            items: (0..2_048)
                .map(|index| {
                    (
                        format!("app:{index}"),
                        UsageRecord {
                            launches: index,
                            last_used: unix_seconds(),
                            query_launches: HashMap::from([("calculator".into(), index)]),
                            query_last_used: HashMap::from([("calculator".into(), unix_seconds())]),
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        let ((mut queue_samples, flush_elapsed), peak_threads) =
            crate::perf_support::measure_peak_threads(|| {
                let mut queue_samples = Vec::with_capacity(50);
                for _ in 0..50 {
                    let started = Instant::now();
                    writer.queue(&history);
                    queue_samples.push(started.elapsed());
                }
                let started = Instant::now();
                writer.flush();
                (queue_samples, started.elapsed())
            });
        let (queue_p50, queue_p95) = crate::perf_support::percentiles(&mut queue_samples);
        let file_bytes = fs::metadata(&path).map_or(0, |metadata| metadata.len());
        let stats = crate::utils::background::stats();
        eprintln!(
            "[perf] history_writer records=2048 snapshots=50 queue_p50_us={} queue_p95_us={} flush_ms={} persisted_bytes={} process_peak_threads={} pending_tasks={} pending_peak={}",
            queue_p50.as_micros(),
            queue_p95.as_micros(),
            flush_elapsed.as_millis(),
            file_bytes,
            peak_threads,
            stats.pending,
            stats.peak_pending,
        );
        drop(writer);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn compaction_normalizes_queries_and_removes_empty_or_expired_entries() {
        let now = 1_000_000_000;
        let mut items = HashMap::from([
            (
                "app".to_owned(),
                UsageRecord {
                    launches: 4,
                    last_used: now,
                    query_launches: HashMap::from([
                        (" Calc ".into(), 2),
                        ("calc".into(), 1),
                        ("  ".into(), 1),
                        ("old".into(), 1),
                    ]),
                    query_last_used: HashMap::from([
                        (" Calc ".into(), now),
                        ("calc".into(), now - 1),
                        ("  ".into(), now),
                        ("old".into(), now - HISTORY_MAX_AGE - 1),
                    ]),
                },
            ),
            (
                "stale".to_owned(),
                UsageRecord {
                    launches: 1,
                    last_used: now - HISTORY_MAX_AGE - 1,
                    ..Default::default()
                },
            ),
        ]);
        assert!(compact_behavior_history(&mut items, now));
        assert!(!items.contains_key("stale"));
        assert_eq!(
            items["app"].query_launches,
            HashMap::from([("calc".into(), 3)])
        );
        assert_eq!(
            items["app"].query_last_used,
            HashMap::from([("calc".into(), now)])
        );
        assert!(!compact_behavior_history(&mut items, now));
    }

    #[test]
    fn compaction_bounds_many_random_queries_and_items() {
        let now = 1_000_000_000;
        let mut items = HashMap::new();
        for item in 0..MAX_HISTORY_ITEMS + 100 {
            let mut record = UsageRecord {
                launches: 1,
                last_used: now - (item as u64),
                ..Default::default()
            };
            for query in 0..MAX_QUERIES_PER_ITEM + 10 {
                let key = format!("query-{item}-{query}");
                record.query_launches.insert(key.clone(), 1);
                record.query_last_used.insert(key, now - (query as u64));
            }
            items.insert(format!("item-{item}"), record);
        }
        assert!(compact_behavior_history(&mut items, now));
        assert!(items.len() <= MAX_HISTORY_ITEMS);
        assert!(items
            .values()
            .all(|record| record.query_launches.len() <= MAX_QUERIES_PER_ITEM));
        assert!(
            items
                .values()
                .map(|record| record.query_launches.len())
                .sum::<usize>()
                <= MAX_HISTORY_QUERIES
        );
        assert!(items.contains_key("item-0"));
        assert!(!items.contains_key(&format!("item-{}", MAX_HISTORY_ITEMS + 99)));
    }
}
