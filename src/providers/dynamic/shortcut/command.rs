#[cfg(test)]
use super::COMMAND_PREFIX;
use super::{
    build_result, configured_description, expand_raw_template, join_command_and_arguments,
    result_id, ShortcutQuery, ShortcutSpec,
};
use crate::{config::ShortcutConfig, platform, result::SearchResult};

/// Render the built-in `> command` shortcut.
#[cfg(test)]
pub(super) fn builtin(query: ShortcutQuery<'_>) -> Option<SearchResult> {
    builtin_with_prefix(query, COMMAND_PREFIX)
}

pub(super) fn builtin_with_prefix(query: ShortcutQuery<'_>, prefix: &str) -> Option<SearchResult> {
    let argument = query.resolve_argument(prefix);
    let shell = platform::command_shell();
    Some(build_result(
        query,
        ShortcutSpec {
            id: result_id(0, prefix),
            name: crate::utils::i18n::tr(query.language, "Execute Command").into(),
            prefix: prefix.into(),
            argument: argument.value.into(),
            display_argument: argument.value.into(),
            location: format!(
                "{} ({})",
                crate::utils::i18n::tr(query.language, "Run a command using your default shell"),
                shell.label
            ),
            target: shell.path,
            launch_arguments: platform::command_shell_arguments(argument.value),
            preview: argument.explicit && argument.value.is_empty(),
            include_query_alias: argument.explicit,
            icon: Some(crate::result::IconSpec::Named("console-line".into())),
        },
    ))
}

/// Render a configured command shortcut. A configured `>` shortcut retains
/// the built-in behavior for backwards compatibility.
#[cfg(test)]
pub(super) fn configured(
    query: ShortcutQuery<'_>,
    shortcut: &ShortcutConfig,
    index: usize,
) -> Option<SearchResult> {
    configured_with_builtin(query, shortcut, index, COMMAND_PREFIX)
}

pub(super) fn configured_with_builtin(
    query: ShortcutQuery<'_>,
    shortcut: &ShortcutConfig,
    index: usize,
    builtin_prefix: &str,
) -> Option<SearchResult> {
    let argument = query.resolve_argument(&shortcut.prefix);
    let preview = argument.explicit && argument.value.is_empty();
    let command = if shortcut
        .prefix
        .trim_end()
        .eq_ignore_ascii_case(builtin_prefix.trim_end())
    {
        argument.value.to_owned()
    } else {
        join_command_and_arguments(
            &expand_raw_template(&shortcut.target, argument.value),
            &expand_raw_template(&shortcut.arguments, argument.value),
        )
    };
    let shell = platform::command_shell();
    let launch_arguments = platform::command_shell_arguments(&command);

    Some(build_result(
        query,
        ShortcutSpec {
            id: result_id(index, &shortcut.prefix),
            name: shortcut.name.clone(),
            prefix: shortcut.prefix.clone(),
            argument: argument.value.to_owned(),
            display_argument: argument.value.to_owned(),
            location: configured_description(query, shortcut),
            target: shell.path,
            launch_arguments,
            preview,
            include_query_alias: argument.explicit,
            icon: Some(crate::result::IconSpec::Named("console-line".into())),
        },
    ))
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
    fn supports_explicit_and_fallback_commands() {
        let explicit = builtin(query("> ipconfig")).expect("explicit command");
        assert_eq!(explicit.title, "Execute Command \"ipconfig\"");
        assert_eq!(
            explicit.launch_arguments,
            "-NoLogo -NoExit -Command \"ipconfig\""
        );
        assert_eq!(explicit.category, crate::result::ResultCategory::Command);

        let fallback = builtin(query("ipconfig")).expect("fallback command");
        assert_eq!(fallback.title, "Execute Command \"ipconfig\"");
        assert!(fallback.ranking_bias < 0);
    }

    #[test]
    fn explicit_prefix_without_argument_is_a_preview() {
        let preview = builtin(query("> ")).expect("command preview");
        assert_eq!(preview.title, "Execute Command");
        assert!(preview.actions.is_empty());
    }

    #[test]
    fn prefix_matching_does_not_add_a_separator_requirement() {
        let compact = super::super::shortcut_argument(">ipconfig", ">")
            .expect("prefix should be matched literally");
        assert_eq!(compact, "ipconfig");
    }

    #[test]
    fn configured_command_uses_shell_with_arguments() {
        let shortcut = ShortcutConfig {
            name: "Git status".into(),
            prefix: "gs".into(),
            placeholder: String::new(),
            shortcut_type: "command".into(),
            mode: "execute".into(),
            template: String::new(),
            target: "git".into(),
            arguments: "status --short {query}".into(),
            description: String::new(),
        };
        let item = configured(query("gs wowie"), &shortcut, 2).expect("configured command");
        assert!(item.launch_arguments.contains("git status --short wowie"));
        assert!(matches!(
            item.actions.first(),
            Some(crate::result::Action::Launch(_))
        ));
    }
}
