//! Renderer-neutral page/session state.
//!
//! Search results describe content. Page state describes which interaction
//! owns the current query and how the launcher should interpret navigation.

/// The interaction contract of a search engine entered through a shortcut.
///
/// A shortcut is only the syntax that selects this engine. It is not a
/// result category and does not describe a row in the result list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchEngineMode {
    Execute,
    Search,
    Panel,
}

impl SearchEngineMode {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "search" => Self::Search,
            "panel" => Self::Panel,
            _ => Self::Execute,
        }
    }
}

/// State for the currently active non-default search engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEngineSession {
    pub id: String,
    pub title: String,
    pub prefix: String,
    pub argument: String,
    pub mode: SearchEngineMode,
    /// Provider-defined ephemeral state for menu-like search sessions.
    pub state: std::collections::BTreeMap<String, String>,
}

/// The complete routing decision for one search request.
///
/// It is resolved once at the SearchCore boundary and then passed unchanged to
/// providers and the UI, avoiding independent shortcut parsing in each layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchContext {
    Default,
    Engine(SearchEngineSession),
}

impl SearchContext {
    pub fn session(&self) -> Option<&SearchEngineSession> {
        match self {
            Self::Default => None,
            Self::Engine(session) => Some(session),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_exposes_only_the_active_engine_session() {
        assert!(SearchContext::Default.session().is_none());
        let context = SearchContext::Engine(SearchEngineSession {
            id: "test:engine".into(),
            title: "Test".into(),
            prefix: "t ".into(),
            argument: "query".into(),
            mode: SearchEngineMode::Search,
            state: Default::default(),
        });
        assert_eq!(
            context.session().map(|session| session.id.as_str()),
            Some("test:engine")
        );
    }
}
