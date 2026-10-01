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

const I18N_CONTEXT: &str = "Windows Settings";
const POWERTOYS_CONTEXT: &str = I18N_CONTEXT;
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
    show_as_first_result: bool,
}

struct PowerToysCatalog {
    settings: Vec<PowerToysSetting>,
    resources: HashMap<String, String>,
    zh_cn_resources: HashMap<String, String>,
    microsoft_learn_zh_cn_resources: HashMap<String, String>,
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
        let zh_cn_resources: HashMap<String, String> =
            serde_json::from_str(include_str!("windows_settings_data/Resources.zh-CN.json"))
                .expect("Wowie's Chinese Windows Settings resources must be valid JSON");
        let microsoft_learn_zh_cn_resources: HashMap<String, String> = serde_json::from_str(
            include_str!("windows_settings_data/Resources.zh-CN.microsoft-learn.json"),
        )
        .expect("Microsoft Learn Chinese Settings labels must be valid JSON");
        PowerToysCatalog {
            settings: settings.settings,
            resources,
            zh_cn_resources,
            microsoft_learn_zh_cn_resources,
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
    let english = resource_text(key);
    if i18n::effective_language(language).eq_ignore_ascii_case("zh-CN") {
        if let Some(label) = powertoys_catalog().microsoft_learn_zh_cn_resources.get(key) {
            return label.clone();
        }
    }
    let translated = i18n::tr_owned_in_context(language, POWERTOYS_CONTEXT, &english);
    if translated != english {
        return translated;
    }
    if i18n::effective_language(language).eq_ignore_ascii_case("zh-CN") {
        return powertoys_catalog()
            .zh_cn_resources
            .get(key)
            .cloned()
            .unwrap_or(english);
    }
    english
}

fn resource_aliases(key: &str) -> Vec<String> {
    let english = resource_text(key);
    let mut aliases = i18n::tr_aliases_all_languages_owned(POWERTOYS_CONTEXT, &english);
    for alias in localized_resource("zh-CN", key)
        .split('|')
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
    {
        append_unique_alias(&mut aliases, alias.to_owned());
    }
    aliases
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
        "AppMMC" => "mmc.exe",
        _ => "shell:ControlPanelFolder",
    };
    let path = translated_path(setting, "en");
    let identity = format!("{POWERTOYS_ID_PREFIX}{}", setting.name);
    let result = SearchResult {
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
        ranking_bias: if setting.show_as_first_result { 60 } else { 0 },
        allow_favorite: true,
        visibility: crate::result::ResultVisibility::Always,
    };

    result
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
    match setting.name.as_str() {
        "BatterySaver" | "BatterySaverSettings" | "BatteryUse" => Some("battery"),
        "MouseAndTouchpad" | "Touchpad" => Some("touchpad"),
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
        return true;
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
        crate::platform::windows_setting_capability_available(requirement) != Some(false)
    })
}

#[cfg(windows)]
pub(crate) fn powertoys_results() -> Vec<SearchResult> {
    let current_build = crate::platform::windows_build_number();
    let mut results = powertoys_catalog()
        .settings
        .iter()
        .filter(|setting| {
            available_on_build(setting, current_build) && available_on_device(setting)
        })
        .map(powertoys_result)
        .collect::<Vec<_>>();
    results.sort_by(|left, right| left.title.cmp(&right.title));
    results
}

pub(crate) fn powertoys_name_aliases(item: &SearchResult) -> Vec<String> {
    let Some(name) = item.id.strip_prefix(POWERTOYS_ID_PREFIX) else {
        return Vec::new();
    };
    let Some(setting) = powertoys_catalog()
        .settings
        .iter()
        .find(|setting| setting.name == name)
    else {
        return Vec::new();
    };
    let mut aliases = resource_aliases(&setting.name);
    if setting.kind == "AppControlPanel" {
        append_unique_alias(&mut aliases, format!("Microsoft.{}", setting.name));
    }
    aliases
}

pub(crate) fn is_powertoys_result(item: &SearchResult) -> bool {
    item.id.starts_with(POWERTOYS_ID_PREFIX)
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

pub(crate) fn compatible_settings_uri_for_canonical_name(name: &str) -> Option<&'static str> {
    COMPATIBLE_DESTINATIONS
        .iter()
        .find(|(canonical, _)| canonical.eq_ignore_ascii_case(name))
        .map(|(_, uri)| *uri)
}

pub(crate) fn localized_result(mut item: SearchResult, language: &str) -> SearchResult {
    if let Some(name) = item.id.strip_prefix(POWERTOYS_ID_PREFIX) {
        if let Some(setting) = powertoys_catalog()
            .settings
            .iter()
            .find(|setting| setting.name == name)
        {
            item.title = localized_resource(language, &setting.name);
            item.location = translated_path(setting, language);
        }
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

    let target = PathBuf::from(uri);
    item.category = ResultCategory::Settings;
    item.launch_target = target.clone();
    item.launch_arguments.clear();
    item.launch_working_directory = None;
    item.icon = Some(IconSpec::System(PathBuf::from(SETTINGS_ICON_TARGET)));
    for entry in item
        .launch_entries
        .iter_mut()
        .filter(|entry| entry.source.as_str() == source_ids::CONTROL_PANEL)
    {
        entry.path = target.clone();
        entry.target = target.clone();
        entry.arguments.clear();
        entry.working_directory = None;
        entry.icon = Some(IconSpec::System(PathBuf::from(SETTINGS_ICON_TARGET)));
    }
}
