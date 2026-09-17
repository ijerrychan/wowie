//! Shared icon resolution and caching for all result providers.

use crate::{
    platform::IconData,
    result::{IconSpec, SearchResult},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
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

    let icon_target = item
        .launch_entries
        .iter()
        .filter(|entry| {
            !entry.target.to_string_lossy().starts_with("shell:")
                && !entry
                    .target
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("\\update.exe")
        })
        .min_by_key(|entry| entry.target.to_string_lossy().to_lowercase())
        .map(|entry| &entry.target)
        .unwrap_or(&item.launch_target);

    crate::platform::application_icon(icon_target).or_else(|| {
        item.launch_entries
            .iter()
            .map(|entry| &entry.path)
            .find_map(|path| crate::platform::application_icon(path))
    })
}

fn resolve_spec(spec: &IconSpec) -> Option<IconData> {
    match spec {
        IconSpec::System(target) => crate::platform::application_icon(target),
        IconSpec::DefaultOpen(target) => crate::platform::default_open_icon(target),
        IconSpec::Resource(path) => load_png_icon(path),
    }
}

fn load_png_icon(path: &Path) -> Option<IconData> {
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    Some(IconData {
        width,
        height,
        rgba: image.into_raw(),
    })
}

fn icon_source_key(item: &SearchResult) -> String {
    if let Some(icon) = item.icon.as_ref() {
        return match icon {
            IconSpec::System(path) | IconSpec::Resource(path) => {
                format!("explicit:{}", path.display())
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

    let fallback = item
        .launch_entries
        .first()
        .map(|entry| entry.target.display().to_string())
        .unwrap_or_else(|| item.launch_target.display().to_string());
    format!("fallback:{fallback}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{ResultCategory, SearchResult};

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
        }
    }
}
