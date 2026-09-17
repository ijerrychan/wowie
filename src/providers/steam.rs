//! Steam library discovery based on Steam's own library and app manifests.

use crate::result::{IconSpec, LaunchEntry, ResultCategory, SearchResult};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

pub fn discover() -> Vec<SearchResult> {
    let mut roots = crate::platform::steam_install_roots();
    roots.extend(default_roots());
    roots.sort();
    roots.dedup();
    let steam_roots = roots.clone();
    let app_types = load_app_types(&steam_roots);

    let mut libraries = Vec::new();
    for root in roots {
        let config = root.join("config/libraryfolders.vdf");
        if config.is_file() {
            libraries.extend(value_for_key(
                &fs::read_to_string(config).unwrap_or_default(),
                "path",
            ));
        }
        libraries.push(root);
    }
    libraries.sort();
    libraries.dedup();

    let mut games = Vec::new();
    for library in libraries {
        let steamapps = library.join("steamapps");
        let Ok(entries) = fs::read_dir(&steamapps) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let fields = scalar_fields(&text);
            let Some(appid) = fields.get("appid").filter(|value| !value.is_empty()) else {
                continue;
            };
            let Some(title) = fields.get("name").filter(|value| !value.is_empty()) else {
                continue;
            };
            let Some(install_dir) = fields.get("installdir").filter(|value| !value.is_empty())
            else {
                continue;
            };
            let Some(category) = classify_app(appid, fields.get("name"), &app_types) else {
                continue;
            };
            let location = steamapps.join("common").join(install_dir);
            let icon = game_icon(&steam_roots, appid, &location);
            games.push(game_result(appid, title, location, icon, category));
        }
    }
    games.sort_by_cached_key(|item| item.title.to_lowercase());
    games.dedup_by(|left, right| left.identity == right.identity);
    games
}

fn game_result(
    appid: &str,
    title: &str,
    location: PathBuf,
    icon: Option<IconSpec>,
    category: ResultCategory,
) -> SearchResult {
    let identity = format!("game:steam:{appid}");
    let target = PathBuf::from(format!("steam://rungameid/{appid}"));
    SearchResult {
        id: identity.clone(),
        identity,
        title: title.to_owned(),
        category,
        aliases: vec!["steam".into(), appid.into()],
        location: target.display().to_string(),
        launch_target: target.clone(),
        launch_arguments: String::new(),
        launch_working_directory: Some(location.clone()),
        source: "Steam".into(),
        sources: vec!["Steam".into()],
        launch_entries: vec![LaunchEntry {
            source: "Steam".into(),
            path: target.clone(),
            target,
            arguments: String::new(),
            working_directory: Some(location),
        }],
        actions: Vec::new(),
        icon,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SteamAppKind {
    Game,
    Application,
}

fn load_app_types(steam_roots: &[PathBuf]) -> HashMap<String, SteamAppKind> {
    let mut types = HashMap::new();
    for root in steam_roots {
        let path = root.join("appcache/appinfo.vdf");
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        let Ok(document) = steam_vdf_parser::parse_appinfo(&bytes) else {
            continue;
        };
        let Some(apps) = document.as_obj() else {
            continue;
        };
        for (appid, value) in apps.iter() {
            let Some(kind) = app_kind_from_metadata(value) else {
                continue;
            };
            types.insert(appid.to_string(), kind);
        }
        if !types.is_empty() {
            break;
        }
    }
    types
}

fn app_kind_from_metadata(value: &steam_vdf_parser::Value<'_>) -> Option<SteamAppKind> {
    let common = value
        .as_obj()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("common"))
        .and_then(|(_, value)| value.as_obj())?;
    common
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("type"))
        .and_then(|(_, value)| value.as_str())
        .and_then(parse_app_kind)
}

