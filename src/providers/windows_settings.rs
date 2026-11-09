//! PowerToys-derived Windows Settings and legacy management catalog.
//!
//! Windows does not provide a supported API for enumerating Settings pages.
//! PowerToys' data catalog is the single source for indexed pages and legacy
//! entries. Shell supplies installed applets/tasks; the catalog supplies pages,
//! build bounds and multilingual aliases. Policy and device checks are vetoes,
//! not an undocumented XML/DLL whitelist.

use crate::{
    platform::source_ids,
    result::{IconSpec, LaunchEntry, ResultCategory, SearchMetadata, SearchResult},
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
    #[cfg(test)]
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

fn powertoys_aliases(setting: &PowerToysSetting) -> Vec<String> {
    resource_aliases(&setting.name)
}

fn powertoys_metadata(setting: &PowerToysSetting) -> SearchMetadata {
    let mut metadata = SearchMetadata::default();
    for key in &setting.alt_names {
        for value in resource_aliases(key) {
            append_unique_alias(&mut metadata.keywords, value);
        }
    }
    for key in std::iter::once(&setting.kind).chain(setting.areas.iter()) {
        for value in resource_aliases(key) {
            append_unique_alias(&mut metadata.context, value);
        }
    }
    for language in ["en", "zh-CN"] {
        append_unique_alias(&mut metadata.context, translated_path(setting, language));
    }
    if setting.kind == "AppControlPanel" {
        metadata
            .technical
            .push(format!("Microsoft.{}", setting.name));
    }
    metadata
}

fn parse_powertoys_command(command: &str) -> (PathBuf, String) {
    let command = command.trim();
    // Split before expanding environment variables: an expanded Windows path
    // can contain spaces even when the catalog's executable token does not.
    if let Some(quoted) = command.strip_prefix('"') {
        if let Some((target, arguments)) = quoted.split_once('"') {
            return (
                expand_environment(target).into(),
                arguments.trim().to_owned(),
            );
        }
    }
    if let Some(split_at) = command.find(char::is_whitespace) {
        let (target, arguments) = command.split_at(split_at);
        (
            expand_environment(target).into(),
            arguments.trim().to_owned(),
        )
    } else {
        (expand_environment(command).into(), String::new())
    }
}

fn expand_environment(value: &str) -> String {
    let mut expanded = String::new();
    let mut remaining = value;
    while let Some(start) = remaining.find('%') {
        expanded.push_str(&remaining[..start]);
        let tail = &remaining[start + 1..];
        let Some(end) = tail.find('%') else {
            expanded.push_str(&remaining[start..]);
            return expanded;
        };
        let name = &tail[..end];
        let replacement = std::env::vars_os()
            .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.to_string_lossy().into_owned())
            .or_else(|| {
                (name.eq_ignore_ascii_case("windir") || name.eq_ignore_ascii_case("systemroot"))
                    .then(|| std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()))
            });
        expanded.push_str(
            replacement
                .as_deref()
                .unwrap_or(&remaining[start..start + end + 2]),
        );
        remaining = &tail[end + 1..];
    }
    expanded.push_str(remaining);
    expanded
}

/// MMC console files specify their own icon; the .msc association only gives
/// the generic MMC icon. Read the declarative resource without loading code.
fn mmc_icon(xml: &str) -> Option<IconSpec> {
    let document = roxmltree::Document::parse(xml.trim_start_matches('\u{feff}')).ok()?;
    let root = document.root_element();
    if !root.has_tag_name("MMC_ConsoleFile") {
        return None;
    }
    let icon = root
        .children()
        .find(|n| n.has_tag_name("VisualAttributes"))?
        .children()
        .find(|n| n.has_tag_name("Icon"))?;
    let path = PathBuf::from(expand_environment(icon.attribute("File")?));
    if !path.is_absolute() {
        return None;
    }
    Some(IconSpec::SystemIndexed {
        path,
        index: icon.attribute("Index")?.parse().ok()?,
    })
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
        metadata: powertoys_metadata(setting),
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
            icon: Some(IconSpec::System(PathBuf::from(icon_target).into())),
        }],
        actions: Vec::new(),
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: Some(IconSpec::System(PathBuf::from(icon_target).into())),
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

fn available_on_build(setting: &PowerToysSetting, current_build: Option<u32>) -> bool {
    // Cortana's standalone Windows app was retired in 2023 and these old
    // Settings URIs are absent from Microsoft's current URI reference.
    if setting.command.starts_with("ms-settings:cortana") {
        return false;
    }

    let Some(current_build) = current_build else {
        // Failure to read the build is not evidence that the host is too old.
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
        capability_allows(crate::platform::windows_setting_capability_available(
            requirement,
        ))
    })
}

fn capability_allows(detected: Option<bool>) -> bool {
    detected != Some(false)
}

