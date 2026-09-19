//! Runtime provider registry.
//!
//! SearchCore owns the search algorithm, while this module owns the list of
//! dynamic result providers. Adding a provider should only require implementing
//! `DynamicProvider` and placing it in `src/providers/dynamic`.

use crate::{config::ShortcutConfig, plugin::PluginManager, result::SearchResult};
use anyhow::{Context, Result};
use std::sync::Mutex;

pub trait DynamicProvider: Send + Sync {
    fn query(&self, query: &str) -> Vec<SearchResult>;

    fn replace_config(&mut self, _shortcuts: Vec<ShortcutConfig>) {}

    fn set_language(&mut self, _language: String) {}
}

pub(crate) fn find_plugin_action<'a>(
    item: &'a SearchResult,
    requested_action_id: &str,
) -> Result<(&'a str, &'a str)> {
    item.actions
        .iter()
        .find_map(|action| match action {
            crate::result::Action::Plugin {
                plugin_id,
                action_id,
            } if action_id == requested_action_id => Some((plugin_id.as_str(), action_id.as_str())),
            _ => None,
        })
        .context("result has no plugin action")
}

pub struct ProviderRegistry {
    plugins: PluginManager,
    dynamic: Vec<Mutex<Box<dyn DynamicProvider>>>,
}

impl ProviderRegistry {
    #[cfg(test)]
    pub fn without_dynamic(plugins: PluginManager) -> Self {
        Self {
            plugins,
            dynamic: Vec::new(),
        }
    }

    pub fn default_providers(plugins: PluginManager, shortcuts: Vec<ShortcutConfig>) -> Self {
        let mut registry = Self {
            plugins,
            dynamic: Vec::new(),
        };
        crate::providers::dynamic::register_all(&mut registry, shortcuts);
        registry
    }

    pub fn register<P>(&mut self, provider: P)
    where
        P: DynamicProvider + 'static,
    {
        self.dynamic.push(Mutex::new(Box::new(provider)));
    }

    pub fn query(&self, query: &str) -> Vec<SearchResult> {
        let mut results = self.plugins.query(query);
        for provider in &self.dynamic {
            if let Ok(provider) = provider.lock() {
                results.extend(provider.query(query));
            }
        }
        results
    }

    pub fn replace_shortcuts(&self, shortcuts: Vec<ShortcutConfig>) {
        for provider in &self.dynamic {
            if let Ok(mut provider) = provider.lock() {
                provider.replace_config(shortcuts.clone());
            }
        }
    }

    pub fn set_language(&self, language: String) {
        self.plugins.set_locale(language.clone());
        for provider in &self.dynamic {
            if let Ok(mut provider) = provider.lock() {
                provider.set_language(language.clone());
            }
        }
    }

    pub fn execute_plugin_action(
        &self,
        item: &SearchResult,
        requested_action_id: &str,
    ) -> Result<crate::plugin::ExecuteResponse> {
        let (plugin_id, action_id) = find_plugin_action(item, requested_action_id)?;
        let output = self.plugins.execute(
            plugin_id,
            item.id.rsplit(':').next().unwrap_or_default(),
            action_id,
        )?;
        serde_json::from_str(&output).context("decode plugin execute response")
    }
}
