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
const MAX_DETAIL_IMAGE_DIMENSION: u32 = 256;
const MAX_DECODED_PIXELS: u64 = 16 * 1024 * 1024;
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

type IconCache = ByteCache<IconData, MAX_CACHED_ICON_BYTES>;
type SourceCache = ByteCache<Vec<u8>, MAX_CACHED_SOURCE_BYTES>;

struct ByteCache<T, const LIMIT: usize> {
    entries: HashMap<Arc<str>, CacheEntry<T>>,
    // Access stamps make a hit append-only. Stale stamps are skipped during
    // eviction and compacted periodically to keep metadata bounded.
    lru: VecDeque<(u64, Arc<str>)>,
    bytes: usize,
    access_clock: u64,
}

struct CacheEntry<T> {
    value: Arc<T>,
    bytes: usize,
    access: u64,
}

impl<T, const LIMIT: usize> Default for ByteCache<T, LIMIT> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            bytes: 0,
            access_clock: 0,
        }
    }
}

trait CacheSize {
    fn cache_bytes(&self) -> usize;
}

impl CacheSize for IconData {
    fn cache_bytes(&self) -> usize {
        self.rgba.len()
    }
}

impl CacheSize for Vec<u8> {
    fn cache_bytes(&self) -> usize {
        self.len()
    }
}

#[derive(Default)]
struct InFlight {
    result: Mutex<Option<Option<Arc<IconData>>>>,
    ready: Condvar,
}

impl InFlight {
    fn wait(&self) -> Option<Arc<IconData>> {
        let mut result = self.result.lock().ok()?;
        while result.is_none() {
            result = self.ready.wait(result).ok()?;
        }
        result.as_ref().cloned().flatten()
    }
}

/// Always release waiters, including when a decoder unwinds.
struct FlightOwner<'a> {
    service: &'a IconService,
    key: &'a str,
    request: Arc<InFlight>,
    result: Option<Arc<IconData>>,
}

impl Drop for FlightOwner<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.request.result.lock() {
            *state = Some(self.result.clone());
            self.request.ready.notify_all();
        }
        if let Ok(mut requests) = self.service.in_flight.lock() {
            requests.remove(self.key);
        }
    }
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

    /// Resolve a plugin-owned detail image with its own display size. Keep
    /// filesystem access and image decoding off the UI thread.
    pub fn resolve_detail_image(&self, path: &Path) -> Option<Arc<IconData>> {
        let key = format!("detail-image:{}", path_cache_key(path));
        let sources = Arc::clone(&self.sources);
        self.resolve_cached(key, || {
            let bytes = read_icon_bytes(path, &sources)?;
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
            {
                load_svg_bytes(&bytes, MAX_DETAIL_IMAGE_DIMENSION)
            } else {
                decode_image_at(&bytes, MAX_DETAIL_IMAGE_DIMENSION)
            }
        })
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
            return in_flight.wait();
        }
        let mut owner = FlightOwner {
            service: self,
            key: &key,
            request: in_flight,
            result: None,
        };

        let cached = self.cache.lock().ok().and_then(|mut cache| cache.get(&key));
        let result = cached.or_else(|| resolve().map(Arc::new));
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(icon) = result.as_ref() {
                cache.insert(key.clone(), Arc::clone(icon));
            }
        }
        owner.result = result.clone();
        result
    }
}

impl<T: CacheSize, const LIMIT: usize> ByteCache<T, LIMIT> {
    fn get(&mut self, key: &str) -> Option<Arc<T>> {
        let access = self.next_access();
        let (stored_key, entry) = self.entries.get_key_value(key)?;
        let stored_key = Arc::clone(stored_key);
        let value = Arc::clone(&entry.value);
        self.entries.get_mut(key)?.access = access;
        self.lru.push_back((access, stored_key));
        self.compact_if_needed();
        Some(value)
    }

