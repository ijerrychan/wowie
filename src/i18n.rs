include!(concat!(env!("OUT_DIR"), "/supported_languages.rs"));

pub fn is_supported_language(language: &str) -> bool {
    SUPPORTED_LANGUAGES.contains(&language)
}
