use super::ShortcutQuery;
use crate::{
    platform::SourceId,
    result::{IconSpec, ResultCategory, SearchResult},
};
use std::path::PathBuf;

pub(super) fn matches(query: ShortcutQuery<'_>) -> bool {
    crate::utils::i18n::tr_aliases(query.language, "Launcher", "Weather keyword")
        .iter()
        .any(|keyword| query.text.eq_ignore_ascii_case(keyword))
}

pub(super) fn result(language: &str) -> SearchResult {
    let weather_name = crate::utils::i18n::tr(language, "Weather");
    let weather_keywords = crate::utils::i18n::tr_aliases(language, "Launcher", "Weather keyword");
    SearchResult {
        id: "shortcut:weather-test".into(),
        identity: "shortcut:weather-test".into(),
        title: weather_name.into(),
        category: ResultCategory::Shortcut,
        aliases: weather_keywords
            .into_iter()
            .map(String::from)
            .chain(std::iter::once(weather_name.into()))
            .collect(),
        location: crate::utils::i18n::tr(language, "View weather information").into(),
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: SourceId::Shortcut,
        sources: vec![SourceId::Shortcut],
        launch_entries: Vec::new(),
        actions: Vec::new(),
        detail: Some(detail_view(language)),
        icon: Some(IconSpec::Weather),
        ranking_bias: 0,
    }
}

fn detail_view(language: &str) -> crate::result::DetailView {
    crate::result::DetailView {
        title: crate::utils::i18n::tr(language, "Weather").into(),
        subtitle: crate::utils::i18n::tr(language, "Current location").into(),
        source: "Open-Meteo".into(),
        blocks: vec![crate::result::DetailBlock::Text {
            text: crate::utils::i18n::tr(language, "Loading weather").into(),
            color: "secondary".into(),
        }],
        actions: Vec::new(),
        preferred_width: None,
        minimum_height: None,
        scrollable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query<'a>(raw: &'a str, language: &'a str) -> ShortcutQuery<'a> {
        ShortcutQuery {
            raw,
            text: raw.trim(),
            language,
        }
    }

    #[test]
    fn returns_a_single_detail_fixture() {
        let found = result("en");
        assert_eq!(found.id, "shortcut:weather-test");
        assert!(found.actions.is_empty());
        assert!(matches!(found.icon, Some(IconSpec::Weather)));
    }

    #[test]
    fn localized_keyword_is_loaded_from_i18n() {
        assert!(matches(query("天气", "zh-CN")));
        let found = result("zh-CN");
        assert_eq!(found.title, "天气");
        assert_eq!(found.location, "查看天气信息");
        assert!(matches(query("weather", "zh-CN")));
    }
}
