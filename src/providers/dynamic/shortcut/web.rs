use super::{
    build_result, configured_description, expand_url_template, result_id, ShortcutQuery,
    ShortcutSpec, WEB_PREFIX,
};
use crate::{
    config::ShortcutConfig,
    result::{IconSpec, SearchResult},
};
use std::path::PathBuf;

/// Render the built-in `w address` shortcut and implicit URL matches.
pub(super) fn builtin(query: ShortcutQuery<'_>) -> Option<SearchResult> {
    render(
        query,
        crate::utils::i18n::tr(query.language, "Open website").into(),
        WEB_PREFIX,
        String::new(),
        None,
        1,
    )
}

/// Render a configured URL shortcut. The `w` prefix intentionally keeps the
/// built-in URL behavior even if a config entry uses it.
pub(super) fn configured(
    query: ShortcutQuery<'_>,
    shortcut: &ShortcutConfig,
    index: usize,
) -> Option<SearchResult> {
    render(
        query,
        shortcut.name.clone(),
        &shortcut.prefix,
        shortcut.template.clone(),
        Some(shortcut),
        index,
    )
}

fn render(
    query: ShortcutQuery<'_>,
    name: String,
    prefix: &str,
    template: String,
    shortcut: Option<&ShortcutConfig>,
    index: usize,
) -> Option<SearchResult> {
    let argument = query.resolve_argument(prefix);
    let preview = argument.explicit && argument.value.is_empty();
    let is_builtin_web = prefix.trim_end() == WEB_PREFIX.trim_end();
    let web_target = if is_builtin_web {
        web_url(argument.value)
            .or_else(|| (argument.explicit && !preview).then(|| fallback_web_url(argument.value)))
    } else {
        None
    };
    let target_value = web_target
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if is_builtin_web {
                fallback_web_url(argument.value)
            } else {
                expand_url_template(&template, argument.value)
            }
        });
    let target = PathBuf::from(target_value);
    let location = if is_builtin_web {
        crate::utils::i18n::tr(query.language, "Visit the internet with your default browser").into()
    } else {
        configured_description(query, shortcut.expect("configured shortcut must be present"))
    };
    let display_argument = web_target
        .clone()
        .unwrap_or_else(|| argument.value.to_owned());
    let icon = is_builtin_web.then(|| {
        IconSpec::DefaultOpen(PathBuf::from(
            web_target.as_deref().unwrap_or("https://example.com"),
        ))
    });

    Some(build_result(
        query,
        ShortcutSpec {
            id: result_id(index, prefix),
            name,
            prefix: prefix.into(),
            argument: argument.value.into(),
            display_argument,
            location,
            target,
            launch_arguments: String::new(),
            preview,
            include_query_alias: argument.explicit || web_target.is_some(),
            icon,
        },
    ))
}

/// Locate a user-entered web address with linkify, then use the URL parser
/// only to normalize the recognized HTTP/HTTPS URL.
fn web_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return None;
    }

    let mut finder = linkify::LinkFinder::new();
    finder
        .kinds(&[linkify::LinkKind::Url])
        .url_must_have_scheme(false)
        .url_can_be_iri(true);
    let candidate = finder.links(value).next().and_then(|link| {
        (link.start() == 0 && link.end() == value.len()).then(|| {
            if link.as_str().starts_with("http://") || link.as_str().starts_with("https://") {
                link.as_str().to_owned()
            } else {
                format!("https://{}", link.as_str())
            }
        })
    });
    let candidate = candidate?;
    let parsed = url::Url::parse(&candidate).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return None;
    }
    Some(parsed.to_string())
}

fn fallback_web_url(value: &str) -> String {
    if value.starts_with("http://") || value.starts_with("https://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(raw: &str) -> ShortcutQuery<'_> {
        ShortcutQuery {
            raw,
            text: raw.trim(),
            language: "en",
        }
    }

    #[test]
    fn opens_explicit_and_implicit_urls() {
        let explicit = builtin(query("w example.com/docs?q=rust")).expect("explicit web");
        assert_eq!(
            explicit.launch_target,
            PathBuf::from("https://example.com/docs?q=rust")
        );
        assert_eq!(
            explicit.title,
            "Open website \"https://example.com/docs?q=rust\""
        );
        assert!(matches!(explicit.icon, Some(IconSpec::DefaultOpen(_))));

        let implicit = builtin(query("https://example.com/docs")).expect("implicit web");
        assert_eq!(
            implicit.launch_target,
            PathBuf::from("https://example.com/docs")
        );
        assert!(matches!(implicit.icon, Some(IconSpec::DefaultOpen(_))));
    }

    #[test]
    fn plain_text_is_a_low_priority_web_fallback() {
        for input in ["notepad", "123", "12345", "192.168", "999.999.999.999"] {
            let item = builtin(query(input)).expect("web fallback");
            assert_eq!(item.ranking_bias, -1000);
            assert!(item.launch_target.to_string_lossy().starts_with("https://"));
        }
    }

    #[test]
    fn follows_linkify_ip_support() {
        assert!(builtin(query("192.168.1.10")).is_some());
        let fallback = builtin(query("[::1]")).expect("web fallback");
        assert_eq!(fallback.ranking_bias, -1000);
    }

    #[test]
    fn supports_ported_hosts() {
        let ported = builtin(query("example.com:8443/path")).expect("ported host");
        assert_eq!(
            ported.launch_target,
            PathBuf::from("https://example.com:8443/path")
        );
    }

    #[test]
    fn explicit_invalid_input_still_has_a_consistent_action() {
        let invalid = builtin(query("w xxx")).expect("explicit web action");
        assert_eq!(invalid.launch_target, PathBuf::from("https://xxx"));
        assert!(!invalid.actions.is_empty());
    }

    #[test]
    fn preview_uses_the_default_browser_icon() {
        let preview = builtin(query("w ")).expect("web preview");
        assert_eq!(preview.title, "Open website");
        assert_eq!(preview.location, "Visit the internet with your default browser");
        assert!(preview.actions.is_empty());
        assert!(matches!(preview.icon, Some(IconSpec::DefaultOpen(_))));
    }

    #[test]
    fn configured_url_expands_and_encodes_query() {
        let shortcut = ShortcutConfig {
            name: "用谷歌搜索".into(),
            prefix: "g".into(),
            placeholder: String::new(),
            shortcut_type: "url".into(),
            mode: "execute".into(),
            template: "https://www.google.com/search?q={query}".into(),
            target: String::new(),
            arguments: String::new(),
            description: String::new(),
        };
        let item = configured(query("g rust lang"), &shortcut, 2).expect("configured URL");
        assert_eq!(
            item.launch_target,
            PathBuf::from("https://www.google.com/search?q=rust+lang")
        );
        assert_eq!(item.location, "Custom shortcut");
    }

    #[test]
    fn configured_url_uses_its_description_instead_of_the_generated_url() {
        let shortcut = ShortcutConfig {
            name: "Search docs".into(),
            prefix: "d".into(),
            placeholder: String::new(),
            shortcut_type: "url".into(),
            mode: "execute".into(),
            template: "https://example.com/?q={query}".into(),
            target: String::new(),
            arguments: String::new(),
            description: "Search documentation for {query}".into(),
        };
        let item = configured(query("d engine settings"), &shortcut, 2).expect("configured URL");
        assert_eq!(item.location, "Search documentation for engine settings");
    }
}
