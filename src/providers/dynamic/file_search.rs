//! Native Everything/Windows Search file-query engine.
use crate::{
    core::provider::{MatchingPolicy, ProviderRegistry, SearchEngineProvider, SearchEngineSettings},
    page::{SearchEngineMode, SearchEngineSession},
    platform::SourceId,
    result::{Action, IconSpec, ResultCategory, ResultVisibility, SearchResult},
    services::file_search::{self, FileSearchRuntimeConfig},
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

const SESSION_ID: &str = "plugin:everything:search";
const ENTRY_ID: &str = "plugin:everything:entry";
const PAGE_SIZE: usize = 30;

pub struct FileSearchProvider {
    language: String,
    config: Arc<FileSearchRuntimeConfig>,
    query_generation: Option<Arc<AtomicU64>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider(enabled: bool, keyword: &str) -> FileSearchProvider {
        let config = Arc::new(FileSearchRuntimeConfig::default());
        config.replace("windows-index", enabled, keyword);
        FileSearchProvider {
            language: "en".into(),
            config,
            query_generation: Some(Arc::new(AtomicU64::new(1))),
        }
    }

    #[test]
    fn configured_keyword_and_root_fallback_are_native_and_hide_when_disabled() {
        let mut provider = make_provider(true, "files");
        let session = provider.search_engine("files notes").unwrap();
        assert_eq!(session.prefix, "files ");
        assert_eq!(session.argument, "notes");
        provider.set_language("zh-CN".into());
        let localized = provider.search_engine("files notes").unwrap();
        assert_eq!(localized.title, "文件搜索");
        assert_eq!(localized.placeholder, "文件搜索");
        let root = provider.query_root("files notes");
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].id, ENTRY_ID);
        assert_eq!(root[0].ranking_bias, -1000);
        assert_eq!(root[0].visibility, ResultVisibility::Always);
        assert_eq!(root[0].source, SourceId::Custom("File Search".into()));
        assert!(matches!(
            root[0].actions.first(),
            Some(Action::EnterSearchEngine { prefix }) if prefix == "files "
        ));

        let blank_root = provider.query_root("");
        assert_eq!(blank_root[0].ranking_bias, 0);
        assert_eq!(blank_root[0].visibility, ResultVisibility::EmptyQueryOnly);

        let disabled = make_provider(false, "files");
        assert!(disabled.search_engine("files notes").is_none());
        assert!(disabled.query_root("files notes").is_empty());
    }
}

pub fn register(
    registry: &mut ProviderRegistry,
    _: Vec<crate::config::ShortcutConfig>,
    _: crate::config::ShortcutKeywords,
) {
    registry.register_search_engine(FileSearchProvider {
        language: "system".into(),
        config: Arc::new(FileSearchRuntimeConfig::default()),
        query_generation: None,
    });
}

impl SearchEngineProvider for FileSearchProvider {
    fn settings(&self) -> SearchEngineSettings {
        SearchEngineSettings {
            matching_policy: MatchingPolicy::ProviderOwned,
            ..SearchEngineSettings::default()
        }
    }

    fn set_query_cancellation(&mut self, generation: Arc<AtomicU64>) {
        self.query_generation = Some(generation);
    }

    fn set_file_search_config(&mut self, config: Arc<FileSearchRuntimeConfig>) {
        self.config = config;
    }

    fn search_engine(&self, query: &str) -> Option<SearchEngineSession> {
        if !self.config.enabled() {
            return None;
        }
        let raw = query.trim_start();
        let (typed_keyword, argument) = raw.split_once(' ')?;
        let keyword = self.config.keyword();
        if !typed_keyword.eq_ignore_ascii_case(&keyword) {
            return None;
        }
        let prefix = format!("{keyword} ");
        Some(SearchEngineSession {
            id: SESSION_ID.into(),
            title: tr(&self.language, "Search files"),
            placeholder: tr(&self.language, "Search files"),
            prefix,
            argument: argument.into(),
            mode: SearchEngineMode::Search,
            state: Default::default(),
        })
    }

    fn query_root(&self, query: &str) -> Vec<SearchResult> {
        if !self.config.enabled() {
            return Vec::new();
        }
        vec![entry_result(
            &self.language,
            query,
            self.config.backend(),
            &self.config.keyword(),
        )]
    }

    fn query_active(&self, session: &SearchEngineSession, query: &str) -> Vec<SearchResult> {
        if !self.owns_session(session) || query.trim().is_empty() || !self.config.enabled() {
            return Vec::new();
        }
        self.query_page(query, 0)
    }

    fn query_active_page(
        &self,
        session: &SearchEngineSession,
        query: &str,
        offset: usize,
    ) -> Vec<SearchResult> {
        if !self.owns_session(session) || query.trim().is_empty() || !self.config.enabled() {
            return Vec::new();
        }
        self.query_page(query, offset)
    }

    fn owns_session(&self, session: &SearchEngineSession) -> bool {
        session.id == SESSION_ID
    }

    fn shortcut_hint(&self, result_id: &str) -> Option<String> {
        (result_id == ENTRY_ID).then(|| format!("{} ", self.config.keyword()))
    }

    fn search_engine_result(&self, session: &SearchEngineSession) -> Option<SearchResult> {
        self.owns_session(session)
            .then(|| {
                entry_result(
                    &self.language,
                    "",
                    self.config.backend(),
                    &self.config.keyword(),
                )
            })
    }