#[cfg(windows)]
pub(crate) fn powertoys_results(
    availability: &crate::platform::SettingsAvailability,
) -> Vec<SearchResult> {
    let current_build = crate::platform::windows_build_number();
    let mut capabilities = HashMap::new();
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
                    item.icon = std::fs::metadata(&item.launch_target)
                        .ok()
                        .filter(|metadata| metadata.len() <= 4 * 1024 * 1024)
                        .and_then(|_| std::fs::read_to_string(&item.launch_target).ok())
                        .and_then(|xml| mmc_icon(&xml))
                        .or_else(|| Some(IconSpec::System(item.launch_target.clone().into())));
                }
            }
            for entry in &mut item.launch_entries {
                entry.icon = item.icon.clone();
            }
            item
        })
        .collect::<Vec<_>>();
    results.sort_by(|left, right| left.title.cmp(&right.title));
    results
}

/// Merge by invariant launch destination, never by localized display title.
/// Shell records come first to retain existing favorite IDs and native icons.
/// Keep every catalog synonym when several entries lead to the same page.
pub(crate) fn merge_results(items: impl IntoIterator<Item = SearchResult>) -> Vec<SearchResult> {
    let mut positions = HashMap::new();
    let mut results: Vec<SearchResult> = Vec::new();
    for item in items {
        let key = (
            item.launch_target.to_string_lossy().to_ascii_lowercase(),
            item.launch_arguments.trim().to_owned(),
        );
        if let Some(&index) = positions.get(&key) {
            let existing: &mut SearchResult = &mut results[index];
            for alias in std::iter::once(item.title).chain(item.aliases) {
                append_unique_alias(&mut existing.aliases, alias);
            }
            for (target, values) in [
                (&mut existing.metadata.keywords, item.metadata.keywords),
                (&mut existing.metadata.context, item.metadata.context),
                (&mut existing.metadata.technical, item.metadata.technical),
            ] {
                for value in values {
                    append_unique_alias(target, value);
                }
            }
            existing.ranking_bias = existing.ranking_bias.max(item.ranking_bias);
            for entry in item.launch_entries {
                if !existing.launch_entries.contains(&entry) {
                    existing.launch_entries.push(entry);
                }
            }
        } else {
            positions.insert(key, results.len());
            results.push(item);
        }
    }
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
                        .chain(item.metadata.technical.iter())
                        .any(|alias| alias.eq_ignore_ascii_case(canonical))
                })
    })
}

pub(crate) fn enrich_shell_result(item: &mut SearchResult) {
    if let Some(setting) = shell_catalog_setting(item) {
        for alias in powertoys_aliases(setting) {
            append_unique_alias(&mut item.aliases, alias);
        }
        let metadata = powertoys_metadata(setting);
        for (target, values) in [
            (&mut item.metadata.keywords, metadata.keywords),
            (&mut item.metadata.context, metadata.context),
            (&mut item.metadata.technical, metadata.technical),
        ] {
            for value in values {
                append_unique_alias(target, value);
            }
        }
    }
}

