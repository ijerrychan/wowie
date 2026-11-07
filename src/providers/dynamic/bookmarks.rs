//! Asynchronously cached Chromium-family and Firefox bookmark search.
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
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_FIREFOX_DB: u64 = 256 * 1024 * 1024;
const MAX_FIREFOX_WAL: u64 = 64 * 1024 * 1024;
const MAX_BOOKMARKS: usize = 5_000;
const MAX_PROFILES: usize = 32;
const MAX_DEPTH: usize = 64;
const REFRESH: Duration = Duration::from_secs(300);
#[derive(Clone, PartialEq, Eq)]
struct Bookmark {
    title: String,
    url: String,
    folder: String,
    browser_profile: String,
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
    scan_completed: bool,
    scan_failed: bool,
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

pub struct BookmarksProvider {
    notifier: Option<Arc<AtomicU64>>,
    language: String,
}
pub fn register(
    registry: &mut ProviderRegistry,
    _: Vec<crate::config::ShortcutConfig>,
    _: crate::config::ShortcutKeywords,
) {
    let notifier = registry.register_search_engine(BookmarksProvider {
        notifier: None,
        language: "system".into(),
    });
    request_refresh(notifier);
}
impl SearchEngineProvider for BookmarksProvider {
    fn settings(&self) -> SearchEngineSettings {
        SearchEngineSettings::default()
    }
    fn set_content_notifier(&mut self, notifier: Arc<AtomicU64>) {
        self.notifier = Some(notifier);
    }
    fn search_engine(&self, query: &str) -> Option<crate::page::SearchEngineSession> {
        let argument = query.trim_start().strip_prefix("bm ")?.trim();
        Some(crate::page::SearchEngineSession {
            id: "bookmarks".into(),
            title: crate::utils::i18n::tr(&self.language, "Browser bookmarks").into(),
            placeholder: crate::utils::i18n::tr(&self.language, "Search browser bookmarks").into(),
            prefix: "bm ".into(),
            argument: argument.into(),
            mode: crate::page::SearchEngineMode::Search,
            state: std::collections::BTreeMap::new(),
        })
    }
    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        if query.trim().is_empty() {
            return vec![entry_result(&self.language)];
        }
        request_refresh(self.notifier.clone().unwrap_or_default());
        let snapshot = CACHE
            .get_or_init(|| Arc::new(Mutex::new(Cache::default())))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone();
        snapshot.iter().map(result).collect()
    }
    fn owns_session(&self, session: &crate::page::SearchEngineSession) -> bool {
        session.id == "bookmarks"
    }
    fn query_active(
        &self,
        session: &crate::page::SearchEngineSession,
        _query: &str,
    ) -> Vec<SearchResult> {
        if !self.owns_session(session) {
            return Vec::new();
        }
        request_refresh(self.notifier.clone().unwrap_or_default());
        let snapshot = CACHE
            .get_or_init(|| Arc::new(Mutex::new(Cache::default())))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone();
        snapshot.iter().map(result).collect()
    }
    fn search_engine_result(
        &self,
        session: &crate::page::SearchEngineSession,
    ) -> Option<SearchResult> {
        self.owns_session(session)
            .then(|| entry_result(&self.language))
    }
    fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        (result_id == "bookmarks:entry").then(|| "bm ".into())
    }
    fn empty_state(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
    ) -> Option<SearchResult> {
        let session = context.session()?;
        if !self.owns_session(session) {
            return None;
        }
        let c = CACHE
            .get_or_init(|| Arc::new(Mutex::new(Cache::default())))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let description = if !c.scan_completed {
            "Loading browser bookmarks"
        } else if c.snapshot.is_empty() && c.scan_failed {
            "Could not read browser bookmarks"
        } else if !query.trim().is_empty() {
            "No matching browser bookmarks"
        } else {
            "No browser bookmarks found"
        };
        Some(empty_result(&self.language, description))
    }
    fn set_language(&mut self, language: String) {
        self.language = language;
    }
}

