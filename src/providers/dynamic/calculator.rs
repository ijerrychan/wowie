use crate::{
    platform::SourceId,
    provider::{DynamicProvider, ProviderRegistry},
    result::{IconSpec, ResultCategory, SearchResult},
};
use std::path::PathBuf;

/// Evaluates arithmetic expressions from the current query.
///
/// This provider deliberately has no action: selecting a calculator result
/// must not launch anything. The result is regenerated for every query so the
/// UI can show the value while the user is typing.
pub struct CalculatorProvider {
    language: String,
}

impl CalculatorProvider {
    pub fn new() -> Self {
        Self {
            language: "system".into(),
        }
    }
}

impl Default for CalculatorProvider {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &mut ProviderRegistry, _shortcuts: Vec<crate::config::ShortcutConfig>) {
    registry.register(CalculatorProvider::new());
}

impl DynamicProvider for CalculatorProvider {
    fn query(&self, query: &str) -> Vec<SearchResult> {
        result(query, &self.language).into_iter().collect()
    }

    fn set_language(&mut self, language: String) {
        self.language = language;
    }
}

fn result(query: &str, language: &str) -> Option<SearchResult> {
    let expression = query.trim();
    if expression.is_empty()
        || !expression
            .chars()
            .any(|character| character.is_ascii_digit())
        || !is_explicit_expression(expression)
    {
        return None;
    }

    let value = evaluate(expression)?;
    if !value.is_finite() {
        return None;
    }

    let display_value = format_value(value);
    let id = format!("calculator:{expression}");
    let source = SourceId::Custom("Calculator".into());
    Some(SearchResult {
        id: id.clone(),
        identity: id,
        title: format!("= {display_value}"),
        category: ResultCategory::Calculator,
        aliases: vec![expression.to_owned()],
        location: crate::utils::i18n::tr(language, "Calculator").into(),
        launch_target: PathBuf::new(),
        launch_arguments: String::new(),
        launch_working_directory: None,
        source: source.clone(),
        sources: vec![source],
        launch_entries: Vec::new(),
        actions: Vec::new(),
        detail: None,
        description: None,
        app_user_model_id: None,
        package_family_name: None,
        package_metadata: None,
        icon: Some(IconSpec::Calculator),
        ranking_bias: 0,
    })
}

fn evaluate(expression: &str) -> Option<f64> {
    let mut namespace = |name: &str, arguments: Vec<f64>| -> Option<f64> {
        match name {
            "sqrt" if arguments.len() == 1 => Some(arguments[0].sqrt()),
            _ => None,
        }
    };
    fasteval::ez_eval(expression, &mut namespace).ok()
}

fn is_explicit_expression(expression: &str) -> bool {
    expression
        .chars()
        .any(|character| matches!(character, '+' | '-' | '*' | '/' | '%' | '^' | '(' | ')'))
}

fn format_value(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_arithmetic_expression() {
        let item = result("2 * (3 + 4)", "en").expect("calculator result");
        assert_eq!(item.title, "= 14");
        assert_eq!(item.category, ResultCategory::Calculator);
        assert!(item.actions.is_empty());
    }

    #[test]
    fn evaluates_math_functions() {
        let item = result("sqrt(16) + 1", "en").expect("calculator result");
        assert_eq!(item.title, "= 5");
    }

    #[test]
    fn ignores_non_expressions_and_non_finite_values() {
        assert!(result("notepad", "en").is_none());
        assert!(result("123", "en").is_none());
        assert!(result("1 / 0", "en").is_none());
    }
}
