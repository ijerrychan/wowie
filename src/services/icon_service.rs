//! Shared icon resolution and caching for all result providers.

use crate::{
    platform::IconData,
    result::{IconSpec, SearchResult},
};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Arc, Condvar, Mutex},
    time::UNIX_EPOCH,
};

// Keep decoded raster icons bounded, while rendering vector icons at the size
// requested by their UI surface.
const MAX_ICON_DIMENSION: u32 = 128;
const MAX_EXTERNAL_ICON_DIMENSION: u32 = 64;
const MAX_ICON_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REMOTE_ICON_BYTES: usize = 1024 * 1024;
const MAX_CACHED_ICON_BYTES: usize = 2 * 1024 * 1024;
const MAX_CACHED_SOURCE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) const DISPLAY_ICON_SIZE: u32 = 24;

#[derive(Clone, Default)]
pub struct IconService {
    cache: Arc<Mutex<IconCache>>,
    sources: Arc<Mutex<SourceCache>>,
    in_flight: Arc<Mutex<HashMap<String, Arc<InFlight>>>>,
}

#[derive(Default)]
struct IconCache {
    entries: HashMap<String, CachedIcon>,
    // The front is the least recently used key.
    lru: VecDeque<String>,
    bytes: usize,
}

struct CachedIcon {
    icon: Arc<IconData>,
    bytes: usize,
}

#[derive(Default)]
struct SourceCache {
    entries: HashMap<String, CachedSource>,
    lru: VecDeque<String>,
    bytes: usize,
}

struct CachedSource {
    bytes: Arc<Vec<u8>>,
    size: usize,
}

#[derive(Default)]
struct InFlight {
    result: Mutex<Option<Option<Arc<IconData>>>>,
    ready: Condvar,
}

impl IconService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve an icon off the UI thread and reuse successful lookups. Failed
    /// lookups are not retained because they carry no pixel data and would
    /// otherwise bypass the byte-based cache bound.
    pub fn resolve_at(&self, item: &SearchResult, size: u32) -> Option<Arc<IconData>> {
        let size = normalize_icon_size(size);
        let source_key = icon_source_key(item);
        let key = cache_key(&source_key, size, icon_source_is_size_independent(item));
        let sources = Arc::clone(&self.sources);
        self.resolve_cached(key, || resolve_uncached(item, size, &sources))
    }

    pub fn resolve_spec_at(&self, spec: &IconSpec, size: u32) -> Option<Arc<IconData>> {
        let size = normalize_icon_size(size);
        let key = cache_key(
            &icon_spec_key(spec),
            size,
            icon_spec_is_size_independent(spec),
        );
        let sources = Arc::clone(&self.sources);
        self.resolve_cached(key, || resolve_spec_with_cache(spec, size, &sources))
    }

    fn resolve_cached<F>(&self, key: String, resolve: F) -> Option<Arc<IconData>>
    where
        F: FnOnce() -> Option<IconData>,
    {
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(icon) = cache.get(&key) {
                return Some(icon);
            }
        }

        let in_flight = Arc::new(InFlight::default());
        let (in_flight, is_owner) = if let Ok(mut requests) = self.in_flight.lock() {
            if let Some(existing) = requests.get(&key) {
                (Arc::clone(existing), false)
            } else {
                requests.insert(key.clone(), Arc::clone(&in_flight));
                (in_flight, true)
            }
        } else {
            (in_flight, true)
        };

        if !is_owner {
            let mut result = in_flight.result.lock().ok()?;
            while result.is_none() {
                result = in_flight.ready.wait(result).ok()?;
            }
            return result.as_ref().cloned().flatten();
        }

        let result = resolve().map(Arc::new);
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(icon) = result.as_ref() {
                cache.insert(key.clone(), Arc::clone(icon));
            }
        }
        if is_owner {
            if let Ok(mut requests) = self.in_flight.lock() {
                requests.remove(&key);
            }
            if let Ok(mut state) = in_flight.result.lock() {
                *state = Some(result.clone());
                in_flight.ready.notify_all();
            }
        }
        result
    }
}

impl IconCache {
    fn get(&mut self, key: &str) -> Option<Arc<IconData>> {
        let icon = self.entries.get(key).map(|entry| Arc::clone(&entry.icon))?;
        self.touch(key);
        Some(icon)
    }

    fn insert(&mut self, key: String, icon: Arc<IconData>) {
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
            self.lru.retain(|existing| existing != &key);
        }

