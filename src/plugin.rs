//! Embedded Lua plugin support.
//!
//! Plugins execute inside Wowie.  The host owns the Lua VM and exposes a small
//! data-oriented API, so plugin execution does not depend on a user-installed
//! runtime and never creates a plugin child process.

use crate::{
    config::PluginConfig,
    result::{Action, IconRef, IconSpec, LaunchEntry, ResultCategory, SearchResult},
};
use anyhow::{Context, Result};
use mlua::{Function, Lua, LuaSerdeExt, Value};
use serde::Deserialize;
use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        Arc, Mutex,
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
        )
    }

    fn from_source(
        source: &str,
        chunk_name: &str,
        locale: &str,
        fallback_locale: &str,
        preferences: &std::collections::BTreeMap<String, serde_json::Value>,
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

#[derive(Debug, Deserialize)]
struct QueryResponse {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    items: Vec<PluginItem>,
    #[serde(default)]
    error: Option<PluginError>,
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
    height: Option<PluginDetailHeight>,
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
struct PluginDetailHeight {
    min: i32,
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
    Separator,
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
                match query_plugin(plugin, &plugin_query, &locale, command_id.as_deref()) {
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
    let Some(command) = plugin.manifest.commands.iter().find(|command| {
        command
            .action_words
            .iter()
            .any(|action_word| action_word.eq_ignore_ascii_case(word))
    }) else {
        return (query.to_owned(), None);
    };
    (
        parts.next().unwrap_or_default().trim_start().to_owned(),
        Some(command.id.clone()),
    )
}

fn query_plugin(
    plugin: &InstalledPlugin,
    query: &str,
    locale: &str,
    command_id: Option<&str>,
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
    let embedded = embedded
        .lock()
        .map_err(|_| anyhow::anyhow!("plugin VM lock poisoned"))?;
    let output = embedded.call("query", &request, None, None)?;
    let response: QueryResponse =
        serde_json::from_str(&output).context("decode plugin response")?;
    validate_query_response(&response, &request_id)?;
    Ok(response
        .items
        .into_iter()
        .map(|item| to_result(plugin, item, command_id))
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
        category: ResultCategory::Application,
        aliases: item
            .aliases
            .into_iter()
            .chain(plugin.manifest.keywords.clone())
            .chain(command_keywords)
            .collect(),
        location: item.subtitle,
        launch_target: target,
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: format!("plugin:{}", plugin.manifest.id).into(),
        sources: vec![format!("plugin:{}", plugin.manifest.id).into()],
        launch_entries: Vec::<LaunchEntry>::new(),
        actions,
        detail: item.detail.map(|view| to_detail_view(&plugin.root, view)),
        icon: icon.and_then(|icon| resolve_plugin_icon(&plugin.root, icon)),
        ranking_bias: plugin.weight.load(Ordering::Acquire),
    }
}

fn resolve_plugin_icon(root: &Path, icon: IconRef) -> Option<IconSpec> {
    match icon {
        IconRef::Resource { path } => plugin_resource_path(root, &path).map(IconSpec::Resource),
        IconRef::System { target } => Some(IconSpec::System(target.into())),
        IconRef::Named { id } => Some(IconSpec::Named(id)),
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

fn to_detail_view(root: &Path, view: PluginDetailView) -> crate::result::DetailView {
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
                PluginDetailBlock::Separator => crate::result::DetailBlock::Separator,
            })
            .collect(),
        preferred_width: view.width.map(|width| {
            width
                .preferred
                .clamp(width.min.min(width.max), width.max.max(width.min))
        }),
        minimum_height: view.height.map(|height| height.min.max(0)),
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
            EmbeddedPlugin::from_source(source, "test.lua", "zh-Hans", "en", &preferences).unwrap();
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
    fn bundled_devtools_plugin_exercises_public_plugin_surface() {
        let manager = PluginManager::discover_with_config("zh-Hans", &Default::default());
        let results = manager.query("dev");
        assert!(results
            .iter()
            .any(|result| result.id == "plugin:wowie-devtools:developer-guide"));
        let guide = results
            .iter()
            .find(|result| result.id == "plugin:wowie-devtools:developer-guide")
            .unwrap();
        assert_eq!(guide.title, "插件开发指南");
        assert!(guide.detail.is_some());
        assert!(!guide.actions.is_empty());
        assert!(matches!(guide.icon, Some(IconSpec::Resource(_))));
        assert!(matches!(
            guide.actions[0],
            Action::Plugin {
                icon: Some(IconSpec::Named(_)),
                ..
            }
        ));
        assert!(guide
            .detail
            .as_ref()
            .unwrap()
            .actions
            .iter()
            .all(|action| action.icon.is_some()));
        let output = manager
            .execute("wowie-devtools", "developer-guide", "open")
            .unwrap();
        assert!(output.contains("open"));
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
                ,"height":{"min":280}
            }"#,
        )
        .unwrap();
        let detail = to_detail_view(Path::new("."), view);
        assert_eq!(detail.title, "Repository");
        assert_eq!(detail.blocks.len(), 3);
        assert_eq!(detail.actions[0].title, "Open");
        assert_eq!(detail.minimum_height, Some(280));
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
