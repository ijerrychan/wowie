//! Asynchronously cached Chrome and Edge bookmark search.
use crate::{
    platform::SourceId,
    provider::{ProviderRegistry, SearchEngineProvider, SearchEngineSettings},
    result::{Action, IconSpec, LaunchEntry, ResultCategory, SearchResult},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_BOOKMARKS: usize = 5_000;
const MAX_PROFILES: usize = 32;
const MAX_DEPTH: usize = 64;
const REFRESH: Duration = Duration::from_secs(300);
#[derive(Clone)]
struct Bookmark {
    title: String,
    url: String,
    folder: String,
}
#[derive(Default)]
struct ProfileCache {
    items: Vec<Bookmark>,
    last_attempt: Option<Instant>,
}
#[derive(Default)]
struct Cache {
    profiles: BTreeMap<PathBuf, ProfileCache>,
    snapshot: Arc<Vec<Bookmark>>,
    last_attempt: Option<Instant>,
}
static CACHE: OnceLock<Arc<Mutex<Cache>>> = OnceLock::new();
static QUEUED: AtomicBool = AtomicBool::new(false);
#[derive(Deserialize)]
struct Root {
    roots: Roots,
}
#[derive(Deserialize)]
struct Roots {
    #[serde(rename = "bookmark_bar", default)]
    bar: Node,
    #[serde(default)]
    other: Node,
    #[serde(default)]
    synced: Node,
}
#[derive(Deserialize, Default)]
struct Node {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    children: Vec<Node>,
    #[serde(rename = "type", default)]
    kind: String,
}

pub struct BookmarksProvider;
pub fn register(
    registry: &mut ProviderRegistry,
    _: Vec<crate::config::ShortcutConfig>,
    _: crate::config::ShortcutKeywords,
) {
    registry.register_search_engine(BookmarksProvider);
    request_refresh();
}
impl SearchEngineProvider for BookmarksProvider {
    fn settings(&self) -> SearchEngineSettings {
        SearchEngineSettings::default()
    }
    fn search_engine(&self, _: &str) -> Option<crate::page::SearchEngineSession> {
        None
    }
    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        request_refresh();
        let snapshot = CACHE
            .get_or_init(|| Arc::new(Mutex::new(Cache::default())))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone();
        snapshot.iter().map(result).collect()
    }
}

fn request_refresh() {
    let cache = CACHE
        .get_or_init(|| Arc::new(Mutex::new(Cache::default())))
        .clone();
    let mut state = cache.lock().unwrap_or_else(|e| e.into_inner());
    if state
        .last_attempt
        .map(|t| t.elapsed() < REFRESH)
        .unwrap_or(false)
        || QUEUED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    state.last_attempt = Some(Instant::now());
    drop(state);
    if !crate::utils::background::spawn(move || {
        refresh_profiles(&cache);
        QUEUED.store(false, Ordering::Release);
    }) {
        QUEUED.store(false, Ordering::Release);
    }
}