        let bytes = icon_bytes(&icon);
        self.bytes += bytes;
        self.lru.push_back(key.clone());
        self.entries.insert(key, CachedIcon { icon, bytes });

        while self.bytes > MAX_CACHED_ICON_BYTES {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(removed.bytes);
            }
        }
    }

    fn touch(&mut self, key: &str) {
        self.lru.retain(|existing| existing != key);
        self.lru.push_back(key.to_owned());
    }
}

fn icon_bytes(icon: &IconData) -> usize {
    icon.rgba.len()
}

impl SourceCache {
    fn get(&mut self, key: &str) -> Option<Arc<Vec<u8>>> {
        let bytes = self
            .entries
            .get(key)
            .map(|entry| Arc::clone(&entry.bytes))?;
        self.touch(key);
        Some(bytes)
    }

    fn insert(&mut self, key: String, bytes: Arc<Vec<u8>>) {
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.size);
            self.lru.retain(|existing| existing != &key);
        }

        let size = bytes.len();
        self.bytes += size;
        self.lru.push_back(key.clone());
        self.entries.insert(key, CachedSource { bytes, size });
        while self.bytes > MAX_CACHED_SOURCE_BYTES {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(removed.size);
            }
        }
    }

    fn touch(&mut self, key: &str) {
        self.lru.retain(|existing| existing != key);
        self.lru.push_back(key.to_owned());
    }
}

fn source_bytes<F>(cache: &Mutex<SourceCache>, key: String, load: F) -> Option<Arc<Vec<u8>>>
where
    F: FnOnce() -> Option<Vec<u8>>,
{
    if let Ok(mut cache) = cache.lock() {
        if let Some(bytes) = cache.get(&key) {
            return Some(bytes);
        }
    }
    let bytes = Arc::new(load()?);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, Arc::clone(&bytes));
    }
    Some(bytes)
}

fn resolve_uncached(
    item: &SearchResult,
    size: u32,
    sources: &Mutex<SourceCache>,
) -> Option<IconData> {
    if let Some(icon) = item
        .icon
        .as_ref()
        .and_then(|spec| resolve_spec_with_cache(spec, size, sources))
    {
        return Some(icon);
    }

    let icon_target = crate::providers::app::preferred_launch_entry(item)
        .map(|entry| &entry.target)
        .unwrap_or(&item.launch_target);

    if matches!(item.source, crate::platform::SourceId::Shortcut) {
        if let Some(icon) = fetch_favicon(&item.launch_target, size, sources) {
            return Some(icon);
        }
    }

    let icon = application_icon_with_path_lookup(icon_target).or_else(|| {
        item.launch_entries
            .iter()
            .map(|entry| &entry.path)
            .filter(|path| !crate::platform::windows_rules::is_shortcut_path(path))
            .find_map(|path| application_icon_with_path_lookup(path))
    });
    icon
}

fn application_icon_with_path_lookup(target: &Path) -> Option<IconData> {
    crate::platform::application_icon(target).or_else(|| {
        let name = target.file_name()?.to_str()?;
        if target
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            return None;
        }
        let search_path = std::env::var_os("PATH")?;
        std::env::split_paths(&search_path)
            .map(|directory| directory.join(name))
            .find(|path| path.is_file())
            .and_then(|path| crate::platform::application_icon(&path))
    })
}

fn fetch_favicon(target: &Path, size: u32, sources: &Mutex<SourceCache>) -> Option<IconData> {
    let origin = favicon_origin(target)?;
    let favicon = format!("{origin}/favicon.ico");
    let bytes = source_bytes(sources, format!("favicon:{origin}"), || {
        crate::utils::http::get_bytes(&favicon, 1_048_576).ok()
    })?;
    decode_image(&bytes, size)
}

fn decode_image(bytes: &[u8], size: u32) -> Option<IconData> {
    let image = image::load_from_memory(bytes).ok()?;
    let size = normalize_icon_size(size).min(MAX_EXTERNAL_ICON_DIMENSION);
    let image = image.thumbnail(size, size).to_rgba8();
    let (width, height) = image.dimensions();
    Some(IconData {
        width,
        height,
        rgba: image.into_raw(),
    })
}

