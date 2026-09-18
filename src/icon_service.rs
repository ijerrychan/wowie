//! Shared icon resolution and caching for all result providers.

use crate::{
    platform::IconData,
    result::{IconSpec, SearchResult},
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub struct IconService {
    cache: Arc<Mutex<HashMap<String, Option<IconData>>>>,
}

impl IconService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve an icon off the UI thread and reuse both successful and failed
    /// lookups. A failed lookup is cached as well so broken entries do not
    /// repeatedly call into the Windows Shell while the user types.
    pub fn resolve(&self, item: &SearchResult) -> Option<IconData> {
        let source_key = icon_source_key(item);
        if let Ok(cache) = self.cache.lock() {
            if let Some(icon) = cache.get(&source_key) {
                return icon.clone();
            }
        }

        let icon = resolve_uncached(item);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(source_key, icon.clone());
        }
        icon
    }
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

    crate::platform::application_icon(icon_target).or_else(|| {
        item.launch_entries
            .iter()
            .map(|entry| &entry.path)
            .filter(|path| !crate::platform::windows_rules::is_shortcut_path(path))
            .find_map(|path| crate::platform::application_icon(path))
    })
}

fn fetch_favicon(target: &Path) -> Option<IconData> {
    let origin = favicon_origin(target)?;
    let favicon = format!("{origin}/favicon.ico");
    let output = std::process::Command::new("curl.exe")
        .args([
            "-fsSL",
            "--max-time",
            "2",
            "--max-filesize",
            "1048576",
            &favicon,
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    decode_image(&output.stdout)
}

fn decode_image(bytes: &[u8]) -> Option<IconData> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    Some(IconData {
        width,
        height,
        rgba: image.into_raw(),
    })
}

fn resolve_spec(spec: &IconSpec) -> Option<IconData> {
    match spec {
        IconSpec::System(target) => crate::platform::application_icon(target),
        IconSpec::SystemIndexed { path, index } => {
            crate::platform::indexed_application_icon(path, *index)
        }
        IconSpec::DefaultOpen(target) => crate::platform::default_open_icon(target),
        IconSpec::Resource(path) => load_png_icon(path),
    }
}

fn load_png_icon(path: &Path) -> Option<IconData> {
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
            icon: None,
            ranking_bias: 0,
        }
    }
}
