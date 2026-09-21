//! Embedded Lua plugin support.
//!
//! Plugins execute inside Wowie.  The host owns the Lua VM and exposes a small
//! data-oriented API, so plugin execution does not depend on a user-installed
//! runtime and never creates a plugin child process.

use crate::{
    config::PluginConfig,
    page::{SearchEngineMode, SearchEngineSession},
    result::{Action, IconRef, IconSpec, LaunchEntry, ResultCategory, SearchResult},
};
use anyhow::{Context, Result};
use mlua::{Function, Lua, LuaSerdeExt, Value};
use serde::{de, Deserialize, Deserializer};
use std::{
    collections::BTreeMap,
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        Arc, Mutex, TryLockError,
    },
};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    #[allow(dead_code)]
    pub name: String,
    #[serde(default = "default_version")]
    #[allow(dead_code)]
    pub version: String,
    #[serde(default = "default_api_version")]
    pub api_version: u32,
    pub entry_point: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
    #[serde(default)]
    #[allow(dead_code)]
    pub permissions: Vec<String>,
    #[serde(default = "default_locale")]
    pub default_locale: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PluginCommand {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub action_words: Vec<String>,
    /// Optional launcher presentation hint for this command.
    #[serde(default)]
    pub presentation: Option<String>,
    /// How an action word owns the launcher interaction. Plugin commands
    /// default to provider-backed search for backwards compatibility.
    #[serde(default)]
    pub mode: Option<String>,
    /// Require whitespace after the action word before entering its page.
    #[serde(default)]
    pub require_separator: bool,
    #[serde(default)]
    pub icon: Option<IconRef>,
}

fn default_version() -> String {
    "0.1.0".into()
}
fn default_api_version() -> u32 {
    1
}
fn default_locale() -> String {
    "en".into()
}

#[derive(Debug, Clone)]
struct InstalledPlugin {
    root: PathBuf,
    manifest: PluginManifest,
    embedded: Option<Arc<Mutex<EmbeddedPlugin>>>,
    enabled: Arc<AtomicBool>,
    weight: Arc<AtomicI32>,
    preferences: Arc<Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    status: Arc<Mutex<PluginRuntimeStatus>>,
}

#[derive(Debug, Clone)]
struct PluginRuntimeStatus {
    state: String,
    last_method: Option<String>,
    last_error: Option<String>,
    failure_count: u32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStatus {
    pub id: String,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub state: String,
    pub last_method: Option<String>,
    pub last_error: Option<String>,
    pub failure_count: u32,
}

struct EmbeddedPlugin {
    lua: Lua,
}

impl std::fmt::Debug for EmbeddedPlugin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EmbeddedPlugin")
    }
}

