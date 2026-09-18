//! Windows application index based on the legacy provider.
//!
//! The index intentionally has a small, deterministic set of sources:
//! Start Menu and Desktop shortcuts/URLs/executables plus AppsFolder/Package
//! applications. Shortcuts are launched as shortcuts, but are de-duplicated
//! by their resolved target.

use crate::platform::source_ids;
use crate::result::{Action, IconSpec, LaunchEntry, ResultCategory, SearchResult};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};
use url::Url;

pub struct AppProvider {
    items: Vec<SearchResult>,
    sources: HashMap<crate::platform::AppProviderSource, Vec<SearchResult>>,
    config: crate::config::ProviderConfig,
}

struct ProviderDefinition {
    source: crate::platform::AppProviderSource,
    enabled: fn(&crate::config::ProviderConfig) -> bool,
    discover: fn(&crate::config::ProviderConfig) -> Vec<SearchResult>,
}

const PROVIDER_DEFINITIONS: [ProviderDefinition; 6] = [
    ProviderDefinition {
        source: crate::platform::AppProviderSource::StoreApps,
        enabled: |config| config.store_apps,
        discover: discover_store_source,
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
        source: crate::platform::AppProviderSource::Steam,
        enabled: |config| config.steam,
        discover: crate::providers::steam::discover_with_config,
    },
    ProviderDefinition {
        source: crate::platform::AppProviderSource::CustomDirectories,
        enabled: |config| !config.indexed_directories.is_empty(),
        discover: discover_custom_directories,
    },
];

impl AppProvider {
    #[allow(dead_code)]
    pub fn discover() -> Self {
        Self::discover_with_config(&crate::config::ProviderConfig::default())
    }

    pub fn discover_with_config(config: &crate::config::ProviderConfig) -> Self {
        let sources = PROVIDER_DEFINITIONS
            .into_iter()
            .filter(|provider| (provider.enabled)(config))
            .map(|provider| (provider.source, (provider.discover)(config)))
            .collect();
        let items = merge_sources(&sources);
        Self {
            items,
            sources,
            config: config.clone(),
        }
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
        let refreshed = (provider.discover)(&self.config);
        if self.sources.get(&source) == Some(&refreshed) {
            return false;
        }
        self.sources.insert(source, refreshed);
        self.items = merge_sources(&self.sources);
        true
    }

    pub fn apply_config(&mut self, config: &crate::config::ProviderConfig) -> bool {
        if self.config == *config {
            return false;
        }
        let sources = PROVIDER_DEFINITIONS
            .into_iter()
            .filter(|provider| (provider.enabled)(config))
            .map(|provider| (provider.source, (provider.discover)(config)))
            .collect();
        self.config = config.clone();
        self.sources = sources;
        self.items = merge_sources(&self.sources);
        true
    }

    pub fn snapshot(&self) -> Vec<SearchResult> {
        self.items.clone()
    }

    pub fn watch_sources(
        custom_directories: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    ) -> std::sync::mpsc::Receiver<crate::platform::AppProviderSource> {
        crate::platform::watch_app_provider_sources(custom_directories)
    }
}

fn discover_store_source(_config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    let mut items = Vec::new();
    collect_store_apps(&mut items);
    items
}

fn discover_start_menu_source(_config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    discover_file_source(false, source_ids::START_MENU)
}

fn discover_desktop_source(_config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    discover_file_source(true, source_ids::DESKTOP)
}

fn discover_custom_directories(config: &crate::config::ProviderConfig) -> Vec<SearchResult> {
    let mut items = Vec::new();
    for root in &config.indexed_directories {
        if !root.is_dir() {
            eprintln!(
                "custom index directory is not accessible: {}",
                root.display()
            );
            continue;
        }
        collect_tree(
            root,
            &mut items,
            true,
            source_ids::CUSTOM_DIRECTORIES,
            &config.indexed_extensions,
        );
    }
    items
}

fn discover_file_source(include_executables: bool, source: &str) -> Vec<SearchResult> {
    let mut items = Vec::new();
    let roots = if source == source_ids::START_MENU {
        start_menu_roots()
    } else {
        desktop_roots()
    };
    for root in roots {
        collect_tree(&root, &mut items, include_executables, source, &[]);
    }
    items
}

