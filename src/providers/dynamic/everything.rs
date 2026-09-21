//! Everything IPC-backed file search engine.
//!
//! The provider uses the `everything-ipc` crate's window-message client; it
//! never starts Everything and safely returns no file rows when the client is
//! unavailable.

use crate::{
    page::{SearchEngineMode, SearchEngineSession},
    platform::SourceId,
    provider::{ProviderRegistry, SearchEngineProvider},
    result::{Action, DetailBlock, DetailView, IconSpec, LaunchEntry, ResultCategory, SearchResult},
};
use std::path::PathBuf;

const PREFIX: &str = "e ";
const RESULT_ID: &str = "everything:search";
// Everything can match an entire indexed disk for a short or empty query.
// Bound the IPC response before constructing result rows; the generic search
// UI remains unbounded for providers whose result sets are already practical.
// Slint creates result delegates synchronously when the model is replaced.
// Keep one IPC page small enough that a broad query does not create a long
// callback/instantiation chain; the search-engine page API can request more.
const MAX_RESULTS: u32 = 30;

pub fn register(registry: &mut ProviderRegistry, _shortcuts: Vec<crate::config::ShortcutConfig>) {
    registry.register_search_engine(EverythingProvider::default());
}

pub struct EverythingProvider {
    language: String,
}

impl Default for EverythingProvider {
    fn default() -> Self {
        Self {
            language: "system".into(),
        }
    }
}

impl SearchEngineProvider for EverythingProvider {
    fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        let argument = query.trim_start().strip_prefix(PREFIX)?.trim_start();
        Some(SearchEngineSession {
            id: RESULT_ID.into(),
            title: crate::utils::i18n::tr(&self.language, "Search with Everything").into(),
            prefix: PREFIX.into(),
            argument: argument.into(),
            mode: SearchEngineMode::Search,
            state: Default::default(),
        })
    }

    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        if query.trim().is_empty() {
            return vec![empty_entry(&self.language)];
        }
        vec![entry(query.trim(), &self.language)]
    }

    fn search_debounce(&self, _session: &SearchEngineSession) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    fn query_active(&self, _session: &SearchEngineSession, query: &str) -> Vec<SearchResult> {
        if query.trim().is_empty() {
            return vec![intro_entry(&self.language)];
        }
        #[cfg(windows)]
        {
            query_ipc(query, 0)
        }
        #[cfg(not(windows))]
        {
            let _ = session;
            Vec::new()
        }
    }

    fn query_active_page(
        &self,
        _session: &SearchEngineSession,
        query: &str,
        offset: usize,
    ) -> Vec<SearchResult> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        #[cfg(windows)]
        {
            query_ipc(query, offset)
        }
        #[cfg(not(windows))]
        {
            let _ = (session, offset);
            Vec::new()
        }
    }

    fn owns_session(&self, session: &SearchEngineSession) -> bool {
        session.id == RESULT_ID
    }

    fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        (result_id == RESULT_ID).then(|| PREFIX.into())
    }

    fn search_engine_result(&self, session: &SearchEngineSession) -> Option<SearchResult> {
        (session.id == RESULT_ID).then(|| entry("", &self.language))
    }

    fn set_language(&mut self, language: String) {
        self.language = language;
    }
}

fn entry(query: &str, language: &str) -> SearchResult {
    SearchResult {
        id: RESULT_ID.into(),
        identity: RESULT_ID.into(),
        title: format!(
            "{} \"{query}\"",
            crate::utils::i18n::tr(language, "Search with Everything")
        ),
        category: ResultCategory::Other,
        aliases: vec![crate::utils::i18n::tr(language, "Everything").into()],
        location: crate::utils::i18n::tr(language, "Everything IPC").into(),
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: SourceId::Custom("Everything".into()),
        sources: vec![SourceId::Custom("Everything".into())],
        launch_entries: Vec::new(),
        actions: vec![Action::EnterSearchEngine {
            prefix: PREFIX.into(),
        }],
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: Some(everything_icon()),
        ranking_bias: -1000,
        visibility: crate::result::ResultVisibility::Always,
    }
}

