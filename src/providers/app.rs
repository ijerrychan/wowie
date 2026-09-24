//! Windows application index based on the legacy provider.
//!
//! The index intentionally has a small, deterministic set of sources:
//! Start Menu and Desktop shortcuts/URLs/executables plus AppsFolder/Package
//! applications. Shortcuts are launched as shortcuts, but are de-duplicated
//! by their resolved target.

use crate::result::{IconSpec, ResultCategory, SearchResult};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

#[cfg(test)]
use crate::{
    platform::source_ids,
    result::{Action, DetailView, LaunchEntry},
};
#[cfg(test)]
use std::path::Path;

#[path = "app/association.rs"]
mod association;
#[path = "app/discovery.rs"]
mod discovery;
#[path = "app/identity.rs"]
mod identity;

use association::{
    associate_file_entries, dedup_launch_entries_by_behavior, merge_launch_entries,
    refresh_application_location, AssociationStats,
};
pub(crate) use association::{preferred_launch_entry, same_launch_behavior};
use discovery::{
    debug_enabled, debug_record_matches, debug_records, discover_apps_folder_source,
    discover_custom_directories, discover_desktop_source, discover_packaged_source,
    discover_start_menu_source, expand_windows_variables, filesystem_source_fingerprint,
    normalize_url, website_url,
};
#[cfg(test)]
use discovery::{generic_file_result, is_website_shortcut, read_url_target};
#[cfg(test)]
use identity::same_filesystem_target;
use identity::{
    apps_folder_identity, executable_identity, filesystem_target_key,
    filesystem_target_key_for_entry, looks_like_uri, merge_by_identity, normalize_path,
    packaged_identity_fields, same_filesystem_entry_target, shortcut_icon_target, source_priority,
    target_identity, target_stem_alias, IdentityMergeStats,
};

pub struct AppProvider {
    items: Arc<Vec<SearchResult>>,
    sources: HashMap<crate::platform::AppProviderSource, Vec<SearchResult>>,
    source_lookup: SourceLookup,
    config: crate::config::ProviderConfig,
    source_fingerprints: HashMap<crate::platform::AppProviderSource, u64>,
    source_indexed_at: HashMap<crate::platform::AppProviderSource, u64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedAppIndex {
    version: u32,
    config: crate::config::ProviderConfig,
    source_fingerprints: Vec<(crate::platform::AppProviderSource, u64)>,
    source_indexed_at: Vec<(crate::platform::AppProviderSource, u64)>,
    sources: Vec<(crate::platform::AppProviderSource, Vec<SearchResult>)>,
}

const APP_INDEX_CACHE_VERSION: u32 = 4;
const APP_INDEX_CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

pub(crate) fn launch_entry_label(entry: &crate::result::LaunchEntry) -> String {
    if crate::platform::windows_rules::is_shortcut_path(&entry.path) {
        if let Some(name) = entry.path.file_stem().and_then(|name| name.to_str()) {
            if !name.is_empty() {
                return name.to_owned();
            }
        }
    }
    let name = entry
        .target
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| entry.target.to_string_lossy().into_owned());
    if entry.arguments.trim().is_empty() {
        name
    } else {
        format!("{name} {}", entry.arguments.trim())
    }
}

pub(crate) fn visible_launch_entries<'a>(
    item: &'a SearchResult,
    show_advanced_launch_entries: bool,
) -> Vec<(usize, &'a crate::result::LaunchEntry)> {
    let Some(default) = preferred_launch_entry(item) else {
        return Vec::new();
    };
    item.launch_entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            // Installer shortcuts that only identify an icon are not launch
            // actions, even when advanced entries are enabled.
            if crate::platform::windows_rules::is_icon_path(&entry.target)
                || same_launch_behavior(default, entry)
            {
                return None;
            }
            let allowed = show_advanced_launch_entries
                || (crate::platform::windows_rules::is_shortcut_path(&entry.path)
                    && entry.arguments.trim() != default.arguments.trim());
            allowed.then_some((index, entry))
        })
        .collect()
}

struct ProviderDefinition {
    source: crate::platform::AppProviderSource,
    enabled: fn(&crate::config::ProviderConfig) -> bool,
    discover: fn(&crate::config::ProviderConfig) -> Vec<SearchResult>,
}

#[derive(Debug, Default, Clone)]
struct SourceLookup {
    by_aumid: HashMap<String, Vec<(crate::platform::AppProviderSource, usize)>>,
    by_package_family: HashMap<String, Vec<(crate::platform::AppProviderSource, usize)>>,
    by_filesystem_target: HashMap<String, Vec<(crate::platform::AppProviderSource, usize)>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct LookupImpact {
    aumids: HashSet<String>,
    package_families: HashSet<String>,
    filesystem_targets: HashSet<String>,
}

impl SourceLookup {
    fn from_sources(
        sources: &HashMap<crate::platform::AppProviderSource, Vec<SearchResult>>,
    ) -> Self {
        let mut lookup = Self::default();
        for (source, records) in sources {
            for (index, record) in records.iter().enumerate() {
                if let Some(aumid) = record.app_user_model_id.as_deref() {
                    lookup
                        .by_aumid
                        .entry(aumid.trim().to_lowercase())
                        .or_default()
                        .push((*source, index));
                }
                if let Some(family) = record.package_family_name.as_deref() {
                    lookup
                        .by_package_family
                        .entry(family.trim().to_lowercase())
                        .or_default()
                        .push((*source, index));
                }
                let mut targets = record
                    .launch_entries
                    .iter()
                    .filter_map(filesystem_target_key_for_entry)
                    .collect::<Vec<_>>();
                if let Some(target) = filesystem_target_key(&record.launch_target) {
                    targets.push(target);
                }
                targets.sort();
                targets.dedup();
                for target in targets {
                    lookup
                        .by_filesystem_target
                        .entry(target)
                        .or_default()
                        .push((*source, index));
                }
            }
        }
        for index in [
            &mut lookup.by_aumid,
            &mut lookup.by_package_family,
            &mut lookup.by_filesystem_target,
        ] {
            for records in index.values_mut() {
                records.sort_unstable();
            }
        }
        lookup
    }

    fn key_counts(&self) -> (usize, usize, usize) {
        (
            self.by_aumid.len(),
            self.by_package_family.len(),
            self.by_filesystem_target.len(),
        )
    }

    fn diff(&self, other: &Self) -> LookupImpact {
        fn changed_keys<T: PartialEq>(
            before: &HashMap<String, T>,
            after: &HashMap<String, T>,
        ) -> HashSet<String> {
            before
                .keys()
                .chain(after.keys())
                .filter(|key| before.get(*key) != after.get(*key))
                .cloned()
                .collect()
        }

        LookupImpact {
            aumids: changed_keys(&self.by_aumid, &other.by_aumid),
            package_families: changed_keys(&self.by_package_family, &other.by_package_family),
            filesystem_targets: changed_keys(
                &self.by_filesystem_target,
                &other.by_filesystem_target,
            ),
        }
    }
}

const PROVIDER_DEFINITIONS: [ProviderDefinition; 6] = [
    ProviderDefinition {
        source: crate::platform::AppProviderSource::AppsFolder,
        enabled: |config| config.store_apps,
        discover: discover_apps_folder_source,
    },
    ProviderDefinition {
        source: crate::platform::AppProviderSource::PackagedApps,
        enabled: |config| config.store_apps,
        discover: discover_packaged_source,
    },
    ProviderDefinition {
        source: crate::platform::AppProviderSource::StartMenu,
        enabled: |config| config.start_menu,
        discover: discover_start_menu_source,
    },
    ProviderDefinition {
        source: crate::platform::AppProviderSource::Desktop,
        enabled: |config| config.desktop,
        discover: discover_desktop_source,
    },
    ProviderDefinition {
        source: crate::platform::AppProviderSource::ControlPanel,
        enabled: |config| config.control_panel,
        discover: crate::providers::control_panel::discover,
    },
    ProviderDefinition {
        source: crate::platform::AppProviderSource::CustomDirectories,
        enabled: |config| !config.indexed_directories.is_empty(),
        discover: discover_custom_directories,
    },
];

impl AppProvider {
    /// Create an empty provider so the launcher can enter its event loop
    /// before the potentially slow Windows Shell enumeration starts.
    pub fn empty_with_config(config: &crate::config::ProviderConfig) -> Self {
        Self {
            items: Arc::new(Vec::new()),
            sources: HashMap::new(),
            source_lookup: SourceLookup::default(),
            config: config.clone(),
            source_fingerprints: HashMap::new(),
            source_indexed_at: HashMap::new(),
        }
    }

