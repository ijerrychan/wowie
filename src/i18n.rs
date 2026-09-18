include!(concat!(env!("OUT_DIR"), "/supported_languages.rs"));

pub fn is_supported_language(language: &str) -> bool {
    SUPPORTED_LANGUAGES.contains(&language)
}

pub fn execute_command_name(language: &str) -> &'static str {
    let language = effective_language(language);
    EXECUTE_COMMAND_TRANSLATIONS
        .iter()
        .find(|(locale, _)| locale.eq_ignore_ascii_case(language.as_str()))
        .map(|(_, value)| *value)
        .unwrap_or("Execute Command")
}

pub fn open_website_name(language: &str) -> &'static str {
    let language = effective_language(language);
    OPEN_WEBSITE_TRANSLATIONS
        .iter()
        .find(|(locale, _)| locale.eq_ignore_ascii_case(language.as_str()))
        .map(|(_, value)| *value)
        .unwrap_or("Open website")
}

pub fn exit_name(language: &str) -> &'static str {
    let language = effective_language(language);
    EXIT_TRANSLATIONS
        .iter()
        .find(|(locale, _)| locale.eq_ignore_ascii_case(language.as_str()))
        .map(|(_, value)| *value)
        .unwrap_or("Exit")
}

fn effective_language(language: &str) -> String {
    if language != "system" {
        return language.to_owned();
    }

    let system_locale = system_locale().unwrap_or_else(|| "en".into());
    if system_locale.to_ascii_lowercase().starts_with("zh") {
        "zh-CN".into()
    } else {
        "en".into()
    }
}

#[cfg(windows)]
fn system_locale() -> Option<String> {
    let mut buffer = [0_u16; 85];
    let length = unsafe {
        windows_sys::Win32::Globalization::GetUserDefaultLocaleName(
            buffer.as_mut_ptr(),
            buffer.len() as i32,
        )
    };
    (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize - 1]))
}

#[cfg(not(windows))]
fn system_locale() -> Option<String> {
    ["LC_ALL", "LANG", "LANGUAGE"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_chinese_language_uses_translation() {
        assert_eq!(execute_command_name("zh-CN"), "执行命令");
    }
}
