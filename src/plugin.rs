//! Script plugin support.
//!
//! Plugins are deliberately process based.  The launcher speaks a small
//! JSON-lines protocol, so Lua is a first-class option without making the
//! launcher depend on a particular Lua implementation.  Python, Node, or a
//! small native helper can use the same protocol as well.

use crate::result::{Action, IconSpec, LaunchEntry, ResultCategory, SearchResult};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
};

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
    pub runtime: String,
    pub entry_point: String,
    #[serde(default)]
    pub mode: PluginMode,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub permissions: Vec<String>,
    #[serde(default = "default_locale")]
    pub default_locale: String,
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginMode {
    ShortLived,
    Resident,
}

impl Default for PluginMode {
    fn default() -> Self {
        Self::ShortLived
    }
}

#[derive(Debug, Clone)]
struct InstalledPlugin {
    root: PathBuf,
    manifest: PluginManifest,
    process: Option<Arc<Mutex<ResidentProcess>>>,
}

#[derive(Debug)]
struct ResidentProcess {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Debug, Serialize)]
struct QueryRequest<'a> {
    method: &'static str,
    query: &'a str,
    api_version: u32,
    locale: String,
    fallback_locale: &'a str,
}

#[derive(Debug, Deserialize)]
struct QueryResponse {
    #[serde(default)]
    items: Vec<PluginItem>,
}

#[derive(Debug, Deserialize)]
pub struct ExecuteResponse {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
}

#[derive(Debug, Deserialize)]
struct PluginItem {
    id: String,
    title: String,
    #[serde(default)]
    subtitle: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    actions: Vec<PluginAction>,
    #[serde(default)]
    icon: Option<PluginIcon>,
    #[serde(default)]
    detail: Option<PluginDetailView>,
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
}

#[derive(Debug, Deserialize)]
struct PluginDetailAction {
    id: String,
    #[serde(default)]
    title: String,
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
    },
    Weather {
        value: String,
    },
    Separator,
}