impl EmbeddedPlugin {
    fn from_file(
        root: &Path,
        manifest: &PluginManifest,
        locale: &str,
        preferences: &std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<Self> {
        let entry = root.join(&manifest.entry_point);
        let source = fs::read_to_string(&entry)
            .with_context(|| format!("read plugin entry point {}", entry.display()))?;
        Self::from_source(
            &source,
            &manifest.entry_point,
            locale,
            manifest.default_locale.as_str(),
            preferences,
            &manifest.permissions,
        )
    }

    fn from_source(
        source: &str,
        chunk_name: &str,
        locale: &str,
        fallback_locale: &str,
        preferences: &BTreeMap<String, serde_json::Value>,
        permissions: &[String],
    ) -> Result<Self> {
        let lua = Lua::new();
        let wowie = lua.create_table()?;
        let locale_value = locale.to_owned();
        let fallback_value = fallback_locale.to_owned();
        let preferences_value = preferences.clone();

        let on = lua.create_function(|lua, (method, callback): (String, Function)| {
            lua.globals().set(format!("__wowie_{method}"), callback)?;
            Ok(())
        })?;
        wowie.set("on", on)?;
        wowie.set(
            "locale",
            lua.create_function(move |_, key: Option<String>| {
                if let Some(key) = key {
                    Ok(if key == "current" {
                        locale_value.clone()
                    } else {
                        fallback_value.clone()
                    })
                } else {
                    Ok(locale_value.clone())
                }
            })?,
        )?;
        wowie.set(
            "result",
            lua.create_function(|_, value: mlua::Table| Ok(value))?,
        )?;
        wowie.set(
            "results",
            lua.create_function(|lua, items: mlua::Table| {
                let result = lua.create_table()?;
                result.set("items", items)?;
                Ok(result)
            })?,
        )?;
        wowie.set(
            "action",
            lua.create_function(|lua, (id, icon): (String, Option<mlua::Table>)| {
                let action = lua.create_table()?;
                action.set("id", id)?;
                if let Some(icon) = icon {
                    action.set("icon", icon)?;
                }
                Ok(action)
            })?,
        )?;
        let network_allowed = has_plugin_permission(permissions, "network");
        wowie.set(
            "http_get_json",
            lua.create_function(move |lua, url: String| {
                require_plugin_permission(network_allowed, "network")?;
                validate_plugin_http_url(&url).map_err(mlua::Error::external)?;
                let value: serde_json::Value =
                    crate::utils::http::get_json(&url).map_err(mlua::Error::external)?;
                lua.to_value(&value)
            })?,
        )?;
        let network_allowed = has_plugin_permission(permissions, "network");
        wowie.set(
            "http_get_text",
            lua.create_function(move |_, url: String| {
                require_plugin_permission(network_allowed, "network")?;
                validate_plugin_http_url(&url).map_err(mlua::Error::external)?;
                crate::utils::http::get_text(&url, 2 * 1024 * 1024).map_err(mlua::Error::external)
            })?,
        )?;
        wowie.set(
            "url_encode",
            lua.create_function(|_, value: String| {
                Ok(url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>())
            })?,
        )?;
        wowie.set(
            "url_encode_path",
            lua.create_function(|_, value: String| Ok(encode_url_path_segment(&value)))?,
        )?;
        let clipboard_allowed = has_plugin_permission(permissions, "clipboard");
        wowie.set(
            "clipboard_set",
            lua.create_function(move |_, text: String| {
                require_plugin_permission(clipboard_allowed, "clipboard")?;
                set_clipboard_text(&text).map_err(mlua::Error::external)
            })?,
        )?;
        let network_allowed = has_plugin_permission(permissions, "network");
        let download_allowed = has_plugin_permission(permissions, "download");
        wowie.set(
            "download",
            lua.create_function(move |_, (url, filename): (String, String)| {
                require_plugin_permission(network_allowed, "network")?;
                require_plugin_permission(download_allowed, "download")?;
                validate_plugin_http_url(&url).map_err(mlua::Error::external)?;
                download_plugin_file(&url, &filename).map_err(mlua::Error::external)
            })?,
        )?;
        lua.globals().set("wowie", wowie)?;
        lua.globals().set("__wowie_locale", locale)?;
        lua.globals()
            .set("__wowie_fallback_locale", fallback_locale)?;
        lua.globals()
            .set("__wowie_preferences", lua.to_value(preferences)?)?;
        lua.load(source).set_name(chunk_name).exec()?;

        let plugin = Self { lua };
        let initialize = serde_json::json!({
            "locale": locale,
            "fallback_locale": fallback_locale,
            "preferences": preferences_value,
        });
        if plugin.has_handler("initialize")? {
            plugin.call("initialize", &initialize, None, None)?;
        }
        Ok(plugin)
    }

    fn has_handler(&self, method: &str) -> Result<bool> {
        Ok(!matches!(
            self.lua
                .globals()
                .get::<Value>(format!("__wowie_{method}"))?,
            Value::Nil
        ))
    }

    fn call(
        &self,
        method: &str,
        request: &serde_json::Value,
        item_id: Option<&str>,
        action_id: Option<&str>,
    ) -> Result<String> {
        let callback: Function = self
            .lua
            .globals()
            .get(format!("__wowie_{method}"))
            .with_context(|| format!("plugin does not register {method} handler"))?;
        let context = self.lua.to_value(request)?;
        let value = match (item_id, action_id) {
            (Some(item_id), Some(action_id)) => {
                callback.call::<Value>((item_id, action_id, context))?
            }
            _ if method == "query" => callback.call::<Value>((
                request
                    .get("query")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default(),
                context,
            ))?,
            _ => callback.call::<Value>(context)?,
        };
        let value = if matches!(value, Value::Nil) {
            serde_json::Value::Object(Default::default())
        } else {
            self.lua.from_value(value)?
        };
        Ok(serde_json::to_string(&value)?)
    }
}

fn has_plugin_permission(permissions: &[String], requested: &str) -> bool {
    permissions
        .iter()
        .any(|permission| permission.eq_ignore_ascii_case(requested))
}

fn require_plugin_permission(
    allowed: bool,
    permission: &str,
) -> std::result::Result<(), mlua::Error> {
    allowed
        .then_some(())
        .ok_or_else(|| mlua::Error::runtime(format!("plugin permission required: {permission}")))
}

fn validate_plugin_http_url(value: &str) -> Result<()> {
    let value = value.trim();
    let parsed = url::Url::parse(value).context("invalid plugin URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || value.chars().any(char::is_whitespace)
    {
        anyhow::bail!("plugin URL must be an HTTP(S) URL without credentials or whitespace");
    }
    Ok(())
}

fn encode_url_path_segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn set_clipboard_text(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("open system clipboard")?;
    clipboard
        .set_text(text.to_owned())
        .context("write system clipboard")
}

fn download_plugin_file(url: &str, filename: &str) -> Result<String> {
    let bytes = crate::utils::http::get_bytes(url, 4 * 1024 * 1024)?;
    let directory = dirs::download_dir().context("download directory is unavailable")?;
    fs::create_dir_all(&directory).context("create download directory")?;

    let safe_name: String = filename
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
        .take(96)
        .collect();
    let safe_name = if safe_name.is_empty() {
        "wowie-download.svg".to_owned()
    } else {
        safe_name
    };
    let path = Path::new(&safe_name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("wowie-download");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("svg");

    for suffix in 0..1000_u32 {
        let name = if suffix == 0 {
            safe_name.clone()
        } else {
            format!("{stem}-{suffix}.{extension}")
        };
        let target = directory.join(name);
        let Ok(mut file) = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)
        else {
            continue;
        };
        file.write_all(&bytes).context("write downloaded file")?;
        return Ok(target.to_string_lossy().into_owned());
    }

    anyhow::bail!("could not choose a free download filename")
}

#[derive(Debug, Deserialize)]
struct QueryResponse {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_plugin_items")]
    items: Vec<PluginItem>,
    #[serde(default)]
    error: Option<PluginError>,
}

fn deserialize_plugin_items<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<PluginItem>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(de::Error::custom),
        // Lua has no distinct empty-array type. An empty table is encoded as
        // an object by mlua, so accept it as the protocol's empty item list.
        serde_json::Value::Object(items) if items.is_empty() => Ok(Vec::new()),
        other => Err(de::Error::custom(format!(
            "expected plugin items array, got {}",
            json_type_name(&other)
        ))),
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[derive(Debug, Deserialize)]
pub struct ExecuteResponse {
    #[serde(default)]
    #[allow(dead_code)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub error: Option<PluginError>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct PluginError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct PluginItem {
    id: String,
    title: String,
    #[serde(default)]
    subtitle: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default = "default_optional_string")]
    target: Option<String>,
    #[serde(default)]
    actions: Vec<PluginAction>,
    #[serde(default)]
    icon: Option<IconRef>,
    #[serde(default)]
    detail: Option<PluginDetailView>,
    #[serde(default)]
    command_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PluginDetailView {
    #[serde(default)]
    title: String,
    #[serde(default)]
    subtitle: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    blocks: Vec<PluginDetailBlock>,
    #[serde(default)]
    actions: Vec<PluginDetailAction>,
    #[serde(default)]
    width: Option<PluginDetailWidth>,
    #[serde(default)]
    scrollable: bool,
}

#[derive(Debug, Deserialize)]
struct PluginDetailWidth {
    preferred: i32,
    #[serde(default = "default_detail_min_width")]
    min: i32,
    #[serde(default = "default_detail_max_width")]
    max: i32,
}

fn default_detail_min_width() -> i32 {
    320
}

fn default_detail_max_width() -> i32 {
    640
}

#[derive(Debug, Deserialize)]
struct PluginDetailAction {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    icon: Option<IconRef>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PluginDetailBlock {
    Text {
        text: String,
        #[serde(default = "default_detail_color")]
        color: String,
    },
    Metric {
        label: String,
        value: String,
        #[serde(default = "default_detail_color")]
        color: String,
    },
    Icon {
        glyph: String,
        #[serde(default = "default_detail_color")]
        color: String,
        #[serde(default)]
        icon: Option<IconRef>,
    },
    Weather {
        value: String,
    },
    Markdown {
        markdown: String,
        #[serde(default = "default_detail_color")]
        color: String,
    },
    Image {
        source: String,
        #[serde(default)]
        alt: String,
    },
    Code {
        language: String,
        code: String,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Link {
        label: String,
        url: String,
    },
    Loading {
        #[serde(default = "default_loading_message")]
        message: String,
    },
    Error {
        message: String,
        #[serde(default)]
        retry_action: Option<String>,
    },
    Separator,
}

fn default_loading_message() -> String {
    "Loading preview".into()
}

fn default_detail_color() -> String {
    "secondary".into()
}

fn default_optional_string() -> Option<String> {
    None
}

#[derive(Debug, Deserialize)]
struct PluginAction {
    id: String,
    #[serde(default)]
    icon: Option<IconRef>,
}

#[derive(Debug, Clone)]
pub struct PluginManager {
    plugins: Vec<InstalledPlugin>,
    locale: Arc<Mutex<String>>,
}

impl PluginManager {
    pub fn discover_with_config(
        locale: &str,
        plugin_configs: &std::collections::BTreeMap<String, PluginConfig>,
    ) -> Self {
        let locale = crate::utils::i18n::effective_language(locale);
        let mut roots = vec![PathBuf::from("plugins")];
        if let Some(data) = dirs::data_local_dir() {
            roots.push(data.join("Wowie").join("plugins"));
        }
        let mut plugins = Vec::new();
        for root in roots {
            let Ok(entries) = fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                let manifest_path = dir.join("manifest.json");
                let Ok(text) = fs::read_to_string(&manifest_path) else {
                    continue;
                };
                match serde_json::from_str::<PluginManifest>(&text) {
                    Ok(manifest) => {
                        let settings = plugin_configs
                            .get(&manifest.id)
                            .cloned()
                            .unwrap_or_default();
                        let status = Arc::new(Mutex::new(PluginRuntimeStatus {
                            state: if settings.enabled {
                                "ready".into()
                            } else {
                                "disabled".into()
                            },
                            last_method: None,
                            last_error: None,
                            failure_count: 0,
                        }));
                        let embedded = match EmbeddedPlugin::from_file(
                            &dir,
                            &manifest,
                            &locale,
                            &settings.preferences,
                        ) {
                            Ok(plugin) => Some(Arc::new(Mutex::new(plugin))),
                            Err(error) => {
                                if let Ok(mut status) = status.lock() {
                                    status.state = "error".into();
                                    status.last_method = Some("initialize".into());
                                    status.last_error = Some(error.to_string());
                                    status.failure_count = 1;
                                }
                                eprintln!(
                                    "[plugin:{}] embedded start failed: {error:#}",
                                    manifest.id
                                );
                                None
                            }
                        };
                        plugins.push(InstalledPlugin {
                            root: dir,
                            manifest,
                            embedded,
                            enabled: Arc::new(AtomicBool::new(settings.enabled)),
                            weight: Arc::new(AtomicI32::new(settings.weight)),
                            preferences: Arc::new(Mutex::new(settings.preferences)),
                            status,
                        });
                    }
                    Err(error) => {
                        eprintln!("[plugin] invalid manifest {:?}: {error}", manifest_path)
                    }
                }
            }
        }
        plugins.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        if !plugins.is_empty() {
            eprintln!("[plugin] discovered {} script plugin(s)", plugins.len());
        }
        Self {
            plugins,
            locale: Arc::new(Mutex::new(locale.to_owned())),
        }
    }