    fn cache_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Wowie")
            .join("app-index-v4.json")
    }

    fn restore_persisted_index(&mut self) -> bool {
        let path = Self::cache_path();
        let Ok(metadata) = fs::metadata(&path) else {
            return false;
        };
        if metadata.len() > 64 * 1024 * 1024 {
            return false;
        }
        let Ok(bytes) = fs::read(path) else {
            return false;
        };
        let Ok(cache) = serde_json::from_slice::<PersistedAppIndex>(&bytes) else {
            return false;
        };
        if cache.version != APP_INDEX_CACHE_VERSION || cache.config != self.config {
            return false;
        }
        self.source_fingerprints = cache.source_fingerprints.into_iter().collect();
        self.source_indexed_at = cache.source_indexed_at.into_iter().collect();
        self.sources = cache.sources.into_iter().collect();
        self.rebuild_source_lookup();
        self.items = Arc::new(merge_sources(&self.sources));
        eprintln!(
            "[provider] restored {} app index record(s) from cache",
            self.items.len()
        );
        true
    }

    fn persist_index(&self) {
        let path = Self::cache_path();
        let cache = PersistedAppIndex {
            version: APP_INDEX_CACHE_VERSION,
            config: self.config.clone(),
            source_fingerprints: self
                .source_fingerprints
                .iter()
                .map(|(source, value)| (*source, *value))
                .collect(),
            source_indexed_at: self
                .source_indexed_at
                .iter()
                .map(|(source, value)| (*source, *value))
                .collect(),
            sources: self
                .sources
                .iter()
                .map(|(source, rows)| (*source, rows.clone()))
                .collect(),
        };
        let Ok(bytes) = serde_json::to_vec(&cache) else {
            return;
        };
        if bytes.len() > 64 * 1024 * 1024 {
            return;
        }
        let _ = crate::utils::atomic_write(&path, &bytes);
    }

    fn source_needs_refresh(&self, source: crate::platform::AppProviderSource) -> bool {
        if let Some(current) = filesystem_source_fingerprint(source, &self.config) {
            return self.source_fingerprints.get(&source) != Some(&current);
        }
        let now = unix_timestamp();
        self.source_indexed_at
            .get(&source)
            .is_none_or(|last_indexed| {
                now.saturating_sub(*last_indexed) > APP_INDEX_CACHE_MAX_AGE.as_secs()
            })
    }

    fn refresh_source_fingerprints(&mut self) {
        self.source_fingerprints = PROVIDER_DEFINITIONS
            .iter()
            .filter_map(|provider| {
                filesystem_source_fingerprint(provider.source, &self.config)
                    .map(|fingerprint| (provider.source, fingerprint))
            })
            .collect();
    }

    /// Populate the initial index. This is intentionally separate from
    /// construction so callers can run it on the provider worker thread.
    pub fn discover_initial(&mut self) {
        if self.restore_persisted_index() {
            let stale_sources = PROVIDER_DEFINITIONS
                .iter()
                .filter(|provider| (provider.enabled)(&self.config))
                .filter(|provider| self.source_needs_refresh(provider.source))
                .map(|provider| provider.source)
                .collect::<Vec<_>>();
            if stale_sources.is_empty() {
                return;
            }
            for source in stale_sources {
                self.refresh_source(source);
            }
            return;
        }
        self.sources = discover_sources(&self.config);
        self.refresh_source_fingerprints();
        let indexed_at = unix_timestamp();
        self.source_indexed_at = self
            .sources
            .keys()
            .map(|source| (*source, indexed_at))
            .collect();
        self.rebuild_source_lookup();
        self.items = Arc::new(merge_sources(&self.sources));
        self.persist_index();
    }

    /// Re-enumerate only the source reported by the platform watcher.
    ///
    /// Windows notifications are deliberately treated as invalidation hints:
    /// a rename, burst of writes, or an overflow can affect more than one
    /// record, so the source is re-enumerated and the normal merge rules are
    /// applied again.
    pub fn refresh_source(&mut self, source: crate::platform::AppProviderSource) -> bool {
        let Some(provider) = PROVIDER_DEFINITIONS
            .iter()
            .find(|provider| provider.source == source && (provider.enabled)(&self.config))
        else {
            return false;
        };
        let started = Instant::now();
        let refreshed = (provider.discover)(&self.config);
        if debug_enabled() {
            eprintln!(
                "[provider] discovery stats source={:?} records={} elapsed_ms={}",
                source,
                refreshed.len(),
                started.elapsed().as_millis(),
            );
        }
        self.source_indexed_at.insert(source, unix_timestamp());
        let _fingerprint_changed =
            filesystem_source_fingerprint(source, &self.config).is_some_and(|fingerprint| {
                self.source_fingerprints.insert(source, fingerprint) != Some(fingerprint)
            });
        if self.sources.get(&source) == Some(&refreshed) {
            self.persist_index();
            return false;
        }
        let previous = self.sources.get(&source).cloned().unwrap_or_default();
        self.sources.insert(source, refreshed);
        let updated_lookup = SourceLookup::from_sources(&self.sources);
        let impact = self.source_lookup.diff(&updated_lookup);
        if debug_enabled() {
            eprintln!(
                "[provider] source refresh impact source={:?} aumid_keys={} package_family_keys={} filesystem_target_keys={}",
                source,
                impact.aumids.len(),
                impact.package_families.len(),
                impact.filesystem_targets.len(),
            );
        }
        self.source_lookup = updated_lookup;
        let refreshed = self.sources.get(&source).map(Vec::as_slice).unwrap_or(&[]);
        self.items = Arc::new(
            try_merge_store_metadata_refresh(&previous, refreshed, &self.items)
                .unwrap_or_else(|| merge_sources(&self.sources)),
        );
        self.persist_index();
        true
    }

    pub fn apply_config(&mut self, config: &crate::config::ProviderConfig) -> bool {
        if self.config == *config {
            return false;
        }
        let mut changed_sources = Vec::new();
        if self.config.store_apps != config.store_apps {
            changed_sources.push(crate::platform::AppProviderSource::AppsFolder);
            changed_sources.push(crate::platform::AppProviderSource::PackagedApps);
        }
        if self.config.start_menu != config.start_menu {
            changed_sources.push(crate::platform::AppProviderSource::StartMenu);
        }
        if self.config.desktop != config.desktop {
            changed_sources.push(crate::platform::AppProviderSource::Desktop);
        }
        if self.config.control_panel != config.control_panel {
            changed_sources.push(crate::platform::AppProviderSource::ControlPanel);
        }
        if self.config.indexed_directories != config.indexed_directories
            || self.config.indexed_extensions != config.indexed_extensions
        {
            changed_sources.push(crate::platform::AppProviderSource::CustomDirectories);
        }
        self.config = config.clone();
        for source in changed_sources {
            let Some(provider) = PROVIDER_DEFINITIONS
                .iter()
                .find(|provider| provider.source == source)
            else {
                continue;
            };
            if (provider.enabled)(&self.config) {
                self.sources
                    .insert(source, (provider.discover)(&self.config));
                self.source_indexed_at.insert(source, unix_timestamp());
                if let Some(fingerprint) = filesystem_source_fingerprint(source, &self.config) {
                    self.source_fingerprints.insert(source, fingerprint);
                }
            } else {
                self.sources.remove(&source);
                self.source_fingerprints.remove(&source);
                self.source_indexed_at.remove(&source);
            }
        }
        self.rebuild_source_lookup();
        self.items = Arc::new(merge_sources(&self.sources));
        self.persist_index();
        true
    }

    pub fn snapshot(&self) -> Arc<Vec<SearchResult>> {
        Arc::clone(&self.items)
    }

    fn rebuild_source_lookup(&mut self) {
        self.source_lookup = SourceLookup::from_sources(&self.sources);
        if debug_enabled() {
            let (aumids, families, targets) = self.source_lookup.key_counts();
            eprintln!(
                "[provider] source lookup keys aumid={} package_family={} filesystem_target={}",
                aumids, families, targets
            );
        }
    }