fn request_refresh(notifier: Arc<AtomicU64>) {
    let cache = CACHE
        .get_or_init(|| Arc::new(Mutex::new(Cache::default())))
        .clone();
    let state = cache.lock().unwrap_or_else(|e| e.into_inner());
    if state
        .last_attempt
        .map(|t| t.elapsed() < REFRESH)
        .unwrap_or(false)
        || QUEUED.swap(true, Ordering::AcqRel)
    {
        return;
    }
    drop(state);
    if !crate::utils::background::spawn(move || {
        {
            let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
            c.last_attempt = Some(Instant::now());
            c.scan_completed = false;
            c.scan_failed = false;
        }
        refresh_profiles(&cache, &notifier);
        finish_scan(&cache, &notifier);
        QUEUED.store(false, Ordering::Release);
    }) {
        QUEUED.store(false, Ordering::Release);
    }
}

fn finish_scan(cache: &Arc<Mutex<Cache>>, notifier: &Arc<AtomicU64>) {
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    let changed = !c.scan_completed;
    c.scan_completed = true;
    drop(c);
    if changed {
        notifier.fetch_add(1, Ordering::AcqRel);
    }
}

fn refresh_profiles(cache: &Arc<Mutex<Cache>>, notifier: &Arc<AtomicU64>) {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        cache.lock().unwrap_or_else(|e| e.into_inner()).scan_failed = true;
        return;
    };
    let mut any_success = false;
    let base = PathBuf::from(local);
    for (root, label) in chromium_roots(&base) {
        let Ok(entries) = std::fs::read_dir(&root) else {
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
                break;
            }
            let now = Instant::now();
            drop(c);
            if refresh_file(cache, &file, now, &format!("{label} · {name}"), notifier).is_ok() {
                any_success = true;
            }
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        for (file, label) in firefox_profiles(&PathBuf::from(appdata)) {
            if refresh_firefox(cache, &file, &label, notifier).is_ok() {
                any_success = true;
            }
        }
    }
    if !any_success {
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        c.scan_failed = true;
    }
}
fn chromium_roots(base: &std::path::Path) -> Vec<(PathBuf, &'static str)> {
    vec![
        (base.join("Google/Chrome/User Data"), "Chrome"),
        (base.join("Microsoft/Edge/User Data"), "Edge"),
        (base.join("BraveSoftware/Brave-Browser/User Data"), "Brave"),
        (base.join("Vivaldi/User Data"), "Vivaldi"),
        (base.join("Chromium/User Data"), "Chromium"),
    ]
}
fn firefox_profiles(appdata: &std::path::Path) -> Vec<(PathBuf, String)> {
    let root = appdata.join("Mozilla/Firefox");
    let ini = root.join("profiles.ini");
    let mut profiles = Vec::new();
    let content = std::fs::File::open(&ini)
        .ok()
        .and_then(|f| crate::utils::read_bounded(f, 256 * 1024).ok())
        .and_then(|b| String::from_utf8(b).ok());
    if let Some(content) = content {
        let mut section = String::new();
        let mut values = std::collections::BTreeMap::new();
        let flush = |section: &str,
                     values: &std::collections::BTreeMap<String, String>,
                     profiles: &mut Vec<(PathBuf, String)>| {
            if !section.starts_with("Profile") {
                return;
            }
            if let Some(path) = values.get("Path") {
                let relative = values.get("IsRelative").map(|v| v != "0").unwrap_or(true);
                let profile = if relative {
                    root.join(path)
                } else {
                    PathBuf::from(path)
                };
                if profiles.len() < MAX_PROFILES {
                    profiles.push((
                        profile,
                        format!(
                            "Firefox · {}",
                            path.rsplit(['\\', '/']).next().unwrap_or("Profile")
                        ),
                    ));
                }
            }
        };
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                flush(&section, &values, &mut profiles);
                section = line[1..line.len() - 1].into();
                values.clear();
            } else if let Some((k, v)) = line.split_once('=') {
                values.insert(k.into(), v.into());
            }
        }
        flush(&section, &values, &mut profiles);
    }
    if profiles.is_empty() {
        if let Ok(entries) = std::fs::read_dir(root.join("Profiles")) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let p = entry.path();
                    let name = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    if profiles.len() < MAX_PROFILES {
                        profiles.push((p, format!("Firefox · {name}")));
                    }
                }
            }
        }
    }
    profiles
        .into_iter()
        .map(|(p, label)| (p.join("places.sqlite"), label))
        .collect()
}
fn refresh_firefox(
    cache: &Arc<Mutex<Cache>>,
    file: &std::path::Path,
    label: &str,
    _notifier: &Arc<AtomicU64>,
) -> Result<bool, ()> {
    let now = Instant::now();
    let budget = {
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        if c.profiles.len() >= MAX_PROFILES && !c.profiles.contains_key(file) {
            return Err(());
        }
        let p = c.profiles.entry(file.to_path_buf()).or_default();
        if p.last_attempt
            .map(|t| now.duration_since(t) < REFRESH)
            .unwrap_or(false)
        {
            return Err(());
        }
        p.last_attempt = Some(now);
        MAX_BOOKMARKS.saturating_sub(
            c.profiles
                .iter()
                .filter(|(p, _)| p.as_path() != file)
                .map(|(_, v)| v.items.len())
                .sum::<usize>(),
        )
    };
    let items = read_firefox(file, budget, label)?;
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    let before = c.snapshot.clone();
    c.profiles.get_mut(file).unwrap().items = items;
    rebuild(&mut c);
    let changed = before.as_ref() != c.snapshot.as_ref();
    drop(c);
    Ok(changed)
}
static TEMP_ID: AtomicU64 = AtomicU64::new(1);
struct TempDirectory(PathBuf);
impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn copy_bounded(
    source: &std::path::Path,
    destination: &std::path::Path,
    limit: u64,
) -> Result<(), ()> {
    use std::io::Read;
    let mut input = std::fs::File::open(source).map_err(|_| ())?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| ())?;
    let copied = std::io::copy(
        &mut input.by_ref().take(limit.saturating_add(1)),
        &mut output,
    )
    .map_err(|_| ())?;
    if copied > limit {
        let _ = std::fs::remove_file(destination);
        return Err(());
    }
    Ok(())
}
fn read_firefox(file: &std::path::Path, budget: usize, label: &str) -> Result<Vec<Bookmark>, ()> {
    let main_size = std::fs::metadata(file).map_err(|_| ())?.len();
    if main_size > MAX_FIREFOX_DB {
        return Err(());
    }
    let temp = std::env::temp_dir().join(format!(
        "wowie-firefox-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&temp).map_err(|_| ())?;
    let _temp_guard = TempDirectory(temp.clone());
    let result = (|| {
        for (suffix, limit) in [("", MAX_FIREFOX_DB), ("-wal", MAX_FIREFOX_WAL)] {
            let mut source_name = file.as_os_str().to_os_string();
            source_name.push(suffix);
            let src = PathBuf::from(source_name);
            if !src.exists() {
                continue;
            }
            let meta = std::fs::metadata(&src).map_err(|_| ())?;
            if meta.len() > limit {
                return Err(());
            }
            copy_bounded(&src, &temp.join(format!("places.sqlite{suffix}")), limit)?;
        }
        let mut db = rusqlite::Connection::open_with_flags(
            temp.join("places.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(|_| ())?;
        db.busy_timeout(Duration::from_millis(80)).map_err(|_| ())?;
        db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 32 * 1024)
            .map_err(|_| ())?;
        let deadline = Instant::now() + Duration::from_millis(750);
        db.progress_handler(5000, Some(move || Instant::now() >= deadline))
            .map_err(|_| ())?;
        db.pragma_update(None, "query_only", true).map_err(|_| ())?;
        let mut folders = std::collections::HashMap::<i64, (i64, String)>::new();
        {
            let mut stmt = db
                .prepare("SELECT id, parent, title FROM moz_bookmarks WHERE type=2 LIMIT ?1")
                .map_err(|_| ())?;
            let rows = stmt
                .query_map([MAX_BOOKMARKS as i64], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    ))
                })
                .map_err(|_| ())?;
            for row in rows {
                let row = row.map_err(|_| ())?;
                let (id, parent, title) = row;
                folders.insert(id, (parent, bounded(&title, 4096)));
            }
        }
        let mut out = Vec::new();
        if budget > 0 {
            let mut stmt=db.prepare("SELECT b.title, p.url, b.parent FROM moz_bookmarks b JOIN moz_places p ON p.id=b.fk WHERE b.type=1 AND (p.url LIKE 'http://%' OR p.url LIKE 'https://%') LIMIT ?1").map_err(|_|())?;
            let rows = stmt
                .query_map([budget as i64], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    ))
                })
                .map_err(|_| ())?;
            for row in rows {
                let (title, url, parent) = row.map_err(|_| ())?;
                if url.len() > 16 * 1024 {
                    continue;
                }
                let Ok(parsed) = url::Url::parse(&url) else {
                    continue;
                };
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
                    continue;
                }
                let mut names = Vec::new();
                let mut cursor = parent;
                for _ in 0..MAX_DEPTH {
                    let Some((up, name)) = folders.get(&cursor) else {
                        break;
                    };
                    if !name.is_empty() {
                        names.push(name.clone());
                    }
                    cursor = *up;
                }
                names.reverse();
                out.push(Bookmark {
                    title: bounded(&title, 4096),
                    url,
                    folder: bounded(&names.join(" / "), 4096),
                    browser_profile: label.into(),
                });
            }
        }
        Ok(out)
    })();
    result
}
fn refresh_file(
    cache: &Arc<Mutex<Cache>>,
    file: &std::path::Path,
    now: Instant,
    label: &str,
    _notifier: &Arc<AtomicU64>,
) -> Result<bool, ()> {
    let budget = {
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        let profile = c.profiles.entry(file.to_path_buf()).or_default();
        if profile
            .last_attempt
            .map(|t| now.duration_since(t) < REFRESH)
            .unwrap_or(false)
        {
            return Err(());
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
        .map(|root| parse_root(root, budget, label));
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    let Some(items) = loaded else {
        return Err(());
    };
    let before = c.snapshot.clone();
    c.profiles.get_mut(file).expect("profile inserted").items = items;
    rebuild(&mut c);
    let changed = before.as_ref() != c.snapshot.as_ref();
    drop(c);
    Ok(changed)
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
fn parse_root(doc: Root, limit: usize, label: &str) -> Vec<Bookmark> {
    let mut out = Vec::new();
    let limit = limit.min(MAX_BOOKMARKS);
    for node in [doc.roots.bar, doc.roots.other, doc.roots.synced] {
        let mut path = Vec::new();
        walk(&node, &mut path, &mut out, 0, limit, label);
        if out.len() >= limit {
            break;
        }
    }
    out
}
fn walk(
    node: &Node,
    path: &mut Vec<String>,
    out: &mut Vec<Bookmark>,
    depth: usize,
    limit: usize,
    label: &str,
) {
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
                            browser_profile: label.into(),
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
        walk(n, path, out, depth + 1, limit, label);
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
fn bookmark_location(b: &Bookmark) -> String {
    let host = url::Url::parse(&b.url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    format!("{} — {} — {}", b.browser_profile, b.folder, host)
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
        title: if b.title.trim().is_empty() {
            url::Url::parse(&b.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| b.url.clone())
        } else {
            b.title.clone()
        },
        category: ResultCategory::Bookmark,
        aliases: Vec::new(),
        metadata: crate::result::SearchMetadata {
            keywords: url::Url::parse(&b.url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .into_iter()
                .collect(),
            context: {
                let mut values = vec![b.folder.clone(), b.browser_profile.clone()];
                if let Ok(url) = url::Url::parse(&b.url) {
                    if url.path() != "/" && !url.path().is_empty() {
                        values.push(url.path().to_owned());
                    }
                }
                values.into_iter().filter(|value| !value.trim().is_empty()).collect()
            },
            technical: {
                let mut values = vec![b.url.clone()];
                if let Some(host) = url::Url::parse(&b.url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned))
                {
                    values.push(host);
                }
                values
            },
        },
        location: bookmark_location(b),
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
        icon: Some(IconSpec::Named("bookmark".into())),
        ranking_bias: 0,
        allow_favorite: true,
        visibility: crate::result::ResultVisibility::Always,
    }
}

fn entry_result(language: &str) -> SearchResult {
    let id = "bookmarks:entry".to_string();
    let source = SourceId::Custom("Bookmarks".into());
    SearchResult {
        id: id.clone(),
        identity: id,
        title: crate::utils::i18n::tr(language, "Browser bookmarks").into(),
        category: ResultCategory::Other,
        metadata: Default::default(),
        aliases: vec!["bm".into()],
        location: crate::utils::i18n::tr(language, "Bookmarks from your browsers").into(),
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: source.clone(),
        sources: vec![source],
        launch_entries: Vec::new(),
        actions: vec![Action::EnterSearchEngine {
            prefix: "bm ".into(),
        }],
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: Some(IconSpec::Named("bookmark".into())),
        ranking_bias: 0,
        allow_favorite: false,
        visibility: crate::result::ResultVisibility::EmptyQueryOnly,
    }
}

fn empty_result(language: &str, description: &'static str) -> SearchResult {
    let entry = entry_result(language);
    SearchResult {
        id: "bookmarks:empty-state".into(),
        identity: "bookmarks:empty-state".into(),
        title: entry.title,
        category: ResultCategory::Other,
        metadata: Default::default(),
        aliases: Vec::new(),
        location: entry.location,
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: entry.source,
        sources: entry.sources,
        launch_entries: Vec::new(),
        actions: Vec::new(),
        detail: None,
        description: Some(crate::utils::i18n::tr(language, description).into()),
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: entry.icon,
        ranking_bias: 0,
        allow_favorite: false,
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
        let v = parse_root(d, MAX_BOOKMARKS, "Chrome · Default");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].folder, "Dev");
        assert_eq!(v[0].url, "https://example.com/Path?q=Case");
        let r = result(&v[0]);
        assert_eq!(r.category, ResultCategory::Bookmark);
        assert_eq!(r.launch_target, PathBuf::from(&v[0].url));
        assert!(matches!(r.actions[0], Action::Launch(_)));
    }
    #[test]
    fn missing_synced_root_and_damaged_json_are_safe() {
        assert!(serde_json::from_str::<Root>("{broken").is_err());
        let d = doc(r#"{"children":[{"type":"url","name":"site","url":"http://example.com"}]}"#);
        assert_eq!(parse_root(d, MAX_BOOKMARKS, "Edge · Default").len(), 1);
    }
    #[test]
    fn empty_root_has_engine_entry_and_only_entity_results_are_bookmarks() {
        let provider = BookmarksProvider {
            notifier: Some(Arc::new(AtomicU64::new(0))),
            language: "en".into(),
        };
        let entry = provider.query_root("").pop().unwrap();
        assert_eq!(entry.category, ResultCategory::Other);
        assert!(
            matches!(entry.actions.as_slice(),[Action::EnterSearchEngine{prefix}] if prefix=="bm ")
        );
        assert_eq!(provider.shortcut_hint(&entry.id).as_deref(), Some("bm "));
        let doc =
            doc(r#"{"children":[{"type":"url","name":"  ","url":"https://example.com/path"}]}"#);
        let item = result(&parse_root(doc, 1, "Firefox Default")[0]);
        assert_eq!(item.category, ResultCategory::Bookmark);
        assert_eq!(item.title, "example.com");
        assert!(item.location.contains("example.com"));
        assert_eq!(provider.shortcut_hint(&item.id), None);
        let session = provider.search_engine(" bm rust ").unwrap();
        assert_eq!(session.argument, "rust");
        assert!(provider.owns_session(&session));
    }
    #[test]
    fn completed_async_scan_notifies_once_after_status_changes() {
        let cache = Arc::new(Mutex::new(Cache::default()));
        let notifier = Arc::new(AtomicU64::new(0));
        finish_scan(&cache, &notifier);
        assert_eq!(notifier.load(Ordering::Acquire), 1);
        finish_scan(&cache, &notifier);
        assert_eq!(notifier.load(Ordering::Acquire), 1);
    }
    #[test]
    fn firefox_profiles_ini_resolves_relative_and_absolute_paths() {
        let base = std::env::temp_dir().join(format!("wowie-firefox-ini-{}", std::process::id()));
        let root = base.join("Mozilla/Firefox");
        std::fs::create_dir_all(&root).unwrap();
        let absolute = base.join("external/profile");
        let data=format!("[Profile0]\nName=default\nIsRelative=1\nPath=Profiles/abc.default\n[Profile1]\nIsRelative=0\nPath={}\n",absolute.display());
        std::fs::write(root.join("profiles.ini"), data).unwrap();
        let profiles = firefox_profiles(&base);
        assert_eq!(profiles.len(), 2);
        assert!(profiles
            .iter()
            .any(|(p, _)| p == &root.join("Profiles/abc.default/places.sqlite")));
        assert!(profiles
            .iter()
            .any(|(p, _)| p == &absolute.join("places.sqlite")));
        let _ = std::fs::remove_dir_all(base);
    }
    #[test]
    fn firefox_loader_reads_bookmarks_from_database_and_wal_copy() {
        let dir =
            std::env::temp_dir().join(format!("wowie-firefox-fixture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dbpath = dir.join("places.sqlite");
        let db = rusqlite::Connection::open(&dbpath).unwrap();
        db.pragma_update(None, "journal_mode", "WAL").unwrap();
        db.execute_batch("CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT);CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY,parent INTEGER,type INTEGER,fk INTEGER,title TEXT);INSERT INTO moz_places VALUES(1,'https://example.com/Path');INSERT INTO moz_bookmarks VALUES(2,1,2,NULL,'Folder');INSERT INTO moz_bookmarks VALUES(3,2,1,1,'Fixture');").unwrap();
        let wal = PathBuf::from(format!("{}-wal", dbpath.display()));
        assert!(wal.exists());
        let items = read_firefox(&dbpath, MAX_BOOKMARKS, "Firefox test").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Fixture");
        assert_eq!(items[0].folder, "Folder");
        assert_eq!(items[0].url, "https://example.com/Path");
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "reads the current Firefox profile read-only via bounded temporary copies"]
    fn diagnose_real_firefox_bookmarks_without_private_content() {
        let Some(appdata) = std::env::var_os("APPDATA") else {
            println!("Firefox profiles: 0; bookmarks: 0 (skipped: APPDATA missing)");
            return;
        };
        let profiles = firefox_profiles(&PathBuf::from(appdata));
        let mut count = 0;
        let mut remaining = MAX_BOOKMARKS;
        for (file, label) in &profiles {
            if remaining == 0 {
                break;
            }
            if let Ok(rows) = read_firefox(file, remaining, label) {
                count += rows.len();
                remaining = remaining.saturating_sub(rows.len());
            }
        }
        if profiles.is_empty() {
            println!("Firefox profiles: 0; bookmarks: 0 (skipped: no profiles)");
        } else {
            println!(
                "Firefox profiles: {}; HTTP(S) bookmarks loaded: {}",
                profiles.len(),
                count
            );
        }
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
                    browser_profile: "Chrome Default".into(),
                }],
                last_attempt: Some(Instant::now() - REFRESH),
            },
        );
        rebuild(&mut c);
        let shared = Arc::new(Mutex::new(c));
        std::fs::write(&path, b"broken json").unwrap();
        let notifier = Arc::new(AtomicU64::new(0));
        assert!(refresh_file(&shared, &path, Instant::now(), "Firefox · test", &notifier).is_err());
        assert_eq!(shared.lock().unwrap().snapshot[0].title, "x");
        std::fs::write(
            &path,
            br#"{"roots":{"bookmark_bar":{"children":[]},"other":{"children":[]}}}"#,
        )
        .unwrap();
        assert!(refresh_file(
            &shared,
            &path,
            Instant::now() + REFRESH,
            "Firefox · test",
            &notifier
        )
        .unwrap());
        assert!(shared.lock().unwrap().snapshot.is_empty());
        let _ = std::fs::remove_file(path);
    }
}