    pub fn set_locale(&self, locale: String) {
        if let Ok(mut current) = self.locale.lock() {
            *current = crate::utils::i18n::effective_language(&locale);
        }
    }

    pub fn replace_config(
        &self,
        plugin_configs: &std::collections::BTreeMap<String, PluginConfig>,
    ) {
        for plugin in &self.plugins {
            let settings = plugin_configs
                .get(&plugin.manifest.id)
                .cloned()
                .unwrap_or_default();
            plugin.enabled.store(settings.enabled, Ordering::Release);
            plugin.weight.store(settings.weight, Ordering::Release);
            if let Ok(mut status) = plugin.status.lock() {
                status.state = if settings.enabled {
                    if status.last_error.is_some() {
                        "error".into()
                    } else {
                        "ready".into()
                    }
                } else {
                    "disabled".into()
                };
            }
            if let Ok(mut preferences) = plugin.preferences.lock() {
                *preferences = settings.preferences;
            }
        }
    }

    #[allow(dead_code)]
    pub fn statuses(&self) -> Vec<PluginStatus> {
        self.plugins
            .iter()
            .map(|plugin| {
                let runtime = plugin
                    .status
                    .lock()
                    .ok()
                    .map(|status| status.clone())
                    .unwrap_or_else(|| PluginRuntimeStatus {
                        state: "error".into(),
                        last_method: None,
                        last_error: Some("plugin status lock poisoned".into()),
                        failure_count: 1,
                    });
                PluginStatus {
                    id: plugin.manifest.id.clone(),
                    name: plugin.manifest.name.clone(),
                    version: plugin.manifest.version.clone(),
                    enabled: plugin.enabled.load(Ordering::Acquire),
                    state: runtime.state,
                    last_method: runtime.last_method,
                    last_error: runtime.last_error,
                    failure_count: runtime.failure_count,
                }
            })
            .collect()
    }

