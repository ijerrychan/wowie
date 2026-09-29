//! Embedded Lua plugin support.
//!
//! Plugins execute inside Wowie.  The host owns the Lua VM and exposes a small
//! data-oriented API, so plugin execution does not depend on a user-installed
//! runtime and never creates a plugin child process.

use crate::{
    config::PluginConfig,
    page::{SearchEngineMode, SearchEngineSession},
    provider::{EngineSearchResult, SearchEngineSettings},
    result::{
        Action, ContextMenuAction, ContextMenuBehavior, IconRef, IconSpec, LaunchEntry,
        ResultCategory, SearchResult,
    },
};
use anyhow::{Context, Result};
use mlua::{Function, HookTriggers, Lua, LuaSerdeExt, Value, VmState};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use rayon::prelude::*;
use serde::{de, de::DeserializeOwned, Deserialize, Deserializer};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        Arc, Mutex, OnceLock, RwLock, TryLockError,
    },
    time::{Duration, Instant},
};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static PLUGIN_QUERY_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

/// Keep an individual plugin from flooding the root aggregator. Plugins should
/// return their most relevant rows first; the host still performs the final
/// fuzzy match and ranking over this bounded contribution.
const MAX_ROOT_PLUGIN_RESULTS: usize = 128;
const MAX_SESSION_PLUGIN_RESULTS: usize = 256;
const MAX_PLUGIN_CONTEXT_ACTIONS: usize = 16;
const MAX_PLUGIN_CONTEXT_ACTION_ID_BYTES: usize = 128;
const MAX_PLUGIN_CONTEXT_TITLE_CHARS: usize = 160;
const MAX_PLUGIN_CONTEXT_SUBTITLE_CHARS: usize = 240;
const MAX_PLUGIN_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PLUGIN_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_PLUGIN_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PLUGIN_SMALL_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_PLUGIN_ITEMS: usize = 512;
const MAX_PLUGIN_ITEM_ID_CHARS: usize = 256;
const MAX_PLUGIN_ITEM_TITLE_CHARS: usize = 512;
const MAX_PLUGIN_ITEM_SUBTITLE_CHARS: usize = 4096;
const MAX_PLUGIN_ITEM_ALIASES: usize = 32;
const MAX_PLUGIN_ALIAS_CHARS: usize = 256;
const MAX_PLUGIN_ACTIONS: usize = 32;
const MAX_PLUGIN_DETAIL_BLOCKS: usize = 128;
const MAX_PLUGIN_DETAIL_ACTIONS: usize = 32;
const MAX_PLUGIN_DETAIL_TEXT_CHARS: usize = 64 * 1024;
const MAX_PLUGIN_DETAIL_ROWS: usize = 256;
const MAX_PLUGIN_DETAIL_CELLS: usize = 64;
const MAX_PLUGIN_JSON_DEPTH: usize = 32;
const MAX_PLUGIN_JSON_NODES: usize = 20_000;
const MAX_PLUGIN_JSON_STRING_BYTES: usize = 2 * 1024 * 1024;
const ROOT_PLUGIN_QUERY_BUDGET: Duration = Duration::from_millis(150);
const SESSION_PLUGIN_QUERY_BUDGET: Duration = Duration::from_millis(750);
const QUERY_HOOK_INSTRUCTION_INTERVAL: u32 = 10_000;
const QUERY_CANCELLED: &str = "plugin query cancelled";
const QUERY_TIMED_OUT: &str = "plugin query timed out";
const MAX_EVERYTHING_IPC_RESULTS: usize = 256;
const MAX_PLUGIN_FUZZY_CANDIDATES: usize = 20_000;
const MAX_PLUGIN_FUZZY_FIELDS: usize = 8;
const MAX_PLUGIN_FUZZY_TEXT_BYTES: usize = 4096;
const MAX_PLUGIN_FUZZY_QUERY_BYTES: usize = 4096;

struct PluginFuzzyScratch {
    matcher: Matcher,
    text_buffer: Vec<char>,
    query_buffer: Vec<char>,
}

impl PluginFuzzyScratch {
    fn new() -> Self {
        Self {
            matcher: Matcher::new(MatcherConfig::DEFAULT),
            text_buffer: Vec::new(),
            query_buffer: Vec::new(),
        }
    }

    fn matches(&mut self, text: &str, query: &str) -> bool {
        if query.trim().is_empty() {
            return true;
        }
        let haystack = Utf32Str::new(text, &mut self.text_buffer);
        let needle = Utf32Str::new(query, &mut self.query_buffer);
        self.matcher.fuzzy_match(haystack, needle).is_some()
    }
}

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
    /// Settings exposed in the host's settings window. Values are stored in
    /// `plugins.<id>.preferences` and passed back to the plugin at runtime.
    #[serde(default)]
    pub preferences: Vec<PluginPreference>,
    /// Standard engine behaviour. Plugins may contribute to root search
    /// without declaring a command/session entry.
    #[serde(default)]
    pub engine: SearchEngineSettings,
    #[serde(default)]
    #[allow(dead_code)]
    pub permissions: Vec<String>,
    #[serde(default = "default_locale")]
    pub default_locale: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginPreference {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "type", default = "default_preference_type")]
    pub value_type: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub default: serde_json::Value,
}

fn default_preference_type() -> String {
    "text".into()
}

#[derive(Debug, Clone)]
pub struct PluginSettingsDefinition {
    pub id: String,
    pub name: String,
    pub preferences: Vec<PluginPreference>,
    pub commands: Vec<PluginCommandSettingsDefinition>,
}

#[derive(Debug, Clone)]
pub struct PluginCommandSettingsDefinition {
    pub id: String,
    pub title: String,
    pub action_words: Vec<String>,
    pub customizable_action_words: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PluginCommand {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// Optional search-field hint for this command's session.
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub action_words: Vec<String>,
    /// Let users replace the words that enter this command from root search.
    #[serde(default)]
    pub customizable_action_words: bool,
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
    has_detail_handler: bool,
    enabled: Arc<AtomicBool>,
    weight: Arc<AtomicI32>,
    preferences: Arc<Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    action_word_overrides: Arc<RwLock<BTreeMap<String, Vec<String>>>>,
    status: Arc<Mutex<PluginRuntimeStatus>>,
}

#[derive(Debug, Clone)]
struct PluginRuntimeStatus {
    state: String,
    last_method: Option<String>,
    last_error: Option<String>,
    failure_count: u32,
    last_query_ms: u64,
    last_query_results: usize,
    query_truncations: u64,
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
    pub last_query_ms: u64,
    pub last_query_results: usize,
    pub query_truncations: u64,
}

#[derive(Clone)]
struct QueryCancellation {
    epoch: Arc<AtomicU64>,
    expected_epoch: u64,
    budget: Duration,
}

struct PluginQueryOptions<'a> {
    command_id: Option<&'a str>,
    requested_query: &'a str,
    session: Option<&'a SearchEngineSession>,
    search_history: &'a [crate::provider::SearchHistoryEntry],
    offset: usize,
    max_results: usize,
    cancellation: Option<QueryCancellation>,
}

