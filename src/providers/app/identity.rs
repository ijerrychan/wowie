//! Application identity, path normalization, and merge rules.

use crate::result::{ResultCategory, SearchResult};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use url::Url;

use super::{
    debug_enabled, debug_record_matches, dedup_launch_entries_by_behavior,
    expand_windows_variables, merge_launch_entries, refresh_application_location,
};

pub(super) fn target_stem_alias(title: &str, target: &Path) -> Vec<String> {
    let Some(stem) = target.file_stem().and_then(|value| value.to_str()) else {
        return Vec::new();
    };
    if !stem.is_empty() && !stem.eq_ignore_ascii_case(title) {
        vec![stem.to_owned()]
    } else {
        Vec::new()
    }
}

fn merge_metadata(preferred: &mut SearchResult, other: &SearchResult) {
    if preferred.category == ResultCategory::Unclassified {
        preferred.category = other.category;
    } else if preferred.category == ResultCategory::Application
        && other.category == ResultCategory::Website
    {
        preferred.category = ResultCategory::Website;
    }

    if preferred.icon.is_none() {
        preferred.icon = other.icon.clone();
    }
    if preferred.detail.is_none() {
        preferred.detail = other.detail.clone();
    }
    if preferred.description.is_none() {
        preferred.description = other.description.clone();
    }
    if preferred.app_user_model_id.is_none() {
        preferred.app_user_model_id = other.app_user_model_id.clone();
    }
    if preferred.package_family_name.is_none() {
        preferred.package_family_name = other.package_family_name.clone();
    }
    if preferred.package_metadata.is_none() {
        preferred.package_metadata = other.package_metadata.clone();
    }
    if preferred.ranking_bias == 0 {
        preferred.ranking_bias = other.ranking_bias;
    }
    for action in &other.actions {
        if !preferred.actions.contains(action) {
            preferred.actions.push(action.clone());
        }
    }
}

pub(super) fn apps_folder_identity(target: &Path, arguments: Option<&str>) -> Option<String> {
    let value = apps_folder_value(target, arguments)?;
    if value.contains('!') && !value.contains(':') {
        Some(aumid_identity(&value))
    } else {
        Some(format!("shell:{}", value.to_lowercase()))
    }
}

pub(super) fn apps_folder_value(target: &Path, arguments: Option<&str>) -> Option<String> {
    let target = target.to_string_lossy();
    shell_apps_folder_value(target.as_ref())
        .or_else(|| arguments.and_then(shell_apps_folder_value))
        .map(str::to_owned)
}

pub(super) fn shell_apps_folder_value(value: &str) -> Option<&str> {
    let value = value.trim_matches(|character: char| character.is_whitespace() || character == '"');
    let prefix = "shell:AppsFolder\\";
    value
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .map(|_| value[prefix.len()..].trim())
        .filter(|value| !value.is_empty())
}

pub(super) fn same_filesystem_target(left: &Path, right: &Path) -> bool {
    filesystem_target_key(left)
        .zip(filesystem_target_key(right))
        .is_some_and(|(left, right)| left == right)
}

pub(super) fn filesystem_target_key(path: &Path) -> Option<String> {
    (!crate::platform::windows_rules::is_shell_namespace(path) && !looks_like_uri(path))
        .then(|| normalize_path(path))
}

pub(super) fn looks_like_uri(path: &Path) -> bool {
    Url::parse(path.to_string_lossy().trim())
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false)
}

pub(super) fn normalize_path(path: &Path) -> String {
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

pub(super) fn executable_identity(path: &Path) -> String {
    format!("exe:{}", normalize_path(path))
}

pub(super) fn target_identity(path: &Path) -> String {
    let prefix = path
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| extension.eq_ignore_ascii_case("exe"))
        .map(|_| "exe")
        .unwrap_or("target");
    format!("{prefix}:{}", normalize_path(path))
}

pub(super) fn shortcut_icon_target(
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

pub(super) fn aumid_identity(aumid: &str) -> String {
    format!("aumid:{}", aumid.trim().to_lowercase())
}

pub(super) fn packaged_identity_fields(identity: &str) -> (Option<String>, Option<String>) {
    let Some(aumid) = identity.strip_prefix("aumid:") else {
        return (None, None);
    };
    let aumid = aumid.trim().to_lowercase();
    let family_name = aumid
        .split('!')
        .next()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    (Some(aumid), family_name)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct IdentityMergeStats {
    pub merged_records: usize,
    pub launch_entries_removed: usize,
}

pub(super) fn merge_by_identity(items: &mut Vec<SearchResult>) -> IdentityMergeStats {
    let input_count = items.len();
    let mut grouped = HashMap::<String, SearchResult>::with_capacity(items.len());
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
                let incoming_is_preferred =
                    source_priority(&item.source) < source_priority(&existing.source);
                let debug_merge = debug_enabled()
                    && (debug_record_matches(existing) || debug_record_matches(&item));
                if debug_merge {
                    eprintln!(
                        "[provider] identity merge identity={:?} existing_source={} incoming_source={} canonical_source={} canonical_replaced={} existing_launch_entries={} incoming_launch_entries={}",
                        item.identity,
                        existing.source,
                        item.source,
                        if incoming_is_preferred {
                            &item.source
                        } else {
                            &existing.source
                        },
                        incoming_is_preferred,
                        existing.launch_entries.len(),
                        item.launch_entries.len(),
                    );
                }
                existing.aliases.extend(aliases);
                existing.sources.extend(item.sources.iter().cloned());
                merge_launch_entries(existing, item.launch_entries.clone());

                if incoming_is_preferred {
                    let preserved_aliases = std::mem::take(&mut existing.aliases);
                    let preserved_sources = std::mem::take(&mut existing.sources);
                    let preserved_launch_entries = std::mem::take(&mut existing.launch_entries);
                    merge_metadata(&mut item, existing);
                    item.aliases = preserved_aliases;
                    item.sources = preserved_sources;
                    item.launch_entries = preserved_launch_entries;
                    *existing = item;
                } else {
                    merge_metadata(existing, &item);
                }
            }
            None => {
                item.aliases = aliases;
                grouped.insert(item.identity.clone(), item);
            }
        }
    }
    let mut merged: Vec<_> = grouped.into_values().collect();
    let merged_records = input_count.saturating_sub(merged.len());
    let mut launch_entries_removed = 0;
    for item in &mut merged {
        // Deduplicate only after all records with this identity have been
        // merged. Doing it earlier repeats the same normalization work for
        // every intermediate merge.
        let launch_entries_before = item.launch_entries.len();
        dedup_launch_entries_by_behavior(&mut item.launch_entries);
        let removed = launch_entries_before - item.launch_entries.len();
        launch_entries_removed += removed;
        if removed > 0 && debug_enabled() && debug_record_matches(item) {
            eprintln!(
                "[provider] launch behavior dedup identity={:?} title={:?} source={} before={} after={} removed={}",
                item.identity,
                item.title,
                item.source,
                launch_entries_before,
                item.launch_entries.len(),
                removed,
            );
        }
        item.aliases
            .sort_by_cached_key(|alias| alias.to_lowercase());
        item.aliases
            .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        item.sources
            .sort_by_cached_key(|source| source.as_str().to_lowercase());
        item.sources.dedup();
        refresh_application_location(item);
    }
    merged.sort_by_cached_key(|item| item.title.to_lowercase());
    *items = merged;
    IdentityMergeStats {
        merged_records,
        launch_entries_removed,
    }
}

pub(super) fn source_priority(source: &crate::platform::SourceId) -> u8 {
    source.priority()
}
