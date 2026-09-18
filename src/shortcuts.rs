use crate::{
    config::ShortcutConfig,
    platform::SourceId,
    result::{Action, ResultCategory, SearchResult},
};
use std::path::PathBuf;

const COMMAND_PREFIX: &str = ">";

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

/// Dynamic results generated from the current query rather than an index.
pub fn results(query: &str, configured: &[ShortcutConfig], language: &str) -> Vec<SearchResult> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let mut definitions = Vec::with_capacity(configured.len() + 1);
    definitions.push(ShortcutConfig {
        name: crate::i18n::execute_command_name(language).into(),
        prefix: COMMAND_PREFIX.into(),
        template: String::new(),
    });
    definitions.extend(configured.iter().cloned());

    definitions
        .into_iter()
        .enumerate()
        .filter_map(|(index, shortcut)| {
            let argument = shortcut_argument(query, &shortcut.prefix);
            let explicit = argument.is_some();
            let argument = argument.unwrap_or(query).trim();
            if argument.is_empty() {
                return None;
            }
            let target = if shortcut.prefix == COMMAND_PREFIX {
                command_shell()
            } else {
                PathBuf::from(expand_template(&shortcut.template, argument))
            };
            let launch_arguments = if shortcut.prefix == COMMAND_PREFIX {
                powershell_arguments(argument)
            } else {
                String::new()
            };
            let location = if shortcut.prefix == COMMAND_PREFIX {
                command_shell_label(&target)
            } else {
                target.display().to_string()
            };
            let title = format!("{} \"{}\"", shortcut.name, argument);
            let id = format!("shortcut:{}:{}", index, shortcut.prefix);
            let mut aliases = vec![format!("{} {}", shortcut.prefix, argument)];
            if explicit {
                aliases.push(query.to_string());
            }
            Some(SearchResult {
                id: id.clone(),
                identity: id,
                title,
                category: ResultCategory::Shortcut,
                aliases,
                location,
                launch_target: target,
                launch_arguments: launch_arguments.clone(),
                launch_working_directory: None,
                source: SourceId::Shortcut,
                sources: vec![SourceId::Shortcut],
                launch_entries: Vec::new(),
                actions: vec![Action::Launch(crate::result::LaunchEntry {
                    source: SourceId::Shortcut,
                    path: PathBuf::new(),
                    target: if shortcut.prefix == COMMAND_PREFIX {
                        command_shell()
                    } else {
                        PathBuf::from(expand_template(&shortcut.template, argument))
                    },
                    arguments: launch_arguments.clone(),
                    working_directory: None,
                })],
                icon: None,
                ranking_bias: if explicit { 0 } else { -400 },
            })
        })
        .collect()
}

fn shortcut_argument<'a>(query: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = query.strip_prefix(prefix)?;
    let trimmed = rest.trim_start();
    (trimmed.len() < rest.len()).then_some(trimmed)
}

fn expand_template(template: &str, query: &str) -> String {
    template.replace(
        "{query}",
        &url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>(),
    )
}

/// Resolve the user's likely command shell. The terminal application itself is
/// selected by Windows; launching PowerShell through ShellExecute lets Windows
/// host it in the configured default terminal (for example Windows Terminal).
fn command_shell() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path).find_map(|directory| {
                let candidate = directory.join("pwsh.exe");
                candidate.is_file().then_some(candidate)
            })
        }) {
            return path;
        }
        PathBuf::from("powershell.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        PathBuf::from("pwsh")
    }
}

fn powershell_arguments(command: &str) -> String {
    let escaped = command.replace('"', "\\\"");
    format!("-NoLogo -NoExit -Command \"{escaped}\"")
}

fn command_shell_label(target: &PathBuf) -> String {
    let name = target
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "pwsh" => "PowerShell 7".into(),
        "powershell" => "Windows PowerShell".into(),
        "cmd" => "cmd".into(),
        _ => target.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_shortcut_supports_explicit_and_implicit_queries() {
        let explicit = results("> ipconfig", &[], "zh-CN");
        assert_eq!(explicit[0].title, "执行命令 \"ipconfig\"");
        let shell = explicit[0]
            .launch_target
            .to_string_lossy()
            .to_ascii_lowercase();
        assert!(
            shell.ends_with("pwsh")
                || shell.ends_with("pwsh.exe")
                || shell.ends_with("powershell.exe")
        );
        assert_eq!(
            explicit[0].launch_arguments,
            "-NoLogo -NoExit -Command \"ipconfig\""
        );
        assert_eq!(explicit[0].location, "PowerShell 7");
        assert_eq!(explicit[0].category, ResultCategory::Shortcut);

        let implicit = results("ipconfig", &[], "zh-CN");
        assert_eq!(implicit[0].title, "执行命令 \"ipconfig\"");
        assert_eq!(implicit[0].ranking_bias, -400);
    }

    #[test]
    fn configured_shortcut_expands_and_encodes_query() {
        let shortcut = ShortcutConfig {
            name: "用谷歌搜索".into(),
            prefix: "g".into(),
            template: "https://www.google.com/search?q={query}".into(),
        };
        let item = results("g rust lang", &[shortcut], "en")
            .into_iter()
            .find(|item| item.title.starts_with("用谷歌搜索"))
            .expect("configured shortcut result");
        assert_eq!(
            item.launch_target,
            PathBuf::from("https://www.google.com/search?q=rust+lang")
        );
    }
}