impl QueryCancellation {
    fn new(epoch: &Arc<AtomicU64>, expected_epoch: u64, budget: Duration) -> Self {
        Self {
            epoch: Arc::clone(epoch),
            expected_epoch,
            budget,
        }
    }
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
        let metadata = fs::metadata(&entry)
            .with_context(|| format!("read plugin entry point metadata {}", entry.display()))?;
        if metadata.len() > MAX_PLUGIN_SOURCE_BYTES as u64 {
            anyhow::bail!(
                "plugin entry point {} exceeds {} bytes",
                entry.display(),
                MAX_PLUGIN_SOURCE_BYTES
            );
        }
        let source = crate::utils::read_bounded(
            fs::File::open(&entry)
                .with_context(|| format!("open plugin entry point {}", entry.display()))?,
            MAX_PLUGIN_SOURCE_BYTES,
        )?;
        let source = String::from_utf8(source).context("plugin entry point is not UTF-8")?;
        Self::from_source(
            &source,
            &manifest.entry_point,
            locale,
            manifest.default_locale.as_str(),
            preferences,
            &manifest.permissions,
            Some(plugin_storage_dir(&manifest.id)),
        )
    }

    fn from_source(
        source: &str,
        chunk_name: &str,
        locale: &str,
        fallback_locale: &str,
        preferences: &BTreeMap<String, serde_json::Value>,
        permissions: &[String],
        storage_dir: Option<PathBuf>,
    ) -> Result<Self> {
        if source.len() > MAX_PLUGIN_SOURCE_BYTES {
            anyhow::bail!("plugin source exceeds {} bytes", MAX_PLUGIN_SOURCE_BYTES);
        }
        let lua = Lua::new();
        let wowie = lua.create_table()?;
        let locale_value = locale.to_owned();
        let fallback_value = fallback_locale.to_owned();
        let preferences_value = preferences.clone();
        let preferences_json = serde_json::to_value(&preferences_value)?;
        validate_json_shape(&preferences_json)?;

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
                validate_json_shape(&value).map_err(mlua::Error::external)?;
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
        let network_allowed = has_plugin_permission(permissions, "network");
        wowie.set(
            "http_post_form_json",
            lua.create_function(move |lua, (url, form): (String, mlua::Table)| {
                require_plugin_permission(network_allowed, "network")?;
                validate_plugin_http_url(&url).map_err(mlua::Error::external)?;
                validate_lua_value(&Value::Table(form.clone())).map_err(mlua::Error::external)?;
                let form: BTreeMap<String, String> = lua.from_value(Value::Table(form))?;
                let value: serde_json::Value = crate::utils::http::post_form_json(&url, &form)
                    .map_err(mlua::Error::external)?;
                validate_json_shape(&value).map_err(mlua::Error::external)?;
                lua.to_value(&value)
            })?,
        )?;
        let network_allowed = has_plugin_permission(permissions, "network");
        wowie.set(
            "http_post_json",
            lua.create_function(
                move |lua, (url, body, headers): (String, mlua::Table, Option<mlua::Table>)| {
                    require_plugin_permission(network_allowed, "network")?;
                    validate_plugin_http_url(&url).map_err(mlua::Error::external)?;
                    validate_lua_value(&Value::Table(body.clone()))
                        .map_err(mlua::Error::external)?;
                    if let Some(headers) = &headers {
                        validate_lua_value(&Value::Table(headers.clone()))
                            .map_err(mlua::Error::external)?;
                    }
                    let body: serde_json::Value = lua.from_value(Value::Table(body))?;
                    let headers: BTreeMap<String, String> = headers
                        .map(|headers| lua.from_value(Value::Table(headers)))
                        .transpose()?
                        .unwrap_or_default();
                    let value: serde_json::Value =
                        crate::utils::http::post_json_with_headers(&url, &body, &headers)
                            .map_err(mlua::Error::external)?;
                    validate_json_shape(&value).map_err(mlua::Error::external)?;
                    lua.to_value(&value)
                },
            )?,
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
        let read_storage_dir = storage_dir.clone();
        wowie.set(
            "storage_read",
            lua.create_function(move |lua, key: String| {
                let path = plugin_storage_path(read_storage_dir.as_deref(), &key)?;
                match fs::metadata(&path) {
                    Ok(metadata) if metadata.len() <= MAX_PLUGIN_STORAGE_VALUE_BYTES as u64 => {}
                    Ok(_) => return Err(mlua::Error::runtime("plugin storage value is too large")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(mlua::Error::external(error)),
                }
                let bytes = crate::utils::read_bounded(
                    fs::File::open(path).map_err(mlua::Error::external)?,
                    MAX_PLUGIN_STORAGE_VALUE_BYTES,
                )
                .map_err(mlua::Error::external)?;
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(mlua::Error::external)?;
                validate_json_shape(&value).map_err(mlua::Error::external)?;
                lua.to_value(&value).map(Some)
            })?,
        )?;
        let write_storage_dir = storage_dir.clone();
        wowie.set(
            "storage_write",
            lua.create_function(move |lua, (key, value): (String, Value)| {
                validate_lua_value(&value).map_err(mlua::Error::external)?;
                let value: serde_json::Value = lua.from_value(value)?;
                validate_json_shape(&value).map_err(mlua::Error::external)?;
                let bytes = serde_json::to_vec(&value).map_err(mlua::Error::external)?;
                if bytes.len() > MAX_PLUGIN_STORAGE_VALUE_BYTES {
                    return Err(mlua::Error::runtime("plugin storage value is too large"));
                }
                let path = plugin_storage_path(write_storage_dir.as_deref(), &key)?;
                crate::utils::atomic_write(&path, &bytes).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
        let fuzzy_scratch = Arc::new(Mutex::new(PluginFuzzyScratch::new()));
        let single_fuzzy_scratch = Arc::clone(&fuzzy_scratch);
        wowie.set(
            "fuzzy_match",
            lua.create_function(move |_, (text, query): (String, String)| {
                let mut scratch = single_fuzzy_scratch
                    .lock()
                    .map_err(|_| mlua::Error::runtime("fuzzy matcher is unavailable"))?;
                Ok(scratch.matches(&text, &query))
            })?,
        )?;
        wowie.set(
            "fuzzy_filter",
            lua.create_function(move |lua, (candidates, query): (mlua::Table, String)| {
                if query.len() > MAX_PLUGIN_FUZZY_QUERY_BYTES {
                    return Err(mlua::Error::runtime("fuzzy query is too long"));
                }
                if candidates.raw_len() > MAX_PLUGIN_FUZZY_CANDIDATES {
                    return Err(mlua::Error::runtime("too many fuzzy candidates"));
                }
                let results = lua.create_table()?;
                let mut scratch = fuzzy_scratch
                    .lock()
                    .map_err(|_| mlua::Error::runtime("fuzzy matcher is unavailable"))?;
                let PluginFuzzyScratch {
                    matcher,
                    text_buffer,
                    query_buffer,
                } = &mut *scratch;
                let needle = Utf32Str::new(&query, query_buffer);
                let match_all = query.trim().is_empty();
                let mut result_count = 0;
                for (index, candidate) in candidates.sequence_values::<Value>().enumerate() {
                    let candidate = candidate?;
                    let mut matches = match_all;
                    if !matches {
                        let mut check_text = |text: mlua::String| -> mlua::Result<bool> {
                            let text = text.to_str()?;
                            if text.len() > MAX_PLUGIN_FUZZY_TEXT_BYTES {
                                return Err(mlua::Error::runtime(
                                    "fuzzy candidate text is too long",
                                ));
                            }
                            let haystack = Utf32Str::new(&text, text_buffer);
                            Ok(matcher.fuzzy_match(haystack, needle).is_some())
                        };
                        matches = match candidate {
                            Value::String(text) => check_text(text)?,
                            Value::Table(fields) => {
                                if fields.raw_len() > MAX_PLUGIN_FUZZY_FIELDS {
                                    return Err(mlua::Error::runtime(
                                        "too many fuzzy candidate fields",
                                    ));
                                }
                                let mut found = false;
                                for field in fields.sequence_values::<mlua::String>() {
                                    if check_text(field?)? {
                                        found = true;
                                        break;
                                    }
                                }
                                found
                            }
                            _ => return Err(mlua::Error::runtime("invalid fuzzy candidate")),
                        };
                    }
                    if matches {
                        result_count += 1;
                        results.raw_set(result_count, index + 1)?;
                    }
                }
                Ok(results)
            })?,
        )?;
        // File-system access is deliberately generic: plugins can implement
        // their own local indexes without introducing host-side provider
        // special cases.  The manifest must opt in with the `filesystem`
        // permission.
        let filesystem_allowed = has_plugin_permission(permissions, "filesystem");
        wowie.set(
            "env",
            lua.create_function(move |_, name: String| {
                require_plugin_permission(filesystem_allowed, "filesystem")?;
                Ok(std::env::var(name).ok())
            })?,
        )?;
        let filesystem_allowed = has_plugin_permission(permissions, "filesystem");
        wowie.set(
            "fs_read_text",
            lua.create_function(move |_, path: String| {
                require_plugin_permission(filesystem_allowed, "filesystem")?;
                let path = PathBuf::from(path);
                let metadata = match fs::metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(mlua::Error::external(error)),
                };
                if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
                    return Err(mlua::Error::runtime(
                        "plugin file is too large or not a file",
                    ));
                }
                let bytes = crate::utils::read_bounded(
                    fs::File::open(path).map_err(mlua::Error::external)?,
                    4 * 1024 * 1024,
                )
                .map_err(mlua::Error::external)?;
                Ok(Some(
                    String::from_utf8(bytes).map_err(mlua::Error::external)?,
                ))
            })?,
        )?;
        let filesystem_allowed = has_plugin_permission(permissions, "filesystem");
        wowie.set(
            "fs_list",
            lua.create_function(move |lua, path: String| {
                require_plugin_permission(filesystem_allowed, "filesystem")?;
                let result = lua.create_table()?;
                let entries = match fs::read_dir(PathBuf::from(path)) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(result)
                    }
                    Err(error) => return Err(mlua::Error::external(error)),
                };
                for (index, entry) in entries.take(4096).enumerate() {
                    let entry = entry.map_err(mlua::Error::external)?;
                    let metadata = entry.metadata().map_err(mlua::Error::external)?;
                    let item = lua.create_table()?;
                    item.set("name", entry.file_name().to_string_lossy().to_string())?;
                    item.set("path", entry.path().to_string_lossy().to_string())?;
                    item.set("is_dir", metadata.is_dir())?;
                    item.set("size", metadata.len())?;
                    result.set(index + 1, item)?;
                }
                Ok(result)
            })?,
        )?;
        let everything_allowed = has_plugin_permission(permissions, "everything");
        wowie.set(
            "everything_search",
            lua.create_function(
                move |lua, (query, offset, limit): (String, Option<u64>, Option<u64>)| {
                    require_plugin_permission(everything_allowed, "everything")?;
                    let offset = offset.unwrap_or_default().min(usize::MAX as u64) as usize;
                    let limit = limit
                        .unwrap_or(30)
                        .clamp(1, MAX_EVERYTHING_IPC_RESULTS as u64)
                        as usize;
                    let rows =
                        query_everything(&query, offset, limit).map_err(mlua::Error::external)?;
                    let result = lua.create_table()?;
                    for (index, (name, path)) in rows.into_iter().enumerate() {
                        let item = lua.create_table()?;
                        item.set("name", name)?;
                        item.set("path", path)?;
                        result.set(index + 1, item)?;
                    }
                    Ok(result)
                },
            )?,
        )?;
        let everything_allowed = has_plugin_permission(permissions, "everything");
        wowie.set(
            "everything_icon_target",
            lua.create_function(move |_, _: ()| {
                require_plugin_permission(everything_allowed, "everything")?;
                Ok(everything_icon_target())
            })?,
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
            "request_id": next_request_id("initialize"),
            "locale": locale,
            "fallback_locale": fallback_locale,
            "preferences": preferences_value,
        });
        if plugin.has_handler("initialize")? {
            let request_id = initialize
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let output = plugin.call("initialize", &initialize, None, None)?;
            validate_protocol_response(&output, request_id)?;
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
            (Some(item_id), None) if method == "detail" => {
                callback.call::<Value>((item_id, context))?
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
            validate_lua_value(&value)?;
            self.lua.from_value(value)?
        };
        let output = serde_json::to_string(&value)?;
        if output.len() > MAX_PLUGIN_RESPONSE_BYTES {
            anyhow::bail!(
                "plugin {method} response exceeds {} bytes",
                MAX_PLUGIN_RESPONSE_BYTES
            );
        }
        validate_json_shape(&value)?;
        Ok(output)
    }

    fn call_query(
        &self,
        request: &serde_json::Value,
        cancellation: Option<QueryCancellation>,
    ) -> Result<String> {
        let Some(cancellation) = cancellation else {
            return self.call("query", request, None, None);
        };

        let deadline = Instant::now() + cancellation.budget;
        let expected_epoch = cancellation.expected_epoch;
        let epoch = cancellation.epoch;
        self.lua.set_hook(
            HookTriggers::new().every_nth_instruction(QUERY_HOOK_INSTRUCTION_INTERVAL),
            move |_lua, _debug| {
                if epoch.load(Ordering::Acquire) != expected_epoch {
                    Err(mlua::Error::runtime(QUERY_CANCELLED))
                } else if Instant::now() >= deadline {
                    Err(mlua::Error::runtime(QUERY_TIMED_OUT))
                } else {
                    Ok(VmState::Continue)
                }
            },
        );
        let result = self.call("query", request, None, None);
        self.lua.remove_hook();
        result
    }
}

