//! Read installed Settings metadata without launching pages or loading DLLs.
//!
//! These files are an implementation detail, so missing local evidence
//! deliberately yields no pages. A protocol registration alone proves nothing
//! about an individual page's availability.

use std::{collections::HashSet, path::PathBuf};

pub(crate) struct SettingsAvailability {
    pages: HashSet<String>,
    visibility: Vec<String>,
    pub(crate) system_directory: PathBuf,
    settings_installed: bool,
    pub(crate) settings_icon_path: Option<PathBuf>,
}

impl SettingsAvailability {
    pub(crate) fn discover() -> Self {
        let windows_directory = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let settings_directory = windows_directory.join("ImmersiveControlPanel");
        let settings_installed = settings_directory.join("SystemSettings.exe").is_file();
        let mut pages = HashSet::new();
        if settings_installed {
            if let Ok(files) = std::fs::read_dir(settings_directory.join("Settings")) {
                for file in files.flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if !name.starts_with("AllSystemSettings_") || !name.ends_with(".xml") {
                        continue;
                    }
                    let Ok(metadata) = file.metadata() else {
                        continue;
                    };
                    if metadata.len() > 16 * 1024 * 1024 {
                        continue;
                    }
                    if let Ok(xml) = std::fs::read_to_string(file.path()) {
                        pages.extend(pages_from_metadata(&xml));
                    }
                }
            }
            // Windows 10 metadata often uses internal page names instead of
            // policy IDs. Read literal URI strings from the installed routing
            // module as a fallback; do not load or execute the module.
            if pages.is_empty() {
                let module = settings_directory.join("SystemSettings.dll");
                if std::fs::metadata(&module)
                    .is_ok_and(|metadata| metadata.len() <= 64 * 1024 * 1024)
                {
                    if let Ok(bytes) = std::fs::read(module) {
                        pages.extend(pages_from_module(&bytes));
                    }
                }
            }
        }
        let visibility = [super::HKEY_LOCAL_MACHINE, super::HKEY_CURRENT_USER]
            .into_iter()
            .filter_map(|hive| {
                let key = super::shell::open_registry_key(
                    hive,
                    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\Explorer",
                )?;
                let value = super::shell::read_registry_string(key, "SettingsPageVisibility");
                unsafe { super::RegCloseKey(key) };
                value
            })
            .collect();
        Self {
            pages,
            visibility,
            system_directory: windows_directory.join("System32"),
            settings_installed,
            settings_icon_path: [
                "logo.targetsize-32_altform-unplated.png",
                "logo.targetsize-32.png",
                "logo.png",
            ]
            .into_iter()
            .map(|name| settings_directory.join("images").join(name))
            .find(|path| path.is_file()),
        }
    }

    pub(crate) fn contains(&self, uri: &str) -> bool {
        let Some(page) = uri.strip_prefix("ms-settings:") else {
            return false;
        };
        self.settings_installed
            && (page.is_empty() || self.pages.contains(&page.to_ascii_lowercase()))
            && self
                .visibility
                .iter()
                .all(|policy| visible_by_policy(page, policy))
    }
}