fn resolve_spec_with_cache(
    spec: &IconSpec,
    size: u32,
    sources: &Mutex<SourceCache>,
) -> Option<IconData> {
    match spec {
        IconSpec::Calculator => named_icon("calculator", size),
        IconSpec::System(target) => crate::platform::application_icon(target),
        IconSpec::SystemIndexed { path, index } => {
            crate::platform::indexed_application_icon(path, *index)
        }
        IconSpec::DefaultOpen(target) => crate::platform::default_open_icon(target),
        IconSpec::Resource(path) => match path.extension().and_then(|extension| extension.to_str())
        {
            Some(extension) if extension.eq_ignore_ascii_case("svg") => {
                load_svg_icon(path, size, sources)
            }
            _ => load_png_icon(path, size, sources),
        },
        IconSpec::Named(name) => named_icon(name, size),
        IconSpec::Remote(url) => {
            let bytes = source_bytes(sources, format!("remote:{url}"), || {
                fetch_remote_icon_bytes(url)
            })?;
            if url.to_ascii_lowercase().ends_with(".svg")
                || bytes.iter().take(256).any(|byte| *byte == b'<')
            {
                load_svg_bytes(&bytes, size.min(MAX_EXTERNAL_ICON_DIMENSION))
            } else {
                decode_image(&bytes, size)
            }
        }
    }
}

fn fetch_remote_icon_bytes(url: &str) -> Option<Vec<u8>> {
    crate::utils::http::get_bytes(url, MAX_REMOTE_ICON_BYTES).ok()
}

#[cfg(test)]
fn resolve_spec(spec: &IconSpec, size: u32) -> Option<IconData> {
    let sources = Mutex::new(SourceCache::default());
    resolve_spec_with_cache(spec, size, &sources)
}

fn named_icon(name: &str, size: u32) -> Option<IconData> {
    match name {
        "weather" => load_svg_bytes(include_bytes!("../../assets/icons/weather.svg"), size),
        "calculator" => load_svg_bytes(include_bytes!("../../assets/icons/calculator.svg"), size),
        "book" | "wowie" => {
            load_svg_bytes(include_bytes!("../../assets/icons/book-open.svg"), size)
        }
        "checklist" => load_svg_bytes(include_bytes!("../../assets/icons/list-checks.svg"), size),
        "folder-open" => load_svg_bytes(
            include_bytes!("../../assets/icons/folder-open-outline.svg"),
            size,
        ),
        "shield-account" => load_svg_bytes(
            include_bytes!("../../assets/icons/shield-account-outline.svg"),
            size,
        ),
        "console-line" => {
            load_svg_bytes(include_bytes!("../../assets/icons/console-line.svg"), size)
        }
        _ => None,
    }
}

fn load_png_icon(path: &Path, size: u32, sources: &Mutex<SourceCache>) -> Option<IconData> {
    decode_image(&read_icon_bytes(path, sources)?, size)
}

fn load_svg_icon(path: &Path, size: u32, sources: &Mutex<SourceCache>) -> Option<IconData> {
    load_svg_bytes(
        &read_icon_bytes(path, sources)?,
        size.min(MAX_EXTERNAL_ICON_DIMENSION),
    )
}

fn read_icon_bytes(path: &Path, sources: &Mutex<SourceCache>) -> Option<Arc<Vec<u8>>> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_ICON_FILE_BYTES {
        return None;
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos().to_string())
        .unwrap_or_default();
    let key = format!(
        "file:{}:{}:{modified}",
        stable_path_key(path),
        metadata.len()
    );
    let path = path.to_owned();
    source_bytes(sources, key, move || std::fs::read(path).ok())
}

fn load_svg_bytes(bytes: &[u8], size: u32) -> Option<IconData> {
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let source = tree.size();
    let size = normalize_icon_size(size);
    let scale = (size as f32 / source.width()).min(size as f32 / source.height());
    let width = (source.width() * scale).round().max(1.0) as u32;
    let height = (source.height() * scale).round().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let offset_x = (size - width) as f32 / 2.0;
    let offset_y = (size - height) as f32 / 2.0;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale).post_translate(offset_x, offset_y),
        &mut pixmap.as_mut(),
    );
    Some(IconData {
        width: size,
        height: size,
        rgba: pixmap.take(),
    })
}

fn normalize_icon_size(size: u32) -> u32 {
    size.clamp(1, MAX_ICON_DIMENSION)
}

fn cache_key(source: &str, size: u32, size_independent: bool) -> String {
    if size_independent {
        source.to_owned()
    } else {
        format!("{source}@{size}")
    }
}