    pub fn query(&self, query: &str) -> Vec<SearchResult> {
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "system".into());
        self.plugins
            .iter()
            .filter(|plugin| plugin.enabled.load(Ordering::Acquire))
            .flat_map(|plugin| {
                let (plugin_query, command_id) = action_word_query(plugin, query);
                match query_plugin(plugin, &plugin_query, &locale, command_id.as_deref(), query) {
                    Ok(items) => {
                        mark_success(plugin, "query");
                        items
                    }
                    Err(error) => {
                        mark_failure(plugin, "query", &error);
                        eprintln!("[plugin:{}] query failed: {error:#}", plugin.manifest.id);
                        Vec::new()
                    }
                }
            })
            .collect()
    }

    pub fn owns_search_engine_session(&self, session: &SearchEngineSession) -> bool {
        self.plugin_command_for_session(session).is_some()
    }

    pub fn query_active_search_engine(
        &self,
        session: &SearchEngineSession,
        raw_query: &str,
    ) -> Vec<SearchResult> {
        let Some((plugin, command_id)) = self.plugin_command_for_session(session) else {
            return Vec::new();
        };
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "system".into());
        match query_plugin(
            plugin,
            &session.argument,
            &locale,
            Some(&command_id),
            raw_query,
        ) {
            Ok(items) => {
                mark_success(plugin, "query");
                items
            }
            Err(error) => {
                mark_failure(plugin, "query", &error);
                eprintln!(
                    "[plugin:{}] search engine query failed: {error:#}",
                    plugin.manifest.id
                );
                Vec::new()
            }
        }
    }

    pub fn preferred_view_mode_for_session(&self, session: &SearchEngineSession) -> &'static str {
        self.plugin_command_for_session(session)
            .and_then(|(plugin, command_id)| {
                let command = plugin
                    .manifest
                    .commands
                    .iter()
                    .find(|command| command.id == command_id)?;
                (command.presentation.as_deref() == Some("grid")).then_some("grid")
            })
            .unwrap_or("list")
    }

    fn plugin_command_for_session<'a>(
        &'a self,
        session: &SearchEngineSession,
    ) -> Option<(&'a InstalledPlugin, String)> {
        let (plugin_id, command_id) = session
            .id
            .strip_prefix("plugin:")
            .and_then(|value| value.split_once(':'))?;
        let plugin = self.plugins.iter().find(|plugin| {
            plugin.enabled.load(Ordering::Acquire) && plugin.manifest.id == plugin_id
        })?;
        plugin
            .manifest
            .commands
            .iter()
            .any(|command| command.id == command_id)
            .then_some((plugin, command_id.to_owned()))
    }

    pub fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        self.plugins
            .iter()
            .filter(|plugin| plugin.enabled.load(Ordering::Acquire))
            .find_map(|plugin| {
                let (_, command_id) = action_word_query(plugin, query);
                let command_id = command_id?;
                let command = plugin
                    .manifest
                    .commands
                    .iter()
                    .find(|command| command.id == command_id)?;
                let word = query.trim_start().split_whitespace().next()?;
                let argument = query
                    .trim_start()
                    .strip_prefix(word)
                    .unwrap_or_default()
                    .trim_start();
                let title = if command.title.trim().is_empty() {
                    plugin.manifest.name.clone()
                } else {
                    command.title.clone()
                };
                Some(SearchEngineSession {
                    id: format!("plugin:{}:{}", plugin.manifest.id, command.id),
                    title,
                    prefix: format!("{word} "),
                    argument: argument.into(),
                    mode: command
                        .mode
                        .as_deref()
                        .map(SearchEngineMode::from_config)
                        .unwrap_or(SearchEngineMode::Search),
                })
            })
    }

    pub fn execute(&self, plugin_id: &str, item_id: &str, action_id: &str) -> Result<String> {
        let plugin = self
            .plugins
            .iter()
            .find(|p| p.manifest.id == plugin_id)
            .context("plugin not found")?;
        if !plugin.enabled.load(Ordering::Acquire) {
            anyhow::bail!("plugin is disabled");
        }
        let request_id = next_request_id("execute");
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "en".into());
        let request = serde_json::json!({"method":"execute", "request_id":request_id, "item_id":item_id, "action_id":action_id, "api_version":plugin.manifest.api_version, "locale":locale, "preferences": plugin_preferences(plugin)});
        let result: Result<String> = (|| {
            let embedded = plugin
                .embedded
                .as_ref()
                .context("plugin is not initialized")?;
            let embedded = embedded
                .lock()
                .map_err(|_| anyhow::anyhow!("plugin VM lock poisoned"))?;
            let output = embedded.call("execute", &request, Some(item_id), Some(action_id))?;
            validate_protocol_response(&output, &request_id)?;
            Ok(output)
        })();
        match result {
            Ok(output) => {
                mark_success(plugin, "execute");
                Ok(output)
            }
            Err(error) => {
                mark_failure(plugin, "execute", &error);
                Err(error)
            }
        }
    }
}

