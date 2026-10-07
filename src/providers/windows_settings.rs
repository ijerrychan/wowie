//! PowerToys-derived Windows Settings and legacy management catalog.
//!
//! Windows does not provide a supported API for enumerating Settings pages.
//! PowerToys' data catalog is the single source for indexed pages and legacy
//! entries. A small compatibility map redirects a built-in Control Panel item.

use crate::{
    platform::source_ids,
    result::{IconSpec, LaunchEntry, ResultCategory, SearchResult},
    utils::i18n,
};
use std::{collections::HashMap, path::PathBuf, sync::OnceLock};

const POWERTOYS_CONTEXT: &str = "Windows Settings";
const POWERTOYS_ID_PREFIX: &str = "windows-settings:powertoys:";
pub(crate) const SETTINGS_ICON_TARGET: &str = r"shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PowerToysSettingsFile {
    settings: Vec<PowerToysSetting>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PowerToysSetting {
    name: String,
    #[serde(default)]
    areas: Vec<String>,
    #[serde(rename = "Type")]
    kind: String,
    #[serde(default)]
    alt_names: Vec<String>,
    command: String,
    note: Option<String>,
    introduced_in_build: Option<u32>,
    deprecated_in_build: Option<u32>,
    #[serde(default)]
    app_homepage_score: i32,
}

struct PowerToysCatalog {
    settings: Vec<PowerToysSetting>,
    resources: HashMap<String, String>,
}

fn powertoys_catalog() -> &'static PowerToysCatalog {
    static CATALOG: OnceLock<PowerToysCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let settings: PowerToysSettingsFile =
            serde_json::from_str(include_str!("windows_settings_data/WindowsSettings.json"))
                .expect("vendored PowerToys Windows Settings data must be valid JSON");
        let resources: HashMap<String, String> =
            serde_json::from_str(include_str!("windows_settings_data/Resources.en.json"))
                .expect("vendored PowerToys English resources must be valid JSON");
        PowerToysCatalog {
            settings: settings.settings,
            resources,
        }
    })
}

fn resource_text(key: &str) -> String {
    powertoys_catalog()
        .resources
        .get(key)
        .cloned()
        .unwrap_or_else(|| key.to_owned())
}

fn localized_resource(language: &str, key: &str) -> String {
    let translated = i18n::tr_owned_in_context(language, POWERTOYS_CONTEXT, key);
    if translated != key {
        return translated;
    }
    resource_text(key)
}

fn resource_aliases(key: &str) -> Vec<String> {
    i18n::tr_aliases_all_languages_owned(POWERTOYS_CONTEXT, key)
}

fn append_unique_alias(aliases: &mut Vec<String>, value: String) {
    if !value.trim().is_empty()
        && !aliases
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&value))
    {
        aliases.push(value);
    }
}

fn translated_paths(setting: &PowerToysSetting) -> Vec<String> {
    let mut paths = vec![String::new()];
    for key in std::iter::once(&setting.kind).chain(setting.areas.iter()) {
        let parts = resource_aliases(key);
        let mut combined = Vec::new();
        for prefix in &paths {
            for part in &parts {
                let path = if prefix.is_empty() {
                    part.clone()
                } else {
                    format!("{prefix} › {part}")
                };
                append_unique_alias(&mut combined, path);
                if combined.len() >= 64 {
                    break;
                }
            }
            if combined.len() >= 64 {
                break;
            }
        }
        paths = combined;
    }
    paths
}

fn powertoys_aliases(setting: &PowerToysSetting) -> Vec<String> {
    let mut aliases = Vec::new();
    for key in std::iter::once(&setting.name)
        .chain(setting.areas.iter())
        .chain(setting.alt_names.iter())
    {
        for alias in resource_aliases(key) {
            append_unique_alias(&mut aliases, alias);
        }
    }
    for path in translated_paths(setting) {
        append_unique_alias(&mut aliases, path);
    }
    if setting.kind == "AppControlPanel" {
        append_unique_alias(&mut aliases, format!("Microsoft.{}", setting.name));
    }
    if let Some(note) = &setting.note {
        for alias in resource_aliases(note) {
            append_unique_alias(&mut aliases, alias);
        }
    }
    aliases
}