pub(crate) fn matches_settings_path(item: &SearchResult, query: &str) -> bool {
    if !query.contains('>') {
        return true;
    }
    let query_parts = query
        .trim()
        .trim_end_matches('>')
        .split('>')
        .map(str::trim)
        .collect::<Vec<_>>();
    if query_parts.iter().any(|part| part.is_empty()) {
        return false;
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
            .metadata
            .context
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
pub(crate) fn apply_compatible_destination(
    item: &mut SearchResult,
    #[cfg(windows)] availability: &crate::platform::SettingsAvailability,
) {
    if item.category != ResultCategory::ControlPanel {
        return;
    }
    let Some(uri) = item
        .metadata
        .technical
        .iter()
        .find_map(|alias| compatible_settings_uri_for_canonical_name(alias))
    else {
        return;
    };

    let icon = Some(IconSpec::System(PathBuf::from(SETTINGS_ICON_TARGET).into()));
    #[cfg(windows)]
    let icon = {
        if !availability.contains(uri) {
            return;
        }
        availability
            .settings_icon_path
            .clone()
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

    #[test]
    fn build_boundaries_filter_windows_10_and_11_and_unknown_builds() {
        let setting = powertoys_catalog()
            .settings
            .iter()
            .find(|entry| entry.introduced_in_build == Some(22000))
            .unwrap();
        assert!(!available_on_build(setting, Some(19045)));
        assert!(available_on_build(setting, Some(22000)));
        assert!(available_on_build(setting, None));
        let fonts = powertoys_catalog()
            .settings
            .iter()
            .find(|entry| entry.name == "Fonts" && entry.kind == "AppSettingsApp")
            .unwrap();
        assert!(available_on_build(fonts, Some(26100)));
        for entry in &powertoys_catalog().settings {
            if entry.command.starts_with("ms-settings:cortana") {
                assert!(!available_on_build(entry, Some(26100)));
                assert!(!available_on_build(entry, None));
            }
            if let Some(build) = entry.deprecated_in_build {
                if entry.name != "Fonts" {
                    assert!(!available_on_build(entry, Some(build)), "{}", entry.name);
                }
            }
        }
    }

    #[test]
    fn unknown_capability_is_not_a_negative_probe() {
        assert!(capability_allows(None));
        assert!(capability_allows(Some(true)));
        assert!(!capability_allows(Some(false)));
    }

    #[test]
    fn commands_preserve_quoted_paths_arguments_and_environment_names() {
        assert_eq!(
            parse_powertoys_command(r#" "C:\Windows Tools\control.exe" /name Microsoft.Mouse "#),
            (
                PathBuf::from(r"C:\Windows Tools\control.exe"),
                "/name Microsoft.Mouse".into()
            )
        );
        assert_eq!(
            parse_powertoys_command("ms-settings:network-wifi"),
            (PathBuf::from("ms-settings:network-wifi"), String::new())
        );
        assert_eq!(
            expand_environment("%WoWiE_MISSING_VARIABLE%/file"),
            "%WoWiE_MISSING_VARIABLE%/file"
        );
        assert_eq!(
            expand_environment("%WINDIR%"),
            expand_environment("%windir%")
        );
    }

    #[test]
    fn merging_destinations_preserves_synonyms_and_existing_identity() {
        let display = powertoys_catalog()
            .settings
            .iter()
            .find(|s| s.command == "ms-settings:display")
            .unwrap();
        let first = powertoys_result(display);
        let mut duplicate = first.clone();
        duplicate.id = "another-resource-id".into();
        duplicate.title = "屏幕设置".into();
        duplicate.aliases = vec!["monitor configuration".into()];
        duplicate.metadata.keywords.push("monitor setup".into());
        duplicate.metadata.context.push("system display".into());
        duplicate
            .metadata
            .technical
            .push("Microsoft.Display".into());
        let mut different = first.clone();
        different.launch_target = "ms-settings:display-advanced".into();
        let results = merge_results([first.clone(), duplicate, different]);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, first.id);
        assert!(results[0].aliases.iter().any(|a| a == "屏幕设置"));
        assert!(results[0]
            .aliases
            .iter()
            .any(|a| a == "monitor configuration"));
        assert!(results[0]
            .metadata
            .keywords
            .iter()
            .any(|value| value == "monitor setup"));
        assert!(results[0]
            .metadata
            .context
            .iter()
            .any(|value| value == "system display"));
        assert!(results[0]
            .metadata
            .technical
            .iter()
            .any(|value| value == "Microsoft.Display"));
    }

    #[test]
    fn settings_paths_match_both_languages_and_reject_empty_middle_segments() {
        let setting = powertoys_catalog()
            .settings
            .iter()
            .find(|s| s.command == "ms-settings:display")
            .unwrap();
        let item = powertoys_result(setting);
        assert!(matches_settings_path(&item, "settings > system"));
        assert!(matches_settings_path(&item, "settings > system >"));
        assert!(matches_settings_path(&item, "设置 > 系统"));
        assert!(!matches_settings_path(&item, "settings >> unrelated"));
        assert!(!matches_settings_path(&item, "settings > network"));
    }

    #[test]
    fn mmc_resource_icon_uses_console_metadata_and_rejects_unrelated_xml() {
        let path = if cfg!(windows) {
            r"C:\Windows\System32\devmgr.dll"
        } else {
            "/windows/devmgr.dll"
        };
        let xml = format!(
            r#"<MMC_ConsoleFile><VisualAttributes><Icon File="{path}" Index="4"/></VisualAttributes></MMC_ConsoleFile>"#
        );
        assert_eq!(
            mmc_icon(&xml),
            Some(IconSpec::SystemIndexed {
                path: path.into(),
                index: 4
            })
        );
        assert!(mmc_icon("<Other><Icon File='x' Index='0'/></Other>").is_none());
        assert!(mmc_icon("<MMC_ConsoleFile><VisualAttributes><Icon File='relative.dll' Index='0'/></VisualAttributes></MMC_ConsoleFile>").is_none());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads installed Windows pages and extracts native Shell icons"]
    fn installed_pages_and_icons() {
        let availability = crate::platform::SettingsAvailability::discover();
        let results =
            crate::providers::control_panel::discover(&crate::config::ProviderConfig::default());
        let mut ids = std::collections::HashSet::new();
        let mut destinations = std::collections::HashSet::new();
        for result in &results {
            assert!(ids.insert(&result.id), "duplicate identity: {}", result.id);
            if result.category == ResultCategory::Settings {
                assert!(availability.contains(&result.launch_target.to_string_lossy()));
                assert!(destinations
                    .insert(result.launch_target.to_string_lossy().to_ascii_lowercase()));
            }
            for entry in &result.launch_entries {
                assert_eq!(entry.icon, result.icon);
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
        assert!(matches!(&shell_result.icon, Some(IconSpec::System(path))
            if crate::platform::shell_id_list::decode(path).is_some()));
        for result in &results {
            let icon = icon_service
                .resolve_at(result, 32)
                .unwrap_or_else(|| panic!("missing icon: {}", result.title));
            assert!(
                icon.rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0),
                "transparent icon: {}",
                result.title
            );
        }
        let settings_count = results
            .iter()
            .filter(|r| r.category == ResultCategory::Settings)
            .count();
        eprintln!(
            "Windows build {:?}: {} indexed items ({} settings, {} tasks), all icons visible",
            crate::platform::windows_build_number(),
            results.len(),
            settings_count,
            tasks.len()
        );
    }
}
