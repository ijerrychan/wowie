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
    path::PathBuf,
    process::{Command, Stdio},
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

#[derive(Debug, Clone)]
struct InstalledPlugin {
    root: PathBuf,
    manifest: PluginManifest,
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
    locale: String,
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
                    Ok(manifest) => plugins.push(InstalledPlugin {
                        root: dir,
                        manifest,
                    }),
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
            locale: locale.to_owned(),
        }
    }

    pub fn query(&self, query: &str) -> Vec<SearchResult> {
        self.plugins
            .iter()
            .flat_map(|plugin| match query_plugin(plugin, query, &self.locale) {
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
        let mut child = Command::new(&plugin.manifest.runtime)
            .arg(&plugin.manifest.entry_point)
            .current_dir(&plugin.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
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
    let mut child = Command::new(&plugin.manifest.runtime)
        .arg(&plugin.manifest.entry_point)
        .current_dir(&plugin.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
        icon: item.icon.map(|icon| match icon {
            PluginIcon::Resource { path } => IconSpec::Resource(plugin.root.join(path)),
            PluginIcon::System { target } => IconSpec::System(target.into()),
        }),
        ranking_bias: 0,
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
}