fn parse_powertoys_command(command: &str) -> (PathBuf, String) {
    let mut command = command.to_owned();
    if command.to_ascii_lowercase().contains("%windir%") {
        let windows_directory = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        command = command.replace("%windir%", &windows_directory.to_string_lossy());
    }
    if let Some(split_at) = command.find(char::is_whitespace) {
        let (target, arguments) = command.split_at(split_at);
        (PathBuf::from(target), arguments.trim().to_owned())
    } else {
        (PathBuf::from(command), String::new())
    }
}

fn powertoys_result(setting: &PowerToysSetting) -> SearchResult {
    let (target, arguments) = parse_powertoys_command(&setting.command);
    let category = match setting.kind.as_str() {
        "AppSettingsApp" => ResultCategory::Settings,
        _ => ResultCategory::ControlPanel,
    };
    let icon_target = match setting.kind.as_str() {
        "AppSettingsApp" => SETTINGS_ICON_TARGET,
        "AppMMC" => setting.command.as_str(),
        _ => "shell:ControlPanelFolder",
    };
    let path = translated_path(setting, "en");
    // A resource name can describe several distinct pages (Display, Speech,
    // Keyboard). Keep existing IDs for unique names, disambiguate collisions
    // by their invariant command instead of the translated title.
    let identity = if powertoys_catalog()
        .settings
        .iter()
        .filter(|entry| entry.name == setting.name)
        .count()
        > 1
    {
        format!(
            "{POWERTOYS_ID_PREFIX}{}|{}",
            setting.name,
            setting.command.to_ascii_lowercase()
        )
    } else {
        format!("{POWERTOYS_ID_PREFIX}{}", setting.name)
    };
    SearchResult {
        id: identity.clone(),
        identity,
        title: localized_resource("en", &setting.name),
        category,
        aliases: powertoys_aliases(setting),
        location: path,
        launch_target: target.clone(),
        launch_arguments: arguments.clone(),
        launch_working_directory: None,
        source: source_ids::CONTROL_PANEL.into(),
        sources: vec![source_ids::CONTROL_PANEL.into()],
        launch_entries: vec![LaunchEntry {
            source: source_ids::CONTROL_PANEL.into(),
            path: target.clone(),
            target,
            arguments,
            working_directory: None,
            icon: Some(IconSpec::System(PathBuf::from(icon_target))),
        }],
        actions: Vec::new(),
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: Some(IconSpec::System(PathBuf::from(icon_target))),
        ranking_bias: setting.app_homepage_score,
        allow_favorite: true,
        visibility: crate::result::ResultVisibility::Always,
    }
}

fn translated_path(setting: &PowerToysSetting, language: &str) -> String {
    std::iter::once(&setting.kind)
        .chain(setting.areas.iter())
        .map(|key| localized_resource(language, key))
        .collect::<Vec<_>>()
        .join(" › ")
}

#[cfg(windows)]
fn capability_requirement(setting: &PowerToysSetting) -> Option<&'static str> {
    match setting.command.as_str() {
        "ms-settings:network-cellular" | "ms-settings:network-wificalling" => {
            return Some("cellular")
        }
        "ms-settings:wheel" => return Some("surface_dial"),
        "ms-settings:network-directaccess" => return Some("direct_access"),
        "ms-settings:assignedaccess" | "gpedit.msc" | "secpol.msc" | "lusrmgr.msc" => {
            return Some("professional_edition")
        }
        _ => {}
    }
    match setting.name.as_str() {
        "BatterySaver" | "BatterySaverSettings" | "BatteryUse" => Some("battery"),
        "Touchpad" => Some("touchpad"),
        "WiFi" | "ManageKnownNetworks" | "WiFiCalling" => Some("wifi_adapter"),
        "AudioAndSpeech" | "Environment" | "HeadsetDisplay" | "Uninstall" => {
            Some("mixed_reality_portal")
        }
        _ => None,
    }
}