fn merge_sources(
    sources: &HashMap<crate::platform::AppProviderSource, Vec<SearchResult>>,
) -> Vec<SearchResult> {
    let mut items = Vec::new();
    for provider in PROVIDER_DEFINITIONS {
        if let Some(records) = sources.get(&provider.source) {
            items.extend(records.iter().cloned());
        }
    }
    debug_records("before file association", &items);
    associate_file_entries(&mut items);
    debug_records("after file association", &items);
    items.sort_by(|left, right| {
        source_priority(&left.source)
            .cmp(&source_priority(&right.source))
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
            .then_with(|| left.identity.cmp(&right.identity))
            .then_with(|| left.location.cmp(&right.location))
    });
    merge_by_identity(&mut items);
    classify_unclassified_items(&mut items);
    debug_records("final merged records", &items);
    items
}

fn classify_unclassified_items(items: &mut [SearchResult]) {
    for item in items {
        if item.category != ResultCategory::Unclassified {
            continue;
        }
        let target = item
            .launch_entries
            .iter()
            .find_map(|entry| {
                (!entry.target.as_os_str().is_empty()
                    && !is_shell_namespace(&entry.target)
                    && !looks_like_uri(&entry.target))
                .then_some(&entry.target)
            })
            .or_else(|| {
                item.launch_entries.iter().find_map(|entry| {
                    (!entry.target.as_os_str().is_empty()).then_some(&entry.target)
                })
            })
            .unwrap_or(&item.launch_target);
        item.category = crate::classifier::classify_target(target);
        if item.category == ResultCategory::Website && item.icon.is_none() {
            item.icon = Some(IconSpec::DefaultOpen(target.clone()));
        }
    }
}

fn debug_enabled() -> bool {
    // Control Panel diagnostics use WOWIE_PROVIDER_DEBUG. Keep the verbose
    // application-provider trace behind a separate opt-in switch.
    env::var_os("WOWIE_ALL_PROVIDER_DEBUG").is_some_and(|value| value != "0")
}

fn debug_filter_matches(value: &str) -> bool {
    let Some(filter) = env::var_os("WOWIE_PROVIDER_DEBUG_FILTER") else {
        return true;
    };
    let value = value.to_ascii_lowercase();
    let terms: Vec<_> = filter
        .to_string_lossy()
        .split(',')
        .map(|term| term.trim().to_ascii_lowercase())
        .filter(|term| !term.is_empty())
        .collect();
    terms.is_empty() || terms.iter().any(|term| value.contains(term))
}

fn debug_record_matches(record: &SearchResult) -> bool {
    let mut haystack = format!(
        "{} {} {} {}",
        record.title,
        record.identity,
        record.location,
        record.launch_target.display()
    );
    for alias in &record.aliases {
        haystack.push(' ');
        haystack.push_str(alias);
    }
    for entry in &record.launch_entries {
        haystack.push(' ');
        haystack.push_str(&entry.path.to_string_lossy());
        haystack.push(' ');
        haystack.push_str(&entry.target.to_string_lossy());
        haystack.push(' ');
        haystack.push_str(&entry.arguments);
    }
    debug_filter_matches(&haystack)
}

fn debug_records(stage: &str, records: &[SearchResult]) {
    if !debug_enabled() {
        return;
    }
    let relevant: Vec<_> = records
        .iter()
        .filter(|record| debug_record_matches(record))
        .collect();
    eprintln!(
        "[provider] {stage}: {} records matching WOWIE_PROVIDER_DEBUG_FILTER",
        relevant.len()
    );
    for record in relevant {
        eprintln!(
            "[provider] record title={:?} source={} identity={:?} launch_target={:?} sources={:?}",
            record.title, record.source, record.identity, record.launch_target, record.sources
        );
        for entry in &record.launch_entries {
            eprintln!(
                "[provider]   launch_entry source={} path={:?} target={:?} args={:?} working_directory={:?}",
                entry.source,
                entry.path,
                entry.target,
                entry.arguments,
                entry.working_directory
            );
        }
    }
}

