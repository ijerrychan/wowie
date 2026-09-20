use crate::result::{LaunchEntry, ResultCategory, SearchResult};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use url::Url;

use super::{
    debug_enabled, debug_record_matches, filesystem_target_key, looks_like_uri, normalize_path,
    normalize_url, same_filesystem_target, source_priority, website_url,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct AssociationStats {
    pub associated: usize,
    pub ambiguous_rejections: usize,
}

pub(super) fn associate_file_entries(items: &mut Vec<SearchResult>) -> AssociationStats {
    let mut unified = Vec::with_capacity(items.len());
    let mut application_targets: HashMap<String, Vec<usize>> = HashMap::new();
    let mut stats = AssociationStats::default();
    for item in std::mem::take(items) {
        let is_file_entry = is_file_source(&item.source);
        let (match_index, candidate_count) = if is_file_entry && !is_remote_website(&item) {
            select_file_association(&item, &unified, &application_targets)
        } else {
            (None, 0)
        };

        if let Some(index) = match_index {
            stats.associated += 1;
            let application = &mut unified[index];
            if application.icon.is_none() {
                application.icon = item.icon.clone();
            }
            application.aliases.extend(item.aliases);
            application.sources.extend(item.sources);
            merge_launch_entries(application, item.launch_entries);
        } else {
            let index = unified.len();
            if is_store_application(&item) {
                for entry in &item.launch_entries {
                    if let Some(key) = filesystem_target_key(&entry.target) {
                        let matches = application_targets.entry(key).or_default();
                        if !matches.contains(&index) {
                            matches.push(index);
                        }
                    }
                }
            }
            unified.push(item);
            if candidate_count > 0 {
                stats.ambiguous_rejections += 1;
            }
        }
    }
    for item in &mut unified {
        refresh_application_location(item);
    }
    *items = unified;
    stats
}

fn is_store_application(item: &SearchResult) -> bool {
    item.source.is_store_application()
}

fn is_remote_website(item: &SearchResult) -> bool {
    if website_url(&item.launch_target, None).is_some() {
        return true;
    }
    item.launch_entries
        .iter()
        .any(|entry| website_url(&entry.target, Some(&entry.arguments)).is_some())
}

fn is_file_source(source: &crate::platform::SourceId) -> bool {
    source.is_file_source()
}

fn select_file_association(
    item: &SearchResult,
    applications: &[SearchResult],
    application_targets: &HashMap<String, Vec<usize>>,
) -> (Option<usize>, usize) {
    let mut candidates = HashMap::<usize, MatchStrength>::new();
    for entry in &item.launch_entries {
        let strong_matches = filesystem_target_key(&entry.target)
            .and_then(|key| application_targets.get(&key))
            .cloned()
            .unwrap_or_default();
        if !strong_matches.is_empty() {
            for index in strong_matches {
                if !package_identity_conflicts(item, &applications[index]) {
                    candidates.insert(index, MatchStrength::FilesystemTarget);
                }
            }
            continue;
        }

        // Virtual Shell entries and constrained title fallbacks do not have a
        // filesystem key. Keep the slower compatibility path only for those
        // cases, but evaluate every launch entry before deciding.
        for (index, application) in applications.iter().enumerate() {
            if !is_store_application(application) {
                continue;
            }
            if let Some(strength) = shortcut_match_strength(item, entry, application) {
                candidates
                    .entry(index)
                    .and_modify(|current| {
                        if strength.is_strong() {
                            *current = strength;
                        }
                    })
                    .or_insert(strength);
            }
        }
    }
    let selected = select_association_candidate(
        candidates
            .iter()
            .map(|(&index, &strength)| (index, strength)),
    );
    let debug_association = (debug_enabled() || std::env::var_os("WOWIE_PROVIDER_DEBUG").is_some())
        && (debug_record_matches(item)
            || candidates
                .keys()
                .any(|&index| debug_record_matches(&applications[index])));
    if debug_association {
        match selected {
            Some(index) => eprintln!(
                "[provider] association item={:?} application={:?} evidence={:?} candidates={}",
                item.title,
                applications[index].title,
                candidates[&index],
                candidates.len(),
            ),
            None if !candidates.is_empty() => eprintln!(
                "[provider] association rejected item={:?} candidates={}",
                item.title,
                candidates.len()
            ),
            None => {}
        }
    }
    (selected, candidates.len())
}

fn supports_exact_title_association(target: &Path) -> bool {
    target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("ico")
        })
}