const MAX_PLUGIN_STORAGE_VALUE_BYTES: usize = 8 * 1024 * 1024;

fn plugin_storage_dir(plugin_id: &str) -> PathBuf {
    let id = plugin_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Wowie")
        .join("plugin-data")
        .join(id)
}

fn plugin_storage_path(storage_dir: Option<&Path>, key: &str) -> mlua::Result<PathBuf> {
    if key.is_empty()
        || key.len() > 128
        || key == "."
        || key == ".."
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(mlua::Error::runtime("invalid plugin storage key"));
    }
    storage_dir
        .map(|dir| dir.join(format!("{key}.json")))
        .ok_or_else(|| mlua::Error::runtime("plugin storage is unavailable"))
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
    crate::utils::clipboard::set_text(text)
}

fn query_everything(
    query: &str,
    offset: usize,
    max_results: usize,
) -> Result<Vec<(String, String)>> {
    #[cfg(windows)]
    {
        use everything_ipc::wm::{EverythingClient, RequestFlags};

        let client = EverythingClient::new().context("Everything IPC is unavailable")?;
        let list = client
            .query_wait(query)
            .request_flags(RequestFlags::FileName | RequestFlags::Path)
            .offset(offset.min(u32::MAX as usize) as u32)
            .max_results(max_results.min(u32::MAX as usize) as u32)
            .call()
            .context("Everything IPC query failed")?;
        Ok(list
            .iter()
            .filter_map(|item| {
                let name = item.get_string(RequestFlags::FileName)?;
                let path = item.get_string(RequestFlags::Path)?;
                let full = PathBuf::from(path).join(&name);
                Some((name, full.to_string_lossy().into_owned()))
            })
            .collect())
    }
    #[cfg(not(windows))]
    {
        let _ = (query, offset, max_results);
        Ok(Vec::new())
    }
}

fn everything_icon_target() -> Option<String> {
    #[cfg(windows)]
    {
        static TARGET: OnceLock<Option<String>> = OnceLock::new();
        TARGET
            .get_or_init(|| {
                find_everything_executable().map(|path| path.to_string_lossy().into_owned())
            })
            .clone()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn find_everything_executable() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        candidates
            .extend(std::env::split_paths(&path).map(|directory| directory.join("Everything.exe")));
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            let root = PathBuf::from(root);
            candidates.push(root.join("Everything").join("Everything.exe"));
            if let Ok(entries) = fs::read_dir(&root) {
                candidates.extend(entries.flatten().filter_map(|entry| {
                    let name = entry.file_name();
                    name.to_string_lossy()
                        .to_ascii_lowercase()
                        .starts_with("everything")
                        .then(|| entry.path().join("Everything.exe"))
                }));
            }
        }
    }
    candidates.into_iter().find(|path| path.is_file())
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

mod protocol;
use protocol::*;
#[allow(unused_imports)]
pub use protocol::{ExecuteResponse, PluginError};

#[derive(Clone)]
pub struct PluginManager {
    plugins: Vec<InstalledPlugin>,
    locale: Arc<Mutex<String>>,
    query_epoch: Arc<AtomicU64>,
}

fn plugin_config_for(
    plugin_id: &str,
    plugin_configs: &std::collections::BTreeMap<String, PluginConfig>,
) -> PluginConfig {
    plugin_configs
        .get(plugin_id)
        .or_else(|| {
            (plugin_id == "google-translate")
                .then(|| plugin_configs.get("tencent-translate"))
                .flatten()
        })
        .cloned()
        .unwrap_or_default()
}

fn merge_preference_defaults(
    mut settings: PluginConfig,
    manifest: &PluginManifest,
) -> PluginConfig {
    for preference in &manifest.preferences {
        settings
            .preferences
            .entry(preference.key.clone())
            .or_insert_with(|| preference.default.clone());
    }
    settings
}

fn configured_action_word_overrides(
    manifest: &PluginManifest,
    settings: &PluginConfig,
) -> BTreeMap<String, Vec<String>> {
    manifest
        .commands
        .iter()
        .filter(|command| command.customizable_action_words)
        .filter_map(|command| {
            settings
                .command_action_words
                .get(&command.id)
                .map(|words| (command.id.clone(), words.clone()))
        })
        .collect()
}