fn collect_store_apps(output: &mut Vec<SearchResult>) {
    for app in crate::platform::discover_store_applications() {
        if debug_enabled()
            && debug_filter_matches(&format!(
                "{} {:?} {:?} {:?}",
                app.title, app.aliases, app.identity, app.launch_target
            ))
        {
            eprintln!(
                "[provider] store app title={:?} aliases={:?} identity={:?} launch_target={:?} executable={:?}",
                app.title, app.aliases, app.identity, app.launch_target, app.executable
            );
        }
        let launch_target = app.launch_target.clone();
        output.push(SearchResult {
            id: app.identity.clone(),
            identity: app.identity,
            title: app.title,
            category: ResultCategory::Unclassified,
            aliases: app.aliases,
            location: launch_target.display().to_string(),
            launch_target: launch_target.clone(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: app.source.into(),
            sources: vec![app.source.into()],
            launch_entries: vec![LaunchEntry {
                source: app.source.into(),
                path: app.launch_target.clone(),
                target: app
                    .executable
                    .clone()
                    .unwrap_or_else(|| app.launch_target.clone()),
                arguments: String::new(),
                working_directory: None,
            }],
            actions: Vec::new(),
            icon: None,
            ranking_bias: 0,
        });
    }
}

fn start_menu_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(appdata) = env::var("APPDATA") {
        roots.push(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(program_data) = env::var("ProgramData") {
        roots.push(PathBuf::from(program_data).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    roots
}

fn desktop_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(user_profile) = env::var("USERPROFILE") {
        roots.push(PathBuf::from(user_profile).join("Desktop"));
    }
    if let Ok(public) = env::var("PUBLIC") {
        roots.push(PathBuf::from(public).join("Desktop"));
    }
    roots
}

fn collect_tree(
    root: &Path,
    output: &mut Vec<SearchResult>,
    include_executables: bool,
    source: &str,
    indexed_extensions: &[String],
) {
    let Ok(entries) = fs::read_dir(root) else {
        if source == source_ids::CUSTOM_DIRECTORIES {
            eprintln!("could not read custom index directory: {}", root.display());
        }
        return;
    };

    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.path().to_string_lossy().to_lowercase());
    for entry in entries {
        let path = entry.path();
        // This mirrors WalkDir::follow_links(false) used by the legacy index.
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            collect_tree(
                &path,
                output,
                include_executables,
                source,
                indexed_extensions,
            );
            continue;
        }

        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        let is_shortcut =
            extension.eq_ignore_ascii_case(crate::platform::windows_rules::SHORTCUT_EXTENSION);
        let is_url = extension.eq_ignore_ascii_case(crate::platform::windows_rules::URL_EXTENSION);
        let is_executable = include_executables
            && extension.eq_ignore_ascii_case(crate::platform::windows_rules::EXECUTABLE_EXTENSION);
        let is_custom_file = indexed_extensions
            .iter()
            .any(|value| value.eq_ignore_ascii_case(extension));
        if is_executable {
            let Some(title) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let identity = executable_identity(&path);
            output.push(SearchResult {
                id: identity.clone(),
                identity,
                title: title.to_owned(),
                category: ResultCategory::Unclassified,
                aliases: Vec::new(),
                location: path.display().to_string(),
                launch_target: path.clone(),
                launch_arguments: String::new(),
                launch_working_directory: None,
                source: source.into(),
                sources: vec![source.into()],
                launch_entries: vec![LaunchEntry {
                    source: source.into(),
                    path: path.clone(),
                    target: path.clone(),
                    arguments: String::new(),
                    working_directory: None,
                }],
                actions: Vec::new(),
                icon: None,
                ranking_bias: 0,
            });
            continue;
        }
        if is_custom_file && !is_shortcut && !is_url {
            let Some(title) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            output.push(generic_file_result(&path, title, source));
            continue;
        }
        if !is_shortcut && !is_url {
            continue;
        }
        let Some(title) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };

        if is_url {
            let Some(url) = read_url_target(&path) else {
                continue;
            };
            let Some(normalized_url) = normalize_url(&url) else {
                continue;
            };
            output.push(SearchResult {
                id: format!("url:{normalized_url}"),
                identity: format!("url:{normalized_url}"),
                title: title.to_owned(),
                category: ResultCategory::Unclassified,
                aliases: Vec::new(),
                location: url.clone(),
                launch_target: PathBuf::from(&url),
                launch_arguments: String::new(),
                launch_working_directory: None,
                source: source.into(),
                sources: vec![source.into()],
                launch_entries: vec![LaunchEntry {
                    source: source.into(),
                    path: path.clone(),
                    target: PathBuf::from(&url),
                    arguments: String::new(),
                    working_directory: None,
                }],
                actions: Vec::new(),
                icon: Some(IconSpec::DefaultOpen(PathBuf::from(&url))),
                ranking_bias: 0,
            });
            continue;
        }
        let shortcut = crate::platform::resolve_shortcut(&path);
        let Some(resolved_target) = shortcut.target.map(expand_windows_variables) else {
            continue;
        };
        if debug_enabled()
            && debug_filter_matches(&format!(
                "{title} {path:?} {resolved_target:?} {:?}",
                shortcut.arguments
            ))
        {
            eprintln!(
                "[provider] shortcut source={source} title={title:?} path={path:?} target={resolved_target:?} args={:?}",
                shortcut.arguments
            );
        }
        let website_url = website_url(&resolved_target, shortcut.arguments.as_deref());
        let identity = if let Some(url) = website_url.as_deref() {
            format!("url:{url}")
        } else if let Some(identity) =
            apps_folder_identity(&resolved_target, shortcut.arguments.as_deref())
        {
            identity
        } else {
            target_identity(&resolved_target)
        };
        let aliases = target_stem_alias(title, &resolved_target);
        output.push(SearchResult {
            id: identity.clone(),
            identity,
            title: title.to_owned(),
            category: ResultCategory::Unclassified,
            aliases,
            location: path.display().to_string(),
            // Launch the shortcut itself so Windows preserves ShellLink metadata.
            launch_target: path.clone(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: source.into(),
            sources: vec![source.into()],
            launch_entries: vec![LaunchEntry {
                source: source.into(),
                path: path.clone(),
                target: resolved_target.clone(),
                arguments: shortcut.arguments.clone().unwrap_or_default(),
                working_directory: shortcut.working_directory.clone(),
            }],
            actions: Vec::new(),
            icon: website_url
                .map(PathBuf::from)
                .map(IconSpec::DefaultOpen)
                .or_else(|| {
                    Some(match (shortcut.icon_path.as_deref(), shortcut.icon_index) {
                        (Some(icon_path), Some(index)) => IconSpec::SystemIndexed {
                            path: shortcut_icon_target(&path, Some(icon_path), &resolved_target),
                            index,
                        },
                        _ => IconSpec::System(shortcut_icon_target(
                            &path,
                            shortcut.icon_path.as_deref(),
                            &resolved_target,
                        )),
                    })
                }),
            ranking_bias: 0,
        });
    }
}

fn generic_file_result(path: &Path, title: &str, source: &str) -> SearchResult {
    let target = path.to_owned();
    let identity = format!("file:{}", normalize_path(path));
    let source_id: crate::platform::SourceId = source.into();
    SearchResult {
        id: identity.clone(),
        identity,
        title: title.to_owned(),
        category: ResultCategory::Unclassified,
        aliases: Vec::new(),
        location: path.display().to_string(),
        launch_target: target.clone(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: source_id.clone(),
        sources: vec![source_id.clone()],
        launch_entries: vec![LaunchEntry {
            source: source_id,
            path: target.clone(),
            target,
            arguments: String::new(),
            working_directory: None,
        }],
        actions: Vec::new(),
        icon: Some(IconSpec::DefaultOpen(path.to_owned())),
        ranking_bias: 0,
    }
}

fn normalize_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value.trim()).ok()?;
    url.set_fragment(None);
    Some(url.to_string())
}