fn package_identity_conflicts(shortcut: &SearchResult, application: &SearchResult) -> bool {
    let aumid_conflicts = shortcut
        .app_user_model_id
        .as_deref()
        .zip(application.app_user_model_id.as_deref())
        .is_some_and(|(left, right)| !left.eq_ignore_ascii_case(right));
    let family_conflicts = shortcut
        .package_family_name
        .as_deref()
        .zip(application.package_family_name.as_deref())
        .is_some_and(|(left, right)| !left.eq_ignore_ascii_case(right));
    aumid_conflicts || family_conflicts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchStrength {
    /// The shortcut and application expose the same authoritative AUMID.
    Aumid,
    /// The normalized identity strings are exactly equal.
    ExactIdentity,
    /// The resolved filesystem targets are equal.
    FilesystemTarget,
    /// A constrained title/virtual identity fallback.
    TitleFallback,
}

impl MatchStrength {
    fn is_strong(self) -> bool {
        !matches!(self, Self::TitleFallback)
    }
}

fn shortcut_match_strength(
    shortcut: &SearchResult,
    entry: &LaunchEntry,
    application: &SearchResult,
) -> Option<MatchStrength> {
    if package_identity_conflicts(shortcut, application) {
        return None;
    }
    // AUMID is authoritative when the shortcut exposes one. Conflicting
    // AUMIDs must never be merged through a weaker executable match.
    if shortcut.identity.starts_with("aumid:") {
        return (shortcut.identity == application.identity).then_some(MatchStrength::Aumid);
    }
    if shortcut.identity == application.identity {
        return Some(MatchStrength::ExactIdentity);
    }
    if application.identity.starts_with("aumid:") {
        return application
            .launch_entries
            .iter()
            .any(|app_entry| same_filesystem_target(&app_entry.target, &entry.target))
            .then_some(MatchStrength::FilesystemTarget);
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
        if crate::platform::windows_rules::is_legacy_tools_path(&entry.path)
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
        .then_some(MatchStrength::FilesystemTarget)
}

fn select_association_candidate(
    candidates: impl IntoIterator<Item = (usize, MatchStrength)>,
) -> Option<usize> {
    let mut strong = None;
    let mut strong_count = 0;
    let mut fallback = None;
    let mut fallback_count = 0;

    for (index, strength) in candidates {
        match strength {
            MatchStrength::Aumid
            | MatchStrength::ExactIdentity
            | MatchStrength::FilesystemTarget => {
                strong = Some(index);
                strong_count += 1;
            }
            MatchStrength::TitleFallback => {
                fallback = Some(index);
                fallback_count += 1;
            }
        }
    }

    if strong_count > 1 {
        return None;
    }
    strong.or_else(|| (fallback_count == 1).then_some(fallback).flatten())
}

/// Merge launch provenance at the same point as application-record merging.
/// The caller decides whether two records represent the same application;
/// this helper then keeps only distinct launch behavior within that record.
pub(super) fn merge_launch_entries(existing: &mut SearchResult, incoming: Vec<LaunchEntry>) {
    existing.launch_entries.extend(incoming);
}

/// Keep one entry for each actual launch behavior. The discovered source and
/// shortcut path are provenance metadata; they do not change what Windows
/// launches. Arguments and working directory do change the behavior and must
/// therefore remain part of the deduplication key.
pub(super) fn dedup_launch_entries_by_behavior(entries: &mut Vec<LaunchEntry>) {
    let mut keyed = entries
        .drain(..)
        .map(|entry| {
            let behavior = launch_behavior_key(&entry);
            let source_name = entry.source.as_str().into_owned();
            let path = entry.path.clone();
            (
                behavior,
                source_priority(&entry.source),
                source_name,
                path,
                entry,
            )
        })
        .collect::<Vec<_>>();

    keyed.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
    });
    keyed.dedup_by(|left, right| left.0 == right.0);
    entries.extend(keyed.into_iter().map(|(_, _, _, _, entry)| entry));
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
    if !crate::platform::windows_rules::is_shell_namespace(target) && !looks_like_uri(target) {
        if let Some(parent) = target.parent() {
            return normalize_path(parent);
        }
    }
    String::new()
}

pub(super) fn refresh_application_location(item: &mut SearchResult) {
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
    if is_store_application(item) || !crate::platform::windows_rules::is_shortcut_path(&path) {
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
    let is_concrete = |entry: &&LaunchEntry| {
        !crate::platform::windows_rules::is_shell_namespace(&entry.target)
            && !looks_like_uri(&entry.target)
    };
    let has_non_updater = item
        .launch_entries
        .iter()
        .filter(is_concrete)
        .any(|entry| !crate::platform::windows_rules::is_updater_path(&entry.target));

    item.launch_entries
        .iter()
        .filter(is_concrete)
        .filter(|entry| {
            !has_non_updater || !crate::platform::windows_rules::is_updater_path(&entry.target)
        })
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

pub(crate) fn same_launch_behavior(left: &LaunchEntry, right: &LaunchEntry) -> bool {
    launch_behavior_key(left) == launch_behavior_key(right)
}
