//! Windows application index based on the legacy provider.
//!
//! The index intentionally has a small, deterministic set of sources:
//! Start Menu and Desktop shortcuts/URLs/executables plus AppsFolder/Package
//! applications. Shortcuts are launched as shortcuts, but are de-duplicated
//! by their resolved target.

use crate::result::{Action, LaunchEntry, SearchResult};
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};
use url::Url;

pub struct AppProvider {
    items: Vec<SearchResult>,
}

impl AppProvider {
    pub fn discover() -> Self {
        let mut items = Vec::new();
        // AppsFolder/Package records are the application index. File-based
        // sources are added afterwards as launch-entry candidates.
        collect_store_apps(&mut items);
        for root in start_menu_roots() {
            collect_start_menu(&root, &mut items);
        }
        for root in desktop_roots() {
            collect_desktop(&root, &mut items);
        }
        associate_file_entries(&mut items);
        items.sort_by(|left, right| {
            source_priority(&left.source)
                .cmp(&source_priority(&right.source))
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
                .then_with(|| left.identity.cmp(&right.identity))
                .then_with(|| left.location.cmp(&right.location))
        });
        merge_by_identity(&mut items);
        debug_records("final merged records", &items);
        Self { items }
    }

    pub fn results(self) -> Vec<SearchResult> {
        self.items
    }
}

fn debug_enabled() -> bool {
    env::var_os("WOWIE_PROVIDER_DEBUG").is_some_and(|value| value != "0")
}

fn debug_records(stage: &str, records: &[SearchResult]) {
    if !debug_enabled() {
        return;
    }
    let relevant: Vec<_> = records
        .iter()
        .filter(|record| record.sources.len() > 1)
        .collect();
    eprintln!("[provider] {stage}: {} merged records", relevant.len());
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
        if debug_enabled() {
            eprintln!(
                "[provider] store app title={:?} identity={:?} launch_target={:?} executable={:?}",
                app.title, app.identity, app.launch_target, app.executable
            );
        }
        let launch_target = app.launch_target.clone();
        output.push(SearchResult {
            id: app.identity.clone(),
            identity: app.identity,
            title: app.title,
            aliases: app.aliases,
            location: launch_target.display().to_string(),
            launch_target,
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

fn collect_start_menu(root: &Path, output: &mut Vec<SearchResult>) {
    collect_tree(root, output, false, "StartMenu");
}

fn collect_desktop(root: &Path, output: &mut Vec<SearchResult>) {
    collect_tree(root, output, true, "Desktop");
}

fn collect_tree(
    root: &Path,
    output: &mut Vec<SearchResult>,
    include_executables: bool,
    source: &str,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.path().to_string_lossy().to_lowercase());
    for entry in entries {
        let path = entry.path();
        // This mirrors WalkDir::follow_links(false) used by the legacy index.
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            collect_tree(&path, output, include_executables, source);
            continue;
        }

        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        let is_shortcut = extension.eq_ignore_ascii_case("lnk");
        let is_url = extension.eq_ignore_ascii_case("url");
        let is_executable = include_executables && extension.eq_ignore_ascii_case("exe");
        if is_executable {
            let Some(title) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let identity = executable_identity(&path);
            output.push(SearchResult {
                id: identity.clone(),
                identity,
                title: title.to_owned(),
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
            });
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
            });
            continue;
        }
        let shortcut = crate::platform::resolve_shortcut(&path);
        let Some(resolved_target) = shortcut.target.map(expand_windows_variables) else {
            continue;
        };
        if debug_enabled() {
            eprintln!(
                "[provider] shortcut source={source} title={title:?} path={path:?} target={resolved_target:?} args={:?}",
                shortcut.arguments
            );
        }
        let identity = if let Some(identity) =
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
        });
    }
}

fn normalize_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value.trim()).ok()?;
    url.set_fragment(None);
    Some(url.to_string())
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
        let match_index = if is_file_entry {
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
            application.aliases.extend(item.aliases);
            application.sources.extend(item.sources);
            application.launch_entries.extend(item.launch_entries);
        } else {
            unified.push(item);
        }
    }
    for item in &mut unified {
        item.sources.sort_by_key(|source| source.to_lowercase());
        item.sources
            .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        item.launch_entries.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.arguments.cmp(&right.arguments))
        });
        item.launch_entries.dedup();
        refresh_application_location(item);
        refresh_launch_actions(item);
    }
    *items = unified;
}

fn is_store_application(item: &SearchResult) -> bool {
    item.source == "AppsFolder" || item.source == "Package"
}