fn is_website_target(value: &Path) -> bool {
    Url::parse(value.to_string_lossy().trim())
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false)
}

#[cfg(test)]
fn is_website_shortcut(target: &Path, arguments: Option<&str>) -> bool {
    website_url(target, arguments).is_some()
}

fn website_url(target: &Path, arguments: Option<&str>) -> Option<String> {
    let candidate = if is_website_target(target) {
        Some(target.to_string_lossy().trim().to_owned())
    } else {
        arguments
            .into_iter()
            .flat_map(|value| value.split_whitespace())
            .map(|value| value.trim_matches('"'))
            .find(|value| {
                Url::parse(value)
                    .map(|url| matches!(url.scheme(), "http" | "https"))
                    .unwrap_or(false)
            })
            .map(str::to_owned)
    }?;
    normalize_url(&candidate)
}

fn expand_windows_variables(path: PathBuf) -> PathBuf {
    let mut value = path.to_string_lossy().into_owned();
    let mut cursor = 0;
    while let Some(start_rel) = value[cursor..].find('%') {
        let start = cursor + start_rel;
        let Some(end_rel) = value[start + 1..].find('%') else {
            break;
        };
        let end = start + 1 + end_rel;
        let name = &value[start + 1..end];
        let Some(replacement) = env::var(name).ok() else {
            cursor = end + 1;
            continue;
        };
        value.replace_range(start..=end, &replacement);
        cursor = start + replacement.len();
    }
    PathBuf::from(value)
}

fn read_url_target(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .filter_map(|line| line.split_once('='))
        .find_map(|(key, value)| {
            key.trim()
                .eq_ignore_ascii_case("URL")
                .then(|| value.trim().to_owned())
        })
        .filter(|value| !value.is_empty())
}

