//! Shared fallback classification for results without a provider category.

use crate::result::ResultCategory;
use std::path::Path;

const WEBSITE_EXTENSIONS: &[&str] = &["htm", "html", "mht", "mhtml", "url", "website"];
const APPLICATION_EXTENSIONS: &[&str] = &[
    "bat", "cmd", "com", "cpl", "dll", "exe", "msi", "msc", "scr",
];
const DOCUMENT_EXTENSIONS: &[&str] = &[
    "abw", "csv", "doc", "docm", "docx", "dot", "dotm", "dotx", "epub", "fodp", "fods", "fodt",
    "key", "md", "mobi", "numbers", "odp", "ods", "odt", "pages", "pdf", "pps", "ppsm", "ppsx",
    "hlp", "ppt", "pptm", "pptx", "rtf", "tex", "txt", "wpd", "xls", "xlsb", "xlsm", "xlsx", "xlt",
    "xltm", "xltx", "chm",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetClassification {
    pub category: ResultCategory,
    pub is_http_url: bool,
}

pub fn classify_target_info(target: &Path) -> TargetClassification {
    let value = target.to_string_lossy().trim().to_owned();
    if value.is_empty() {
        return TargetClassification {
            category: ResultCategory::Other,
            is_http_url: false,
        };
    }
    if is_shell_target(&value) {
        return TargetClassification {
            category: ResultCategory::Application,
            is_http_url: false,
        };
    }
    let parsed_url = url::Url::parse(&value).ok();
    let is_http_url = parsed_url
        .as_ref()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https"));
    let category = if parsed_url
        .as_ref()
        .is_some_and(|url| is_http_url && url.host().is_some())
    {
        ResultCategory::Website
    } else {
        let extension = target
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some(extension) if WEBSITE_EXTENSIONS.contains(&extension) => ResultCategory::Website,
            Some(extension) if DOCUMENT_EXTENSIONS.contains(&extension) => ResultCategory::Document,
            Some(extension) if APPLICATION_EXTENSIONS.contains(&extension) => {
                ResultCategory::Application
            }
            _ => ResultCategory::Other,
        }
    };

    TargetClassification {
        category,
        is_http_url,
    }
}

fn is_shell_target(value: &str) -> bool {
    value
        .get(.."shell:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:"))
        || value.starts_with("::")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_final_targets_by_extension() {
        assert_eq!(
            classify_target_info(Path::new("https://example.com")).category,
            ResultCategory::Website
        );
        assert_eq!(
            classify_target_info(Path::new(r"C:\Users\User\Desktop\manual.pdf")).category,
            ResultCategory::Document
        );
        assert_eq!(
            classify_target_info(Path::new(r"C:\Apps\launcher.exe")).category,
            ResultCategory::Application
        );
        assert_eq!(
            classify_target_info(Path::new(r"C:\Help\reference.chm")).category,
            ResultCategory::Document
        );
        assert_eq!(
            classify_target_info(Path::new(r"C:\Help\legacy.hlp")).category,
            ResultCategory::Document
        );
    }

    #[test]
    fn unknown_extensions_fall_back_to_other() {
        assert_eq!(
            classify_target_info(Path::new(r"C:\Apps\tool.custom")).category,
            ResultCategory::Other
        );
    }

    #[test]
    fn classifies_shell_namespace_apps_as_applications() {
        assert_eq!(
            classify_target_info(Path::new(r"shell:AppsFolder\Vendor.App!Main")).category,
            ResultCategory::Application
        );
        assert_eq!(
            classify_target_info(Path::new(r"::{26EE0668-A00A-44D7-9371-BEB064C98683}")).category,
            ResultCategory::Application
        );
    }
}