#[cfg(windows)]
fn available_on_build(setting: &PowerToysSetting, current_build: Option<u32>) -> bool {
    // Cortana's standalone Windows app was retired in 2023 and these old
    // Settings URIs are absent from Microsoft's current URI reference.
    if setting.command.starts_with("ms-settings:cortana") {
        return false;
    }

    let Some(current_build) = current_build else {
        return false;
    };

    // The upstream catalog incorrectly marks the still-documented Fonts page
    // as deprecated. Microsoft continues to list ms-settings:fonts.
    let deprecated_in_build = (setting.name != "Fonts")
        .then_some(setting.deprecated_in_build)
        .flatten();
    setting
        .introduced_in_build
        .is_none_or(|build| current_build >= build)
        && deprecated_in_build.is_none_or(|build| current_build < build)
        // Microsoft removed Windows Mixed Reality settings in Windows 11 24H2.
        && !(current_build >= 26_100
            && matches!(
                setting.name.as_str(),
                "AudioAndSpeech" | "Environment" | "HeadsetDisplay" | "Uninstall"
            ))
}

#[cfg(windows)]
fn available_on_device(setting: &PowerToysSetting) -> bool {
    capability_requirement(setting).is_none_or(|requirement| {
        crate::platform::windows_setting_capability_available(requirement) == Some(true)
    })
}

#[cfg(windows)]
pub(crate) fn powertoys_results() -> Vec<SearchResult> {
    let current_build = crate::platform::windows_build_number();
    let availability = crate::platform::SettingsAvailability::discover();
    let mut capabilities = HashMap::new();
    let mut targets = std::collections::HashSet::new();
    let mut results = powertoys_catalog()
        .settings
        .iter()
        .filter(|setting| {
            let device_available = *capabilities
                .entry(capability_requirement(setting))
                .or_insert_with(|| available_on_device(setting));
            available_on_build(setting, current_build)
                && device_available
                && match setting.kind.as_str() {
                    "AppSettingsApp" => availability.contains(&setting.command),
                    "AppMMC" => availability
                        .system_directory
                        .join(&setting.command)
                        .is_file(),
                    // Legacy applets come exclusively from Shell enumeration.
                    "AppControlPanel" => {
                        setting.name == "OpenControlPanel"
                            && availability.system_directory.join("control.exe").is_file()
                    }
                    _ => false,
                }
        })
        .map(powertoys_result)
        .map(|mut item| {
            if item.category == ResultCategory::Settings {
                if let Some(path) = &availability.settings_icon_path {
                    item.icon = Some(IconSpec::Resource(path.clone()));
                    for entry in &mut item.launch_entries {
                        entry.icon = item.icon.clone();
                    }
                }
            } else if item.category == ResultCategory::ControlPanel {
                item.launch_target = availability.system_directory.join(&item.launch_target);
                for entry in &mut item.launch_entries {
                    entry.target = item.launch_target.clone();
                    entry.path = item.launch_target.clone();
                }
                if item
                    .launch_target
                    .extension()
                    .is_some_and(|extension| extension == "msc")
                {
                    item.icon = Some(IconSpec::System(item.launch_target.clone()));
                }
            }
            item
        })
        .filter(|item| {
            targets.insert((
                item.launch_target.to_string_lossy().to_ascii_lowercase(),
                item.launch_arguments.clone(),
            ))
        })
        .collect::<Vec<_>>();
    results.sort_by(|left, right| left.title.cmp(&right.title));
    results
}

fn shell_catalog_setting(item: &SearchResult) -> Option<&'static PowerToysSetting> {
    if item.id.starts_with("control-panel-task:") {
        return None;
    }
    powertoys_catalog().settings.iter().find(|setting| {
        setting.kind == "AppControlPanel"
            && setting
                .command
                .strip_prefix("control /name ")
                .is_some_and(|canonical| {
                    item.aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(canonical))
                })
    })
}