fn default_detail_color() -> String {
    "secondary".into()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PluginIcon {
    Resource { path: String },
    System { target: String },
}

#[derive(Debug, Deserialize)]
struct PluginAction {
    id: String,
}

#[derive(Debug, Clone)]
pub struct PluginManager {
    plugins: Vec<InstalledPlugin>,
    locale: Arc<Mutex<String>>,
}

impl PluginManager {
    pub fn discover_with_locale(locale: &str) -> Self {
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
                        let process = if manifest.mode == PluginMode::Resident {
                            match start_resident(&dir, &manifest, locale) {
                                Ok(process) => Some(Arc::new(Mutex::new(process))),
                                Err(error) => {
                                    eprintln!(
                                        "[plugin:{}] resident start failed: {error:#}",
                                        manifest.id
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        plugins.push(InstalledPlugin {
                            root: dir,
                            manifest,
                            process,
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
            *current = locale;
        }
    }

    pub fn query(&self, query: &str) -> Vec<SearchResult> {
        let locale = self
            .locale
            .lock()
            .map(|locale| locale.clone())
            .unwrap_or_else(|_| "system".into());
        self.plugins
            .iter()
            .flat_map(|plugin| match query_plugin(plugin, query, &locale) {
                Ok(items) => items,
                Err(error) => {
                    eprintln!("[plugin:{}] query failed: {error:#}", plugin.manifest.id);
                    Vec::new()
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
        let request = serde_json::json!({"method":"execute", "item_id":item_id, "action_id":action_id, "api_version":plugin.manifest.api_version});
        if let Some(process) = &plugin.process {
            return resident_request(process, &request);
        }
        let mut command = Command::new(&plugin.manifest.runtime);
        command
            .arg(&plugin.manifest.entry_point)
            .current_dir(&plugin.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut command);
        let mut child = command.spawn()?;
        use std::io::Write;
        child
            .stdin
            .take()
            .context("plugin stdin unavailable")?
            .write_all(format!("{request}\n").as_bytes())?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            anyhow::bail!(
                "exit status {}; {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

fn query_plugin(plugin: &InstalledPlugin, query: &str, locale: &str) -> Result<Vec<SearchResult>> {
    let request = serde_json::to_string(&QueryRequest {
        method: "query",
        query,
        api_version: plugin.manifest.api_version,
        locale: locale.to_owned(),
        fallback_locale: &plugin.manifest.default_locale,
    })?;
    if let Some(process) = &plugin.process {
        let request = serde_json::json!({
            "method": "query",
            "query": query,
            "api_version": plugin.manifest.api_version,
            "locale": locale,
            "fallback_locale": plugin.manifest.default_locale,
        });
        let output = resident_request(process, &request)?;
        let response: QueryResponse =
            serde_json::from_str(&output).context("decode plugin response")?;
        return Ok(response
            .items
            .into_iter()
            .map(|item| to_result(plugin, item))
            .collect());
    }
    let mut command = Command::new(&plugin.manifest.runtime);
    command
        .arg(&plugin.manifest.entry_point)
        .current_dir(&plugin.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console_window(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("start runtime {}", plugin.manifest.runtime))?;
    use std::io::Write;
    child
        .stdin
        .take()
        .context("plugin stdin unavailable")?
        .write_all(format!("{request}\n").as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!(
            "exit status {}; {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let response: QueryResponse =
        serde_json::from_slice(&output.stdout).context("decode plugin response")?;
    Ok(response
        .items
        .into_iter()
        .map(|item| to_result(plugin, item))
        .collect())
}

fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Plugin queries run once per search update. Console runtimes such as
        // python.exe must not flash a window for every typed character.
        command.creation_flags(0x0800_0000);
    }
}

fn start_resident(
    root: &PathBuf,
    manifest: &PluginManifest,
    locale: &str,
) -> Result<ResidentProcess> {
    let mut command = Command::new(&manifest.runtime);
    command
        .arg(&manifest.entry_point)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console_window(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("start resident runtime {}", manifest.runtime))?;
    let stdin = child
        .stdin
        .take()
        .context("resident plugin stdin unavailable")?;
    let stdout = BufReader::new(
        child
            .stdout
            .take()
            .context("resident plugin stdout unavailable")?,
    );
    let mut process = ResidentProcess {
        _child: child,
        stdin,
        stdout,
    };
    let init = serde_json::json!({
        "method": "initialize",
        "api_version": manifest.api_version,
        "locale": locale,
        "fallback_locale": manifest.default_locale,
    });
    let _ = resident_request_inner(&mut process, &init)?;
    Ok(process)
}

fn resident_request(
    process: &Arc<Mutex<ResidentProcess>>,
    request: &serde_json::Value,
) -> Result<String> {
    let mut process = process
        .lock()
        .map_err(|_| anyhow::anyhow!("resident plugin lock poisoned"))?;
    resident_request_inner(&mut process, request)
}

fn resident_request_inner(
    process: &mut ResidentProcess,
    request: &serde_json::Value,
) -> Result<String> {
    writeln!(process.stdin, "{}", serde_json::to_string(request)?)?;
    process.stdin.flush()?;
    let mut response = String::new();
    if process.stdout.read_line(&mut response)? == 0 {
        anyhow::bail!("resident plugin exited unexpectedly");
    }
    Ok(response.trim().to_owned())
}

fn to_result(plugin: &InstalledPlugin, item: PluginItem) -> SearchResult {
    let target = item.target.map(PathBuf::from).unwrap_or_default();
    let actions = item
        .actions
        .into_iter()
        .map(|action| Action::Plugin {
            plugin_id: plugin.manifest.id.clone(),
            action_id: action.id,
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
            .collect(),
        location: item.subtitle,
        launch_target: target,
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: format!("plugin:{}", plugin.manifest.id).into(),
        sources: vec![format!("plugin:{}", plugin.manifest.id).into()],
        launch_entries: Vec::<LaunchEntry>::new(),
        actions,
        detail: item.detail.map(to_detail_view),
        icon: item.icon.map(|icon| match icon {
            PluginIcon::Resource { path } => IconSpec::Resource(plugin.root.join(path)),
            PluginIcon::System { target } => IconSpec::System(target.into()),
        }),
        ranking_bias: 0,
    }
}

fn to_detail_view(view: PluginDetailView) -> crate::result::DetailView {
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
                PluginDetailBlock::Icon { glyph, color } => crate::result::DetailBlock::Icon {
                    glyph: detail_text(glyph),
                    color: detail_color(&color),
                },
                PluginDetailBlock::Weather { value } => crate::result::DetailBlock::Weather {
                    value: match value.as_str() {
                        "clear" | "cloud" | "rain" | "snow" | "storm" => value,
                        _ => "cloud".into(),
                    },
                },
                PluginDetailBlock::Separator => crate::result::DetailBlock::Separator,
            })
            .collect(),
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
    fn manifest_defaults_are_backward_compatible() {
        let m: PluginManifest = serde_json::from_str(
            r#"{"id":"x","name":"X","runtime":"lua","entry_point":"main.lua"}"#,
        )
        .unwrap();
        assert_eq!(m.api_version, 1);
        assert_eq!(m.version, "0.1.0");
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
        let detail = to_detail_view(view);
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