fn pages_from_module(bytes: &[u8]) -> HashSet<String> {
    let mut pages = HashSet::new();
    let mut collect = |units: &[u16]| {
        let prefix = "ms-settings:".encode_utf16().collect::<Vec<_>>();
        for (offset, window) in units.windows(prefix.len()).enumerate() {
            if window != prefix {
                continue;
            }
            let tail = &units[offset + prefix.len()..];
            let Some(length) = tail.iter().take(129).position(|unit| *unit == 0) else {
                continue;
            };
            if length == 0 || length > 128 {
                continue;
            }
            if tail[..length].iter().all(|unit| {
                u8::try_from(*unit).is_ok_and(|c| c.is_ascii_alphanumeric() || c == b'-')
            }) {
                if let Ok(page) = String::from_utf16(&tail[..length]) {
                    pages.insert(page.to_ascii_lowercase());
                }
            }
        }
    };
    // PE strings may be UTF-16LE or ASCII. Restrict matches to complete,
    // terminated URI strings rather than substring/prefix guesses.
    for alignment in 0..2 {
        let units = bytes[alignment.min(bytes.len())..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        collect(&units);
    }
    let units = bytes
        .iter()
        .map(|byte| u16::from(*byte))
        .collect::<Vec<_>>();
    collect(&units);
    pages
}

fn pages_from_metadata(xml: &str) -> HashSet<String> {
    let Ok(document) = roxmltree::Document::parse(xml) else {
        return HashSet::new();
    };
    if document.root_element().tag_name().name() != "PCSettings" {
        return HashSet::new();
    }
    document
        .descendants()
        .filter(|node| node.is_element())
        .flat_map(|node| {
            let text = node.text().unwrap_or_default().trim();
            match node.tag_name().name() {
                // Newer Windows 11 metadata uses policy IDs; older Windows 10
                // metadata exposes the URI in an ApplicationInformation DeepLink.
                "PolicyIds" => text.split(';').map(str::to_owned).collect::<Vec<_>>(),
                "DeepLink" => text
                    .split_whitespace()
                    .filter_map(|part| part.strip_prefix("ms-settings:"))
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            }
        })
        .filter(|page| {
            !page.is_empty() && page.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        .map(|page| page.to_ascii_lowercase())
        .collect()
}

fn visible_by_policy(page: &str, policy: &str) -> bool {
    let Some((mode, pages)) = policy.split_once(':') else {
        return true;
    };
    let listed = pages
        .split(';')
        .any(|entry| entry.trim().eq_ignore_ascii_case(page));
    if mode.eq_ignore_ascii_case("hide") {
        !listed
    } else if mode.eq_ignore_ascii_case("showonly") {
        page.is_empty() || listed
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_windows_10_and_11_metadata_without_matching_keywords() {
        let pages = pages_from_metadata(
            r#"<PCSettings>
            <SearchableContent><ApplicationInformation><DeepLink>%windir%\System32\control.exe ms-settings:display</DeepLink></ApplicationInformation></SearchableContent>
            <SearchableContent><SettingIdentity><SettingPaths><Path><PolicyIds>network-wifi;NETWORK-PROXY</PolicyIds></Path></SettingPaths></SettingIdentity></SearchableContent>
            <Description>ms-settings:fake</Description></PCSettings>"#,
        );
        assert_eq!(
            pages,
            HashSet::from([
                "display".into(),
                "network-wifi".into(),
                "network-proxy".into()
            ])
        );
        assert!(pages_from_metadata("<PCSettings><PolicyIds>display</PCSettings>").is_empty());
        assert!(pages_from_metadata("<Other><PolicyIds>display</PolicyIds></Other>").is_empty());
    }

    #[test]
    fn policy_lists_match_complete_page_ids() {
        assert!(!visible_by_policy("display", "hide:DISPLAY;network-wifi"));
        assert!(visible_by_policy("display-advanced", "hide:display"));
        assert!(!visible_by_policy("network-wifi", "showonly:display"));
        assert!(visible_by_policy("", "showonly:display"));
    }

    #[test]
    fn missing_metadata_does_not_publish_speculative_pages() {
        let availability = SettingsAvailability {
            pages: HashSet::new(),
            visibility: vec![],
            system_directory: PathBuf::new(),
            settings_installed: true,
            settings_icon_path: None,
        };
        assert!(availability.contains("ms-settings:"));
        assert!(!availability.contains("ms-settings:display"));
    }

    #[test]
    fn module_fallback_reads_exact_ascii_and_utf16_uris() {
        let mut bytes = b"ms-settings:display\0ms-settings:fake?parameter\0".to_vec();
        bytes.extend(
            "ms-settings:network-wifi\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        assert_eq!(
            pages_from_module(&bytes),
            HashSet::from(["display".into(), "network-wifi".into()])
        );
    }
}
