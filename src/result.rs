#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Launch using this specific discovered entry, including its arguments.
    Launch(LaunchEntry),
    /// An action implemented by a script plugin.
    Plugin {
        plugin_id: String,
        action_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCategory {
    /// Provider has not supplied a semantic category yet.
    Unclassified,
    Application,
    Game,
    Website,
    ControlPanel,
    Settings,
    Document,
    Other,
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
    pub icon: Option<IconSpec>,
    /// Provider-supplied ranking adjustment consumed generically by SearchCore.
    pub ranking_bias: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconSpec {
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
}