fn is_file_source(source: &str) -> bool {
    matches!(source, "StartMenu" | "Desktop")
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
    if application.identity.starts_with("shell:") {
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
        let shell_identity = compact(application.identity.trim_start_matches("shell:"));
        if !title.is_empty()
            && shortcut.title.eq_ignore_ascii_case(&application.title)
            && shell_identity.contains(&title)
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

fn refresh_application_location(item: &mut SearchResult) {
    if !is_store_application(item) {
        return;
    }
    if let Some(entry) = item
        .launch_entries
        .iter()
        .filter(|entry| !is_shell_namespace(&entry.target) && !looks_like_uri(&entry.target))
        .min_by_key(|entry| {
            let is_update = entry
                .target
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.eq_ignore_ascii_case("update"));
            (
                is_update,
                entry.source != "AppsFolder",
                entry.target.to_string_lossy().to_lowercase(),
            )
        })
    {
        item.location = entry.target.display().to_string();
        // Use the concrete app executable as the default launch target. Keep
        // updater/alternate launchers in launch_entries/actions, but do not
        // let an updater determine the default icon or primary launch.
        item.launch_target = entry.target.clone();
        item.launch_arguments = entry.arguments.clone();
        item.launch_working_directory = entry.working_directory.clone();
    } else {
        item.location = item.launch_target.display().to_string();
    }
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
    path.to_string_lossy()
        .trim()
        .get(.."shell:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:"))
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

fn aumid_identity(aumid: &str) -> String {
    format!("aumid:{}", aumid.trim().to_lowercase())
}

fn merge_by_identity(items: &mut Vec<SearchResult>) {
    let mut grouped = HashMap::<String, SearchResult>::new();
    for mut item in std::mem::take(items) {
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
                existing.aliases.extend(aliases);
                existing.sources.extend(item.sources);
                existing.launch_entries.extend(item.launch_entries);
                if source_priority(&item.source) < source_priority(&existing.source) {
                    let preserved_aliases = std::mem::take(&mut existing.aliases);
                    let preserved_sources = std::mem::take(&mut existing.sources);
                    let preserved_launch_entries = std::mem::take(&mut existing.launch_entries);
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
        item.sources.sort_by_key(|source| source.to_lowercase());
        item.sources
            .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        item.launch_entries.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.arguments.cmp(&right.arguments))
        });
        item.launch_entries.dedup();
        refresh_application_location(item);
        refresh_launch_actions(item);
    }
    merged.sort_by_cached_key(|item| item.title.to_lowercase());
    *items = merged;
}

fn source_priority(source: &str) -> u8 {
    match source {
        "AppsFolder" => 0,
        "Package" => 1,
        "StartMenu" => 2,
        "Desktop" => 3,
        "ExecutionAlias" => 4,
        _ => 4,
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
            aliases: vec![],
            location: String::new(),
            launch_target: PathBuf::from("target.exe"),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: source.into(),
            sources: vec![source.into()],
            launch_entries: vec![],
            actions: Vec::new(),
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
        assert_eq!(SearchCore::new(vec![item]).search("notepad", 1).len(), 1);
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
        assert_eq!(items[0].source, "AppsFolder");
        assert_eq!(items[0].launch_entries.len(), 2);
        assert!(items[0].sources.iter().any(|source| source == "StartMenu"));
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
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source, "AppsFolder");
        assert!(items[0].sources.iter().any(|source| source == "StartMenu"));
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
        assert_eq!(items[0].source, "AppsFolder");
        assert_eq!(items[0].launch_entries.len(), 3);
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
        assert_eq!(items[0].sources, vec!["Desktop", "StartMenu"]);
        assert_eq!(items[0].launch_entries.len(), 2);
        assert!(items[0]
            .aliases
            .iter()
            .any(|alias| alias == "desktop alias"));
        assert!(items[0].aliases.iter().any(|alias| alias == "start alias"));
    }

    #[test]
    fn local_title_and_aumid_product_remain_searchable_after_merge() {
        let mut item = item("aumid:vendor.calculator_abc!main", "计算器", "AppsFolder");
        item.launch_target = PathBuf::from("shell:AppsFolder\\Vendor.Calculator_abc!Main");
        let mut entries = vec![item];
        merge_by_identity(&mut entries);
        let search = SearchCore::new(entries);
        assert_eq!(search.search("计算器", 1).len(), 1);
        assert_eq!(search.search("calculator", 1).len(), 1);
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