fn mark_success(plugin: &InstalledPlugin, method: &str) {
    if let Ok(mut status) = plugin.status.lock() {
        status.state = "ready".into();
        status.last_method = Some(method.into());
        status.last_error = None;
    }
}

fn mark_failure(plugin: &InstalledPlugin, method: &str, error: &anyhow::Error) {
    if let Ok(mut status) = plugin.status.lock() {
        status.state = "error".into();
        status.last_method = Some(method.into());
        status.last_error = Some(error.to_string());
        status.failure_count = status.failure_count.saturating_add(1);
    }
}

fn action_word_query(plugin: &InstalledPlugin, query: &str) -> (String, Option<String>) {
    let mut parts = query.trim_start().splitn(2, char::is_whitespace);
    let Some(word) = parts.next() else {
        return (query.to_owned(), None);
    };
    let argument = parts.next();
    let Some(command) = plugin.manifest.commands.iter().find(|command| {
        command
            .action_words
            .iter()
            .any(|action_word| action_word.eq_ignore_ascii_case(word))
    }) else {
        return (query.to_owned(), None);
    };
    if command.require_separator && argument.is_none() {
        return (query.to_owned(), None);
    }
    (
        argument.unwrap_or_default().trim_start().to_owned(),
        Some(command.id.clone()),
    )
}

fn query_plugin(
    plugin: &InstalledPlugin,
    query: &str,
    locale: &str,
    command_id: Option<&str>,
    requested_query: &str,
) -> Result<Vec<SearchResult>> {
    let request_id = next_request_id("query");
    let request = serde_json::json!({
        "method": "query",
        "request_id": request_id,
        "query": query,
        "command_id": command_id,
        "api_version": plugin.manifest.api_version,
        "locale": locale,
        "fallback_locale": plugin.manifest.default_locale,
        "preferences": plugin_preferences(plugin),
    });
    let embedded = plugin
        .embedded
        .as_ref()
        .context("plugin is not initialized")?;
    // A stale shortcut request may still be waiting on network I/O while the
    // user has already returned to ordinary search. Do not make that ordinary
    // search wait for the plugin VM; the plugin is optional for this query.
    let embedded = if command_id.is_none() {
        match embedded.try_lock() {
            Ok(embedded) => embedded,
            Err(TryLockError::WouldBlock) => return Ok(Vec::new()),
            Err(TryLockError::Poisoned(_)) => {
                anyhow::bail!("plugin VM lock poisoned")
            }
        }
    } else {
        embedded
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin VM lock poisoned"))?
    };
    let output = embedded.call("query", &request, None, None)?;
    let response: QueryResponse =
        serde_json::from_str(&output).context("decode plugin response")?;
    validate_query_response(&response, &request_id)?;
    Ok(response
        .items
        .into_iter()
        .map(|item| to_result(plugin, item, command_id, requested_query, locale))
        .collect())
}