fn parse_app_kind(value: &str) -> Option<SteamAppKind> {
    match value.to_ascii_lowercase().as_str() {
        "game" | "demo" => Some(SteamAppKind::Game),
        "application" | "tool" => Some(SteamAppKind::Application),
        _ => None,
    }
}

fn classify_app(
    appid: &str,
    name: Option<&String>,
    app_types: &HashMap<String, SteamAppKind>,
) -> Option<ResultCategory> {
    if appid == "228980"
        || name.is_some_and(|value| value.to_ascii_lowercase().contains("redistributable"))
    {
        return None;
    }
    match app_types.get(appid) {
        Some(SteamAppKind::Game) => Some(ResultCategory::Game),
        Some(SteamAppKind::Application) => Some(ResultCategory::Application),
        None if app_types.is_empty() => {
            // If appinfo is unavailable, preserve the old behavior for
            // ordinary manifests while excluding the known redist.
            Some(ResultCategory::Game)
        }
        None => None,
    }
}

fn game_icon(steam_roots: &[PathBuf], appid: &str, location: &Path) -> Option<IconSpec> {
    find_cached_icon(steam_roots, appid)
        .map(IconSpec::Resource)
        .or_else(|| find_game_executable(location).map(IconSpec::System))
}

/// Steam's local icon cache has appeared in both `<appid>_icon.jpg` and
/// `<appid>/<hash>.jpg` layouts. Prefer it over executable icons because it is
/// the icon Steam itself associates with the game.
fn find_cached_icon(steam_roots: &[PathBuf], appid: &str) -> Option<PathBuf> {
    for root in steam_roots {
        let cache = root.join("appcache/librarycache");
        let direct = [
            cache.join(format!("{appid}_icon.jpg")),
            cache.join(format!("{appid}_icon.png")),
        ];
        if let Some(path) = direct.into_iter().find(|path| path.is_file()) {
            return Some(path);
        }

        let app_dir = cache.join(appid);
        let Ok(entries) = fs::read_dir(&app_dir) else {
            continue;
        };
        let mut hashed = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().ok().is_some_and(|kind| kind.is_file()) {
                if is_hash_image(&path) {
                    hashed.push(path);
                }
            } else if entry.file_type().ok().is_some_and(|kind| kind.is_dir()) {
                collect_hash_images(&path, 1, &mut hashed);
            }
        }
        hashed.sort();
        if let Some(path) = hashed.into_iter().next() {
            return Some(path);
        }
    }
    None
}

fn collect_hash_images(root: &Path, depth: usize, output: &mut Vec<PathBuf>) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().ok().is_some_and(|kind| kind.is_file()) {
            if is_hash_image(&path) {
                output.push(path);
            }
        } else if entry.file_type().ok().is_some_and(|kind| kind.is_dir()) {
            collect_hash_images(&path, depth + 1, output);
        }
    }
}

fn is_hash_image(path: &Path) -> bool {
    is_image_file(path)
        && path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(is_hash)
}

fn is_image_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png")
    )
}

fn is_hash(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Use the most substantial executable near the game's root; Windows can
/// extract the embedded application icon from it without network access.
fn find_game_executable(root: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    collect_executables(root, 0, &mut candidates);
    candidates.sort_by(|left, right| right.1.cmp(&left.1));
    candidates.into_iter().next().map(|(path, _)| path)
}

fn collect_executables(root: &Path, depth: usize, output: &mut Vec<(PathBuf, u64)>) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_executables(&path, depth + 1, output);
            continue;
        }
        if !file_type.is_file() || path.extension().and_then(|value| value.to_str()) != Some("exe")
        {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if ["unins", "uninstall", "setup", "install", "crash", "redist"]
            .iter()
            .any(|part| name.contains(part))
        {
            continue;
        }
        let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        output.push((path, size));
    }
}

fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    for variable in ["PROGRAMFILES(X86)", "PROGRAMFILES"] {
        if let Ok(value) = std::env::var(variable) {
            roots.push(PathBuf::from(value).join("Steam"));
        }
    }
    #[cfg(target_os = "linux")]
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".steam/steam"));
        roots.push(home.join(".local/share/Steam"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Library/Application Support/Steam"));
    }
    roots
}

