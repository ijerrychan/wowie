#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Replace the current query with a shortcut-prefixed engine query.
    EnterSearchEngine { prefix: String },
    /// Launch using this specific discovered entry, including its arguments.
    Launch(LaunchEntry),
    /// An action implemented by a script plugin.
    Plugin {
        plugin_id: String,
        action_id: String,
        icon: Option<IconSpec>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCategory {
    /// Provider has not supplied a semantic category yet.
    Unclassified,
    Application,
    Game,
    Website,
    Calculator,
    ControlPanel,
    Document,
    Other,
}

/// A renderer-neutral recommendation for presenting a result's content.
///
/// The launcher owns the final list/grid layout and navigation behavior. This
/// value is reserved for content hints such as a detail preview, so changing
/// the global view mode never depends on a provider category.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultPresentation {
    List,
    Compact,
    Preview,
    Grid,
}

impl ResultPresentation {
    pub const fn recommended(has_detail: bool) -> Self {
        if has_detail {
            return Self::Preview;
        }
        Self::List
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Compact => "compact",
            Self::Preview => "preview",
            Self::Grid => "grid",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchEntry {
    pub source: crate::platform::SourceId,
    pub path: std::path::PathBuf,
    pub target: std::path::PathBuf,
    pub arguments: String,
    pub working_directory: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Stable identity used for merging records from different providers.
    pub id: String,
    pub identity: String,
    pub title: String,
    pub category: ResultCategory,
    pub aliases: Vec<String>,
    pub location: String,
    pub launch_target: std::path::PathBuf,
    pub launch_arguments: String,
    pub launch_working_directory: Option<std::path::PathBuf>,
    pub source: crate::platform::SourceId,
    /// All discovery sources that contributed this application entry.
    pub sources: Vec<crate::platform::SourceId>,
    pub launch_entries: Vec<LaunchEntry>,
    pub actions: Vec<Action>,
    /// Optional declarative content rendered in the launcher's detail pane.
    pub detail: Option<DetailView>,
    /// Optional user-facing application description collected during indexing.
    pub description: Option<String>,
    /// Windows AppUserModelId for packaged applications, when available.
    pub app_user_model_id: Option<String>,
    /// Windows PackageFamilyName for packaged applications, when available.
    pub package_family_name: Option<String>,
    /// Additional package identity metadata, when the package catalog exposes it.
    pub package_metadata: Option<crate::platform::PackageMetadata>,
    pub icon: Option<IconSpec>,
    /// Provider-supplied ranking adjustment consumed generically by SearchCore.
    pub ranking_bias: i32,
}

impl SearchResult {
    /// Return the provider-independent presentation recommendation consumed by
    /// the UI. The global list/grid mode is controlled by the launcher.
    pub fn recommended_presentation(&self) -> ResultPresentation {
        ResultPresentation::recommended(self.detail.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailView {
    pub title: String,
    pub subtitle: String,
    pub blocks: Vec<DetailBlock>,
    pub actions: Vec<DetailAction>,
    pub source: String,
    pub preferred_width: Option<i32>,
    pub scrollable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailAction {
    pub id: String,
    pub title: String,
    pub icon: Option<IconSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailBlock {
    Text {
        text: String,
        color: String,
    },
    Metric {
        label: String,
        value: String,
        color: String,
    },
    Icon {
        glyph: String,
        color: String,
        icon: Option<IconSpec>,
    },
    Weather {
        value: String,
    },
    Markdown {
        markdown: String,
        color: String,
    },
    Image {
        source: String,
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
        message: String,
    },
    Error {
        message: String,
        retry_action: Option<String>,
    },
    Separator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconSpec {
    /// A built-in colored weather glyph used by the weather shortcut.
    Weather,
    /// A built-in calculator glyph.
    Calculator,
    /// A plugin-owned PNG/ICO resource resolved to an absolute path.
    Resource(std::path::PathBuf),
    /// A path or shell target whose icon is provided by the operating system.
    System(std::path::PathBuf),
    SystemIndexed {
        path: std::path::PathBuf,
        index: i32,
    },
    /// The icon associated with opening this file type in the system default
    /// application. The cache can safely share it across files with the same
    /// extension.
    DefaultOpen(std::path::PathBuf),
    /// A semantic host icon name, resolved by the shared icon service.
    Named(String),
    /// A host-fetched HTTPS image, normally supplied by a plugin.
    Remote(String),
}

/// A plugin-facing icon reference. It is resolved into an `IconSpec` only at
/// the host boundary, so commands and results share one protocol type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IconRef {
    Resource { path: String },
    System { target: String },
    Named { id: String },
    Remote { url: String },
}

#[cfg(test)]
mod tests {
    use super::ResultPresentation;

    #[test]
    fn presentation_recommendations_are_provider_independent() {
        assert_eq!(
            ResultPresentation::recommended(false),
            ResultPresentation::List
        );
        assert_eq!(
            ResultPresentation::recommended(false),
            ResultPresentation::List
        );
        assert_eq!(
            ResultPresentation::recommended(true),
            ResultPresentation::Preview
        );
        assert_eq!(
            ResultPresentation::recommended(false),
            ResultPresentation::List
        );
    }

    #[test]
    fn presentation_names_are_stable_for_the_ui_boundary() {
        assert_eq!(ResultPresentation::List.as_str(), "list");
        assert_eq!(ResultPresentation::Compact.as_str(), "compact");
        assert_eq!(ResultPresentation::Preview.as_str(), "preview");
        assert_eq!(ResultPresentation::Grid.as_str(), "grid");
    }
}