fn icon_source_key(item: &SearchResult) -> String {
    if let Some(icon) = item.icon.as_ref() {
        return icon_spec_key(icon);
    }

    if matches!(item.source, crate::platform::SourceId::Shortcut) {
        if let Some(origin) = favicon_origin(&item.launch_target) {
            return format!("favicon:{origin}");
        }
    }

    let target = crate::providers::app::preferred_launch_entry(item)
        .map(|entry| path_cache_key(&entry.target))
        .unwrap_or_else(|| path_cache_key(&item.launch_target));
    let candidates = item
        .launch_entries
        .iter()
        .filter(|entry| !crate::platform::windows_rules::is_shortcut_path(&entry.path))
        .map(|entry| path_cache_key(&entry.path))
        .collect::<Vec<_>>()
        .join("|");
    format!("fallback:{target}|{candidates}")
}

fn icon_spec_key(icon: &IconSpec) -> String {
    match icon {
        IconSpec::System(path) | IconSpec::Resource(path) => {
            format!("explicit:{}", path_cache_key(path))
        }
        IconSpec::SystemIndexed { path, index } => {
            format!("explicit:{}#{}", path_cache_key(path), index)
        }
        IconSpec::DefaultOpen(path) => {
            let value = path.to_string_lossy();
            let extension = if value.starts_with("http://") || value.starts_with("https://") {
                "web".to_owned()
            } else {
                path.extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.to_lowercase())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| value.to_lowercase())
            };
            format!("default-open:{extension}")
        }
        IconSpec::Calculator => "calculator".into(),
        IconSpec::Named(name) => format!("named:{name}"),
        IconSpec::Remote(url) => format!("remote:{url}"),
    }
}

fn icon_spec_is_size_independent(icon: &IconSpec) -> bool {
    matches!(
        icon,
        IconSpec::System(_) | IconSpec::SystemIndexed { .. } | IconSpec::DefaultOpen(_)
    )
}

fn icon_source_is_size_independent(item: &SearchResult) -> bool {
    match item.icon.as_ref() {
        Some(icon) => icon_spec_is_size_independent(icon),
        None => {
            !matches!(item.source, crate::platform::SourceId::Shortcut)
                || favicon_origin(&item.launch_target).is_none()
        }
    }
}

fn stable_path_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        value.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        value
    }
}

fn path_cache_key(path: &Path) -> String {
    let revision = std::fs::metadata(path)
        .ok()
        .map(|metadata| {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_nanos().to_string())
                .unwrap_or_default();
            format!("{}:{modified}", metadata.len())
        })
        .unwrap_or_else(|| "missing".into());
    format!("{}:{revision}", stable_path_key(path))
}

