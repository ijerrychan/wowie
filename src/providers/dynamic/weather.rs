//! Search-driven weather detail provider.
//!
//! Weather is triggered by a search keyword, but it is not a launch shortcut.
//! Keeping it in its own provider prevents shortcut parsing and UI behavior
//! from leaking into this detail-only result.

use crate::{
    platform::SourceId,
    provider::{DynamicProvider, ProviderRegistry},
    result::{IconSpec, ResultCategory, SearchResult},
};
use std::path::PathBuf;

pub fn register(registry: &mut ProviderRegistry, _shortcuts: Vec<crate::config::ShortcutConfig>) {
    registry.register(WeatherProvider::new());
}

pub struct WeatherProvider {
    language: String,
}

impl WeatherProvider {
    pub fn new() -> Self {
        Self {
            language: "system".into(),
        }
    }

    fn results(&self, query: &str) -> Vec<SearchResult> {
        let text = query.trim();
        if text.is_empty() || !matches_keyword(text, &self.language) {
            return Vec::new();
        }
        vec![result(&self.language)]
    }
}

impl Default for WeatherProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicProvider for WeatherProvider {
    fn query(&self, query: &str) -> Vec<SearchResult> {
        self.results(query)
    }

    fn supports_fuzzy_search(&self) -> bool {
        true
    }

    fn fuzzy_candidates(&self) -> Vec<SearchResult> {
        vec![result(&self.language)]
    }

    fn set_language(&mut self, language: String) {
        self.language = language;
    }
}

fn matches_keyword(query: &str, language: &str) -> bool {
    let keywords = crate::utils::i18n::tr_aliases(language, "Launcher", "Weather keyword");
    keywords
        .iter()
        .any(|keyword| query.trim().eq_ignore_ascii_case(keyword))
}

fn result(language: &str) -> SearchResult {
    let weather_name = crate::utils::i18n::tr(language, "Weather");
    let weather_keywords = crate::utils::i18n::tr_aliases(language, "Launcher", "Weather keyword");
    SearchResult {
        id: "weather:current".into(),
        identity: "weather:current".into(),
        title: weather_name.into(),
        category: ResultCategory::Other,
        aliases: weather_keywords
            .into_iter()
            .map(String::from)
            .chain(std::iter::once(weather_name.into()))
            .collect(),
        location: crate::utils::i18n::tr(language, "View weather information").into(),
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: SourceId::Custom("Weather".into()),
        sources: vec![SourceId::Custom("Weather".into())],
        launch_entries: Vec::new(),
        actions: Vec::new(),
        detail: Some(detail_view(language)),
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
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
        scrollable: false,
    }
}

#[cfg(test)]
mod fuzzy_tests {
    use super::*;

    #[test]
    fn declares_fuzzy_search_candidates_without_matching_locally() {
        let provider = WeatherProvider::new();
        assert!(provider.supports_fuzzy_search());
        assert_eq!(provider.fuzzy_candidates().len(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_returns_a_detail_result_without_shortcut_semantics() {
        let provider = WeatherProvider::new();
        let found = provider.results("weather");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "weather:current");
        assert_eq!(found[0].category, ResultCategory::Other);
        assert!(found[0].actions.is_empty());
        assert!(found[0].detail.is_some());
    }

    #[test]
    fn localized_keyword_is_loaded_from_i18n() {
        assert!(matches_keyword("天气", "zh-CN"));
        let found = result("zh-CN");
        assert_eq!(found.title, "天气");
        assert_eq!(found.location, "查看天气信息");
    }
}
