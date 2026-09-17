fn main() {
    println!("cargo:rerun-if-changed=lang");
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
    let generated = format!("pub const SUPPORTED_LANGUAGES: &[&str] = &[{language_list}];\n");
    std::fs::write(
        std::path::Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR is not set"))
            .join("supported_languages.rs"),
        generated,
    )
    .expect("failed to write supported language list");

    let config = slint_build::CompilerConfiguration::new().with_bundled_translations("lang");
    slint_build::compile_with_config("ui/launcher.slint", config)
        .expect("failed to compile Slint UI");
}