fn next_request_id(method: &str) -> String {
    format!(
        "{method}-{}",
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn plugin_preferences(
    plugin: &InstalledPlugin,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    plugin
        .preferences
        .lock()
        .map(|preferences| preferences.clone())
        .unwrap_or_default()
}

fn validate_query_response(response: &QueryResponse, request_id: &str) -> Result<()> {
    validate_response_id(response.request_id.as_deref(), request_id)?;
    if let Some(error) = &response.error {
        anyhow::bail!("plugin error [{}]: {}", error.code, error.message);
    }
    Ok(())
}

fn validate_protocol_response(output: &str, request_id: &str) -> Result<()> {
    let value: serde_json::Value =
        serde_json::from_str(output).context("decode plugin response envelope")?;
    if let Some(response_id) = value.get("request_id").and_then(|value| value.as_str()) {
        validate_response_id(Some(response_id), request_id)?;
    }
    if let Some(error) = value.get("error") {
        let error: PluginError =
            serde_json::from_value(error.clone()).context("decode plugin error")?;
        anyhow::bail!("plugin error [{}]: {}", error.code, error.message);
    }
    Ok(())
}

fn validate_response_id(response_id: Option<&str>, request_id: &str) -> Result<()> {
    if let Some(response_id) = response_id {
        if response_id != request_id {
            anyhow::bail!(
                "plugin response request_id mismatch: expected {request_id}, got {response_id}"
            );
        }
    }
    Ok(())
}

fn to_result(
    plugin: &InstalledPlugin,
    item: PluginItem,
    requested_command_id: Option<&str>,
    requested_query: &str,
    language: &str,
) -> SearchResult {
    let item_command_id = item.command_id.as_deref().or(requested_command_id);
    let command = item_command_id.and_then(|command_id| {
        plugin
            .manifest
            .commands
            .iter()
            .find(|command| command.id == *command_id)
    });
    let command_keywords = command
        .map(|command| command.keywords.clone())
        .unwrap_or_default();
    let command_action_words = command
        .map(|command| command.action_words.clone())
        .unwrap_or_default();
    let command_query_alias = command.is_some().then(|| requested_query.to_owned());
    let icon = item
        .icon
        .or_else(|| command.and_then(|command| command.icon.clone()));
    let target = item.target.map(PathBuf::from).unwrap_or_default();
    let actions = item
        .actions
        .into_iter()
        .map(|action| Action::Plugin {
            plugin_id: plugin.manifest.id.clone(),
            action_id: action.id,
            icon: action
                .icon
                .and_then(|icon| resolve_plugin_icon(&plugin.root, icon)),
        })
        .collect();
    SearchResult {
        id: format!("plugin:{}:{}", plugin.manifest.id, item.id),
        identity: format!("plugin:{}:{}", plugin.manifest.id, item.id),
        title: item.title,
        // Command ownership routes the query to a plugin search engine; it
        // does not change what the plugin item is.
        category: ResultCategory::Application,
        aliases: item
            .aliases
            .into_iter()
            .chain(plugin.manifest.keywords.clone())
            .chain(command_keywords)
            .chain(command_action_words)
            .chain(command_query_alias)
            .collect(),
        location: item.subtitle,
        launch_target: target,
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: format!("plugin:{}", plugin.manifest.id).into(),
        sources: vec![format!("plugin:{}", plugin.manifest.id).into()],
        launch_entries: Vec::<LaunchEntry>::new(),
        actions,
        detail: item
            .detail
            .map(|view| to_detail_view(&plugin.root, view, language)),
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: icon.and_then(|icon| resolve_plugin_icon(&plugin.root, icon)),
        ranking_bias: plugin.weight.load(Ordering::Acquire),
    }
}

fn resolve_plugin_icon(root: &Path, icon: IconRef) -> Option<IconSpec> {
    match icon {
        IconRef::Resource { path } => plugin_resource_path(root, &path).map(IconSpec::Resource),
        IconRef::System { target } => Some(IconSpec::System(target.into())),
        IconRef::Named { id } => Some(IconSpec::Named(id)),
        IconRef::Remote { url } => validate_plugin_http_url(&url)
            .is_ok()
            .then_some(IconSpec::Remote(url)),
    }
}

fn plugin_resource_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return None;
    }
    Some(root.join(relative_path))
}

fn to_detail_view(
    root: &Path,
    view: PluginDetailView,
    language: &str,
) -> crate::result::DetailView {
    crate::result::DetailView {
        title: detail_text(view.title),
        subtitle: detail_text(view.subtitle),
        source: detail_text(view.source),
        actions: view
            .actions
            .into_iter()
            .take(3)
            .map(|action| crate::result::DetailAction {
                title: if action.title.is_empty() {
                    detail_text(action.id.as_str().into())
                } else {
                    detail_text(action.title)
                },
                id: detail_text(action.id),
                icon: action.icon.and_then(|icon| resolve_plugin_icon(root, icon)),
            })
            .collect(),
        blocks: view
            .blocks
            .into_iter()
            .take(10)
            .map(|block| match block {
                PluginDetailBlock::Text { text, color } => crate::result::DetailBlock::Text {
                    text: detail_text(text),
                    color: detail_color(&color),
                },
                PluginDetailBlock::Metric {
                    label,
                    value,
                    color,
                } => crate::result::DetailBlock::Metric {
                    label: detail_text(label),
                    value: detail_text(value),
                    color: detail_color(&color),
                },
                PluginDetailBlock::Icon { glyph, color, icon } => {
                    crate::result::DetailBlock::Icon {
                        glyph: detail_text(glyph),
                        color: detail_color(&color),
                        icon: icon.and_then(|icon| resolve_plugin_icon(root, icon)),
                    }
                }
                PluginDetailBlock::Weather { value } => crate::result::DetailBlock::Weather {
                    value: match value.as_str() {
                        "clear" | "cloud" | "rain" | "snow" | "storm" => value,
                        _ => "cloud".into(),
                    },
                },
                PluginDetailBlock::Markdown { markdown, color } => {
                    crate::result::DetailBlock::Markdown {
                        markdown: detail_text(markdown),
                        color: detail_color(&color),
                    }
                }
                PluginDetailBlock::Image { source, alt } => {
                    let Some(path) = plugin_resource_path(root, &source) else {
                        return crate::result::DetailBlock::Error {
                            message: crate::utils::i18n::tr(language, "Preview image unavailable")
                                .into(),
                            retry_action: None,
                        };
                    };
                    crate::result::DetailBlock::Image {
                        source: path.to_string_lossy().into_owned(),
                        alt: detail_text(alt),
                    }
                }
                PluginDetailBlock::Code { language, code } => crate::result::DetailBlock::Code {
                    language: detail_text(language),
                    code: code.chars().take(8_192).collect(),
                },
                PluginDetailBlock::Table { columns, rows } => crate::result::DetailBlock::Table {
                    columns: columns.into_iter().take(8).map(detail_text).collect(),
                    rows: rows
                        .into_iter()
                        .take(32)
                        .map(|row| row.into_iter().take(8).map(detail_text).collect())
                        .collect(),
                },
                PluginDetailBlock::Link { label, url } => {
                    if is_supported_detail_url(&url) {
                        crate::result::DetailBlock::Link {
                            label: detail_text(label),
                            url: detail_text(url),
                        }
                    } else {
                        crate::result::DetailBlock::Error {
                            message: crate::utils::i18n::tr(language, "Preview link unsupported")
                                .into(),
                            retry_action: None,
                        }
                    }
                }
                PluginDetailBlock::Loading { message } => crate::result::DetailBlock::Loading {
                    message: if message == "Loading preview" {
                        crate::utils::i18n::tr(language, "Loading preview").into()
                    } else {
                        detail_text(message)
                    },
                },
                PluginDetailBlock::Error {
                    message,
                    retry_action,
                } => crate::result::DetailBlock::Error {
                    message: detail_text(message),
                    retry_action: retry_action.map(detail_text),
                },
                PluginDetailBlock::Separator => crate::result::DetailBlock::Separator,
            })
            .collect(),
        preferred_width: view.width.map(|width| {
            width
                .preferred
                .clamp(width.min.min(width.max), width.max.max(width.min))
        }),
        scrollable: view.scrollable,
    }
}