fn associate_file_entries(items: &mut Vec<SearchResult>) {
    let mut unified = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        let is_file_entry = is_file_source(&item.source);
        let match_index = if is_file_entry && !is_remote_website(&item) {
            item.launch_entries.first().and_then(|entry| {
                let candidates: Vec<_> = unified
                    .iter()
                    .enumerate()
                    .filter_map(|(index, application)| {
                        is_store_application(application)
                            .then(|| {
                                shortcut_match_strength(&item, entry, application)
                                    .map(|strength| (index, strength))
                            })
                            .flatten()
                    })
                    .collect();
                select_association_candidate(&candidates)
            })
        } else {
            None
        };

        if let Some(index) = match_index {
            let application = &mut unified[index];
            if application.icon.is_none() {
                application.icon = item.icon.clone();
            }
            application.aliases.extend(item.aliases);
            application.sources.extend(item.sources);
            merge_launch_entries(application, item.launch_entries);
        } else {
            unified.push(item);
        }
    }
    for item in &mut unified {
        item.sources
            .sort_by_key(|source| source.as_str().to_lowercase());
        item.sources.dedup();
        refresh_application_location(item);
        refresh_display_location(item);
        refresh_launch_actions(item);
    }
    *items = unified;
}

fn is_store_application(item: &SearchResult) -> bool {
    item.source.is_store_application()
}

fn is_remote_website(item: &SearchResult) -> bool {
    if looks_like_uri(&item.launch_target) {
        return true;
    }
    item.launch_entries.iter().any(|entry| {
        looks_like_uri(&entry.target)
            || website_url(&entry.target, Some(&entry.arguments)).is_some()
    })
}

fn is_file_source(source: &crate::platform::SourceId) -> bool {
    source.is_file_source()
}

fn is_legacy_tools_shortcut(path: &Path) -> bool {
    crate::platform::windows_rules::is_legacy_tools_path(path)
}

fn supports_exact_title_association(target: &Path) -> bool {
    target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("ico")
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchStrength {
    Strong,
    TitleFallback,
}

fn shortcut_match_strength(
    shortcut: &SearchResult,
    entry: &LaunchEntry,
    application: &SearchResult,
) -> Option<MatchStrength> {
    // AUMID is authoritative when the shortcut exposes one. Conflicting
    // AUMIDs must never be merged through a weaker executable match.
    if shortcut.identity.starts_with("aumid:") {
        return (shortcut.identity == application.identity).then_some(MatchStrength::Strong);
    }
    if shortcut.identity == application.identity {
        return Some(MatchStrength::Strong);
    }
    if application.identity.starts_with("aumid:") {
        return application
            .launch_entries
            .iter()
            .any(|app_entry| same_filesystem_target(&app_entry.target, &entry.target))
            .then_some(MatchStrength::Strong);
    }
    // Some Windows AppsFolder entries expose a virtual shell identity while
    // their Start Menu shortcut points at a real executable. WSL is a notable
    // exception: both WSL shortcuts can point at an installer-generated .ico
    // placeholder. Associate these only when the title matches exactly and
    // the target has a known executable/placeholder extension.
    if (application.identity.starts_with("shell:") || application.identity.starts_with("exe:"))
        && !application.title.is_empty()
        && shortcut.title.eq_ignore_ascii_case(&application.title)
        && supports_exact_title_association(&entry.target)
    {
        return Some(MatchStrength::TitleFallback);
    }
    if application.identity.starts_with("shell:") {
        // The legacy Administrative Tools and System Tools folders contain
        // classic .msc/.exe shortcuts. AppsFolder exposes those entries as
        // virtual shell items, so their filesystem targets cannot be compared
        // directly. Restrict the title fallback to these known folders and
        // still let select_association_candidate reject ambiguous matches.
        if is_legacy_tools_shortcut(&entry.path)
            && !application.title.is_empty()
            && shortcut.title.eq_ignore_ascii_case(&application.title)
        {
            return Some(MatchStrength::TitleFallback);
        }
        // Virtual AppsFolder entries (for example Squirrel apps) do not
        // expose a filesystem executable. Their Start Menu shortcuts may
        // legitimately point to different launchers (Update.exe vs the
        // versioned executable), so use the stable virtual identity only as
        // a constrained title association.
        let compact = |value: &str| {
            value
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        };
        let title = compact(&application.title);
        let shell_identity_path = application.identity.trim_start_matches("shell:");
        let shell_identity = compact(shell_identity_path);
        let shell_identity_matches_title = shell_identity_path
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|part| !part.is_empty())
            .map(compact)
            .any(|part| part == title);
        let is_auto_generated = shell_identity.starts_with("microsoftautogenerated");
        let target_stem = entry
            .target
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(compact);
        let target_matches_title = target_stem.as_ref().is_some_and(|stem| stem == &title);
        let shell_path_stem = shell_identity_path
            .rsplit(['\\', '/'])
            .next()
            .and_then(|name| name.rsplit_once('.').map(|(stem, _)| compact(stem)));
        let shell_path_matches_target = shell_path_stem
            .zip(target_stem.as_ref())
            .is_some_and(|(shell_stem, target_stem)| shell_stem == *target_stem);
        if !title.is_empty()
            && shortcut.title.eq_ignore_ascii_case(&application.title)
            && (shell_identity_matches_title
                || (is_auto_generated && target_matches_title)
                || shell_path_matches_target)
        {
            return Some(MatchStrength::TitleFallback);
        }
    }
    application
        .launch_entries
        .iter()
        .any(|app_entry| same_filesystem_target(&app_entry.target, &entry.target))
        .then_some(MatchStrength::Strong)
}