fn scalar_fields(text: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    let tokens = tokenize(text);
    let mut index = 0;
    collect_scalars(&tokens, &mut index, &mut fields);
    fields
}

fn value_for_key(text: &str, wanted: &str) -> Vec<PathBuf> {
    let tokens = tokenize(text);
    let mut index = 0;
    let mut values = Vec::new();
    collect_values(&tokens, &mut index, wanted, &mut values);
    values.into_iter().map(PathBuf::from).collect()
}

fn collect_scalars(tokens: &[String], index: &mut usize, fields: &mut HashMap<String, String>) {
    while *index + 1 < tokens.len() {
        let key = &tokens[*index];
        *index += 1;
        if tokens[*index] == "{" {
            *index += 1;
            collect_scalars(tokens, index, fields);
            continue;
        }
        fields.insert(key.clone(), tokens[*index].clone());
        *index += 1;
    }
}

fn collect_values(tokens: &[String], index: &mut usize, wanted: &str, values: &mut Vec<String>) {
    while *index + 1 < tokens.len() {
        let key = &tokens[*index];
        *index += 1;
        if tokens[*index] == "{" {
            *index += 1;
            collect_values(tokens, index, wanted, values);
            continue;
        }
        if key == wanted {
            values.push(tokens[*index].clone());
        }
        *index += 1;
    }
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            while chars.next().is_some_and(|c| c != '\n') {}
            continue;
        }
        if ch == '{' || ch == '}' {
            tokens.push(ch.to_string());
            continue;
        }
        if ch == '"' {
            let mut value = String::new();
            while let Some(next) = chars.next() {
                if next == '"' {
                    break;
                }
                if next == '\\' {
                    if let Some(escaped) = chars.next() {
                        value.push(escaped);
                    }
                } else {
                    value.push(next);
                }
            }
            tokens.push(value);
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_manifest_fields() {
        let fields =
            scalar_fields(r#""AppState" "4" "appid" "570" "name" "Dota 2" "installdir" "Dota 2""#);
        assert_eq!(fields.get("appid").map(String::as_str), Some("570"));
        assert_eq!(fields.get("name").map(String::as_str), Some("Dota 2"));
    }
    #[test]
    fn parses_library_paths_inside_nested_objects() {
        let paths = value_for_key(
            r#""libraryfolders" { "0" { "path" "C:\\SteamLibrary" } }"#,
            "path",
        );
        assert_eq!(paths, vec![PathBuf::from(r"C:\SteamLibrary")]);
    }

    #[test]
    fn recognizes_steam_icon_hash_names() {
        assert!(is_hash("0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_hash("header"));
        assert!(is_image_file(Path::new("icon.jpg")));
    }

    #[test]
    fn excludes_redistributables_and_maps_metadata_types() {
        let mut types = HashMap::new();
        types.insert("10".into(), SteamAppKind::Game);
        types.insert("20".into(), SteamAppKind::Application);
        assert_eq!(
            classify_app(
                "228980",
                Some(&"Steamworks Common Redistributables".into()),
                &types
            ),
            None
        );
        assert_eq!(classify_app("10", None, &types), Some(ResultCategory::Game));
        assert_eq!(
            classify_app("20", None, &types),
            Some(ResultCategory::Application)
        );
        assert_eq!(classify_app("30", None, &types), None);
    }

    #[test]
    fn steam_games_keep_the_steam_source_alias() {
        let result = game_result(
            "570",
            "Dota 2",
            PathBuf::from(r"C:\Steam\steamapps\common\Dota 2"),
            None,
            ResultCategory::Game,
        );
        assert!(result.aliases.iter().any(|alias| alias == "steam"));
        assert!(result.aliases.iter().any(|alias| alias == "570"));
    }
}
