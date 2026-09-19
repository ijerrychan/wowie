//! Shared icon resolution and caching for all result providers.

use crate::{
    platform::IconData,
    result::{IconSpec, SearchResult},
};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
};

// Result rows render icons at 24px. Keep up to a 2x source for HiDPI displays,
// but never enlarge a low-resolution icon before it reaches the renderer.
const MAX_ICON_DIMENSION: u32 = 64;
const MAX_ICON_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CACHED_ICON_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct IconService {
    cache: Arc<Mutex<IconCache>>,
}

#[derive(Default)]
struct IconCache {
    entries: HashMap<String, CachedIcon>,
    // The front is the least recently used key.
    lru: VecDeque<String>,
    bytes: usize,
}

struct CachedIcon {
    icon: Option<Arc<IconData>>,
    bytes: usize,
}

impl IconService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve an icon off the UI thread and reuse successful lookups. Failed
    /// lookups are not retained because they carry no pixel data and would
    /// otherwise bypass the byte-based cache bound.
    pub fn resolve(&self, item: &SearchResult) -> Option<Arc<IconData>> {
        let source_key = icon_source_key(item);
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(icon) = cache.get(&source_key) {
                return icon;
            }
        }

        let icon = resolve_uncached(item).map(Arc::new);
        if let Some(icon) = icon.as_ref() {
            if let Ok(mut cache) = self.cache.lock() {
                cache.insert(source_key, Some(Arc::clone(icon)));
            }
        }
        icon
    }

    pub fn resolve_spec(&self, spec: &IconSpec) -> Option<Arc<IconData>> {
        resolve_spec(spec).map(Arc::new)
    }
}

impl IconCache {
    fn get(&mut self, key: &str) -> Option<Option<Arc<IconData>>> {
        let icon = self.entries.get(key).map(|entry| entry.icon.clone())?;
        self.touch(key);
        Some(icon)
    }

    fn insert(&mut self, key: String, icon: Option<Arc<IconData>>) {
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
            self.lru.retain(|existing| existing != &key);
        }

        let bytes = icon.as_ref().map_or(0, |icon| icon_bytes(icon));
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

fn resolve_uncached(item: &SearchResult) -> Option<IconData> {
    if let Some(icon) = item.icon.as_ref().and_then(resolve_spec) {
        return Some(icon);
    }

    let icon_target = crate::providers::app::preferred_launch_entry(item)
        .map(|entry| &entry.target)
        .unwrap_or(&item.launch_target);

    if matches!(item.source, crate::platform::SourceId::Shortcut) {
        if let Some(icon) = fetch_favicon(&item.launch_target) {
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

fn fetch_favicon(target: &Path) -> Option<IconData> {
    let origin = favicon_origin(target)?;
    let favicon = format!("{origin}/favicon.ico");
    let bytes = crate::http::get_bytes(&favicon, 1_048_576).ok()?;
    decode_image(&bytes)
}

fn decode_image(bytes: &[u8]) -> Option<IconData> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = image
        .thumbnail(MAX_ICON_DIMENSION, MAX_ICON_DIMENSION)
        .to_rgba8();
    let (width, height) = image.dimensions();
    Some(IconData {
        width,
        height,
        rgba: image.into_raw(),
    })
}

fn resolve_spec(spec: &IconSpec) -> Option<IconData> {
    match spec {
        IconSpec::Weather => Some(weather_icon()),
        IconSpec::Calculator => Some(calculator_icon()),
        IconSpec::System(target) => crate::platform::application_icon(target),
        IconSpec::SystemIndexed { path, index } => {
            crate::platform::indexed_application_icon(path, *index)
        }
        IconSpec::DefaultOpen(target) => crate::platform::default_open_icon(target),
        IconSpec::Resource(path) => load_png_icon(path),
        IconSpec::Named(name) => match name.as_str() {
            "weather" => Some(weather_icon()),
            "calculator" => Some(calculator_icon()),
            _ => None,
        },
    }
}

fn load_png_icon(path: &Path) -> Option<IconData> {
    if std::fs::metadata(path).ok()?.len() > MAX_ICON_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    decode_image(&bytes)
}

fn icon_source_key(item: &SearchResult) -> String {
    if let Some(icon) = item.icon.as_ref() {
        return match icon {
            IconSpec::System(path) | IconSpec::Resource(path) => {
                format!("explicit:{}", path.display())
            }
            IconSpec::SystemIndexed { path, index } => {
                format!("explicit:{}#{}", path.display(), index)
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
            IconSpec::Weather => "weather".into(),
            IconSpec::Calculator => "calculator".into(),
            IconSpec::Named(name) => format!("named:{name}"),
        };
    }

    if matches!(item.source, crate::platform::SourceId::Shortcut) {
        if let Some(origin) = favicon_origin(&item.launch_target) {
            return format!("favicon:{origin}");
        }
    }

    let fallback = item
        .launch_entries
        .first()
        .map(|entry| entry.target.display().to_string())
        .unwrap_or_else(|| item.launch_target.display().to_string());
    format!("fallback:{fallback}")
}

fn weather_icon() -> IconData {
    let size = 64_u32;
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    let mut pixel = |x: u32, y: u32, color: [u8; 4]| {
        if x < size && y < size {
            let index = ((y * size + x) * 4) as usize;
            rgba[index..index + 4].copy_from_slice(&color);
        }
    };
    let sun = [255, 193, 68, 255];
    for y in 8..42 {
        for x in 8..42 {
            let dx = x as i32 - 25;
            let dy = y as i32 - 25;
            if dx * dx + dy * dy <= 13 * 13 {
                pixel(x, y, sun);
            }
        }
    }
    let cloud = [235, 241, 248, 255];
    for y in 31..53 {
        for x in 19..57 {
            let left = (x as i32 - 29).pow(2) + (y as i32 - 42).pow(2) <= 11 * 11;
            let middle = (x as i32 - 39).pow(2) + (y as i32 - 35).pow(2) <= 13 * 13;
            let right = (x as i32 - 49).pow(2) + (y as i32 - 43).pow(2) <= 10 * 10;
            if left || middle || right || (y >= 42 && x >= 24 && x <= 53) {
                pixel(x, y, cloud);
            }
        }
    }
    IconData {
        width: size,
        height: size,
        rgba,
    }
}

fn calculator_icon() -> IconData {
    let size = 64_u32;
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    let mut pixel = |x: u32, y: u32, color: [u8; 4]| {
        if x < size && y < size {
            let index = ((y * size + x) * 4) as usize;
            rgba[index..index + 4].copy_from_slice(&color);
        }
    };
    let body = [86, 98, 116, 255];
    let screen = [220, 239, 224, 255];
    let key = [244, 180, 76, 255];
    for y in 5..59 {
        for x in 10..54 {
            pixel(x, y, body);
        }
    }
    for y in 12..24 {
        for x in 16..48 {
            pixel(x, y, screen);
        }
    }
    for row in 0..3 {
        for column in 0..4 {
            let left = 16 + column * 8;
            let top = 30 + row * 8;
            for y in top..top + 5 {
                for x in left..left + 5 {
                    pixel(x, y, key);
                }
            }
        }
    }
    IconData {
        width: size,
        height: size,
        rgba,
    }
}

fn favicon_origin(target: &Path) -> Option<String> {
    let value = target.to_str()?;
    let parsed = url::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;
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
            icon: None,
            ranking_bias: 0,
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
        item.category = ResultCategory::Shortcut;
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
            icon: None,
            ranking_bias: 0,
        }
    }
}