fn select_association_candidate(candidates: &[(usize, MatchStrength)]) -> Option<usize> {
    let strong: Vec<_> = candidates
        .iter()
        .filter_map(|(index, strength)| (*strength == MatchStrength::Strong).then_some(*index))
        .collect();
    if strong.len() == 1 {
        return strong.into_iter().next();
    }
    if !strong.is_empty() {
        return None;
    }
    let fallback: Vec<_> = candidates
        .iter()
        .filter_map(|(index, strength)| {
            (*strength == MatchStrength::TitleFallback).then_some(*index)
        })
        .collect();
    (fallback.len() == 1).then(|| fallback[0])
}

fn refresh_launch_actions(item: &mut SearchResult) {
    item.actions = item
        .launch_entries
        .iter()
        .cloned()
        .map(Action::Launch)
        .collect();
}

/// Merge launch provenance at the same point as application-record merging.
/// The caller decides whether two records represent the same application;
/// this helper then keeps only distinct launch behavior within that record.
fn merge_launch_entries(existing: &mut SearchResult, incoming: Vec<LaunchEntry>) {
    existing.launch_entries.extend(incoming);
    dedup_launch_entries_by_behavior(&mut existing.launch_entries);
}

/// Keep one entry for each actual launch behavior. The discovered source and
/// shortcut path are provenance metadata; they do not change what Windows
/// launches. Arguments and working directory do change the behavior and must
/// therefore remain part of the deduplication key.
fn dedup_launch_entries_by_behavior(entries: &mut Vec<LaunchEntry>) {
    entries.sort_by(|left, right| {
        launch_behavior_key(left)
            .cmp(&launch_behavior_key(right))
            .then_with(|| source_priority(&left.source).cmp(&source_priority(&right.source)))
            .then_with(|| left.source.as_str().cmp(&right.source.as_str()))
            .then_with(|| left.path.cmp(&right.path))
    });

    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert(launch_behavior_key(entry)));
}

fn launch_behavior_key(entry: &LaunchEntry) -> (String, String, String) {
    (
        launch_behavior_target_key(&entry.target),
        entry.arguments.clone(),
        launch_behavior_working_directory_key(&entry.target, &entry.working_directory),
    )
}

fn launch_behavior_target_key(target: &Path) -> String {
    let value = target.to_string_lossy().trim().replace('/', "\\");
    if let Ok(url) = Url::parse(&value) {
        if matches!(url.scheme(), "http" | "https") {
            return normalize_url(&value).unwrap_or(value);
        }
    }
    normalize_path(Path::new(&value))
}

fn launch_behavior_working_directory_key(
    target: &Path,
    working_directory: &Option<PathBuf>,
) -> String {
    if let Some(path) = working_directory {
        return normalize_path(path);
    }

    // When a shortcut omits its working directory, Windows commonly uses the
    // executable's parent directory. Treat that implicit value as equivalent
    // to an explicit shortcut working directory pointing at the same parent.
    if !is_shell_namespace(target) && !looks_like_uri(target) {
        if let Some(parent) = target.parent() {
            return normalize_path(parent);
        }
    }
    String::new()
}