    fn insert(&mut self, key: String, value: Arc<T>) {
        if let Some(entry) = self.entries.remove(key.as_str()) {
            self.bytes -= entry.bytes;
        }
        let size = value.cache_bytes();
        // Do not let oversized or empty entries evict useful data or bypass
        // the byte budget with an unbounded number of zero-byte keys.
        if size == 0 || size > LIMIT {
            return;
        }
        while self.bytes > LIMIT - size {
            let Some((access, oldest)) = self.lru.pop_front() else {
                break;
            };
            if self
                .entries
                .get(oldest.as_ref())
                .is_some_and(|entry| entry.access == access)
            {
                if let Some(removed) = self.entries.remove(oldest.as_ref()) {
                    self.bytes -= removed.bytes;
                }
            }
        }
        let access = self.next_access();
        let key: Arc<str> = Arc::from(key);
        self.bytes += size;
        self.lru.push_back((access, Arc::clone(&key)));
        self.entries.insert(
            key,
            CacheEntry {
                value,
                bytes: size,
                access,
            },
        );
        self.compact_if_needed();
    }

    fn next_access(&mut self) -> u64 {
        if self.access_clock == u64::MAX {
            self.compact_lru();
            let mut entries = self
                .entries
                .iter()
                .map(|(key, entry)| (entry.access, Arc::clone(key)))
                .collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(access, _)| *access);
            self.lru.clear();
            for (index, (_, key)) in entries.into_iter().enumerate() {
                let access = index as u64 + 1;
                if let Some(entry) = self.entries.get_mut(key.as_ref()) {
                    entry.access = access;
                }
                self.lru.push_back((access, key));
            }
            self.access_clock = self.entries.len() as u64;
        }
        self.access_clock += 1;
        self.access_clock
    }

    fn compact_if_needed(&mut self) {
        let limit = self.entries.len().saturating_mul(2).saturating_add(64);
        if self.lru.len() > limit {
            self.compact_lru();
        }
    }

    fn compact_lru(&mut self) {
        self.lru.retain(|(access, key)| {
            self.entries
                .get(key.as_ref())
                .is_some_and(|entry| entry.access == *access)
        });
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

    if item.category == crate::result::ResultCategory::Settings {
        return crate::platform::application_icon(Path::new(
            crate::providers::windows_settings::SETTINGS_ICON_TARGET,
        ))
        .or_else(|| named_icon("settings", size));
    }

    if item.category == crate::result::ResultCategory::ControlPanel {
        // A failed resource extraction must still try the original Shell item
        // or console before falling back to a category icon.
        return item
            .launch_entries
            .iter()
            .filter_map(|entry| entry.icon.as_ref())
            .filter(|spec| Some(*spec) != item.icon.as_ref())
            .find_map(|spec| resolve_spec_with_cache(spec, size, sources))
            .or_else(|| crate::platform::application_icon(&item.launch_target))
            .or_else(|| crate::platform::application_icon(Path::new("shell:ControlPanelFolder")))
            .or_else(|| named_icon("control-panel", size));
    }

    if item.category == crate::result::ResultCategory::Window {
        return named_icon("window", size);
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
    decode_image_at(
        bytes,
        normalize_icon_size(size).min(MAX_EXTERNAL_ICON_DIMENSION),
    )
}

fn decode_image_at(bytes: &[u8], size: u32) -> Option<IconData> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let (source_width, source_height) = reader.into_dimensions().ok()?;
    if source_width == 0
        || source_height == 0
        || u64::from(source_width) * u64::from(source_height) > MAX_DECODED_PIXELS
    {
        return None;
    }
    let size = size.min(source_width.max(source_height));
    let image = image::load_from_memory(bytes).ok()?;
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
        "settings" => load_svg_bytes(include_bytes!("../../assets/icons/settings.svg"), size),
        "control-panel" => {
            load_svg_bytes(include_bytes!("../../assets/icons/control-panel.svg"), size)
        }
        "weather" => load_svg_bytes(include_bytes!("../../assets/icons/weather.svg"), size),
        "calculator" => load_svg_bytes(include_bytes!("../../assets/icons/calculator.svg"), size),
        "book" | "wowie" => {
            load_svg_bytes(include_bytes!("../../assets/icons/book-open.svg"), size)
        }
        "bookmark" => load_svg_bytes(
            include_bytes!("../../assets/icons/bookmark-outline.svg"),
            size,
        ),
        "window" => load_svg_bytes(include_bytes!("../../assets/icons/window.svg"), size),
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
    source_bytes(sources, key, move || {
        crate::utils::read_bounded(
            std::fs::File::open(path).ok()?,
            MAX_ICON_FILE_BYTES as usize,
        )
        .ok()
    })
}

