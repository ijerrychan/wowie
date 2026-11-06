fn main() {
    println!("cargo:rerun-if-changed=src/providers/dynamic");
    println!("cargo:rerun-if-changed=src/details");
    println!("cargo:rerun-if-changed=lang");
    println!("cargo:rerun-if-changed=wowie.ico");
    println!("cargo:rerun-if-changed=wowie.png");
    println!("cargo:rerun-if-changed=ui/settings.slint");
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
    let translations = languages
        .iter()
        .flat_map(|language| {
            let path = std::path::Path::new("lang")
                .join(language)
                .join("LC_MESSAGES")
                .join("wowie.po");
            let text = std::fs::read_to_string(path).unwrap_or_default();
            po_entries(&text)
                .into_iter()
                .map(move |(context, message_id, value)| {
                    format!("({language:?}, {context:?}, {message_id:?}, {value:?})")
                })
        })
        .collect::<Vec<_>>()
        .join(", ");
    let generated = format!(
        "pub const SUPPORTED_LANGUAGES: &[&str] = &[{language_list}];\n\
         pub const TRANSLATIONS: &[(&str, &str, &str, &str)] = &[{translations}];\n"
    );
    std::fs::write(
        std::path::Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"))
            .join("supported_languages.rs"),
        generated,
    )
    .expect("failed to write supported language list");

    generate_provider_catalog();

    #[cfg(windows)]
    if std::path::Path::new("wowie.ico").is_file() {
        winresource::WindowsResource::new()
            .set_icon("wowie.ico")
            .compile()
            .expect("failed to compile Windows application icon");
    } else {
        println!(
            "cargo:warning=wowie.ico is missing; building without an embedded application icon"
        );
    }

    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("lang");
    slint_build::compile_with_config("ui/settings.slint", config.clone())
        .expect("failed to compile settings UI");
    slint_build::compile_with_config("ui/launcher.slint", config)
        .expect("failed to compile Slint UI");
}

fn generate_provider_catalog() {
    let out_dir =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let dynamic = generated_modules("src/providers/dynamic", "provider");
    let details = generated_modules("src/details", "detail");
    let dynamic_dir = std::env::current_dir()
        .unwrap()
        .join("src/providers/dynamic")
        .display()
        .to_string()
        .replace('\\', "/");
    let detail_dir = std::env::current_dir()
        .unwrap()
        .join("src/details")
        .display()
        .to_string()
        .replace('\\', "/");

    std::fs::write(
        out_dir.join("dynamic_providers.rs"),
        format!(
            "{}\npub fn register_all(registry: &mut crate::provider::ProviderRegistry, shortcuts: Vec<crate::config::ShortcutConfig>, shortcut_keywords: crate::config::ShortcutKeywords) {{\n{}\n}}\n",
            dynamic
                .iter()
                .map(|name| format!("#[path = \"{}/{}.rs\"] pub mod {};", dynamic_dir, name, name))
                .collect::<Vec<_>>().join("\n"),
            dynamic.iter().map(|name| format!("    {}::register(registry, shortcuts.clone(), shortcut_keywords.clone());", name)).collect::<Vec<_>>().join("\n")
        ),
    ).expect("failed to write dynamic provider catalog");

    let detail_registrations = format!(
        "    let _ = registry;\n{}",
        details
            .iter()
            .map(|name| format!("    {}::register(registry);", name))
            .collect::<Vec<_>>()
            .join("\n")
    );
    std::fs::write(
        out_dir.join("detail_providers.rs"),
        format!(
            "{}\npub fn register_all(registry: &mut crate::details::DetailRegistry) {{\n{}\n}}\n",
            details
                .iter()
                .map(|name| format!("#[path = \"{}/{}.rs\"] pub mod {};", detail_dir, name, name))
                .collect::<Vec<_>>()
                .join("\n"),
            detail_registrations
        ),
    )
    .expect("failed to write detail provider catalog");
}

fn generated_modules(directory: &str, kind: &str) -> Vec<String> {
    let path = std::path::Path::new(directory);
    let mut modules = std::fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file = entry.path();
            (file.extension().and_then(|value| value.to_str()) == Some("rs")
                && file.file_stem().and_then(|value| value.to_str()) != Some("mod"))
            .then(|| file.file_stem()?.to_str().map(str::to_owned))
            .flatten()
        })
        .collect::<Vec<_>>();
    modules.sort();
    println!(
        "cargo:warning=discovered {} {} module(s): {:?}",
        modules.len(),
        kind,
        modules
    );
    modules
}

fn po_entries(text: &str) -> Vec<(String, String, String)> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut entries = Vec::new();
    let mut context = String::new();
    for index in 0..lines.len().saturating_sub(1) {
        if let Some(value) = lines[index].strip_prefix("msgctxt ") {
            context = po_string(value).unwrap_or_default();
            continue;
        }
        let Some(message_id) = lines[index].strip_prefix("msgid ") else {
            continue;
        };
        let Some(message_id) = po_string(message_id) else {
            continue;
        };
        if message_id.is_empty() {
            continue;
        }
        let Some(message) = lines[index + 1].strip_prefix("msgstr ") else {
            continue;
        };
        let Some(message) = po_string(message) else {
            continue;
        };
        entries.push((context.clone(), message_id, message));
        context.clear();
    }
    entries
}

fn po_string(value: &str) -> Option<String> {
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    Some(
        value
            .replace("\\\\", "\\")
            .replace("\\\"", "\"")
            .replace("\\n", "\n"),
    )
}
