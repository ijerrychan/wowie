use super::{
    build_result, expand_raw_template, join_command_and_arguments, result_id, ShortcutQuery,
    ShortcutSpec, COMMAND_PREFIX,
};
use crate::{config::ShortcutConfig, platform, result::SearchResult};

/// Render the built-in `> command` shortcut.
pub(super) fn builtin(query: ShortcutQuery<'_>) -> Option<SearchResult> {
    let argument = query.resolve_argument(COMMAND_PREFIX);
    let shell = platform::command_shell();
    Some(build_result(
        query,
        ShortcutSpec {
            id: result_id(0, COMMAND_PREFIX),
            name: crate::utils::i18n::tr(query.language, "Execute Command").into(),
            prefix: COMMAND_PREFIX.into(),
            argument: argument.value.into(),
            display_argument: argument.value.into(),
            location: shell.label,
            target: shell.path,
            launch_arguments: platform::command_shell_arguments(argument.value),
            preview: argument.explicit && argument.value.is_empty(),
            include_query_alias: argument.explicit,
            icon: None,
        },
    ))
}

/// Render a configured command shortcut. A configured `>` shortcut retains
/// the built-in behavior for backwards compatibility.
pub(super) fn configured(
    query: ShortcutQuery<'_>,
    shortcut: &ShortcutConfig,
    index: usize,
) -> Option<SearchResult> {
    let argument = query.resolve_argument(&shortcut.prefix);
    let preview = argument.explicit && argument.value.is_empty();
    let command = if shortcut.prefix.trim_end() == COMMAND_PREFIX.trim_end() {
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
            location: shell.label,
            target: shell.path,
            launch_arguments,
            preview,
            include_query_alias: argument.explicit,
            icon: None,
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
    fn supports_explicit_and_implicit_commands() {
        let explicit = builtin(query("> ipconfig")).expect("explicit command");
        assert_eq!(explicit.title, "Execute Command \"ipconfig\"");
        assert_eq!(
            explicit.launch_arguments,
            "-NoLogo -NoExit -Command \"ipconfig\""
        );
        assert_eq!(explicit.category, crate::result::ResultCategory::Shortcut);

        let implicit = builtin(query("ipconfig")).expect("implicit command");
        assert_eq!(implicit.title, "Execute Command \"ipconfig\"");
        assert_eq!(implicit.ranking_bias, 0);
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
            shortcut_type: "command".into(),
            template: String::new(),
            target: "git".into(),
            arguments: "status --short {query}".into(),
        };
        let item = configured(query("gs wowie"), &shortcut, 2).expect("configured command");
        assert!(item.launch_arguments.contains("git status --short wowie"));
        assert!(matches!(
            item.actions.first(),
            Some(crate::result::Action::Launch(_))
        ));
    }
}
