use crate::{
    config::ShortcutConfig,
    platform::SourceId,
    result::{Action, IconSpec, ResultCategory, SearchResult},
};
use std::path::PathBuf;

const COMMAND_PREFIX: &str = ">";
const WEB_PREFIX: &str = "w";

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
    let input = query;
    let query = input.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let mut definitions = Vec::with_capacity(configured.len() + 2);
    definitions.push(ShortcutConfig {
        name: crate::i18n::execute_command_name(language).into(),
        prefix: COMMAND_PREFIX.into(),
        template: String::new(),
    });
    definitions.push(ShortcutConfig {
        name: crate::i18n::open_website_name(language).into(),
        prefix: WEB_PREFIX.into(),
        template: String::new(),
    });
    definitions.extend(configured.iter().cloned());

    definitions
        .into_iter()
        .enumerate()
        .filter_map(|(index, shortcut)| {
            let argument = shortcut_argument(input, &shortcut.prefix);
            let explicit = argument.is_some();
            let argument = argument.unwrap_or(query).trim();
            if argument.is_empty() && !explicit {
                return None;
            }
            let preview = explicit && argument.is_empty();
            let is_web = shortcut.prefix == WEB_PREFIX;
            let web_target = is_web.then(|| web_url(argument)).flatten();
            let web_target = web_target
                .or_else(|| (is_web && explicit && !preview).then(|| fallback_web_url(argument)));
            if is_web && !preview && !explicit && web_target.is_none() {
                return None;
            }
            let target = if let Some(url) = &web_target {
                PathBuf::from(url)
            } else if shortcut.prefix == COMMAND_PREFIX {
                command_shell()
            } else {
                PathBuf::from(expand_template(&shortcut.template, argument))
            };
            let launch_arguments = if shortcut.prefix == COMMAND_PREFIX {
                powershell_arguments(argument)
            } else {
                String::new()
            };
            let location = if is_web {
                web_target.clone().unwrap_or_default()
            } else if shortcut.prefix == COMMAND_PREFIX {
                command_shell_label(&target)
            } else {
                target.display().to_string()
            };
            let title = if preview {
                shortcut.name.clone()
            } else {
                let display_argument = web_target.as_deref().unwrap_or(argument);
                format!("{} \"{}\"", shortcut.name, display_argument)
            };
            let id = format!("shortcut:{}:{}", index, shortcut.prefix);
            let mut aliases = vec![format!("{} {}", shortcut.prefix, argument)];
            if explicit || web_target.is_some() {
                aliases.push(query.to_string());
            }
            Some(SearchResult {
                id: id.clone(),
                identity: id,
                title,
                category: ResultCategory::Shortcut,
                aliases,
                location,
                launch_target: target.clone(),
                launch_arguments: launch_arguments.clone(),
                launch_working_directory: None,
                source: SourceId::Shortcut,
                sources: vec![SourceId::Shortcut],
                launch_entries: Vec::new(),
                actions: if preview {
                    Vec::new()
                } else {
                    vec![Action::Launch(crate::result::LaunchEntry {
                        source: SourceId::Shortcut,
                        path: PathBuf::new(),
                        target,
                        arguments: launch_arguments.clone(),
                        working_directory: None,
                    })]
                },
                icon: if is_web {
                    Some(IconSpec::DefaultOpen(PathBuf::from(
                        web_target.as_deref().unwrap_or("https://example.com"),
                    )))
                } else {
                    None
                },
                ranking_bias: 0,
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

fn web_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return None;
    }
    let candidate = if value.contains("://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    let parsed = url::Url::parse(&candidate).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    let host = parsed.host_str()?;
    let obvious_host = host.eq_ignore_ascii_case("localhost") || is_valid_domain(host);
    obvious_host.then(|| parsed.to_string())
}

fn is_valid_domain(host: &str) -> bool {
    let labels = host.trim_end_matches('.').split('.').collect::<Vec<_>>();
    if labels.len() < 2
        || labels.iter().any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
    {
        return false;
    }
    let top_level = labels.last().expect("domain has at least two labels");
    top_level.len() >= 2
        && top_level
            .chars()
            .any(|character| character.is_ascii_alphabetic())
        && labels
            .iter()
            .any(|label| label.chars().any(|character| !character.is_ascii_digit()))
}

fn fallback_web_url(value: &str) -> String {
    if value.starts_with("http://") || value.starts_with("https://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    }
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
        assert_eq!(implicit[0].ranking_bias, 0);
    }

    #[test]
    fn explicit_prefix_without_argument_returns_a_non_executable_preview() {
        let preview = results("> ", &[], "zh-CN");
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].title, "执行命令");
        assert!(preview[0].actions.is_empty());
        assert_eq!(preview[0].ranking_bias, 0);
    }

    #[test]
    fn web_shortcut_opens_explicit_and_implicit_urls() {
        let explicit = results("w example.com/docs?q=rust", &[], "en");
        let explicit = explicit
            .into_iter()
            .find(|item| item.title.starts_with("Open website"))
            .expect("explicit web shortcut");
        assert_eq!(
            explicit.launch_target,
            PathBuf::from("https://example.com/docs?q=rust")
        );
        assert_eq!(
            explicit.title,
            "Open website \"https://example.com/docs?q=rust\""
        );
        assert!(matches!(explicit.icon, Some(IconSpec::DefaultOpen(_))));

        let implicit = results("https://example.com/docs", &[], "en");
        let implicit = implicit
            .into_iter()
            .find(|item| item.title.starts_with("Open website"))
            .expect("implicit web shortcut");
        assert_eq!(
            implicit.launch_target,
            PathBuf::from("https://example.com/docs")
        );
        assert!(matches!(implicit.icon, Some(IconSpec::DefaultOpen(_))));
        assert_eq!(implicit.ranking_bias, 0);
    }

    #[test]
    fn web_shortcut_ignores_plain_text() {
        for input in ["notepad", "123", "12345", "192.168", "999.999.999.999"] {
            assert!(results(input, &[], "en")
                .into_iter()
                .all(|item| item.launch_target.to_string_lossy() != format!("https://{input}")));
        }
    }

    #[test]
    fn web_shortcut_requires_explicit_prefix_for_ip_addresses() {
        let implicit_ipv4 = results("192.168.1.10", &[], "en")
            .into_iter()
            .any(|item| item.title.starts_with("Open website"));
        assert!(!implicit_ipv4);

        let implicit_ipv6 = results("[::1]", &[], "en")
            .into_iter()
            .any(|item| item.title.starts_with("Open website"));
        assert!(!implicit_ipv6);

        let explicit = results("w 192.168.1.10", &[], "en")
            .into_iter()
            .find(|item| item.title.starts_with("Open website"))
            .expect("explicit IP web shortcut");
        assert_eq!(
            explicit.launch_target,
            PathBuf::from("https://192.168.1.10")
        );
    }

    #[test]
    fn web_prefix_preview_uses_the_default_browser_icon() {
        let preview = results("w ", &[], "en")
            .into_iter()
            .find(|item| item.title == "Open website")
            .expect("web preview");
        assert!(preview.actions.is_empty());
        assert!(matches!(preview.icon, Some(IconSpec::DefaultOpen(_))));
    }

    #[test]
    fn explicit_invalid_web_input_still_has_a_consistent_action() {
        let result = results("w xxx", &[], "en")
            .into_iter()
            .find(|item| item.title.starts_with("Open website"))
            .expect("explicit web action");
        assert_eq!(result.launch_target, PathBuf::from("https://xxx"));
        assert!(!result.actions.is_empty());
        assert!(matches!(result.icon, Some(IconSpec::DefaultOpen(_))));
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