fn refresh_application_location(item: &mut SearchResult) {
    // Control Panel deliberately keeps its user-facing location hidden; its
    // Shell parsing name is an implementation detail used only for launch.
    if item.category == ResultCategory::ControlPanel {
        return;
    }
    let Some((target, arguments, working_directory, path)) =
        preferred_launch_entry(item).map(|entry| {
            (
                entry.target.clone(),
                entry.arguments.clone(),
                entry.working_directory.clone(),
                entry.path.clone(),
            )
        })
    else {
        return;
    };
    item.location = target.display().to_string();

    // Keep the default launch tied to the selected entry. For a shortcut,
    // launching the .lnk preserves ShellLink metadata; its resolved target
    // and arguments are used for presentation only.
    if is_store_application(item) || !is_shortcut_path(&path) {
        item.launch_target = target;
        item.launch_arguments = arguments;
        item.launch_working_directory = working_directory;
    } else {
        item.launch_target = path;
        item.launch_arguments.clear();
        item.launch_working_directory = working_directory;
    }
}

/// Prefer the same indexed entry for both the displayed location and the
/// default icon/launch target. Source priority is applied after excluding
/// Shell namespace targets, URLs, and updater launchers when a better target
/// exists. This keeps presentation aligned with the provider merge policy.
pub(crate) fn preferred_launch_entry(item: &SearchResult) -> Option<&LaunchEntry> {
    let concrete = item
        .launch_entries
        .iter()
        .filter(|entry| !is_shell_namespace(&entry.target) && !looks_like_uri(&entry.target))
        .collect::<Vec<_>>();
    let candidates = if concrete
        .iter()
        .any(|entry| !crate::platform::windows_rules::is_updater_path(&entry.target))
    {
        concrete
            .into_iter()
            .filter(|entry| !crate::platform::windows_rules::is_updater_path(&entry.target))
            .collect::<Vec<_>>()
    } else {
        concrete
    };
    candidates
        .into_iter()
        .min_by_key(|entry| {
            (
                entry.source.priority(),
                entry.target.to_string_lossy().to_lowercase(),
                entry.arguments.clone(),
                entry.working_directory.clone(),
            )
        })
        .or_else(|| {
            item.launch_entries.iter().min_by_key(|entry| {
                (
                    entry.source.priority(),
                    entry.target.to_string_lossy().to_lowercase(),
                    entry.arguments.clone(),
                    entry.working_directory.clone(),
                )
            })
        })
}

pub(crate) fn is_default_launch_entry(item: &SearchResult, entry: &LaunchEntry) -> bool {
    preferred_launch_entry(item)
        .is_some_and(|default| launch_behavior_key(default) == launch_behavior_key(entry))
}

fn refresh_display_location(item: &mut SearchResult) {
    if item.category == ResultCategory::ControlPanel {
        return;
    }
    if let Some(target) = preferred_launch_entry(item).map(|entry| entry.target.clone()) {
        item.location = target.display().to_string();
    }
}

fn is_shortcut_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case(crate::platform::windows_rules::SHORTCUT_EXTENSION)
    })
}

fn target_stem_alias(title: &str, target: &Path) -> Vec<String> {
    let Some(stem) = target.file_stem().and_then(|value| value.to_str()) else {
        return Vec::new();
    };
    if !stem.is_empty() && !stem.eq_ignore_ascii_case(title) {
        vec![stem.to_owned()]
    } else {
        Vec::new()
    }
}

fn apps_folder_identity(target: &Path, arguments: Option<&str>) -> Option<String> {
    let value = apps_folder_value(target, arguments)?;
    if value.contains('!') && !value.contains(':') {
        Some(aumid_identity(&value))
    } else {
        Some(format!("shell:{}", value.to_lowercase()))
    }
}

fn apps_folder_value(target: &Path, arguments: Option<&str>) -> Option<String> {
    let target = target.to_string_lossy();
    shell_apps_folder_value(target.as_ref())
        .or_else(|| arguments.and_then(shell_apps_folder_value))
        .map(str::to_owned)
}

fn shell_apps_folder_value(value: &str) -> Option<&str> {
    let value = value.trim_matches(|character: char| character.is_whitespace() || character == '"');
    let prefix = "shell:AppsFolder\\";
    value
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .map(|_| value[prefix.len()..].trim())
        .filter(|value| !value.is_empty())
}

fn is_shell_namespace(path: &Path) -> bool {
    crate::platform::windows_rules::is_shell_namespace(path)
}

fn same_filesystem_target(left: &Path, right: &Path) -> bool {
    !is_shell_namespace(left)
        && !is_shell_namespace(right)
        && !looks_like_uri(left)
        && !looks_like_uri(right)
        && normalize_path(left) == normalize_path(right)
}

fn looks_like_uri(path: &Path) -> bool {
    Url::parse(path.to_string_lossy().trim())
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false)
}

fn normalize_path(path: &Path) -> String {
    let raw = path.to_string_lossy().trim().replace('/', "\\");
    let is_unc = raw.starts_with("\\\\");
    let mut parts = Vec::new();
    for part in raw.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value),
        }
    }
    let prefix = if is_unc { "\\\\" } else { "" };
    format!("{prefix}{}", parts.join("\\")).to_lowercase()
}

