//! Installed Settings host and administrator policy snapshot.
//!
//! Page discovery belongs to the versioned catalog. Settings XML and literal
//! strings in SystemSettings.dll are incomplete implementation details; their
//! absence must never veto an otherwise supported catalog page.

use std::path::PathBuf;

pub(crate) struct SettingsAvailability {
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
            visibility,
            system_directory: windows_directory.join("System32"),
            settings_installed: settings_directory.join("SystemSettings.exe").is_file(),
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

    /// Apply only to catalog pages after version/device filtering. Host
    /// installation alone cannot validate arbitrary external URIs.
    pub(crate) fn contains(&self, uri: &str) -> bool {
        let Some((scheme, page)) = uri.split_once(':') else {
            return false;
        };
        scheme.eq_ignore_ascii_case("ms-settings")
            && page.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && self.settings_installed
            && self
                .visibility
                .iter()
                .all(|policy| visible_by_policy(page, policy))
    }
}

fn visible_by_policy(page: &str, policy: &str) -> bool {
    let Some((mode, pages)) = policy.trim().split_once(':') else {
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

    fn installed(visibility: Vec<String>) -> SettingsAvailability {
        SettingsAvailability {
            visibility,
            system_directory: PathBuf::new(),
            settings_installed: true,
            settings_icon_path: None,
        }
    }

    #[test]
    fn catalog_pages_do_not_require_undocumented_metadata() {
        let mut availability = installed(vec![]);
        assert!(availability.contains("ms-settings:"));
        assert!(availability.contains("ms-settings:display"));
        assert!(availability.contains("MS-SETTINGS:network-wifi"));
        assert!(!availability.contains("https:display"));
        assert!(!availability.contains("ms-settings:display unexpected-arguments"));
        availability.settings_installed = false;
        assert!(!availability.contains("ms-settings:display"));
    }

    #[test]
    fn policies_match_complete_ids_and_combine_machine_and_user_restrictions() {
        assert!(!visible_by_policy("display", "hide:DISPLAY;network-wifi"));
        assert!(visible_by_policy("display-advanced", "hide:display"));
        assert!(!visible_by_policy("network-wifi", "showonly:display"));
        assert!(visible_by_policy("", "showonly:display"));
        let availability = installed(vec![
            "showonly:display;network-wifi".into(),
            "hide:display".into(),
        ]);
        assert!(!availability.contains("ms-settings:display"));
        assert!(availability.contains("ms-settings:network-wifi"));
        assert!(!availability.contains("ms-settings:privacy"));
    }
}