fn empty_entry(language: &str) -> SearchResult {
    let mut result = entry("", language);
    result.title = crate::utils::i18n::tr(language, "Search with Everything").into();
    result.location = crate::utils::i18n::tr(language, "Search files with Everything; all Everything syntax is supported").into();
    result.visibility = crate::result::ResultVisibility::EmptyQueryOnly;
    result.ranking_bias = 0;
    result
}

fn intro_entry(language: &str) -> SearchResult {
    SearchResult {
        id: "everything:introduction".into(),
        identity: "everything:introduction".into(),
        title: crate::utils::i18n::tr(language, "Search with Everything").into(),
        category: ResultCategory::Other,
        aliases: Vec::new(),
        location: crate::utils::i18n::tr(language, "Search files with Everything; all Everything syntax is supported. Press Enter to view the usage guide.").into(),
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: SourceId::Custom("Everything".into()),
        sources: vec![SourceId::Custom("Everything".into())],
        launch_entries: Vec::new(),
        actions: vec![Action::OpenDetail],
        detail: Some(DetailView {
            title: crate::utils::i18n::tr(language, "Search with Everything").into(),
            subtitle: crate::utils::i18n::tr(language, "Everything search syntax").into(),
            blocks: vec![DetailBlock::Text {
                text: crate::utils::i18n::tr(language, "Everything accepts its complete native query syntax, including filename filters, paths, operators, and regular expressions.").into(),
                color: "primary".into(),
                copyable: true,
            }],
            actions: Vec::new(), source: String::new(), preferred_width: None, scrollable: false,
            fullscreen: true,
        }),
        description: None, app_user_model_id: None, package_family_name: None, package_metadata: None,
        icon: Some(everything_icon()), ranking_bias: 0,
        visibility: crate::result::ResultVisibility::EmptyQueryOnly,
    }
}

fn everything_icon() -> IconSpec {
    #[cfg(windows)]
    {
        IconSpec::System(find_everything_executable())
    }
    #[cfg(not(windows))]
    {
        IconSpec::Named("folder-open".into())
    }
}

#[cfg(windows)]
fn find_everything_executable() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        candidates
            .extend(std::env::split_paths(&path).map(|directory| directory.join("Everything.exe")));
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            let root = PathBuf::from(root);
            candidates.push(root.join("Everything").join("Everything.exe"));
            if let Ok(entries) = std::fs::read_dir(&root) {
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
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("Everything.exe"))
}

#[cfg(windows)]
fn query_ipc(query: &str, offset: usize) -> Vec<SearchResult> {
    use everything_ipc::wm::{EverythingClient, RequestFlags};
    let Ok(client) = EverythingClient::new() else {
        return Vec::new();
    };
    let Ok(list) = client
        .query_wait(query)
        .request_flags(RequestFlags::FileName | RequestFlags::Path)
        .offset(offset.min(u32::MAX as usize) as u32)
        .max_results(MAX_RESULTS)
        .call()
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let name = item.get_string(RequestFlags::FileName)?;
            let path = item.get_string(RequestFlags::Path)?;
            let full = PathBuf::from(path).join(&name);
            let icon = if matches!(
                full.extension().and_then(|extension| extension.to_str()),
                Some(extension)
                    if matches!(extension.to_ascii_lowercase().as_str(), "exe" | "lnk")
            ) {
                IconSpec::System(full.clone())
            } else {
                IconSpec::DefaultOpen(full.clone())
            };
            Some(SearchResult {
                id: format!("everything:file:{}", full.display()),
                identity: format!("everything:file:{}", full.display()),
                title: name,
                category: ResultCategory::Document,
                aliases: Vec::new(),
                location: full.display().to_string(),
                launch_target: full.clone(),
                launch_arguments: String::new(),
                launch_working_directory: None,
                source: SourceId::Custom("Everything".into()),
                sources: vec![SourceId::Custom("Everything".into())],
                launch_entries: Vec::new(),
                actions: vec![Action::Launch(LaunchEntry {
                    source: SourceId::Custom("Everything".into()),
                    path: PathBuf::new(),
                    target: full.clone(),
                    arguments: String::new(),
                    working_directory: None,
                })],
                detail: None,
                description: None,
                app_user_model_id: None,
                package_family_name: None,
                package_metadata: None,
                icon: Some(icon),
                ranking_bias: 0,
                visibility: crate::result::ResultVisibility::Always,
            })
        })
        .collect()
}