    pub fn watch_sources(
        custom_directories: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    ) -> std::sync::mpsc::Receiver<crate::platform::AppProviderSource> {
        crate::platform::watch_app_provider_sources(custom_directories)
    }
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn discover_sources(
    config: &crate::config::ProviderConfig,
) -> HashMap<crate::platform::AppProviderSource, Vec<SearchResult>> {
    let mut sources = HashMap::new();
    for provider in PROVIDER_DEFINITIONS {
        if !(provider.enabled)(config) {
            continue;
        }
        let started = Instant::now();
        let records = (provider.discover)(config);
        if debug_enabled() {
            eprintln!(
                "[provider] discovery stats source={:?} records={} elapsed_ms={}",
                provider.source,
                records.len(),
                started.elapsed().as_millis(),
            );
        }
        sources.insert(provider.source, records);
    }
    sources
}

fn merge_sources(
    sources: &HashMap<crate::platform::AppProviderSource, Vec<SearchResult>>,
) -> Vec<SearchResult> {
    let started = Instant::now();
    let mut items = Vec::new();
    let source_counts = PROVIDER_DEFINITIONS
        .iter()
        .filter_map(|provider| {
            sources
                .get(&provider.source)
                .map(|records| (provider.source, records.len()))
        })
        .collect::<Vec<_>>();
    for provider in PROVIDER_DEFINITIONS {
        if let Some(records) = sources.get(&provider.source) {
            items.extend(records.iter().cloned());
        }
    }
    debug_records("before file association", &items);
    let association_stats = associate_file_entries(&mut items);
    debug_records("after file association", &items);
    sort_merge_inputs(&mut items);
    let identity_stats = merge_by_identity(&mut items);
    classify_unclassified_items(&mut items);
    debug_records("final merged records", &items);
    log_index_stats(
        &source_counts,
        association_stats,
        identity_stats,
        &items,
        started.elapsed(),
    );
    items
}

fn sort_merge_inputs(items: &mut [SearchResult]) {
    items.sort_by_cached_key(|item| {
        (
            source_priority(&item.source),
            item.title.to_lowercase(),
            item.identity.clone(),
            item.location.clone(),
        )
    });
}

/// Update a packaged-source refresh locally when its topology is unchanged.
///
/// A package refresh can change display metadata (for example a localized
/// description) without changing any launch or identity evidence. In that
/// narrow case the existing merged records already contain all associations,
/// so rebuilding only the affected identity is safe. Any addition/removal,
/// identity change, target change, or shortcut-like source falls back to the
/// complete association pipeline.
fn try_merge_store_metadata_refresh(
    previous: &[SearchResult],
    refreshed: &[SearchResult],
    current: &[SearchResult],
) -> Option<Vec<SearchResult>> {
    if previous.is_empty() || previous.len() != refreshed.len() || current.is_empty() {
        return None;
    }

    let mut before = HashMap::with_capacity(previous.len());
    for record in previous {
        if !record.source.is_store_application()
            || record.identity.is_empty()
            || before.insert(record.identity.clone(), record).is_some()
        {
            return None;
        }
    }

    let mut changed = Vec::<SearchResult>::new();
    let mut seen = HashSet::with_capacity(refreshed.len());
    for record in refreshed {
        if !record.source.is_store_application()
            || record.identity.is_empty()
            || !seen.insert(record.identity.clone())
        {
            return None;
        }
        let old = before.get(&record.identity)?;
        if old.source != record.source
            || old.launch_target != record.launch_target
            || old.launch_arguments != record.launch_arguments
            || old.launch_working_directory != record.launch_working_directory
            || old.launch_entries != record.launch_entries
            || old.app_user_model_id != record.app_user_model_id
            || old.package_family_name != record.package_family_name
        {
            return None;
        }
        if *old != record {
            changed.push(record.clone());
        }
    }

    let mut result = current.to_vec();
    for incoming in changed {
        let index = result
            .iter()
            .position(|item| item.identity == incoming.identity)?;
        let existing = &result[index];
        let mut candidates = if existing.source == incoming.source {
            vec![incoming.clone(), existing.clone()]
        } else {
            vec![existing.clone(), incoming.clone()]
        };
        merge_by_identity(&mut candidates);
        if candidates.len() != 1 {
            return None;
        }
        result[index] = candidates.pop()?;
    }
    classify_unclassified_items(&mut result);
    sort_merge_inputs(&mut result);
    Some(result)
}

fn log_index_stats(
    source_counts: &[(crate::platform::AppProviderSource, usize)],
    association: AssociationStats,
    identity: IdentityMergeStats,
    records: &[SearchResult],
    elapsed: std::time::Duration,
) {
    if !debug_enabled() {
        return;
    }

    let mut title_identities = HashMap::<String, HashSet<String>>::new();
    for record in records {
        let title = record.title.trim().to_lowercase();
        if !title.is_empty() {
            title_identities
                .entry(title)
                .or_default()
                .insert(record.identity.clone());
        }
    }
    let same_title_distinct_identities = title_identities
        .values()
        .filter(|identities| identities.len() > 1)
        .count();
    let missing_package_descriptions = records
        .iter()
        .filter(|record| {
            record.source.is_store_application()
                && record
                    .description
                    .as_deref()
                    .is_none_or(|description| description.trim().is_empty())
        })
        .count();

    eprintln!(
        "[provider] index stats sources={:?} associated={} ambiguous_rejections={} identity_merges={} launch_entries_removed={} final_records={} missing_package_descriptions={} same_title_distinct_identities={} elapsed_ms={}",
        source_counts,
        association.associated,
        association.ambiguous_rejections,
        identity.merged_records,
        identity.launch_entries_removed,
        records.len(),
        missing_package_descriptions,
        same_title_distinct_identities,
        elapsed.as_millis(),
    );
}

fn classify_unclassified_items(items: &mut [SearchResult]) {
    for item in items {
        if item.category != ResultCategory::Unclassified {
            continue;
        }
        let selected = item
            .launch_entries
            .iter()
            .find_map(|entry| {
                if entry.target.as_os_str().is_empty()
                    || crate::platform::windows_rules::is_shell_namespace(&entry.target)
                {
                    return None;
                }
                let classification = crate::classifier::classify_target_info(&entry.target);
                (!classification.is_http_url).then_some((&entry.target, classification))
            })
            .or_else(|| {
                item.launch_entries.iter().find_map(|entry| {
                    (!entry.target.as_os_str().is_empty()).then_some((
                        &entry.target,
                        crate::classifier::classify_target_info(&entry.target),
                    ))
                })
            })
            .unwrap_or_else(|| {
                (
                    &item.launch_target,
                    crate::classifier::classify_target_info(&item.launch_target),
                )
            });
        let (target, classification) = selected;
        item.category = classification.category;
        if item.category == ResultCategory::Website && item.icon.is_none() {
            item.icon = Some(IconSpec::DefaultOpen(target.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchCore;

    fn item(identity: &str, title: &str, source: &str) -> SearchResult {
        SearchResult {
            id: identity.into(),
            identity: identity.into(),
            title: title.into(),
            category: ResultCategory::Application,
            aliases: vec![],
            location: String::new(),
            launch_target: PathBuf::from("target.exe"),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: source.into(),
            sources: vec![source.into()],
            launch_entries: vec![],
            actions: Vec::new(),
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

    #[test]
    fn source_lookup_indexes_package_and_filesystem_identity() {
        let mut application = item(
            "aumid:contoso.editor_abc!main",
            "Editor",
            source_ids::APPS_FOLDER,
        );
        application.app_user_model_id = Some("Contoso.Editor_abc!Main".into());
        application.package_family_name = Some("Contoso.Editor_abc".into());
        application.launch_target = PathBuf::from(r"shell:AppsFolder\Contoso.Editor_abc!Main");
        application.launch_entries = vec![LaunchEntry {
            source: source_ids::APPS_FOLDER.into(),
            path: PathBuf::from(r"shell:AppsFolder\Contoso.Editor_abc!Main"),
            target: PathBuf::from(r"C:\Apps\Editor.exe"),
            arguments: String::new(),
            working_directory: None,
        }];

        let source = crate::platform::AppProviderSource::AppsFolder;
        let mut sources = HashMap::new();
        sources.insert(source, vec![application]);
        let lookup = SourceLookup::from_sources(&sources);

        assert_eq!(lookup.key_counts(), (1, 1, 1));
        assert_eq!(
            lookup.by_aumid["contoso.editor_abc!main"],
            vec![(source, 0)]
        );
        assert_eq!(
            lookup.by_package_family["contoso.editor_abc"],
            vec![(source, 0)]
        );
        assert_eq!(
            lookup.by_filesystem_target["c:\\apps\\editor.exe"],
            vec![(source, 0)]
        );
    }

    #[test]
    fn empty_identity_records_are_never_merged() {
        let mut items = vec![
            item("", "First", source_ids::START_MENU),
            item("", "Second", source_ids::DESKTOP),
        ];

        merge_by_identity(&mut items);

        assert_eq!(items.len(), 2);
        assert_eq!(
            items
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            vec!["First", "Second"]
        );
    }

    #[test]
    fn relative_launch_targets_include_working_directory_in_matching() {
        let left = LaunchEntry {
            source: source_ids::START_MENU.into(),
            path: PathBuf::from(r"C:\Start Menu\Editor.lnk"),
            target: PathBuf::from(r"bin\editor.exe"),
            arguments: String::new(),
            working_directory: Some(PathBuf::from(r"C:\Apps")),
        };
        let right = LaunchEntry {
            source: source_ids::APPS_FOLDER.into(),
            path: PathBuf::from(r"shell:AppsFolder\Vendor.Editor"),
            target: PathBuf::from(r"C:\Apps\bin\editor.exe"),
            arguments: String::new(),
            working_directory: None,
        };

        assert!(same_filesystem_entry_target(&left, &right));
        assert_eq!(filesystem_target_key(Path::new("")), None);
    }

    #[test]
    fn source_lookup_reports_refresh_impact_keys() {
        let mut first = item(
            "aumid:contoso.editor_abc!main",
            "Editor",
            source_ids::APPS_FOLDER,
        );
        first.app_user_model_id = Some("Contoso.Editor_abc!Main".into());
        first.package_family_name = Some("Contoso.Editor_abc".into());
        first.launch_target = PathBuf::from(r"C:\Apps\Editor.exe");

        let source = crate::platform::AppProviderSource::AppsFolder;
        let mut before_sources = HashMap::new();
        before_sources.insert(source, vec![first.clone()]);

        let mut second = first;
        second.app_user_model_id = Some("Contoso.Editor_def!Main".into());
        second.package_family_name = Some("Contoso.Editor_def".into());
        second.launch_target = PathBuf::from(r"C:\Apps\Editor2.exe");
        let mut after_sources = HashMap::new();
        after_sources.insert(source, vec![second]);

        let impact = SourceLookup::from_sources(&before_sources)
            .diff(&SourceLookup::from_sources(&after_sources));
        assert_eq!(impact.aumids.len(), 2);
        assert_eq!(impact.package_families.len(), 2);
        assert_eq!(impact.filesystem_targets.len(), 2);
        assert!(impact.aumids.contains("contoso.editor_abc!main"));
        assert!(impact.aumids.contains("contoso.editor_def!main"));
    }

    #[test]
    fn store_metadata_refresh_reuses_merged_topology() {
        let identity = "aumid:contoso.editor_abc!main";
        let mut previous = item(identity, "Editor", source_ids::APPS_FOLDER);
        previous.app_user_model_id = Some("Contoso.Editor_abc!Main".into());
        previous.package_family_name = Some("Contoso.Editor_abc".into());
        previous.launch_target = PathBuf::from(r"shell:AppsFolder\Contoso.Editor_abc!Main");
        previous.launch_entries = vec![LaunchEntry {
            source: source_ids::APPS_FOLDER.into(),
            path: PathBuf::from(r"shell:AppsFolder\Contoso.Editor_abc!Main"),
            target: PathBuf::from(r"C:\Apps\Editor.exe"),
            arguments: String::new(),
            working_directory: None,
        }];

        let mut refreshed = previous.clone();
        refreshed.title = "Editor (中文)".into();
        refreshed.description = Some("Localized description".into());

        let source = crate::platform::AppProviderSource::AppsFolder;
        let mut sources = HashMap::new();
        sources.insert(source, vec![previous.clone()]);
        let current = merge_sources(&sources);
        let result = try_merge_store_metadata_refresh(&[previous], &[refreshed], &current)
            .expect("metadata-only refresh should be local");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Editor (中文)");
        assert_eq!(
            result[0].description.as_deref(),
            Some("Localized description")
        );
    }

    #[test]
    fn store_refresh_falls_back_when_launch_topology_changes() {
        let identity = "aumid:contoso.editor_abc!main";
        let mut previous = item(identity, "Editor", source_ids::APPS_FOLDER);
        previous.launch_entries = vec![LaunchEntry {
            source: source_ids::APPS_FOLDER.into(),
            path: PathBuf::from(r"shell:AppsFolder\Contoso.Editor_abc!Main"),
            target: PathBuf::from(r"C:\Apps\Editor.exe"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut refreshed = previous.clone();
        refreshed.launch_entries[0].arguments = "--new-profile".into();

        assert!(try_merge_store_metadata_refresh(
            &[previous],
            &[refreshed],
            &[item(identity, "Editor", source_ids::APPS_FOLDER)]
        )
        .is_none());
    }

    #[test]
    fn merge_sources_is_deterministic_when_source_record_order_changes() {
        let first = item("exe:c:\\apps\\demo.exe", "Demo", source_ids::APPS_FOLDER);
        let second = item("exe:c:\\apps\\other.exe", "Other", source_ids::APPS_FOLDER);
        let source = crate::platform::AppProviderSource::AppsFolder;

        let mut left = HashMap::new();
        left.insert(source, vec![first.clone(), second.clone()]);
        let mut right = HashMap::new();
        right.insert(source, vec![second, first]);

        assert_eq!(merge_sources(&left), merge_sources(&right));
    }

    #[test]
    fn merge_sources_runs_full_index_pipeline() {
        use crate::platform::{PackageMetadata, SourceId};

        fn launch(
            source: SourceId,
            path: &str,
            target: &str,
            arguments: &str,
            working_directory: Option<&str>,
        ) -> LaunchEntry {
            LaunchEntry {
                source,
                path: PathBuf::from(path),
                target: PathBuf::from(target),
                arguments: arguments.into(),
                working_directory: working_directory.map(PathBuf::from),
            }
        }

        let editor_identity = "aumid:vendor.editor_abc!main";
        let mut apps_folder = item(editor_identity, "Editor", source_ids::APPS_FOLDER);
        apps_folder.category = ResultCategory::Unclassified;
        apps_folder.description = Some("Code editor".into());
        apps_folder.app_user_model_id = Some("vendor.editor_abc!main".into());
        apps_folder.package_family_name = Some("vendor.editor_abc".into());
        apps_folder.launch_target = PathBuf::from(r"shell:AppsFolder\vendor.editor_abc!main");
        apps_folder.launch_entries = vec![launch(
            SourceId::AppsFolder,
            r"shell:AppsFolder\vendor.editor_abc!main",
            r"C:\Apps\Editor.exe",
            "",
            None,
        )];

        let mut package = item(editor_identity, "Editor", source_ids::PACKAGE);
        package.category = ResultCategory::Unclassified;
        package.package_metadata = Some(PackageMetadata {
            full_name: Some("Vendor.Editor_1.0.0.0_x64__abc".into()),
            install_location: Some(PathBuf::from(r"C:\Program Files\WindowsApps\Vendor.Editor")),
            identity_name: Some("Vendor.Editor".into()),
        });

        let mut conflicting_package = item(
            "aumid:other.editor_xyz!main",
            "Editor",
            source_ids::APPS_FOLDER,
        );
        conflicting_package.category = ResultCategory::Unclassified;
        conflicting_package.app_user_model_id = Some("other.editor_xyz!main".into());
        conflicting_package.package_family_name = Some("other.editor_xyz".into());
        conflicting_package.launch_entries = vec![launch(
            SourceId::AppsFolder,
            r"shell:AppsFolder\other.editor_xyz!main",
            r"C:\Apps\Editor.exe",
            "",
            None,
        )];

        let mut start_menu = item("exe:c:\\apps\\editor.exe", "Editor", source_ids::START_MENU);
        start_menu.category = ResultCategory::Unclassified;
        start_menu.app_user_model_id = Some("vendor.editor_abc!main".into());
        start_menu.package_family_name = Some("vendor.editor_abc".into());
        start_menu.launch_entries = vec![launch(
            SourceId::StartMenu,
            r"C:\Users\Test\Start Menu\Editor.lnk",
            r"C:\Apps\Editor.exe",
            "--profile work",
            None,
        )];

        let mut desktop = item("exe:c:\\apps\\editor.exe", "Editor", source_ids::DESKTOP);
        desktop.category = ResultCategory::Unclassified;
        desktop.app_user_model_id = Some("vendor.editor_abc!main".into());
        desktop.package_family_name = Some("vendor.editor_abc".into());
        desktop.launch_entries = vec![launch(
            SourceId::Desktop,
            r"C:\Users\Test\Desktop\Editor.lnk",
            r"C:\Apps\Editor.exe",
            "--profile default",
            Some(r"C:\Apps"),
        )];

        let mut sources = HashMap::new();
        sources.insert(
            crate::platform::AppProviderSource::AppsFolder,
            vec![apps_folder, conflicting_package],
        );
        sources.insert(
            crate::platform::AppProviderSource::PackagedApps,
            vec![package],
        );
        sources.insert(
            crate::platform::AppProviderSource::StartMenu,
            vec![start_menu],
        );
        sources.insert(crate::platform::AppProviderSource::Desktop, vec![desktop]);

        let results = merge_sources(&sources);

        assert_eq!(results.len(), 2);
        let editor = results
            .iter()
            .find(|result| result.app_user_model_id.as_deref() == Some("vendor.editor_abc!main"))
            .expect("the package-backed editor should be indexed");
        assert_eq!(editor.source, SourceId::AppsFolder);
        assert_eq!(editor.category, ResultCategory::Application);
        assert_eq!(editor.description.as_deref(), Some("Code editor"));
        assert_eq!(
            editor
                .package_metadata
                .as_ref()
                .and_then(|metadata| metadata.full_name.as_deref()),
            Some("Vendor.Editor_1.0.0.0_x64__abc")
        );
        assert_eq!(editor.launch_target, PathBuf::from(r"C:\Apps\Editor.exe"));
        assert!(editor.sources.contains(&SourceId::AppsFolder));
        assert!(editor.sources.contains(&SourceId::Package));
        assert!(editor.sources.contains(&SourceId::StartMenu));
        assert!(editor.sources.contains(&SourceId::Desktop));
        assert_eq!(editor.launch_entries.len(), 3);
        assert!(editor
            .launch_entries
            .iter()
            .any(|entry| entry.arguments == "--profile work"));
        assert!(editor
            .launch_entries
            .iter()
            .any(|entry| entry.arguments == "--profile default"));

        let conflicting = results
            .iter()
            .find(|result| result.app_user_model_id.as_deref() == Some("other.editor_xyz!main"))
            .expect("the conflicting package must remain separate");
        assert_eq!(conflicting.sources, vec![SourceId::AppsFolder]);
        assert_eq!(conflicting.launch_entries.len(), 1);
    }

    #[test]
    fn merge_sources_covers_url_html_and_source_removal() {
        fn launch(source: &str, path: &str, target: &str) -> LaunchEntry {
            LaunchEntry {
                source: source.into(),
                path: PathBuf::from(path),
                target: PathBuf::from(target),
                arguments: String::new(),
                working_directory: None,
            }
        }

        let url_identity = "url:https://example.com/docs";
        let mut url_app = item(url_identity, "Documentation", source_ids::APPS_FOLDER);
        url_app.category = ResultCategory::Unclassified;
        url_app.launch_entries = vec![launch(
            source_ids::APPS_FOLDER,
            "https://example.com/docs",
            "https://example.com/docs",
        )];

        let mut url_shortcut = item(url_identity, "Documentation", source_ids::START_MENU);
        url_shortcut.category = ResultCategory::Unclassified;
        url_shortcut.launch_entries = vec![launch(
            source_ids::START_MENU,
            r"C:\Start Menu\Documentation.url",
            "https://example.com/docs",
        )];

        let html_identity = r"shell:{6d809377-6af0-444b-8957-a3773f02200e}\git\releasenotes.html";
        let mut html_app = item(html_identity, "Git Release Notes", source_ids::APPS_FOLDER);
        html_app.category = ResultCategory::Unclassified;
        html_app.launch_entries = vec![launch(
            source_ids::APPS_FOLDER,
            r"shell:AppsFolder\{6D809377-6AF0-444B-8957-A3773F02200E}\Git\ReleaseNotes.html",
            r"shell:AppsFolder\{6D809377-6AF0-444B-8957-A3773F02200E}\Git\ReleaseNotes.html",
        )];

        let mut html_shortcut = item(
            r"target:c:\program files\git\releasenotes.html",
            "Git Release Notes",
            source_ids::START_MENU,
        );
        html_shortcut.category = ResultCategory::Unclassified;
        html_shortcut.launch_entries = vec![launch(
            source_ids::START_MENU,
            r"C:\Start Menu\Git Release Notes.lnk",
            r"C:\Program Files\Git\ReleaseNotes.html",
        )];

        let mut sources = HashMap::new();
        sources.insert(
            crate::platform::AppProviderSource::AppsFolder,
            vec![url_app, html_app],
        );
        sources.insert(
            crate::platform::AppProviderSource::StartMenu,
            vec![url_shortcut, html_shortcut],
        );

        let results = merge_sources(&sources);
        assert_eq!(results.len(), 2);
        let documentation = results
            .iter()
            .find(|result| result.identity == url_identity)
            .expect("URL application should remain indexed");
        assert_eq!(documentation.category, ResultCategory::Website);
        assert!(documentation
            .sources
            .contains(&crate::platform::SourceId::StartMenu));

        let release_notes = results
            .iter()
            .find(|result| result.identity == html_identity)
            .expect("local HTML application should remain indexed");
        assert!(release_notes
            .sources
            .contains(&crate::platform::SourceId::StartMenu));

        sources.remove(&crate::platform::AppProviderSource::StartMenu);
        let refreshed = merge_sources(&sources);
        assert_eq!(refreshed.len(), 2);
        assert!(refreshed
            .iter()
            .all(|result| result.sources == vec![crate::platform::SourceId::AppsFolder]));
    }

    #[test]
    fn equal_identities_merge_titles_into_aliases() {
        let mut items = vec![
            item("exe:c:\\apps\\demo.exe", "Demo", "StartMenu"),
            item("exe:c:\\apps\\demo.exe", "Demo (Public)", "StartMenu"),
        ];
        merge_by_identity(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Demo");
        assert!(items[0]
            .aliases
            .iter()
            .any(|alias| alias == "Demo (Public)"));
    }

    #[test]
    fn identity_replacement_preserves_existing_icon() {
        let mut start_menu = item("exe:c:\\apps\\demo.exe", "Demo", "StartMenu");
        start_menu.icon = Some(IconSpec::System(PathBuf::from(r"C:\Apps\demo.exe")));
        let apps_folder = item("exe:c:\\apps\\demo.exe", "Demo", "AppsFolder");
        let mut items = vec![start_menu, apps_folder];

        merge_by_identity(&mut items);
        assert_eq!(items.len(), 1);
        assert!(items[0].icon.is_some());
    }

    #[test]
    fn identity_replacement_preserves_metadata_from_all_sources() {
        let identity = "exe:c:\\apps\\demo.exe";
        let mut start_menu = item(identity, "Demo", "StartMenu");
        start_menu.category = ResultCategory::Application;
        start_menu.actions = vec![Action::Plugin {
            plugin_id: "demo".into(),
            action_id: "start-menu-action".into(),
            icon: None,
        }];
        start_menu.detail = Some(DetailView {
            title: "Start Menu details".into(),
            subtitle: String::new(),
            blocks: Vec::new(),
            actions: Vec::new(),
            source: "StartMenu".into(),
            preferred_width: None,
            scrollable: false,
            fullscreen: false,
        });
        start_menu.description = Some("Start Menu description".into());
        start_menu.ranking_bias = 7;

        let mut apps_folder = item(identity, "Demo", "AppsFolder");
        apps_folder.category = ResultCategory::Unclassified;
        apps_folder.app_user_model_id = Some("Vendor.Demo_abc!Main".into());
        apps_folder.package_family_name = Some("Vendor.Demo_abc".into());
        apps_folder.actions = vec![Action::Plugin {
            plugin_id: "demo".into(),
            action_id: "apps-folder-action".into(),
            icon: None,
        }];
        apps_folder.detail = Some(DetailView {
            title: "AppsFolder details".into(),
            subtitle: String::new(),
            blocks: Vec::new(),
            actions: Vec::new(),
            source: "AppsFolder".into(),
            preferred_width: None,
            scrollable: false,
            fullscreen: false,
        });
        apps_folder.description = None;

        let mut items = vec![start_menu, apps_folder];
        merge_by_identity(&mut items);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, source_ids::APPS_FOLDER.into());
        assert_eq!(items[0].category, ResultCategory::Application);
        assert_eq!(
            items[0].app_user_model_id.as_deref(),
            Some("Vendor.Demo_abc!Main")
        );
        assert_eq!(
            items[0].package_family_name.as_deref(),
            Some("Vendor.Demo_abc")
        );
        assert_eq!(items[0].actions.len(), 2);
        assert_eq!(
            items[0].detail.as_ref().unwrap().title,
            "AppsFolder details"
        );
        assert_eq!(
            items[0].description.as_deref(),
            Some("Start Menu description")
        );
        assert_eq!(items[0].ranking_bias, 7);
    }

    #[test]
    fn same_title_with_distinct_targets_is_not_deduplicated() {
        let mut items = vec![
            item("shortcut:c:\\one.exe", "Settings", "StartMenu"),
            item("shortcut:c:\\two.exe", "Settings", "StartMenu"),
        ];
        merge_by_identity(&mut items);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn target_stem_is_searchable() {
        let mut item = item("exe:c:\\windows\\notepad.exe", "记事本", "StartMenu");
        item.aliases = target_stem_alias(&item.title, Path::new(r"C:\Windows\notepad.exe"));
        assert_eq!(SearchCore::new(vec![item]).search("notepad").len(), 1);
    }

    #[test]
    fn custom_file_result_uses_default_open_action() {
        let result = generic_file_result(
            Path::new(r"C:\Tools\Readme.md"),
            "Readme",
            source_ids::CUSTOM_DIRECTORIES,
        );
        assert_eq!(result.title, "Readme");
        assert_eq!(result.category, ResultCategory::Unclassified);
        assert_eq!(result.source, crate::platform::SourceId::CustomDirectories);
        assert!(matches!(result.icon, Some(IconSpec::DefaultOpen(_))));
    }

    #[test]
    fn launch_context_does_not_change_application_identity() {
        let identity = executable_identity(Path::new(r"C:\Apps\demo.exe"));
        let mut first = item(&identity, "Demo", "StartMenu");
        first.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Demo default.lnk"),
            target: PathBuf::from(r"C:\Apps\demo.exe"),
            arguments: "--profile one".into(),
            working_directory: Some(PathBuf::from(r"C:\Apps")),
        }];
        let mut second = item(&identity, "Demo (work)", "Desktop");
        second.launch_entries = vec![LaunchEntry {
            source: "Desktop".into(),
            path: PathBuf::from(r"C:\Users\User\Desktop\Demo work.lnk"),
            target: PathBuf::from(r"C:\Apps\demo.exe"),
            arguments: "--profile two".into(),
            working_directory: Some(PathBuf::from(r"C:\Apps")),
        }];
        let mut items = vec![first, second];
        merge_by_identity(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].launch_entries.len(), 2);
        assert!(items[0].actions.is_empty());
        assert!(items[0].aliases.iter().any(|alias| alias == "Demo (work)"));
    }

    #[test]
    fn shortcut_is_attached_to_store_app_by_executable_target() {
        let mut app = item("aumid:vendor.app!main", "Vendor App", "AppsFolder");
        app.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(r"shell:AppsFolder\Vendor.App!Main"),
            target: PathBuf::from(r"C:\Apps\vendor.exe"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut shortcut = item("exe:vendor-shortcut", "Vendor App", "StartMenu");
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Vendor App.lnk"),
            target: PathBuf::from(r"C:\Apps\vendor.exe"),
            arguments: "--profile default".into(),
            working_directory: None,
        }];
        let mut items = vec![app, shortcut];
        associate_file_entries(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, crate::platform::SourceId::AppsFolder);
        assert_eq!(items[0].launch_entries.len(), 2);
        assert!(items[0]
            .sources
            .contains(&crate::platform::SourceId::StartMenu));
    }

    #[test]
    fn wsl_shortcut_with_ico_target_is_attached_by_exact_title() {
        let mut app = item(
            "exe:c:\\program files\\wsl\\wslsettings\\wslsettings.exe",
            "WSL Settings",
            "AppsFolder",
        );
        app.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(r"shell:AppsFolder\Microsoft.WSLSettings"),
            target: PathBuf::from(r"C:\Program Files\WSL\WslSettings\wslsettings.exe"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut shortcut = item(
            "target:c:\\windows\\installer\\wsl.ico",
            "WSL Settings",
            "StartMenu",
        );
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\WSL Settings.lnk"),
            target: PathBuf::from(r"C:\Windows\Installer\{guid}\wsl.ico"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut items = vec![app, shortcut];
        associate_file_entries(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].launch_entries.len(), 2);
    }

    #[test]
    fn shell_app_shortcut_with_executable_target_is_attached_by_exact_title() {
        let mut app = item(
            "shell:docker.dockerforwindows.settings",
            "Docker Desktop",
            "AppsFolder",
        );
        app.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(r"shell:AppsFolder\Docker.DockerForWindows.Settings"),
            target: PathBuf::from(r"shell:AppsFolder\Docker.DockerForWindows.Settings"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut shortcut = item(
            "exe:c:\\users\\user\\appdata\\local\\programs\\dockerdesktop\\docker desktop.exe",
            "Docker Desktop",
            "StartMenu",
        );
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Docker Desktop.lnk"),
            target: PathBuf::from(
                r"C:\Users\User\AppData\Local\Programs\DockerDesktop\Docker Desktop.exe",
            ),
            arguments: String::new(),
            working_directory: Some(PathBuf::from(
                r"C:\Users\User\AppData\Local\Programs\DockerDesktop\frontend",
            )),
        }];
        let mut items = vec![app, shortcut];
        associate_file_entries(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].launch_entries.len(), 2);
    }

    #[test]
    fn url_shortcut_is_attached_to_apps_folder_url() {
        let target = PathBuf::from("http://support.steampowered.com/");
        let mut app = item(
            "url:http://support.steampowered.com/",
            "Steam Support Center",
            "AppsFolder",
        );
        app.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from("http://support.steampowered.com/"),
            target: target.clone(),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut shortcut = item(
            "url:http://support.steampowered.com/",
            "Steam Support Center",
            "StartMenu",
        );
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Steam Support Center.url"),
            target,
            arguments: String::new(),
            working_directory: None,
        }];
        let mut items = vec![app, shortcut];
        associate_file_entries(&mut items);
        merge_by_identity(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, crate::platform::SourceId::AppsFolder);
        assert!(items[0]
            .sources
            .contains(&crate::platform::SourceId::StartMenu));
    }

    #[test]
    fn virtual_apps_folder_app_attaches_different_discord_launchers() {
        let mut app = item(
            "shell:com.squirrel.discord.discord",
            "Discord",
            "AppsFolder",
        );
        app.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(r"shell:AppsFolder\com.squirrel.Discord.Discord"),
            target: PathBuf::from(r"shell:AppsFolder\com.squirrel.Discord.Discord"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut update = item("exe:update", "Discord", "StartMenu");
        update.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Discord.lnk"),
            target: PathBuf::from(r"C:\Users\User\AppData\Local\Discord\Update.exe"),
            arguments: "--processStart Discord.exe".into(),
            working_directory: None,
        }];
        let mut versioned = item("exe:discord", "Discord", "StartMenu");
        versioned.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Discord Inc\Discord.lnk"),
            target: PathBuf::from(r"C:\Users\User\AppData\Local\Discord\app-1.0.9258\Discord.exe"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut items = vec![app, update, versioned];
        associate_file_entries(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, crate::platform::SourceId::AppsFolder);
        assert_eq!(items[0].launch_entries.len(), 3);
    }

    #[test]
    fn auto_generated_git_apps_folder_entry_matches_shortcut_by_target_stem() {
        let mut app = item(
            "shell:microsoft.autogenerated.{0fdd9c6b-656b-0f1f-ab08-b1cdbb7067b3}",
            "Git Bash",
            "AppsFolder",
        );
        app.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(
                r"shell:AppsFolder\Microsoft.AutoGenerated.{0FDD9C6B-656B-0F1F-AB08-B1CDBB7067B3}",
            ),
            target: PathBuf::from(
                r"shell:AppsFolder\Microsoft.AutoGenerated.{0FDD9C6B-656B-0F1F-AB08-B1CDBB7067B3}",
            ),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut shortcut = item(
            "exe:c:\\program files\\git\\git-bash.exe",
            "Git Bash",
            "StartMenu",
        );
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Git Bash.lnk"),
            target: PathBuf::from(r"C:\Program Files\Git\git-bash.exe"),
            arguments: "--cd-to-home".into(),
            working_directory: None,
        }];

        let mut items = vec![app, shortcut];
        associate_file_entries(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].identity,
            "shell:microsoft.autogenerated.{0fdd9c6b-656b-0f1f-ab08-b1cdbb7067b3}"
        );
        assert_eq!(items[0].launch_entries.len(), 2);
    }

    #[test]
    fn shortcut_association_checks_all_launch_entries() {
        let mut app = item("aumid:vendor.app!main", "Vendor App", "AppsFolder");
        app.launch_entries = vec![
            LaunchEntry {
                source: "AppsFolder".into(),
                path: PathBuf::from(r"shell:AppsFolder\Vendor.App!Main"),
                target: PathBuf::from(r"C:\Apps\other.exe"),
                arguments: String::new(),
                working_directory: None,
            },
            LaunchEntry {
                source: "AppsFolder".into(),
                path: PathBuf::from(r"shell:AppsFolder\Vendor.App!Main"),
                target: PathBuf::from(r"C:\Apps\vendor.exe"),
                arguments: String::new(),
                working_directory: None,
            },
        ];
        let mut shortcut = item("exe:vendor-shortcut", "Vendor App", "StartMenu");
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Vendor App.lnk"),
            target: PathBuf::from(r"C:\Apps\vendor.exe"),
            arguments: String::new(),
            working_directory: None,
        }];

