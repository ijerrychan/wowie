#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Launch using this specific discovered entry, including its arguments.
    Launch(LaunchEntry),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchEntry {
    pub source: String,
    pub path: std::path::PathBuf,
    pub target: std::path::PathBuf,
    pub arguments: String,
    pub working_directory: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Stable identity used for merging records from different providers.
    pub id: String,
    pub identity: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub location: String,
    pub launch_target: std::path::PathBuf,
    pub launch_arguments: String,
    pub launch_working_directory: Option<std::path::PathBuf>,
    pub source: String,
    /// All discovery sources that contributed this application entry.
    pub sources: Vec<String>,
    pub launch_entries: Vec<LaunchEntry>,
    pub actions: Vec<Action>,
}