fn executable_identity(path: &Path) -> String {
    format!("exe:{}", normalize_path(path))
}

fn target_identity(path: &Path) -> String {
    let prefix = path
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| extension.eq_ignore_ascii_case("exe"))
        .map(|_| "exe")
        .unwrap_or("target");
    format!("{prefix}:{}", normalize_path(path))
}

fn shortcut_icon_target(
    shortcut_path: &Path,
    icon_path: Option<&Path>,
    resolved_target: &Path,
) -> PathBuf {
    let Some(icon_path) = icon_path else {
        return resolved_target.to_owned();
    };
    let expanded = expand_windows_variables(icon_path.to_owned());
    if expanded.is_absolute() {
        expanded
    } else {
        shortcut_path
            .parent()
            .map(|parent| parent.join(&expanded))
            .unwrap_or(expanded)
    }
}

fn aumid_identity(aumid: &str) -> String {
    format!("aumid:{}", aumid.trim().to_lowercase())
}

fn merge_by_identity(items: &mut Vec<SearchResult>) {
    let mut grouped = HashMap::<String, SearchResult>::new();
    for mut item in std::mem::take(items) {
        // Establish the launch-entry invariant for records that arrive here
        // without passing through file association (including provider tests
        // and future providers with multiple entries per record).
        dedup_launch_entries_by_behavior(&mut item.launch_entries);
        let mut aliases = std::mem::take(&mut item.aliases);
        aliases.push(item.title.clone());
        if item.identity.starts_with("exe:") {
            if let Some(stem) = item
                .launch_target
                .file_stem()
                .and_then(|value| value.to_str())
            {
                aliases.push(stem.to_owned());
            }
        }
        if let Some(aumid) = item.identity.strip_prefix("aumid:") {
            if let Some(product) = aumid
                .split('!')
                .next()
                .and_then(|value| value.split('_').next())
            {
                let product = product.replace(['.', '_'], " ");
                aliases.extend(product.split_whitespace().map(str::to_owned));
                aliases.push(product);
            }
        }
        match grouped.get_mut(&item.identity) {
            Some(existing) => {
                if existing.category == ResultCategory::Application
                    && item.category == ResultCategory::Website
                {
                    existing.category = ResultCategory::Website;
                }
                if existing.icon.is_none() {
                    existing.icon = item.icon.clone();
                }
                existing.aliases.extend(aliases);
                existing.sources.extend(item.sources);
                merge_launch_entries(existing, item.launch_entries);
                if source_priority(&item.source) < source_priority(&existing.source) {
                    let preserved_aliases = std::mem::take(&mut existing.aliases);
                    let preserved_sources = std::mem::take(&mut existing.sources);
                    let preserved_launch_entries = std::mem::take(&mut existing.launch_entries);
                    if item.icon.is_none() {
                        item.icon = existing.icon.take();
                    }
                    item.aliases = preserved_aliases;
                    item.sources = preserved_sources;
                    item.launch_entries = preserved_launch_entries;
                    *existing = item;
                }
            }
            None => {
                item.aliases = aliases;
                grouped.insert(item.identity.clone(), item);
            }
        }
    }
    let mut merged: Vec<_> = grouped.into_values().collect();
    for item in &mut merged {
        item.aliases.sort_by_key(|alias| alias.to_lowercase());
        item.aliases
            .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        item.sources
            .sort_by_key(|source| source.as_str().to_lowercase());
        item.sources.dedup();
        refresh_application_location(item);
        refresh_launch_actions(item);
    }
    merged.sort_by_cached_key(|item| item.title.to_lowercase());
    *items = merged;
}

fn source_priority(source: &crate::platform::SourceId) -> u8 {
    source.priority()
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
            icon: None,
            ranking_bias: 0,
        }
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
        assert_eq!(items[0].actions.len(), 2);
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
            .iter()
            .any(|source| *source == crate::platform::SourceId::StartMenu));
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
            .iter()
            .any(|source| *source == crate::platform::SourceId::StartMenu));
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
        assert!(is_shell_namespace(shell));
        assert!(!is_shell_namespace(file));
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
        refresh_display_location(&mut application);
        assert_eq!(application.location, r"C:\Apps\Demo.exe");
        assert_eq!(application.launch_target, PathBuf::from("target.exe"));
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
            .iter()
            .any(|source| *source == crate::platform::SourceId::StartMenu));
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