pub(crate) fn enrich_shell_result(item: &mut SearchResult) {
    if let Some(setting) = shell_catalog_setting(item) {
        for alias in powertoys_aliases(setting) {
            append_unique_alias(&mut item.aliases, alias);
        }
    }
}

pub(crate) fn matches_settings_path(item: &SearchResult, query: &str) -> bool {
    if !query.contains('>') {
        return true;
    }
    let query_parts = query
        .split('>')
        .map(str::trim)
        .take_while(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if query_parts.is_empty() {
        return true;
    }
    let path_matches = |path: &str| {
        let path_parts = path.split('›').map(str::trim).collect::<Vec<_>>();
        query_parts.len() <= path_parts.len()
            && query_parts
                .iter()
                .zip(path_parts)
                .all(|(query_part, path_part)| {
                    path_part
                        .get(..query_part.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(query_part))
                })
    };
    path_matches(&item.location)
        || item
            .aliases
            .iter()
            .filter(|alias| alias.contains('›'))
            .any(|alias| path_matches(alias))
}

const COMPATIBLE_DESTINATIONS: &[(&str, &str)] = &[("Microsoft.Fonts", "ms-settings:fonts")];

fn compatible_settings_uri_for_canonical_name(name: &str) -> Option<&'static str> {
    COMPATIBLE_DESTINATIONS
        .iter()
        .find(|(canonical, _)| canonical.eq_ignore_ascii_case(name))
        .map(|(_, uri)| *uri)
}

pub(crate) fn localized_result(mut item: SearchResult, language: &str) -> SearchResult {
    if item.id.starts_with("control-panel-task:") {
        item.location = if item.location.is_empty() {
            localized_resource(language, "AppControlPanel")
        } else {
            format!(
                "{} → {}",
                localized_resource(language, "AppControlPanel"),
                item.location
            )
        };
        return item;
    }
    let setting = if let Some(key) = item.id.strip_prefix(POWERTOYS_ID_PREFIX) {
        let (name, command) = key
            .split_once('|')
            .map_or((key, None), |(name, command)| (name, Some(command)));
        powertoys_catalog().settings.iter().find(|setting| {
            setting.name == name
                && command.is_none_or(|command| command.eq_ignore_ascii_case(&setting.command))
        })
    } else {
        (item.category == ResultCategory::Settings)
            .then(|| {
                powertoys_catalog().settings.iter().find(|setting| {
                    setting.kind == "AppSettingsApp"
                        && item.launch_target.to_str() == Some(setting.command.as_str())
                })
            })
            .flatten()
            .or_else(|| shell_catalog_setting(&item))
    };
    if let Some(setting) = setting {
        item.title = localized_resource(language, &setting.name);
        item.location = translated_path(setting, language);
    }
    item
}