fn refresh_profiles(cache: &Arc<Mutex<Cache>>) {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let base = PathBuf::from(local);
    for browser in [
        base.join("Google/Chrome/User Data"),
        base.join("Microsoft/Edge/User Data"),
    ] {
        let Ok(entries) = std::fs::read_dir(browser) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name != "Default" && !name.starts_with("Profile ") {
                continue;
            }
            let file = entry.path().join("Bookmarks");
            let c = cache.lock().unwrap_or_else(|e| e.into_inner());
            if !c.profiles.contains_key(&file) && c.profiles.len() >= MAX_PROFILES {
                continue;
            }
            let now = Instant::now();
            drop(c);
            refresh_file(cache, &file, now);
        }
    }
}
fn refresh_file(cache: &Arc<Mutex<Cache>>, file: &std::path::Path, now: Instant) -> bool {
    let budget = {
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        let profile = c.profiles.entry(file.to_path_buf()).or_default();
        if profile
            .last_attempt
            .map(|t| now.duration_since(t) < REFRESH)
            .unwrap_or(false)
        {
            return false;
        }
        profile.last_attempt = Some(now);
        MAX_BOOKMARKS.saturating_sub(
            c.profiles
                .iter()
                .filter(|(path, _)| path.as_path() != file)
                .map(|(_, p)| p.items.len())
                .sum::<usize>(),
        )
    };
    let loaded = std::fs::metadata(file)
        .ok()
        .filter(|m| m.len() <= MAX_FILE)
        .and_then(|_| std::fs::File::open(file).ok())
        .and_then(|f| crate::utils::read_bounded(f, MAX_FILE as usize).ok())
        .and_then(|b| serde_json::from_slice::<Root>(&b).ok())
        .map(|root| parse_root(root, budget));
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    let Some(items) = loaded else {
        return false;
    };
    c.profiles.get_mut(file).expect("profile inserted").items = items;
    rebuild(&mut c);
    true
}
fn rebuild(c: &mut Cache) {
    let mut seen = HashSet::new();
    let mut all = Vec::new();
    for p in c.profiles.values() {
        for b in &p.items {
            if seen.insert(url_identity(&b.url)) {
                all.push(b.clone());
                if all.len() == MAX_BOOKMARKS {
                    break;
                }
            }
        }
        if all.len() == MAX_BOOKMARKS {
            break;
        }
    }
    c.snapshot = Arc::new(all);
}
fn url_identity(s: &str) -> String {
    url::Url::parse(s)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| s.to_owned())
}
fn parse_root(doc: Root, limit: usize) -> Vec<Bookmark> {
    let mut out = Vec::new();
    let limit = limit.min(MAX_BOOKMARKS);
    for node in [doc.roots.bar, doc.roots.other, doc.roots.synced] {
        let mut path = Vec::new();
        walk(&node, &mut path, &mut out, 0, limit);
        if out.len() >= limit {
            break;
        }
    }
    out
}
fn walk(node: &Node, path: &mut Vec<String>, out: &mut Vec<Bookmark>, depth: usize, limit: usize) {
    if depth > MAX_DEPTH || out.len() >= limit {
        return;
    }
    if node.kind == "url" {
        if let Some(raw) = &node.url {
            if raw.len() <= 16 * 1024 {
                if let Ok(u) = url::Url::parse(raw) {
                    if matches!(u.scheme(), "http" | "https") && u.host().is_some() {
                        out.push(Bookmark {
                            title: bounded(&node.name, 4096),
                            url: raw.clone(),
                            folder: bounded(&path.join(" / "), 4096),
                        });
                    }
                }
            }
        }
        return;
    }
    if !node.name.is_empty() {
        path.push(bounded(&node.name, 4096));
    }
    for n in &node.children {
        walk(n, path, out, depth + 1, limit);
        if out.len() >= limit {
            break;
        }
    }
    if !node.name.is_empty() {
        path.pop();
    }
}
fn bounded(value: &str, max: usize) -> String {
    let mut end = value.len().min(max);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
fn result(b: &Bookmark) -> SearchResult {
    let identity = url_identity(&b.url);
    let id = format!("bookmark:{identity}");
    let target = PathBuf::from(&b.url);
    let source = SourceId::Custom("Bookmarks".into());
    let entry = LaunchEntry {
        source: source.clone(),
        path: PathBuf::new(),
        target: target.clone(),
        arguments: String::new(),
        working_directory: None,
        icon: None,
    };
    SearchResult {
        id: id.clone(),
        identity: id,
        title: b.title.clone(),
        category: ResultCategory::Website,
        aliases: vec![b.url.clone(), b.folder.clone()],
        location: b.folder.clone(),
        launch_target: target.clone(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: source.clone(),
        sources: vec![source],
        launch_entries: vec![entry.clone()],
        actions: vec![Action::Launch(entry)],
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: Some(IconSpec::DefaultOpen(target)),
        ranking_bias: 0,
        allow_favorite: true,
        visibility: crate::result::ResultVisibility::Always,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc(bar: &str) -> Root {
        serde_json::from_str(&format!(
            r#"{{"roots":{{"bookmark_bar":{bar},"other":{{"children":[]}}}}}}"#
        ))
        .unwrap()
    }
    #[test]
    fn parses_folders_validates_schemes_and_preserves_url_case() {
        let d = doc(
            r#"{"children":[{"type":"folder","name":"Dev","children":[{"type":"url","name":"Rust","url":"https://example.com/Path?q=Case"},{"type":"url","name":"bad","url":"javascript:alert(1)"}]}]}"#,
        );
        let v = parse_root(d, MAX_BOOKMARKS);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].folder, "Dev");
        assert_eq!(v[0].url, "https://example.com/Path?q=Case");
        let r = result(&v[0]);
        assert_eq!(r.category, ResultCategory::Website);
        assert_eq!(r.launch_target, PathBuf::from(&v[0].url));
        assert!(matches!(r.actions[0], Action::Launch(_)));
    }
    #[test]
    fn missing_synced_root_and_damaged_json_are_safe() {
        assert!(serde_json::from_str::<Root>("{broken").is_err());
        let d = doc(r#"{"children":[{"type":"url","name":"site","url":"http://example.com"}]}"#);
        assert_eq!(parse_root(d, MAX_BOOKMARKS).len(), 1);
    }
    #[test]
    fn url_identity_canonicalizes_host_but_keeps_path_case_and_fragment() {
        assert_eq!(
            url_identity("HTTPS://E.TEST/a"),
            url_identity("https://e.test/a")
        );
        assert_ne!(
            url_identity("https://e.test/A"),
            url_identity("https://e.test/a")
        );
        assert_ne!(
            url_identity("https://e.test/a#one"),
            url_identity("https://e.test/a#two")
        );
    }
    #[test]
    fn refresh_failure_keeps_snapshot_and_successful_empty_file_removes_entries() {
        let path =
            std::env::temp_dir().join(format!("wowie-bookmarks-{}.json", std::process::id()));
        let mut c = Cache::default();
        c.profiles.insert(
            path.clone(),
            ProfileCache {
                items: vec![Bookmark {
                    title: "x".into(),
                    url: "https://e.test".into(),
                    folder: String::new(),
                }],
                last_attempt: Some(Instant::now() - REFRESH),
            },
        );
        rebuild(&mut c);
        let shared = Arc::new(Mutex::new(c));
        std::fs::write(&path, b"broken json").unwrap();
        assert!(!refresh_file(&shared, &path, Instant::now()));
        assert_eq!(shared.lock().unwrap().snapshot[0].title, "x");
        std::fs::write(
            &path,
            br#"{"roots":{"bookmark_bar":{"children":[]},"other":{"children":[]}}}"#,
        )
        .unwrap();
        assert!(refresh_file(&shared, &path, Instant::now() + REFRESH));
        assert!(shared.lock().unwrap().snapshot.is_empty());
        let _ = std::fs::remove_file(path);
    }
}
