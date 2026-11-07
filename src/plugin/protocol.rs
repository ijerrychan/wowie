use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct QueryResponse {
    #[serde(default)]
    pub(super) request_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_plugin_items")]
    pub(super) items: Vec<PluginItem>,
    #[serde(default)]
    pub(super) error: Option<PluginError>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DetailResponse {
    #[serde(default)]
    pub(super) request_id: Option<String>,
    #[serde(default)]
    pub(super) detail: Option<PluginDetailView>,
    #[serde(default)]
    pub(super) error: Option<PluginError>,
}

#[derive(Debug, Deserialize)]
pub(super) struct EmptyStateResponse {
    #[serde(default)]
    pub(super) request_id: Option<String>,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default)]
    pub(super) icon: Option<IconRef>,
    #[serde(default)]
    pub(super) detail: Option<PluginDetailView>,
    #[serde(default)]
    pub(super) error: Option<PluginError>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SearchEngineIconResponse {
    #[serde(default)]
    pub(super) request_id: Option<String>,
    #[serde(default)]
    pub(super) icon: Option<IconRef>,
    #[serde(default)]
    pub(super) error: Option<PluginError>,
}

pub(super) fn deserialize_plugin_items<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<PluginItem>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(items) => {
            if items.len() > MAX_PLUGIN_ITEMS {
                return Err(de::Error::custom(format!(
                    "plugin returned too many items ({}; limit {})",
                    items.len(),
                    MAX_PLUGIN_ITEMS
                )));
            }
            items
                .into_iter()
                .map(serde_json::from_value)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(de::Error::custom)
        }
        // Lua has no distinct empty-array type. An empty table is encoded as
        // an object by mlua, so accept it as the protocol's empty item list.
        serde_json::Value::Object(items) if items.is_empty() => Ok(Vec::new()),
        other => Err(de::Error::custom(format!(
            "expected plugin items array, got {}",
            json_type_name(&other)
        ))),
    }
}

pub(super) fn deserialize_plugin_string_list<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(serde_json::from_value)
            .collect::<std::result::Result<Vec<String>, _>>()
            .map_err(de::Error::custom),
        // Lua's empty table has no array/object distinction and mlua encodes
        // it as an object. Accept that representation only when it is empty;
        // non-empty objects remain invalid list data.
        serde_json::Value::Object(values) if values.is_empty() => Ok(Vec::new()),
        other => Err(de::Error::custom(format!(
            "expected plugin string list array, got {}",
            json_type_name(&other)
        ))),
    }
}