    fn empty_state(
        &self,
        context: &crate::page::SearchContext,
        _query: &str,
    ) -> Option<SearchResult> {
        let session = context.session()?;
        self.owns_session(session)
            .then(|| {
                empty_state_result(
                    &self.language,
                    file_search::file_search_backend(self.config.backend()),
                )
            })
    }

    fn set_language(&mut self, language: String) {
        self.language = language;
    }
}

impl FileSearchProvider {
    fn query_page(&self, query: &str, offset: usize) -> Vec<SearchResult> {
        let Some(generation) = &self.query_generation else {
            return Vec::new();
        };
        let expected = generation.load(Ordering::Acquire);
        let cancelled = || (generation.load(Ordering::Acquire) != expected).then_some("file search query cancelled");
        match file_search::query_file_search(
            query,
            offset,
            PAGE_SIZE,
            self.config.backend(),
            &cancelled,
        ) {
            Ok(rows) => rows
                .into_iter()
                .map(|(name, path)| file_result(name, path))
                .collect(),
            Err(error) => {
                if cancelled().is_none() {
                    eprintln!("[file-search] query failed: {error:#}");
                }
                Vec::new()
            }
        }
    }
}

fn entry_result(language: &str, query: &str, preference: &str, keyword: &str) -> SearchResult {
    let backend = file_search::file_search_backend(preference);
    let title = backend_title(language, backend);
    let empty = query.trim().is_empty();
    let source = SourceId::Custom("File Search".into());
    SearchResult {
        id: ENTRY_ID.into(),
        identity: ENTRY_ID.into(),
        title: if empty {
            title
        } else {
            format!("{title} \"{query}\"")
        },
        category: ResultCategory::Other,
        metadata: Default::default(),
        aliases: vec![keyword.into(), "Everything".into(), "File Search".into(), "文件搜索".into()],
        location: backend_description(language, backend),
        launch_target: Default::default(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: source.clone(),
        sources: vec![source],
        launch_entries: Vec::new(),
        actions: vec![Action::EnterSearchEngine { prefix: format!("{keyword} ") }],
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: backend_icon(backend),
        ranking_bias: if empty { 0 } else { -1000 },
        allow_favorite: true,
        visibility: if empty { ResultVisibility::EmptyQueryOnly } else { ResultVisibility::Always },
    }
}

fn file_result(name: String, path: String) -> SearchResult {
    let id = format!("plugin:everything:file:{path}");
    let source = SourceId::Custom("File Search".into());
    SearchResult {
        id: id.clone(),
        identity: id,
        title: name.clone(),
        category: ResultCategory::Document,
        metadata: Default::default(),
        aliases: vec![name],
        location: path.clone(),
        launch_target: path.clone().into(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: source.clone(),
        sources: vec![source],
        launch_entries: Vec::new(),
        actions: vec![Action::Launch(crate::result::LaunchEntry {
            source: SourceId::Custom("File Search".into()),
            path: path.clone().into(),
            target: path.clone().into(),
            arguments: String::new(),
            working_directory: None,
            icon: Some(IconSpec::System(path.clone().into())),
        })],
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: Some(IconSpec::System(path.into())),
        ranking_bias: 0,
        allow_favorite: true,
        visibility: ResultVisibility::Always,
    }
}

fn empty_state_result(language: &str, backend: &str) -> SearchResult {
    let title = backend_title(language, backend);
    let description = backend_description(language, backend);
    SearchResult {
        id: "plugin:everything:search:empty-state".into(),
        identity: "plugin:everything:search:empty-state".into(),
        title,
        category: ResultCategory::Other,
        metadata: Default::default(),
        aliases: Vec::new(),
        location: String::new(),
        launch_target: Default::default(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: SourceId::Custom("File Search".into()),
        sources: vec![SourceId::Custom("File Search".into())],
        launch_entries: Vec::new(),
        actions: Vec::new(),
        detail: None,
        description: Some(description),
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: backend_icon(backend),
        ranking_bias: 0,
        allow_favorite: false,
        visibility: ResultVisibility::Always,
    }
}

fn backend_title(language: &str, backend: &str) -> String {
    match backend {
        "everything" => tr(language, "Search with Everything"),
        "windows-index" => tr(language, "Search files"),
        "everything-unavailable" => tr(language, "Everything is unavailable"),
        _ => tr(language, "File search is available on Windows"),
    }
}

fn backend_description(language: &str, backend: &str) -> String {
    match backend {
        "everything" => tr(language, "Search files with Everything; all Everything syntax is supported"),
        "windows-index" => tr(language, "Search file names in locations included in the Windows Search index"),
        "everything-unavailable" => tr(language, "Start Everything or choose Automatic or Windows Search in settings"),
        _ => tr(language, "File search is available on Windows"),
    }
}

fn backend_icon(backend: &str) -> Option<IconSpec> {
    if backend == "everything" {
        if let Some(target) = file_search::everything_icon_target() {
            return Some(IconSpec::System(target.into()));
        }
    }
    Some(IconSpec::Named("folder-open".into()))
}

fn tr(language: &str, message: &'static str) -> String {
    crate::utils::i18n::tr(language, message).to_owned()
}