fn favicon_origin(target: &Path) -> Option<String> {
    let value = target.to_str()?;
    let parsed = url::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.to_lowercase();
    let port = parsed.port_or_known_default().filter(|port| {
        (*port != 80 || parsed.scheme() != "http") && (*port != 443 || parsed.scheme() != "https")
    });
    Some(match port {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{ResultCategory, SearchResult};
    use std::path::PathBuf;

    #[test]
    fn source_key_is_stable_for_same_result() {
        let item = SearchResult {
            id: "game:steam:10".into(),
            identity: "game:steam:10".into(),
            title: "Game".into(),
            category: ResultCategory::Game,
            aliases: vec![],
            location: "steam://rungameid/10".into(),
            launch_target: PathBuf::from("steam://rungameid/10"),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "Steam".into(),
            sources: vec!["Steam".into()],
            launch_entries: vec![],
            actions: vec![],
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: None,
            ranking_bias: 0,
            visibility: crate::result::ResultVisibility::Always,
        };
        assert_eq!(icon_source_key(&item), icon_source_key(&item));
    }

    #[test]
    fn source_key_reuses_the_same_explicit_icon() {
        let mut first = test_result("one");
        let mut second = test_result("two");
        let icon = IconSpec::System(PathBuf::from("https://example.com"));
        first.icon = Some(icon.clone());
        second.icon = Some(icon);
        assert_eq!(icon_source_key(&first), icon_source_key(&second));
    }

    #[test]
    fn different_system_targets_do_not_share_an_icon_cache_key() {
        let mut first = test_result("one");
        let mut second = test_result("two");
        first.icon = Some(IconSpec::System(PathBuf::from(r"C:\Apps\one.exe")));
        second.icon = Some(IconSpec::System(PathBuf::from(r"C:\Apps\two.exe")));
        assert_ne!(icon_source_key(&first), icon_source_key(&second));
    }

    #[test]
    fn fixed_platform_icons_share_one_cache_key_across_sizes() {
        for spec in [
            IconSpec::System(PathBuf::from("C:\\Apps\\demo.exe")),
            IconSpec::SystemIndexed {
                path: PathBuf::from("C:\\Apps\\demo.exe"),
                index: 1,
            },
            IconSpec::DefaultOpen(PathBuf::from("demo.txt")),
        ] {
            let source = icon_spec_key(&spec);
            assert_eq!(
                cache_key(&source, 16, icon_spec_is_size_independent(&spec)),
                cache_key(&source, 40, icon_spec_is_size_independent(&spec))
            );
        }
    }

    #[test]
    fn source_cache_reuses_loaded_bytes() {
        let cache = Mutex::new(SourceCache::default());
        let first = source_bytes(&cache, "test".into(), || Some(vec![1, 2, 3])).unwrap();
        let second = source_bytes(&cache, "test".into(), || Some(vec![9])).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.as_ref(), &[1, 2, 3]);
    }

    #[test]
    fn named_plugin_icons_are_resolvable() {
        for name in ["book", "checklist", "wowie"] {
            assert!(resolve_spec(&IconSpec::Named(name.into()), 24).is_some());
        }
    }

    #[test]
    fn bundled_svg_icons_render_to_icon_data() {
        for spec in [IconSpec::Calculator] {
            let icon = resolve_spec(&spec, 40).unwrap();
            assert_eq!((icon.width, icon.height), (40, 40));
            assert!(icon.rgba.iter().any(|channel| *channel != 0));
        }
    }

    #[test]
    fn service_reuses_explicit_icon_results() {
        let service = IconService::new();
        let first = service
            .resolve_spec_at(&IconSpec::Calculator, 24)
            .expect("first icon");
        let second = service
            .resolve_spec_at(&IconSpec::Calculator, 24)
            .expect("cached icon");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn service_keeps_size_specific_icon_results() {
        let service = IconService::new();
        let small = service
            .resolve_spec_at(&IconSpec::Calculator, 16)
            .expect("small icon");
        let large = service
            .resolve_spec_at(&IconSpec::Calculator, 40)
            .expect("large icon");
        let small_again = service
            .resolve_spec_at(&IconSpec::Calculator, 16)
            .expect("cached small icon");

        assert_eq!((small.width, small.height), (16, 16));
        assert_eq!((large.width, large.height), (40, 40));
        assert!(Arc::ptr_eq(&small, &small_again));
        assert!(!Arc::ptr_eq(&small, &large));
    }

    #[test]
    fn source_key_reuses_default_open_icon_by_extension() {
        let mut first = test_result("one");
        let mut second = test_result("two");
        first.icon = Some(IconSpec::DefaultOpen(PathBuf::from("one.txt")));
        second.icon = Some(IconSpec::DefaultOpen(PathBuf::from("two.TXT")));
        assert_eq!(icon_source_key(&first), icon_source_key(&second));
    }

    #[test]
    fn default_open_web_icon_does_not_use_favicon_cache_key() {
        let mut item = test_result("web");
        item.source = "Shortcut".into();
        item.icon = Some(IconSpec::DefaultOpen(PathBuf::from(
            "https://untrusted.example",
        )));
        assert_eq!(icon_source_key(&item), "default-open:web");
    }

    #[test]
    fn favicon_cache_key_ignores_query_parameters() {
        let mut first = test_result("one");
        let mut second = test_result("two");
        first.source = "Shortcut".into();
        second.source = "Shortcut".into();
        first.launch_target = PathBuf::from("https://example.com/search?q=one");
        second.launch_target = PathBuf::from("https://example.com/search?q=two");
        assert_eq!(icon_source_key(&first), icon_source_key(&second));
    }

    fn test_result(id: &str) -> SearchResult {
        SearchResult {
            id: id.into(),
            identity: id.into(),
            title: "Test".into(),
            category: ResultCategory::Website,
            aliases: vec![],
            location: "https://example.com".into(),
            launch_target: PathBuf::from("https://example.com"),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: "Test".into(),
            sources: vec!["Test".into()],
            launch_entries: vec![],
            actions: vec![],
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: None,
            ranking_bias: 0,
            visibility: crate::result::ResultVisibility::Always,
        }
    }
}