fn command_action_words(plugin: &InstalledPlugin, command: &PluginCommand) -> Vec<String> {
    if command.customizable_action_words {
        if let Ok(overrides) = plugin.action_word_overrides.read() {
            if let Some(words) = overrides.get(&command.id) {
                return words.clone();
            }
        }
    }
    command.action_words.clone()
}

impl PluginManager {
    pub fn settings_definitions(&self) -> Vec<PluginSettingsDefinition> {
        let mut definitions = self
            .plugins
            .iter()
            .map(|plugin| PluginSettingsDefinition {
                id: plugin.manifest.id.clone(),
                name: plugin.manifest.name.clone(),
                preferences: plugin.manifest.preferences.clone(),
                commands: plugin
                    .manifest
                    .commands
                    .iter()
                    .map(|command| PluginCommandSettingsDefinition {
                        id: command.id.clone(),
                        title: command.title.clone(),
                        action_words: command.action_words.clone(),
                        customizable_action_words: command.customizable_action_words,
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        definitions.sort_by_cached_key(|definition| definition.name.to_lowercase());
        definitions
    }

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
                let Ok(metadata) = fs::metadata(&manifest_path) else {
                    continue;
                };
                if metadata.len() > MAX_PLUGIN_MANIFEST_BYTES as u64 {
                    eprintln!(
                        "[plugin] ignoring oversized manifest {}",
                        manifest_path.display()
                    );
                    continue;
                }
                let Ok(file) = fs::File::open(&manifest_path) else {
                    continue;
                };
                let Ok(bytes) = crate::utils::read_bounded(file, MAX_PLUGIN_MANIFEST_BYTES) else {
                    continue;
                };
                let Ok(text) = String::from_utf8(bytes) else {
                    continue;
                };
                match serde_json::from_str::<PluginManifest>(&text) {
                    Ok(manifest) => {
                        let settings = merge_preference_defaults(
                            plugin_config_for(&manifest.id, plugin_configs),
                            &manifest,
                        );
                        let action_word_overrides = Arc::new(RwLock::new(
                            configured_action_word_overrides(&manifest, &settings),
                        ));
                        let status = Arc::new(Mutex::new(PluginRuntimeStatus {
                            state: if settings.enabled {
                                "ready".into()
                            } else {
                                "disabled".into()
                            },
                            last_method: None,
                            last_error: None,
                            failure_count: 0,
                            last_query_ms: 0,
                            last_query_results: 0,
                            query_truncations: 0,
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
                        // Cache handler availability during discovery. Checking
                        // the Lua VM on every selection can block the UI while
                        // a plugin query is making a network request.
                        let has_detail_handler = embedded
                            .as_ref()
                            .map(|plugin| {
                                let Ok(plugin) = plugin.lock() else {
                                    return false;
                                };
                                plugin.has_handler("detail").unwrap_or(false)
                            })
                            .unwrap_or(false);
                        plugins.push(InstalledPlugin {
                            root: dir,
                            manifest,
                            embedded,
                            has_detail_handler,
                            enabled: Arc::new(AtomicBool::new(settings.enabled)),
                            weight: Arc::new(AtomicI32::new(settings.weight)),
                            preferences: Arc::new(Mutex::new(settings.preferences)),
                            action_word_overrides,
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
            query_epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_locale(&self, locale: String) {
        if let Ok(mut current) = self.locale.lock() {
            *current = crate::utils::i18n::effective_language(&locale);
        }
    }

    fn begin_query(&self) -> u64 {
        self.query_epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn query_is_current(&self, epoch: u64) -> bool {
        self.query_epoch.load(Ordering::Acquire) == epoch
    }

    pub fn replace_config(
        &self,
        plugin_configs: &std::collections::BTreeMap<String, PluginConfig>,
    ) {
        for plugin in &self.plugins {
            let settings = merge_preference_defaults(
                plugin_config_for(&plugin.manifest.id, plugin_configs),
                &plugin.manifest,
            );
            let action_word_overrides =
                configured_action_word_overrides(&plugin.manifest, &settings);
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
            if let Ok(mut overrides) = plugin.action_word_overrides.write() {
                *overrides = action_word_overrides;
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
                        last_query_ms: 0,
                        last_query_results: 0,
                        query_truncations: 0,
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
                    last_query_ms: runtime.last_query_ms,
                    last_query_results: runtime.last_query_results,
                    query_truncations: runtime.query_truncations,
                }
            })
            .collect()
    }

    pub fn query(&self, query: &str) -> Vec<SearchResult> {
        self.query_with_engine_settings(query)
            .into_iter()
            .map(|candidate| candidate.item)
            .collect()
    }

    pub fn query_with_engine_settings(&self, query: &str) -> Vec<EngineSearchResult> {
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "system".into());
        let locale = locale.as_str();
        let epoch = self.begin_query();
        let plugins = self
            .plugins
            .iter()
            .filter(|plugin| {
                plugin.enabled.load(Ordering::Acquire)
                    && plugin.manifest.engine.participates_in_root
            })
            .collect::<Vec<_>>();
        if plugins.len() <= 1 {
            return plugins
                .into_iter()
                .flat_map(|plugin| self.query_root_plugin(plugin, query, locale, epoch))
                .collect();
        }

        // Each plugin owns an independent Lua VM. Query them concurrently so
        // one slow plugin cannot serialize every other root contribution, but
        // collect by the original manifest order to keep stable tie-breaking.
        let results = plugin_query_pool().install(|| {
            plugins
                .par_iter()
                .map(|plugin| self.query_root_plugin(plugin, query, locale, epoch))
                .collect::<Vec<_>>()
        });
        results.into_iter().flatten().collect()
    }

    fn query_root_plugin(
        &self,
        plugin: &InstalledPlugin,
        query: &str,
        locale: &str,
        epoch: u64,
    ) -> Vec<EngineSearchResult> {
        if !self.query_is_current(epoch) {
            return Vec::new();
        }
        let (plugin_query, command_id) = action_word_query(plugin, query);
        let started = Instant::now();
        let query_budget = if command_id.is_some() {
            SESSION_PLUGIN_QUERY_BUDGET
        } else {
            ROOT_PLUGIN_QUERY_BUDGET
        };
        match query_plugin(
            plugin,
            &plugin_query,
            locale,
            PluginQueryOptions {
                command_id: command_id.as_deref(),
                requested_query: query,
                session: None,
                search_history: &[],
                offset: 0,
                max_results: MAX_ROOT_PLUGIN_RESULTS,
                cancellation: Some(QueryCancellation::new(
                    &self.query_epoch,
                    epoch,
                    query_budget,
                )),
            },
        ) {
            Ok(items) => {
                record_query_metrics(
                    plugin,
                    started.elapsed(),
                    items.len(),
                    MAX_ROOT_PLUGIN_RESULTS,
                );
                mark_success(plugin, "query");
                if !self.query_is_current(epoch) {
                    return Vec::new();
                }
                items
                    .into_iter()
                    .map(|item| EngineSearchResult {
                        item,
                        settings: plugin.manifest.engine,
                    })
                    .collect()
            }
            Err(error) => {
                record_query_metrics(plugin, started.elapsed(), 0, MAX_ROOT_PLUGIN_RESULTS);
                if is_query_abort(&error) {
                    return Vec::new();
                }
                mark_failure(plugin, "query", &error);
                eprintln!("[plugin:{}] query failed: {error:#}", plugin.manifest.id);
                Vec::new()
            }
        }
    }

    pub fn owns_search_engine_session(&self, session: &SearchEngineSession) -> bool {
        self.plugin_command_for_session(session).is_some()
    }

    pub fn search_engine_settings_for_session(
        &self,
        session: &SearchEngineSession,
    ) -> SearchEngineSettings {
        self.plugin_command_for_session(session)
            .map(|(plugin, _)| plugin.manifest.engine)
            .unwrap_or_default()
    }

    /// Return the command shortcut for any result owned by a plugin. This is
    /// intentionally independent of the item id: all rows produced by a
    /// command belong to the same shortcut and should display the same hint.
    pub fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        let rest = result_id.strip_prefix("plugin:")?;
        let (plugin_id, _) = rest.split_once(':')?;
        let plugin = self.plugins.iter().find(|plugin| {
            plugin.enabled.load(Ordering::Acquire) && plugin.manifest.id == plugin_id
        })?;
        plugin
            .manifest
            .commands
            .iter()
            .flat_map(|command| command_action_words(plugin, command))
            .next()
            .map(|word| format!("{} ", word.trim()))
    }

    pub fn query_active_search_engine(
        &self,
        session: &SearchEngineSession,
        raw_query: &str,
    ) -> Vec<SearchResult> {
        self.query_active_search_engine_with_history(session, raw_query, &[])
    }

    pub fn query_active_search_engine_with_history(
        &self,
        session: &SearchEngineSession,
        raw_query: &str,
        search_history: &[crate::provider::SearchHistoryEntry],
    ) -> Vec<SearchResult> {
        let Some((plugin, command_id)) = self.plugin_command_for_session(session) else {
            return Vec::new();
        };
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "system".into());
        let epoch = self.begin_query();
        let started = Instant::now();
        match query_plugin(
            plugin,
            raw_query,
            &locale,
            PluginQueryOptions {
                command_id: Some(&command_id),
                requested_query: raw_query,
                session: Some(session),
                search_history,
                offset: 0,
                max_results: MAX_SESSION_PLUGIN_RESULTS,
                cancellation: Some(QueryCancellation::new(
                    &self.query_epoch,
                    epoch,
                    SESSION_PLUGIN_QUERY_BUDGET,
                )),
            },
        ) {
            Ok(items) => {
                record_query_metrics(
                    plugin,
                    started.elapsed(),
                    items.len(),
                    MAX_SESSION_PLUGIN_RESULTS,
                );
                if !self.query_is_current(epoch) {
                    return Vec::new();
                }
                mark_success(plugin, "query");
                items
            }
            Err(error) => {
                record_query_metrics(plugin, started.elapsed(), 0, MAX_SESSION_PLUGIN_RESULTS);
                if is_query_abort(&error) {
                    return Vec::new();
                }
                mark_failure(plugin, "query", &error);
                eprintln!(
                    "[plugin:{}] search engine query failed: {error:#}",
                    plugin.manifest.id
                );
                Vec::new()
            }
        }
    }

    pub fn query_active_search_engine_page(
        &self,
        session: &SearchEngineSession,
        raw_query: &str,
        offset: usize,
    ) -> Vec<SearchResult> {
        let Some((plugin, command_id)) = self.plugin_command_for_session(session) else {
            return Vec::new();
        };
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "system".into());
        let epoch = self.begin_query();
        let started = Instant::now();
        match query_plugin(
            plugin,
            raw_query,
            &locale,
            PluginQueryOptions {
                command_id: Some(&command_id),
                requested_query: raw_query,
                session: Some(session),
                search_history: &[],
                offset,
                max_results: MAX_SESSION_PLUGIN_RESULTS,
                cancellation: Some(QueryCancellation::new(
                    &self.query_epoch,
                    epoch,
                    SESSION_PLUGIN_QUERY_BUDGET,
                )),
            },
        ) {
            Ok(items) => {
                record_query_metrics(
                    plugin,
                    started.elapsed(),
                    items.len(),
                    MAX_SESSION_PLUGIN_RESULTS,
                );
                if !self.query_is_current(epoch) {
                    return Vec::new();
                }
                mark_success(plugin, "query");
                items
            }
            Err(error) => {
                record_query_metrics(plugin, started.elapsed(), 0, MAX_SESSION_PLUGIN_RESULTS);
                if is_query_abort(&error) {
                    return Vec::new();
                }
                mark_failure(plugin, "query", &error);
                eprintln!(
                    "[plugin:{}] search engine page failed: {error:#}",
                    plugin.manifest.id
                );
                Vec::new()
            }
        }
    }

    fn empty_state_for_session(
        &self,
        session: &SearchEngineSession,
        query: &str,
    ) -> Option<SearchResult> {
        let (plugin, command_id) = self.plugin_command_for_session(session)?;
        let embedded = plugin.embedded.as_ref()?;
        let embedded = embedded.lock().ok()?;
        if !embedded.has_handler("empty_state").ok()? {
            return None;
        }
        let request_id = next_request_id("empty-state");
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "system".into());
        let request = serde_json::json!({
            "method": "empty_state",
            "request_id": request_id,
            "api_version": plugin.manifest.api_version,
            "command_id": command_id,
            "query": query,
            "locale": locale,
            "fallback_locale": plugin.manifest.default_locale,
            "preferences": plugin_preferences(plugin),
            "session": session.state,
        });
        let output = match embedded.call("empty_state", &request, None, None) {
            Ok(output) => output,
            Err(error) => {
                eprintln!(
                    "[plugin:{}] empty-state callback failed: {error:#}",
                    plugin.manifest.id
                );
                return None;
            }
        };
        let response: EmptyStateResponse =
            match parse_plugin_response(&output, MAX_PLUGIN_SMALL_RESPONSE_BYTES) {
                Ok(response) => response,
                Err(error) => {
                    eprintln!(
                        "[plugin:{}] invalid empty-state response: {error}",
                        plugin.manifest.id
                    );
                    return None;
                }
            };
        if let Err(error) = validate_response_id(response.request_id.as_deref(), &request_id) {
            eprintln!(
                "[plugin:{}] invalid empty-state response: {error:#}",
                plugin.manifest.id
            );
            return None;
        }
        if let Some(error) = response.error {
            eprintln!(
                "[plugin:{}] empty-state callback error: {}",
                plugin.manifest.id, error.message
            );
            return None;
        }
        let title = limit_context_text(&response.title, MAX_PLUGIN_ITEM_TITLE_CHARS);
        let description = limit_context_text(&response.description, MAX_PLUGIN_ITEM_SUBTITLE_CHARS);
        Some(SearchResult {
            id: format!("plugin:{}:{}:empty-state", plugin.manifest.id, command_id),
            identity: format!("plugin:{}:{}:empty-state", plugin.manifest.id, command_id),
            title,
            category: ResultCategory::Other,
            aliases: Vec::new(),
            location: description.clone(),
            launch_target: PathBuf::new(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: format!("plugin:{}", plugin.manifest.id).into(),
            sources: vec![format!("plugin:{}", plugin.manifest.id).into()],
            launch_entries: Vec::new(),
            actions: Vec::new(),
            detail: response
                .detail
                .map(|detail| to_detail_view(&plugin.root, detail, &locale)),
            description: Some(description),
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: response
                .icon
                .and_then(|icon| resolve_plugin_icon(&plugin.root, icon)),
            ranking_bias: 0,
            allow_favorite: true,
            visibility: crate::result::ResultVisibility::Always,
        })
    }

    /// Materialize the command-owned result used for the active session's
    /// search-field icon. This must be resolved through the plugin manager so
    /// remote/resource command icons work exactly like ordinary plugin rows.
    pub fn search_engine_result(&self, session: &SearchEngineSession) -> Option<SearchResult> {
        let (plugin, command_id) = self.plugin_command_for_session(session)?;
        let command = plugin
            .manifest
            .commands
            .iter()
            .find(|command| command.id == command_id)?;
        let action_words = command_action_words(plugin, command);
        let icon = plugin
            .embedded
            .as_ref()
            .and_then(|embedded| embedded.lock().ok())
            .and_then(|embedded| {
                if !embedded.has_handler("search_engine_icon").ok()? {
                    return None;
                }
                let request_id = next_request_id("search-engine-icon");
                let request = serde_json::json!({
                    "method": "search_engine_icon",
                    "request_id": request_id,
                    "command_id": command_id,
                    "locale": self.locale.lock().ok().map(|locale| locale.clone()).unwrap_or_else(|| "system".into()),
                    "fallback_locale": plugin.manifest.default_locale,
                    "preferences": plugin_preferences(plugin),
                });
                let output = embedded.call("search_engine_icon", &request, None, None).ok()?;
                let response: SearchEngineIconResponse =
                    parse_plugin_response(&output, MAX_PLUGIN_SMALL_RESPONSE_BYTES).ok()?;
                validate_response_id(response.request_id.as_deref(), &request_id).ok()?;
                if response.error.is_some() {
                    return None;
                }
                response.icon.and_then(|icon| resolve_plugin_icon(&plugin.root, icon))
            })
            .or_else(|| {
                command
                    .icon
                    .clone()
                    .and_then(|icon| resolve_plugin_icon(&plugin.root, icon))
            });
        Some(SearchResult {
            id: session.id.clone(),
            identity: session.id.clone(),
            title: command.title.clone(),
            category: ResultCategory::Other,
            aliases: action_words.clone(),
            location: plugin.manifest.name.clone(),
            launch_target: PathBuf::new(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: format!("plugin:{}", plugin.manifest.id).into(),
            sources: vec![format!("plugin:{}", plugin.manifest.id).into()],
            launch_entries: Vec::new(),
            actions: vec![Action::EnterSearchEngine {
                prefix: action_words
                    .first()
                    .map(|word| format!("{} ", word.trim()))
                    .unwrap_or_default(),
            }],
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon,
            ranking_bias: -1000,
            allow_favorite: true,
            visibility: crate::result::ResultVisibility::Always,
        })
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

    fn plugin_for_item<'a>(&'a self, item_id: &str) -> Option<&'a InstalledPlugin> {
        let rest = item_id.strip_prefix("plugin:")?;
        let (plugin_id, _) = rest.split_once(':')?;
        self.plugins.iter().find(|plugin| {
            plugin.enabled.load(Ordering::Acquire) && plugin.manifest.id == plugin_id
        })
    }

    pub fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "en".into());
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
                let word = query.split_whitespace().next()?;
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
                let title = crate::utils::i18n::tr_owned_in_context(&locale, "Launcher", &title);
                let placeholder = command
                    .placeholder
                    .as_deref()
                    .filter(|placeholder| !placeholder.trim().is_empty())
                    .map(|placeholder| {
                        crate::utils::i18n::tr_owned_in_context(&locale, "Launcher", placeholder)
                    })
                    .unwrap_or_else(|| title.clone());
                Some(SearchEngineSession {
                    id: format!("plugin:{}:{}", plugin.manifest.id, command.id),
                    placeholder,
                    title,
                    prefix: format!("{word} "),
                    argument: argument.into(),
                    mode: command
                        .mode
                        .as_deref()
                        .map(SearchEngineMode::from_config)
                        .unwrap_or(SearchEngineMode::Search),
                    state: Default::default(),
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
            bound_execute_response(&output)
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

impl crate::provider::SearchEngineProvider for PluginManager {
    fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        PluginManager::search_engine(self, query)
    }

    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        PluginManager::query(self, query)
    }

    fn query_root_with_settings(&self, query: &str) -> Vec<EngineSearchResult> {
        self.query_with_engine_settings(query)
    }

    fn empty_state(
        &self,
        context: &crate::page::SearchContext,
        query: &str,
    ) -> Option<SearchResult> {
        let session = context.session()?;
        self.empty_state_for_session(session, query)
    }

    fn query_active(&self, session: &SearchEngineSession, query: &str) -> Vec<SearchResult> {
        PluginManager::query_active_search_engine(self, session, query)
    }

    fn query_active_with_history(
        &self,
        session: &SearchEngineSession,
        query: &str,
        history: &[crate::provider::SearchHistoryEntry],
    ) -> Vec<SearchResult> {
        PluginManager::query_active_search_engine_with_history(self, session, query, history)
    }

    fn query_active_page(
        &self,
        session: &SearchEngineSession,
        query: &str,
        offset: usize,
    ) -> Vec<SearchResult> {
        PluginManager::query_active_search_engine_page(self, session, query, offset)
    }

    fn owns_session(&self, session: &SearchEngineSession) -> bool {
        PluginManager::owns_search_engine_session(self, session)
    }

    fn settings_for_session(&self, session: &SearchEngineSession) -> SearchEngineSettings {
        PluginManager::search_engine_settings_for_session(self, session)
    }

    fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        PluginManager::shortcut_hint(self, result_id)
    }

    fn search_engine_result(&self, session: &SearchEngineSession) -> Option<SearchResult> {
        PluginManager::search_engine_result(self, session)
    }
}

impl crate::details::DynamicDetailProvider for PluginManager {
    fn matches(&self, item: &SearchResult) -> bool {
        self.plugin_for_item(&item.id)
            .is_some_and(|plugin| plugin.has_detail_handler)
    }

    fn cached(&self, _item: &SearchResult, _language: &str) -> Option<crate::result::DetailView> {
        None
    }

    fn load(&self, item: &SearchResult, language: &str) -> Result<crate::result::DetailView> {
        let plugin = self
            .plugin_for_item(&item.id)
            .context("plugin detail owner not found")?;
        let request_id = next_request_id("detail");
        let request = serde_json::json!({
            "method": "detail",
            "request_id": request_id,
            "item_id": item.id,
            "api_version": plugin.manifest.api_version,
            "locale": language,
            "fallback_locale": plugin.manifest.default_locale,
            "preferences": plugin_preferences(plugin),
        });
        let embedded = plugin
            .embedded
            .as_ref()
            .context("plugin is not initialized")?;
        let embedded = embedded
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin VM lock poisoned"))?;
        let output = embedded.call("detail", &request, Some(&item.id), None)?;
        let response: DetailResponse = parse_plugin_response(&output, MAX_PLUGIN_RESPONSE_BYTES)
            .context("decode plugin detail response")?;
        if let Some(error) = response.error {
            anyhow::bail!("{}: {}", error.code, error.message);
        }
        if response.request_id.as_deref() != Some(request_id.as_str()) {
            anyhow::bail!("plugin detail response request id mismatch");
        }
        let detail = response
            .detail
            .context("plugin detail response is missing detail")?;
        Ok(to_detail_view(&plugin.root, detail, language))
    }

    fn error_view(&self, _item: &SearchResult, language: &str) -> crate::result::DetailView {
        crate::result::DetailView {
            title: crate::utils::i18n::tr(language, "Preview unavailable").into(),
            subtitle: String::new(),
            source: String::new(),
            blocks: vec![crate::result::DetailBlock::Error {
                message: crate::utils::i18n::tr(language, "Preview unavailable").into(),
                retry_action: None,
            }],
            actions: Vec::new(),
            preferred_width: None,
            scrollable: false,
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

fn record_query_metrics(
    plugin: &InstalledPlugin,
    elapsed: Duration,
    result_count: usize,
    max_results: usize,
) {
    if let Ok(mut status) = plugin.status.lock() {
        status.last_query_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
        status.last_query_results = result_count;
        if result_count >= max_results {
            status.query_truncations = status.query_truncations.saturating_add(1);
        }
    }
}

fn is_query_abort(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains(QUERY_CANCELLED) || message.contains(QUERY_TIMED_OUT)
}

fn action_word_query(plugin: &InstalledPlugin, query: &str) -> (String, Option<String>) {
    let mut parts = query.trim_start().splitn(2, char::is_whitespace);
    let Some(word) = parts.next() else {
        return (query.to_owned(), None);
    };
    let argument = parts.next();
    let Some(command) = plugin.manifest.commands.iter().find(|command| {
        command_action_words(plugin, command)
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
    options: PluginQueryOptions<'_>,
) -> Result<Vec<SearchResult>> {
    let request_id = next_request_id("query");
    let request = serde_json::json!({
        "method": "query",
        "request_id": request_id,
        "query": query,
        "command_id": options.command_id,
        "api_version": plugin.manifest.api_version,
        "locale": locale,
        "fallback_locale": plugin.manifest.default_locale,
        "preferences": plugin_preferences(plugin),
        "session": options.session.map(|session| &session.state),
        "search_history": options.search_history,
        "offset": options.offset,
        "limit": options.max_results,
    });
    let embedded = plugin
        .embedded
        .as_ref()
        .context("plugin is not initialized")?;
    // A stale shortcut request may still be waiting on network I/O while the
    // user has already returned to ordinary search. Do not make that ordinary
    // search wait for the plugin VM; the plugin is optional for this query.
    let embedded = if options.command_id.is_none() {
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
    let output = embedded.call_query(&request, options.cancellation)?;
    let response: QueryResponse = parse_plugin_response(&output, MAX_PLUGIN_RESPONSE_BYTES)
        .context("decode plugin response")?;
    validate_query_response(&response, &request_id)?;
    Ok(response
        .items
        .into_iter()
        .filter(|item| {
            item.visible_when != PluginItemVisibility::EmptyQuery || query.trim().is_empty()
        })
        .take(options.max_results)
        .map(|item| {
            to_result(
                plugin,
                item,
                options.command_id,
                options.requested_query,
                locale,
            )
        })
        .collect())
}

fn next_request_id(method: &str) -> String {
    format!(
        "{method}-{}",
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn plugin_query_pool() -> &'static rayon::ThreadPool {
    PLUGIN_QUERY_POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(
                std::thread::available_parallelism()
                    .map(|count| count.get())
                    .unwrap_or(4)
                    .clamp(1, 8),
            )
            .thread_name(|index| format!("wowie-plugin-{index}"))
            .build()
            .expect("plugin query thread pool must initialize")
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    thread_local! {
        static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    #[global_allocator]
    static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let _ = COUNT_ALLOCATIONS.try_with(|enabled| {
                if enabled.get() {
                    let _ = ALLOCATION_COUNT.try_with(|count| count.set(count.get() + 1));
                }
            });
            System.alloc(layout)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout);
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let _ = COUNT_ALLOCATIONS.try_with(|enabled| {
                if enabled.get() {
                    let _ = ALLOCATION_COUNT.try_with(|count| count.set(count.get() + 1));
                }
            });
            System.realloc(ptr, layout, new_size)
        }
    }

    fn measured_call(function: &Function, query: &str) -> (usize, usize, Duration) {
        ALLOCATION_COUNT.with(|count| count.set(0));
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
        let start = Instant::now();
        let result = function.call::<usize>(query).unwrap();
        let elapsed = start.elapsed();
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        let allocations = ALLOCATION_COUNT.with(Cell::get);
        (result, allocations, elapsed)
    }

    #[test]
    #[ignore = "manual performance benchmark; run with --ignored --nocapture"]
    fn fuzzy_filter_benchmark() {
        let plugin =
            EmbeddedPlugin::from_source("", "bench.lua", "en", "en", &BTreeMap::new(), &[], None)
                .unwrap();
        plugin
            .lua
            .load(
                r#"
            local candidates = {}
            for index = 1, 1000 do
              candidates[index] = {"Example Game " .. index, tostring(index)}
            end
            function fuzzy_old(query)
              local count = 0
              for _, fields in ipairs(candidates) do
                if wowie.fuzzy_match(fields[1], query)
                    or wowie.fuzzy_match(fields[2], query) then count = count + 1 end
              end
              return count
            end
            function fuzzy_batch(query)
              return #wowie.fuzzy_filter(candidates, query)
            end
        "#,
            )
            .exec()
            .unwrap();
        let old: Function = plugin.lua.globals().get("fuzzy_old").unwrap();
        let batch: Function = plugin.lua.globals().get("fuzzy_batch").unwrap();
        let queries: Vec<_> = (0..100).map(|index| format!("gm {index}")).collect();
        measured_call(&old, "game");
        measured_call(&batch, "game");
        let ((old_allocations, batch_allocations, mut old_times, mut batch_times), peak_threads) =
            crate::perf_support::measure_peak_threads(|| {
                let mut old_allocations = 0;
                let mut batch_allocations = 0;
                let mut old_times = Vec::with_capacity(queries.len());
                let mut batch_times = Vec::with_capacity(queries.len());
                for query in &queries {
                    let (old_count, allocations, elapsed) = measured_call(&old, query);
                    old_allocations += allocations;
                    old_times.push(elapsed);
                    let (batch_count, allocations, elapsed) = measured_call(&batch, query);
                    batch_allocations += allocations;
                    batch_times.push(elapsed);
                    assert_eq!(old_count, batch_count);
                }
                (old_allocations, batch_allocations, old_times, batch_times)
            });
        let (old_p50, old_p95) = crate::perf_support::percentiles(&mut old_times);
        let (batch_p50, batch_p95) = crate::perf_support::percentiles(&mut batch_times);
        let stats = crate::utils::background::stats();
        println!(
            "[perf] plugin_batch candidates=1000 queries=100 single_allocations_per_query={} single_p50_us={} single_p95_us={} batch_allocations_per_query={} batch_p50_us={} batch_p95_us={} process_peak_threads={} pending_tasks={} pending_peak={}",
            old_allocations / queries.len(),
            old_p50.as_micros(),
            old_p95.as_micros(),
            batch_allocations / queries.len(),
            batch_p50.as_micros(),
            batch_p95.as_micros(),
            peak_threads,
            stats.pending,
            stats.peak_pending,
        );
    }

    #[test]
    fn fuzzy_filter_matches_single_api_and_returns_candidate_indices() {
        let plugin =
            EmbeddedPlugin::from_source("", "test.lua", "en", "en", &BTreeMap::new(), &[], None)
                .unwrap();
        plugin
            .lua
            .load(
                r#"
            local candidates = {
              {"Calculator", "calc"},
              {"Calendar", "schedule"},
              {"翻译", "translate"},
              "Call center"
            }
            local function verify(query)
              local expected = {}
              for index, candidate in ipairs(candidates) do
                local fields = type(candidate) == "table" and candidate or {candidate}
                for _, text in ipairs(fields) do
                  if wowie.fuzzy_match(text, query) then
                    expected[#expected + 1] = index
                    break
                  end
                end
              end
              local actual = wowie.fuzzy_filter(candidates, query)
              assert(#actual == #expected)
              for index, value in ipairs(actual) do assert(value == expected[index]) end
            end
            verify("calc")
            verify("clc")
            verify("翻")
            verify("  ")
            assert(not pcall(wowie.fuzzy_filter, {{string.rep("x", 4097)}}, "x"))
        "#,
            )
            .exec()
            .unwrap();
    }

    #[test]
    fn steam_cache_indexes_icons_refreshes_and_keeps_last_good_snapshot() {
        let lua = Lua::new();
        lua.load(
            r#"
            callbacks, directories, files, scans = {}, {}, {}, {}
            now, fail_scan, stored = 100000, false, nil
            local root = [=[S:\Steam]=]
            local apps = root .. [=[\steamapps]=]
            local icons = root .. [=[\appcache\librarycache]=]
            directories[apps] = {
              {name="appmanifest_1.acf", path=apps .. [=[\appmanifest_1.acf]=]},
              {name="appmanifest_2.acf", path=apps .. [=[\appmanifest_2.acf]=]}
            }
            files[apps .. [=[\appmanifest_1.acf]=]] = '"name" "First Game" "installdir" "First"'
            files[apps .. [=[\appmanifest_2.acf]=]] = '"name" "Second Game" "installdir" "Second"'
            directories[icons] = {
              {name="1_icon.jpg", path=icons .. [=[\1_icon.jpg]=], is_dir=false},
              {name="2", path=icons .. [=[\2]=], is_dir=true}
            }
            directories[icons .. [=[\2]=]] = {
              {name="abc123.jpg", path=icons .. [=[\2\abc123.jpg]=], is_dir=false}
            }
            wowie = {
              on = function(name, callback) callbacks[name] = callback end,
              env = function(name) if name == "PROGRAMFILES(X86)" then return [=[S:]=] end end,
              fs_list = function(path)
                if fail_scan then error("offline") end
                scans[path] = (scans[path] or 0) + 1
                return directories[path] or {}
              end,
              fs_read_text = function(path) return files[path] end,
              storage_read = function(_) return stored end,
              storage_write = function(_, value) stored = value end,
              fuzzy_match = function(text, query) return string.find(string.lower(text), query, 1, true) ~= nil end,
              fuzzy_filter = function(candidates, query)
                local matches = {}
                for index, fields in ipairs(candidates) do
                  for _, text in ipairs(fields) do
                    if string.find(string.lower(text), query, 1, true) then
                      matches[#matches + 1] = index
                      break
                    end
                  end
                end
                return matches
              end,
              result = function(item) return item end
            }
            os.time = function() return now end
            "#,
        )
        .exec()
        .unwrap();
        lua.load(include_str!("../plugins/steam-games/main.lua"))
            .exec()
            .unwrap();
        lua.load(
            r#"
            callbacks.initialize({preferences={refresh_minutes="1"}})
            local icons = [=[S:\Steam\appcache\librarycache]=]
            assert(scans[icons] == 1)
            local first = callbacks.query("game", {request_id=1})
            assert(#first.items == 2 and first.items[1].icon.type == "file")
            assert(first.items[2].icon.path == icons .. [=[\2\abc123.jpg]=])
            assert(scans[icons] == 1)
            now = now + 61
            local apps = [=[S:\Steam\steamapps]=]
            directories[apps] = {{name="appmanifest_1.acf", path=apps .. [=[\appmanifest_1.acf]=]}}
            directories[icons][1] = {name="1_icon.png", path=icons .. [=[\1_icon.png]=], is_dir=false}
            local second = callbacks.query("game", {request_id=2})
            assert(#second.items == 1 and scans[icons] == 2)
            assert(second.items[1].icon.path == icons .. [=[\1_icon.png]=])
            now = now + 61
            fail_scan = true
            local fallback = callbacks.query("game", {request_id=3})
            assert(#fallback.items == 1 and fallback.items[1].title == "First Game")
            "#,
        )
        .exec()
        .unwrap();
    }

    #[test]
    fn plugin_file_apis_preserve_missing_utf8_and_storage_behavior() {
        let root = std::env::temp_dir().join(format!(
            "wowie-plugin-files-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let plugin = EmbeddedPlugin::from_source(
            "",
            "test.lua",
            "en",
            "en",
            &BTreeMap::new(),
            &["filesystem".into()],
            Some(root.clone()),
        )
        .unwrap();
        plugin
            .lua
            .globals()
            .set("test_root", root.to_string_lossy().replace('\\', "/"))
            .unwrap();
        fs::write(root.join("valid.txt"), "hello").unwrap();
        fs::write(root.join("invalid.txt"), [0xff]).unwrap();
        let file = fs::File::create(root.join("large.txt")).unwrap();
        file.set_len(4 * 1024 * 1024 + 1).unwrap();
        drop(file);
        plugin
            .lua
            .load(
                r#"
            assert(wowie.storage_read("missing") == nil)
            wowie.storage_write("cache", {version = 1, title = "hello"})
            assert(wowie.storage_read("cache").title == "hello")
            assert(wowie.fs_read_text(test_root .. "/missing.txt") == nil)
            assert(wowie.fs_read_text(test_root .. "/valid.txt") == "hello")
            assert(not pcall(wowie.fs_read_text, test_root .. "/invalid.txt"))
            assert(not pcall(wowie.fs_read_text, test_root .. "/large.txt"))
            assert(not pcall(wowie.storage_read, "../cache"))
        "#,
            )
            .exec()
            .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_defaults_are_valid() {
        let m: PluginManifest =
            serde_json::from_str(r#"{"id":"x","name":"X","entry_point":"main.lua"}"#).unwrap();
        assert_eq!(m.api_version, 1);
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.engine.history_limit, 0);
        assert_eq!(m.engine.search_debounce_ms, 30);
    }

    #[test]
    fn manifest_engine_can_opt_into_persistent_search_history() {
        let manifest: PluginManifest = serde_json::from_str(
            r#"{"id":"x","name":"X","entry_point":"main.lua","engine":{"history_limit":12,"search_debounce_ms":400}}"#,
        )
        .unwrap();
        assert_eq!(manifest.engine.history_limit, 12);
        assert_eq!(manifest.engine.search_debounce_ms, 400);
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
                    "icon":{"type":"resource","path":"icons/github.png"}
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(m.commands[0].id, "search");
        assert_eq!(m.commands[0].action_words, vec!["gh"]);
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
                    placeholder: None,
                    keywords: vec!["gh".into()],
                    action_words: vec!["gh".into()],
                    customizable_action_words: true,
                    mode: None,
                    require_separator: false,
                    icon: None,
                }],
                preferences: Vec::new(),
                engine: SearchEngineSettings::default(),
                permissions: Vec::new(),
                default_locale: "en".into(),
            },
            embedded: None,
            has_detail_handler: false,
            enabled: Arc::new(AtomicBool::new(true)),
            weight: Arc::new(AtomicI32::new(0)),
            preferences: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            action_word_overrides: Arc::new(RwLock::new(BTreeMap::new())),
            status: Arc::new(Mutex::new(PluginRuntimeStatus {
                state: "ready".into(),
                last_method: None,
                last_error: None,
                failure_count: 0,
                last_query_ms: 0,
                last_query_results: 0,
                query_truncations: 0,
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
    fn plugin_items_can_be_limited_to_an_empty_query() {
        let response: QueryResponse = serde_json::from_str(
            r#"{"items":[{"id":"menu","title":"Menu","visible_when":"empty_query"}]}"#,
        )
        .unwrap();
        assert_eq!(
            response.items[0].visible_when,
            PluginItemVisibility::EmptyQuery
        );
    }

    #[test]
    fn plugin_items_support_independent_context_menu_actions() {
        let response: QueryResponse = serde_json::from_str(
            r#"{
                "items":[{
                    "id":"repository",
                    "title":"Wowie",
                    "context_actions":[{
                        "id":"copy-clone-url",
                        "title":"Copy clone URL",
                        "subtitle":"Copy the HTTPS clone address",
                        "icon":{"type":"named","id":"content-copy"}
                    }]
                }]
            }"#,
        )
        .unwrap();
        let action = &response.items[0].context_actions[0];

        assert_eq!(action.id, "copy-clone-url");
        assert_eq!(action.title, "Copy clone URL");
        assert_eq!(action.subtitle, "Copy the HTTPS clone address");
        assert!(matches!(action.icon, Some(IconRef::Named { .. })));
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
                            subtitle = context.preferences.token,
                            context_actions = {
                                {
                                    id = "copy-token",
                                    title = "Copy token",
                                    subtitle = "Copy the configured token"
                                }
                            }
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
        let plugin = EmbeddedPlugin::from_source(
            source,
            "test.lua",
            "zh-Hans",
            "en",
            &preferences,
            &[],
            None,
        )
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
        assert_eq!(response.items[0].context_actions[0].id, "copy-token");
        assert_eq!(response.items[0].context_actions[0].title, "Copy token");

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
            EmbeddedPlugin::from_source(source, "test.lua", "en", "en", &preferences, &[], None)
                .unwrap();
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
    fn embedded_lua_query_hook_cancels_stale_work() {
        let source = r#"
            wowie.on("query", function(query, context)
                while true do end
            end)
        "#;
        let plugin = EmbeddedPlugin::from_source(
            source,
            "test.lua",
            "en",
            "en",
            &BTreeMap::new(),
            &[],
            None,
        )
        .unwrap();
        let epoch = Arc::new(AtomicU64::new(0));
        let error = plugin
            .call_query(
                &serde_json::json!({"query": "stale"}),
                Some(QueryCancellation::new(&epoch, 1, Duration::from_secs(1))),
            )
            .unwrap_err();
        assert!(error.to_string().contains(QUERY_CANCELLED));
    }

    #[test]
    fn plugin_discovery_reads_project_plugins() {
        let manager = PluginManager::discover_with_config("en", &Default::default());
        assert!(manager
            .query("")
            .iter()
            .any(|item| item.id == "plugin:google-translate:entry"
                && item.visibility == crate::result::ResultVisibility::EmptyQueryOnly));
        assert!(manager
            .query("")
            .iter()
            .any(|item| item.id == "plugin:everything:entry"
                && item.visibility == crate::result::ResultVisibility::EmptyQueryOnly));

        let everything_session = manager.search_engine("e ").unwrap();
        assert!(manager
            .search_engine_result(&everything_session)
            .is_some_and(|item| item.icon.is_some()));
        assert!(manager
            .query_active_search_engine(&everything_session, "")
            .is_empty());
        let empty_state = crate::provider::SearchEngineProvider::empty_state(
            &manager,
            &crate::page::SearchContext::Engine(everything_session),
            "",
        )
        .expect("Everything should provide its custom empty page");
        assert!(empty_state.detail.is_none());
        assert!(empty_state
            .description
            .unwrap_or_default()
            .contains("Everything syntax"));

        assert!(manager.search_engine("eng").is_none());
        let translation_session = manager.search_engine("tmt ").unwrap();
        assert_eq!(
            crate::provider::SearchEngineProvider::search_debounce(&manager, &translation_session,),
            Duration::from_millis(400)
        );
        assert!(manager
            .query("tmt ")
            .iter()
            .any(|item| item.id == "plugin:google-translate:source"));
        let translation_items = manager
            .query("tmt hello")
            .into_iter()
            .filter(|item| item.id.starts_with("plugin:google-translate:"))
            .collect::<Vec<_>>();
        assert!(translation_items.len() >= 3);
        assert!(translation_items
            .iter()
            .any(|item| item.id.ends_with(":source")));
        assert!(translation_items
            .iter()
            .any(|item| item.id.ends_with(":target")));
        assert!(manager
            .statuses()
            .iter()
            .any(|status| status.id == "google-translate"));
        assert_eq!(
            manager.shortcut_hint("plugin:google-translate:entry"),
            Some("gt ".into())
        );
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
                copyable: false,
            }
        );
    }
}
