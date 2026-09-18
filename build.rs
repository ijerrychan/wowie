fn main() {
    println!("cargo:rerun-if-changed=lang");
    println!("cargo:rerun-if-changed=wowie.ico");
    println!("cargo:rerun-if-changed=wowie.png");
    let mut languages = std::fs::read_dir("lang")
        .expect("failed to read translation directory")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir())
                .map(|_| entry.file_name())
        })
        .filter_map(|name| name.into_string().ok())
        .collect::<Vec<_>>();
    languages.sort();
    let language_list = languages
        .iter()
        .map(|language| format!("{language:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let shortcut_translations = languages
        .iter()
        .map(|language| {
            let path = std::path::Path::new("lang")
                .join(language)
                .join("LC_MESSAGES")
                .join("wowie.po");
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let value = po_translation(&text, "Execute Command")
                .unwrap_or_else(|| "Execute Command".into());
            format!("({language:?}, {value:?})")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let exit_translations = languages
        .iter()
        .map(|language| {
            let path = std::path::Path::new("lang")
                .join(language)
                .join("LC_MESSAGES")
                .join("wowie.po");
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let value = po_translation(&text, "Exit").unwrap_or_else(|| "Exit".into());
            format!("({language:?}, {value:?})")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let open_website_translations = languages
        .iter()
        .map(|language| {
            let path = std::path::Path::new("lang")
                .join(language)
                .join("LC_MESSAGES")
                .join("wowie.po");
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let value =
                po_translation(&text, "Open website").unwrap_or_else(|| "Open website".into());
            format!("({language:?}, {value:?})")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let generated = format!(
        "pub const SUPPORTED_LANGUAGES: &[&str] = &[{language_list}];\n\
         pub const EXECUTE_COMMAND_TRANSLATIONS: &[(&str, &str)] = &[{shortcut_translations}];\n\
         pub const OPEN_WEBSITE_TRANSLATIONS: &[(&str, &str)] = &[{open_website_translations}];\n\
         pub const EXIT_TRANSLATIONS: &[(&str, &str)] = &[{exit_translations}];\n"
    );
    std::fs::write(
        std::path::Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"))
            .join("supported_languages.rs"),
        generated,
    )
    .expect("failed to write supported language list");

    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("wowie.ico")
        .compile()
        .expect("failed to compile Windows application icon");

    let config = slint_build::CompilerConfiguration::new().with_bundled_translations("lang");
    slint_build::compile_with_config("ui/launcher.slint", config)
        .expect("failed to compile Slint UI");
}

fn po_translation(text: &str, message_id: &str) -> Option<String> {
    let lines = text.lines().collect::<Vec<_>>();
    let message_id_line = format!("msgid {message_id:?}");
    let index = lines.iter().position(|line| *line == message_id_line)?;
    let message = lines.get(index + 1)?.strip_prefix("msgstr ")?;
    let message = message.strip_prefix('"')?.strip_suffix('"')?;
    Some(
        message
            .replace("\\\\", "\\")
            .replace("\\\"", "\"")
            .replace("\\n", "\n"),
    )
}
