use super::{
    build_result, configured_description, expand_raw_template, result_id, ShortcutQuery,
    ShortcutSpec,
};
use crate::{config::ShortcutConfig, result::SearchResult};
use std::path::PathBuf;

/// Render a configured application shortcut.
pub(super) fn configured(
    query: ShortcutQuery<'_>,
    shortcut: &ShortcutConfig,
    index: usize,
) -> Option<SearchResult> {
    let argument = query.resolve_argument(&shortcut.prefix);
    let preview = argument.explicit && argument.value.is_empty();
    let target = PathBuf::from(expand_raw_template(&shortcut.target, argument.value));
    let launch_arguments = expand_raw_template(&shortcut.arguments, argument.value);

    Some(build_result(
        query,
        ShortcutSpec {
            id: result_id(index, &shortcut.prefix),
            name: shortcut.name.clone(),
            prefix: shortcut.prefix.clone(),
            argument: argument.value.to_owned(),
            display_argument: argument.value.to_owned(),
            location: configured_description(query, shortcut),
            target,
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

    #[test]
    fn expands_raw_application_arguments() {
        let shortcut = ShortcutConfig {
            name: "Notepad".into(),
            prefix: "n".into(),
            placeholder: String::new(),
            shortcut_type: "application".into(),
            mode: "execute".into(),
            template: String::new(),
            target: "notepad.exe".into(),
            arguments: "{query}".into(),
            description: String::new(),
        };
        let item = configured(
            ShortcutQuery {
                raw: "n notes.txt",
                text: "n notes.txt".trim(),
                language: "en",
            },
            &shortcut,
            2,
        )
        .expect("application shortcut result");
        assert_eq!(item.launch_target, PathBuf::from("notepad.exe"));
        assert_eq!(item.launch_arguments, "notes.txt");
    }
}
