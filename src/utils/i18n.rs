include!(concat!(env!("OUT_DIR"), "/supported_languages.rs"));

pub fn is_supported_language(language: &str) -> bool {
    SUPPORTED_LANGUAGES.contains(&language)
}

pub fn tr(language: &str, message_id: &'static str) -> &'static str {
    tr_in_context(language, "", message_id)
}

pub fn tr_in_context(
    language: &str,
    context: &'static str,
    message_id: &'static str,
) -> &'static str {
    translated_message(language, context, message_id).unwrap_or(message_id)
}

fn translated_message(language: &str, context: &str, message_id: &str) -> Option<&'static str> {
    let language = effective_language(language);
    TRANSLATIONS
        .iter()
        .find(|(locale, entry_context, id, _)| {
            locale.eq_ignore_ascii_case(language.as_str())
                && (context.is_empty() || *entry_context == context)
                && *id == message_id
        })
        .map(|(_, _, _, value)| *value)
}

/// Returns a translated message owned by the caller. This is useful for
/// strings loaded from plugin manifests, which cannot be `&'static str` keys.
pub fn tr_owned_in_context(language: &str, context: &str, message_id: &str) -> String {
    translated_message(language, context, message_id)
        .unwrap_or(message_id)
        .to_owned()
}

/// Returns a translated list. Values use `|` as the separator in the `.po` file.
#[allow(dead_code)]
pub fn tr_list(
    language: &str,
    context: &'static str,
    message_id: &'static str,
) -> Vec<&'static str> {
    tr_in_context(language, context, message_id)
        .split('|')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect()
}

/// Returns localized values followed by English fallback values, without duplicates.
#[allow(dead_code)]
pub fn tr_aliases(
    language: &str,
    context: &'static str,
    message_id: &'static str,
) -> Vec<&'static str> {
    let language = effective_language(language);
    let mut aliases = tr_list(&language, context, message_id);
    for value in tr_list("en", context, message_id) {
        if !aliases
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(value))
        {
            aliases.push(value);
        }
    }
    aliases
}

/// Returns translated aliases for a message loaded at runtime, followed by
/// the English fallback. PowerToys' Windows Settings catalog stores resource
/// keys in JSON, so its message IDs are not Rust string literals.
pub fn tr_aliases_all_languages_owned(context: &'static str, message_id: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    for language in SUPPORTED_LANGUAGES
        .iter()
        .copied()
        .chain(std::iter::once("en"))
    {
        let value = tr_owned_in_context(language, context, message_id);
        for alias in value
            .split('|')
            .map(str::trim)
            .filter(|alias| !alias.is_empty())
        {
            if !aliases
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(alias))
            {
                aliases.push(alias.to_owned());
            }
        }
    }
    aliases
}

pub(crate) fn effective_language(language: &str) -> String {
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
        assert_eq!(tr("zh-CN", "Execute Command"), "执行命令");
    }

    #[test]
    fn contextual_translation_can_return_multiple_values() {
        assert_eq!(
            tr_list("zh-CN", "Launcher", "Weather keyword"),
            vec!["天气"]
        );
    }

    #[test]
    fn aliases_include_localized_and_english_values() {
        assert_eq!(
            tr_aliases("zh-CN", "Launcher", "Weather keyword"),
            vec!["天气", "weather"]
        );
    }
}