fn load_svg_bytes(bytes: &[u8], size: u32) -> Option<IconData> {
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let source = tree.size();
    let size = size.clamp(1, MAX_DETAIL_IMAGE_DIMENSION);
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
        IconSpec::System(path) => format!("system:{}", path_cache_key(path)),
        IconSpec::Resource(path) => format!("resource:{}", path_cache_key(path)),
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
    #[ignore = "repeatable performance benchmark; run with --ignored --nocapture --test-threads=1"]
    fn icon_cache_hit_miss_and_lru_benchmark() {
        use std::time::Instant;

        let service = IconService::new();
        let spec = IconSpec::Named("book".into());
        let warm = service.resolve_spec_at(&spec, 24).unwrap();
        let mut lru = ByteCache::<Vec<u8>, MAX_CACHED_ICON_BYTES>::default();
        let keys = (0..800)
            .map(|index| format!("icon-{index}"))
            .collect::<Vec<_>>();
        for key in &keys {
            lru.insert(key.clone(), Arc::new(vec![0; 2048]));
        }
        let (
            (mut hit_samples, mut miss_samples, mut lru_samples, evictions, mut failure_samples),
            peak_threads,
        ) = crate::perf_support::measure_peak_threads(|| {
            let mut hit_samples = Vec::with_capacity(500);
            for _ in 0..500 {
                let started = Instant::now();
                let icon = service.resolve_spec_at(&spec, 24).unwrap();
                assert!(Arc::ptr_eq(&warm, &icon));
                hit_samples.push(started.elapsed());
            }
            let mut miss_samples = Vec::with_capacity(20);
            for _ in 0..20 {
                let miss_service = IconService::new();
                let started = Instant::now();
                assert!(miss_service.resolve_spec_at(&spec, 24).is_some());
                miss_samples.push(started.elapsed());
            }
            let mut lru_samples = Vec::with_capacity(2_000);
            for index in 0..2_000 {
                let started = Instant::now();
                assert!(lru.get(&keys[index % keys.len()]).is_some());
                lru_samples.push(started.elapsed());
            }
            let entries_before_eviction = lru.entries.len();
            lru.insert("eviction-probe".into(), Arc::new(vec![0; 512 * 1024]));
            let evictions = entries_before_eviction + 1 - lru.entries.len();

            let missing = std::env::temp_dir().join(format!(
                "wowie-missing-icon-{}-{}.png",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let failed = IconService::new();
            let mut failure_samples = Vec::with_capacity(5);
            for _ in 0..5 {
                let started = Instant::now();
                assert!(failed
                    .resolve_spec_at(&IconSpec::Resource(missing.clone()), 24)
                    .is_none());
                failure_samples.push(started.elapsed());
            }
            (
                hit_samples,
                miss_samples,
                lru_samples,
                evictions,
                failure_samples,
            )
        });
        let (hit_p50, hit_p95) = crate::perf_support::percentiles(&mut hit_samples);
        let (miss_p50, miss_p95) = crate::perf_support::percentiles(&mut miss_samples);
        let (lru_p50, lru_p95) = crate::perf_support::percentiles(&mut lru_samples);
        let (failure_p50, failure_p95) = crate::perf_support::percentiles(&mut failure_samples);
        let icon_cache_bytes = service
            .cache
            .lock()
            .map(|cache| cache.bytes)
            .unwrap_or_default();
        let source_cache_bytes = service
            .sources
            .lock()
            .map(|cache| cache.bytes)
            .unwrap_or_default();
        let stats = crate::utils::background::stats();
        eprintln!(
            "[perf] icon_cache cache_hits=500 cache_misses=20 hit_rate_percent={:.1} hit_p50_us={} hit_p95_us={} miss_p50_us={} miss_p95_us={} lru_entries={} lru_bytes={} lru_hit_p50_us={} lru_hit_p95_us={} eviction_count={} failure_attempts=5 failure_retries=4 failure_p50_us={} failure_p95_us={} failure_cache_entries=0 decoded_cache_bytes={} source_cache_bytes={} process_peak_threads={} pending_tasks={} pending_peak={}",
            500.0_f64 / 520.0 * 100.0,
            hit_p50.as_micros(),
            hit_p95.as_micros(),
            miss_p50.as_micros(),
            miss_p95.as_micros(),
            lru.entries.len(),
            lru.bytes,
            lru_p50.as_micros(),
            lru_p95.as_micros(),
            evictions,
            failure_p50.as_micros(),
            failure_p95.as_micros(),
            icon_cache_bytes,
            source_cache_bytes,
            peak_threads,
            stats.pending,
            stats.peak_pending,
        );
    }

    #[test]
    fn detail_images_are_decoded_once_and_oversized_files_are_rejected() {
        let directory = std::env::temp_dir().join(format!(
            "wowie-detail-image-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let image_path = directory.join("preview.png");
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            3,
            2,
            image::Rgba([255, 0, 0, 255]),
        ))
        .save(&image_path)
        .unwrap();
        let service = IconService::new();
        let first = service.resolve_detail_image(&image_path).unwrap();
        let second = service.resolve_detail_image(&image_path).unwrap();
        assert_eq!((first.width, first.height), (3, 2));
        assert!(Arc::ptr_eq(&first, &second));

        let oversized = directory.join("oversized.png");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MAX_ICON_FILE_BYTES + 1).unwrap();
        drop(file);
        assert!(service.resolve_detail_image(&oversized).is_none());
        std::fs::remove_file(image_path).unwrap();
        std::fs::remove_file(oversized).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn panicking_icon_loader_releases_waiters_and_allows_retry() {
        use std::{sync::mpsc, time::Duration};
        let service = IconService::new();
        let worker_service = service.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            worker_service.resolve_cached("panic".into(), || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                panic!("simulated decoder failure");
            });
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let request = Arc::clone(service.in_flight.lock().unwrap().get("panic").unwrap());
        let (finished_tx, finished_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            finished_tx.send(request.wait().is_none()).unwrap();
        });
        release_tx.send(()).unwrap();
        assert!(worker.join().is_err());
        assert!(finished_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        waiter.join().unwrap();
        assert!(service.in_flight.lock().unwrap().is_empty());
        assert!(service
            .resolve_cached("panic".into(), || named_icon("book", 24))
            .is_some());
    }

    #[test]
    fn byte_cache_evicts_least_recently_used_and_bounds_empty_entries() {
        let mut cache = ByteCache::<Vec<u8>, 4>::default();
        cache.insert("a".into(), Arc::new(vec![1; 2]));
        cache.insert("b".into(), Arc::new(vec![2; 2]));
        assert!(cache.get("a").is_some());
        cache.insert("c".into(), Arc::new(vec![3; 2]));
        assert!(cache.get("b").is_none());
        cache.insert("large".into(), Arc::new(vec![0; 5]));
        cache.insert("empty".into(), Arc::new(vec![]));
        assert_eq!(cache.entries.len(), 2);
        cache.insert("a".into(), Arc::new(vec![4]));
        assert_eq!(cache.bytes, 3);
        assert_eq!(cache.get("a").unwrap().as_slice(), &[4]);
    }

    #[test]
    fn resource_and_system_decoders_have_distinct_cache_keys() {
        let path = PathBuf::from("test.png");
        assert_ne!(
            icon_spec_key(&IconSpec::System(path.clone())),
            icon_spec_key(&IconSpec::Resource(path))
        );
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
    fn bundled_icons_render_visible_pixels() {
        for name in [
            "book",
            "bookmark",
            "checklist",
            "wowie",
            "settings",
            "control-panel",
            "calculator",
            "window",
        ] {
            let icon = resolve_spec(&IconSpec::Named(name.into()), 24).unwrap();
            assert_eq!((icon.width, icon.height), (24, 24), "{name}");
            assert!(
                icon.rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0),
                "{name}"
            );
        }
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
            metadata: Default::default(),
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
            allow_favorite: true,
            visibility: crate::result::ResultVisibility::Always,
        }
    }
}
