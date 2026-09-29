//! Application source discovery, file parsing, and provider diagnostics.

use crate::{
    platform::source_ids,
    result::{IconSpec, LaunchEntry, ResultCategory, SearchResult},
};
use std::{
    env, fs,
    path::{Path, PathBuf},
};
use url::Url;

use super::{
    apps_folder_identity, executable_identity, normalize_path, packaged_identity_fields,
    shortcut_icon_target, target_identity, target_stem_alias,
};

pub(super) fn discover_apps_folder_source(
    _config: &crate::config::ProviderConfig,
) -> Vec<SearchResult> {
    let mut items = Vec::new();
    collect_store_apps(
        &mut items,
        crate::platform::discover_apps_folder_applications(),
    );
    items
}

pub(super) fn discover_packaged_source(
    _config: &crate::config::ProviderConfig,
) -> Vec<SearchResult> {
    let mut items = Vec::new();
    collect_store_apps(
        &mut items,
        crate::platform::discover_packaged_applications(),
    );
    items
}

pub(super) fn discover_start_menu_source(
    _config: &crate::config::ProviderConfig,
) -> Vec<SearchResult> {
    discover_file_source(false, source_ids::START_MENU)
}

pub(super) fn discover_desktop_source(
    _config: &crate::config::ProviderConfig,
) -> Vec<SearchResult> {
    discover_file_source(true, source_ids::DESKTOP)
}

pub(super) fn discover_custom_directories(
    config: &crate::config::ProviderConfig,
) -> Vec<SearchResult> {
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

/// Cheap metadata-only fingerprint used to avoid parsing unchanged filesystem
/// sources after restoring the serialized index. File contents and shell
/// shortcut resolution are only revisited when a path or its metadata moves.
pub(super) fn filesystem_source_fingerprint(
    source: crate::platform::AppProviderSource,
    config: &crate::config::ProviderConfig,
) -> Option<u64> {
    let roots = match source {
        crate::platform::AppProviderSource::PackagedApps => {
            return crate::platform::package_inventory_fingerprint();
        }
        crate::platform::AppProviderSource::StartMenu => start_menu_roots(),
        crate::platform::AppProviderSource::Desktop => desktop_roots(),
        crate::platform::AppProviderSource::CustomDirectories => config.indexed_directories.clone(),
        crate::platform::AppProviderSource::StoreApps
        | crate::platform::AppProviderSource::AppsFolder
        | crate::platform::AppProviderSource::ControlPanel => return None,
    };
    Some(crate::utils::filesystem_fingerprint(&roots))
}

pub(super) fn discover_file_source(include_executables: bool, source: &str) -> Vec<SearchResult> {
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

pub(super) fn debug_enabled() -> bool {
    // Control Panel diagnostics use WOWIE_PROVIDER_DEBUG. Keep the verbose
    // application-provider trace behind a separate opt-in switch.
    env::var_os("WOWIE_ALL_PROVIDER_DEBUG").is_some_and(|value| value != "0")
}

pub(super) fn debug_filter_matches(value: &str) -> bool {
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

pub(super) fn debug_record_matches(record: &SearchResult) -> bool {
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

pub(super) fn debug_records(stage: &str, records: &[SearchResult]) {
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

pub(super) fn collect_store_apps(
    output: &mut Vec<SearchResult>,
    applications: Vec<crate::platform::StoreApplication>,
) {
    for app in applications {
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
                icon: None,
            }],
            actions: Vec::new(),
            detail: None,
            description: app.description,
            app_user_model_id: app.app_user_model_id,
            package_family_name: app.package_family_name,
            package_metadata: app.package_metadata,
            icon: None,
            ranking_bias: 0,
            allow_favorite: true,
            visibility: crate::result::ResultVisibility::Always,
        });
    }
}

pub(super) fn start_menu_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(appdata) = env::var("APPDATA") {
        roots.push(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(program_data) = env::var("ProgramData") {
        roots.push(PathBuf::from(program_data).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    roots
}

pub(super) fn desktop_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(user_profile) = env::var("USERPROFILE") {
        roots.push(PathBuf::from(user_profile).join("Desktop"));
    }
    if let Ok(public) = env::var("PUBLIC") {
        roots.push(PathBuf::from(public).join("Desktop"));
    }
    roots
}

pub(super) fn collect_tree(
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

    // Ordering is established once after all provider sources are collected
    // in `merge_sources`; sorting every directory here only repeats work.
    for entry in entries.flatten() {
        let path = entry.path();
        // Keep discovery and fingerprinting on the same no-follow-links policy.
        // Windows junctions are reparse points even when `file_type().is_dir()`
        // reports a directory, so skip them before recursing or indexing.
        if crate::utils::is_filesystem_link(&path) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
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
                    icon: None,
                }],
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
            let icon = Some(IconSpec::DefaultOpen(PathBuf::from(&url)));
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
                    icon: icon.clone(),
                }],
                actions: Vec::new(),
                detail: None,
                description: None,
                app_user_model_id: None,
                package_family_name: None,
                package_metadata: None,
                icon,
                ranking_bias: 0,
                allow_favorite: true,
                visibility: crate::result::ResultVisibility::Always,
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
        let (app_user_model_id, package_family_name) = packaged_identity_fields(&identity);
        let icon = website_url
            .as_ref()
            .map(|url| IconSpec::DefaultOpen(PathBuf::from(url)))
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
            });
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
                icon: icon.clone(),
            }],
            actions: Vec::new(),
            detail: None,
            description: None,
            app_user_model_id,
            package_family_name,
            package_metadata: None,
            icon,
            ranking_bias: 0,
            allow_favorite: true,
            visibility: crate::result::ResultVisibility::Always,
        });
    }
}

pub(super) fn generic_file_result(path: &Path, title: &str, source: &str) -> SearchResult {
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
            target: target.clone(),
            arguments: String::new(),
            working_directory: None,
            icon: Some(IconSpec::DefaultOpen(target.clone())),
        }],
        actions: Vec::new(),
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

pub(super) fn normalize_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value.trim()).ok()?;
    url.set_fragment(None);
    Some(url.to_string())
}

#[cfg(test)]
pub(super) fn is_website_shortcut(target: &Path, arguments: Option<&str>) -> bool {
    website_url(target, arguments).is_some()
}

pub(super) fn website_url(target: &Path, arguments: Option<&str>) -> Option<String> {
    normalized_http_url(&target.to_string_lossy()).or_else(|| {
        arguments
            .into_iter()
            .flat_map(|value| value.split_whitespace())
            .map(|value| value.trim_matches('"'))
            .find_map(normalized_http_url)
    })
}

pub(super) fn normalized_http_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

pub(super) fn expand_windows_variables(path: PathBuf) -> PathBuf {
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

pub(super) fn read_url_target(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .filter_map(|line| line.split_once('='))
        .find_map(|(key, value)| {
            key.trim()
                .eq_ignore_ascii_case("URL")
                .then(|| value.trim().to_owned())
        })
}
