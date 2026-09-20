//! Search-driven shortcut provider.
//!
//! This module owns provider lifecycle and dispatch only. Each shortcut
//! behavior lives in its own sibling module so adding a new built-in shortcut
//! does not make this provider a second command parser.

#[path = "shortcut/application.rs"]
mod application;
#[path = "shortcut/command.rs"]
mod command;
#[path = "shortcut/web.rs"]
mod web;

use crate::{
    config::ShortcutConfig,
    platform::SourceId,
    provider::{DynamicProvider, ProviderRegistry},
    result::{Action, IconSpec, LaunchEntry, ResultCategory, SearchResult},
};
use std::path::PathBuf;

pub(super) const COMMAND_PREFIX: &str = "> ";
pub(super) const WEB_PREFIX: &str = "w ";

/// The common query state shared by every shortcut implementation.
#[derive(Clone, Copy)]
pub(super) struct ShortcutQuery<'a> {
    pub(super) raw: &'a str,
    pub(super) text: &'a str,
    pub(super) language: &'a str,
}

impl<'a> ShortcutQuery<'a> {
    fn new(raw: &'a str, language: &'a str) -> Option<Self> {
        let text = raw.trim();
        (!text.is_empty()).then_some(Self {
            raw,
            text,
            language,
        })
    }

    pub(super) fn resolve_argument(self, prefix: &str) -> ShortcutArgument<'a> {
        match shortcut_argument(self.raw, prefix) {
            Some(value) => ShortcutArgument {
                value: value.trim(),
                explicit: true,
            },
            None => ShortcutArgument {
                value: self.text,
                explicit: false,
            },
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ShortcutArgument<'a> {
    pub(super) value: &'a str,
    pub(super) explicit: bool,
}

/// The normalized data needed to materialize a launch-oriented shortcut.
///
/// Shortcut-specific modules calculate targets and display details; this
/// builder keeps SearchResult and Action construction in one place.
pub(super) struct ShortcutSpec {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) prefix: String,
    pub(super) argument: String,
    pub(super) display_argument: String,
    pub(super) location: String,
    pub(super) target: PathBuf,
    pub(super) launch_arguments: String,
    pub(super) preview: bool,
    pub(super) include_query_alias: bool,
    pub(super) icon: Option<IconSpec>,
}

pub(super) fn build_result(query: ShortcutQuery<'_>, spec: ShortcutSpec) -> SearchResult {
    let icon = spec
        .icon
        .or_else(|| Some(IconSpec::System(spec.target.clone())));
    let title = if spec.preview {
        spec.name.clone()
    } else {
        format!("{} \"{}\"", spec.name, spec.display_argument)
    };
    let alias = if spec.prefix.ends_with(char::is_whitespace) {
        format!("{}{}", spec.prefix, spec.argument)
    } else {
        format!("{} {}", spec.prefix, spec.argument)
    };
    let mut aliases = vec![alias];
    if spec.include_query_alias {
        aliases.push(query.text.to_owned());
    }

    let actions = if spec.preview {
        Vec::new()
    } else {
        vec![Action::Launch(LaunchEntry {
            source: SourceId::Shortcut,
            path: PathBuf::new(),
            target: spec.target.clone(),
            arguments: spec.launch_arguments.clone(),
            working_directory: None,
        })]
    };

    SearchResult {
        id: spec.id.clone(),
        identity: spec.id,
        title,
        category: ResultCategory::Shortcut,
        aliases,
        location: spec.location,
        launch_target: spec.target,
        launch_arguments: spec.launch_arguments,
        launch_working_directory: None,
        source: SourceId::Shortcut,
        sources: vec![SourceId::Shortcut],
        launch_entries: Vec::new(),
        actions,
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon,
        ranking_bias: 0,
    }
}

pub(super) fn result_id(index: usize, prefix: &str) -> String {
    format!("shortcut:{index}:{}", prefix.trim_end())
}

pub(super) fn effective_type(shortcut: &ShortcutConfig) -> &str {
    match shortcut.prefix.trim_end() {
        prefix if prefix == COMMAND_PREFIX.trim_end() => "command",
        prefix if prefix == WEB_PREFIX.trim_end() => "url",
        _ => shortcut.shortcut_type.as_str(),
    }
}

/// Dynamic results generated from the current query rather than an index.
pub fn results(query: &str, configured: &[ShortcutConfig], language: &str) -> Vec<SearchResult> {
    let Some(query) = ShortcutQuery::new(query, language) else {
        return Vec::new();
    };

    let mut results = Vec::with_capacity(configured.len() + 2);
    if let Some(result) = command::builtin(query) {
        results.push(result);
    }
    if let Some(result) = web::builtin(query) {
        results.push(result);
    }

    // Keep configured shortcuts in their original order. The index is part of
    // the existing result identity, so preserve its offset for rank history.
    for (index, shortcut) in configured.iter().enumerate() {
        let index = index + 2;
        let result = match effective_type(shortcut) {
            "url" => web::configured(query, shortcut, index),
            "application" => application::configured(query, shortcut, index),
            "command" => command::configured(query, shortcut, index),
            _ => None,
        };
        if let Some(result) = result {
            results.push(result);
        }
    }

    results
}

pub fn register(registry: &mut ProviderRegistry, shortcuts: Vec<ShortcutConfig>) {
    registry.register(ShortcutProvider::new(shortcuts));
}

pub struct ShortcutProvider {
    configured: Vec<ShortcutConfig>,
    language: String,
}

impl ShortcutProvider {
    pub fn new(configured: Vec<ShortcutConfig>) -> Self {
        Self {
            configured,
            language: "system".into(),
        }
    }

    pub fn replace_config(&mut self, configured: Vec<ShortcutConfig>) {
        self.configured = configured;
    }

    pub fn set_language(&mut self, language: String) {
        self.language = language;
    }

    pub fn results(&self, query: &str) -> Vec<SearchResult> {
        results(query, &self.configured, &self.language)
    }
}

impl DynamicProvider for ShortcutProvider {
    fn query(&self, query: &str) -> Vec<SearchResult> {
        self.results(query)
    }

    fn replace_config(&mut self, shortcuts: Vec<ShortcutConfig>) {
        ShortcutProvider::replace_config(self, shortcuts);
    }

    fn set_language(&mut self, language: String) {
        ShortcutProvider::set_language(self, language);
    }
}

pub(super) fn shortcut_argument<'a>(query: &'a str, prefix: &str) -> Option<&'a str> {
    query.strip_prefix(prefix).map(str::trim_start)
}

pub(super) fn expand_url_template(template: &str, query: &str) -> String {
    template.replace(
        "{query}",
        &url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>(),
    )
}

pub(super) fn expand_raw_template(template: &str, query: &str) -> String {
    template.replace("{query}", query)
}

pub(super) fn join_command_and_arguments(command: &str, arguments: &str) -> String {
    if arguments.trim().is_empty() {
        command.trim().to_string()
    } else {
        format!("{} {}", command.trim(), arguments.trim())
    }
}