/// Redirect the legacy Fonts namespace item to Settings while retaining the
/// localized title and stable Shell identity supplied by Windows.
pub(crate) fn apply_compatible_destination(item: &mut SearchResult) {
    if item.category != ResultCategory::ControlPanel {
        return;
    }
    let Some(uri) = item
        .aliases
        .iter()
        .find_map(|alias| compatible_settings_uri_for_canonical_name(alias))
    else {
        return;
    };

    let icon = Some(IconSpec::System(PathBuf::from(SETTINGS_ICON_TARGET)));
    #[cfg(windows)]
    let icon = {
        let availability = crate::platform::SettingsAvailability::discover();
        if !availability.contains(uri) {
            return;
        }
        availability
            .settings_icon_path
            .map(IconSpec::Resource)
            .or(icon)
    };

    let target = PathBuf::from(uri);
    item.category = ResultCategory::Settings;
    item.launch_target = target.clone();
    item.launch_arguments.clear();
    item.launch_working_directory = None;
    item.icon = icon.clone();
    for entry in item
        .launch_entries
        .iter_mut()
        .filter(|entry| entry.source.as_str() == source_ids::CONTROL_PANEL)
    {
        entry.path = target.clone();
        entry.target = target.clone();
        entry.arguments.clear();
        entry.working_directory = None;
        entry.icon = icon.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_translations_match_original_command_palette_resources() {
        let chinese: HashMap<String, String> =
            serde_json::from_str(include_str!("windows_settings_data/Resources.zh-CN.json"))
                .unwrap();
        for setting in &powertoys_catalog().settings {
            for key in std::iter::once(&setting.name)
                .chain(std::iter::once(&setting.kind))
                .chain(&setting.areas)
                .chain(&setting.alt_names)
                .chain(&setting.note)
            {
                assert_eq!(localized_resource("en", key), resource_text(key), "{key}");
                assert_eq!(localized_resource("zh-CN", key), chinese[key], "{key}");
            }
        }
        assert_eq!(localized_resource("zh-CN", "WiFi"), "WLAN");
    }

    #[test]
    fn distinct_display_pages_keep_distinct_ids_and_localize_without_changing_targets() {
        let pages = powertoys_catalog()
            .settings
            .iter()
            .filter(|entry| entry.kind == "AppSettingsApp" && entry.name == "Display")
            .map(powertoys_result)
            .collect::<Vec<_>>();
        assert_eq!(pages.len(), 2);
        assert_ne!(pages[0].id, pages[1].id);
        for page in pages {
            let localized = localized_result(page.clone(), "zh-CN");
            assert_ne!(localized.title, page.title);
            assert_eq!(localized.id, page.id);
            assert_eq!(localized.launch_target, page.launch_target);
            assert!(localized.icon.is_some());
        }
    }

    #[cfg(windows)]
    #[test]
    fn build_boundaries_filter_windows_10_and_11_and_unknown_builds() {
        let setting = powertoys_catalog()
            .settings
            .iter()
            .find(|entry| entry.introduced_in_build == Some(22000))
            .unwrap();
        assert!(!available_on_build(setting, Some(19045)));
        assert!(available_on_build(setting, Some(22000)));
        assert!(!available_on_build(setting, None));
        let fonts = powertoys_catalog()
            .settings
            .iter()
            .find(|entry| entry.name == "Fonts" && entry.kind == "AppSettingsApp")
            .unwrap();
        assert!(available_on_build(fonts, Some(26100)));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads installed Windows pages and extracts native Shell icons"]
    fn installed_pages_and_icons() {
        let availability = crate::platform::SettingsAvailability::discover();
        let results =
            crate::providers::control_panel::discover(&crate::config::ProviderConfig::default());
        let mut ids = std::collections::HashSet::new();
        for result in &results {
            assert!(ids.insert(&result.id), "duplicate identity: {}", result.id);
            if result.category == ResultCategory::Settings {
                assert!(availability.contains(&result.launch_target.to_string_lossy()));
            }
        }
        let icon_service = crate::services::icon_service::IconService::new();
        let tasks = results
            .iter()
            .filter(|item| item.id.starts_with("control-panel-task:"))
            .collect::<Vec<_>>();
        assert!(
            !tasks.is_empty(),
            "All Tasks must expose installed Control Panel tasks"
        );
        for task in &tasks {
            assert!(crate::platform::shell_id_list::decode(&task.launch_target).is_some());
        }
        assert!(
            icon_service.resolve_at(tasks[0], 32).is_some(),
            "All Tasks icon must resolve from its captured PIDL"
        );
        let settings_result = results
            .iter()
            .find(|item| item.category == ResultCategory::Settings)
            .unwrap();
        let settings_icon = icon_service
            .resolve_at(settings_result, 32)
            .expect("Settings app icon must resolve through the shared service");
        assert!(settings_icon
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] != 0));
        let shell_result = results
            .iter()
            .find(|item| {
                item.identity.starts_with("control-panel:")
                    && item.category == ResultCategory::ControlPanel
            })
            .unwrap();
        assert!(crate::platform::application_icon(&shell_result.launch_target).is_some());
        eprintln!(
            "Windows build {:?}: {} indexed items, Settings and Control Panel native icons OK",
            crate::platform::windows_build_number(),
            results.len()
        );
    }
}