pub(super) fn json_type_name(value: &serde_json::Value) -> &'static str {
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
pub(super) struct PluginItem {
    pub(super) id: String,
    pub(super) title: String,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) subtitle: String,
    #[serde(default, deserialize_with = "deserialize_plugin_string_list")]
    pub(super) aliases: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_plugin_string_list")]
    pub(super) keywords: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_plugin_string_list")]
    pub(super) context: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_plugin_string_list")]
    pub(super) technical: Vec<String>,
    #[serde(default = "default_optional_string")]
    pub(super) target: Option<String>,
    #[serde(default)]
    pub(super) actions: Vec<PluginAction>,
    #[serde(default)]
    pub(super) context_actions: Vec<PluginContextAction>,
    #[serde(default)]
    pub(super) icon: Option<IconRef>,
    #[serde(default)]
    pub(super) detail: Option<PluginDetailView>,
    #[serde(default)]
    pub(super) command_id: Option<String>,
    /// Enter the command's search-engine session instead of executing a
    /// plugin callback. This lets a plugin expose a normal custom result in
    /// the default search surface.
    #[serde(default)]
    pub(super) session_entry: bool,
    #[serde(default)]
    pub(super) session_updates: BTreeMap<String, String>,
    /// Optional favorite policy. Session menu rows default to non-favoritable.
    #[serde(default)]
    pub(super) allow_favorite: Option<bool>,
    #[serde(default)]
    pub(super) ranking_bias: i32,
    /// Declarative visibility for session menus and introductory rows.
    #[serde(default)]
    pub(super) visible_when: PluginItemVisibility,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum PluginItemVisibility {
    #[default]
    Always,
    EmptyQuery,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct PluginDetailView {
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) subtitle: String,
    #[serde(default)]
    pub(super) source: String,
    #[serde(default)]
    pub(super) blocks: Vec<PluginDetailBlock>,
    #[serde(default)]
    pub(super) actions: Vec<PluginDetailAction>,
    #[serde(default)]
    pub(super) width: Option<PluginDetailWidth>,
    #[serde(default)]
    pub(super) scrollable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct PluginDetailWidth {
    pub(super) preferred: i32,
    #[serde(default = "default_detail_min_width")]
    pub(super) min: i32,
    #[serde(default = "default_detail_max_width")]
    pub(super) max: i32,
}

pub(super) fn default_detail_min_width() -> i32 {
    320
}

pub(super) fn default_detail_max_width() -> i32 {
    640
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct PluginDetailAction {
    pub(super) id: String,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) icon: Option<IconRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum PluginDetailBlock {
    Text {
        text: String,
        #[serde(default = "default_detail_color")]
        color: String,
        #[serde(default)]
        copyable: bool,
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
        #[serde(default)]
        copyable: bool,
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

pub(super) fn default_loading_message() -> String {
    "Loading preview".into()
}

pub(super) fn default_detail_color() -> String {
    "secondary".into()
}

pub(super) fn default_optional_string() -> Option<String> {
    None
}

#[derive(Debug, Deserialize)]
pub(super) struct PluginAction {
    pub(super) id: String,
    #[serde(default)]
    pub(super) icon: Option<IconRef>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PluginContextAction {
    pub(super) id: String,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) subtitle: String,
    #[serde(default)]
    pub(super) icon: Option<IconRef>,
}

pub(super) fn validate_query_response(response: &QueryResponse, request_id: &str) -> Result<()> {
    validate_response_id(response.request_id.as_deref(), request_id)?;
    if let Some(error) = &response.error {
        anyhow::bail!("plugin error [{}]: {}", error.code, error.message);
    }
    Ok(())
}

pub(super) fn validate_protocol_response(output: &str, request_id: &str) -> Result<()> {
    if output.len() > MAX_PLUGIN_SMALL_RESPONSE_BYTES {
        anyhow::bail!(
            "plugin response exceeds {} bytes",
            MAX_PLUGIN_SMALL_RESPONSE_BYTES
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(output).context("decode plugin response envelope")?;
    validate_json_shape(&value)?;
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

pub(super) fn bound_execute_response(output: &str) -> Result<String> {
    let mut value: serde_json::Value =
        serde_json::from_str(output).context("decode plugin execute response")?;
    if let serde_json::Value::Object(fields) = &mut value {
        for key in ["title", "subtitle"] {
            if let Some(serde_json::Value::String(text)) = fields.get_mut(key) {
                *text = limit_context_text(text, MAX_PLUGIN_ITEM_SUBTITLE_CHARS);
            }
        }
    }
    serde_json::to_string(&value).context("encode bounded plugin execute response")
}

pub(super) fn validate_json_shape(value: &serde_json::Value) -> Result<()> {
    fn visit(
        value: &serde_json::Value,
        depth: usize,
        nodes: &mut usize,
        string_bytes: &mut usize,
    ) -> Result<()> {
        if depth > MAX_PLUGIN_JSON_DEPTH {
            anyhow::bail!("plugin response nesting exceeds {MAX_PLUGIN_JSON_DEPTH} levels");
        }
        *nodes = nodes.saturating_add(1);
        if *nodes > MAX_PLUGIN_JSON_NODES {
            anyhow::bail!("plugin response contains too many JSON values");
        }
        match value {
            serde_json::Value::String(text) => {
                *string_bytes = string_bytes.saturating_add(text.len());
                if *string_bytes > MAX_PLUGIN_JSON_STRING_BYTES {
                    anyhow::bail!("plugin response contains too much string data");
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, depth + 1, nodes, string_bytes)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    *string_bytes = string_bytes.saturating_add(key.len());
                    visit(value, depth + 1, nodes, string_bytes)?;
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
        Ok(())
    }

    let mut nodes = 0;
    let mut string_bytes = 0;
    visit(value, 0, &mut nodes, &mut string_bytes)
}

pub(super) fn validate_lua_value(value: &Value) -> Result<()> {
    fn visit(
        value: &Value,
        depth: usize,
        nodes: &mut usize,
        string_bytes: &mut usize,
    ) -> Result<()> {
        if depth > MAX_PLUGIN_JSON_DEPTH {
            anyhow::bail!("plugin callback nesting exceeds {MAX_PLUGIN_JSON_DEPTH} levels");
        }
        *nodes = nodes.saturating_add(1);
        if *nodes > MAX_PLUGIN_JSON_NODES {
            anyhow::bail!("plugin callback contains too many Lua values");
        }
        match value {
            Value::String(text) => {
                *string_bytes = string_bytes.saturating_add(text.as_bytes().len());
                if *string_bytes > MAX_PLUGIN_JSON_STRING_BYTES {
                    anyhow::bail!("plugin callback contains too much string data");
                }
            }
            Value::Table(table) => {
                for pair in table.clone().pairs::<Value, Value>() {
                    let (key, value) = pair?;
                    visit(&key, depth + 1, nodes, string_bytes)?;
                    visit(&value, depth + 1, nodes, string_bytes)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    let mut nodes = 0;
    let mut string_bytes = 0;
    visit(value, 0, &mut nodes, &mut string_bytes)
}

pub(super) fn parse_plugin_response<T: DeserializeOwned>(
    output: &str,
    max_bytes: usize,
) -> Result<T> {
    if output.len() > max_bytes {
        anyhow::bail!("plugin response exceeds {max_bytes} bytes");
    }
    let value: serde_json::Value =
        serde_json::from_str(output).context("decode plugin response")?;
    validate_json_shape(&value)?;
    serde_json::from_value(value).context("decode plugin response payload")
}

pub(super) fn validate_response_id(response_id: Option<&str>, request_id: &str) -> Result<()> {
    if let Some(response_id) = response_id {
        if response_id != request_id {
            anyhow::bail!(
                "plugin response request_id mismatch: expected {request_id}, got {response_id}"
            );
        }
    }
    Ok(())
}

pub(super) fn to_result(
    plugin: &InstalledPlugin,
    item: PluginItem,
    requested_command_id: Option<&str>,
    _requested_query: &str,
    language: &str,
) -> SearchResult {
    let item = sanitize_plugin_item(item);
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
        .map(|command| command_action_words(plugin, command))
        .unwrap_or_default();
    // A default-search session entry is a discoverable fallback, not an
    // exact alias for every query. Treat it like configured custom commands
    // so ordinary applications and actions remain ahead of it.
    let icon = item
        .icon
        .or_else(|| command.and_then(|command| command.icon.clone()));
    let target = item.target.map(PathBuf::from).unwrap_or_default();
    let mut seen_context_action_ids = HashSet::new();
    let context_actions = item
        .context_actions
        .into_iter()
        .take(MAX_PLUGIN_CONTEXT_ACTIONS)
        .filter(|action| {
            !action.id.trim().is_empty()
                && action.id.len() <= MAX_PLUGIN_CONTEXT_ACTION_ID_BYTES
                && seen_context_action_ids.insert(action.id.clone())
        })
        .map(|action| {
            let action_id = action.id;
            let icon = action
                .icon
                .and_then(|icon| resolve_plugin_icon(&plugin.root, icon));
            Action::ContextMenu(ContextMenuAction {
                id: action_id.clone(),
                title: if action.title.trim().is_empty() {
                    limit_context_text(&action_id, MAX_PLUGIN_CONTEXT_TITLE_CHARS)
                } else {
                    limit_context_text(&detail_text(action.title), MAX_PLUGIN_CONTEXT_TITLE_CHARS)
                },
                subtitle: limit_context_text(
                    &detail_text(action.subtitle),
                    MAX_PLUGIN_CONTEXT_SUBTITLE_CHARS,
                ),
                icon: icon.clone(),
                behavior: ContextMenuBehavior::Plugin {
                    plugin_id: plugin.manifest.id.clone(),
                    action_id,
                },
            })
        })
        .collect::<Vec<_>>();
    let actions = item
        .actions
        .into_iter()
        .map(|action| {
            if action.id == "open_detail" {
                Action::open_detail()
            } else {
                Action::Plugin {
                    plugin_id: plugin.manifest.id.clone(),
                    action_id: action.id,
                    icon: action
                        .icon
                        .and_then(|icon| resolve_plugin_icon(&plugin.root, icon)),
                }
            }
        })
        .collect();
    let allow_favorite = item
        .allow_favorite
        .unwrap_or(item.session_updates.is_empty());
    let mut actions = if !item.session_updates.is_empty() {
        vec![Action::UpdateSession {
            updates: item.session_updates,
        }]
    } else if item.session_entry {
        command_action_words
            .first()
            .map(|word| Action::EnterSearchEngine {
                prefix: format!("{} ", word.trim()),
            })
            .into_iter()
            .collect()
    } else {
        actions
    };
    actions.extend(context_actions);
    let mut aliases = item.aliases;
    let mut metadata = crate::result::SearchMetadata {
        keywords: item.keywords,
        context: item.context,
        technical: item.technical,
    };
    // Keep all explicit item evidence first. The 32-term cap was already
    // enforced by sanitize_plugin_item; injected terms may use only space
    // left after those fields, so manifest keywords cannot evict item IDs or
    // other technical evidence.
    let explicit_count =
        aliases.len() + metadata.keywords.len() + metadata.context.len() + metadata.technical.len();
    let mut remaining = MAX_PLUGIN_ITEM_ALIASES.saturating_sub(explicit_count);
    if item.session_entry {
        for word in &command_action_words {
            if remaining == 0 {
                break;
            }
            if append_unique_term(&mut aliases, word) {
                remaining -= 1;
            }
        }
    }
    for word in plugin
        .manifest
        .keywords
        .iter()
        .chain(command_keywords.iter())
    {
        if remaining == 0 {
            break;
        }
        if append_unique_term(&mut metadata.keywords, word) {
            remaining -= 1;
        }
    }
    SearchResult {
        id: format!("plugin:{}:{}", plugin.manifest.id, item.id),
        identity: format!("plugin:{}:{}", plugin.manifest.id, item.id),
        title: item.title,
        // Command ownership routes the query to a plugin search engine; it
        // does not change what the plugin item is.
        category: item
            .category
            .as_deref()
            .map(parse_plugin_category)
            .unwrap_or(ResultCategory::Application),
        aliases,
        metadata,
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
        ranking_bias: plugin
            .weight
            .load(Ordering::Acquire)
            // Plugin-authored item bias is a small tie-breaker, never a way
            // to bypass root-search relevance or the user's explicit order.
            .saturating_add(item.ranking_bias.clamp(-100, 100)),
        allow_favorite,
        visibility: match item.visible_when {
            PluginItemVisibility::Always => crate::result::ResultVisibility::Always,
            PluginItemVisibility::EmptyQuery => crate::result::ResultVisibility::EmptyQueryOnly,
        },
    }
}

fn append_unique_term(values: &mut Vec<String>, value: &str) -> bool {
    let value = limit_context_text(value, MAX_PLUGIN_ALIAS_CHARS);
    let normalized = crate::ranking::normalize_query(&value);
    if normalized.is_empty()
        || values
            .iter()
            .any(|existing| crate::ranking::normalize_query(existing) == normalized)
    {
        return false;
    }
    values.push(value);
    true
}

pub(super) fn sanitize_plugin_item(mut item: PluginItem) -> PluginItem {
    item.id = limit_context_text(&item.id, MAX_PLUGIN_ITEM_ID_CHARS);
    item.title = limit_context_text(&item.title, MAX_PLUGIN_ITEM_TITLE_CHARS);
    item.subtitle = limit_context_text(&item.subtitle, MAX_PLUGIN_ITEM_SUBTITLE_CHARS);
    item.aliases = item
        .aliases
        .into_iter()
        .take(MAX_PLUGIN_ITEM_ALIASES)
        .map(|alias| limit_context_text(&alias, MAX_PLUGIN_ALIAS_CHARS))
        .collect();
    let mut remaining = MAX_PLUGIN_ITEM_ALIASES.saturating_sub(item.aliases.len());
    for values in [&mut item.keywords, &mut item.context, &mut item.technical] {
        let take = values.len().min(remaining);
        values.truncate(take);
        for value in values.iter_mut() {
            *value = limit_context_text(value, MAX_PLUGIN_ALIAS_CHARS);
        }
        remaining -= take;
    }
    item.target = item
        .target
        .map(|target| limit_context_text(&target, MAX_PLUGIN_ITEM_SUBTITLE_CHARS));
    item.actions.truncate(MAX_PLUGIN_ACTIONS);
    for action in &mut item.actions {
        action.id = limit_context_text(&action.id, MAX_PLUGIN_CONTEXT_ACTION_ID_BYTES);
    }
    item.context_actions.truncate(MAX_PLUGIN_CONTEXT_ACTIONS);
    for action in &mut item.context_actions {
        action.id = limit_context_text(&action.id, MAX_PLUGIN_CONTEXT_ACTION_ID_BYTES);
        action.title = limit_context_text(&action.title, MAX_PLUGIN_CONTEXT_TITLE_CHARS);
        action.subtitle = limit_context_text(&action.subtitle, MAX_PLUGIN_CONTEXT_SUBTITLE_CHARS);
    }
    item.command_id = item
        .command_id
        .map(|command_id| limit_context_text(&command_id, MAX_PLUGIN_ITEM_ID_CHARS));
    item.session_updates = item
        .session_updates
        .into_iter()
        .take(MAX_PLUGIN_ITEMS)
        .map(|(key, value)| {
            (
                limit_context_text(&key, MAX_PLUGIN_ITEM_ID_CHARS),
                limit_context_text(&value, MAX_PLUGIN_ITEM_SUBTITLE_CHARS),
            )
        })
        .collect();
    item
}

pub(super) fn parse_plugin_category(value: &str) -> ResultCategory {
    match value.trim().to_ascii_lowercase().as_str() {
        "game" => ResultCategory::Game,
        "website" => ResultCategory::Website,
        "calculator" => ResultCategory::Calculator,
        "control_panel" | "control-panel" => ResultCategory::ControlPanel,
        "document" => ResultCategory::Document,
        "bookmark" => ResultCategory::Bookmark,
        "window" => ResultCategory::Window,
        "other" => ResultCategory::Other,
        _ => ResultCategory::Application,
    }
}

pub(super) fn resolve_plugin_icon(root: &Path, icon: IconRef) -> Option<IconSpec> {
    match icon {
        IconRef::Resource { path } => plugin_resource_path(root, &path).map(IconSpec::Resource),
        IconRef::File { path } => {
            let path = PathBuf::from(path);
            path.is_absolute().then_some(IconSpec::Resource(path))
        }
        IconRef::System { target } => Some(IconSpec::System(target.into())),
        IconRef::Named { id } => Some(IconSpec::Named(id)),
        IconRef::Remote { url } => validate_plugin_http_url(&url)
            .is_ok()
            .then_some(IconSpec::Remote(url)),
    }
}

pub(super) fn plugin_resource_path(root: &Path, relative: &str) -> Option<PathBuf> {
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

pub(super) fn to_detail_view(
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
            .take(MAX_PLUGIN_DETAIL_ACTIONS)
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
            .take(MAX_PLUGIN_DETAIL_BLOCKS)
            .map(|block| match block {
                PluginDetailBlock::Text {
                    text,
                    color,
                    copyable,
                } => crate::result::DetailBlock::Text {
                    text: detail_text(text),
                    color: detail_color(&color),
                    copyable,
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
                PluginDetailBlock::Markdown {
                    markdown,
                    color,
                    copyable,
                } => crate::result::DetailBlock::Markdown {
                    markdown: detail_text(markdown),
                    color: detail_color(&color),
                    copyable,
                },
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
                    code,
                },
                PluginDetailBlock::Table { columns, rows } => crate::result::DetailBlock::Table {
                    columns: columns
                        .into_iter()
                        .take(MAX_PLUGIN_DETAIL_CELLS)
                        .map(detail_text)
                        .collect(),
                    rows: rows
                        .into_iter()
                        .take(MAX_PLUGIN_DETAIL_ROWS)
                        .map(|row| {
                            row.into_iter()
                                .take(MAX_PLUGIN_DETAIL_CELLS)
                                .map(detail_text)
                                .collect()
                        })
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

pub(super) fn detail_text(value: String) -> String {
    limit_context_text(&value, MAX_PLUGIN_DETAIL_TEXT_CHARS)
}

pub(super) fn limit_context_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(super) fn detail_color(value: &str) -> String {
    match value {
        "primary" | "secondary" | "tertiary" | "accent" => value.into(),
        _ => "secondary".into(),
    }
}

pub(super) fn is_supported_detail_url(value: &str) -> bool {
    let value = value.trim();
    value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .is_some_and(|rest| !rest.is_empty() && !rest.chars().any(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::{
        append_unique_term, parse_plugin_response, validate_json_shape, validate_response_id,
    };
    use crate::plugin::MAX_PLUGIN_JSON_DEPTH;

    #[test]
    fn plugin_string_lists_accept_empty_lua_tables_but_reject_nonempty_objects() {
        let mut item = serde_json::json!({"id":"item", "title":"Item"});
        for field in ["aliases", "keywords", "context", "technical"] {
            item[field] = serde_json::json!({});
        }
        let parsed: super::super::PluginItem = serde_json::from_value(item.clone()).unwrap();
        assert!(parsed.aliases.is_empty());
        assert!(parsed.keywords.is_empty());
        assert!(parsed.context.is_empty());
        assert!(parsed.technical.is_empty());

        item["aliases"] = serde_json::json!(["alias"]);
        item["keywords"] = serde_json::json!(["keyword"]);
        item["context"] = serde_json::json!(["context"]);
        item["technical"] = serde_json::json!(["technical"]);
        let parsed: super::super::PluginItem = serde_json::from_value(item.clone()).unwrap();
        assert_eq!(parsed.aliases, ["alias"]);
        assert_eq!(parsed.keywords, ["keyword"]);
        assert_eq!(parsed.context, ["context"]);
        assert_eq!(parsed.technical, ["technical"]);

        for field in ["aliases", "keywords", "context", "technical"] {
            item[field] = serde_json::json!({"unexpected":"value"});
            assert!(serde_json::from_value::<super::super::PluginItem>(item.clone()).is_err());
            item[field] = serde_json::json!([]);
        }
    }

    #[test]
    fn response_parser_rejects_malformed_json_and_excessive_depth() {
        assert!(parse_plugin_response::<serde_json::Value>("{broken", 1024).is_err());

        let mut value = serde_json::Value::Null;
        for _ in 0..=MAX_PLUGIN_JSON_DEPTH {
            value = serde_json::Value::Array(vec![value]);
        }
        assert!(validate_json_shape(&value).is_err());
    }

    #[test]
    fn stale_request_id_is_rejected() {
        assert!(validate_response_id(Some("request-old"), "request-current").is_err());
        assert!(validate_response_id(Some("request-current"), "request-current").is_ok());
    }

    #[test]
    fn injected_plugin_terms_deduplicate_by_search_normalization() {
        let mut values = vec!["Alpha   Beta".to_owned()];
        assert!(!append_unique_term(&mut values, "Ａｌｐｈａ\tＢｅｔａ"));
        assert!(append_unique_term(&mut values, "Gamma"));
        assert_eq!(values, vec!["Alpha   Beta", "Gamma"]);
    }
}