        let mut items = vec![app, shortcut];
        associate_file_entries(&mut items);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].sources.len(), 2);
    }

    #[test]
    fn conflicting_package_identity_blocks_target_association() {
        let mut app = item("aumid:vendor.app!main", "Vendor App", "AppsFolder");
        app.app_user_model_id = Some("Vendor.App_abc!Main".into());
        app.package_family_name = Some("Vendor.App_abc".into());
        app.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(r"shell:AppsFolder\Vendor.App_abc!Main"),
            target: PathBuf::from(r"C:\Apps\vendor.exe"),
            arguments: String::new(),
            working_directory: None,
        }];

        let mut shortcut = item("exe:other-shortcut", "Vendor App", "StartMenu");
        shortcut.app_user_model_id = Some("Other.App_def!Main".into());
        shortcut.package_family_name = Some("Other.App_def".into());
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Vendor App.lnk"),
            target: PathBuf::from(r"C:\Apps\vendor.exe"),
            arguments: String::new(),
            working_directory: None,
        }];

        let mut items = vec![app, shortcut];
        associate_file_entries(&mut items);

        assert_eq!(items.len(), 2);
    }

    #[test]
    fn local_html_shell_entry_matches_shortcut_before_classification() {
        let mut app = item(
            r"shell:{6d809377-6af0-444b-8957-a3773f02200e}\git\releasenotes.html",
            "Git Release Notes",
            "AppsFolder",
        );
        app.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(
                r"shell:AppsFolder\{6D809377-6AF0-444B-8957-A3773F02200E}\Git\ReleaseNotes.html",
            ),
            target: PathBuf::from(
                r"shell:AppsFolder\{6D809377-6AF0-444B-8957-A3773F02200E}\Git\ReleaseNotes.html",
            ),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut shortcut = item(
            r"target:c:\program files\git\releasenotes.html",
            "Git Release Notes",
            "StartMenu",
        );
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Git Release Notes.lnk"),
            target: PathBuf::from(r"C:\Program Files\Git\ReleaseNotes.html"),
            arguments: String::new(),
            working_directory: None,
        }];

        let mut items = vec![app, shortcut];
        associate_file_entries(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].launch_entries.len(), 2);
    }

    #[test]
    fn shell_namespace_never_matches_a_filesystem_target() {
        let shell = Path::new(r"shell:AppsFolder\Vendor.App_abc!Main");
        let file = Path::new(r"C:\Apps\Vendor.App_abc!Main");
        assert!(crate::platform::windows_rules::is_shell_namespace(shell));
        assert!(!crate::platform::windows_rules::is_shell_namespace(file));
        assert!(!same_filesystem_target(shell, file));
    }

    #[test]
    fn classification_prefers_concrete_target_over_shell_entry() {
        let mut result = item("shell:git-release-notes", "Git Release Notes", "AppsFolder");
        result.category = ResultCategory::Unclassified;
        result.launch_entries = vec![
            LaunchEntry {
                source: "AppsFolder".into(),
                path: PathBuf::from(r"shell:AppsFolder\Git.ReleaseNotes"),
                target: PathBuf::from(r"shell:AppsFolder\Git.ReleaseNotes"),
                arguments: String::new(),
                working_directory: None,
            },
            LaunchEntry {
                source: "StartMenu".into(),
                path: PathBuf::from(r"C:\Start Menu\Git Release Notes.lnk"),
                target: PathBuf::from(r"C:\Program Files\Git\ReleaseNotes.html"),
                arguments: String::new(),
                working_directory: None,
            },
        ];
        classify_unclassified_items(std::slice::from_mut(&mut result));
        assert_eq!(result.category, ResultCategory::Website);
    }

    #[test]
    fn store_location_prefers_concrete_filesystem_launch_target() {
        let mut application = item("shell:vendor.discord", "Discord", "AppsFolder");
        application.launch_target = PathBuf::from(r"shell:AppsFolder\Vendor.Discord");
        application.launch_entries = vec![
            LaunchEntry {
                source: "AppsFolder".into(),
                path: application.launch_target.clone(),
                target: application.launch_target.clone(),
                arguments: String::new(),
                working_directory: None,
            },
            LaunchEntry {
                source: "StartMenu".into(),
                path: PathBuf::from(r"C:\Start Menu\Discord.lnk"),
                target: PathBuf::from(r"C:\Users\User\AppData\Local\Discord\Update.exe"),
                arguments: "--processStart Discord.exe".into(),
                working_directory: None,
            },
            LaunchEntry {
                source: "StartMenu".into(),
                path: PathBuf::from(r"C:\Start Menu\Discord direct.lnk"),
                target: PathBuf::from(
                    r"C:\Users\User\AppData\Local\Discord\app-1.0.9258\Discord.exe",
                ),
                arguments: String::new(),
                working_directory: None,
            },
        ];
        refresh_application_location(&mut application);
        assert_eq!(
            application.location,
            r"C:\Users\User\AppData\Local\Discord\app-1.0.9258\Discord.exe"
        );
    }

    #[test]
    fn preferred_launch_entry_ignores_icon_placeholder_targets() {
        let mut application = item(
            "exe:c:\\apps\\wslsettings.exe",
            "WSL Settings",
            "AppsFolder",
        );
        application.launch_entries = vec![
            LaunchEntry {
                source: "AppsFolder".into(),
                path: PathBuf::from(r"shell:AppsFolder\Microsoft.WSLSettings"),
                target: PathBuf::from(r"C:\Program Files\WSL\WslSettings\wslsettings.exe"),
                arguments: String::new(),
                working_directory: None,
            },
            LaunchEntry {
                source: "StartMenu".into(),
                path: PathBuf::from(r"C:\Start Menu\WSL Settings.lnk"),
                target: PathBuf::from(r"C:\Windows\Installer\{guid}\wsl.ico"),
                arguments: String::new(),
                working_directory: None,
            },
        ];

        assert_eq!(
            preferred_launch_entry(&application).map(|entry| entry.target.clone()),
            Some(PathBuf::from(
                r"C:\Program Files\WSL\WslSettings\wslsettings.exe"
            ))
        );
    }

    #[test]
    fn display_location_prefers_executable_target_over_lnk_path() {
        let mut application = item("exe:c:\\apps\\demo.exe", "Demo", "StartMenu");
        application.location = r"C:\Start Menu\Demo.lnk".into();
        application.launch_entries = vec![
            LaunchEntry {
                source: "StartMenu".into(),
                path: PathBuf::from(r"C:\Start Menu\Demo.lnk"),
                target: PathBuf::from(r"C:\Start Menu\Demo.lnk"),
                arguments: String::new(),
                working_directory: None,
            },
            LaunchEntry {
                source: "AppsFolder".into(),
                path: PathBuf::from(r"shell:AppsFolder\Vendor.Demo"),
                target: PathBuf::from(r"C:\Apps\Demo.exe"),
                arguments: String::new(),
                working_directory: None,
            },
        ];
        refresh_application_location(&mut application);
        assert_eq!(application.location, r"C:\Apps\Demo.exe");
        assert_eq!(
            application.launch_target,
            PathBuf::from(r"C:\Apps\Demo.exe")
        );
    }

    #[test]
    fn virtual_apps_folder_shortcut_uses_shell_identity() {
        assert_eq!(
            apps_folder_identity(
                Path::new(r"C:\Windows\explorer.exe"),
                Some("shell:AppsFolder\\com.squirrel.Discord.Discord"),
            ),
            Some("shell:com.squirrel.discord.discord".into())
        );
    }

    #[test]
    fn administrative_tools_shortcut_matches_virtual_apps_folder_by_title() {
        let mut application = item("shell:legacy.taskmanager", "Task Manager", "AppsFolder");
        application.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(r"shell:AppsFolder\legacy.taskmanager"),
            target: PathBuf::from(r"shell:AppsFolder\legacy.taskmanager"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut shortcut = item("exe:taskmgr", "Task Manager", "StartMenu");
        shortcut.icon = Some(IconSpec::System(PathBuf::from(
            r"C:\Windows\System32\Taskmgr.exe",
        )));
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(
                r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\System Tools\Task Manager.lnk",
            ),
            target: PathBuf::from(r"C:\Windows\System32\Taskmgr.exe"),
            arguments: "/7".into(),
            working_directory: None,
        }];

        let mut items = vec![application, shortcut];
        associate_file_entries(&mut items);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, crate::platform::SourceId::AppsFolder);
        assert!(items[0]
            .sources
            .contains(&crate::platform::SourceId::StartMenu));
        assert_eq!(items[0].launch_entries.len(), 2);
        assert_eq!(
            items[0].icon,
            Some(IconSpec::System(PathBuf::from(
                r"C:\Windows\System32\Taskmgr.exe",
            )))
        );
    }

    #[test]
    fn shortcut_icon_path_expands_variables_and_relative_paths() {
        let target = shortcut_icon_target(
            Path::new(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Admin.lnk"),
            Some(Path::new(r"%windir%\System32\Mycomput.dll")),
            Path::new(r"C:\Windows\System32\Mycomput.exe"),
        );
        assert!(target.ends_with(r"System32\Mycomput.dll"));
    }

    #[test]
    fn same_named_virtual_apps_are_not_merged_by_title_fallback() {
        let mut first = item("shell:vendor.one.discord", "Discord", "AppsFolder");
        first.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(r"shell:AppsFolder\Vendor.One.Discord"),
            target: PathBuf::from(r"shell:AppsFolder\Vendor.One.Discord"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut second = item("shell:vendor.two.discord", "Discord", "AppsFolder");
        second.launch_entries = vec![LaunchEntry {
            source: "AppsFolder".into(),
            path: PathBuf::from(r"shell:AppsFolder\Vendor.Two.Discord"),
            target: PathBuf::from(r"shell:AppsFolder\Vendor.Two.Discord"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut shortcut = item("exe:discord", "Discord", "StartMenu");
        shortcut.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Discord.lnk"),
            target: PathBuf::from(r"C:\Apps\Discord.exe"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut items = vec![first, second, shortcut];
        associate_file_entries(&mut items);
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn merge_preserves_source_aliases_and_all_launch_entries() {
        let identity = "exe:c:\\apps\\demo.exe";
        let mut desktop = item(identity, "Demo Desktop", "Desktop");
        desktop.aliases.push("desktop alias".into());
        desktop.launch_entries = vec![LaunchEntry {
            source: "Desktop".into(),
            path: PathBuf::from(r"C:\Users\User\Desktop\Demo.lnk"),
            target: PathBuf::from(r"C:\Apps\demo.exe"),
            arguments: "--desktop".into(),
            working_directory: None,
        }];
        let mut start_menu = item(identity, "Demo Start", "StartMenu");
        start_menu.aliases.push("start alias".into());
        start_menu.launch_entries = vec![LaunchEntry {
            source: "StartMenu".into(),
            path: PathBuf::from(r"C:\Start Menu\Demo.lnk"),
            target: PathBuf::from(r"C:\Apps\demo.exe"),
            arguments: String::new(),
            working_directory: None,
        }];
        let mut items = vec![desktop, start_menu];
        merge_by_identity(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Demo Start");
        assert_eq!(
            items[0].sources,
            vec![
                crate::platform::SourceId::Desktop,
                crate::platform::SourceId::StartMenu
            ]
        );
        assert_eq!(items[0].launch_entries.len(), 2);
        assert!(items[0]
            .aliases
            .iter()
            .any(|alias| alias == "desktop alias"));
        assert!(items[0].aliases.iter().any(|alias| alias == "start alias"));
    }

    #[test]
    fn launch_entries_deduplicate_provenance_but_keep_distinct_behavior() {
        let identity = "exe:c:\\apps\\demo.exe";
        let mut application = item(identity, "Demo", "AppsFolder");
        application.launch_entries = vec![
            LaunchEntry {
                source: "AppsFolder".into(),
                path: PathBuf::from(r"shell:AppsFolder\Vendor.Demo"),
                target: PathBuf::from(r"C:\Apps\demo.exe"),
                arguments: String::new(),
                working_directory: None,
            },
            LaunchEntry {
                source: "StartMenu".into(),
                path: PathBuf::from(r"C:\Start Menu\Demo.lnk"),
                target: PathBuf::from(r"c:/apps/DEMO.exe"),
                arguments: String::new(),
                working_directory: None,
            },
            LaunchEntry {
                source: "Desktop".into(),
                path: PathBuf::from(r"C:\Users\User\Desktop\Demo.lnk"),
                target: PathBuf::from(r"C:\Apps\demo.exe"),
                arguments: "--profile work".into(),
                working_directory: None,
            },
            LaunchEntry {
                source: "CustomDirectories".into(),
                path: PathBuf::from(r"C:\Tools\Demo.lnk"),
                target: PathBuf::from(r"C:\Apps\demo.exe"),
                arguments: String::new(),
                working_directory: Some(PathBuf::from(r"C:\Other")),
            },
        ];

        let mut items = vec![application];
        merge_by_identity(&mut items);
        application = items.pop().expect("merged item");

        assert_eq!(application.launch_entries.len(), 3);
        assert!(application
            .launch_entries
            .iter()
            .any(|entry| entry.arguments == "--profile work"));
        assert!(application
            .launch_entries
            .iter()
            .any(|entry| entry.working_directory.as_deref() == Some(Path::new(r"C:\Other"))));
    }

    #[test]
    fn local_title_and_aumid_product_remain_searchable_after_merge() {
        let mut item = item("aumid:vendor.calculator_abc!main", "计算器", "AppsFolder");
        item.launch_target = PathBuf::from("shell:AppsFolder\\Vendor.Calculator_abc!Main");
        let mut entries = vec![item];
        merge_by_identity(&mut entries);
        let search = SearchCore::new(entries);
        assert_eq!(search.search("计算器").len(), 1);
        assert_eq!(search.search("calculator").len(), 1);
    }

    #[test]
    fn reads_internet_shortcut_target() {
        let path = std::env::temp_dir().join("wowie-legacy-index-test.url");
        fs::write(&path, "[InternetShortcut]\nURL=https://example.com\n").unwrap();
        assert_eq!(
            read_url_target(&path).as_deref(),
            Some("https://example.com")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn recognizes_browser_shortcut_url_arguments() {
        assert!(is_website_shortcut(
            Path::new(r"C:\Program Files\Browser\browser.exe"),
            Some(r#"--profile "https://example.com/path""#)
        ));
        assert!(!is_website_shortcut(
            Path::new(r"C:\Program Files\Browser\browser.exe"),
            Some("--new-window")
        ));
    }

    #[test]
    fn normalizes_equivalent_url_entries() {
        assert_eq!(
            normalize_url("HTTPS://EXAMPLE.COM:443/path#section"),
            Some("https://example.com/path".into())
        );
        assert_eq!(
            normalize_url("https://example.com/path"),
            normalize_url("https://example.com/path#other")
        );
    }

    #[test]
    fn normalizes_windows_path_segments() {
        assert_eq!(
            normalize_path(Path::new(r"C:/Apps/Browser/../Browser/./browser.exe")),
            r"c:\apps\browser\browser.exe"
        );
        assert_eq!(
            normalize_path(Path::new("file:///C:/Apps/Browser/browser.exe")),
            r"c:\apps\browser\browser.exe"
        );
        assert!(same_filesystem_target(
            Path::new("file:///C:/Apps/Browser/browser.exe"),
            Path::new(r"C:\Apps\Browser\browser.exe")
        ));
        assert_eq!(
            normalize_path(Path::new("file://localhost/C:/Apps/Browser/browser.exe")),
            r"c:\apps\browser\browser.exe"
        );
    }

    #[test]
    fn expands_windows_environment_variables_before_identity_matching() {
        std::env::set_var("WOWIE_TEST_WINDIR", r"C:\Windows");
        assert_eq!(
            expand_windows_variables(PathBuf::from(r"%WOWIE_TEST_WINDIR%\regedit.exe")),
            PathBuf::from(r"C:\Windows\regedit.exe")
        );
        std::env::remove_var("WOWIE_TEST_WINDIR");
    }
}