fn detail_text(value: String) -> String {
    value.chars().take(512).collect()
}

fn detail_color(value: &str) -> String {
    match value {
        "primary" | "secondary" | "tertiary" | "accent" => value.into(),
        _ => "secondary".into(),
    }
}

fn is_supported_detail_url(value: &str) -> bool {
    let value = value.trim();
    value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .is_some_and(|rest| !rest.is_empty() && !rest.chars().any(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_defaults_are_valid() {
        let m: PluginManifest =
            serde_json::from_str(r#"{"id":"x","name":"X","entry_point":"main.lua"}"#).unwrap();
        assert_eq!(m.api_version, 1);
        assert_eq!(m.version, "0.1.0");
    }

    #[test]
    fn manifest_commands_are_optional_and_deserialized() {
        let m: PluginManifest = serde_json::from_str(
            r#"{
                "id":"github",
                "name":"GitHub",
                "entry_point":"main.py",
                "commands":[{
                    "id":"search",
                    "title":"Search GitHub",
                    "keywords":["gh"],
                    "action_words":["gh"],
                    "presentation":"grid",
                    "icon":{"type":"resource","path":"icons/github.png"}
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(m.commands[0].id, "search");
        assert_eq!(m.commands[0].action_words, vec!["gh"]);
        assert_eq!(m.commands[0].presentation.as_deref(), Some("grid"));
        assert!(matches!(m.commands[0].icon, Some(IconRef::Resource { .. })));
    }

    #[test]
    fn plugin_icon_resources_cannot_escape_plugin_root() {
        assert_eq!(
            plugin_resource_path(Path::new("plugins/github"), "icons/github.png"),
            Some(PathBuf::from("plugins/github/icons/github.png"))
        );
        assert!(plugin_resource_path(Path::new("plugins/github"), "../secret.png").is_none());
        assert!(plugin_resource_path(Path::new("plugins/github"), "C:\\secret.png").is_none());
    }

    #[test]
    fn plugin_urls_are_limited_to_http_without_credentials() {
        assert!(validate_plugin_http_url("https://api.dictionaryapi.dev/entries/en/hello").is_ok());
        assert!(validate_plugin_http_url("http://example.com/icon.svg").is_ok());
        assert!(validate_plugin_http_url("file:///secret").is_err());
        assert!(validate_plugin_http_url("https://user:pass@example.com/icon.svg").is_err());
        assert!(validate_plugin_http_url("https://example.com/a b").is_err());
    }

    #[test]
    fn icon_path_encoding_uses_percent_encoding_not_query_plus_encoding() {
        assert_eq!(encode_url_path_segment("line icon/+"), "line%20icon%2F%2B");
        assert_eq!(encode_url_path_segment("alarm-clock"), "alarm-clock");
    }

    #[test]
    fn action_word_query_selects_command_and_strips_prefix() {
        let plugin = InstalledPlugin {
            root: PathBuf::from("plugins/github"),
            manifest: PluginManifest {
                id: "github".into(),
                name: "GitHub".into(),
                version: "1.0.0".into(),
                api_version: 2,
                entry_point: "main.py".into(),
                keywords: Vec::new(),
                commands: vec![PluginCommand {
                    id: "search".into(),
                    title: "Search GitHub".into(),
                    keywords: vec!["gh".into()],
                    action_words: vec!["gh".into()],
                    presentation: None,
                    mode: None,
                    require_separator: false,
                    icon: None,
                }],
                permissions: Vec::new(),
                default_locale: "en".into(),
            },
            embedded: None,
            enabled: Arc::new(AtomicBool::new(true)),
            weight: Arc::new(AtomicI32::new(0)),
            preferences: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            status: Arc::new(Mutex::new(PluginRuntimeStatus {
                state: "ready".into(),
                last_method: None,
                last_error: None,
                failure_count: 0,
            })),
        };
        assert_eq!(
            action_word_query(&plugin, "GH react"),
            ("react".into(), Some("search".into()))
        );
        assert_eq!(
            action_word_query(&plugin, "gh   "),
            (String::new(), Some("search".into()))
        );
        assert_eq!(
            action_word_query(&plugin, "github react"),
            ("github react".into(), None)
        );
    }

    #[test]
    fn response_request_ids_are_optional_but_validated_when_present() {
        validate_response_id(None, "query-1").unwrap();
        validate_response_id(Some("query-1"), "query-1").unwrap();
        assert!(validate_response_id(Some("query-2"), "query-1").is_err());
    }

    #[test]
    fn structured_plugin_errors_are_reported() {
        let error = validate_protocol_response(
            r#"{"request_id":"execute-1","error":{"code":"unauthorized","message":"token required"}}"#,
            "execute-1",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unauthorized"));
        assert!(error.contains("token required"));
    }

    #[test]
    fn empty_lua_item_tables_are_treated_as_empty_plugin_results() {
        let response: QueryResponse = serde_json::from_str(r#"{"items":{}}"#).unwrap();
        assert!(response.items.is_empty());
    }

    #[test]
    fn embedded_lua_can_query_and_execute_with_preferences() {
        let source = r#"
            wowie.on("query", function(query, context)
                return {
                    request_id = context.request_id,
                    items = {
                        wowie.result({
                            id = "demo",
                            title = "Lua " .. query,
                            subtitle = context.preferences.token
                        })
                    }
                }
            end)
            wowie.on("execute", function(item_id, action_id, context)
                return {
                    request_id = context.request_id,
                    title = item_id .. ":" .. action_id,
                    subtitle = context.preferences.token
                }
            end)
        "#;
        let mut preferences = std::collections::BTreeMap::new();
        preferences.insert("token".into(), serde_json::json!("from-preferences"));
        let plugin =
            EmbeddedPlugin::from_source(source, "test.lua", "zh-Hans", "en", &preferences, &[])
                .unwrap();
        let query = serde_json::json!({
            "request_id": "query-test",
            "query": "hello",
            "preferences": preferences,
        });
        let response = plugin.call("query", &query, None, None).unwrap();
        let response: QueryResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(response.request_id.as_deref(), Some("query-test"));
        assert_eq!(response.items[0].title, "Lua hello");
        assert_eq!(response.items[0].subtitle, "from-preferences");

        let execute = serde_json::json!({
            "request_id": "execute-test",
            "preferences": preferences,
        });
        let response = plugin
            .call("execute", &execute, Some("demo"), Some("open"))
            .unwrap();
        let response: ExecuteResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(response.request_id.as_deref(), Some("execute-test"));
        assert_eq!(response.title, "demo:open");
        assert_eq!(response.subtitle, "from-preferences");
    }

    #[test]
    fn embedded_lua_detail_without_actions_is_a_valid_result() {
        let source = r#"
            wowie.on("query", function(query, context)
                return {
                    request_id = context.request_id,
                    items = {
                        wowie.result({
                            id = "definition",
                            title = query,
                            detail = {
                                title = "Definition",
                                blocks = {
                                    { type = "text", text = "A definition" }
                                }
                            }
                        })
                    }
                }
            end)
        "#;
        let preferences = std::collections::BTreeMap::new();
        let plugin =
            EmbeddedPlugin::from_source(source, "test.lua", "en", "en", &preferences, &[]).unwrap();
        let query = serde_json::json!({
            "request_id": "query-detail",
            "query": "hello",
        });
        let response = plugin.call("query", &query, None, None).unwrap();
        let response: QueryResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(response.items.len(), 1);
        assert_eq!(
            response.items[0].detail.as_ref().unwrap().title,
            "Definition"
        );
    }

    #[test]
    fn plugin_discovery_is_empty_when_no_plugins_are_installed() {
        let manager = PluginManager::discover_with_config("en", &Default::default());
        assert!(manager.search_engine("eng").is_none());
        assert!(manager.query("eng ").is_empty());
        assert!(manager.statuses().is_empty());
    }

    #[test]
    fn detail_view_is_deserialized_and_limited() {
        let view: PluginDetailView = serde_json::from_str(
            r#"{
                "title":"Repository",
                "blocks":[
                    {"type":"metric","label":"Stars","value":"230k","color":"primary"},
                    {"type":"weather","value":"storm"},
                    {"type":"text","text":"ignored","color":"invalid"}
                ],
                "actions":[{"id":"open","title":"Open"}]
            }"#,
        )
        .unwrap();
        let detail = to_detail_view(Path::new("."), view, "en");
        assert_eq!(detail.title, "Repository");
        assert_eq!(detail.blocks.len(), 3);
        assert_eq!(detail.actions[0].title, "Open");
        assert!(matches!(
            detail.blocks[1],
            crate::result::DetailBlock::Weather { .. }
        ));
        assert_eq!(
            detail.blocks[2],
            crate::result::DetailBlock::Text {
                text: "ignored".into(),
                color: "secondary".into(),
            }
        );
    }
}
